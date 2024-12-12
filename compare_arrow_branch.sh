set -x -e


pushd ~/arrow-rs
git fetch -p apache



BENCH_COMMAND="cargo bench -p arrow --bench filter_kernels --all-features"
BENCH_FILTER=""
REPO_NAME="chloro-pn"
BRANCH_NAME="filter_for_FixedSizeByteArray"


#BENCH_COMMAND="cargo bench --bench arrow_reader --all-features"
#BENCH_FILTER=""
#REPO_NAME="etseidl"
#BRANCH_NAME="to_prim"

#BENCH_COMMAND="cargo bench --bench bit_mask --all-features"
#BENCH_FILTER=""
#REPO_NAME="alamb"
#BRANCH_NAME="fix-set-bits"

#BENCH_COMMAND="cargo bench -p arrow --bench comparison_kernels --all-features"
#BENCH_FILTER=""
#REPO_NAME="tlm365"
#BRANCH_NAME="regex-is-match-utf8"

#BENCH_COMMAND="cargo bench --bench cast_kernels --all-features"
#BENCH_FILTER=""
#REPO_NAME="dariocurr"
#BRANCH_NAME="master"

BENCH_COMMAND="cargo bench -p arrow --bench comparison_kernels --all-features"
BENCH_FILTER="like"
REPO_NAME="findepi"
BRANCH_NAME="findepi/fix-like-with-escapes-792c56"

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

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/master)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
$BENCH_COMMAND -- --save-baseline master  ${BENCH_FILTER}

critcmp master ${BRANCH_DISPLAY_NAME}

popd
