set -e -x
##
# This script runs criterion benchmarks against a branch
##

REPO_URL='https://github.com/tustvold/arrow-datafusion.git'
BRANCH_NAME='update-arrow-37'

BENCHMARK="cargo bench --bench sql_planner"

pushd ~/arrow-datafusion
pwd

# Fetch the remote branch
git checkout main
git branch -D "${BRANCH_NAME}" || true # clean any old copy
git branch -D "main_compare" || true # clean any old copy

git fetch --force "${REPO_URL}" "${BRANCH_NAME}:${BRANCH_NAME}"
git fetch -p apache

# checkout main at the right location
MERGE_BASE=`git merge-base apache/main ${BRANCH_NAME}`
echo "Checking out merge-base ${MERGE_BASE} where ${BRANCH_NAME} deviates from HEAD"
git checkout -b main_compare ${MERGE_BASE}

# run the benchmark
${BENCHMARK}

# checkout branch code and rerun the benchmarks
git checkout "${BRANCH_NAME}"
${BENCHMARK}
