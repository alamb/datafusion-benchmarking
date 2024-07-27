set -x -e
pushd ~/arrow-datafusion/

# runs cargo bench on two branches in DataFusion



#git remote add Rachelint https://github.com/Rachelint/arrow-datafusion.git
git fetch -p Rachelint
BENCH_COMMAND="cargo bench --bench parquet_statistic"
BENCH_FILTER=""
REPO_NAME="Rachelint"
BRANCH_NAME="improve-page-stats-convert"



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
