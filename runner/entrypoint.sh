#!/usr/bin/env bash
set -euo pipefail

# Required environment variables:
#   GITHUB_TOKEN  - for gh CLI auth
#   PR_URL        - PR URL (e.g. https://github.com/apache/datafusion/pull/12345)
#   BENCH_TYPE    - standard | criterion | arrow_criterion | main_tracking
#   BENCHMARKS    - space-separated benchmark names (for standard type)
#   BENCH_NAME    - benchmark name (for criterion types)
#   REPO          - repo name (e.g. apache/datafusion)

# gh CLI automatically uses GITHUB_TOKEN from the environment

OUTPUT_FILE="/tmp/benchmark_output.txt"
: > "${OUTPUT_FILE}"

error_handler() {
  local exit_code=$?
  set +e

  local tail_output
  tail_output="$(tail -n 20 "$OUTPUT_FILE" 2>/dev/null || true)"

  local body_file
  body_file="$(mktemp)"
  {
    echo "Benchmark script failed with exit code ${exit_code}."
    echo
    echo "Last 20 lines of output:"
    echo "<details><summary>Click to expand</summary>"
    echo
    echo '```'
    echo "${tail_output}"
    echo '```'
    echo
    echo "</details>"
  } > "${body_file}"

  gh pr comment "${PR_URL}" --body-file "${body_file}" || true
  rm -f "${body_file}"
  exit 1
}

trap error_handler ERR

exec > >(tee -a "${OUTPUT_FILE}") 2>&1

case "${BENCH_TYPE}" in
  standard)
    /scripts/run_bench_sh.sh
    ;;
  criterion)
    /scripts/run_criterion.sh
    ;;
  arrow_criterion)
    /scripts/run_arrow_criterion.sh
    ;;
  main_tracking)
    # CI harness test: run a small benchmark against latest main
    /scripts/run_bench_sh.sh
    ;;
  *)
    echo "Unknown BENCH_TYPE: ${BENCH_TYPE}"
    exit 1
    ;;
esac
