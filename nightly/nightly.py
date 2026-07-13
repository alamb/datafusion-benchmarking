#!/usr/bin/env python3
"""
Nightly benchmark orchestrator for the DataFusion benchmarking pipeline.

Runs (in order): check_env (optional) -> build -> bench -> detect -> report
-> publish, per nightly/DESIGN.md.  Stdlib only, python 3.9+ compatible.

Usage:
    python nightly/nightly.py [--config nightly/config.json] [--days N]
        [--dry-run] [--skip-build] [--skip-bench] [--skip-detect]
        [--skip-report] [--no-publish]

Behavior notes:
- chdirs to the repo root (the parent of the nightly/ directory) at startup
  because run_clickbench.py resolves data/ relative to the CWD.
- A portable lock file (nightly/.lock, O_CREAT|O_EXCL) prevents overlapping
  runs; locks older than 24h are broken with a warning.  The lock is always
  released on exit (try/finally plus an atexit safety net).  Both breaking
  and releasing verify the lock's content first, so racing processes never
  delete a lock they do not own.
- Dependency map: bench->build (hard), detect->bench (soft),
  report->bench (soft), publish->report (hard).  "soft" = run anyway if the
  prerequisite was merely skipped, skip only if it hard-failed.  Hard edges
  additionally skip when the prerequisite was itself skipped because of an
  upstream failure (so a build failure cascades bench -> ... but a
  user-requested --skip-build does not).
- detect exit code 2 means "ok, with regression alerts"; regression and
  improvement counts are read from nightly/out/alerts.json and surfaced in
  status.json ("alerts" = regressions only, "improvements" separately).
- the build step records how many builds/datafusion-cli@* files appeared
  (status.json "new_builds"); a bench that exits 0 without adding any new
  revisions while new binaries exist is marked failed.
- Everything is logged to the console AND nightly/logs/nightly-YYYY-MM-DD.log.
- status: nightly/out/status.json; process exit code 0 iff no step failed.
- --dry-run prints the exact commands per step (and what publish would
  commit) without executing anything; no files are written except the log.
"""

import argparse
import atexit
import csv
import glob
import json
import os
import platform
import subprocess
import sys
import time
from datetime import datetime, timezone

# Paths are relative to the repo root (we chdir there at startup).
# Forward slashes work on Windows too and keep logs/tests uniform.
DEFAULT_CONFIG_PATH = "nightly/config.json"
LOCK_PATH = "nightly/.lock"
LOG_DIR = "nightly/logs"
OUT_DIR = "nightly/out"
ALERTS_PATH = OUT_DIR + "/alerts.json"
STATUS_PATH = OUT_DIR + "/status.json"

LOCK_MAX_AGE_HOURS = 24.0

STEP_ORDER = ["check_env", "build", "bench", "detect", "report", "publish"]

# step -> prerequisite step.  Hard: also skip if the prerequisite was
# skipped because of an upstream failure.  Soft: skip only if the
# prerequisite hard-failed.
HARD_DEPS = {"bench": "build", "publish": "report"}
SOFT_DEPS = {"detect": "bench", "report": "bench"}

# Shown in --dry-run for the publish commit; the real message is built at
# publish time from the actual counts.
COMMIT_MSG_PLACEHOLDER = (
    "nightly: <UTC-date> (<new_revisions> new revision(s), <alerts> alert(s))"
)

DEFAULT_CONFIG = {
    "datafusion_dir": "datafusion",
    "days": 2,
    "num_builds": 2,
    "results_dir": "results",
    "benchmarks": ["clickbench"],
    "detect": {
        "window": 60,
        "min_delta_pct": 2.0,
        "iqr_multiplier": 3.0,
        "pvalue": 0.05,
        "permutations": 199,
    },
    "check_env": False,
    "prune_builds_keep": 30,
    "publish": {
        "enabled": False,
        "remote": "origin",
        "branch": "main",
        "paths": ["results", "docs", "nightly/out"],
    },
    # Script locations (relative to the repo root).  Not part of the
    # documented config schema; overridable mainly so the test suite can
    # point steps at stub scripts.
    "scripts": {
        "build": "ensure_datafusion_cli.py",
        "bench": "benchmark.py",
        "detect": "nightly/detect.py",
        "report": "report.py",
        "check_env": "nightly/check_env.sh",
    },
}


def utc_now():
    return datetime.now(timezone.utc)


def utc_iso(dt=None):
    if dt is None:
        dt = utc_now()
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


class Logger(object):
    """Writes messages to the console and (if path given) a log file."""

    def __init__(self, path=None):
        self.path = path
        self._fh = None
        if path is not None:
            log_dir = os.path.dirname(path)
            if log_dir:
                os.makedirs(log_dir, exist_ok=True)
            self._fh = open(path, "a", encoding="utf-8")

    def log(self, msg):
        self._emit(msg)

    def raw(self, line):
        """Log a line of subprocess output verbatim."""
        self._emit(line)

    def _emit(self, msg):
        try:
            print(msg, flush=True)
        except UnicodeEncodeError:
            enc = getattr(sys.stdout, "encoding", None) or "ascii"
            print(msg.encode(enc, "replace").decode(enc, "replace"), flush=True)
        if self._fh is not None:
            stamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
            self._fh.write("[%s] %s\n" % (stamp, msg))
            self._fh.flush()

    def close(self):
        if self._fh is not None:
            self._fh.close()
            self._fh = None


def format_cmd(cmd):
    """Human-readable rendering of an argv list (quotes args with spaces)."""
    parts = []
    for arg in cmd:
        if arg == "" or " " in arg or "\t" in arg or '"' in arg:
            parts.append('"%s"' % arg.replace('"', '\\"'))
        else:
            parts.append(arg)
    return " ".join(parts)


def run_streamed(cmd, logger):
    """Run cmd, streaming combined stdout/stderr line-by-line to the logger.

    Returns the exit code (127 if the executable could not be started).
    """
    logger.log("+ " + format_cmd(cmd))
    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
    except OSError as exc:
        logger.log("ERROR: could not start %s: %s" % (cmd[0], exc))
        return 127
    for line in proc.stdout:
        logger.raw(line.rstrip("\r\n"))
    proc.stdout.close()
    return proc.wait()


# ---------------------------------------------------------------------------
# Lock file
# ---------------------------------------------------------------------------

_HELD_LOCKS = set()
_LOCK_CONTENTS = {}  # abspath -> exact content this process wrote


def _atexit_release_locks():
    for path in list(_HELD_LOCKS):
        release_lock(path)


atexit.register(_atexit_release_locks)


def _read_lock_content(path):
    """Full text of the lock file, or None if it cannot be read."""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return None


def _lock_age_seconds(path, content=None):
    """Age of the lock in seconds, or None if it cannot be determined
    (e.g. the file vanished).  `content` is the lock text if already read."""
    if content is None:
        content = _read_lock_content(path)
    epoch = None
    if content is not None:
        for line in content.splitlines():
            if line.startswith("started_epoch="):
                try:
                    epoch = float(line.split("=", 1)[1].strip())
                except ValueError:
                    pass
    if epoch is None:
        try:
            epoch = os.path.getmtime(path)
        except OSError:
            return None
    return max(0.0, time.time() - epoch)


def _break_stale_lock(path, inspected_content, logger):
    """Atomically remove a stale lock without racing other breakers.

    os.replace the lock to a unique temp name (the loser of a concurrent
    break race gets FileNotFoundError and backs off), verify the renamed
    file still holds the content inspected during the age check, then
    delete it.  Returns True iff this process removed the stale lock."""
    temp = "%s.stale-%d-%d" % (path, os.getpid(), int(time.time() * 1000000))
    try:
        os.replace(path, temp)
    except OSError:
        # FileNotFoundError: another process broke the lock first.
        logger.log("NOTE: stale lock %s already gone; backing off" % path)
        return False
    if _read_lock_content(temp) != inspected_content:
        # The lock changed hands between the age check and the rename: what
        # we renamed is a fresh lock, not the stale one.  Put it back.
        logger.log(
            "NOTE: lock %s changed while breaking it; restoring and backing off"
            % path
        )
        try:
            os.replace(temp, path)
        except OSError:
            pass
        return False
    try:
        os.remove(temp)
    except OSError:
        pass
    return True


def acquire_lock(path, logger, max_age_hours=LOCK_MAX_AGE_HOURS):
    """Create the lock file with O_CREAT|O_EXCL.  Returns True on success.

    Locks older than max_age_hours are broken with a warning.  Portable
    (no fcntl) so it also works on Windows.
    """
    for _attempt in range(3):
        try:
            fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        except FileExistsError:
            content = _read_lock_content(path)
            age = _lock_age_seconds(path, content)
            if age is None:
                continue  # lock vanished between open and stat; retry
            if age > max_age_hours * 3600.0:
                logger.log(
                    "WARNING: breaking stale lock %s (age %.1f h > %.0f h)"
                    % (path, age / 3600.0, max_age_hours)
                )
                _break_stale_lock(path, content, logger)
                continue
            logger.log(
                "Another nightly run appears to be in progress "
                "(lock %s, age %.1f h). Exiting." % (path, age / 3600.0)
            )
            return False
        content = (
            "pid=%d\nstarted_at=%s\nstarted_epoch=%.3f\n"
            % (os.getpid(), utc_iso(), time.time())
        )
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(content)
        abspath = os.path.abspath(path)
        _HELD_LOCKS.add(abspath)
        _LOCK_CONTENTS[abspath] = content
        return True
    logger.log("Could not acquire lock %s after retries. Exiting." % path)
    return False


def release_lock(path):
    """Remove the lock, but only if this process still owns it (the file
    holds exactly the pid+started stamp we wrote at acquire time)."""
    abspath = os.path.abspath(path)
    _HELD_LOCKS.discard(abspath)
    expected = _LOCK_CONTENTS.pop(abspath, None)
    if expected is None:
        return  # never acquired by this process; leave any lock alone
    if _read_lock_content(abspath) != expected:
        return  # broken and re-acquired by someone else; not ours to delete
    try:
        os.remove(abspath)
    except OSError:
        pass


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

def deep_merge(base, override):
    """Recursively merge override into base (returns a new dict)."""
    merged = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def load_config(path, explicit, logger):
    """Built-in defaults <- config file.  Returns None on error.

    If the (explicitly passed) config file is missing that is an error;
    a missing default config just falls back to built-in defaults.
    """
    if not os.path.exists(path):
        if explicit:
            logger.log("ERROR: config file not found: %s" % path)
            return None
        logger.log(
            "NOTE: default config %s not found; using built-in defaults" % path
        )
        return deep_merge(DEFAULT_CONFIG, {})
    try:
        with open(path, "r", encoding="utf-8") as fh:
            user_config = json.load(fh)
    except ValueError as exc:
        logger.log("ERROR: could not parse config %s: %s" % (path, exc))
        return None
    if not isinstance(user_config, dict):
        logger.log("ERROR: config %s must contain a JSON object" % path)
        return None
    return deep_merge(DEFAULT_CONFIG, user_config)


def apply_cli_overrides(config, args):
    """CLI flags override config file values."""
    if args.days is not None:
        config["days"] = args.days
    return config


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def build_commands(config):
    """Pure function: merged config -> {step: [argv, ...]}.

    Python scripts are invoked with sys.executable (never a hardcoded
    'python3').  The publish commit message is a placeholder here; the real
    one is substituted at publish time.
    """
    py = sys.executable
    scripts = config["scripts"]
    det = config["detect"]
    pub = config["publish"]
    commands = {}
    commands["check_env"] = [["bash", scripts["check_env"]]]
    commands["build"] = [[
        py, scripts["build"],
        "--days", str(config["days"]),
        "--num-builds", str(config["num_builds"]),
        "--datafusion-dir", config["datafusion_dir"],
    ]]
    commands["bench"] = [[
        py, scripts["bench"],
        "--output-dir", config["results_dir"],
        "--benchmarks",
    ] + [str(b) for b in config["benchmarks"]]]
    commands["detect"] = [[
        py, scripts["detect"],
        "--results-dir", config["results_dir"],
        "--output-dir", OUT_DIR,
        "--window", str(det["window"]),
        "--min-delta-pct", str(det["min_delta_pct"]),
        "--iqr-multiplier", str(det["iqr_multiplier"]),
        "--pvalue", str(det["pvalue"]),
        "--permutations", str(det["permutations"]),
    ]]
    commands["report"] = [[
        py, scripts["report"],
        "--results-dir", config["results_dir"],
    ]]
    commands["publish"] = [
        ["git", "add", "--"] + [str(p) for p in pub["paths"]],
        ["git", "commit", "-m", COMMIT_MSG_PLACEHOLDER],
        ["git", "pull", "--rebase", pub["remote"], pub["branch"]],
        ["git", "push", pub["remote"], pub["branch"]],
    ]
    return commands


def dependency_block(step, states, skip_kinds):
    """Pure function implementing the dependency map.

    states: {step: "ok"|"failed"|"skipped"} for already-decided steps.
    skip_kinds: {step: "user"|"dependency"} for skipped steps.
    Returns a human-readable reason string if `step` must be skipped
    because of its prerequisite, else None.
    """
    prereq = HARD_DEPS.get(step)
    if prereq is not None:
        state = states.get(prereq)
        if state == "failed":
            return "%s failed" % prereq
        if state == "skipped" and skip_kinds.get(prereq) == "dependency":
            return "%s was skipped due to an upstream failure" % prereq
        return None
    prereq = SOFT_DEPS.get(step)
    if prereq is not None and states.get(prereq) == "failed":
        return "%s failed" % prereq
    return None


# ---------------------------------------------------------------------------
# Step helpers
# ---------------------------------------------------------------------------

def read_revisions(csv_path, logger=None):
    """Distinct git_revision values in a results CSV (cheap column read).

    Tolerates a missing header row (known results_2026_03 bug) by assuming
    the standard schema (git_revision is column 5).  A csv.Error mid-file
    (NUL bytes, oversized fields, ...) is logged as a WARNING and the
    revisions collected so far are returned."""
    revisions = set()
    if not os.path.exists(csv_path):
        return revisions
    with open(csv_path, "r", newline="", encoding="utf-8", errors="replace") as fh:
        reader = csv.reader(fh)
        rev_idx = 5
        try:
            first = next(reader, None)
            if first is None:
                return revisions
            if first and first[0].strip() == "benchmark_name":
                try:
                    rev_idx = first.index("git_revision")
                except ValueError:
                    rev_idx = 5
            elif len(first) > rev_idx and first[rev_idx].strip():
                revisions.add(first[rev_idx].strip())
            for row in reader:
                if len(row) > rev_idx and row[rev_idx].strip():
                    revisions.add(row[rev_idx].strip())
        except csv.Error as exc:
            if logger is not None:
                logger.log(
                    "WARNING: csv error reading %s (%s); using the %d "
                    "revision(s) read so far" % (csv_path, exc, len(revisions))
                )
    return revisions


def count_alerts(path, logger):
    """(regressions, improvements) from the alerts.json 'alerts' list.

    Entries with direction == "improvement" count as improvements; entries
    with direction == "regression" -- or without a direction at all
    (fallback for older detect output) -- count as regressions ("alerts").
    Returns (0, 0) on any error."""
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError) as exc:
        logger.log("WARNING: could not read %s: %s" % (path, exc))
        return 0, 0
    alerts = data.get("alerts") if isinstance(data, dict) else None
    if not isinstance(alerts, list):
        logger.log("WARNING: %s has no 'alerts' list" % path)
        return 0, 0
    regressions = 0
    improvements = 0
    for entry in alerts:
        direction = entry.get("direction") if isinstance(entry, dict) else None
        if direction == "improvement":
            improvements += 1
        else:
            regressions += 1
    return regressions, improvements


def list_build_files():
    """Set of builds/datafusion-cli@* files (the bench step's inputs)."""
    return set(
        p for p in glob.glob("builds/datafusion-cli@*") if os.path.isfile(p)
    )


def prune_builds(keep, logger):
    """Keep the newest `keep` files matching builds/datafusion-cli@* (by
    mtime), delete the rest.  keep <= 0 disables pruning."""
    if keep <= 0:
        logger.log("prune: disabled (prune_builds_keep=%d)" % keep)
        return
    candidates = [
        p for p in glob.glob("builds/datafusion-cli@*") if os.path.isfile(p)
    ]
    candidates.sort(key=os.path.getmtime, reverse=True)  # newest first
    doomed = candidates[keep:]
    if not doomed:
        logger.log(
            "prune: %d build(s) present, keeping up to %d; nothing to prune"
            % (len(candidates), keep)
        )
        return
    pruned = 0
    for path in doomed:
        try:
            os.remove(path)
            pruned += 1
            logger.log("prune: deleted %s" % path)
        except OSError as exc:
            logger.log("WARNING: prune could not delete %s: %s" % (path, exc))
    logger.log(
        "prune: deleted %d old build(s), kept the newest %d of %d"
        % (pruned, min(keep, len(candidates)), len(candidates))
    )


def publish_commit_message(new_revisions, alerts):
    return "nightly: %s (%d new revision(s), %d alert(s))" % (
        utc_now().strftime("%Y-%m-%d"),
        new_revisions,
        alerts,
    )


def do_publish(config, logger, new_revisions, alerts):
    """git add / commit / pull --rebase / push.  Returns True on success.

    Refuses to run (before any git mutation) when the currently checked-out
    branch differs from publish.branch."""
    pub = config["publish"]
    remote = pub["remote"]
    branch = pub["branch"]

    logger.log("+ git rev-parse --abbrev-ref HEAD")
    try:
        proc = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError as exc:
        logger.log("publish: could not run git: %s" % exc)
        return False
    current = proc.stdout.strip()
    if proc.returncode != 0:
        logger.log(
            "publish: 'git rev-parse --abbrev-ref HEAD' failed (exit %d): %s"
            % (proc.returncode, current)
        )
        return False
    if current != branch:
        logger.log(
            "publish: current branch is '%s' but publish.branch is '%s'; "
            "refusing to commit/rebase/push from the wrong branch. Check "
            "out '%s' (or fix publish.branch in the config) and re-run."
            % (current, branch, branch)
        )
        return False

    paths = [str(p) for p in pub["paths"]]
    existing = [p for p in paths if os.path.exists(p)]
    missing = [p for p in paths if p not in existing]
    if missing:
        logger.log(
            "publish: skipping configured paths that do not exist: %s"
            % ", ".join(missing)
        )
    if not existing:
        logger.log("publish: none of the configured paths exist; nothing to publish")
        return True

    rc = run_streamed(["git", "add", "--"] + existing, logger)
    if rc != 0:
        logger.log("publish: git add failed (exit %d)" % rc)
        return False

    # Anything actually staged?
    rc = run_streamed(["git", "diff", "--cached", "--quiet"], logger)
    if rc == 0:
        logger.log("publish: nothing staged; skipping commit and push")
        return True
    if rc != 1:
        logger.log("publish: git diff --cached failed (exit %d)" % rc)
        return False

    message = publish_commit_message(new_revisions, alerts)
    rc = run_streamed(["git", "commit", "-m", message], logger)
    if rc != 0:
        logger.log("publish: git commit failed (exit %d)" % rc)
        return False

    rc = run_streamed(["git", "pull", "--rebase", remote, branch], logger)
    if rc != 0:
        run_streamed(["git", "rebase", "--abort"], logger)  # best effort
        logger.log(
            "publish: ABORTED — 'git pull --rebase %s %s' failed (exit %d), "
            "most likely a conflict with upstream changes. The rebase was "
            "aborted; the nightly commit is preserved on your local branch. "
            "To publish manually: resolve with 'git pull --rebase %s %s', "
            "then 'git push %s %s'." % (remote, branch, rc, remote, branch, remote, branch)
        )
        return False

    rc = run_streamed(["git", "push", remote, branch], logger)
    if rc != 0:
        logger.log(
            "publish: git push failed (exit %d). The commit is local; "
            "push manually with 'git push %s %s'." % (rc, remote, branch)
        )
        return False
    logger.log("publish: pushed to %s %s" % (remote, branch))
    return True


# ---------------------------------------------------------------------------
# Dry run
# ---------------------------------------------------------------------------

def print_dry_run(config, args, logger):
    """Print the exact command list per step without executing anything."""
    commands = build_commands(config)
    logger.log("DRY RUN: the following commands would be executed (nothing runs)")

    if not config["check_env"]:
        logger.log("step check_env: SKIP (check_env disabled in config)")
    elif platform.system() != "Linux":
        logger.log(
            "step check_env: SKIP (enabled, but platform is %s, not Linux)"
            % platform.system()
        )
    else:
        logger.log("step check_env: %s" % format_cmd(commands["check_env"][0]))

    skip_flags = [
        ("build", args.skip_build, "--skip-build"),
        ("bench", args.skip_bench, "--skip-bench"),
        ("detect", args.skip_detect, "--skip-detect"),
        ("report", args.skip_report, "--skip-report"),
    ]
    for step, skipped, flag in skip_flags:
        if skipped:
            logger.log("step %s: SKIP (%s)" % (step, flag))
        else:
            logger.log("step %s: %s" % (step, format_cmd(commands[step][0])))

    if args.no_publish:
        logger.log("step publish: SKIP (--no-publish)")
    elif not config["publish"]["enabled"]:
        logger.log("step publish: SKIP (publish.enabled is false)")
    else:
        for cmd in commands["publish"]:
            logger.log("step publish: %s" % format_cmd(cmd))
        logger.log(
            "publish would commit paths: %s"
            % ", ".join(str(p) for p in config["publish"]["paths"])
        )
    if not args.skip_bench and config["prune_builds_keep"] > 0:
        logger.log(
            "after bench: prune builds/datafusion-cli@* keeping the newest %d"
            % config["prune_builds_keep"]
        )


# ---------------------------------------------------------------------------
# Pipeline
# ---------------------------------------------------------------------------

def run_pipeline(config, args, logger):
    """Execute the steps.  Returns {'steps', 'ok', 'alerts', 'improvements',
    'new_revisions', 'new_builds'}."""
    steps = {}
    skip_kinds = {}
    alerts_count = 0
    improvements_count = 0
    new_revisions = 0
    new_builds = 0
    aborted = False  # set when check_env hard-fails: skip everything after

    commands = build_commands(config)

    def states_view():
        return dict((name, info["state"]) for name, info in steps.items())

    def mark_skipped(name, kind, reason):
        steps[name] = {"state": "skipped", "seconds": 0.0}
        skip_kinds[name] = kind
        logger.log("step %s: skipped (%s)" % (name, reason))

    def finish(name, ok, started):
        seconds = round(time.time() - started, 3)
        steps[name] = {"state": "ok" if ok else "failed", "seconds": seconds}
        logger.log("step %s: %s (%.1fs)" % (name, "ok" if ok else "FAILED", seconds))

    # ---- check_env ---------------------------------------------------------
    if not config["check_env"]:
        mark_skipped("check_env", "user", "disabled in config")
    elif platform.system() != "Linux":
        mark_skipped(
            "check_env",
            "user",
            "check_env is enabled but this platform is %s, not Linux; "
            "skipping the environment check" % platform.system(),
        )
    else:
        started = time.time()
        rc = run_streamed(commands["check_env"][0], logger)
        finish("check_env", rc == 0, started)
        if rc != 0:
            aborted = True
            logger.log(
                "check_env FAILED (exit %d): the box is not in a benchmarkable "
                "state; aborting the run (all remaining steps skipped). "
                "Run 'bash nightly/tune.sh' or see nightly/README.md." % rc
            )

    # ---- build -------------------------------------------------------------
    if aborted:
        mark_skipped("build", "dependency", "check_env failed")
    elif args.skip_build:
        mark_skipped("build", "user", "--skip-build")
    else:
        builds_before = list_build_files()
        started = time.time()
        rc = run_streamed(commands["build"][0], logger)
        new_builds = len(list_build_files() - builds_before)
        logger.log("build: %d new build(s) appeared in builds/" % new_builds)
        finish("build", rc == 0, started)
        if rc == 0 and new_builds == 0:
            logger.log(
                "WARNING: build succeeded but produced no new "
                "builds/datafusion-cli@* binaries -- fine on a quiet day "
                "with no new upstream commits, suspicious otherwise"
            )

    # ---- bench -------------------------------------------------------------
    if aborted:
        mark_skipped("bench", "dependency", "check_env failed")
    else:
        block = dependency_block("bench", states_view(), skip_kinds)
        if block:
            mark_skipped("bench", "dependency", block)
        elif args.skip_bench:
            mark_skipped("bench", "user", "--skip-bench")
        else:
            results_csv = os.path.join(config["results_dir"], "results.csv")
            revisions_before = read_revisions(results_csv, logger)
            started = time.time()
            rc = run_streamed(commands["bench"][0], logger)
            revisions_after = read_revisions(results_csv, logger)
            new_revisions = len(revisions_after - revisions_before)
            logger.log(
                "bench: %d new revision(s) appeared in %s"
                % (new_revisions, results_csv)
            )
            ok = rc == 0
            if ok and new_builds > 0 and new_revisions == 0:
                ok = False
                logger.log(
                    "bench FAILED effect check: %d new binar%s existed but "
                    "no revisions were added to %s"
                    % (new_builds, "y" if new_builds == 1 else "ies",
                       results_csv)
                )
            finish("bench", ok, started)
            if ok:
                prune_builds(config["prune_builds_keep"], logger)

    # ---- detect ------------------------------------------------------------
    if aborted:
        mark_skipped("detect", "dependency", "check_env failed")
    else:
        block = dependency_block("detect", states_view(), skip_kinds)
        if block:
            mark_skipped("detect", "dependency", block)
        elif args.skip_detect:
            mark_skipped("detect", "user", "--skip-detect")
        else:
            started = time.time()
            rc = run_streamed(commands["detect"][0], logger)
            ok = rc in (0, 2)
            if ok:
                alerts_count, improvements_count = count_alerts(
                    ALERTS_PATH, logger)
                if rc == 2:
                    logger.log(
                        "detect: exit code 2 — regression alerts present "
                        "(%d alert(s) in %s)" % (alerts_count, ALERTS_PATH)
                    )
            finish("detect", ok, started)

    # ---- report ------------------------------------------------------------
    if aborted:
        mark_skipped("report", "dependency", "check_env failed")
    else:
        block = dependency_block("report", states_view(), skip_kinds)
        if block:
            mark_skipped("report", "dependency", block)
        elif args.skip_report:
            mark_skipped("report", "user", "--skip-report")
        else:
            started = time.time()
            rc = run_streamed(commands["report"][0], logger)
            finish("report", rc == 0, started)

    # ---- publish -----------------------------------------------------------
    if aborted:
        mark_skipped("publish", "dependency", "check_env failed")
    elif args.no_publish:
        mark_skipped("publish", "user", "--no-publish")
    elif not config["publish"]["enabled"]:
        mark_skipped("publish", "user", "publish.enabled is false")
    else:
        block = dependency_block("publish", states_view(), skip_kinds)
        if block:
            mark_skipped("publish", "dependency", block)
        else:
            started = time.time()
            ok = do_publish(config, logger, new_revisions, alerts_count)
            finish("publish", ok, started)

    overall_ok = all(info["state"] != "failed" for info in steps.values())
    return {
        "steps": steps,
        "ok": overall_ok,
        "alerts": alerts_count,
        "improvements": improvements_count,
        "new_revisions": new_revisions,
        "new_builds": new_builds,
    }


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="Nightly benchmark orchestrator: "
        "check_env -> build -> bench -> detect -> report -> publish"
    )
    parser.add_argument("--config", default=DEFAULT_CONFIG_PATH,
                        help="Config file (default: %(default)s)")
    parser.add_argument("--days", type=int, default=None,
                        help="Override config 'days' (build commits from the last N days)")
    parser.add_argument("--dry-run", action="store_true",
                        help="Print the commands each step would run, then exit")
    parser.add_argument("--skip-build", action="store_true",
                        help="Skip the build step")
    parser.add_argument("--skip-bench", action="store_true",
                        help="Skip the bench step")
    parser.add_argument("--skip-detect", action="store_true",
                        help="Skip the detect step")
    parser.add_argument("--skip-report", action="store_true",
                        help="Skip the report step")
    parser.add_argument("--no-publish", action="store_true",
                        help="Skip the publish step regardless of config")
    return parser.parse_args(argv)


def main(argv=None, repo_root=None):
    args = parse_args(argv)

    # Run from the repo root (parent of the nightly/ directory this file
    # lives in): run_clickbench.py resolves data/ relative to the CWD.
    if repo_root is None:
        repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    os.chdir(repo_root)

    started_at = utc_iso()
    log_path = "%s/nightly-%s.log" % (LOG_DIR, utc_now().strftime("%Y-%m-%d"))
    logger = Logger(log_path)
    try:
        logger.log("nightly.py starting at %s (cwd: %s)" % (started_at, os.getcwd()))

        explicit_config = (
            os.path.normpath(args.config) != os.path.normpath(DEFAULT_CONFIG_PATH)
        )
        config = load_config(args.config, explicit_config, logger)
        if config is None:
            return 1
        config = apply_cli_overrides(config, args)

        if args.dry_run:
            print_dry_run(config, args, logger)
            return 0

        if not acquire_lock(LOCK_PATH, logger):
            return 1
        try:
            result = run_pipeline(config, args, logger)
        finally:
            release_lock(LOCK_PATH)

        status = {
            "started_at": started_at,
            "finished_at": utc_iso(),
            "ok": result["ok"],
            "steps": result["steps"],
            "alerts": result["alerts"],
            "improvements": result["improvements"],
            "new_revisions": result["new_revisions"],
            "new_builds": result["new_builds"],
        }
        os.makedirs(OUT_DIR, exist_ok=True)
        with open(STATUS_PATH, "w", encoding="utf-8") as fh:
            json.dump(status, fh, indent=2)
            fh.write("\n")
        logger.log(
            "status written to %s (ok=%s, alerts=%d, improvements=%d, "
            "new_revisions=%d, new_builds=%d)"
            % (STATUS_PATH, result["ok"], result["alerts"],
               result["improvements"], result["new_revisions"],
               result["new_builds"])
        )
        return 0 if result["ok"] else 1
    finally:
        logger.close()


if __name__ == "__main__":
    sys.exit(main())
