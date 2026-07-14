#!/usr/bin/env python3
"""Tests for nightly/detect.py.

Run from the repo root with:
    python -m unittest discover nightly/tests
"""
import json
import os
import random
import shutil
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone

_NIGHTLY_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir))
if _NIGHTLY_DIR not in sys.path:
    sys.path.insert(0, _NIGHTLY_DIR)

import detect  # noqa: E402

FIXTURES_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'fixtures')

CSV_HEADER = ('benchmark_name,query_name,query_type,execution_time,'
              'run_timestamp,git_revision,git_revision_timestamp,num_cores')


def synth_csv_lines(medians_by_query, runs=3, jitter_seed=None, jitter=0.0,
                    start=None):
    """Build CSV data lines: one revision per index of each query's median
    list, `runs` runs per revision. Revisions are shared across queries."""
    if start is None:
        start = datetime(2025, 1, 1, tzinfo=timezone.utc)
    rng = random.Random(jitter_seed)
    lines = []
    n = max(len(m) for m in medians_by_query.values())
    for i in range(n):
        rev = 'r%03d' % i
        rev_ts = (start + timedelta(days=i)).isoformat()
        for query, medians in medians_by_query.items():
            if i >= len(medians):
                continue
            # one run_timestamp per revision, matching run_clickbench.py
            # which writes a single run_timestamp per session
            run_ts = (start + timedelta(days=i, hours=8)
                      ).strftime('%Y-%m-%d %H:%M:%S')
            for run in range(runs):
                value = medians[i]
                if jitter:
                    value *= 1.0 + rng.uniform(-jitter, jitter)
                lines.append('clickbench_partitioned,%s,query,%.6f,%s,%s,%s,8'
                             % (query, value, run_ts, rev, rev_ts))
    return lines


def write_csv(path, lines, header=True):
    with open(path, 'w', encoding='utf-8') as fh:
        if header:
            fh.write(CSV_HEADER + '\n')
        fh.write('\n'.join(lines) + '\n')


def flat_series(n, base=1.0, noise=0.005, seed=7):
    rng = random.Random(seed)
    return [base * (1.0 + rng.uniform(-noise, noise)) for _ in range(n)]


class TestParseTimestamp(unittest.TestCase):
    def test_z_suffix(self):
        dt = detect.parse_timestamp('2025-06-19T18:09:42Z')
        self.assertIsNotNone(dt)
        self.assertEqual(dt.utcoffset(), timedelta(0))

    def test_offset(self):
        dt = detect.parse_timestamp('2025-06-19T19:09:42+01:00')
        z = detect.parse_timestamp('2025-06-19T18:09:42Z')
        self.assertEqual(dt, z)

    def test_naive_assumed_utc(self):
        dt = detect.parse_timestamp('2026-03-30 13:52:55')
        self.assertIsNotNone(dt.tzinfo)

    def test_garbage(self):
        self.assertIsNone(detect.parse_timestamp('not-a-date'))
        self.assertIsNone(detect.parse_timestamp(''))


class CsvTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix='detect-test-')

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


class TestLoadResults(CsvTestCase):
    def test_headerless_file(self):
        lines = synth_csv_lines({'q1': [1.0, 1.1, 1.05]})
        write_csv(os.path.join(self.tmp, 'results.csv'), lines, header=False)
        rows = detect.load_results(self.tmp)
        self.assertEqual(len(rows), 9)
        self.assertEqual(rows[0]['benchmark_name'], 'clickbench_partitioned')

    def test_query_type_filter(self):
        lines = synth_csv_lines({'q1': [1.0, 1.1]})
        lines.append('clickbench_partitioned,q1,table_creation,9.9,'
                     '2025-01-05 08:00:00,r009,2025-01-05T00:00:00+00:00,8')
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        rows = detect.load_results(self.tmp)
        self.assertEqual(len(rows), 6)
        self.assertTrue(all(r['execution_time'] < 9 for r in rows))

    def test_dedup_across_files(self):
        lines = synth_csv_lines({'q1': [1.0, 1.1, 1.2]})
        write_csv(os.path.join(self.tmp, 'a.csv'), lines)
        write_csv(os.path.join(self.tmp, 'b.csv'), lines, header=False)
        rows = detect.load_results(self.tmp)
        # b.csv is an exact copy of a.csv: every row collapses with its twin
        self.assertEqual(len(rows), 9)

    def test_within_file_repeats_survive(self):
        # run_clickbench.py writes one run_timestamp per session and
        # ms-quantized times, so fast queries emit identical rows that are
        # genuine repeated measurements, not duplicates to collapse
        ts = '2025-01-01 08:00:00'
        rev_ts = '2025-01-01T00:00:00+00:00'
        lines = ['clickbench_partitioned,q0,query,%s,%s,r000,%s,8'
                 % (v, ts, rev_ts)
                 for v in ('0.035', '0.035', '0.035', '0.036')]
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        rows = detect.load_results(self.tmp)
        self.assertEqual(len(rows), 4)
        series = detect.build_series(rows)
        points = series[('clickbench_partitioned', 'q0')]
        self.assertEqual(points[0]['median'], 0.035)

    def test_dedup_multiplicity_across_files(self):
        ts = '2025-01-01 08:00:00'
        rev_ts = '2025-01-01T00:00:00+00:00'
        line = ('clickbench_partitioned,q0,query,0.035,%s,r000,%s,8'
                % (ts, rev_ts))
        write_csv(os.path.join(self.tmp, 'a.csv'), [line] * 3)
        write_csv(os.path.join(self.tmp, 'b.csv'), [line] * 2)
        rows = detect.load_results(self.tmp)
        # the Nth duplicate in b.csv collapses with the Nth in a.csv
        self.assertEqual(len(rows), 3)

    def test_malformed_rows_skipped(self):
        lines = synth_csv_lines({'q1': [1.0, 1.1]})
        lines.insert(1, 'short,row')
        lines.insert(2, 'clickbench_partitioned,q1,query,not_a_number,'
                        '2025-01-01 08:00:00,rbad,2025-01-01T00:00:00+00:00,8')
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        rows = detect.load_results(self.tmp)
        self.assertEqual(len(rows), 6)

    def test_no_csv_files(self):
        with self.assertRaises(FileNotFoundError):
            detect.load_results(self.tmp)

    def test_nul_bytes_and_bad_utf8_tolerated(self):
        # csv.reader raises "line contains NUL" on raw NUL bytes, and strict
        # utf-8 decoding raises on invalid bytes; a single corrupted results
        # file must not abort the whole nightly run
        lines = synth_csv_lines({'q1': [1.0, 1.1]})
        path = os.path.join(self.tmp, 'results.csv')
        write_csv(path, lines)
        with open(path, 'ab') as fh:
            fh.write(b'clickbench_partitioned,q\x00N,query,1.0,'
                     b'2025-01-05 08:00:00,r009,2025-01-05T00:00:00+00:00,8\n')
            fh.write(b'\xff\xfe not,a,valid,utf8,row\n')
        rows = detect.load_results(self.tmp)
        # NUL is stripped, so the q\x00N row survives as qN; the undecodable
        # row is short and gets skipped as malformed
        self.assertEqual(len(rows), 7)
        self.assertEqual(len([r for r in rows if r['query_name'] == 'qN']), 1)

    def test_oversized_field_skips_file_not_run(self):
        # an unterminated quote makes csv accumulate one giant field until it
        # exceeds field_size_limit and raises csv.Error; that must skip the
        # rest of the corrupted file, not abort the whole run
        lines = synth_csv_lines({'q1': [1.0, 1.1]})
        write_csv(os.path.join(self.tmp, 'a.csv'), lines)
        write_csv(os.path.join(self.tmp, 'b.csv'), ['"' + 'x' * 200000])
        rows = detect.load_results(self.tmp)
        self.assertEqual(len(rows), 6)


class TestBuildSeries(CsvTestCase):
    def test_ragged_queries(self):
        # q2 is missing from the middle revision — series just lacks a point
        lines = synth_csv_lines({'q1': [1.0, 1.1, 1.2]})
        lines = [ln for ln in lines] + [
            'clickbench_partitioned,q2,query,2.0,2025-01-01 08:00:00,r000,2025-01-01T00:00:00+00:00,8',
            'clickbench_partitioned,q2,query,2.1,2025-01-03 08:00:00,r002,2025-01-03T00:00:00+00:00,8',
        ]
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        series = detect.build_series(detect.load_results(self.tmp))
        self.assertEqual(len(series[('clickbench_partitioned', 'q1')]), 3)
        self.assertEqual(len(series[('clickbench_partitioned', 'q2')]), 2)

    def test_ordering_and_median(self):
        lines = [
            # out of file order; mixed tz offsets; 2 runs for the revision
            'b,q1,query,3.0,2025-01-02 08:00:00,rB,2025-01-02T09:00:00+09:00,8',
            'b,q1,query,1.0,2025-01-01 08:00:00,rA,2025-01-01T00:00:00Z,8',
            'b,q1,query,5.0,2025-01-02 08:05:00,rB,2025-01-02T09:00:00+09:00,8',
            'b,q1,query,2.0,2025-01-03 08:00:00,rC,2025-01-02T01:00:00+00:00,8',
        ]
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        series = detect.build_series(detect.load_results(self.tmp))
        points = series[('b', 'q1')]
        # rB's timestamp is 2025-01-02T00:00:00 UTC, before rC's 01:00 UTC
        self.assertEqual([p['revision'] for p in points], ['rA', 'rB', 'rC'])
        self.assertEqual(points[1]['median'], 4.0)

    def test_tie_break_run_timestamp_then_revision(self):
        ts = '2025-01-01T00:00:00+00:00'
        lines = [
            'b,q1,query,1.0,2025-01-02 08:00:00,zzz,%s,8' % ts,
            'b,q1,query,1.0,2025-01-01 08:00:00,yyy,%s,8' % ts,
            'b,q1,query,1.0,2025-01-01 08:00:00,aaa,%s,8' % ts,
        ]
        write_csv(os.path.join(self.tmp, 'results.csv'), lines)
        series = detect.build_series(detect.load_results(self.tmp))
        points = series[('b', 'q1')]
        self.assertEqual([p['revision'] for p in points], ['aaa', 'yyy', 'zzz'])


def _points_from_values(values):
    start = datetime(2025, 1, 1, tzinfo=timezone.utc)
    points = []
    for i, v in enumerate(values):
        dt = start + timedelta(days=i)
        points.append({
            'revision': 'r%03d' % i,
            'revision_timestamp': dt.isoformat(),
            'revision_dt': dt,
            'run_dt': dt,
            'median': v,
        })
    return points


class TestThresholdDetector(unittest.TestCase):
    def test_flat_noise_no_alert(self):
        points = _points_from_values(flat_series(30))
        self.assertIsNone(detect.threshold_detect(points, 8, 3.0, 2.0))

    def test_step_regression_at_newest(self):
        values = flat_series(30)
        values[-1] = values[-2] * 1.30
        points = _points_from_values(values)
        alert = detect.threshold_detect(points, 8, 3.0, 2.0)
        self.assertIsNotNone(alert)
        self.assertEqual(alert['direction'], 'regression')
        self.assertEqual(alert['revision'], 'r029')
        self.assertAlmostEqual(alert['delta_pct'], 30.0, places=5)
        self.assertAlmostEqual(alert['median_after_s'], values[-1])
        self.assertAlmostEqual(alert['median_before_s'], values[-2])
        self.assertIsNone(alert['p_value'])
        self.assertGreater(alert['delta_pct'], alert['threshold_pct'])

    def test_step_improvement_direction(self):
        values = flat_series(30)
        values[-1] = values[-2] * 0.70
        points = _points_from_values(values)
        alert = detect.threshold_detect(points, 8, 3.0, 2.0)
        self.assertIsNotNone(alert)
        self.assertEqual(alert['direction'], 'improvement')
        self.assertLess(alert['delta_pct'], 0)
        self.assertLess(alert['threshold_pct'], 0)

    def test_gradual_drift_no_alert(self):
        # +1% every night: every historical delta looks like the newest one
        values = [1.0 * (1.01 ** i) for i in range(30)]
        points = _points_from_values(values)
        self.assertIsNone(detect.threshold_detect(points, 8, 3.0, 2.0))

    def test_too_few_points(self):
        values = flat_series(7)
        values[-1] = values[-2] * 1.5
        points = _points_from_values(values)
        self.assertIsNone(detect.threshold_detect(points, 8, 3.0, 2.0))

    def test_min_delta_floor(self):
        # tiny but statistically outlying delta stays below the 2% floor
        values = [1.0] * 30
        values[-1] = 1.005
        points = _points_from_values(values)
        self.assertIsNone(detect.threshold_detect(points, 8, 3.0, 2.0))


class TestEDivisive(unittest.TestCase):
    def test_step_in_middle(self):
        values = flat_series(40, base=1.0, noise=0.01)
        values = values[:20] + [v * 1.30 for v in values[20:]]
        cps = detect.e_divisive(values, 199, 0.05, random.Random(42))
        self.assertEqual(len(cps), 1)
        index, p = cps[0]
        self.assertEqual(index, 20)
        self.assertLessEqual(p, 0.05)
        self.assertGreaterEqual(p, 1.0 / 200.0)

    def test_flat_noise_no_changepoint(self):
        values = flat_series(40, noise=0.01)
        cps = detect.e_divisive(values, 199, 0.05, random.Random(42))
        self.assertEqual(cps, [])

    def test_constant_series(self):
        cps = detect.e_divisive([1.0] * 40, 199, 0.05, random.Random(42))
        self.assertEqual(cps, [])

    def test_determinism(self):
        values = flat_series(50, noise=0.02, seed=3)
        values = values[:25] + [v * 1.2 for v in values[25:]]
        first = detect.e_divisive(values, 199, 0.05, random.Random(42))
        second = detect.e_divisive(values, 199, 0.05, random.Random(42))
        self.assertEqual(first, second)
        self.assertTrue(first)

    def test_two_changepoints(self):
        values = ([1.0] * 20) + ([1.5] * 20) + ([0.8] * 20)
        rng = random.Random(42)
        cps = detect.e_divisive([v * (1 + 0.005 * random.Random(i).random())
                                 for i, v in enumerate(values)],
                                199, 0.05, rng)
        indexes = [i for i, _p in cps]
        self.assertIn(20, indexes)
        self.assertIn(40, indexes)

    def test_p_value_lower_bound(self):
        # with 199 permutations the smallest achievable p is 1/200
        values = ([1.0] * 20) + ([2.0] * 20)
        values = [v * (1 + 0.001 * i) for i, v in enumerate(values)]
        cps = detect.e_divisive(values, 199, 0.05, random.Random(42))
        self.assertTrue(cps)
        self.assertGreaterEqual(min(p for _i, p in cps), 1.0 / 200.0)

    def test_qhat_matches_naive(self):
        # cross-check the O(n log n) implementation against a brute force one
        rng = random.Random(9)
        values = [rng.uniform(0.5, 2.0) for _ in range(24)]
        items, n_unique, total_pair = detect._prep_segment(values)
        best_q, best_t = detect._max_qhat(items, n_unique, total_pair)
        naive_best = float('-inf')
        naive_t = -1
        n = len(values)
        for t in range(1, n):
            left = values[:t]
            right = values[t:]
            cross = sum(abs(x - y) for x in left for y in right)
            lp = sum(abs(left[i] - left[j])
                     for i in range(t) for j in range(i + 1, t))
            rp = sum(abs(right[i] - right[j])
                     for i in range(n - t) for j in range(i + 1, n - t))
            m, r = t, n - t
            term1 = 2.0 * cross / (m * r)
            term2 = 2.0 * lp / (m * (m - 1)) if m > 1 else 0.0
            term3 = 2.0 * rp / (r * (r - 1)) if r > 1 else 0.0
            q = (float(m) * r / (m + r)) * (term1 - term2 - term3)
            if q > naive_best:
                naive_best = q
                naive_t = t
        self.assertAlmostEqual(best_q, naive_best, places=9)
        self.assertEqual(best_t, naive_t)


class TestEndToEnd(CsvTestCase):
    def run_main(self, extra_args=None):
        out_dir = os.path.join(self.tmp, 'out')
        args = ['--results-dir', self.tmp, '--output-dir', out_dir,
                '--seed', '42']
        if extra_args:
            args.extend(extra_args)
        code = detect.main(args)
        alerts = None
        events = None
        alerts_path = os.path.join(out_dir, 'alerts.json')
        if os.path.exists(alerts_path):
            with open(alerts_path, encoding='utf-8') as fh:
                alerts = json.load(fh)
            with open(os.path.join(out_dir, 'detected_events.json'),
                      encoding='utf-8') as fh:
                events = json.load(fh)
        return code, alerts, events, out_dir

    def test_step_regression_alerts_and_exit_code(self):
        # q_bad regresses 30% at the newest revision (threshold detector);
        # q_mid has a mid-window step (changepoint detector); q_ok is flat.
        bad = flat_series(40, base=0.5, noise=0.01, seed=11)
        bad[-1] = bad[-2] * 1.3
        mid = flat_series(40, base=2.0, noise=0.01, seed=12)
        mid = mid[:25] + [v * 1.3 for v in mid[25:]]
        ok = flat_series(40, base=1.0, noise=0.01, seed=13)
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q_bad': bad, 'q_mid': mid, 'q_ok': ok}))
        code, alerts, events, out_dir = self.run_main()
        self.assertEqual(code, 2)
        kinds = set((a['query_name'], a['kind'], a['direction'])
                    for a in alerts['alerts'])
        self.assertIn(('q_bad', 'threshold', 'regression'), kinds)
        self.assertIn(('q_mid', 'changepoint', 'regression'), kinds)
        self.assertFalse(any(a['query_name'] == 'q_ok'
                             for a in alerts['alerts']))
        threshold_alert = next(a for a in alerts['alerts']
                               if a['kind'] == 'threshold'
                               and a['query_name'] == 'q_bad')
        self.assertEqual(threshold_alert['revision'], 'r039')
        changepoint_alert = next(a for a in alerts['alerts']
                                 if a['kind'] == 'changepoint'
                                 and a['query_name'] == 'q_mid')
        self.assertEqual(changepoint_alert['revision'], 'r025')
        self.assertIsNotNone(changepoint_alert['p_value'])
        self.assertIsNone(changepoint_alert['threshold_pct'])
        # alerts.json shape
        self.assertEqual(set(alerts.keys()),
                         {'generated_at', 'results_dir', 'window', 'alerts'})
        for a in alerts['alerts']:
            self.assertEqual(sorted(a.keys()), sorted(detect.ALERT_FIELDS))
        # detected_events.json: same shape as events.json, changepoints only
        self.assertTrue(any(e['revision'] == 'r025' for e in events))
        for e in events:
            self.assertEqual(set(e.keys()), {'revision', 'label'})
        self.assertTrue(os.path.exists(os.path.join(out_dir, 'alerts.md')))
        with open(os.path.join(out_dir, 'alerts.md'), encoding='utf-8') as fh:
            md = fh.read()
        self.assertIn('q_bad', md)
        self.assertIn('Regressions', md)

    def test_multiple_regimes_use_adjacent_segments(self):
        # 2.0 -> 1.0 -> 1.2: the +20% regression entering the third regime
        # must be measured against the adjacent 1.0 regime; medians over the
        # whole prefix/suffix would call it an improvement (-20%) instead
        values = [2.0] * 20 + [1.0] * 20 + [1.2] * 20
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': values}))
        code, alerts, _events, _out = self.run_main()
        self.assertEqual(code, 2)
        regression = next(a for a in alerts['alerts']
                          if a['kind'] == 'changepoint'
                          and a['revision'] == 'r040')
        self.assertEqual(regression['direction'], 'regression')
        self.assertAlmostEqual(regression['delta_pct'], 20.0, delta=1.0)
        self.assertAlmostEqual(regression['median_before_s'], 1.0, delta=0.05)
        self.assertAlmostEqual(regression['median_after_s'], 1.2, delta=0.05)
        improvement = next(a for a in alerts['alerts']
                           if a['kind'] == 'changepoint'
                           and a['revision'] == 'r020')
        self.assertEqual(improvement['direction'], 'improvement')
        self.assertAlmostEqual(improvement['delta_pct'], -50.0, delta=1.0)

    def test_clean_run_exits_zero(self):
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': flat_series(40, noise=0.01)}))
        code, alerts, events, _out = self.run_main()
        self.assertEqual(code, 0)
        self.assertEqual(alerts['alerts'], [])
        self.assertEqual(events, [])

    def test_improvements_do_not_fail(self):
        values = flat_series(40, noise=0.01)
        values = values[:25] + [v * 0.6 for v in values[25:]]
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': values}))
        code, alerts, _events, _out = self.run_main()
        self.assertEqual(code, 0)
        self.assertTrue(alerts['alerts'])
        self.assertTrue(all(a['direction'] == 'improvement'
                            for a in alerts['alerts']))

    def test_no_edivisive_flag(self):
        values = flat_series(40, noise=0.01)
        values = values[:25] + [v * 1.5 for v in values[25:]]
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': values}))
        code, alerts, events, _out = self.run_main(['--no-edivisive'])
        self.assertEqual(code, 0)
        self.assertFalse(any(a['kind'] == 'changepoint'
                             for a in alerts['alerts']))
        self.assertEqual(events, [])

    def test_few_revisions_degrade_gracefully(self):
        # like the live results/ dir: too few points for window statistics
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': [1.0, 1.1, 0.9, 1.05, 2.0, 1.0]}))
        code, alerts, _events, _out = self.run_main()
        self.assertEqual(code, 0)
        self.assertEqual(alerts['alerts'], [])

    def test_missing_results_dir_exits_one(self):
        code = detect.main(['--results-dir',
                            os.path.join(self.tmp, 'nope'),
                            '--output-dir', os.path.join(self.tmp, 'out')])
        self.assertEqual(code, 1)

    def test_window_trims_series(self):
        # a step 30 revisions ago is invisible with --window 10
        values = flat_series(60, noise=0.005)
        values = values[:30] + [v * 1.4 for v in values[30:]]
        write_csv(os.path.join(self.tmp, 'results.csv'),
                  synth_csv_lines({'q1': values}))
        code, alerts, _events, _out = self.run_main(['--window', '10'])
        self.assertEqual(code, 0)
        self.assertEqual(alerts['alerts'], [])


class TestRealData(unittest.TestCase):
    """Fixture carved from results_metal/results.csv around revision
    2d7ae0926 ('default collect_statistics to true'): q0 and q6 visibly
    changed (~-89%), q28 did not."""

    EVENT_REV = '2d7ae0926'

    @classmethod
    def setUpClass(cls):
        fixture = os.path.join(FIXTURES_DIR, 'metal_subset.csv')
        assert os.path.exists(fixture), 'missing fixture %s' % fixture
        cls.rows = detect.load_results(FIXTURES_DIR)
        cls.alerts = detect.detect(
            cls.rows, window=0, min_points=8, iqr_multiplier=3.0,
            min_delta_pct=2.0, run_edivisive=True, permutations=199,
            pvalue=0.05, seed=42)
        cls.series = detect.build_series(cls.rows)

    def _revisions_near_event(self, query):
        points = self.series[('clickbench_partitioned', query)]
        revisions = [p['revision'] for p in points]
        idx = revisions.index(self.EVENT_REV)
        return set(revisions[max(0, idx - 1):idx + 2])

    def test_changepoint_flagged_at_event_for_affected_query(self):
        flagged = False
        for query in ('q0', 'q6'):
            near = self._revisions_near_event(query)
            for alert in self.alerts:
                if (alert['query_name'] == query
                        and alert['kind'] == 'changepoint'
                        and alert['revision'] in near):
                    flagged = True
                    self.assertLessEqual(alert['p_value'], 0.05)
        self.assertTrue(flagged,
                        'no changepoint at/adjacent to %s for q0/q6'
                        % self.EVENT_REV)

    def test_control_query_has_no_changepoint_at_event(self):
        near = self._revisions_near_event('q28')
        offenders = [a for a in self.alerts
                     if a['query_name'] == 'q28'
                     and a['kind'] == 'changepoint'
                     and a['revision'] in near]
        self.assertEqual(offenders, [])

    def test_deterministic_on_real_data(self):
        again = detect.detect(
            self.rows, window=0, min_points=8, iqr_multiplier=3.0,
            min_delta_pct=2.0, run_edivisive=True, permutations=199,
            pvalue=0.05, seed=42)
        self.assertEqual(again, self.alerts)


if __name__ == '__main__':
    unittest.main()
