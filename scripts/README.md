# Scripts for benchmarksing datafusion and arrow-rs PRs
This directory contains[@alamb](https://github.com/alamb)'s scripts for benchmarking 
- https://github.com/apache/datafusion 
- https://github.com/apache/arrow-rs

# Cookbook: Requesting Benchmarks

Note these scripts only run benchmarks for whitelisted users and whitelisted jobs.

To request a benchmark run on a PR, add a comment to the PR with the following format:

```
run benchmark <benchmark_name>
```

To run the "standard" benchmarks run 
```
run benchmarks
```

The scraper script will post a 🚀 reaction to the comment to indicate it has been seen.



# High level flow:
1. "jobs" (aka scripts) are written to jobs/*.files
2. `poll.sh` looks for jobs and runs them
3. `scrape_comment.py` scrapes comments from PRs to find benchmarks to run (creates new jobs)

# Job scripts

While the job scripts can do anything, they mostly run benchmarks and post results to PRs using
one of the following scripts:

- compare_branch.sh: Run `bench.sh` based benchmark on a PR and main and post results to the PR.
- compare_branch_bench.sh: Run `cargo bench` based benchmark on a PR and main and post results to the PR
- scrape_comment.py: Scrape benchmark results from a PR comment and save to CSV

## Runner Cookbook

Scrape comments every second (in one terminal):
```shell
while true ; do python3 scrape_comments.py ; sleep 1; done
```

Run jobs (in a second terminal):
```shell
bash poll.sh
```

# TODO:
- [ ] Add more benchmark types (e.g. criterion based benchmarks)
- [ ] Add support for arrow-rs benchmarks
