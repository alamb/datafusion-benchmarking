set -x -e
pushd ~/arrow-datafusion/

# runs cargo bench on two branches in DataFusion


BENCH_COMMAND="cargo bench --bench sql_planner"
BENCH_FILTER=""
REPO_NAME="peter-toth"
BRANCH_NAME="make-cse-top-down-like"

#BENCH_COMMAND="cargo bench --bench sql_planner"
#BENCH_FILTER=""
#REPO_NAME="crepererum"
#BRANCH_NAME="crepererum/fix_collect_columns_o2"


#BENCH_COMMAND="cargo bench --bench substr"
#BENCH_FILTER=""
#REPO_NAME="Kev1n8"
#BRANCH_NAME="stringview-output-for-substr"

#BENCH_COMMAND="cargo bench --bench regx"
#BENCH_FILTER=""
#REPO_NAME="devanbenz"
#BRANCH_NAME="fix/12150-regexprep-err-2"

#BENCH_COMMAND="cargo bench --bench ltrim"
#BENCH_FILTER=""
#REPO_NAME="Rachelint"
#BRANCH_NAME="string-view-trim"

#BENCH_COMMAND="cargo bench --bench sql_planner"
#BENCH_FILTER=""
#REPO_NAME="peter-toth"
#BRANCH_NAME="extract-cse-logic"

#BENCH_COMMAND="cargo bench --bench sort"
#BENCH_FILTER=""
#REPO_NAME="alamb"
#BRANCH_NAME="alamb/main_copy"

#BENCH_COMMAND="cargo bench --bench sort"
#BENCH_FILTER=""
#REPO_NAME="jayzhan211"
#BRANCH_NAME="rrt-spm-upstream"

#BENCH_COMMAND="cargo bench --bench sql_planner"
#BENCH_FILTER=""
#REPO_NAME="blaginin"
#BRANCH_NAME="blaginin/switch-to-recursive-tree-iteration"

BENCH_COMMAND="cargo bench --bench sql_planner"
BENCH_FILTER=""
REPO_NAME="westonpace"
BRANCH_NAME="feat/async-catalog"


BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _

#git remote remove ${REPO_NAME}  || true
git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/arrow-datafusion.git || true # ignore exisitng remote error
#git remote add ${REPO_NAME} https://github.com/${REPO_NAME}/datafusion.git || true # ignore exisitng remote error
git fetch -p ${REPO_NAME}

git fetch -p apache

# remove old test runs
rm -rf target/criterion/

git checkout $BRANCH_NAME
git reset --hard "$REPO_NAME/$BRANCH_NAME"
cargo update

# Run on test branch
$BENCH_COMMAND -- --save-baseline ${BENCH_BRANCH_NAME} ${BENCH_FILTER}

# Run on master
MERGE_BASE=$(git merge-base HEAD apache/main)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
cargo update
$BENCH_COMMAND -- --save-baseline main  ${BENCH_FILTER}

critcmp main ${BENCH_BRANCH_NAME}

popd
