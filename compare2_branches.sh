set -e -x
##
# This script runs the datafusion bench.sh script
# against two branches using two different checkouts
# ~/arrow-datafusion: branch.sh comparison
# ~/arrow-datafusion2: branch1
# ~/arrow-datafusion3: branch2
##

# setup python environment
source ~/venv/bin/activate

REPO1_URL='https://github.com/apache/datafusion.git'
BRANCH_NAME1='branch-45'

REPO2_URL='https://github.com/apache/datafusion.git'
BRANCH_NAME2='branch-46'

## Command used to pre-warm (aka precompile) the directories
CARGO_COMMAND="cargo run --release"

######
# Fetch and checkout the remote branch
######
pushd ~/arrow-datafusion2
# Move to a known branch
git branch -D alamb/temp || true
git checkout -b alamb/temp
git branch -D "${BRANCH_NAME1}" || true # clean any old copy
git fetch --force "${REPO1_URL}" "${BRANCH_NAME1}:${BRANCH_NAME1}"

# Checkout branch code into arrow-datafusion2
git checkout "${BRANCH_NAME1}"

# start compiling the branch (in the background)
cd benchmarks
${CARGO_COMMAND} --bin tpch > build.log 2>&1 &
${CARGO_COMMAND} --bin parquet > build.log 2>&1 &
popd

######
# checkout branch2 into ~/arrow-datafusion3
######
pushd ~/arrow-datafusion3
# Move to a known branch
git branch -D alamb/temp || true
git checkout -b alamb/temp
git branch -D "${BRANCH_NAME2}" || true # clean any old copy
git fetch --force "${REPO2_URL}" "${BRANCH_NAME2}:${BRANCH_NAME2}"

# Checkout branch code into arrow-datafusion3
git checkout "${BRANCH_NAME2}"

# start compiling the branch (in the background)
cd benchmarks
${CARGO_COMMAND} --bin tpch > build.log 2>&1 &
${CARGO_COMMAND} --bin parquet > build.log 2>&1 &
popd

echo "------------------"
echo "Wait for background compilation to complete..."
echo "------------------"
wait
echo "DONE"


######
# run the benchmark (from the arrow-datafusion directory
######
pushd ~/arrow-datafusion
## Generate data
cd benchmarks
./bench.sh data clickbench_partitioned
./bench.sh data clickbench_1

## Run against branch
export DATAFUSION_DIR=~/arrow-datafusion2
#./bench.sh run sort
#./bench.sh run tpch
#./bench.sh run tpch_mem
./bench.sh run clickbench_partitioned
./bench.sh run clickbench_extended


## Run against main
export DATAFUSION_DIR=~/arrow-datafusion3
#./bench.sh run sort
#./bench.sh run tpch
#./bench.sh run tpch_mem
./bench.sh run clickbench_partitioned
./bench.sh run clickbench_extended

## Compare
BENCH_BRANCH_NAME1=${BRANCH_NAME1//\//_} # mind blowing syntax to replace / with _
BENCH_BRANCH_NAME2=${BRANCH_NAME2//\//_} # mind blowing syntax to replace / with _
./bench.sh compare "${BENCH_BRANCH_NAME1}" "${BENCH_BRANCH_NAME2}"
