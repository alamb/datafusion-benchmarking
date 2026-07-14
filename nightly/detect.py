#!/usr/bin/env python3
"""Regression detection over benchmark results CSVs.

Reads every *.csv in --results-dir (schema:
benchmark_name,query_name,query_type,execution_time,run_timestamp,
git_revision,git_revision_timestamp,num_cores), builds a per-(benchmark,query)
median series ordered by git_revision_timestamp, and runs two detectors:

1. Threshold (rustc-perf style): the newest night-over-night relative delta is
   compared against an IQR fence learned from that query's historical deltas,
   with an absolute floor of --min-delta-pct.
2. Changepoint (E-divisive means, MongoDB style): energy-statistic Q-hat
   maximized over split points with a permutation significance test,
   recursively splitting while p-value <= --pvalue.

Outputs alerts.json, alerts.md and detected_events.json to --output-dir.
Exit codes: 0 = ran fine and no regression alerts, 2 = regression alerts
present, 1 = error.
"""
import argparse
import bisect
import csv
import glob
import json
import os
import random
import sys
from datetime import datetime, timezone

CSV_COLUMNS = [
    'benchmark_name', 'query_name', 'query_type', 'execution_time',
    'run_timestamp', 'git_revision', 'git_revision_timestamp', 'num_cores',
]

# Fallback timestamp formats for values datetime.fromisoformat (3.9) rejects.
_FALLBACK_TS_FORMATS = [
    '%Y-%m-%d %H:%M:%S',
    '%Y-%m-%dT%H:%M:%S',
    '%Y-%m-%d %H:%M:%S.%f',
    '%Y-%m-%dT%H:%M:%S.%f',
    '%Y-%m-%d',
]


def parse_timestamp(value):
    """Parse an ISO-8601-ish timestamp into a tz-aware datetime (UTC assumed
    when no offset is present). Returns None if unparseable.

    Python 3.9's datetime.fromisoformat cannot parse a trailing 'Z'; handle
    that explicitly.
    """
    if value is None:
        return None
    text = value.strip()
    if not text:
        return None
    if text.endswith('Z') or text.endswith('z'):
        text = text[:-1] + '+00:00'
    dt = None
    try:
        dt = datetime.fromisoformat(text)
    except ValueError:
        for fmt in _FALLBACK_TS_FORMATS:
            try:
                dt = datetime.strptime(text, fmt)
                break
            except ValueError:
                continue
    if dt is None:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def load_results(results_dir):
    """Load and merge every *.csv in results_dir.

    Robustness rules:
    - a file whose first line does not start with 'benchmark_name' is treated
      as headerless and the known schema is applied;
    - only query_type == 'query' rows are kept;
    - malformed rows (too few columns, non-numeric execution_time,
      unparseable git_revision_timestamp) are skipped with a warning count;
    - NUL bytes are stripped, undecodable bytes are replaced, and any other
      csv.Error (e.g. a field exceeding the csv field-size limit) skips the
      remainder of that file, so a corrupted file cannot abort the run;
    - identical (benchmark_name, git_revision, query_name, execution_time,
      run_timestamp) rows are de-duplicated across files, multiplicity-aware:
      the Nth duplicate within one file only collapses with the Nth duplicate
      in another file, so genuine repeated measurements within a file (one
      run_timestamp per session, ms-quantized times) survive while exact-copy
      files still collapse fully.

    Returns a list of row dicts.
    """
    paths = sorted(glob.glob(os.path.join(results_dir, '*.csv')))
    if not paths:
        raise FileNotFoundError('no *.csv files found in %r' % results_dir)
    rows = []
    seen = set()
    skipped = 0
    for path in paths:
        occurrences = {}
        try:
            with open(path, 'r', newline='', encoding='utf-8',
                      errors='replace') as fh:
                # csv.reader raises on NUL bytes ("line contains NUL"); strip
                # them so one corrupted file cannot break the whole nightly run
                reader = csv.reader(line.replace('\0', '') for line in fh)
                first = True
                for record in reader:
                    if not record or all(not field.strip() for field in record):
                        continue
                    if first:
                        first = False
                        if record[0].strip() == 'benchmark_name':
                            continue  # header row present
                    if len(record) < len(CSV_COLUMNS):
                        skipped += 1
                        continue
                    row = dict(zip(CSV_COLUMNS, record))
                    if row['query_type'].strip() != 'query':
                        continue
                    try:
                        exec_time = float(row['execution_time'])
                    except ValueError:
                        skipped += 1
                        continue
                    rev_ts = parse_timestamp(row['git_revision_timestamp'])
                    if rev_ts is None:
                        skipped += 1
                        continue
                    base_key = (row['benchmark_name'], row['git_revision'],
                                row['query_name'], row['execution_time'],
                                row['run_timestamp'])
                    occurrence = occurrences.get(base_key, 0)
                    occurrences[base_key] = occurrence + 1
                    key = base_key + (occurrence,)
                    if key in seen:
                        continue
                    seen.add(key)
                    rows.append({
                        'benchmark_name': row['benchmark_name'].strip(),
                        'query_name': row['query_name'].strip(),
                        'execution_time': exec_time,
                        'run_timestamp': row['run_timestamp'].strip(),
                        'git_revision': row['git_revision'].strip(),
                        'git_revision_timestamp': row['git_revision_timestamp'].strip(),
                        'revision_dt': rev_ts,
                    })
        except csv.Error as exc:
            # e.g. a field exceeding csv.field_size_limit() in a truncated or
            # binary file; keep whatever parsed before the error
            print('warning: skipping rest of %s: %s' % (path, exc),
                  file=sys.stderr)
    if skipped:
        print('warning: skipped %d malformed row(s)' % skipped, file=sys.stderr)
    return rows


def median(values):
    ordered = sorted(values)
    n = len(ordered)
    mid = n // 2
    if n % 2 == 1:
        return ordered[mid]
    return (ordered[mid - 1] + ordered[mid]) / 2.0


def percentile(values, fraction):
    """Linear-interpolation percentile (numpy default) of a non-empty list."""
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = fraction * (len(ordered) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(ordered) - 1)
    weight = rank - lo
    return ordered[lo] * (1.0 - weight) + ordered[hi] * weight


def build_series(rows):
    """Group rows into per-(benchmark_name, query_name) median series.

    Each series is a list of point dicts ordered by git_revision_timestamp
    (ties broken by earliest run_timestamp, then by revision string):
    {'revision', 'revision_timestamp', 'revision_dt', 'median'}
    """
    grouped = {}
    for row in rows:
        series_key = (row['benchmark_name'], row['query_name'])
        rev_key = row['git_revision']
        bucket = grouped.setdefault(series_key, {}).setdefault(rev_key, {
            'times': [],
            'revision_dt': row['revision_dt'],
            'revision_timestamp': row['git_revision_timestamp'],
            'run_ts': None,
        })
        bucket['times'].append(row['execution_time'])
        run_dt = parse_timestamp(row['run_timestamp'])
        if run_dt is not None and (bucket['run_ts'] is None or run_dt < bucket['run_ts']):
            bucket['run_ts'] = run_dt
    epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    series = {}
    for series_key, revisions in grouped.items():
        points = []
        for revision, bucket in revisions.items():
            points.append({
                'revision': revision,
                'revision_timestamp': bucket['revision_timestamp'],
                'revision_dt': bucket['revision_dt'],
                'run_dt': bucket['run_ts'] if bucket['run_ts'] is not None else epoch,
                'median': median(bucket['times']),
            })
        points.sort(key=lambda p: (p['revision_dt'], p['run_dt'], p['revision']))
        series[series_key] = points
    return series


def threshold_detect(points, min_points, iqr_multiplier, min_delta_pct):
    """rustc-perf-style detector on the newest night-over-night delta.

    Returns an alert dict fragment or None. `points` is the (already
    window-trimmed) median series for one query.
    """
    if len(points) < min_points:
        return None
    deltas = []
    for prev, cur in zip(points, points[1:]):
        if prev['median'] <= 0:
            deltas.append(None)
        else:
            deltas.append((cur['median'] - prev['median']) / prev['median'])
    newest = deltas[-1]
    history = [d for d in deltas[:-1] if d is not None]
    if newest is None or len(history) < 2:
        return None
    q1 = percentile(history, 0.25)
    q3 = percentile(history, 0.75)
    iqr = q3 - q1
    floor = min_delta_pct / 100.0
    upper_fence = max(q3 + iqr_multiplier * iqr, floor)
    lower_fence = min(q1 - iqr_multiplier * iqr, -floor)
    if newest > upper_fence:
        direction = 'regression'
        fence = upper_fence
    elif newest < lower_fence:
        direction = 'improvement'
        fence = lower_fence
    else:
        return None
    last = points[-1]
    prev = points[-2]
    return {
        'kind': 'threshold',
        'direction': direction,
        'revision': last['revision'],
        'revision_timestamp': last['revision_timestamp'],
        'median_before_s': prev['median'],
        'median_after_s': last['median'],
        'delta_pct': newest * 100.0,
        'threshold_pct': fence * 100.0,
        'p_value': None,
    }


class _Fenwick(object):
    """Fenwick (binary indexed) tree over value ranks tracking counts and
    value sums, used to get sum(|x - p|) over an online set in O(log n)."""

    __slots__ = ('n', 'cnt', 'sm')

    def __init__(self, n):
        self.n = n
        self.cnt = [0] * (n + 1)
        self.sm = [0.0] * (n + 1)

    def add(self, rank, value):
        i = rank + 1
        cnt = self.cnt
        sm = self.sm
        n = self.n
        while i <= n:
            cnt[i] += 1
            sm[i] += value
            i += i & (-i)

    def query(self, rank):
        """(count, sum) of inserted values with rank <= rank."""
        i = rank + 1
        c = 0
        s = 0.0
        cnt = self.cnt
        sm = self.sm
        while i > 0:
            c += cnt[i]
            s += sm[i]
            i -= i & (-i)
        return c, s


def _prep_segment(values):
    """Precompute permutation-invariant structures for a segment.

    Returns (items, total_pair) where items is a list of
    (value, rank, rowsum) triples in original order; rowsum(v) is the sum of
    |v - z| over every z in the segment; total_pair is the sum of all
    pairwise distances.
    """
    ordered = sorted(values)
    n = len(ordered)
    prefix = [0.0] * (n + 1)
    for i, v in enumerate(ordered):
        prefix[i + 1] = prefix[i] + v
    uniq = []
    for v in ordered:
        if not uniq or v != uniq[-1]:
            uniq.append(v)
    rank_of = {}
    for r, v in enumerate(uniq):
        rank_of[v] = r
    items = []
    total_rowsum = 0.0
    for v in values:
        idx = bisect.bisect_left(ordered, v)
        # elements < v contribute idx*v - prefix[idx]; elements >= v
        # contribute (prefix[n]-prefix[idx]) - (n-idx)*v (equal ones are 0)
        rowsum = (idx * v - prefix[idx]) + ((prefix[n] - prefix[idx]) - (n - idx) * v)
        total_rowsum += rowsum
        items.append((v, rank_of[v], rowsum))
    return items, len(uniq), total_rowsum / 2.0


def _max_qhat(items, n_unique, total_pair):
    """Max e-divisive Q-hat over all split points of the sequence.

    items: list of (value, rank, rowsum) in sequence order.
    Returns (best_q, best_split) where the split t means clusters
    items[:t] / items[t:]. O(n log n).
    """
    n = len(items)
    tree = _Fenwick(n_unique)
    left_pair = 0.0
    left_rowsum = 0.0
    left_cnt = 0
    left_sum = 0.0
    best_q = float('-inf')
    best_t = -1
    for t in range(1, n):
        value, rank, rowsum = items[t - 1]
        cnt_le, sum_le = tree.query(rank)
        # distance from `value` to every element already in the left set
        left_pair += (cnt_le * value - sum_le) + \
                     ((left_sum - sum_le) - (left_cnt - cnt_le) * value)
        tree.add(rank, value)
        left_cnt += 1
        left_sum += value
        left_rowsum += rowsum
        cross = left_rowsum - 2.0 * left_pair
        right_pair = total_pair - left_pair - cross
        m = t
        r = n - t
        term1 = 2.0 * cross / (m * r)
        term2 = 2.0 * left_pair / (m * (m - 1)) if m > 1 else 0.0
        term3 = 2.0 * right_pair / (r * (r - 1)) if r > 1 else 0.0
        q = (float(m) * r / (m + r)) * (term1 - term2 - term3)
        if q > best_q:
            best_q = q
            best_t = t
    return best_q, best_t


def e_divisive(values, permutations, pvalue, rng, min_points=4):
    """E-divisive means changepoint detection with permutation testing.

    Recursively splits `values` while the permutation p-value <= pvalue.
    Returns a list of (index, p_value) tuples where `index` is the first
    point of the new regime, sorted by index. Deterministic for a given rng.
    """
    changepoints = []
    stack = [(0, list(values))]
    while stack:
        offset, segment = stack.pop()
        n = len(segment)
        if n < max(4, min_points):
            continue
        items, n_unique, total_pair = _prep_segment(segment)
        observed_q, split = _max_qhat(items, n_unique, total_pair)
        if split <= 0 or observed_q <= 0:
            continue
        greater_or_equal = 0
        shuffled = list(items)
        for _ in range(permutations):
            rng.shuffle(shuffled)
            perm_q, _unused = _max_qhat(shuffled, n_unique, total_pair)
            if perm_q >= observed_q:
                greater_or_equal += 1
        p = (greater_or_equal + 1.0) / (permutations + 1.0)
        if p <= pvalue:
            changepoints.append((offset + split, p))
            # recurse; push right first so left is processed first (order of
            # processing does not affect determinism: rng consumption order is
            # fixed by the stack discipline)
            stack.append((offset + split, segment[split:]))
            stack.append((offset, segment[:split]))
    changepoints.sort(key=lambda cp: cp[0])
    return changepoints


def changepoint_detect(points, permutations, pvalue, seed, min_points):
    """Run e-divisive over a query's median series; return alert fragments."""
    if len(points) < min_points:
        return []
    values = [p['median'] for p in points]
    rng = random.Random(seed)
    alerts = []
    changepoints = e_divisive(values, permutations, pvalue, rng, min_points=4)
    # before/after medians are taken over the segments adjacent to each
    # changepoint (bounded by the neighbouring changepoints) so that multiple
    # regimes in one window do not mix into a single median
    bounds = [0] + [index for index, _p in changepoints] + [len(values)]
    for i, (index, p) in enumerate(changepoints):
        before = median(values[bounds[i]:index])
        after = median(values[index:bounds[i + 2]])
        if before <= 0:
            continue
        delta = (after - before) / before
        point = points[index]
        alerts.append({
            'kind': 'changepoint',
            'direction': 'regression' if delta > 0 else 'improvement',
            'revision': point['revision'],
            'revision_timestamp': point['revision_timestamp'],
            'median_before_s': before,
            'median_after_s': after,
            'delta_pct': delta * 100.0,
            'threshold_pct': None,
            'p_value': p,
        })
    return alerts


def detect(rows, window, min_points, iqr_multiplier, min_delta_pct,
           run_edivisive, permutations, pvalue, seed):
    """Run both detectors over all series. Returns the list of alert dicts."""
    series = build_series(rows)
    alerts = []
    for (benchmark_name, query_name) in sorted(series):
        points = series[(benchmark_name, query_name)]
        if window > 0:
            points = points[-window:]
        found = []
        threshold_alert = threshold_detect(points, min_points,
                                           iqr_multiplier, min_delta_pct)
        if threshold_alert is not None:
            found.append(threshold_alert)
        if run_edivisive:
            found.extend(changepoint_detect(points, permutations, pvalue,
                                            seed, min_points))
        for alert in found:
            alert['benchmark_name'] = benchmark_name
            alert['query_name'] = query_name
            alerts.append(alert)
    alerts.sort(key=lambda a: (a['benchmark_name'], a['query_name'],
                               a['kind'], a['revision']))
    return alerts


ALERT_FIELDS = ['benchmark_name', 'query_name', 'kind', 'direction',
                'revision', 'revision_timestamp', 'median_before_s',
                'median_after_s', 'delta_pct', 'threshold_pct', 'p_value']

_EVENT_LABEL_MAX = 100


def format_alerts_md(alerts, generated_at, results_dir, window):
    regressions = [a for a in alerts if a['direction'] == 'regression']
    improvements = [a for a in alerts if a['direction'] == 'improvement']
    lines = []
    lines.append('# Nightly benchmark alerts')
    lines.append('')
    lines.append('- Generated: %s' % generated_at)
    lines.append('- Results dir: `%s`' % results_dir)
    lines.append('- Window: %d' % window)
    lines.append('- %d regression(s), %d improvement(s)' %
                 (len(regressions), len(improvements)))
    lines.append('')
    if not alerts:
        lines.append('No alerts.')
        lines.append('')
        return '\n'.join(lines)
    for title, group in (('Regressions', regressions),
                         ('Improvements', improvements)):
        if not group:
            continue
        lines.append('## %s' % title)
        lines.append('')
        lines.append('| Benchmark | Query | Kind | Revision | Before (s) | After (s) | Delta % | Threshold % | p-value |')
        lines.append('|---|---|---|---|---|---|---|---|---|')
        for a in group:
            lines.append('| %s | %s | %s | %s | %.4f | %.4f | %+.1f | %s | %s |' % (
                a['benchmark_name'], a['query_name'], a['kind'], a['revision'],
                a['median_before_s'], a['median_after_s'], a['delta_pct'],
                '%.1f' % a['threshold_pct'] if a['threshold_pct'] is not None else '-',
                '%.3f' % a['p_value'] if a['p_value'] is not None else '-'))
        lines.append('')
    return '\n'.join(lines)


def build_detected_events(alerts):
    """detected_events.json entries: one per changepoint revision, in the
    same shape as events.json ({'revision', 'label'})."""
    by_revision = {}
    order = []
    for a in alerts:
        if a['kind'] != 'changepoint':
            continue
        if a['revision'] not in by_revision:
            by_revision[a['revision']] = {
                'ts': a['revision_timestamp'],
                'parts': [],
            }
            order.append(a['revision'])
        by_revision[a['revision']]['parts'].append(
            '%s %+.0f%%' % (a['query_name'], a['delta_pct']))
    events = []
    suffix = ' (changepoint)'
    for revision in order:
        parts = by_revision[revision]['parts']
        label = parts[0]
        for i, part in enumerate(parts[1:], start=1):
            candidate = label + ', ' + part
            if len(candidate) + len(suffix) > _EVENT_LABEL_MAX:
                label += ' +%d more' % (len(parts) - i)
                break
            label = candidate
        events.append({'revision': revision, 'label': label + suffix})
    return events


def write_outputs(output_dir, alerts, results_dir, window):
    if not os.path.exists(output_dir):
        os.makedirs(output_dir)
    generated_at = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    payload = {
        'generated_at': generated_at,
        'results_dir': results_dir,
        'window': window,
        'alerts': [{field: a[field] for field in ALERT_FIELDS} for a in alerts],
    }
    with open(os.path.join(output_dir, 'alerts.json'), 'w', encoding='utf-8') as fh:
        json.dump(payload, fh, indent=2)
        fh.write('\n')
    with open(os.path.join(output_dir, 'alerts.md'), 'w', encoding='utf-8') as fh:
        fh.write(format_alerts_md(alerts, generated_at, results_dir, window))
    with open(os.path.join(output_dir, 'detected_events.json'), 'w', encoding='utf-8') as fh:
        json.dump(build_detected_events(alerts), fh, indent=2)
        fh.write('\n')


def main(argv=None):
    parser = argparse.ArgumentParser(
        description='Detect performance regressions in benchmark results')
    parser.add_argument('--results-dir', default='results',
                        help='Directory containing results *.csv files (default: results)')
    parser.add_argument('--output-dir', default='nightly/out',
                        help='Directory for alerts.json/alerts.md/detected_events.json (default: nightly/out)')
    parser.add_argument('--window', type=int, default=60,
                        help='Trailing number of revisions to analyse per query (default: 60)')
    parser.add_argument('--min-delta-pct', type=float, default=2.0,
                        help='Absolute floor (percent) for the threshold detector (default: 2.0)')
    parser.add_argument('--iqr-multiplier', type=float, default=3.0,
                        help='IQR fence multiplier for the threshold detector (default: 3.0)')
    parser.add_argument('--no-edivisive', action='store_true',
                        help='Disable the e-divisive changepoint detector')
    parser.add_argument('--pvalue', type=float, default=0.05,
                        help='Significance level for the changepoint permutation test (default: 0.05)')
    parser.add_argument('--permutations', type=int, default=199,
                        help='Number of permutations for the significance test (default: 199)')
    parser.add_argument('--seed', type=int, default=42,
                        help='RNG seed for the permutation test (default: 42)')
    parser.add_argument('--min-points', type=int, default=8,
                        help='Minimum series points required to run detection (default: 8)')
    args = parser.parse_args(argv)

    try:
        rows = load_results(args.results_dir)
        alerts = detect(rows,
                        window=args.window,
                        min_points=args.min_points,
                        iqr_multiplier=args.iqr_multiplier,
                        min_delta_pct=args.min_delta_pct,
                        run_edivisive=not args.no_edivisive,
                        permutations=args.permutations,
                        pvalue=args.pvalue,
                        seed=args.seed)
        write_outputs(args.output_dir, alerts, args.results_dir, args.window)
    except Exception as exc:
        print('error: %s' % exc, file=sys.stderr)
        return 1

    regressions = sum(1 for a in alerts if a['direction'] == 'regression')
    improvements = len(alerts) - regressions
    print('detect: %d regression alert(s), %d improvement(s); outputs in %s'
          % (regressions, improvements, args.output_dir))
    return 2 if regressions else 0


if __name__ == '__main__':
    sys.exit(main())
