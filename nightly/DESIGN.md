# Nightly Automation & Regression Detection — Design

Addresses the remaining gap of [apache/datafusion#5504](https://github.com/apache/datafusion/issues/5504)
and the "Trackable over time" goal of [apache/datafusion#21165](https://github.com/apache/datafusion/issues/21165):
run benchmarks regularly on `main`, keep history, **detect regressions statistically**, and publish the dashboard —
all with plain cron + bash + python (no CI-vendor lock-in), per maintainer guidance.

Non-goals: PR-triggered benchmarks (solved by adriangb's GKE infra, #18115), replacing the existing
`benchmark.py` / `report.py` pipeline (we wrap and extend it, we don't rewrite it).

## Pipeline

```
cron / systemd timer
  └── nightly/nightly.sh            flock guard, env setup, failure logging
        └── nightly/nightly.py      orchestrator (python3, stdlib only)
              1. check_env          (optional) nightly/check_env.sh must pass on Linux
              2. build              python ensure_datafusion_cli.py --days N  (existing script)
              3. bench              python benchmark.py --output-dir results  (existing script)
              4. detect             python nightly/detect.py --results-dir results
                                      → nightly/out/alerts.json, alerts.md, detected_events.json
              5. report             python report.py --results-dir results    (existing script)
              6. publish            git add/commit/pull --rebase/push         (config-gated, default OFF)
              7. status             nightly/out/status.json + log file; exit non-zero on any failure
```

Every stage is skippable (`--skip-build`, `--skip-bench`, ...) and `--dry-run` prints the exact
commands without executing anything (this also makes the orchestrator testable on Windows).

## New files (all under `nightly/` except noted)

| File | Purpose |
|---|---|
| `nightly.py` | Orchestrator. Stdlib only. |
| `config.json` | Default config (see schema below). |
| `detect.py` | Regression detection. Stdlib only. |
| `nightly.sh` | Cron entry: flock, PATH, git pull --rebase, run nightly.py, log. |
| `tune.sh` / `check_env.sh` | Linux benchmark-box tuning + pre-run assertions (governor, turbo, SMT, ASLR, irqbalance). Both no-op with a clear message on non-Linux. |
| `systemd/datafusion-nightly.service` + `.timer` | systemd alternative to cron. |
| `crontab.example` | one-liner cron install example. |
| `github-workflow.example.yml` | OPTIONAL self-hosted-runner workflow (schedule-only trigger, documented security caveats). Not installed by default. |
| `tests/` | `unittest` suite + small fixtures carved from real `results_metal` data. |
| `README.md` | Setup, operation, detection methodology, alert interpretation. |

Modified files: `report.py` (3 surgical fixes, below), top-level `README.md` (Automation section).

## detect.py — the analytical core

CLI:
```
python nightly/detect.py --results-dir results \
  [--output-dir nightly/out] [--window 60] [--min-delta-pct 2.0] \
  [--iqr-multiplier 3.0] [--no-edivisive] [--pvalue 0.05] \
  [--permutations 199] [--seed 42] [--min-points 8]
```

Input: every `*.csv` in `--results-dir` with the existing schema
`benchmark_name,query_name,query_type,execution_time,run_timestamp,git_revision,git_revision_timestamp,num_cores`.
Robustness requirements:
- tolerate a missing header row (results_2026_03 bug): if line 1 doesn't start with `benchmark_name`, apply the known schema;
- filter `query_type == 'query'`;
- ragged data: queries missing from some revisions are fine (series just lacks that point);
- dedup identical (git_revision, query_name, execution_time, run_timestamp) rows across files.

Series construction: for each `(benchmark_name, query_name)`, group by `git_revision`,
summary statistic = **median** of execution_time, ordered by `git_revision_timestamp`
(ISO-8601 with tz, `datetime.fromisoformat`; ties broken by run_timestamp then revision string).
Only the trailing `--window` points feed detection.

Two independent detectors:

1. **Threshold (rustc-perf style, per-query learned noise).** For consecutive points compute relative
   delta `d_i = (m_i - m_{i-1}) / m_{i-1}`. Historical deltas for that query (excluding the newest one)
   give Q1/Q3; the newest delta is significant iff
   `d_n > max(Q3 + iqr_multiplier * IQR, min_delta_pct/100)` (regressions; upper fence only)
   or symmetric on the improvement side (reported separately, never alerted as failure).
   Needs ≥ `--min-points` points, else skip.
2. **Changepoint (E-divisive means, MongoDB style).** Pure-python implementation of the e-divisive
   energy statistic with a permutation significance test (`--permutations`, seeded RNG, deterministic).
   Recursively split the window while p-value ≤ `--pvalue`; report every changepoint with its
   before/after medians. O(n²) per candidate is fine for n ≤ ~500.

Outputs (to `--output-dir`):
- `alerts.json`:
  ```json
  { "generated_at": "2026-07-14T12:00:00Z", "results_dir": "results", "window": 60,
    "alerts": [ { "benchmark_name": "clickbench_partitioned", "query_name": "q13",
        "kind": "threshold|changepoint", "direction": "regression|improvement",
        "revision": "2d7ae0926", "revision_timestamp": "...",
        "median_before_s": 0.51, "median_after_s": 0.72, "delta_pct": 41.2,
        "threshold_pct": 6.3, "p_value": 0.005 } ] }
  ```
  (`threshold_pct` for kind=threshold, `p_value` for kind=changepoint; the other is null.)
- `alerts.md`: human-readable summary table (posted-in-issue-friendly).
- `detected_events.json`: `[{"revision": "...", "label": "q13 +41% (changepoint)"}]` —
  **same shape as the existing events.json** so report.py can merge and draw annotation lines.
  One entry per changepoint revision (queries aggregated into the label, capped length).
- Exit code: 0 = ran fine (even with alerts), 2 = regression alerts present (lets cron wrappers
  distinguish), 1 = error. `--fail-on-alert` opts into exit 2; default exits 0 either way? —
  NO: default IS exit 2 on regression alerts; `nightly.py` treats 2 as "success with alerts"
  and surfaces the count in status.json.

## nightly.py — orchestrator

CLI: `python nightly/nightly.py [--config nightly/config.json] [--days N] [--dry-run]
[--skip-build] [--skip-bench] [--skip-detect] [--skip-report] [--no-publish]`

- Runs from the repo root (chdir to parent of the `nightly/` dir it lives in) because
  `run_clickbench.py` resolves `data/` CWD-relative.
- Lock: create `nightly/.lock` with `O_CREAT|O_EXCL` containing pid + start time; stale (>24h) locks
  are broken with a warning. Portable (works on Windows for testing).
- Each step: `subprocess.run` with the interpreter `sys.executable` (NOT hardcoded `python3`),
  streamed to console AND appended to `nightly/logs/nightly-YYYY-MM-DD.log`; step timing recorded.
- `detect` exit 2 → step ok, `alerts` count recorded. Any other non-zero → step failed;
  remaining steps still attempt IF independent (report/publish run even if detect failed;
  bench skipped if build failed) — dependency map: bench→build, detect→bench(soft), report→bench(soft),
  publish→report. "soft" = run anyway if the prerequisite was merely skipped, skip if it hard-failed.
- publish (config-gated `publish.enabled`, default **false**): `git add <paths from config>`,
  commit `nightly: <UTC date> (<n_new_revisions> new revision(s), <n_alerts> alert(s))`,
  `git pull --rebase`, `git push <remote> <branch>`. Aborts cleanly (with instructions) on conflict.
- status: `nightly/out/status.json`
  `{ "started_at", "finished_at", "ok": bool, "steps": {name: {"state": "ok|failed|skipped", "seconds": f}}, "alerts": n, "new_revisions": n }`
- Exit: 0 if all non-skipped steps ok, 1 otherwise.

`config.json` schema (all keys optional, these are defaults):
```json
{ "datafusion_dir": "datafusion", "days": 2, "num_builds": 2,
  "results_dir": "results", "benchmarks": ["clickbench"],
  "detect": { "window": 60, "min_delta_pct": 2.0, "iqr_multiplier": 3.0,
              "pvalue": 0.05, "permutations": 199 },
  "check_env": false, "prune_builds_keep": 30,
  "publish": { "enabled": false, "remote": "origin", "branch": "main",
               "paths": ["results", "docs", "nightly/out"] } }
```
`prune_builds_keep`: after a successful bench step, delete all but the newest K binaries in `builds/`
(they are ~100 MB each). 0 disables pruning.

## report.py — surgical fixes only

1. **mostRecentRelease bug**: `releases_info[-1]` picks the OLDEST entry because releases.json is
   newest-first. Fix: select the entry whose revision has the max `git_revision_timestamp` among
   releases present in the loaded data; fall back to `releases_info[0]`.
2. **Merge detected events**: after loading `events.json`, also load `nightly/out/detected_events.json`
   if present, de-duplicated by revision (hand-curated events.json wins), rendered in a distinct
   color/style (e.g. orange dash-dot) with its own "Show detected changes" toggle if cheap, otherwise
   reuse the events toggle.
3. **Pin Plotly**: `plotly-latest.min.js` → a pinned 2.x version URL.

Keep the diff minimal — no reformatting, match existing style.

## Testing strategy (must run on Windows dev box, Python 3.12)

- `nightly/tests/test_detect.py`:
  - synthetic series: flat+noise (no alerts), single injected step of +30% (threshold AND e-divisive
    both fire, correct revision), gradual drift (no threshold alert), improvement step (direction).
  - e-divisive determinism with fixed seed; permutation p-value sanity.
  - CSV robustness: headerless file, ragged queries, duplicate rows.
  - **real-data test**: fixture carved from `results_metal/results.csv` for 2–3 queries around
    revision `2d7ae0926` ("default collect_statistics to true", a known real perf event) —
    detector must flag a changepoint at/adjacent to that revision on at least one affected query.
    (Fixture generated once by a helper script, committed as small CSV ≤ 200 KB.)
- `nightly/tests/test_nightly.py`: dry-run produces the right command list; lock behavior;
  status.json shape; config merge; dependency map (build fails → bench skipped).
- Bash: `bash -n` syntax check for all `.sh` (run in Git Bash on the dev box).

## Style & licensing

Match the existing repo: plain argparse scripts, no third-party deps for new code
(existing report.py deps unchanged). Apache-2.0; add the standard ASF license header
to new files ONLY if existing repo files carry one (check first; alamb's files may not).
