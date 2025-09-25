# DataFusion Benchmarking Over Time

Code for benchmarking the DataFusion query engine over time: https://alamb.github.io/datafusion-benchmarking/

## About:

This repo contains scripts used to run benchmarks against the DataFusion query
engine, specifically using the `datafusion-cli` binary. The benchmarks are
designed to measure the performance of various queries and operations in
DataFusion over time.

It is hugely inspired by Mike McCandless's https://benchmarks.mikemccandless.com/

> The goal is to spot any long-term regressions (or, gains!) in ~Lucene's~ DataFusion's performance that might otherwise accidentally slip past the committers, hopefully avoiding the fate of the [boiling frog](https://en.wikipedia.org/wiki/Boiling_frog)

## Design Notes

This is purposely not in the actual DataFusion repo so that it can be run
against any DataFusion version, including the latest master branch and
historical releases.

Because building and benchmarking DataFusion takes a long time, the scripts
separate the build, execution, and reporting phase, saving all intermediate
results as files which allows easily re-running benchmarks or analyzing results
without needing to rebuild


## Directory Structure
The directory structure is as follows:

1. `data`: benchmark input data (symlink to `datafusion/data`)
2. `queries`: query scripts (new copy)
3. `results`: output directory for benchmark results
4. `builds`: directory binaries, named with the format `datafusion-cli@<revision>@<revision_timestamp>`
4. `scripts`: scripts for manually running benchmarks (not part of this report)


## TODOs:
1. Automate parallel builds of multiple DataFusion versions
2. Rerun benchmarks on a dedicated machine (ec2 metal?)
3. Rerun the benchmarks on a regular basis (cron job?)


1. Add clickbench extended queries
2. Add tpch queries
3. Add h2o.ai benchmarks
4. sqlplanner benchmarks

## Prerequisites

### Install DataFusion:

```shell
git clone git@github.com:apache/datafusion.git
```

## Benchmark Flow

### Step 1: Build `datafusion-cli`

Build datafusion-cli for the desired version(s) of DataFusion using the `build_datafusion.sh` script, which will:
   - Checkout the specified version of DataFusion.
   - Build the datafusion-cli using the `cargo build --release` command.
   - Copy the built binary to the `builds` directory with a specific naming convention.

   Example usage:
   ```shell
   git clone git@github.com:apache/datafusion.git
   ./build_datafusion.sh 47.0.0
   ```

   Example building datafusion-cli for version 48
   ```rust
   DATAFUSION_DIR=/home/alamb/arrow-datafusion2 ./build_datafusion_cli.sh 48.0.0
   ```

Note the `build-releases.sh` script builds the releases used for DataFusion blogs
such as [this one](https://datafusion.apache.org/blog/2025/07/28/datafusion-49.0.0/)

### Step 2: Run Benchmarks

The  `./benchmark.py` script can be used to run benchmarks for `datafusion-cli` binaries in `builds`
Results are left in the `results` directory, with each benchmark's results stored in a separate CSV file.

### Step 3: Analyze Results

You can then produce reports using the provided report generator script

```shell
# Run analysis on the results (outputs to docs/ directory)
./report.py

# Or specify a custom results directory
./report.py --results-dir results
```

## Automation

TODO: create a cron job or similar to automate the daily builds and tests.
Ideally it will automatically build datafusion-cli for all commits in the last day
and run the benchmarks, storing the results for later analysis.

builds.sh (remaining builds)

Then we'll generate the benchmark results

Then we'll do some charting and analaysis along with starting to automate the daily builds






## Cookbook:

The commands used to build the datafusion-cli command have changed over time
To build older versions, use the `build_datafusion_cli_old` script:

```
DATAFUSION_DIR=/home/alamb/arrow-datafusion2 ./build_datafusion_cli_old.sh 45.0.0 > 45.0.0.log 2>&1 &
DATAFUSION_DIR=/home/alamb/arrow-datafusion3 ./build_datafusion_cli_old.sh 44.0.0 > 44.0.0.log 2>&1 &
DATAFUSION_DIR=/home/alamb/arrow-datafusion4 ./build_datafusion_cli_old.sh 43.0.0 > 43.0.0.log 2>&1 &
```

Here are some possible useful commands to run manually.

Find one git commit per day

1. Dump git log to a csv file:
```shell
cd datafusion
echo "revision,time,url" > ../commits.csv
git log --pretty=format:"%h,%ci,https://github.com/apache/datafusion/commit/%h" >> ../commits.csv
cd ..
```
Now use sql to find the first commit of each day:
```
SELECT revision, day, time
FROM (
  SELECT revision, day, time, first_value(revision) OVER (PARTITION BY day ORDER BY time DESC) as first_rev, url
  FROM (select *, date_bin('1 day', time) as day from 'commits.csv')
)
WHERE first_rev = revision
ORDER by time DESC;"
```

Here is how to use datafusion-cli to generate the commands to build datafusion-cli for each commit in the last day:

```shell
datafusion-cli --format csv -c "SELECT './build_datafusion_cli.sh ' || revision FROM (select revision, day, time, first_value(revision) OVER (PARTITION BY day ORDER BY time DESC) as first_rev, url FROM (select *, date_bin('1 day', time) as day from 'commits.csv')) WHERE first_rev = revision ORDER by time DESC;"
```

Here is how to use datafusion-cli to generate the commands to build datafusion-cli for all commits

```shell
datafusion-cli --format=csv -c  "SELECT 'DATAFUSION_DIR=/home/alamb/arrow-datafusion ./build_datafusion_cli.sh ' || revision from 'commits.csv' order by time desc"
```





# Usage: Gather Data

Run the ClickBench queries with datafusion and output the results to a CSV file
```shell
./run_clickbench.py --output-dir /path/to/output/dir
```
