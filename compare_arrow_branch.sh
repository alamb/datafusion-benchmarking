set -x -e


pushd ~/arrow-rs
git fetch -p apache



#BENCH_COMMAND="cargo bench -p arrow --bench json_reader --all-features"
BENCH_COMMAND="cargo bench --bench ipc_reader"
BENCH_FILTER=""
REPO_NAME="alamb"
BRANCH_NAME="alamb/ipc_disable_validation"

#BENCH_COMMAND="cargo bench --bench filter_kernels --all-features"
#BENCH_FILTER="string"
#REPO_NAME="Dandandan"
#BRANCH_NAME="speedup_filter"

#BENCH_COMMAND="cargo bench --bench filter_kernels --all-features"
#BENCH_FILTER=""
#REPO_NAME="delamarch3"
#BRANCH_NAME="run-end-filter-safety"


git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/arrow-rs.git || true


BRANCH_DISPLAY_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _

# remove old test runs
rm -rf target/criterion/

git fetch -p $REPO_NAME
git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"
cargo update

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BRANCH_DISPLAY_NAME} ${BENCH_FILTER}

# Run on main
MERGE_BASE=$(git merge-base HEAD apache/main)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
$BENCH_COMMAND -- --save-baseline main  ${BENCH_FILTER}

critcmp main ${BRANCH_DISPLAY_NAME}

popd
