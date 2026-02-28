//! Benchmark trigger detection and per-repo allowlists.
//!
//! Parses PR comment bodies for "run benchmark …" trigger phrases, validates
//! benchmark names against repo-specific allowlists, and classifies them by
//! [`JobType`].

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::models::{BenchmarkRequest, JobType};

pub static ALLOWED_USERS: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        "alamb",
        "Dandandan",
        "adriangb",
        "rluvaton",
        "geoffreyclaude",
        "xudong963",
        "zhuqi-lucas",
        "Omega359",
        "comphead",
        "klion26",
        "gabotechs",
        "Jefffrey",
        "etseidl",
    ]
    .into_iter()
    .collect()
});

pub static ALLOWED_BENCHMARKS_DF: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        "tpch",
        "tpch10",
        "tpch_mem",
        "tpch_mem10",
        "clickbench_partitioned",
        "clickbench_extended",
        "clickbench_1",
        "clickbench_pushdown",
        "external_aggr",
        "tpcds",
    ]
    .into_iter()
    .collect()
});

pub static ALLOWED_CRITERION_BENCHMARKS_DF: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        "sql_planner",
        "in_list",
        "case_when",
        "aggregate_vectorized",
        "aggregate_query_sql",
        "with_hashes",
        "range_and_generate_series",
        "sort",
        "left",
        "strpos",
        "substr_index",
        "character_length",
        "reset_plan_states",
        "replace",
        "plan_reuse",
    ]
    .into_iter()
    .collect()
});

pub static ALLOWED_CRITERION_BENCHMARKS_ARROW: Lazy<HashSet<&str>> = Lazy::new(|| {
    [
        "arrow_reader",
        "arrow_reader_clickbench",
        "arrow_reader_row_filter",
        "arrow_statistics",
        "arrow_writer",
        "array_iter",
        "array_from",
        "bitwise_kernel",
        "boolean_kernels",
        "buffer_bit_ops",
        "builder",
        "cast_kernels",
        "comparison_kernels",
        "csv_writer",
        "coalesce_kernels",
        "encoding",
        "metadata",
        "json-reader",
        "ipc_reader",
        "take_kernels",
        "sort_kernel",
        "interleave_kernels",
        "union_array",
        "variant_builder",
        "variant_kernels",
        "view_types",
        "variant_validation",
        "filter_kernels",
        "concatenate_kernel",
        "row_format",
        "zip_kernels",
    ]
    .into_iter()
    .collect()
});

static ENV_VAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Z_][A-Z0-9_]*=[a-zA-Z0-9._\-]+$").unwrap());

static TRIGGER_DEFAULT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*run\s+benchmarks\s*$").unwrap());

static TRIGGER_NAMED_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*run\s+benchmark\s+([a-zA-Z0-9_\-\s]+?)\s*$").unwrap());

/// Per-repo benchmark allowlists. Maps a GitHub repo to its valid standard and criterion benchmarks.
pub struct RepoConfig {
    pub repo: String,
    pub allowed_standard: &'static HashSet<&'static str>,
    pub allowed_criterion: &'static HashSet<&'static str>,
}

impl RepoConfig {
    /// Factory: returns the benchmark config for a known repo, or `None`.
    pub fn for_repo(repo: &str) -> Option<Self> {
        match repo {
            "apache/datafusion" => Some(Self {
                repo: repo.to_string(),
                allowed_standard: &ALLOWED_BENCHMARKS_DF,
                allowed_criterion: &ALLOWED_CRITERION_BENCHMARKS_DF,
            }),
            "apache/arrow-rs" => Some(Self {
                repo: repo.to_string(),
                allowed_standard: &EMPTY_SET,
                allowed_criterion: &ALLOWED_CRITERION_BENCHMARKS_ARROW,
            }),
            _ => None,
        }
    }

    /// Determine the [`JobType`] for a benchmark name, or `None` if not recognized.
    pub fn classify_benchmark(&self, name: &str) -> Option<JobType> {
        if self.allowed_standard.contains(name) {
            Some(JobType::Standard)
        } else if self.allowed_criterion.contains(name) {
            if self.repo == "apache/arrow-rs" {
                Some(JobType::ArrowCriterion)
            } else {
                Some(JobType::Criterion)
            }
        } else {
            None
        }
    }
}

// Empty set for repos with no standard benchmarks
static EMPTY_SET: Lazy<HashSet<&str>> = Lazy::new(HashSet::new);

/// Extract `KEY=value` lines that match `^[A-Z_][A-Z0-9_]*=[a-zA-Z0-9._-]+$`.
fn parse_env_vars(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && ENV_VAR_RE.is_match(l))
        .map(|l| l.to_string())
        .collect()
}

/// Parse a PR comment body into a [`BenchmarkRequest`] if it matches a trigger pattern.
///
/// Recognizes `run benchmarks` (default suite) and `run benchmark <name> [<name>...]`.
/// Extra lines after the trigger are scanned for environment variable overrides.
pub fn detect_benchmark(repo_cfg: &RepoConfig, body: &str) -> Option<BenchmarkRequest> {
    let lines: Vec<&str> = body.trim().lines().collect();
    if lines.is_empty() {
        return None;
    }

    let trigger = lines[0];
    let extra = &lines[1..];

    if TRIGGER_DEFAULT_RE.is_match(trigger) {
        return Some(BenchmarkRequest {
            benchmarks: vec![],
            env_vars: parse_env_vars(extra),
        });
    }

    let caps = TRIGGER_NAMED_RE.captures(trigger)?;
    let names: Vec<String> = caps[1].split_whitespace().map(|s| s.to_string()).collect();
    if names.is_empty() {
        return None;
    }

    let all_valid = names.iter().all(|n| {
        repo_cfg.allowed_standard.contains(n.as_str())
            || repo_cfg.allowed_criterion.contains(n.as_str())
    });

    if all_valid {
        Some(BenchmarkRequest {
            benchmarks: names,
            env_vars: parse_env_vars(extra),
        })
    } else {
        None
    }
}

/// Returns `true` if the first line starts with "run benchmark" (case-insensitive).
/// Used to detect malformed or unauthorized trigger attempts.
pub fn is_benchmark_trigger(body: &str) -> bool {
    let first_line = body.trim().lines().next().unwrap_or("");
    let lower = first_line.trim().to_lowercase();
    lower.starts_with("run benchmark")
}

/// Returns `true` if the comment body is exactly "show benchmark queue" (case-insensitive).
pub fn is_queue_request(body: &str) -> bool {
    body.trim().eq_ignore_ascii_case("show benchmark queue")
}

/// Build a markdown message listing all valid benchmarks for a repo, highlighting any unsupported names.
pub fn supported_benchmarks_message(repo_cfg: &RepoConfig, requested: &[String]) -> String {
    let standard: Vec<&str> = {
        let mut v: Vec<&str> = repo_cfg.allowed_standard.iter().copied().collect();
        v.sort();
        v
    };
    let criterion: Vec<&str> = {
        let mut v: Vec<&str> = repo_cfg.allowed_criterion.iter().copied().collect();
        v.sort();
        v
    };

    let standard_str = if standard.is_empty() {
        "(none)".to_string()
    } else {
        standard.join(", ")
    };
    let criterion_str = if criterion.is_empty() {
        "(none)".to_string()
    } else {
        criterion.join(", ")
    };

    let bad: Vec<&String> = requested
        .iter()
        .filter(|n| {
            !repo_cfg.allowed_standard.contains(n.as_str())
                && !repo_cfg.allowed_criterion.contains(n.as_str())
        })
        .collect();

    let unsupported = if bad.is_empty() {
        String::new()
    } else {
        format!(
            "\nUnsupported benchmarks: {}.",
            bad.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    format!(
        "Supported benchmarks:\n- Standard: {standard_str}\n- Criterion: {criterion_str}\n\n\
         Please choose with `run benchmark <name>` or `run benchmark <name1> <name2>...`\n\n\
         You can also set environment variables on subsequent lines:\n\
         ```\nrun benchmark tpch_mem\nDATAFUSION_RUNTIME_MEMORY_LIMIT=1G\n```{unsupported}"
    )
}

/// Format the allowlist as a comma-separated list of GitHub profile links.
pub fn allowed_users_markdown() -> String {
    let mut users: Vec<&&str> = ALLOWED_USERS.iter().collect();
    users.sort();
    users
        .iter()
        .map(|u| format!("[{u}](https://github.com/{u})"))
        .collect::<Vec<_>>()
        .join(", ")
}
