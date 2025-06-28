set -x -e
pushd ~/arrow-datafusion4/

#git remote add wiedld https://github.com/wiedld/arrow-datafusion.git
#git fetch -p wiedld
#git remote add comphead https://github.com/comphead/arrow-datafusion.git
#git fetch -p comphead
#git remote add Jefffrey https://github.com/Jefffrey/arrow-datafusion.git
#git fetch -p Jefffrey
git fetch -p apache
git fetch -p alamb

# remove old test runs
rm -rf target/criterion/

#git checkout feat/make-dfschema-wrap-schemaref
#git checkout 9090-optimize-to-timestamp-with-format
#git checkout 9870/common-expr-elimination-id-tracking
#git checkout alamb/optimizer_tree_node


#BRANCH_NAME="optimizer_tree_node2"
#git checkout alamb/optimizer_tree_node2
#git reset --hard alamb/alamb/optimizer_tree_node2


BRANCH_NAME="optimize_pushdown"
git checkout alamb/optimize_pushdown
git reset --hard alamb/alamb/optimize_pushdown


#BRANCH_NAME="refactor_create_initial_plan"
#git checkout refactor_create_initial_plan
#git reset --hard Jefffrey/refactor_create_initial_plan

cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}

MERGE_BASE=$(git merge-base HEAD apache/main)
echo "** Comparing to ${MERGE_BASE}"

git checkout ${MERGE_BASE}
cargo bench --bench sql_planner -- --save-baseline main

critcmp main ${BRANCH_NAME}

popd
