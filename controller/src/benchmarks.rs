//! Benchmark trigger detection and per-repo allowlists.
//!
//! Parses PR comment bodies for "run benchmark …" trigger phrases, validates
//! benchmark names against repo-specific allowlists, and classifies them by
//! [`JobType`].

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

use crate::config::RepoEntry;
use crate::models::{BenchmarkRequest, JobType};

/// Unified trigger regex: matches `run benchmark(s) [name1 name2 ...]`.
static TRIGGER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*run\s+(benchmarks?)(?:\s+([a-zA-Z0-9_\-\s]+?))?\s*$").unwrap()
});

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CommentConfig {
    env: Option<HashMap<String, String>>,
    baseline: Option<SideConfig>,
    changed: Option<SideConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SideConfig {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    env: Option<HashMap<String, String>>,
}

/// Result of [`detect_benchmark`].
pub enum DetectResult {
    /// Successfully parsed trigger and config.
    Parsed(BenchmarkRequest),
    /// Trigger matched but YAML config had errors.
    ConfigError(String),
    /// Not a trigger at all.
    None,
}

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

/// Parse the extra lines (after the trigger line) into structured env vars and refs.
///
/// Supports an optional ` ```yaml ` / ` ``` ` fence around the YAML content.
/// Returns `Err` with a human-readable message if YAML is present but invalid.
fn parse_sections(
    lines: &[&str],
) -> Result<
    (
        HashMap<String, String>,
        HashMap<String, String>,
        HashMap<String, String>,
        Option<String>,
        Option<String>,
    ),
    String,
> {
    let yaml: String = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("```")
        })
        .copied()
        .collect::<Vec<&str>>()
        .join("\n");

    if yaml.trim().is_empty() {
        return Ok(Default::default());
    }

    let config: CommentConfig =
        serde_yaml::from_str(&yaml).map_err(|e| format!("invalid configuration: {e}"))?;

    let shared_env = config.env.unwrap_or_default();
    let baseline_env = config
        .baseline
        .as_ref()
        .and_then(|s| s.env.clone())
        .unwrap_or_default();
    let changed_env = config
        .changed
        .as_ref()
        .and_then(|s| s.env.clone())
        .unwrap_or_default();
    let baseline_ref = config.baseline.as_ref().and_then(|s| s.git_ref.clone());
    let changed_ref = config.changed.as_ref().and_then(|s| s.git_ref.clone());

    Ok((
        shared_env,
        baseline_env,
        changed_env,
        baseline_ref,
        changed_ref,
    ))
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
pub fn detect_benchmark(repo_entry: &RepoEntry, body: &str) -> DetectResult {
    let lines: Vec<&str> = body.trim().lines().collect();
    if lines.is_empty() {
        return DetectResult::None;
    }

    let trigger = lines[0];
    let extra = &lines[1..];

    let trigger_kind = match parse_trigger(trigger) {
        Some(k) => k,
        None => return DetectResult::None,
    };

    let (shared_env, baseline_env, changed_env, baseline_ref, changed_ref) =
        match parse_sections(extra) {
            Ok(sections) => sections,
            Err(e) => return DetectResult::ConfigError(e),
        };

    match trigger_kind {
        TriggerKind::DefaultSuite => DetectResult::Parsed(BenchmarkRequest {
            benchmarks: vec![],
            env_vars: shared_env,
            baseline_env_vars: baseline_env,
            changed_env_vars: changed_env,
            baseline_ref,
            changed_ref,
        }),
        TriggerKind::Named(names) => {
            if names.is_empty() {
                return DetectResult::None;
            }

            let standard = repo_entry.standard_set();
            let criterion = repo_entry.criterion_set();

            let all_valid = names
                .iter()
                .all(|n| standard.contains(n.as_str()) || criterion.contains(n.as_str()));

            if all_valid {
                DetectResult::Parsed(BenchmarkRequest {
                    benchmarks: names,
                    env_vars: shared_env,
                    baseline_env_vars: baseline_env,
                    changed_env_vars: changed_env,
                    baseline_ref,
                    changed_ref,
                })
            } else {
                DetectResult::None
            }
        }
        TriggerKind::SingularNoNames => DetectResult::None,
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
         Per-side configuration (`run benchmark tpch` followed by):\n\
         ```yaml\n\
         env:\n\
           SHARED_SETTING: enabled\n\
         baseline:\n\
           ref: v45.0.0\n\
           env:\n\
             DATAFUSION_RUNTIME_MEMORY_LIMIT: 1G\n\
         changed:\n\
           ref: v46.0.0\n\
           env:\n\
             DATAFUSION_RUNTIME_MEMORY_LIMIT: 2G\n\
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

    /// Helper to unwrap a DetectResult::Parsed or panic.
    fn unwrap_parsed(result: DetectResult) -> BenchmarkRequest {
        match result {
            DetectResult::Parsed(req) => req,
            DetectResult::ConfigError(e) => panic!("expected Parsed, got ConfigError: {e}"),
            DetectResult::None => panic!("expected Parsed, got None"),
        }
    }

    fn is_none(result: &DetectResult) -> bool {
        matches!(result, DetectResult::None)
    }

    fn is_parsed(result: &DetectResult) -> bool {
        matches!(result, DetectResult::Parsed(_))
    }

    #[test]
    fn detect_default_suite() {
        let req = unwrap_parsed(detect_benchmark(&df_entry(), "run benchmarks"));
        assert!(req.benchmarks.is_empty());
        assert!(req.env_vars.is_empty());
    }

    #[test]
    fn detect_default_suite_with_env_vars() {
        let body = "run benchmarks\nenv:\n  DATAFUSION_RUNTIME_MEMORY_LIMIT: 1G";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
        assert!(req.benchmarks.is_empty());
        assert_eq!(
            req.env_vars.get("DATAFUSION_RUNTIME_MEMORY_LIMIT").unwrap(),
            "1G"
        );
    }

    #[test]
    fn detect_single_named() {
        let req = unwrap_parsed(detect_benchmark(&df_entry(), "run benchmark tpch_mem"));
        assert_eq!(req.benchmarks, vec!["tpch_mem"]);
    }

    #[test]
    fn detect_multiple_named() {
        let req = unwrap_parsed(detect_benchmark(
            &df_entry(),
            "run benchmark tpch_mem tpch10",
        ));
        assert_eq!(req.benchmarks, vec!["tpch_mem", "tpch10"]);
    }

    #[test]
    fn detect_criterion_benchmark() {
        let req = unwrap_parsed(detect_benchmark(&df_entry(), "run benchmark sql_planner"));
        assert_eq!(req.benchmarks, vec!["sql_planner"]);
    }

    #[test]
    fn detect_bogus_name_returns_none() {
        assert!(is_none(&detect_benchmark(
            &df_entry(),
            "run benchmark bogus_name"
        )));
    }

    #[test]
    fn detect_one_invalid_rejects_all() {
        assert!(is_none(&detect_benchmark(
            &df_entry(),
            "run benchmark tpch_mem bogus"
        )));
    }

    #[test]
    fn detect_not_a_trigger() {
        assert!(is_none(&detect_benchmark(&df_entry(), "hello world")));
    }

    #[test]
    fn detect_empty_string() {
        assert!(is_none(&detect_benchmark(&df_entry(), "")));
    }

    #[test]
    fn detect_case_insensitive() {
        assert!(is_parsed(&detect_benchmark(&df_entry(), "Run Benchmarks")));
        assert!(is_parsed(&detect_benchmark(
            &df_entry(),
            "RUN BENCHMARK tpch"
        )));
    }

    #[test]
    fn detect_arrow_criterion() {
        let req = unwrap_parsed(detect_benchmark(
            &arrow_entry(),
            "run benchmark arrow_reader",
        ));
        assert_eq!(req.benchmarks, vec!["arrow_reader"]);
    }

    // ── plural trigger with names (new) ─────────────────────────────

    #[test]
    fn detect_plural_with_names() {
        let req = unwrap_parsed(detect_benchmark(
            &df_entry(),
            "run benchmarks tpch clickbench_1",
        ));
        assert_eq!(req.benchmarks, vec!["tpch", "clickbench_1"]);
    }

    // ── singular without names returns None ─────────────────────────

    #[test]
    fn detect_singular_no_names_returns_none() {
        assert!(is_none(&detect_benchmark(&df_entry(), "run benchmark")));
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
        let body = "run benchmark tpch\nbaseline:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT: 1G\nchanged:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT: 2G";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
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
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
        assert_eq!(req.baseline_ref.as_deref(), Some("abc1234def"));
        assert!(req.changed_ref.is_none());
    }

    #[test]
    fn parse_both_refs_with_env() {
        let body = "run benchmark tpch\nbaseline:\n  ref: v45.0.0\n  env:\n    FOO: old_value\nchanged:\n  ref: v46.0.0\n  env:\n    FOO: new_value";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
        assert_eq!(req.baseline_ref.as_deref(), Some("v45.0.0"));
        assert_eq!(req.changed_ref.as_deref(), Some("v46.0.0"));
        assert_eq!(req.baseline_env_vars.get("FOO").unwrap(), "old_value");
        assert_eq!(req.changed_env_vars.get("FOO").unwrap(), "new_value");
    }

    #[test]
    fn parse_shared_plus_per_side() {
        let body = "run benchmark tpch\nenv:\n  SHARED_SETTING: enabled\nbaseline:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT: 1G\nchanged:\n  env:\n    DATAFUSION_RUNTIME_MEMORY_LIMIT: 2G";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
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
        let body = "run benchmark tpch\nenv:\n  DATAFUSION_RUNTIME_MEMORY_LIMIT: 1G";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
        assert_eq!(
            req.env_vars.get("DATAFUSION_RUNTIME_MEMORY_LIMIT").unwrap(),
            "1G"
        );
    }

    #[test]
    fn parse_yaml_fenced_block() {
        let body = "run benchmark tpch\n```yaml\nbaseline:\n  ref: v45.0.0\n  env:\n    FOO: bar\nchanged:\n  ref: v46.0.0\n```";
        let req = unwrap_parsed(detect_benchmark(&df_entry(), body));
        assert_eq!(req.baseline_ref.as_deref(), Some("v45.0.0"));
        assert_eq!(req.changed_ref.as_deref(), Some("v46.0.0"));
        assert_eq!(req.baseline_env_vars.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn parse_unknown_field_returns_config_error() {
        let body = "run benchmark tpch\ncurrent:\n  ref: HEAD";
        match detect_benchmark(&df_entry(), body) {
            DetectResult::ConfigError(e) => {
                assert!(e.contains("unknown field"), "error was: {e}");
            }
            other => panic!(
                "expected ConfigError, got {}",
                match other {
                    DetectResult::Parsed(_) => "Parsed",
                    DetectResult::None => "None",
                    DetectResult::ConfigError(_) => unreachable!(),
                }
            ),
        }
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
}
