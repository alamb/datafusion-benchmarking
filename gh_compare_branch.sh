set -e -x
##
# This script runs the datafusion bench.sh script
# against a branch and main
# ~/arrow-datafusion: branch.sh comparison
# ~/arrow-datafusion2: branch
# ~/arrow-datafusion3: main
#
# And then reports the results to a pull request using the gh command line
#
# Install gh
# https://github.com/cli/cli
# https://github.com/cli/cli/releases/download/v2.69.0/gh_2.69.0_linux_amd64.deb
##

# setup python environment
source ~/venv/bin/activate

# Usage
# gh_compare_branch.sh <$PR_URL>
#
# Example
# https://github.com/apache/datafusion/pull/15466

PR=$1
if [ -z "$PR" ] ; then
    echo "gh_compare_branch.sh <$PR_URL>"
fi

## Benchmarks to run (bench.sh run <BENCHMARK>)
BENCHMARKS="tpch_mem"
#./bench.sh run sort
#./bench.sh run tpch
#./bench.sh run tpch_mem
#./bench.sh run clickbench_1
#./bench.sh run clickbench_extended
#./bench.sh run clickbench_partitioned
#./bench.sh run tpch_mem
#./bench.sh run h2o_medium

## Command used to pre-warm (aka precompile) the directories
CARGO_COMMAND="cargo run --release"

######
# Fetch and checkout the remote branch in arrow-datafusion2
######

pushd ~/arrow-datafusion2
git reset --hard
git fetch -p apache
gh pr checkout $PR
MERGE_BASE=`git merge-base HEAD apache/main `
BRANCH_NAME=`git rev-parse --abbrev-ref HEAD`

# start compiling the branch (in the background)
cd benchmarks
#${CARGO_COMMAND} --bin tpch >> build.log 2>&1 &
#${CARGO_COMMAND} --bin parquet >> build.log 2>&1 &
#${CARGO_COMMAND} --bin dfbench >> build.log 2>&1 &
popd


######
# checkout main corresponding to place the branch diverges (merge-base)
# in arrow-datafusion3
######

pushd ~/arrow-datafusion3
git reset --hard
git checkout $MERGE_BASE

cd benchmarks
#${CARGO_COMMAND}  --bin tpch  >> build.log 2>&1 &
#${CARGO_COMMAND}  --bin parquet  >> build.log 2>&1 &
#${CARGO_COMMAND}  --bin dfbench  >> build.log 2>&1 &
popd

# create comment saying the benchmarks are running
rm -f /tmp/comment.txt
cat >/tmp/comment.txt <<EOL
$0 Benchmark Script Running
`uname -a`
Comparing $BRANCH_NAME to $MERGE_BASE
Benchmarks: $BENCHMARKS
EOL
gh pr comment -F /tmp/comment.txt $PR

echo "------------------"
echo "Wait for background pre-compilation to complete..."
echo "------------------"
wait
echo "DONE"

exit 0

######
# run the benchmark (from the arrow-datafusion directory
######
pushd ~/arrow-datafusion
git checkout main
git pull
cd benchmarks
#./bench.sh data
# clear old results
rm -rf results/*


for bench in $BENCHMARKS ; do
    ## Run against main
    echo "** Running $bench baseline.. **"
    export DATAFUSION_DIR=~/arrow-datafusion3
    ./bench.sh run $bench
    ## Run against branch
    echo "** Running $bench branch.. **"
    echo "** Running branch $benchmark


## Compare
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
./bench.sh compare main_base "${BENCH_BRANCH_NAME}"
