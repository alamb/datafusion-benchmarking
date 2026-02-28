# DataFusion Benchmarking

Automated benchmarking infrastructure for the [Apache DataFusion](https://github.com/apache/datafusion) query engine, running on GKE Autopilot.

## How It Works

1. A **controller pod** (Rust, StatefulSet) polls GitHub for PR comments containing benchmark triggers
2. When a trigger is detected, it creates a **K8s Job** on a Performance-class spot instance
3. The job builds DataFusion, runs benchmarks, and posts results back to the PR
4. A **scheduled CI workflow** benchmarks the latest `main` every 6 hours

## Triggering Benchmarks

Comment on a PR in `apache/datafusion` or `apache/arrow-rs`:

```
run benchmarks
```

Or run specific benchmarks:

```
run benchmark tpch_mem clickbench_partitioned
```

With environment variables:

```
run benchmark tpch_mem
DATAFUSION_RUNTIME_MEMORY_LIMIT=1G
```

View the queue:

```
show benchmark queue
```

## Project Structure

```
.github/workflows/     GitHub Actions (deploy, build, CI, scheduled benchmarks)
infra/                 Pulumi TypeScript (GKE Autopilot, Artifact Registry, IAM)
controller/            Rust crate (GitHub poller, K8s job manager, SQLite state)
runner/                Benchmark runner container (builds DF, runs benchmarks, posts results)
queries/               SQL query files for ClickBench
scripts/               Legacy benchmark scripts (reference)
```

## Infrastructure

- **GKE Autopilot** cluster with Performance-class spot instances for benchmark jobs
- **Hyperdisk-balanced** ephemeral volumes for fast compilation I/O
- **Workload Identity Federation** for keyless auth (GitHub Actions + controller pod)
- **Pulumi** for infrastructure-as-code, deployed via GitHub Actions

## Supported Benchmarks

### DataFusion (standard)
tpch, tpch10, tpch_mem, tpch_mem10, clickbench_partitioned, clickbench_extended, clickbench_1, clickbench_pushdown, external_aggr, tpcds

### DataFusion (criterion)
sql_planner, in_list, case_when, aggregate_vectorized, aggregate_query_sql, with_hashes, range_and_generate_series, sort, left, strpos, substr_index, character_length, reset_plan_states, replace, plan_reuse

### Arrow-rs (criterion)
arrow_reader, arrow_reader_clickbench, arrow_reader_row_filter, arrow_statistics, arrow_writer, array_iter, array_from, bitwise_kernel, boolean_kernels, buffer_bit_ops, builder, cast_kernels, comparison_kernels, csv_writer, coalesce_kernels, encoding, metadata, json-reader, ipc_reader, take_kernels, sort_kernel, interleave_kernels, union_array, variant_builder, variant_kernels, view_types, variant_validation, filter_kernels, concatenate_kernel, row_format, zip_kernels

## Development

```bash
# Controller
cd controller && cargo build

# Infra
cd infra && npm install && pulumi preview

# Runner (local test)
docker build -t runner runner/
```
