#!/usr/bin/env bash
# Cache-aware benchmark data generation wrapper.
# Falls back to bench.sh data generation if no cache bucket is configured.
set -euo pipefail

BENCH=$1
BENCH_DIR=$2
BUCKET="${DATA_CACHE_BUCKET:-}"

if [ -z "$BUCKET" ]; then
  cd "$BENCH_DIR" && ./bench.sh data "$BENCH" || true
  exit 0
fi

if bench-cache download "$BENCH" --bucket "$BUCKET" --data-dir "$BENCH_DIR/data"; then
  echo "Cache HIT: $BENCH"
else
  echo "Cache MISS: $BENCH"
  # Upload only complete data. A failed generation must not leave a cache
  # marker, or every later run would get a "hit" on incomplete data.
  if (cd "$BENCH_DIR" && ./bench.sh data "$BENCH"); then
    bench-cache upload "$BENCH" --bucket "$BUCKET" --data-dir "$BENCH_DIR/data" || true
  else
    echo "Data generation failed for $BENCH; skipping cache upload" >&2
  fi
fi
