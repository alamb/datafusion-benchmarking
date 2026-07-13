# Nightly Benchmark Automation

Operator manual for the `nightly/` pipeline. Interface details live in
[DESIGN.md](DESIGN.md); this document is about installing, running, and
interpreting it.

## What this is

This directory closes the "Rerun the benchmarks on a regular basis (cron
job?)" TODO from the [top-level README](../README.md). It addresses the
remaining gap of [apache/datafusion#5504](https://github.com/apache/datafusion/issues/5504)
("run the benchmarks regularly and track history") and the follow-on concern
alamb raised there — that running benchmarks is the easy half, and the results
are only useful if the history is kept and *analyzed* so regressions are
actually noticed — as well as the "trackable over time" goal of
[EPIC apache/datafusion#21165](https://github.com/apache/datafusion/issues/21165).
Concretely: a cron/systemd-driven orchestrator builds recent `main` commits,
runs the existing benchmark pipeline, runs statistical regression detection
over the accumulated history, regenerates the dashboard, and (optionally)
publishes the results — plain cron + bash + python, no CI-vendor lock-in.

Pipeline (each stage skippable, `--dry-run` prints commands without executing):

```
cron / systemd timer
  └── nightly/nightly.sh            flock guard, env setup, failure logging
        └── nightly/nightly.py      orchestrator (python3, stdlib only)
              1. check_env          (optional) nightly/check_env.sh must pass on Linux
              2. build              python ensure_datafusion_cli.py --days N
              3. bench              python benchmark.py --output-dir results
              4. detect             python nightly/detect.py --results-dir results
              5. report             python report.py --results-dir results
              6. publish            git add/commit/pull --rebase/push  (default OFF)
              7. status             nightly/out/status.json + log file
```

## Quick start on a fresh Linux box

Prerequisites: git, a Rust toolchain (`rustup` — builds `datafusion-cli` from
source), and Python ≥ 3.9. `nightly/nightly.py` and `nightly/detect.py` are
**stdlib-only**; the only pip installs needed are for the existing `report.py`:

```shell
python3 -m pip install pandas datafusion
```

(`report.py`'s docstring also lists `matplotlib seaborn numpy plotly`; only
`pandas` and `datafusion` are imported today — the Plotly JS in the generated
dashboard is loaded from a CDN, not from the Python package.)

1. Clone this repo, and clone DataFusion *inside* it (the default
   `datafusion_dir` in `nightly/config.json` is `datafusion`):

   ```shell
   git clone https://github.com/alamb/datafusion-benchmarking.git
   cd datafusion-benchmarking
   git clone https://github.com/apache/datafusion.git
   ```

2. Provision the ClickBench data using DataFusion's own benchmark tooling,
   then symlink it so `data/hits_partitioned/` resolves from the repo root
   (`run_clickbench.py` references `data/` relative to its working directory):

   ```shell
   (cd datafusion/benchmarks && ./bench.sh data clickbench_partitioned)
   ln -s datafusion/benchmarks/data data
   ls data/hits_partitioned/   # should list partitioned parquet files
   ```

3. Smoke-test with a dry run — prints the exact commands each stage would run,
   executes nothing:

   ```shell
   python3 nightly/nightly.py --dry-run
   ```

4. Real run (first build takes a while — it compiles `datafusion-cli` in
   release mode for each recent commit):

   ```shell
   python3 nightly/nightly.py
   ```

   Useful flags: `--config nightly/config.json`, `--days N`, `--skip-build`,
   `--skip-bench`, `--skip-detect`, `--skip-report`, `--no-publish`.
   Exit code: 0 if all non-skipped steps succeeded, 1 otherwise.

You can also exercise the detector alone against historical data — the
`results_metal` dataset contains the known `collect_statistics` perf change at
revision `2d7ae0926`, so it should produce alerts:

```shell
python3 nightly/detect.py --results-dir results_metal --output-dir nightly/out
cat nightly/out/alerts.md
```

## Scheduling

`nightly/nightly.sh` is the cron entry point: it takes an `flock` guard (so
overlapping runs can't stack up), sets PATH, does a `git pull --rebase` of this
repo (skipped with a logged warning if the working tree is dirty — the normal
state when publishing is disabled and results accumulate locally), runs
`nightly.py`, and logs failures. Environment variables it honors:

| Variable | Effect |
|---|---|
| `NIGHTLY_NO_PULL` | If set to any non-empty value other than `0`, skip the `git pull --rebase` of this repo before running (useful while testing local changes). |
| `NIGHTLY_NOTIFY_CMD` | Optional notification hook, fired when `nightly.sh` or `nightly.py` fails **and** when a successful run ends with regression alerts. Run via `bash -c` with a short message appended as `$1`, so quoting inside the variable works; see the header comment in `nightly/nightly.sh` for working mail/ntfy examples. |

### cron

`nightly/crontab.example` contains a one-liner. Review it, adjust the schedule
and repo path, then install:

```shell
cat nightly/crontab.example
( crontab -l 2>/dev/null; cat nightly/crontab.example ) | crontab -
crontab -l    # verify
```

### systemd

Edit `User=` / `WorkingDirectory=` and paths in the units first, then:

```shell
sudo cp nightly/systemd/datafusion-nightly.service /etc/systemd/system/
sudo cp nightly/systemd/datafusion-nightly.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now datafusion-nightly.timer
systemctl list-timers datafusion-nightly.timer      # confirm next run
journalctl -u datafusion-nightly.service -n 100     # inspect a run
```

### GitHub Actions (optional)

`nightly/github-workflow.example.yml` is an *optional* self-hosted-runner
workflow (schedule-only trigger; security caveats documented in the file). It
is not installed by default — cron/systemd is the recommended path.

### What a run leaves behind

- `nightly/logs/nightly-YYYY-MM-DD.log` — full streamed output of every step,
  with per-step timing.
- `nightly/out/status.json` — machine-readable summary:
  `{ "started_at", "finished_at", "ok": bool, "steps": {name: {"state": "ok|failed|skipped", "seconds": f}}, "alerts": n, "new_revisions": n }`
- `nightly/.lock` — created with `O_CREAT|O_EXCL`, contains pid + start time.
  Stale locks (> 24 h) are broken automatically with a warning; if a run
  crashed and left one behind less than 24 h old, delete it by hand.

Step dependencies: `bench` needs `build`; `detect` and `report` need `bench`
*softly* (they still run if bench was merely skipped, but are skipped if it
hard-failed); `publish` needs `report`. A `detect` exit code of 2 (alerts
present, see below) is treated as success-with-alerts, and the alert count is
recorded in `status.json`.

## Machine tuning

Benchmark numbers are only as stable as the machine underneath. This follows
standard practice from the
[LLVM benchmarking guide](https://llvm.org/docs/Benchmarking.html),
[rustc-perf](https://github.com/rust-lang/rustc-perf), and
[pyperf's `system tune`](https://pyperf.readthedocs.io/en/latest/system.html).

`nightly/tune.sh` (run once per boot, needs root) changes, one knob per line:

- **CPU governor → `performance`** — stops frequency scaling from adding
  run-to-run variance.
- **Turbo boost off** — turbo frequency depends on thermal headroom, so it
  varies between runs.
- **SMT (hyper-threading) off** — sibling-thread contention perturbs timings.
- **ASLR off** — randomized address-space layout changes code/data placement
  enough to cause measurable swings.
- **irqbalance stopped** — keeps interrupt handling from migrating onto
  benchmark cores mid-run.

`nightly/check_env.sh` asserts those settings *before* a run instead of
changing them. It is gated by the `"check_env": true` config key (default
`false`): when enabled, `nightly.py` runs it first and aborts the run if the
box is not tuned. Both scripts no-op with a clear message on non-Linux
machines.

## Configuration

`nightly/config.json` (override with `nightly.py --config <path>`). Every key
is optional; the values below are the defaults.

| Key | Default | Meaning |
|---|---|---|
| `datafusion_dir` | `"datafusion"` | Path to the DataFusion checkout used for builds (passed to `ensure_datafusion_cli.py --datafusion-dir`). |
| `days` | `2` | Look-back window: build/benchmark `origin/main` commits from the last N days (`ensure_datafusion_cli.py --days`; override per-run with `nightly.py --days`). |
| `num_builds` | `2` | Passed to `ensure_datafusion_cli.py --num-builds` — how many commits it builds per run. |
| `results_dir` | `"results"` | Where benchmark CSVs accumulate; input to the detect and report steps. |
| `benchmarks` | `["clickbench"]` | Suites passed to `benchmark.py --benchmarks` (only `clickbench` is recognized today). |
| `detect.window` | `60` | Trailing number of per-revision points fed to the detectors. |
| `detect.min_delta_pct` | `2.0` | Floor (in %) a step must exceed before a threshold alert can fire, regardless of learned noise. |
| `detect.iqr_multiplier` | `3.0` | IQR fence multiplier for the per-query threshold detector. |
| `detect.pvalue` | `0.05` | Significance level for the e-divisive permutation test. |
| `detect.permutations` | `199` | Permutations per significance test (seeded RNG, deterministic). |
| `check_env` | `false` | If `true`, run `nightly/check_env.sh` first and abort if the machine is not tuned. |
| `prune_builds_keep` | `30` | After a successful bench step, delete all but the newest K binaries in `builds/` (each is ~100 MB). `0` disables pruning. |
| `publish.enabled` | `false` | Master switch for the publish step. |
| `publish.remote` | `"origin"` | Git remote pushed to. |
| `publish.branch` | `"main"` | Branch pushed to. |
| `publish.paths` | `["results", "docs", "nightly/out"]` | Paths `git add`ed before the nightly commit. |

## Regression detection methodology

`nightly/detect.py` reads every `*.csv` in `--results-dir` (the existing
schema: `benchmark_name,query_name,query_type,execution_time,run_timestamp,git_revision,git_revision_timestamp,num_cores`).
It tolerates a missing header row (the known `results_2026_03` bug), keeps
only `query_type == 'query'` rows, tolerates queries missing from some
revisions, and de-duplicates identical rows across files.

**Series construction.** For each `(benchmark_name, query_name)` pair, runs
are grouped by `git_revision` and summarized as the **median**
`execution_time`, ordered by `git_revision_timestamp` (ties broken by
`run_timestamp`, then revision string). Only the trailing `--window` (default
60) points feed detection. The median-per-revision step is what makes single
noisy runs mostly harmless.

Two independent detectors then run over each series:

1. **Threshold — per-query learned noise (the
   [rustc-perf](https://github.com/rust-lang/rustc-perf) approach).**
   Consecutive medians give relative deltas `d_i = (m_i - m_{i-1}) / m_{i-1}`.
   The query's *own* historical deltas (excluding the newest) yield Q1/Q3, and
   the newest delta is significant iff
   `d_n > max(Q3 + iqr_multiplier * IQR, min_delta_pct/100)` — i.e. the alert
   threshold is learned from how noisy that particular query has historically
   been, with `min_delta_pct` as an absolute floor. Improvements use the
   symmetric lower fence and are reported separately (never as a failure).
   Series with fewer than `--min-points` (default 8) points are skipped.
2. **Changepoint — E-divisive means (the
   [MongoDB](https://arxiv.org/abs/2003.00584) approach).** A pure-python
   e-divisive energy statistic with a permutation significance test
   (`--permutations`, default 199; seeded RNG, so results are deterministic).
   The window is split recursively while the p-value ≤ `--pvalue`; every
   changepoint is reported with its before/after medians. This catches shifts
   the step detector misses (e.g. a level change spread over 2–3 commits) and
   is far more robust to one-off noise spikes.

Full CLI:

```
python nightly/detect.py --results-dir results \
  [--output-dir nightly/out] [--window 60] [--min-delta-pct 2.0] \
  [--iqr-multiplier 3.0] [--no-edivisive] [--pvalue 0.05] \
  [--permutations 199] [--seed 42] [--min-points 8]
```

Outputs (all to `--output-dir`, default `nightly/out`):

- `alerts.json` — machine-readable:

  ```json
  { "generated_at": "2026-07-14T12:00:00Z", "results_dir": "results", "window": 60,
    "alerts": [ { "benchmark_name": "clickbench_partitioned", "query_name": "q13",
        "kind": "threshold|changepoint", "direction": "regression|improvement",
        "revision": "2d7ae0926", "revision_timestamp": "...",
        "median_before_s": 0.51, "median_after_s": 0.72, "delta_pct": 41.2,
        "threshold_pct": 6.3, "p_value": 0.005 } ] }
  ```

  (`threshold_pct` is set for `kind=threshold`, `p_value` for
  `kind=changepoint`; the other is null.)
- `alerts.md` — human-readable summary table, suitable for pasting into a
  GitHub issue.
- `detected_events.json` — `[{"revision": "...", "label": "q13 +41% (changepoint)"}]`,
  the **same shape as the hand-curated `events.json`**. `report.py` merges it
  (de-duplicated by revision; hand-curated `events.json` wins) and draws the
  detected changepoints as annotation lines on the dashboard in a distinct
  style, so detected regressions are visible right on the charts.

**Exit-code contract:** `0` = ran fine and no regression alerts, `2` =
regression alerts present (improvements alone do not cause exit 2), `1` =
error. This lets cron wrappers distinguish "broken" from "found something";
`nightly.py` treats 2 as success-with-alerts and surfaces the count in
`status.json`.

## Interpreting alerts

- **Threshold vs changepoint.** A threshold alert says "the latest
  commit-to-commit step is large relative to this query's own noise history".
  A changepoint alert says "the distribution of this series shifted at this
  revision, and a permutation test says that shift is unlikely to be noise".
  Both detectors firing on the same query/revision is a strong signal; a lone
  threshold alert on a query with a short or spiky history is the weakest.
- **Improvements vs regressions.** Improvements are detected with the same
  machinery, reported with `direction: improvement`, and never fail the run or
  cause exit code 2. They are worth skimming — an "improvement" you didn't
  expect is sometimes a query silently doing less work.
- **Know the noise floor.** Even on a tuned, dedicated machine, ~1–3 %
  wall-time variance between identical runs is normal. Treat single-query
  alerts with deltas near `min_delta_pct` skeptically. The cheap
  confirmation is to rerun the *same* sha on a quiet night and compare — the
  built binary is still in `builds/`, and pointing at a scratch output
  directory bypasses the "already benchmarked" skip:

  ```shell
  ./run_clickbench.py --output-dir /tmp/recheck \
      --datafusion-binary builds/datafusion-cli@<revision>@<revision_timestamp>
  ```

  If the recheck reproduces the delta, bisect the commit range on the
  DataFusion side; if not, it was noise — the changepoint detector will stop
  reporting it as more points accumulate.

## Publishing

The publish step is gated by `publish.enabled` (default **false**) — nothing
is committed or pushed until you opt in. When enabled, after a successful
report step, `nightly.py` runs `git add` on `publish.paths` (default
`results`, `docs`, `nightly/out`), commits with the message
`nightly: <UTC date> (<n_new_revisions> new revision(s), <n_alerts> alert(s))`,
then `git pull --rebase` and `git push <remote> <branch>`. On a rebase
conflict it aborts cleanly and prints instructions rather than force-pushing.

Repo-growth note: the results CSVs are append-only and small (a few KB per
night), but `docs/index.html` is rewritten every night, so the git history of
the pages output grows by a full-file snapshot per run. If repository size
ever becomes a nuisance, occasionally squash the history of the publishing
branch — the CSVs, not the generated HTML, are the data of record.

## Limitations & future work

- **Only ClickBench is wired up today.** `benchmark.py` recognizes only the
  `clickbench` suite; the `benchmarks` config key exists so more suites can be
  added without touching the orchestrator.
- **TPC-H and friends** could be added later by wrapping DataFusion's upstream
  `benchmarks/bench.sh` the same way ClickBench data provisioning already
  uses it.
- **Per-operator microbenchmarks** —
  [apache/datafusion#15214](https://github.com/apache/datafusion/issues/15214)
  — would catch regressions that whole-query wall time averages away.
- **Codspeed compatibility** is a stated goal of
  [EPIC apache/datafusion#21165](https://github.com/apache/datafusion/issues/21165);
  the append-only CSV history here is simple enough to export if/when that
  lands.
- **PR-triggered benchmarks are a non-goal** — that niche is served by the GKE
  infra in [apache/datafusion#18115](https://github.com/apache/datafusion/issues/18115).
  This pipeline is for the long-history, catch-the-boiling-frog use case.
