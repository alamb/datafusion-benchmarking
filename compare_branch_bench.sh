set -x -e
pushd ~/arrow-datafusion/

# runs cargo bench on two branches in DataFusion


BENCH_COMMAND="cargo bench --bench sql_planner"
BENCH_FILTER=""
REPO_NAME="peter-toth"
BRANCH_NAME="make-cse-top-down-like"


git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/arrow-datafusion.git || true # ignore exisitng remote error
git fetch -p ${REPO_NAME}

git fetch -p apache

# remove old test runs
rm -rf target/criterion/

git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BRANCH_NAME} ${BENCH_FILTER}

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/main)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
$BENCH_COMMAND -- --save-baseline main  ${BENCH_FILTER}

critcmp main ${BRANCH_NAME}

popd
