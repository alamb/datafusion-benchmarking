set -x -e
#git  remote add korowa https://github.com/korowa/arrow-rs.git
#git remote add apache https://github.com/apache/arrow-rs.git
#git remote add XiangpengHao https://github.com/XiangpengHao/arrow-rs.git

#BENCH_COMMAND="cargo bench --all-features --bench row_format"
#BENCH_FILTER="convert_rows"
#REPO_NAME="korowa"
#BRANCH_NAME="encode-skip-iter"

# cargo bench --bench comparison_kernels String
BENCH_COMMAND="cargo bench --bench comparison_kernels"
BENCH_FILTER="like"
REPO_NAME="XiangpengHao"
BRANCH_NAME="string-view-like"

#BENCH_COMMAND="cargo bench --bench comparison_kernels "
#BENCH_FILTER="StringView"
#REPO_NAME="XiangpengHao"
#BRANCH_NAME="row-view"

BENCH_COMMAND="cargo bench --bench arrow_reader --all-features"
BENCH_FILTER="View"
REPO_NAME="XiangpengHao"
BRANCH_NAME="parquet-string-view-2"

git fetch -p apache
git fetch -p korowa
git fetch -p XiangpengHao

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
