//! Benchmark trigger detection and per-repo allowlists.
//!
//! Parses PR comment bodies for "run benchmark …" trigger phrases, validates
//! benchmark names against repo-specific allowlists, and classifies them by
//! [`JobType`].

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::RepoEntry;
use crate::models::{BenchmarkRequest, JobType};

static ENV_VAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Z_][A-Z0-9_]*=[a-zA-Z0-9._\-]+$").unwrap());

/// Unified trigger regex: matches `run benchmark(s) [name1 name2 ...]`.
static TRIGGER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*run\s+(benchmarks?)(?:\s+([a-zA-Z0-9_\-\s]+?))?\s*$").unwrap()
});

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

/// Parse a `KEY=VALUE` line into a (key, value) tuple.
fn parse_env_var(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if ENV_VAR_RE.is_match(trimmed) {
        let (k, v) = trimmed.split_once('=')?;
        Some((k.to_string(), v.to_string()))
    } else {
        None
    }
}

/// Parser state machine for section-based comment syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    TopLevel,
    TopLevelEnv,
    Baseline,
    BaselineEnv,
    Changed,
    ChangedEnv,
}

/// Parse the extra lines (after the trigger line) into structured env vars and refs.
fn parse_sections(
    lines: &[&str],
) -> (
    HashMap<String, String>,
    HashMap<String, String>,
    HashMap<String, String>,
    Option<String>,
    Option<String>,
) {
    let mut shared_env = HashMap::new();
    let mut baseline_env = HashMap::new();
    let mut changed_env = HashMap::new();
    let mut baseline_ref = None;
    let mut changed_ref = None;
    let mut section = Section::TopLevel;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let lower = trimmed.to_lowercase();

        // Section headers
        if lower == "baseline:" {
            section = Section::Baseline;
            continue;
        }
        if lower == "changed:" {
            section = Section::Changed;
            continue;
        }
        if lower == "env:" {
            section = match section {
                Section::TopLevel | Section::TopLevelEnv => Section::TopLevelEnv,
                Section::Baseline | Section::BaselineEnv => Section::BaselineEnv,
                Section::Changed | Section::ChangedEnv => Section::ChangedEnv,
            };
            continue;
        }

        // ref: <value> inside baseline/changed sections
        if let Some(ref_val) = trimmed
            .strip_prefix("ref:")
            .or_else(|| trimmed.strip_prefix("ref :"))
        {
            let ref_val = ref_val.trim();
            if !ref_val.is_empty() {
                match section {
                    Section::Baseline | Section::BaselineEnv => {
                        baseline_ref = Some(ref_val.to_string());
                    }
                    Section::Changed | Section::ChangedEnv => {
                        changed_ref = Some(ref_val.to_string());
                    }
                    _ => {} // ref at top level is ignored
                }
            }
            continue;
        }

        // ENV_VAR=value lines
        if let Some((k, v)) = parse_env_var(trimmed) {
            match section {
                Section::TopLevel | Section::TopLevelEnv => {
                    shared_env.insert(k, v);
                }
                Section::BaselineEnv => {
                    baseline_env.insert(k, v);
                }
                Section::ChangedEnv => {
                    changed_env.insert(k, v);
                }
                // Bare KEY=VALUE inside baseline:/changed: without env: header — treat as per-side
                Section::Baseline => {
                    baseline_env.insert(k, v);
                }
                Section::Changed => {
                    changed_env.insert(k, v);
                }
            }
        }
    }

    (
        shared_env,
        baseline_env,
        changed_env,
        baseline_ref,
        changed_ref,
    )
}

/// Result of parsing the trigger line.
pub enum TriggerKind {
    /// Specific benchmark names were given.
    Named(Vec<String>),
    /// `run benchmarks` (plural) with no names → default suite.
    DefaultSuite,
    /// `run benchmark` (singular) with no names → error.
    SingularNoNames,
}

/// Parse the trigger line. Returns `None` if it doesn't match the trigger pattern at all.
pub fn parse_trigger(trigger: &str) -> Option<TriggerKind> {
    let caps = TRIGGER_RE.captures(trigger)?;
    let word = caps.get(1).unwrap().as_str(); // "benchmark" or "benchmarks"
    let is_plural = word.to_lowercase().ends_with('s');

    match caps.get(2) {
        Some(names_match) => {
            let names: Vec<String> = names_match
                .as_str()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if names.is_empty() {
                if is_plural {
                    Some(TriggerKind::DefaultSuite)
                } else {
                    Some(TriggerKind::SingularNoNames)
                }
            } else {
                Some(TriggerKind::Named(names))
            }
        }
        None => {
            if is_plural {
                Some(TriggerKind::DefaultSuite)
            } else {
                Some(TriggerKind::SingularNoNames)
            }
        }
    }
}

/// Parse a PR comment body into a [`BenchmarkRequest`] if it matches a trigger pattern.
///
/// Recognizes `run benchmarks` (default suite), `run benchmarks <names>`, and
/// `run benchmark <names>`. `run benchmark` without names returns `None` (caller
/// should post a help message).
///
/// Supports `baseline:`/`changed:` sections with `env:` and `ref:` sub-entries.
pub fn detect_benchmark(repo_entry: &RepoEntry, body: &str) -> Option<BenchmarkRequest> {
    let lines: Vec<&str> = body.trim().lines().collect();
    if lines.is_empty() {
        return None;
    }

    let trigger = lines[0];
    let extra = &lines[1..];

    let trigger_kind = parse_trigger(trigger)?;

    let (shared_env, baseline_env, changed_env, baseline_ref, changed_ref) = parse_sections(extra);

    match trigger_kind {
        TriggerKind::DefaultSuite => Some(BenchmarkRequest {
            benchmarks: vec![],
            env_vars: shared_env,
            baseline_env_vars: baseline_env,
            changed_env_vars: changed_env,
            baseline_ref,
            changed_ref,
        }),
        TriggerKind::Named(names) => {
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
                    env_vars: shared_env,
                    baseline_env_vars: baseline_env,
                    changed_env_vars: changed_env,
                    baseline_ref,
                    changed_ref,
                })
            } else {
                None
            }
        }
        TriggerKind::SingularNoNames => None,
    }
}

/// Returns `true` if the first line starts with "run benchmark" (case-insensitive).
/// Used to detect malformed or unauthorized trigger attempts.
pub fn is_benchmark_trigger(body: &str) -> bool {
    let first_line = body.trim().lines().next().unwrap_or("");
    let lower = first_line.trim().to_lowercase();
    lower.starts_with("run benchmark")
}

/// Returns `true` if `run benchmark` (singular) was used without benchmark names.
pub fn is_singular_no_names(body: &str) -> bool {
    let first_line = body.trim().lines().next().unwrap_or("");
    matches!(
        parse_trigger(first_line),
        Some(TriggerKind::SingularNoNames)
    )
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
         Usage:\n\
         ```\n\
         run benchmark <name>           # run specific benchmark(s)\n\
         run benchmarks                 # run default suite\n\
         run benchmarks <name1> <name2> # run specific benchmarks\n\
         ```\n\n\
         Per-side configuration:\n\
         ```\n\
         run benchmark tpch\n\
         env:\n\
           SHARED_SETTING=enabled\n\
         baseline:\n\
           ref: v45.0.0\n\
           env:\n\
             DATAFUSION_RUNTIME_MEMORY_LIMIT=1G\n\
         changed:\n\
           ref: v46.0.0\n\
           env:\n\
             DATAFUSION_RUNTIME_MEMORY_LIMIT=2G\n\
         ```{unsupported}"
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
        assert_eq!(
            req.env_vars.get("DATAFUSION_RUNTIME_MEMORY_LIMIT").unwrap(),
            "1G"
        );
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

    // ── plural trigger with names (new) ─────────────────────────────

    #[test]
    fn detect_plural_with_names() {
        let req = detect_benchmark(&df_entry(), "run benchmarks tpch clickbench_1").unwrap();
        assert_eq!(req.benchmarks, vec!["tpch", "clickbench_1"]);
    }

    // ── singular without names returns None ─────────────────────────

    #[test]
    fn detect_singular_no_names_returns_none() {
        assert!(detect_benchmark(&df_entry(), "run benchmark").is_none());
    }

    #[test]
    fn is_singular_no_names_detects() {
        assert!(is_singular_no_names("run benchmark"));
        assert!(is_singular_no_names("  run benchmark  "));
        assert!(!is_singular_no_names("run benchmarks"));
        assert!(!is_singular_no_names("run benchmark tpch"));
    }

    // ── section parsing ─────────────────────────────────────────────

    #[test]
    fn parse_baseline_changed_env_vars() {
        let body = "run benchmark tpch\nbaseline:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT=1G\nchanged:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT=2G";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(
            req.baseline_env_vars
                .get("DATAFUSION_RUNTIME_MEMORY_LIMIT")
                .unwrap(),
            "1G"
        );
        assert_eq!(
            req.changed_env_vars
                .get("DATAFUSION_RUNTIME_MEMORY_LIMIT")
                .unwrap(),
            "2G"
        );
        assert!(req.env_vars.is_empty());
    }

    #[test]
    fn parse_baseline_ref() {
        let body = "run benchmarks tpch clickbench_1\nbaseline:\n  ref: abc1234def";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(req.baseline_ref.as_deref(), Some("abc1234def"));
        assert!(req.changed_ref.is_none());
    }

    #[test]
    fn parse_both_refs_with_env() {
        let body = "run benchmark tpch\nbaseline:\n  ref: v45.0.0\n  env:\n    FOO=old_value\nchanged:\n  ref: v46.0.0\n  env:\n    FOO=new_value";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(req.baseline_ref.as_deref(), Some("v45.0.0"));
        assert_eq!(req.changed_ref.as_deref(), Some("v46.0.0"));
        assert_eq!(req.baseline_env_vars.get("FOO").unwrap(), "old_value");
        assert_eq!(req.changed_env_vars.get("FOO").unwrap(), "new_value");
    }

    #[test]
    fn parse_shared_plus_per_side() {
        let body = "run benchmark tpch\nSHARED_SETTING=enabled\nbaseline:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT=1G\nchanged:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT=2G";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(req.env_vars.get("SHARED_SETTING").unwrap(), "enabled");
        assert_eq!(
            req.baseline_env_vars
                .get("DATAFUSION_RUNTIME_MEMORY_LIMIT")
                .unwrap(),
            "1G"
        );
        assert_eq!(
            req.changed_env_vars
                .get("DATAFUSION_RUNTIME_MEMORY_LIMIT")
                .unwrap(),
            "2G"
        );
    }

    #[test]
    fn parse_explicit_env_section() {
        let body = "run benchmark tpch\nenv:\n  DATAFUSION_RUNTIME_MEMORY_LIMIT=1G";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(
            req.env_vars.get("DATAFUSION_RUNTIME_MEMORY_LIMIT").unwrap(),
            "1G"
        );
    }

    #[test]
    fn parse_backward_compat_bare_env() {
        let body = "run benchmark tpch\nDATAFUSION_RUNTIME_MEMORY_LIMIT=1G";
        let req = detect_benchmark(&df_entry(), body).unwrap();
        assert_eq!(
            req.env_vars.get("DATAFUSION_RUNTIME_MEMORY_LIMIT").unwrap(),
            "1G"
        );
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

    // ── parse_env_var ──────────────────────────────────────────────

    #[test]
    fn parse_env_simple() {
        let (k, v) = parse_env_var("FOO=bar").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "bar");
    }

    #[test]
    fn parse_env_dots_hyphens() {
        let (k, v) = parse_env_var("FOO=bar.baz-1").unwrap();
        assert_eq!(k, "FOO");
        assert_eq!(v, "bar.baz-1");
    }

    #[test]
    fn parse_env_lowercase_key_rejected() {
        assert!(parse_env_var("foo=bar").is_none());
    }

    #[test]
    fn parse_env_space_in_value_rejected() {
        assert!(parse_env_var("FOO=bar baz").is_none());
    }
}
