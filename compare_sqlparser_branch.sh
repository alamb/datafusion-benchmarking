set -x -e
#
# This script compares performance of two branches in sqlparser
#


pushd ~/datafusion-sqlparser-rs/
git fetch -p apache


BENCH_COMMAND="cargo bench"
BENCH_FILTER=""
REPO_NAME="davisp"
BRANCH_NAME="reduce-token-cloning"

git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/datafusion-sqlparser-rs.git || true

BRANCH_DISPLAY_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _

# remove old test runs
rm -rf target/criterion/

git fetch -p $REPO_NAME
git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"
cargo update

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BRANCH_DISPLAY_NAME} ${BENCH_FILTER}

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/master)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
$BENCH_COMMAND -- --save-baseline master  ${BENCH_FILTER}

critcmp master ${BRANCH_DISPLAY_NAME}

popd