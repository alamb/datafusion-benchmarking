# DataFusion Benchmarking Scripts



Benchmark results: (TODO LINK / image)



## About:

This repo contains scripts used to run benchmarks against the DataFusion query
engine, specifically using the `datafusion-cli` binary. The benchmarks are
designed to measure the performance of various queries and operations in
DataFusion over time.

It is hugely inspired by Mike McCandless's https://benchmarks.mikemccandless.com/

> The goal is to spot any long-term regressions (or, gains!) in ~Lucene's~ DataFusion's performance that might otherwise accidentally slip past the committers, hopefully avoiding the fate of the [boiling frog](https://en.wikipedia.org/wiki/Boiling_frog)

## Notes

This is purposely not in the actual DataFusion repo so that it can be run
against any DataFusion version, including the latest master branch and
historical releases.

Because building DataFusion takes a long time, the scripts separate the build
and execution phases. This allows you to build DataFusion once and then run
benchmarks against it multiple times without needing to rebuild


## Directory Structure
The directory structure is as follows:


1. `data`: benchmark input data
2. `queries`: query scripts
3. `results`: output directory for benchmark results
4. `scripts`: scripts for manually running benchmarks

## TODOs:
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

### Step 2: Run Benchmarks

The  `./execute_benchmarks.py` script can be used to run benchmarks for `datafusion-cli` binaries in `builds`
Results are left in the `results` directory, with each benchmark's results stored in a separate CSV file.

### Step 3: Analyze Results

You can then analyze the results using the provided analysis script (TODO).

## Automation 

TODO: create a cron job or similar to automate the daily builds and tests.
Ideally it will automatically build datafusion-cli for all commits in the last day
and run the benchmarks, storing the results for later analysis.

builds.sh (remaining builds)

Then we'll generate the benchmark results

Then we'll do some charting and analaysis along with starting to automate the daily builds





## Cookbook:

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
datafusion-cli -c "SELECT revision, day, time FROM (select revision, day, time, first_value(revision) OVER (PARTITION BY day ORDER BY time DESC) as first_rev, url FROM (select *, date_bin('1 day', time) as day from 'commits.csv')) WHERE first_rev = revision ORDER by time DESC;"

Use datafusion-cli to make the commands
```shell
datafusion-cli --format csv -c "SELECT './build_datafusion_cli.sh ' || revision FROM (select revision, day, time, first_value(revision) OVER (PARTITION BY day ORDER BY time DESC) as first_rev, url FROM (select *, date_bin('1 day', time) as day from 'commits.csv')) WHERE first_rev = revision ORDER by time DESC;"
```


Notes:
You can see the all the commits like this:
```sql
 select 
   revision, day, time, 
    first_value(revision) OVER (PARTITION BY day ORDER BY time) as first_rev, url 
 FROM (select *, date_bin('1 day', time) as day from 'commits.csv') 
 ORDER by time DESC;
```

# Usage: Gather Data

Run the ClickBench queries with datafusion and output the results to a CSV file
```shell
./run_clickbench.py --output-dir /path/to/output/dir
```



