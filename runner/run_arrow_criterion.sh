#!/usr/bin/env bash
#
# Runs arrow-rs criterion benchmarks comparing a PR branch to its merge-base.
# Adapted from scripts/gh_compare_arrow.sh for containerized execution.
#
# Required env: PR_URL, BENCH_NAME, GITHUB_TOKEN
# Optional env: BENCH_FILTER
#
set -euo pipefail

BENCH_NAME=${BENCH_NAME:-"concatenate_kernel"}
BENCH_FILTER=${BENCH_FILTER:-""}
BENCH_COMMAND="cargo bench --features=arrow,async,test_common,experimental,object_store --bench ${BENCH_NAME}"

WORK_DIR="/workspace/arrow-rs"

######
# Clone and checkout the PR branch
######
echo "=== Cloning arrow-rs ==="
git clone --depth=200 https://github.com/apache/arrow-rs.git "${WORK_DIR}"
cd "${WORK_DIR}"
git fetch origin
gh pr checkout "${PR_URL}" --force
git submodule update --init
MERGE_BASE=$(git merge-base HEAD origin/main)
BRANCH_BASE=$(git rev-parse HEAD)
BRANCH_NAME=$(git rev-parse --abbrev-ref HEAD)
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_}
cargo update

######
# Post "running" comment
######
cat > /tmp/comment.txt <<EOL
🤖 Arrow criterion benchmark running (GKE)
\`$(uname -a)\`
Comparing ${BRANCH_NAME} (${BRANCH_BASE}) to ${MERGE_BASE} [diff](https://github.com/apache/arrow-rs/compare/${MERGE_BASE}..${BRANCH_BASE})
BENCH_NAME=${BENCH_NAME}
BENCH_COMMAND=${BENCH_COMMAND}
BENCH_FILTER=${BENCH_FILTER}
Results will be posted here when complete
EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt

# Remove old criterion results
rm -rf target/criterion/

######
# Run on PR branch
######
echo "=== Running benchmark on PR branch ==="
${BENCH_COMMAND} -- --save-baseline "${BENCH_BRANCH_NAME}" ${BENCH_FILTER}

######
# Run on merge-base
######
echo "=== Running benchmark on merge-base ==="
git reset --hard
git clean -f -d
git checkout "${MERGE_BASE}"
${BENCH_COMMAND} -- --save-baseline main ${BENCH_FILTER}

######
# Compare and post results
######
rm -f /tmp/report.txt
critcmp main "${BENCH_BRANCH_NAME}" > /tmp/report.txt 2>&1

REPORT=$(cat /tmp/report.txt)
cat > /tmp/comment.txt <<EOL
🤖 Arrow criterion benchmark completed (GKE)

<details><summary>Details</summary>
<p>

\`\`\`
${REPORT}
\`\`\`

</p>
</details>

EOL
gh pr comment "${PR_URL}" --body-file /tmp/comment.txt
