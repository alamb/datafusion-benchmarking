#!/usr/bin/env bash
#
# Runs DataFusion criterion benchmarks comparing a PR branch to its merge-base.
# Adapted from scripts/gh_compare_branch_bench.sh for containerized execution.
#
# Required env: PR_URL, BENCH_NAME, GITHUB_TOKEN
# Optional env: BENCH_FILTER
#
set -euo pipefail

BENCH_NAME=${BENCH_NAME:-"sql_planner"}
BENCH_FILTER=${BENCH_FILTER:-""}
BENCH_COMMAND="cargo bench --features=parquet --bench ${BENCH_NAME}"

REPO_URL="https://github.com/${REPO}.git"

BRANCH_DIR="/workspace/datafusion-branch"
BASE_DIR="/workspace/datafusion-base"

######
# Clone and checkout the PR branch
######
echo "=== Cloning PR branch ==="
git clone --depth=200 "${REPO_URL}" "${BRANCH_DIR}"
cd "${BRANCH_DIR}"
PR_NUMBER="${PR_URL##*/}"
BRANCH_NAME=$(gh pr view "${PR_URL}" --json headRefName --jq '.headRefName')
git fetch origin "refs/pull/${PR_NUMBER}/head:${BRANCH_NAME}" main
git checkout "${BRANCH_NAME}"
MERGE_BASE=$(git merge-base HEAD origin/main)
BRANCH_BASE=$(git rev-parse HEAD)
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_}

######
# Clone and checkout the merge-base
######
echo "=== Cloning merge-base ==="
git clone --depth=200 "${REPO_URL}" "${BASE_DIR}"
cd "${BASE_DIR}"
git -c advice.detachedHead=false checkout "${MERGE_BASE}"

######
# Post "running" comment
######
cat > /tmp/comment.txt <<EOL
🤖 Criterion benchmark running (GKE) | [trigger](${COMMENT_URL})
\`$(uname -a)\`
Comparing ${BRANCH_NAME} (${BRANCH_BASE}) to ${MERGE_BASE} [diff](https://github.com/${REPO}/compare/${MERGE_BASE}..${BRANCH_BASE})
BENCH_NAME=${BENCH_NAME}
BENCH_COMMAND=${BENCH_COMMAND}
BENCH_FILTER=${BENCH_FILTER}
Results will be posted here when complete
EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt

######
# Compile both in parallel (--no-run compiles without executing)
######
echo "=== Compiling PR branch and merge-base in parallel ==="
cd "${BRANCH_DIR}"
${BENCH_COMMAND} --no-run >> /tmp/branch_build.log 2>&1 &
BRANCH_PID=$!

cd "${BASE_DIR}"
${BENCH_COMMAND} --no-run >> /tmp/base_build.log 2>&1 &
BASE_PID=$!

BUILD_FAILED=0
wait ${BRANCH_PID} || BUILD_FAILED=1
if [ ${BUILD_FAILED} -ne 0 ]; then
    echo "=== Branch build failed ==="
    cat /tmp/branch_build.log
    exit 1
fi
wait ${BASE_PID} || BUILD_FAILED=1
if [ ${BUILD_FAILED} -ne 0 ]; then
    echo "=== Base build failed ==="
    cat /tmp/base_build.log
    exit 1
fi
echo "=== Compilation complete ==="

######
# Run benchmarks sequentially to avoid interference
######
echo "=== Running benchmark on merge-base ==="
cd "${BASE_DIR}"
${BENCH_COMMAND} -- --save-baseline main ${BENCH_FILTER}

echo "=== Running benchmark on PR branch ==="
cd "${BRANCH_DIR}"
${BENCH_COMMAND} -- --save-baseline "${BENCH_BRANCH_NAME}" ${BENCH_FILTER}

######
# Copy baselines into one target dir for critcmp
######
cp -r "${BASE_DIR}/target/criterion/"* "${BRANCH_DIR}/target/criterion/" 2>/dev/null || true

######
# Compare and post results
######
cd "${BRANCH_DIR}"
rm -f /tmp/report.txt
critcmp main "${BENCH_BRANCH_NAME}" > /tmp/report.txt 2>&1

REPORT=$(cat /tmp/report.txt)
cat > /tmp/comment.txt <<EOL
🤖 Criterion benchmark completed (GKE) | [trigger](${COMMENT_URL})

<details><summary>Details</summary>
<p>

\`\`\`
${REPORT}
\`\`\`

</p>
</details>

EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt
