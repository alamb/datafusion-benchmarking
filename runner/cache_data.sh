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
  cd "$BENCH_DIR" && ./bench.sh data "$BENCH" || true
  bench-cache upload "$BENCH" --bucket "$BUCKET" --data-dir "$BENCH_DIR/data" || true
fi
