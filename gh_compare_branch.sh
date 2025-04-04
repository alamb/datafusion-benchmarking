set -e -x
##
# This script runs the datafusion bench.sh script
# Usage
#
# gh_compare_branch.sh <$PR_URL>
# BENCHMARKS="clickbench_1 tpchmem" gh_compare_branch.sh <$PR_URL>
#
# Example
# https://github.com/apache/datafusion/pull/15466
#
# Uses directories like this
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


PR=$1
if [ -z "$PR" ] ; then
    echo "gh_compare_branch.sh <$PR_URL>"
fi

## Benchmarks to run (bench.sh run <BENCHMARK>)
## Default suite is tpch and clickbench
BENCHMARKS=${BENCHMARKS:-"tpch_mem clickbench_partitioned clickbench_extended"}


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
${CARGO_COMMAND} --bin tpch >> build.log 2>&1 &
${CARGO_COMMAND} --bin parquet >> build.log 2>&1 &
${CARGO_COMMAND} --bin dfbench >> build.log 2>&1 &
popd


######
# checkout main corresponding to place the branch diverges (merge-base)
# in arrow-datafusion3
######

pushd ~/arrow-datafusion3
git reset --hard
git fetch -p apache
git checkout $MERGE_BASE

cd benchmarks
${CARGO_COMMAND}  --bin tpch  >> build.log 2>&1 &
${CARGO_COMMAND}  --bin parquet  >> build.log 2>&1 &
${CARGO_COMMAND}  --bin dfbench  >> build.log 2>&1 &
popd

# create comment saying the benchmarks are running
rm -f /tmp/comment.txt
cat >/tmp/comment.txt <<EOL
🤖 \`$0\` [Benchmark Script](https://github.com/alamb/datafusion-benchmarking) Running
`uname -a`
Comparing $BRANCH_NAME to $MERGE_BASE using Benchmarks: $BENCHMARKS
Results will be posted here when complete
EOL
# Post the comment to the ticket
gh pr comment -F /tmp/comment.txt $PR

echo "------------------"
echo "Wait for background pre-compilation to complete..."
echo "------------------"
wait
echo "DONE"



######
# run the benchmark (from the arrow-datafusion directory
######
pushd ~/arrow-datafusion
git checkout main
git pull
cd benchmarks

# clear old results
rm -rf results/*


for bench in $BENCHMARKS ; do
    echo "** Creating data if needed **"
    ./bench.sh data $bench
    echo "** Running $bench baseline (merge-base from main)... **"
    export DATAFUSION_DIR=~/arrow-datafusion3
    ./bench.sh run $bench
    ## Run against branch
    echo "** Running $bench branch... **"
    export DATAFUSION_DIR=~/arrow-datafusion2
    ./bench.sh run $bench

done

## Compare
rm -f /tmp/report.txt
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
./bench.sh compare HEAD "${BENCH_BRANCH_NAME}" | tee -a /tmp/report.txt

# Post the results as comment to the PR
REPORT=$(cat /tmp/report.txt)
cat >/tmp/comment.txt <<EOL
🤖: Benchmark completed

<details><summary>Details</summary>
<p>


\`\`\`
$REPORT
\`\`\`


</p>
</details>

EOL
gh pr comment -F /tmp/comment.txt $PR
