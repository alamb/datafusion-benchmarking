set -x -e
## This script tests planning speed of 37.0.0 against the speed on planning on main
git fetch -p apache
git fetch -p alamb
pushd ~/arrow-datafusion2

# remove old test runs
rm -rf target/criterion/

# use a version of 37 with the tpcds benchmarks
BRANCH_NAME="37.0.0"
git checkout alamb/37_bench
git reset --hard alamb/alamb/37_bench
cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}

echo "** Comparing to main"
git checkout main
git reset --hard apache/main
cargo update
cargo bench --bench sql_planner -- --save-baseline main

critcmp main ${BRANCH_NAME}
