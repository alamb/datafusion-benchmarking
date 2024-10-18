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



#REPO_URL='https://github.com/Rachelint/arrow-datafusion.git'
#BRANCH_NAME='string-view-trim'

#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='alamb/improve_boolean_handling'


REPO_URL='https://github.com/alamb/arrow-datafusion.git'
BRANCH_NAME='alamb/const_generics'

REPO_URL='https://github.com/alamb/arrow-datafusion.git'
BRANCH_NAME='alamb/boolean_string_groups'

#REPO_URL='https://github.com/jayzhan211/arrow-datafusion.git'
#RANCH_NAME='rm-clone-v4'

#REPO_URL='https://github.com/mhilton/apache-arrow-datafusion.git'
#BRANCH_NAME='limit-nested-loop-join-record-batch-size'

#REPO_URL='https://github.com/jayzhan211/arrow-datafusion.git'
#BRANCH_NAME='single-mode-v4'


#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='alamb/min_max_strings'

#REPO_URL='https://github.com/alamb/arrow-datafusion.git'
#BRANCH_NAME='alamb/min_max_string_test'

#REPO_URL='https://github.com/goldmedal/datafusion.git'
#BRANCH_NAME='feature/12788-binary-as-string-opt'

REPO_URL='https://github.com/alamb/arrow-datafusion.git'
BRANCH_NAME='alamb/enable_string_view_by_default'


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

## Run against branch
export DATAFUSION_DIR=~/arrow-datafusion2
#./bench.sh run sort
./bench.sh run tpch
#./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
./bench.sh run clickbench_partitioned
#./bench.sh run tpch_mem

## Run against main
export DATAFUSION_DIR=~/arrow-datafusion3
#./bench.sh run sort
./bench.sh run tpch
#./bench.sh run tpch_mem
./bench.sh run clickbench_1
./bench.sh run clickbench_extended
./bench.sh run clickbench_partitioned
#./bench.sh run tpch_mem


## Compare
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
./bench.sh compare main_base "${BENCH_BRANCH_NAME}"
