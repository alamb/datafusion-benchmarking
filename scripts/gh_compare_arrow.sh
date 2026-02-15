set -x -e
##
# This script runs an arrow-rs --bench microbenchmark and posts the results
# to github tickets
#
# Usage
#
# gh_compare_arrow.sh  <$PR_URL>
# BENCH_NAME="<name>" BENCH_FILTER="args" gh_compare_arrow.sh <$PR_URL>
#
# Example
# BENCH_NAME=concatente_kernel gh_compare_arrow.sh https://github.com/apache/datafusion/pull/15466
#
# Uses directories like this
# ~/arrow-rs: branch + main
#
# And then reports the results to a pull request using the gh command line
#
# Install gh
# https://github.com/cli/cli
# https://github.com/cli/cli/releases/download/v2.69.0/gh_2.69.0_linux_amd64.deb
##

PR=$1
if [ -z "$PR" ] ; then
    echo "$0 <$PR_URL>"
fi

# Benchmarks to run
BENCH_NAME=${BENCH_NAME:-"concatenate_kernel"}
BENCH_FILTER=${BENCH_FILTER:-""}
BENCH_COMMAND="cargo bench --features=arrow,async,test_common,experimental,object_store --bench $BENCH_NAME "

## Timeout for each benchmark run in seconds (default 25 minutes)
BENCHMARK_TIMEOUT=${BENCHMARK_TIMEOUT:-1500}

######
# Fetch and checkout the remote branch in arrow-rs
######
pushd ~/arrow-rs

git reset --hard
git clean -f -d
git fetch -p apache
gh pr checkout -f $PR
git submodule update --init
MERGE_BASE=`git merge-base HEAD apache/main`
BRANCH_BASE=`git rev-parse HEAD`
BRANCH_NAME=`git rev-parse --abbrev-ref HEAD`
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
cargo update

# create comment saying the benchmarks are running
rm -f /tmp/comment.txt
cat >/tmp/comment.txt <<EOL
🤖 \`$0\` [gh_compare_arrow.sh](https://github.com/alamb/datafusion-benchmarking/blob/main/scripts/gh_compare_arrow.sh) Running
`uname -a`
Comparing $BRANCH_NAME ($BRANCH_BASE) to $MERGE_BASE [diff](https://github.com/apache/arrow-rs/compare/$MERGE_BASE..$BRANCH_BASE)
BENCH_NAME=$BENCH_NAME
BENCH_COMMAND=$BENCH_COMMAND
BENCH_FILTER=$BENCH_FILTER
BENCH_BRANCH_NAME=$BENCH_BRANCH_NAME
Results will be posted here when complete
EOL
# Post the comment to the ticket
gh pr comment -F /tmp/comment.txt $PR

# remove old test runs, if any
rm -rf target/criterion/

# Run on test branch
echo "** Pre-compiling benchmarks... **"
$BENCH_COMMAND --no-run
echo "** Running benchmarks on branch... **"
set +e
timeout ${BENCHMARK_TIMEOUT} $BENCH_COMMAND -- --save-baseline ${BENCH_BRANCH_NAME} ${BENCH_FILTER}
rc=$?
set -e
if [ $rc -eq 124 ]; then
    echo "TIMEOUT: Benchmark '${BENCH_NAME}' branch exceeded ${BENCHMARK_TIMEOUT}s limit"
    exit 124
elif [ $rc -ne 0 ]; then
    exit $rc
fi

# Run on main (merge base)
git reset --hard
git clean -f -d
git checkout $MERGE_BASE
echo "** Pre-compiling benchmarks for main... **"
$BENCH_COMMAND --no-run
echo "** Running benchmarks on main... **"
set +e
timeout ${BENCHMARK_TIMEOUT} $BENCH_COMMAND -- --save-baseline main ${BENCH_FILTER}
rc=$?
set -e
if [ $rc -eq 124 ]; then
    echo "TIMEOUT: Benchmark '${BENCH_NAME}' main exceeded ${BENCHMARK_TIMEOUT}s limit"
    exit 124
elif [ $rc -ne 0 ]; then
    exit $rc
fi

## Compare
rm -f /tmp/report.txt
critcmp main ${BENCH_BRANCH_NAME} > /tmp/report.txt 2>&1

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
