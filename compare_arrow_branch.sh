set -x -e


pushd ~/arrow-rs
git fetch -p apache

#git remote add samuelcolvin https://github.com/samuelcolvin/arrow-rs.git
git fetch -p samuelcolvin
BENCH_COMMAND="cargo bench -p arrow --bench comparison_kernels -F test_utils"
BENCH_FILTER="like"
REPO_NAME="samuelcolvin"
BRANCH_NAME="contains-performance"

# remove old test runs
rm -rf target/criterion/

git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BRANCH_NAME} ${BENCH_FILTER}

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/master)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
$BENCH_COMMAND -- --save-baseline master  ${BENCH_FILTER}

critcmp master ${BRANCH_NAME}

popd
