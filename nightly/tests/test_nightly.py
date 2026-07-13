#!/usr/bin/env python3
"""Tests for nightly/nightly.py (the orchestrator).

Run from the repo root with:
    python -m unittest discover -s nightly/tests -v
or directly:
    python nightly/tests/test_nightly.py
"""

import csv
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
NIGHTLY_DIR = os.path.dirname(TESTS_DIR)
REPO_ROOT = os.path.dirname(NIGHTLY_DIR)

RESULTS_HEADER = (
    "benchmark_name,query_name,query_type,execution_time,run_timestamp,"
    "git_revision,git_revision_timestamp,num_cores\n"
)


def _load_nightly_module():
    """Load nightly/nightly.py under a unique name (avoids clashing with the
    nightly/ package directory on sys.path)."""
    spec = importlib.util.spec_from_file_location(
        "nightly_orchestrator", os.path.join(NIGHTLY_DIR, "nightly.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


nightly = _load_nightly_module()


class TempRepoTestCase(unittest.TestCase):
    """Base: a throwaway 'repo root' directory; restores cwd afterwards."""

    def setUp(self):
        self._old_cwd = os.getcwd()
        self.root = tempfile.mkdtemp(prefix="nightly-test-")
        os.makedirs(os.path.join(self.root, "nightly"))

    def tearDown(self):
        os.chdir(self._old_cwd)
        shutil.rmtree(self.root, ignore_errors=True)

    # -- helpers ------------------------------------------------------------

    def write_stub(self, name, body):
        """Write a small python stub script into the temp root."""
        path = os.path.join(self.root, name)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(body)
        return path

    def ok_stub(self, name="ok_stub.py"):
        return self.write_stub(name, "import sys\nprint('stub ok')\nsys.exit(0)\n")

    def fail_stub(self, name="fail_stub.py"):
        return self.write_stub(name, "import sys\nprint('stub fail')\nsys.exit(1)\n")

    def bench_stub(self, name="bench_stub.py"):
        # Appends one row with a new git_revision to results/results.csv.
        return self.write_stub(name, (
            "import os\n"
            "os.makedirs('results', exist_ok=True)\n"
            "path = os.path.join('results', 'results.csv')\n"
            "new = not os.path.exists(path)\n"
            "with open(path, 'a') as fh:\n"
            "    if new:\n"
            "        fh.write(%r)\n"
            "    fh.write('clickbench_partitioned,q0,query,0.05,"
            "2026-07-14 00:00:00,abc1234,2026-07-13T00:00:00+00:00,8\\n')\n"
            "print('bench stub done')\n"
        ) % RESULTS_HEADER)

    def detect_stub(self, name="detect_stub.py", n_alerts=2, exit_code=2):
        return self.write_stub(name, (
            "import json, os, sys\n"
            "os.makedirs(os.path.join('nightly', 'out'), exist_ok=True)\n"
            "alerts = {'alerts': [{'query_name': 'q%%d' %% i}"
            " for i in range(%d)]}\n"
            "with open(os.path.join('nightly', 'out', 'alerts.json'), 'w') as fh:\n"
            "    json.dump(alerts, fh)\n"
            "print('detect stub done')\n"
            "sys.exit(%d)\n"
        ) % (n_alerts, exit_code))

    def write_config(self, overrides):
        path = os.path.join(self.root, "test_config.json")
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(overrides, fh)
        return path

    def seed_results(self, revisions=("oldrev",)):
        results_dir = os.path.join(self.root, "results")
        os.makedirs(results_dir, exist_ok=True)
        with open(os.path.join(results_dir, "results.csv"), "w", encoding="utf-8") as fh:
            fh.write(RESULTS_HEADER)
            for rev in revisions:
                fh.write(
                    "clickbench_partitioned,q0,query,0.03,"
                    "2026-03-30 13:52:55,%s,2025-09-13T16:17:34+08:00,8\n" % rev
                )

    def read_status(self):
        path = os.path.join(self.root, "nightly", "out", "status.json")
        self.assertTrue(os.path.exists(path), "status.json not written")
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh)

    def log_text(self):
        logs_dir = os.path.join(self.root, "nightly", "logs")
        texts = []
        for name in sorted(os.listdir(logs_dir)):
            with open(os.path.join(logs_dir, name), "r", encoding="utf-8") as fh:
                texts.append(fh.read())
        return "\n".join(texts)

    def init_git_repo(self, branch="main"):
        """Turn the temp root into a git repo with one commit on `branch`."""
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "t@example.com"],
                       cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "t"],
                       cwd=self.root, check=True)
        subprocess.run(["git", "commit", "--allow-empty", "-m", "init", "-q"],
                       cwd=self.root, check=True)
        subprocess.run(["git", "branch", "-M", branch], cwd=self.root, check=True)

    def run_main(self, extra_argv=(), config_overrides=None):
        if config_overrides is None:
            config_overrides = {}
        config_path = self.write_config(config_overrides)
        argv = ["--config", config_path] + list(extra_argv)
        return nightly.main(argv, repo_root=self.root)


# ---------------------------------------------------------------------------
# Config merging
# ---------------------------------------------------------------------------

class TestConfigMerge(TempRepoTestCase):

    def test_defaults_when_file_has_partial_keys(self):
        path = self.write_config({"days": 9, "detect": {"window": 10},
                                  "publish": {"enabled": True}})
        logger = nightly.Logger(None)
        config = nightly.load_config(path, explicit=True, logger=logger)
        # file overrides defaults
        self.assertEqual(config["days"], 9)
        self.assertEqual(config["detect"]["window"], 10)
        self.assertTrue(config["publish"]["enabled"])
        # untouched defaults survive (deep merge, not replace)
        self.assertEqual(config["detect"]["pvalue"], 0.05)
        self.assertEqual(config["detect"]["permutations"], 199)
        self.assertEqual(config["publish"]["remote"], "origin")
        self.assertEqual(config["publish"]["branch"], "main")
        self.assertEqual(config["num_builds"], 2)

    def test_cli_overrides_config_file(self):
        path = self.write_config({"days": 9})
        logger = nightly.Logger(None)
        config = nightly.load_config(path, explicit=True, logger=logger)
        args = nightly.parse_args(["--days", "3"])
        config = nightly.apply_cli_overrides(config, args)
        self.assertEqual(config["days"], 3)

    def test_cli_none_keeps_config_value(self):
        path = self.write_config({"days": 9})
        logger = nightly.Logger(None)
        config = nightly.load_config(path, explicit=True, logger=logger)
        args = nightly.parse_args([])
        config = nightly.apply_cli_overrides(config, args)
        self.assertEqual(config["days"], 9)

    def test_explicit_missing_config_is_error(self):
        logger = nightly.Logger(None)
        config = nightly.load_config(
            os.path.join(self.root, "nope.json"), explicit=True, logger=logger
        )
        self.assertIsNone(config)

    def test_missing_default_config_falls_back_to_defaults(self):
        logger = nightly.Logger(None)
        config = nightly.load_config(
            os.path.join(self.root, "nightly", "config.json"),
            explicit=False, logger=logger,
        )
        self.assertEqual(config["days"], nightly.DEFAULT_CONFIG["days"])

    def test_repo_config_json_matches_documented_defaults(self):
        # The shipped nightly/config.json must be valid JSON and agree with
        # the built-in defaults for the documented keys.
        with open(os.path.join(NIGHTLY_DIR, "config.json"), "r", encoding="utf-8") as fh:
            shipped = json.load(fh)
        for key, value in shipped.items():
            self.assertEqual(value, nightly.DEFAULT_CONFIG[key],
                             "config.json key %r disagrees with defaults" % key)
        self.assertFalse(shipped["publish"]["enabled"])


# ---------------------------------------------------------------------------
# Command construction (pure function)
# ---------------------------------------------------------------------------

class TestBuildCommands(unittest.TestCase):

    def setUp(self):
        self.config = nightly.deep_merge(nightly.DEFAULT_CONFIG, {})

    def test_build_command(self):
        cmds = nightly.build_commands(self.config)
        self.assertEqual(cmds["build"], [[
            sys.executable, "ensure_datafusion_cli.py",
            "--days", "2", "--num-builds", "2",
            "--datafusion-dir", "datafusion",
        ]])

    def test_bench_command(self):
        cmds = nightly.build_commands(self.config)
        self.assertEqual(cmds["bench"], [[
            sys.executable, "benchmark.py",
            "--output-dir", "results", "--benchmarks", "clickbench",
        ]])

    def test_detect_command(self):
        cmds = nightly.build_commands(self.config)
        self.assertEqual(cmds["detect"], [[
            sys.executable, "nightly/detect.py",
            "--results-dir", "results",
            "--output-dir", "nightly/out",
            "--window", "60",
            "--min-delta-pct", "2.0",
            "--iqr-multiplier", "3.0",
            "--pvalue", "0.05",
            "--permutations", "199",
        ]])

    def test_report_command(self):
        cmds = nightly.build_commands(self.config)
        self.assertEqual(cmds["report"], [[
            sys.executable, "report.py", "--results-dir", "results",
        ]])

    def test_publish_commands(self):
        cmds = nightly.build_commands(self.config)
        publish = cmds["publish"]
        self.assertEqual(publish[0], ["git", "add", "--",
                                      "results", "docs", "nightly/out"])
        self.assertEqual(publish[1][:3], ["git", "commit", "-m"])
        self.assertEqual(publish[2], ["git", "pull", "--rebase", "origin", "main"])
        self.assertEqual(publish[3], ["git", "push", "origin", "main"])

    def test_config_values_flow_into_commands(self):
        config = nightly.deep_merge(self.config, {
            "days": 5, "num_builds": 3, "results_dir": "other_results",
            "detect": {"window": 30},
            "publish": {"remote": "upstream", "branch": "gh-pages"},
        })
        cmds = nightly.build_commands(config)
        self.assertIn("--days", cmds["build"][0])
        self.assertEqual(cmds["build"][0][cmds["build"][0].index("--days") + 1], "5")
        self.assertIn("other_results", cmds["bench"][0])
        detect = cmds["detect"][0]
        self.assertEqual(detect[detect.index("--window") + 1], "30")
        self.assertEqual(cmds["publish"][3], ["git", "push", "upstream", "gh-pages"])

    def test_python_scripts_use_sys_executable(self):
        cmds = nightly.build_commands(self.config)
        for step in ("build", "bench", "detect", "report"):
            self.assertEqual(cmds[step][0][0], sys.executable,
                             "%s must use sys.executable" % step)


# ---------------------------------------------------------------------------
# Lock
# ---------------------------------------------------------------------------

class TestLock(TempRepoTestCase):

    def lock_path(self):
        return os.path.join(self.root, "nightly", ".lock")

    def test_acquire_and_release(self):
        logger = nightly.Logger(None)
        path = self.lock_path()
        self.assertTrue(nightly.acquire_lock(path, logger))
        self.assertTrue(os.path.exists(path))
        with open(path, "r", encoding="utf-8") as fh:
            content = fh.read()
        self.assertIn("pid=%d" % os.getpid(), content)
        self.assertIn("started_at=", content)
        nightly.release_lock(path)
        self.assertFalse(os.path.exists(path))

    def test_second_acquire_fails_while_held(self):
        logger = nightly.Logger(None)
        path = self.lock_path()
        self.assertTrue(nightly.acquire_lock(path, logger))
        self.assertFalse(nightly.acquire_lock(path, logger))
        self.assertTrue(os.path.exists(path))
        nightly.release_lock(path)

    def test_stale_lock_is_broken(self):
        logger = nightly.Logger(None)
        path = self.lock_path()
        stale_epoch = time.time() - 25 * 3600
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("pid=1\nstarted_at=old\nstarted_epoch=%.3f\n" % stale_epoch)
        self.assertTrue(nightly.acquire_lock(path, logger))
        with open(path, "r", encoding="utf-8") as fh:
            content = fh.read()
        self.assertIn("pid=%d" % os.getpid(), content)
        nightly.release_lock(path)

    def test_stale_lock_by_mtime_when_content_unparseable(self):
        logger = nightly.Logger(None)
        path = self.lock_path()
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("garbage\n")
        old = time.time() - 25 * 3600
        os.utime(path, (old, old))
        self.assertTrue(nightly.acquire_lock(path, logger))
        nightly.release_lock(path)

    def test_release_is_idempotent(self):
        nightly.release_lock(self.lock_path())  # no lock: must not raise

    def test_release_leaves_foreign_lock_alone(self):
        # Simulates another process breaking our (stale) lock and taking it
        # over: release must notice the content is no longer ours and NOT
        # delete the foreign lock.
        logger = nightly.Logger(None)
        path = self.lock_path()
        self.assertTrue(nightly.acquire_lock(path, logger))
        foreign = "pid=424242\nstarted_at=x\nstarted_epoch=%.3f\n" % time.time()
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(foreign)
        nightly.release_lock(path)
        self.assertTrue(os.path.exists(path), "foreign lock must survive release")
        with open(path, "r", encoding="utf-8") as fh:
            self.assertEqual(fh.read(), foreign)
        os.remove(path)

    def test_release_without_acquire_leaves_lock_alone(self):
        path = self.lock_path()
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("pid=1\nstarted_at=x\nstarted_epoch=%.3f\n" % time.time())
        nightly.release_lock(path)  # we never acquired it
        self.assertTrue(os.path.exists(path))

    def test_break_stale_lock_removes_on_content_match(self):
        logger = nightly.Logger(None)
        path = self.lock_path()
        stale = "pid=1\nstarted_at=old\nstarted_epoch=1.0\n"
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(stale)
        self.assertTrue(nightly._break_stale_lock(path, stale, logger))
        self.assertFalse(os.path.exists(path))
        self.assertEqual(self.leftover_lock_temps(), [])

    def test_break_stale_lock_content_mismatch_restores_and_backs_off(self):
        # The lock changed hands between the age check and the rename: the
        # breaker must put the fresh lock back and report failure.
        logger = nightly.Logger(None)
        path = self.lock_path()
        fresh = "pid=7\nstarted_at=now\nstarted_epoch=%.3f\n" % time.time()
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(fresh)
        inspected = "pid=1\nstarted_at=old\nstarted_epoch=1.0\n"
        self.assertFalse(nightly._break_stale_lock(path, inspected, logger))
        self.assertTrue(os.path.exists(path))
        with open(path, "r", encoding="utf-8") as fh:
            self.assertEqual(fh.read(), fresh)
        self.assertEqual(self.leftover_lock_temps(), [])

    def test_break_stale_lock_lost_race_backs_off(self):
        # The lock is already gone (another breaker's os.replace won):
        # FileNotFoundError -> back off without creating anything.
        logger = nightly.Logger(None)
        path = self.lock_path()
        self.assertFalse(nightly._break_stale_lock(path, "whatever", logger))
        self.assertFalse(os.path.exists(path))
        self.assertEqual(self.leftover_lock_temps(), [])

    def leftover_lock_temps(self):
        lock_dir = os.path.dirname(self.lock_path())
        return [n for n in os.listdir(lock_dir)
                if n.startswith(".lock") and n != ".lock"]


# ---------------------------------------------------------------------------
# Dependency map (pure function)
# ---------------------------------------------------------------------------

class TestDependencyMap(unittest.TestCase):

    def test_bench_skipped_when_build_failed(self):
        reason = nightly.dependency_block("bench", {"build": "failed"}, {})
        self.assertIsNotNone(reason)

    def test_bench_runs_when_build_user_skipped(self):
        reason = nightly.dependency_block(
            "bench", {"build": "skipped"}, {"build": "user"})
        self.assertIsNone(reason)

    def test_bench_skipped_when_build_dependency_skipped(self):
        reason = nightly.dependency_block(
            "bench", {"build": "skipped"}, {"build": "dependency"})
        self.assertIsNotNone(reason)

    def test_detect_soft_runs_when_bench_skipped_for_any_reason(self):
        for kind in ("user", "dependency"):
            reason = nightly.dependency_block(
                "detect", {"bench": "skipped"}, {"bench": kind})
            self.assertIsNone(reason, "detect blocked for kind=%s" % kind)

    def test_detect_and_report_skipped_when_bench_failed(self):
        for step in ("detect", "report"):
            reason = nightly.dependency_block(step, {"bench": "failed"}, {})
            self.assertIsNotNone(reason)

    def test_report_soft_runs_when_bench_skipped(self):
        reason = nightly.dependency_block(
            "report", {"bench": "skipped"}, {"bench": "dependency"})
        self.assertIsNone(reason)

    def test_publish_hard_on_report(self):
        self.assertIsNotNone(
            nightly.dependency_block("publish", {"report": "failed"}, {}))
        self.assertIsNotNone(nightly.dependency_block(
            "publish", {"report": "skipped"}, {"report": "dependency"}))
        self.assertIsNone(nightly.dependency_block(
            "publish", {"report": "skipped"}, {"report": "user"}))
        self.assertIsNone(
            nightly.dependency_block("publish", {"report": "ok"}, {}))

    def test_steps_without_deps_never_blocked(self):
        self.assertIsNone(nightly.dependency_block("build", {}, {}))
        self.assertIsNone(nightly.dependency_block("check_env", {}, {}))


# ---------------------------------------------------------------------------
# Helpers: revisions / alerts / prune
# ---------------------------------------------------------------------------

class TestHelpers(TempRepoTestCase):

    def test_read_revisions_with_header(self):
        self.seed_results(revisions=("aaa", "bbb", "aaa"))
        revs = nightly.read_revisions(
            os.path.join(self.root, "results", "results.csv"))
        self.assertEqual(revs, {"aaa", "bbb"})

    def test_read_revisions_headerless(self):
        results_dir = os.path.join(self.root, "results")
        os.makedirs(results_dir)
        path = os.path.join(results_dir, "results.csv")
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("clickbench_partitioned,q0,query,0.03,"
                     "2026-03-30 13:52:55,noheader1,2025-09-13T16:17:34+08:00,8\n")
            fh.write("clickbench_partitioned,q1,query,0.04,"
                     "2026-03-30 13:52:56,noheader2,2025-09-13T16:17:34+08:00,8\n")
        self.assertEqual(nightly.read_revisions(path), {"noheader1", "noheader2"})

    def test_read_revisions_missing_file(self):
        self.assertEqual(
            nightly.read_revisions(os.path.join(self.root, "results", "results.csv")),
            set())

    def test_read_revisions_tolerates_csv_error(self):
        # A corrupted row (NUL byte, and an oversized field for pythons
        # where NUL no longer raises) must not crash the read; revisions
        # collected before the bad row are returned.
        results_dir = os.path.join(self.root, "results")
        os.makedirs(results_dir)
        path = os.path.join(results_dir, "results.csv")
        big = "x" * (csv.field_size_limit() + 1)
        with open(path, "w", newline="", encoding="utf-8") as fh:
            fh.write(RESULTS_HEADER)
            fh.write("clickbench_partitioned,q0,query,0.03,"
                     "2026-03-30 13:52:55,goodrev,2025-09-13T16:17:34+08:00,8\n")
            fh.write("bad\x00%s,q1,query,0.03,t,badrev,ts,8\n" % big)
        revs = nightly.read_revisions(path, nightly.Logger(None))
        self.assertEqual(revs, {"goodrev"})

    def test_count_alerts_without_direction_counts_as_regressions(self):
        out_dir = os.path.join(self.root, "nightly", "out")
        os.makedirs(out_dir)
        path = os.path.join(out_dir, "alerts.json")
        with open(path, "w", encoding="utf-8") as fh:
            json.dump({"alerts": [{"q": 1}, {"q": 2}, {"q": 3}]}, fh)
        logger = nightly.Logger(None)
        self.assertEqual(nightly.count_alerts(path, logger), (3, 0))
        self.assertEqual(
            nightly.count_alerts(os.path.join(out_dir, "missing.json"), logger),
            (0, 0))

    def test_count_alerts_splits_regressions_and_improvements(self):
        out_dir = os.path.join(self.root, "nightly", "out")
        os.makedirs(out_dir)
        path = os.path.join(out_dir, "alerts.json")
        with open(path, "w", encoding="utf-8") as fh:
            json.dump({"alerts": [
                {"q": 1, "direction": "regression"},
                {"q": 2, "direction": "improvement"},
                {"q": 3, "direction": "improvement"},
                {"q": 4},  # legacy entry without direction -> regression
            ]}, fh)
        logger = nightly.Logger(None)
        self.assertEqual(nightly.count_alerts(path, logger), (2, 2))

    def test_prune_builds_keeps_newest_k(self):
        builds = os.path.join(self.root, "builds")
        os.makedirs(builds)
        now = time.time()
        # no colons in the timestamp part: invalid in Windows filenames
        names = ["datafusion-cli@sha%d@2025-01-0%dT000000" % (i, i + 1)
                 for i in range(5)]
        for i, name in enumerate(names):
            path = os.path.join(builds, name)
            with open(path, "w") as fh:
                fh.write("x")
            stamp = now - (len(names) - i) * 60  # names[4] is newest
            os.utime(path, (stamp, stamp))
        os.chdir(self.root)
        logger = nightly.Logger(None)
        nightly.prune_builds(2, logger)
        remaining = sorted(os.listdir(builds))
        self.assertEqual(remaining, sorted(names[3:]))

    def test_prune_builds_zero_disables(self):
        builds = os.path.join(self.root, "builds")
        os.makedirs(builds)
        for i in range(3):
            with open(os.path.join(builds, "datafusion-cli@s%d@t" % i), "w") as fh:
                fh.write("x")
        os.chdir(self.root)
        nightly.prune_builds(0, nightly.Logger(None))
        self.assertEqual(len(os.listdir(builds)), 3)

    def test_publish_commit_message_format(self):
        msg = nightly.publish_commit_message(3, 1)
        self.assertRegex(msg, r"^nightly: \d{4}-\d{2}-\d{2} "
                              r"\(3 new revision\(s\), 1 alert\(s\)\)$")


# ---------------------------------------------------------------------------
# Pipeline end-to-end (stubbed steps in a temp repo root)
# ---------------------------------------------------------------------------

class TestPipeline(TempRepoTestCase):

    def stub_scripts(self, build=None, bench=None, detect=None, report=None):
        return {
            "build": build or self.ok_stub("build_stub.py"),
            "bench": bench or self.bench_stub(),
            "detect": detect or self.detect_stub(),
            "report": report or self.ok_stub("report_stub.py"),
        }

    def test_all_ok_run_status_json_shape(self):
        self.seed_results(revisions=("oldrev",))
        rc = self.run_main(config_overrides={"scripts": self.stub_scripts()})
        self.assertEqual(rc, 0)
        status = self.read_status()
        self.assertEqual(
            set(status.keys()),
            {"started_at", "finished_at", "ok", "steps", "alerts",
             "improvements", "new_revisions", "new_builds"})
        self.assertTrue(status["ok"])
        self.assertEqual(status["alerts"], 2)        # detect stub: 2 alerts, exit 2
        self.assertEqual(status["improvements"], 0)
        self.assertEqual(status["new_revisions"], 1)  # bench stub adds abc1234
        self.assertEqual(status["new_builds"], 0)     # build stub builds nothing
        self.assertEqual(
            set(status["steps"].keys()),
            {"check_env", "build", "bench", "detect", "report", "publish"})
        for name, info in status["steps"].items():
            self.assertEqual(set(info.keys()), {"state", "seconds"},
                             "step %s shape" % name)
            self.assertIn(info["state"], ("ok", "failed", "skipped"))
            self.assertIsInstance(info["seconds"], (int, float))
        self.assertEqual(status["steps"]["check_env"]["state"], "skipped")
        self.assertEqual(status["steps"]["build"]["state"], "ok")
        self.assertEqual(status["steps"]["bench"]["state"], "ok")
        self.assertEqual(status["steps"]["detect"]["state"], "ok")
        self.assertEqual(status["steps"]["report"]["state"], "ok")
        self.assertEqual(status["steps"]["publish"]["state"], "skipped")
        # lock released, log written
        self.assertFalse(os.path.exists(os.path.join(self.root, "nightly", ".lock")))
        logs = os.listdir(os.path.join(self.root, "nightly", "logs"))
        self.assertTrue(any(f.startswith("nightly-") and f.endswith(".log")
                            for f in logs))

    def test_build_failure_skips_bench_but_report_still_attempted(self):
        # build points at a nonexistent script -> python exits non-zero.
        scripts = self.stub_scripts(
            build=os.path.join(self.root, "no_such_script.py"))
        rc = self.run_main(config_overrides={"scripts": scripts})
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertFalse(status["ok"])
        self.assertEqual(status["steps"]["build"]["state"], "failed")
        self.assertEqual(status["steps"]["bench"]["state"], "skipped")
        # soft deps: bench merely skipped -> detect and report still run
        self.assertEqual(status["steps"]["detect"]["state"], "ok")
        self.assertEqual(status["steps"]["report"]["state"], "ok")
        self.assertEqual(status["new_revisions"], 0)

    def test_bench_failure_cascades_to_publish(self):
        scripts = self.stub_scripts(bench=self.fail_stub("bench_fail.py"))
        rc = self.run_main(config_overrides={
            "scripts": scripts,
            "publish": {"enabled": True},
        })
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertFalse(status["ok"])
        self.assertEqual(status["steps"]["bench"]["state"], "failed")
        self.assertEqual(status["steps"]["detect"]["state"], "skipped")
        self.assertEqual(status["steps"]["report"]["state"], "skipped")
        # publish is enabled but hard-depends on report, which was skipped
        # due to an upstream failure -> publish must not run.
        self.assertEqual(status["steps"]["publish"]["state"], "skipped")

    def test_skip_flags_mark_steps_skipped(self):
        self.seed_results()
        rc = self.run_main(
            extra_argv=["--skip-build", "--skip-bench", "--skip-detect",
                        "--skip-report"],
            config_overrides={"scripts": self.stub_scripts()})
        self.assertEqual(rc, 0)
        status = self.read_status()
        self.assertTrue(status["ok"])
        for step in ("build", "bench", "detect", "report"):
            self.assertEqual(status["steps"][step]["state"], "skipped")
        self.assertEqual(status["alerts"], 0)
        self.assertEqual(status["new_revisions"], 0)

    def test_skip_build_still_runs_bench(self):
        self.seed_results()
        rc = self.run_main(extra_argv=["--skip-build"],
                           config_overrides={"scripts": self.stub_scripts()})
        self.assertEqual(rc, 0)
        status = self.read_status()
        self.assertEqual(status["steps"]["build"]["state"], "skipped")
        self.assertEqual(status["steps"]["bench"]["state"], "ok")

    def test_detect_exit_zero_is_ok_without_alert_flag(self):
        scripts = self.stub_scripts(
            detect=self.detect_stub("detect_ok.py", n_alerts=0, exit_code=0))
        rc = self.run_main(config_overrides={"scripts": scripts})
        self.assertEqual(rc, 0)
        status = self.read_status()
        self.assertEqual(status["steps"]["detect"]["state"], "ok")
        self.assertEqual(status["alerts"], 0)

    def test_detect_exit_one_is_failure(self):
        scripts = self.stub_scripts(detect=self.fail_stub("detect_fail.py"))
        rc = self.run_main(config_overrides={"scripts": scripts})
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertEqual(status["steps"]["detect"]["state"], "failed")
        # report is independent of detect: still runs
        self.assertEqual(status["steps"]["report"]["state"], "ok")

    def test_bench_failed_when_new_builds_but_no_new_revisions(self):
        # Silent green night: build produces a new binary, bench exits 0 but
        # adds no revisions -> the bench step must be marked failed.
        self.seed_results()
        build = self.write_stub("build_makes_binary.py", (
            "import os\n"
            "os.makedirs('builds', exist_ok=True)\n"
            "with open(os.path.join('builds', 'datafusion-cli@fresh@t0'), 'w') as fh:\n"
            "    fh.write('x')\n"
            "print('build stub made a binary')\n"
        ))
        scripts = self.stub_scripts(build=build,
                                    bench=self.ok_stub("bench_noop.py"))
        rc = self.run_main(config_overrides={"scripts": scripts})
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertFalse(status["ok"])
        self.assertEqual(status["steps"]["build"]["state"], "ok")
        self.assertEqual(status["steps"]["bench"]["state"], "failed")
        self.assertEqual(status["new_builds"], 1)
        self.assertEqual(status["new_revisions"], 0)
        # the effect-check failure cascades exactly like a bench exit != 0
        self.assertEqual(status["steps"]["detect"]["state"], "skipped")
        self.assertEqual(status["steps"]["report"]["state"], "skipped")
        self.assertIn("bench FAILED effect check", self.log_text())

    def test_quiet_day_no_new_builds_no_new_revisions_is_ok(self):
        # Zero new upstream commits: build and bench legitimately do nothing.
        # That must stay green, with a prominent WARNING after the build.
        self.seed_results()
        scripts = self.stub_scripts(bench=self.ok_stub("bench_noop.py"))
        rc = self.run_main(config_overrides={"scripts": scripts})
        self.assertEqual(rc, 0)
        status = self.read_status()
        self.assertTrue(status["ok"])
        self.assertEqual(status["steps"]["build"]["state"], "ok")
        self.assertEqual(status["steps"]["bench"]["state"], "ok")
        self.assertEqual(status["new_builds"], 0)
        self.assertEqual(status["new_revisions"], 0)
        self.assertIn("WARNING: build succeeded but produced no new",
                      self.log_text())

    def test_status_alerts_are_regressions_only(self):
        self.seed_results()
        mixed = self.write_stub("detect_mixed.py", (
            "import json, os, sys\n"
            "os.makedirs(os.path.join('nightly', 'out'), exist_ok=True)\n"
            "alerts = {'alerts': ["
            "{'query_name': 'q0', 'direction': 'regression'}, "
            "{'query_name': 'q1', 'direction': 'improvement'}, "
            "{'query_name': 'q2', 'direction': 'improvement'}, "
            "{'query_name': 'q3'}]}\n"
            "with open(os.path.join('nightly', 'out', 'alerts.json'), 'w') as fh:\n"
            "    json.dump(alerts, fh)\n"
            "sys.exit(2)\n"
        ))
        rc = self.run_main(
            config_overrides={"scripts": self.stub_scripts(detect=mixed)})
        self.assertEqual(rc, 0)
        status = self.read_status()
        # 'alerts' (read by nightly.sh) = regressions + legacy no-direction
        self.assertEqual(status["alerts"], 2)
        self.assertEqual(status["improvements"], 2)

    def test_prune_after_successful_bench(self):
        builds = os.path.join(self.root, "builds")
        os.makedirs(builds)
        now = time.time()
        for i in range(4):
            path = os.path.join(builds, "datafusion-cli@sha%d@t%d" % (i, i))
            with open(path, "w") as fh:
                fh.write("x")
            stamp = now - (4 - i) * 60
            os.utime(path, (stamp, stamp))
        rc = self.run_main(config_overrides={
            "scripts": self.stub_scripts(),
            "prune_builds_keep": 2,
        })
        self.assertEqual(rc, 0)
        self.assertEqual(len(os.listdir(builds)), 2)

    def test_lock_held_prevents_run(self):
        lock = os.path.join(self.root, "nightly", ".lock")
        with open(lock, "w", encoding="utf-8") as fh:
            fh.write("pid=1\nstarted_at=x\nstarted_epoch=%.3f\n" % time.time())
        rc = self.run_main(config_overrides={"scripts": self.stub_scripts()})
        self.assertEqual(rc, 1)
        # foreign lock must not be deleted
        self.assertTrue(os.path.exists(lock))
        self.assertFalse(
            os.path.exists(os.path.join(self.root, "nightly", "out", "status.json")))

    def test_stale_lock_broken_and_run_proceeds(self):
        lock = os.path.join(self.root, "nightly", ".lock")
        stale = time.time() - 25 * 3600
        with open(lock, "w", encoding="utf-8") as fh:
            fh.write("pid=1\nstarted_at=x\nstarted_epoch=%.3f\n" % stale)
        rc = self.run_main(config_overrides={"scripts": self.stub_scripts()})
        self.assertEqual(rc, 0)
        self.assertFalse(os.path.exists(lock))

    @unittest.skipIf(shutil.which("git") is None, "git not on PATH")
    def test_publish_attempted_when_report_user_skipped(self):
        # publish hard-depends on report, but a user --skip-report must NOT
        # block it.  The temp root is a git repo on the publish branch but
        # with no remote, so publish passes the branch guard and gets as far
        # as 'git pull --rebase origin main', failing with the actionable
        # abort message.
        self.init_git_repo(branch="main")
        self.seed_results()
        rc = self.run_main(
            extra_argv=["--skip-report"],
            config_overrides={
                "scripts": self.stub_scripts(),
                "publish": {"enabled": True, "paths": ["results"]},
            })
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertEqual(status["steps"]["report"]["state"], "skipped")
        # attempted (i.e. not "skipped") and failed at pull --rebase
        self.assertEqual(status["steps"]["publish"]["state"], "failed")

    @unittest.skipIf(shutil.which("git") is None, "git not on PATH")
    def test_publish_branch_guard_blocks_wrong_branch(self):
        self.init_git_repo(branch="not-the-publish-branch")
        self.seed_results()
        os.chdir(self.root)
        config = nightly.deep_merge(nightly.DEFAULT_CONFIG, {
            "publish": {"enabled": True, "paths": ["results"]},
        })
        ok = nightly.do_publish(config, nightly.Logger(None), 1, 0)
        self.assertFalse(ok)
        # the guard must fire before any git mutation: still exactly one
        # commit and nothing staged
        log = subprocess.run(["git", "log", "--oneline"], cwd=self.root,
                             capture_output=True, text=True, check=True)
        self.assertEqual(len(log.stdout.strip().splitlines()), 1)
        staged = subprocess.run(["git", "diff", "--cached", "--quiet"],
                                cwd=self.root)
        self.assertEqual(staged.returncode, 0, "guard must not stage anything")

    @unittest.skipIf(shutil.which("git") is None, "git not on PATH")
    def test_publish_branch_guard_fails_step_in_pipeline(self):
        self.init_git_repo(branch="nightly-wip")
        self.seed_results()
        rc = self.run_main(config_overrides={
            "scripts": self.stub_scripts(),
            "publish": {"enabled": True, "paths": ["results"]},
        })
        self.assertEqual(rc, 1)
        status = self.read_status()
        self.assertEqual(status["steps"]["publish"]["state"], "failed")
        self.assertIn("refusing to commit/rebase/push", self.log_text())


# ---------------------------------------------------------------------------
# Dry run
# ---------------------------------------------------------------------------

class TestDryRun(TempRepoTestCase):

    def dry_run_log_text(self):
        logs_dir = os.path.join(self.root, "nightly", "logs")
        texts = []
        for name in os.listdir(logs_dir):
            with open(os.path.join(logs_dir, name), "r", encoding="utf-8") as fh:
                texts.append(fh.read())
        return "\n".join(texts)

    def test_dry_run_writes_no_files_except_log(self):
        rc = self.run_main(extra_argv=["--dry-run"],
                           config_overrides={"scripts": {}})
        self.assertEqual(rc, 0)
        self.assertFalse(os.path.exists(os.path.join(self.root, "nightly", ".lock")))
        self.assertFalse(
            os.path.exists(os.path.join(self.root, "nightly", "out")))
        self.assertTrue(
            os.path.isdir(os.path.join(self.root, "nightly", "logs")))

    def test_dry_run_prints_commands_and_honors_cli_days(self):
        rc = self.run_main(extra_argv=["--dry-run", "--days", "7"],
                           config_overrides={"days": 2})
        self.assertEqual(rc, 0)
        text = self.dry_run_log_text()
        self.assertIn("DRY RUN", text)
        self.assertIn("ensure_datafusion_cli.py", text)
        self.assertIn("--days 7", text)  # CLI overrode config's 2
        self.assertIn("benchmark.py", text)
        self.assertIn("nightly/detect.py", text)
        self.assertIn("report.py", text)
        self.assertIn("step publish: SKIP (publish.enabled is false)", text)

    def test_dry_run_shows_publish_commands_when_enabled(self):
        rc = self.run_main(
            extra_argv=["--dry-run"],
            config_overrides={"publish": {"enabled": True}})
        self.assertEqual(rc, 0)
        text = self.dry_run_log_text()
        self.assertIn("git add -- results docs nightly/out", text)
        self.assertIn("git pull --rebase origin main", text)
        self.assertIn("git push origin main", text)
        self.assertIn("publish would commit paths: results, docs, nightly/out", text)

    def test_dry_run_no_publish_flag_wins_over_config(self):
        rc = self.run_main(
            extra_argv=["--dry-run", "--no-publish"],
            config_overrides={"publish": {"enabled": True}})
        self.assertEqual(rc, 0)
        self.assertIn("step publish: SKIP (--no-publish)", self.dry_run_log_text())

    def test_dry_run_shows_skip_flags(self):
        rc = self.run_main(extra_argv=["--dry-run", "--skip-bench"])
        self.assertEqual(rc, 0)
        self.assertIn("step bench: SKIP (--skip-bench)", self.dry_run_log_text())

    def test_real_repo_dry_run_subprocess(self):
        # End-to-end: run the real nightly.py with the shipped config.json
        # from the actual repo root.  Dry-run executes nothing.
        proc = subprocess.run(
            [sys.executable, os.path.join("nightly", "nightly.py"), "--dry-run"],
            cwd=REPO_ROOT, capture_output=True, text=True, timeout=120)
        self.assertEqual(proc.returncode, 0,
                         "stdout:\n%s\nstderr:\n%s" % (proc.stdout, proc.stderr))
        out = proc.stdout
        self.assertIn("DRY RUN", out)
        self.assertIn("ensure_datafusion_cli.py", out)
        self.assertIn("--days 2", out)
        self.assertIn("--num-builds 2", out)
        self.assertIn("benchmark.py --output-dir results --benchmarks clickbench", out)
        self.assertIn("nightly/detect.py", out)
        self.assertIn("report.py --results-dir results", out)
        self.assertIn("step publish: SKIP (publish.enabled is false)", out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
