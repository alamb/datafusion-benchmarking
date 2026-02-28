#!/usr/bin/env bash
#
# Runs DataFusion bench.sh benchmarks comparing a PR branch to its merge-base.
# Adapted from scripts/gh_compare_branch.sh for containerized execution.
#
# Required env: PR_URL, BENCHMARKS (space-separated), GITHUB_TOKEN
#
set -euo pipefail

BENCHMARKS=${BENCHMARKS:-"tpch_mem clickbench_partitioned clickbench_extended"}
CARGO_COMMAND="cargo run --release"

REPO_URL="https://github.com/${REPO}.git"

BRANCH_DIR="/workspace/datafusion-branch"
BASE_DIR="/workspace/datafusion-base"
BENCH_DIR="/workspace/datafusion-bench"

######
# Clone and checkout the PR branch
######
echo "=== Cloning PR branch ==="
git clone --depth=200 "${REPO_URL}" "${BRANCH_DIR}"
cd "${BRANCH_DIR}"
git fetch origin
gh pr checkout "${PR_URL}" --force
MERGE_BASE=$(git merge-base HEAD origin/main)
BRANCH_BASE=$(git rev-parse HEAD)
BRANCH_NAME=$(git rev-parse --abbrev-ref HEAD)

# Start compiling branch in the background
cd benchmarks
${CARGO_COMMAND} --bin dfbench >> /tmp/branch_build.log 2>&1 &
BRANCH_PID=$!

######
# Clone and checkout the merge-base
######
echo "=== Cloning merge-base ==="
git clone --depth=200 "${REPO_URL}" "${BASE_DIR}"
cd "${BASE_DIR}"
git -c advice.detachedHead=false checkout "${MERGE_BASE}"

cd benchmarks
${CARGO_COMMAND} --bin dfbench >> /tmp/base_build.log 2>&1 &
BASE_PID=$!

######
# Post "running" comment
######
cat > /tmp/comment.txt <<EOL
🤖 Benchmark running (GKE) | [trigger](${COMMENT_URL})
\`$(uname -a)\`
Comparing ${BRANCH_NAME} (${BRANCH_BASE}) to ${MERGE_BASE} [diff](https://github.com/${REPO}/compare/${MERGE_BASE}..${BRANCH_BASE}) using: ${BENCHMARKS}
Results will be posted here when complete
EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt

echo "=== Waiting for builds ==="
wait ${BRANCH_PID}
wait ${BASE_PID}
echo "=== Builds complete ==="

######
# Run benchmarks from a third checkout (uses bench.sh data/run/compare)
######
echo "=== Setting up bench runner ==="
git clone --depth=200 "${REPO_URL}" "${BENCH_DIR}"
cd "${BENCH_DIR}"
git -c advice.detachedHead=false checkout origin/main
cd benchmarks

rm -rf results/*

for bench in ${BENCHMARKS}; do
    echo "** Creating data if needed for ${bench} **"
    ./bench.sh data "${bench}" || true

    echo "** Running ${bench} baseline (merge-base) **"
    export DATAFUSION_DIR="${BASE_DIR}"
    ./bench.sh run "${bench}"

    echo "** Running ${bench} branch **"
    export DATAFUSION_DIR="${BRANCH_DIR}"
    ./bench.sh run "${bench}"
done

######
# Compare and post results
######
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_}
rm -f /tmp/report.txt
./bench.sh compare HEAD "${BENCH_BRANCH_NAME}" | tee /tmp/report.txt

REPORT=$(cat /tmp/report.txt)
cat > /tmp/comment.txt <<EOL
🤖 Benchmark completed (GKE) | [trigger](${COMMENT_URL})

<details><summary>Details</summary>
<p>

\`\`\`
${REPORT}
\`\`\`

</p>
</details>

EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt
