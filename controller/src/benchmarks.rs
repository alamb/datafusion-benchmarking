//! Benchmark trigger detection and per-repo allowlists.
//!
//! Parses PR comment bodies for "run benchmark …" trigger phrases, validates
//! benchmark names against repo-specific allowlists, and classifies them by
//! [`JobType`].

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::RepoEntry;
use crate::models::{BenchmarkRequest, JobType};

static ENV_VAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Z_][A-Z0-9_]*=[a-zA-Z0-9._\-]+$").unwrap());

static TRIGGER_DEFAULT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*run\s+benchmarks\s*$").unwrap());

static TRIGGER_NAMED_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*run\s+benchmark\s+([a-zA-Z0-9_\-\s]+?)\s*$").unwrap());

impl RepoEntry {
    /// Determine the [`JobType`] for a benchmark name, or `None` if not recognized.
    pub fn classify_benchmark(&self, name: &str) -> Option<JobType> {
        if self.standard_set().contains(name) {
            Some(JobType::Standard)
        } else if self.criterion_set().contains(name) {
            if self.criterion_type == "arrow" {
                Some(JobType::ArrowCriterion)
            } else {
                Some(JobType::Criterion)
            }
        } else {
            None
        }
    }
}

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
pub fn detect_benchmark(repo_entry: &RepoEntry, body: &str) -> Option<BenchmarkRequest> {
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

    let standard = repo_entry.standard_set();
    let criterion = repo_entry.criterion_set();

    let all_valid = names
        .iter()
        .all(|n| standard.contains(n.as_str()) || criterion.contains(n.as_str()));

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
pub fn supported_benchmarks_message(repo_entry: &RepoEntry, requested: &[String]) -> String {
    let standard: Vec<&str> = {
        let mut v: Vec<&str> = repo_entry.standard.iter().map(|s| s.as_str()).collect();
        v.sort();
        v
    };
    let criterion: Vec<&str> = {
        let mut v: Vec<&str> = repo_entry.criterion.iter().map(|s| s.as_str()).collect();
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

    let standard_set = repo_entry.standard_set();
    let criterion_set = repo_entry.criterion_set();

    let bad: Vec<&String> = requested
        .iter()
        .filter(|n| !standard_set.contains(n.as_str()) && !criterion_set.contains(n.as_str()))
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
pub fn allowed_users_markdown(allowed_users: &HashSet<String>) -> String {
    let mut users: Vec<&str> = allowed_users.iter().map(|s| s.as_str()).collect();
    users.sort();
    users
        .iter()
        .map(|u| format!("[{u}](https://github.com/{u})"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df_entry() -> RepoEntry {
        RepoEntry {
            standard: vec![
                "tpch".into(),
                "tpch10".into(),
                "tpch_mem".into(),
                "tpch_mem10".into(),
                "clickbench_partitioned".into(),
                "clickbench_extended".into(),
                "clickbench_1".into(),
                "clickbench_pushdown".into(),
                "external_aggr".into(),
                "tpcds".into(),
            ],
            criterion: vec!["sql_planner".into(), "in_list".into(), "case_when".into()],
            criterion_type: "datafusion".into(),
            default_standard: vec![
                "clickbench_partitioned".into(),
                "tpcds".into(),
                "tpch".into(),
            ],
        }
    }

    fn arrow_entry() -> RepoEntry {
        RepoEntry {
            standard: vec![],
            criterion: vec!["arrow_reader".into(), "arrow_writer".into()],
            criterion_type: "arrow".into(),
            default_standard: vec![],
        }
    }

    // ── detect_benchmark ────────────────────────────────────────────

    #[test]
    fn detect_default_suite() {
        let req = detect_benchmark(&df_entry(), "run benchmarks").unwrap();
        assert!(req.benchmarks.is_empty());
        assert!(req.env_vars.is_empty());
    }

    #[test]
    fn detect_default_suite_with_env_vars() {
        let body = "run benchmarks\nDATAFUSION_RUNTIME_MEMORY_LIMIT=1G";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert!(req.benchmarks.is_empty());
        assert_eq!(req.env_vars, vec!["DATAFUSION_RUNTIME_MEMORY_LIMIT=1G"]);
    }

    #[test]
    fn detect_single_named() {
        let req = detect_benchmark(&df_entry(), "run benchmark tpch_mem").unwrap();
        assert_eq!(req.benchmarks, vec!["tpch_mem"]);
    }

    #[test]
    fn detect_multiple_named() {
        let req = detect_benchmark(&df_entry(), "run benchmark tpch_mem tpch10").unwrap();
        assert_eq!(req.benchmarks, vec!["tpch_mem", "tpch10"]);
    }

    #[test]
    fn detect_criterion_benchmark() {
        let req = detect_benchmark(&df_entry(), "run benchmark sql_planner").unwrap();
        assert_eq!(req.benchmarks, vec!["sql_planner"]);
    }

    #[test]
    fn detect_bogus_name_returns_none() {
        assert!(detect_benchmark(&df_entry(), "run benchmark bogus_name").is_none());
    }

    #[test]
    fn detect_one_invalid_rejects_all() {
        assert!(detect_benchmark(&df_entry(), "run benchmark tpch_mem bogus").is_none());
    }

    #[test]
    fn detect_not_a_trigger() {
        assert!(detect_benchmark(&df_entry(), "hello world").is_none());
    }

    #[test]
    fn detect_empty_string() {
        assert!(detect_benchmark(&df_entry(), "").is_none());
    }

    #[test]
    fn detect_case_insensitive() {
        assert!(detect_benchmark(&df_entry(), "Run Benchmarks").is_some());
        assert!(detect_benchmark(&df_entry(), "RUN BENCHMARK tpch").is_some());
    }

    #[test]
    fn detect_arrow_criterion() {
        let req = detect_benchmark(&arrow_entry(), "run benchmark arrow_reader").unwrap();
        assert_eq!(req.benchmarks, vec!["arrow_reader"]);
    }

    // ── is_benchmark_trigger ────────────────────────────────────────

    #[test]
    fn trigger_named() {
        assert!(is_benchmark_trigger("run benchmark tpch"));
    }

    #[test]
    fn trigger_default() {
        assert!(is_benchmark_trigger("run benchmarks"));
    }

    #[test]
    fn trigger_case_insensitive() {
        assert!(is_benchmark_trigger("Run Benchmark FOO"));
    }

    #[test]
    fn trigger_not_matching() {
        assert!(!is_benchmark_trigger("hello"));
    }

    #[test]
    fn trigger_leading_whitespace() {
        assert!(is_benchmark_trigger("  run benchmark x  "));
    }

    // ── is_queue_request ────────────────────────────────────────────

    #[test]
    fn queue_request_exact() {
        assert!(is_queue_request("show benchmark queue"));
    }

    #[test]
    fn queue_request_case_insensitive() {
        assert!(is_queue_request("SHOW BENCHMARK QUEUE"));
    }

    #[test]
    fn queue_request_extra_words() {
        assert!(!is_queue_request("show benchmark queue please"));
    }

    #[test]
    fn queue_request_wrong_phrase() {
        assert!(!is_queue_request("run benchmarks"));
    }

    // ── RepoEntry::classify_benchmark ──────────────────────────────

    #[test]
    fn classify_df_standard() {
        assert_eq!(
            df_entry().classify_benchmark("tpch"),
            Some(JobType::Standard)
        );
    }

    #[test]
    fn classify_df_criterion() {
        assert_eq!(
            df_entry().classify_benchmark("sql_planner"),
            Some(JobType::Criterion)
        );
    }

    #[test]
    fn classify_df_bogus() {
        assert_eq!(df_entry().classify_benchmark("bogus"), None);
    }

    #[test]
    fn classify_arrow_criterion() {
        assert_eq!(
            arrow_entry().classify_benchmark("arrow_reader"),
            Some(JobType::ArrowCriterion)
        );
    }

    // ── supported_benchmarks_message ────────────────────────────────

    #[test]
    fn supported_msg_no_unsupported() {
        let msg = supported_benchmarks_message(&df_entry(), &[]);
        assert!(!msg.contains("Unsupported"));
    }

    #[test]
    fn supported_msg_with_unsupported() {
        let msg = supported_benchmarks_message(&df_entry(), &["bogus".to_string()]);
        assert!(msg.contains("Unsupported benchmarks: bogus"));
    }

    // ── allowed_users_markdown ──────────────────────────────────────

    #[test]
    fn allowed_users_contains_known_user() {
        let users: HashSet<String> = ["alamb", "zhuqi-lucas"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let md = allowed_users_markdown(&users);
        assert!(md.contains("[alamb](https://github.com/alamb)"));
        // Verify sorted (a before z)
        let pos_a = md.find("[alamb]").unwrap();
        let pos_z = md.find("[zhuqi-lucas]").unwrap();
        assert!(pos_a < pos_z);
    }

    // ── parse_env_vars ──────────────────────────────────────────────

    #[test]
    fn parse_env_simple() {
        assert_eq!(parse_env_vars(&["FOO=bar"]), vec!["FOO=bar"]);
    }

    #[test]
    fn parse_env_dots_hyphens() {
        assert_eq!(parse_env_vars(&["FOO=bar.baz-1"]), vec!["FOO=bar.baz-1"]);
    }

    #[test]
    fn parse_env_lowercase_key_rejected() {
        assert!(parse_env_vars(&["foo=bar"]).is_empty());
    }

    #[test]
    fn parse_env_space_in_value_rejected() {
        assert!(parse_env_vars(&["FOO=bar baz"]).is_empty());
    }

    #[test]
    fn parse_env_filters_blanks() {
        let result = parse_env_vars(&["", "  ", "FOO=bar"]);
        assert_eq!(result, vec!["FOO=bar"]);
    }
}
