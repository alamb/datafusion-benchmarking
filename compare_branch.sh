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


#REPO_URL='https://github.com/viirya/arrow-datafusion.git'
#BRANCH_NAME='relex_sort_merge_join_keys'

#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='alamb/specialized_group_keys'

#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='alamb/specialized_group_keys_skip_overflow'

#REPO_URL='https://github.com/asimsedhain/arrow-datafusion.git'
#BRANCH_NAME='df-mem-pool/6934/debug-dump-memory-pool'

#REPO_URL='https://github.com/Lordworms/arrow-datafusion.git'
#BRANCH_NAME='issue_9328_2'

#REPO_URL='https://github.com/alamb/datafusion.git'
#BRANCH_NAME='alamb/vectorized_stats'

#REPO_URL='https://github.com/alamb/datafusion.git'
#BRANCH_NAME='alamb/less_allocation'

#REPO_URL='https://github.com/coralogix/arrow-datafusion.git'
#BRANCH_NAME='array_agg-groups-accumulator-v2'

#REPO_URL='https://github.com/jayzhan211/arrow-datafusion.git'
#BRANCH_NAME='multi-group-v3'

#REPO_URL='https://github.com/acking-you/arrow-datafusion.git'
#BRANCH_NAME='add_short_circuit'

#REPO_URL='https://github.com/korowa/arrow-datafusion.git'
#BRANCH_NAME='skip-partial-aggregation'

#REPO_URL='https://github.com/alamb/datafusion.git'
#BRANCH_NAME='alamb/coalsece_batches_in_struct'


REPO_URL='https://github.com/Rachelint/arrow-datafusion.git'
BRANCH_NAME='check-hash-first'



## Command used to pre-warm (aka precompile) the directories
#CARGO_COMMAND="cargo run --profile release-nonlto"
CARGO_COMMAND="cargo run --release"


######
# Fetch and checkout the remote branch
######
pushd ~/arrow-datafusion2
git checkout main
git branch -D "${BRANCH_NAME}" || true # clean any old copy

git fetch --force "${REPO_URL}" "${BRANCH_NAME}:${BRANCH_NAME}"
git fetch -p apache
MERGE_BASE=`git merge-base apache/main ${BRANCH_NAME}`

# Checkout branch code into arrow-datafusion3
git checkout "${BRANCH_NAME}"
cargo update

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
#./bench.sh data

## Run against branch
export DATAFUSION_DIR=~/arrow-datafusion2
#./bench.sh run sort
./bench.sh run tpch
./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
#./bench.sh run tpch_mem

## Run against main
export DATAFUSION_DIR=~/arrow-datafusion3
#./bench.sh run sort
./bench.sh run tpch
./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
#./bench.sh run tpch_mem


## Compare
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
./bench.sh compare main_base "${BENCH_BRANCH_NAME}"
