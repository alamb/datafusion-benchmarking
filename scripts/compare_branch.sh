set -e -x
##
# This script runs the datafusion bench.sh script
# against a branch and main
# ~/arrow-datafusion: branch.sh comparison
# ~/arrow-datafusion2: branch
# ~/arrow-datafusion3: main
##

# setup python environment
source ~/venv/bin/activate


## Command used to pre-warm (aka precompile) the directories
#CARGO_COMMAND="cargo run --profile release-nonlto"
CARGO_COMMAND="cargo run --release"


#REPO_URL='https://github.com/crepererum/arrow-datafusion.git'
#BRANCH_NAME='crepererum/issue13256_b'

#REPO_URL='https://github.com/westonpace/arrow-datafusion.git'
#BRANCH_NAME='feat/async-catalog'

#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='improve-performance-for-array-agg-merge-batch'

#REPO_URL='https://github.com/Rachelint/arrow-datafusion.git'
#BRANCH_NAME='impl-group-accumulator-for-median'

#REPO_URL='https://github.com/waynexia/arrow-datafusion.git'
#BRANCH_NAME='count-distinct-group'

#REPO_URL='https://github.com/pydantic/datafusion.git'
#BRANCH_NAME='topk-filter'

#REPO_URL='https://github.com/acking-you/arrow-datafusion.git'
#BRANCH_NAME='add_short_circuit'


REPO_URL='https://github.com/rluvaton/datafusion.git'
BRANCH_NAME='remove-clone-in-merge-perf'


#REPO_URL='https://github.com/alamb/datafusion.git'
#BRANCH_NAME='alamb/filter_pushdown'
#BRANCH_NAME='alamb/test_upgrade_54'



######
# Fetch and checkout the remote branch
######
pushd ~/arrow-datafusion2
git reset --hard
git checkout main
git branch -D "${BRANCH_NAME}" || true # clean any old copy

git fetch --force "${REPO_URL}" "${BRANCH_NAME}:${BRANCH_NAME}"
git fetch -p apache
MERGE_BASE=`git merge-base apache/main ${BRANCH_NAME}`

# Checkout branch code into arrow-datafusion3
git reset --hard
git checkout "${BRANCH_NAME}"
#cargo update

# start compiling the branch (in the background)
cd benchmarks
${CARGO_COMMAND} --bin tpch >> build.log 2>&1 &
${CARGO_COMMAND} --bin parquet >> build.log 2>&1 &
${CARGO_COMMAND} --bin dfbench >> build.log 2>&1 &
popd

######
# checkout main corresponding to place the branch diverges (merge-base)
######
pushd ~/arrow-datafusion3
git fetch -p apache
git reset --hard
git checkout "${MERGE_BASE}"
git branch -D "main_base" || true # clean any old copy
git checkout -b main_base "${MERGE_BASE}"
cargo update
cd benchmarks
${CARGO_COMMAND}  --bin tpch  >> build.log 2>&1 &
${CARGO_COMMAND}  --bin parquet  >> build.log 2>&1 &
${CARGO_COMMAND}  --bin dfbench  >> build.log 2>&1 &
popd

echo "------------------"
echo "Wait for background pre-compilation to complete..."
echo "------------------"
wait
echo "DONE"


######
# run the benchmark (from the arrow-datafusion directory
######
pushd ~/arrow-datafusion
## Generate data
cd benchmarks
#./bench.sh data
./bench.sh data tpch_10

## Run against branch
export DATAFUSION_DIR=~/arrow-datafusion2
#./bench.sh run sort
#./bench.sh run tpch
./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
./bench.sh run clickbench_partitioned
#./bench.sh run tpch_mem
#./bench.sh run h2o_medium


## Run against main
export DATAFUSION_DIR=~/arrow-datafusion3
#./bench.sh run sort
#./bench.sh run tpch
./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
./bench.sh run clickbench_partitioned
#./bench.sh run tpch_mem
#./bench.sh run h2o_medium


## Compare
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
./bench.sh compare main_base "${BENCH_BRANCH_NAME}"
