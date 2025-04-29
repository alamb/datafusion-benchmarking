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
    echo "gh_compare_branch_bench.sh <$PR_URL>"
fi

# Benchmarks to run
BENCH_NAME=${BENCH_NAME:-"concatenate_kernel"}
BENCH_FILTER=${BENCH_FILTER:-""}
BENCH_COMMAND="cargo bench --all-features --bench $BENCH_NAME "

######
# Fetch and checkout the remote branch in arrow-rs
######
pushd ~/arrow-rs

git reset --hard
git clean -f -d
git fetch -p apache
gh pr checkout $PR
MERGE_BASE=`git merge-base HEAD apache/main`
BRANCH_BASE=`git rev-parse HEAD`
BRANCH_NAME=`git rev-parse --abbrev-ref HEAD`
BENCH_BRANCH_NAME=${BRANCH_NAME//\//_} # mind blowing syntax to replace / with _
cargo update

# create comment saying the benchmarks are running
rm -f /tmp/comment.txt
cat >/tmp/comment.txt <<EOL
🤖 \`$0\` [Benchmark Script](https://github.com/alamb/datafusion-benchmarking/blob/main/gh_compare_arrow.sh) Running
`uname -a`
Comparing $BRANCH_NAME ($BRANCH_BASE) to $MERGE_BASE [diff](https://github.com/apache/datafusion/compare/$MERGE_BASE..$BRANCH_BASE)
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
$BENCH_COMMAND -- --save-baseline ${BRANCH_DISPLAY_NAME} ${BENCH_FILTER}


# Run on main (merge base)
git reset --hard
git clean -f -d
git checkout $MERGE_BASE
$BENCH_COMMAND -- --save-baseline main  ${BENCH_FILTER}

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
