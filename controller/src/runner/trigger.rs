//! Rendering of the resolved trigger configuration for runner PR comments.
//!
//! Result comments link back to the triggering comment, but the link alone
//! doesn't say what the run actually did: one trigger comment fans out into one
//! job (and one pod) per benchmark name, and a bare `run benchmarks` expands to
//! the repo's default suite. So these helpers re-render *this* run's
//! configuration using the same YAML syntax the trigger accepts. That block is
//! a copy-pasteable reproduction of the run. A second block quotes the raw
//! trigger comment, so readers also see exactly what was asked for.

use std::collections::HashMap;

use crate::resources::PodResources;
use crate::runner::config::RunnerConfig;

/// The resolved two-sided comparison for a run.
pub struct Comparison<'a> {
    pub repo: &'a str,
    /// Ref name (or PR branch) benchmarked as the "changed" side.
    pub changed_display: &'a str,
    pub changed_sha: &'a str,
    /// Human label for the baseline — a ref name, or `<sha> (merge-base)`.
    pub baseline_label: &'a str,
    pub base_sha: &'a str,
}

impl Comparison<'_> {
    /// One-line summary with a GitHub compare link between the two sides.
    pub fn line(&self) -> String {
        format!(
            "Comparing {changed} ({changed_sha}) to {baseline} \
             [diff](https://github.com/{repo}/compare/{base_sha}..{changed_sha})",
            changed = self.changed_display,
            changed_sha = self.changed_sha,
            baseline = self.baseline_label,
            repo = self.repo,
            base_sha = self.base_sha,
        )
    }
}

/// A collapsed `<details>` block reproducing this run's trigger configuration.
///
/// `bench_names` is the space-separated benchmark list for the run (the arrow
/// runner passes its single `BENCH_NAME`); empty renders as `run benchmarks`,
/// which is how the default suite is requested.
pub fn config_block(config: &RunnerConfig, bench_names: &str) -> String {
    format!(
        "{}<details><summary>Run configuration</summary>\n\n\
         ```yaml\n\
         {}\n\
         ```\n\n\
         </details>\n\n{}",
        if config.shard.count() > 1 {
            format!(
                "**Shard {}/{}** · assignment `{}`\n\n",
                config.shard.index + 1,
                config.shard.count(),
                crate::sharding::ASSIGNMENT_VERSION
            )
        } else {
            String::new()
        },
        config_yaml(config, bench_names),
        trigger_comment_block(config.trigger_comment.as_deref()),
    )
}

/// Longest trigger comment quoted in full. Longer comments are cut so the
/// quote cannot push a result comment past GitHub's size limit.
const MAX_TRIGGER_QUOTE_CHARS: usize = 4000;

/// A collapsed `<details>` block quoting the raw trigger comment, or an empty
/// string when the body is unknown.
///
/// The body goes in a code fence, not a `>` quote. In a fence GitHub renders
/// no markdown and sends no `@mention` notifications.
pub fn trigger_comment_block(body: Option<&str>) -> String {
    let Some(body) = body.map(str::trim).filter(|b| !b.is_empty()) else {
        return String::new();
    };
    let mut text: String = body.chars().take(MAX_TRIGGER_QUOTE_CHARS).collect();
    if body.chars().count() > MAX_TRIGGER_QUOTE_CHARS {
        text.push_str("\n… (truncated)");
    }
    format!(
        "<details><summary>Trigger comment</summary>\n\n{}\n\n</details>\n\n",
        crate::criterion_report::fenced(&text, "text")
    )
}

/// Render the run's configuration in trigger-comment YAML syntax.
fn config_yaml(config: &RunnerConfig, bench_names: &str) -> String {
    let mut lines = Vec::new();

    let names = bench_names.trim();
    if names.is_empty() {
        lines.push("run benchmarks".to_string());
    } else {
        lines.push(format!("run benchmark {names}"));
    }

    if config.shard.count() > 1 {
        lines.push(format!("shards: {}", config.shard.count()));
    }

    if !config.shared_env_vars.is_empty() {
        lines.push("env:".to_string());
        lines.extend(env_lines(&config.shared_env_vars, "  "));
    }

    push_side(
        &mut lines,
        "baseline",
        config.baseline_ref.as_deref(),
        &config.baseline_env_vars,
    );
    push_side(
        &mut lines,
        "changed",
        config.changed_ref.as_deref(),
        &config.changed_env_vars,
    );

    push_resources(&mut lines, &config.resources);

    // A trigger comment sets the filter through the shared `env:` block, which
    // is already rendered above. Only the scheduled main-tracking workflow sets
    // BENCH_FILTER outside that block — note it there so it isn't lost.
    if !config.bench_filter.is_empty() && !config.shared_env_vars.contains_key("BENCH_FILTER") {
        lines.push(format!("# BENCH_FILTER: {}", config.bench_filter));
    }

    lines.join("\n")
}

/// Append the `resources:` section, omitting it when the pod was built from
/// the controller defaults. Only the fields the trigger named are rendered,
/// so the block keeps asking for the same pod when it is pasted back.
fn push_resources(lines: &mut Vec<String>, resources: &PodResources) {
    if resources.is_empty() {
        return;
    }
    lines.push("resources:".to_string());
    for (key, value) in [
        ("cpu", &resources.cpu),
        ("memory", &resources.memory),
        ("arch", &resources.arch),
    ] {
        if let Some(value) = value {
            lines.push(format!("  {key}: {}", quote(value)));
        }
    }
}

/// Append a `baseline:`/`changed:` section, omitting it when the side has
/// neither a custom ref nor env vars.
fn push_side(
    lines: &mut Vec<String>,
    name: &str,
    git_ref: Option<&str>,
    env: &HashMap<String, String>,
) {
    if git_ref.is_none() && env.is_empty() {
        return;
    }
    lines.push(format!("{name}:"));
    if let Some(git_ref) = git_ref {
        lines.push(format!("  ref: {}", quote(git_ref)));
    }
    if !env.is_empty() {
        lines.push("  env:".to_string());
        lines.extend(env_lines(env, "    "));
    }
}

/// Render an env map as sorted `KEY: "VALUE"` lines. Sorted because the map
/// iteration order is otherwise arbitrary and would reshuffle between comments.
fn env_lines(env: &HashMap<String, String>, indent: &str) -> Vec<String> {
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    keys.iter()
        .map(|k| format!("{indent}{k}: {}", quote(&env[*k])))
        .collect()
}

/// Always double-quote scalars: values like `1` or `on` would otherwise round-trip
/// as a number/bool rather than the string the run actually saw.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::config::{BenchType, PosterMode, RunnerConfig};

    fn config() -> RunnerConfig {
        RunnerConfig {
            trigger_comment: None,
            pr_url: "https://github.com/apache/datafusion/pull/42".into(),
            comment_id: "100".into(),
            comment_url: "https://github.com/apache/datafusion/pull/42#issuecomment-100".into(),
            benchmarks: "tpch".into(),
            bench_type: BenchType::Datafusion,
            bench_name: "sql_planner".into(),
            bench_filter: String::new(),
            repo: "apache/datafusion".into(),
            poster_mode: PosterMode::Direct {
                github_token: "x".into(),
            },
            sccache_gcs_bucket: None,
            data_cache_bucket: None,
            shared_env_vars: HashMap::new(),
            baseline_env_vars: HashMap::new(),
            changed_env_vars: HashMap::new(),
            baseline_ref: None,
            changed_ref: None,
            runner_repo_url: None,
            resources: PodResources::default(),
            shard: crate::sharding::Shard::default(),
            frozen_sources: None,
        }
    }

    #[test]
    fn shard_override_round_trips_and_header_is_separate() {
        for count in [1, 4] {
            let mut cfg = config();
            cfg.shard = crate::sharding::Shard {
                count,
                index: count - 1,
            };
            let yaml = config_yaml(&cfg, "arrow_writer");
            assert_eq!(yaml.contains(&format!("shards: {count}")), count > 1);
            let req = crate::benchmarks::detect_benchmark(
                &yaml,
                &crate::resources::ResourceLimits::default(),
            );
            let crate::benchmarks::DetectResult::Parsed(req) = req else {
                panic!("invalid reproduction YAML")
            };
            assert_eq!(req.shards, (count > 1).then_some(count));
            let block = config_block(&cfg, "arrow_writer");
            assert_eq!(block.contains("**Shard"), count > 1);
        }
        let mut one = config();
        one.shard.count = 1;
        assert_eq!(
            config_block(&one, "arrow_writer"),
            config_block(&config(), "arrow_writer")
        );
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn yaml_minimal_is_just_the_trigger_line() {
        assert_eq!(config_yaml(&config(), "tpch"), "run benchmark tpch");
    }

    #[test]
    fn yaml_no_names_renders_default_suite() {
        assert_eq!(config_yaml(&config(), "  "), "run benchmarks");
    }

    #[test]
    fn yaml_full_config_round_trips_trigger_syntax() {
        let mut cfg = config();
        cfg.shared_env_vars = map(&[("CARGO_BUILD_JOBS", "1")]);
        cfg.baseline_ref = Some("v45.0.0".into());
        cfg.changed_ref = Some("v46.0.0".into());
        cfg.baseline_env_vars = map(&[("DATAFUSION_RUNTIME_MEMORY_LIMIT", "1G")]);
        cfg.changed_env_vars = map(&[("DATAFUSION_RUNTIME_MEMORY_LIMIT", "2G")]);

        assert_eq!(
            config_yaml(&cfg, "tpch"),
            "run benchmark tpch\n\
             env:\n\
             \x20 CARGO_BUILD_JOBS: \"1\"\n\
             baseline:\n\
             \x20 ref: \"v45.0.0\"\n\
             \x20 env:\n\
             \x20   DATAFUSION_RUNTIME_MEMORY_LIMIT: \"1G\"\n\
             changed:\n\
             \x20 ref: \"v46.0.0\"\n\
             \x20 env:\n\
             \x20   DATAFUSION_RUNTIME_MEMORY_LIMIT: \"2G\""
        );
    }

    #[test]
    fn yaml_re_parses_as_the_same_request() {
        // The whole point of re-rendering in trigger syntax: pasting the block
        // back into a PR comment reproduces the run.
        let mut cfg = config();
        cfg.shared_env_vars = map(&[("CARGO_BUILD_JOBS", "1")]);
        cfg.baseline_ref = Some("v45.0.0".into());
        cfg.changed_env_vars = map(&[("DATAFUSION_RUNTIME_MEMORY_LIMIT", "2G")]);
        cfg.resources = PodResources {
            cpu: Some("16".into()),
            memory: Some("128Gi".into()),
            arch: Some("amd64".into()),
        };

        let yaml = config_yaml(&cfg, "tpch clickbench_1");
        let limits = crate::resources::ResourceLimits::default();
        let req = match crate::benchmarks::detect_benchmark(&yaml, &limits) {
            crate::benchmarks::DetectResult::Parsed(req) => req,
            _ => panic!("re-rendered config did not parse: {yaml}"),
        };
        assert_eq!(req.benchmarks, vec!["tpch", "clickbench_1"]);
        assert_eq!(req.env_vars.get("CARGO_BUILD_JOBS").unwrap(), "1");
        assert_eq!(req.baseline_ref.as_deref(), Some("v45.0.0"));
        assert_eq!(
            req.changed_env_vars
                .get("DATAFUSION_RUNTIME_MEMORY_LIMIT")
                .unwrap(),
            "2G"
        );
        assert_eq!(req.resources, cfg.resources);
    }

    #[test]
    fn yaml_omits_resources_for_a_default_sized_pod() {
        assert_eq!(config_yaml(&config(), "tpch"), "run benchmark tpch");
    }

    #[test]
    fn yaml_renders_the_requested_resources() {
        let mut cfg = config();
        cfg.resources = PodResources {
            cpu: Some("16".into()),
            memory: Some("128Gi".into()),
            arch: Some("amd64".into()),
        };
        assert_eq!(
            config_yaml(&cfg, "tpch"),
            "run benchmark tpch\nresources:\n  cpu: \"16\"\n  memory: \"128Gi\"\n  arch: \"amd64\""
        );
    }

    /// Only the fields the trigger named, so pasting the block back keeps the
    /// rest on the controller defaults rather than pinning today's values.
    #[test]
    fn yaml_renders_only_the_requested_resource_fields() {
        let mut cfg = config();
        cfg.resources = PodResources {
            memory: Some("128Gi".into()),
            ..Default::default()
        };
        assert_eq!(
            config_yaml(&cfg, "tpch"),
            "run benchmark tpch\nresources:\n  memory: \"128Gi\""
        );
    }

    #[test]
    fn yaml_side_with_only_env_omits_ref() {
        let mut cfg = config();
        cfg.baseline_env_vars = map(&[("FOO", "bar")]);
        assert_eq!(
            config_yaml(&cfg, "tpch"),
            "run benchmark tpch\nbaseline:\n  env:\n    FOO: \"bar\""
        );
    }

    #[test]
    fn yaml_env_keys_are_sorted() {
        let mut cfg = config();
        cfg.shared_env_vars = map(&[("ZZZ", "1"), ("AAA", "2"), ("MMM", "3")]);
        let yaml = config_yaml(&cfg, "tpch");
        let a = yaml.find("AAA").unwrap();
        let m = yaml.find("MMM").unwrap();
        let z = yaml.find("ZZZ").unwrap();
        assert!(a < m && m < z);
    }

    #[test]
    fn trigger_comment_is_fenced_in_its_own_block() {
        let mut cfg = config();
        cfg.trigger_comment = Some("run benchmark tpch\nenv:\n  BENCH_FILTER: q1 @someone".into());
        let block = config_block(&cfg, "tpch");
        assert!(block.ends_with(
            "<details><summary>Trigger comment</summary>\n\n\
             ```text\nrun benchmark tpch\nenv:\n  BENCH_FILTER: q1 @someone\n```\n\n\
             </details>\n\n"
        ));
    }

    #[test]
    fn missing_or_blank_trigger_comment_renders_nothing() {
        assert_eq!(trigger_comment_block(None), "");
        assert_eq!(trigger_comment_block(Some("  \n ")), "");
    }

    #[test]
    fn trigger_comment_with_backticks_gets_a_longer_fence() {
        let block = trigger_comment_block(Some("```\nrun benchmarks\n```"));
        assert!(block.contains("````text\n```\nrun benchmarks\n```\n````"));
    }

    #[test]
    fn long_trigger_comment_is_truncated() {
        let body = "x".repeat(MAX_TRIGGER_QUOTE_CHARS + 10);
        let block = trigger_comment_block(Some(&body));
        let expected = format!("{}\n… (truncated)", "x".repeat(MAX_TRIGGER_QUOTE_CHARS));
        assert!(block.contains(&expected));
        assert!(!block.contains(&"x".repeat(MAX_TRIGGER_QUOTE_CHARS + 1)));
    }

    #[test]
    fn yaml_notes_a_bench_filter_set_outside_the_env_block() {
        // The main-tracking workflow sets BENCH_FILTER directly on the pod.
        let mut cfg = config();
        cfg.bench_filter = "sort_tpch".into();
        assert!(config_yaml(&cfg, "sql_planner").contains("# BENCH_FILTER: sort_tpch"));
    }

    #[test]
    fn yaml_does_not_repeat_a_bench_filter_from_the_env_block() {
        // `env: BENCH_FILTER: float` in the trigger comment reaches the runner
        // as both a shared env var and `bench_filter` — render it once.
        let mut cfg = config();
        cfg.bench_filter = "float".into();
        cfg.shared_env_vars = map(&[("BENCH_FILTER", "float")]);
        let yaml = config_yaml(&cfg, "arrow_writer");
        assert!(yaml.contains("  BENCH_FILTER: \"float\""));
        assert!(!yaml.contains("# BENCH_FILTER"));
    }

    #[test]
    fn quote_escapes_quotes_and_backslashes() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn config_block_is_collapsed_details() {
        let block = config_block(&config(), "tpch");
        assert!(block.starts_with("<details><summary>Run configuration</summary>"));
        assert!(block.contains("```yaml\nrun benchmark tpch\n```"));
        assert!(block.ends_with("</details>\n\n"));
    }

    #[test]
    fn comparison_line_links_the_diff() {
        let cmp = Comparison {
            repo: "apache/datafusion",
            changed_display: "my-branch",
            changed_sha: "aaaa111",
            baseline_label: "bbbb222 (merge-base)",
            base_sha: "bbbb222",
        };
        assert_eq!(
            cmp.line(),
            "Comparing my-branch (aaaa111) to bbbb222 (merge-base) \
             [diff](https://github.com/apache/datafusion/compare/bbbb222..aaaa111)"
        );
    }
}
