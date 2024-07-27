set -x -e
pushd ~/arrow-datafusion2


## This script tests planning speed of 37.0.0 against the speed on planning on main
git fetch -p apache
git fetch -p alamb

# remove old test runs
rm -rf target/criterion/

# use a version of 37 with the tpcds benchmarks
BRANCH_NAME="37.0.0"
git checkout alamb/37_bench
git reset --hard alamb/alamb/37_bench
cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}

echo "comparing to 38.0.0"
BRANCH_NAME="38.0.0"
git checkout 38.0.0
cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}

echo "comparing to 39.0.0"
BRANCH_NAME="39.0.0"
git checkout 39.0.0
cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}


echo "comparing to 40.0.0"
BRANCH_NAME="40.0.0"
git checkout 40.0.0
cargo update

cargo bench --bench sql_planner -- --save-baseline ${BRANCH_NAME}


echo "** Comparing to main"
git checkout main
git reset --hard apache/main
cargo update
cargo bench --bench sql_planner -- --save-baseline main

critcmp main 37.0.0 38.0.0 39.0.0 40.0.0


popd
