set -x -e
pushd ~/arrow-datafusion/

# runs cargo bench on two branches in DataFusion


#BENCH_COMMAND="cargo bench --bench case_when "
BENCH_COMMAND="cargo bench --bench sql_planner "
BENCH_FILTER="all"
REPO_NAME="Omega359"
BRANCH_NAME="feature/scalar_func_args_session_config"

BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _

#git remote remove ${REPO_NAME}  || true
git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/arrow-datafusion.git || true # ignore exisitng remote error
#git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/datafusion.git || true # ignore exisitng remote error
git fetch -p ${REPO_NAME}

git fetch -p apache

# remove old test runs
rm -rf target/criterion/

git reset --hard
#git checkout $BRANCH_NAME --no-guess
git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"
cargo update

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BENCH_BRANCH_NAME} ${BENCH_FILTER}

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/main)
echo "** Comparing to ${MERGE_BASE}"

git reset --hard
git checkout ${MERGE_BASE}
cargo update
$BENCH_COMMAND -- --save-baseline main  ${BENCH_FILTER}

critcmp main ${BENCH_BRANCH_NAME}

popd
