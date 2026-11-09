use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    github,
    models::BenchmarkJob,
    resources::PodResources,
    runner::{config::RunnerConfig, trigger::Comparison},
    shard_reporting::{Baseline, ShardResult},
    sharding::FrozenSources,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunnerInfo {
    pub node_name: String,
    pub instance: String,
    pub resources: String,
    pub uname: String,
    pub cpu_details: String,
    pub bench_command: String,
    pub resource_report: String,
}

pub struct ExecutionContext {
    pub repo: String,
    pub comment_url: String,
    pub target: String,
    pub shards: u32,
    pub sources: Option<FrozenSources>,
    shared_env: BTreeMap<String, String>,
    baseline_env: BTreeMap<String, String>,
    changed_env: BTreeMap<String, String>,
    baseline_ref: Option<String>,
    changed_ref: Option<String>,
    resources: PodResources,
    trigger_comment: Option<String>,
}

impl ExecutionContext {
    pub fn from_job(job: &BenchmarkJob) -> Result<Self> {
        let names: Vec<String> = serde_json::from_str(&job.benchmarks)?;
        ensure!(names.len() == 1, "expected one target per execution unit");
        Ok(Self {
            repo: job.repo.clone(),
            comment_url: format!("{}#issuecomment-{}", job.pr_url, job.comment_id),
            target: names[0].clone(),
            shards: job.effective_shards,
            sources: job
                .resolved_source_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            shared_env: parse_env(&job.env_vars)?,
            baseline_env: parse_env(&job.baseline_env_vars)?,
            changed_env: parse_env(&job.changed_env_vars)?,
            baseline_ref: job.baseline_ref.clone(),
            changed_ref: job.changed_ref.clone(),
            resources: PodResources {
                cpu: job.cpu_request.clone(),
                memory: job.memory_request.clone(),
                arch: job.cpu_arch.clone(),
            },
            trigger_comment: job.trigger_comment.clone(),
        })
    }

    pub fn from_config(config: &RunnerConfig) -> Self {
        Self {
            repo: config.repo.clone(),
            comment_url: config.comment_url.clone(),
            target: config.bench_name.clone(),
            shards: config.shard.count(),
            sources: config.frozen_sources.clone(),
            shared_env: config.shared_env_vars.clone().into_iter().collect(),
            baseline_env: config.baseline_env_vars.clone().into_iter().collect(),
            changed_env: config.changed_env_vars.clone().into_iter().collect(),
            baseline_ref: config.baseline_ref.clone(),
            changed_ref: config.changed_ref.clone(),
            resources: config.resources.clone(),
            trigger_comment: config.trigger_comment.clone(),
        }
    }

    fn configuration(&self) -> Result<String> {
        let comparison = if let Some(sources) = &self.sources {
            sources.validate()?;
            let baseline = self
                .baseline_ref
                .clone()
                .unwrap_or_else(|| format!("{} (merge-base)", &sources.baseline_sha[..7]));
            Comparison {
                repo: &self.repo,
                changed_display: self.changed_ref.as_deref().unwrap_or(&sources.pr_head_ref),
                changed_sha: &sources.changed_sha,
                baseline_label: &baseline,
                base_sha: &sources.baseline_sha,
            }
            .line()
        } else {
            "Commit identities unavailable.".into()
        };
        let mut yaml = BTreeMap::<&str, Value>::new();
        yaml.insert("shards", self.shards.into());
        if !self.shared_env.is_empty() {
            yaml.insert("env", serde_json::to_value(&self.shared_env)?);
        }
        for (name, reference, env) in [
            ("baseline", &self.baseline_ref, &self.baseline_env),
            ("changed", &self.changed_ref, &self.changed_env),
        ] {
            let mut side = BTreeMap::<&str, Value>::new();
            if let Some(reference) = reference {
                side.insert("ref", reference.clone().into());
            }
            if !env.is_empty() {
                side.insert("env", serde_json::to_value(env)?);
            }
            if !side.is_empty() {
                yaml.insert(name, serde_json::to_value(side)?);
            }
        }
        let resources: BTreeMap<_, _> = [
            ("cpu", &self.resources.cpu),
            ("memory", &self.resources.memory),
            ("arch", &self.resources.arch),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k, v)))
        .collect();
        if !resources.is_empty() {
            yaml.insert("resources", serde_json::to_value(resources)?);
        }
        let yaml = format!(
            "run benchmark {}\n{}",
            self.target,
            serde_yaml::to_string(&yaml)?
        );
        let quote = crate::runner::trigger::trigger_comment_block(self.trigger_comment.as_deref());
        Ok(format!(
            "{comparison}\n\n<details><summary>Run configuration</summary>\n\n{}\n\n</details>\n\n{quote}",
            fenced(&yaml, "yaml"),
        )
        .trim_end()
        .to_string())
    }
}

fn parse_env(json: &str) -> Result<BTreeMap<String, String>> {
    if json.trim_start().starts_with('[') {
        Ok(serde_json::from_str::<Vec<String>>(json)?
            .into_iter()
            .filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect())
    } else {
        Ok(serde_json::from_str(json)?)
    }
}

pub fn start_body(context: &ExecutionContext, runner_repo: Option<&str>) -> Result<String> {
    Ok(format!("🤖 Benchmark starting (GKE) | [trigger]({})\n\n**Target:** {}\n\n**Sharding factor: {}** ({} workers).\n\n{}\n\nResults will be posted when all workers finish.{}",
        context.comment_url, escape(&context.target), context.shards, context.shards,
        context.configuration()?, github::issues_footer(runner_repo)))
}

pub struct WorkerReport<'a> {
    pub index: u32,
    pub info: Option<&'a RunnerInfo>,
    pub result: Option<&'a ShardResult>,
    pub error: Option<&'a str>,
}

/// Keep the native comparison separate so oversized reports can link to it
/// without re-running critcmp or rewriting its output.
pub struct Report {
    prefix: String,
    comparison: Option<String>,
    suffix: String,
    summary: String,
    gist_description: String,
}

impl Report {
    pub fn inline_body(&self) -> String {
        let comparison = self.comparison.as_ref().map(|text| {
            format!(
                "<details><summary>Details</summary>\n\n{}\n\n</details>\n\n",
                fenced(text, "")
            )
        });
        self.with_comparison(comparison.as_deref().unwrap_or_default())
    }

    fn with_comparison(&self, section: &str) -> String {
        format!("{}{section}{}", self.prefix, self.suffix)
    }

    /// Return the comment and any Gist URL that the caller must persist before
    /// posting it. Transient/ambiguous API failures leave reconciliation pending;
    /// permanent failures produce an explicit reporting error instead of hiding
    /// the execution outcome or silently omitting the comparison.
    pub async fn github_body(
        &self,
        gh: &github::GitHubClient,
        key: &str,
        suffix_chars: usize,
        existing_gist: Option<String>,
    ) -> Result<(String, Option<String>)> {
        let inline = self.inline_body();
        let length = inline.chars().count() + suffix_chars;
        if length <= MAX_GITHUB_COMMENT_CHARS && existing_gist.is_none() {
            return Ok((inline, None));
        }
        let mut gist_url = existing_gist;
        if let (Some(comparison), None) = (&self.comparison, &gist_url) {
            match gh
                .ensure_comparison_gist(key, &self.gist_description, comparison)
                .await
            {
                Ok(url) => gist_url = Some(url),
                Err(error) if github::is_retryable(&error) => return Err(error),
                Err(error) => tracing::warn!(%error, "could not publish comparison Gist"),
            }
        }
        let section = match &gist_url {
            Some(url) => format!("**Comparison results:** [View the complete native critcmp output]({url}).\n\nThe comparison is published in an unlisted Gist because the original inline report exceeded GitHub's comment limit.\n\n"),
            None if self.comparison.is_some() => "**Reporting error:** Could not publish the comparison Gist. Check the reporting process logs and ensure its GitHub token has permission to create Gists. No comparison results have been published.\n\n".into(),
            None => String::new(),
        };
        let linked = self.with_comparison(&section);
        if linked.chars().count() + suffix_chars <= MAX_GITHUB_COMMENT_CHARS {
            return Ok((linked, gist_url));
        }
        // Even diagnostics alone may be too large. Keep execution status and
        // the complete comparison link, but never truncate diagnostics to fit.
        Ok((format!("{}\n\n{section}**Reporting error:** The full inline report contains {length} characters and the remaining diagnostics still exceed GitHub's {MAX_GITHUB_COMMENT_CHARS}-character limit. Full runner diagnostics could not be posted; no diagnostic sections have been truncated.", self.summary), gist_url))
    }
}

pub async fn report_body(
    context: &ExecutionContext,
    merged: Result<(Baseline, Baseline)>,
    workers: &[WorkerReport<'_>],
    runner_repo: Option<&str>,
) -> Result<Report> {
    let mut errors = Vec::new();
    let mut machines = Vec::new();
    let mut branch_only = Vec::new();
    for worker in workers {
        let label = format!(
            "{} — shard {}/{}",
            context.target,
            worker.index + 1,
            context.shards
        );
        if let Some(error) = worker.error {
            errors.push(format!("{label}: {error}"));
        }
        if worker.result.is_some_and(|r| r.base.is_none()) {
            branch_only.push((worker.index + 1).to_string());
        }
        let details = match worker.info {
            None => "Runner information unavailable.".into(),
            Some(info) => format!("**Node:** {}\n\n**Instance:** {} ({})\n\n**uname:**\n\n{}\n\n**BENCH_COMMAND:**\n\n{}\n\n<details><summary>CPU Details (lscpu)</summary>\n\n{}\n\n</details>\n\n<details><summary>Resource Usage</summary>\n\n{}\n\n</details>",
                escape(&info.node_name), escape(&info.instance), escape(&info.resources),
                fenced(&info.uname, "text"), fenced(&info.bench_command, "sh"),
                fenced(&info.cpu_details, "text"),
                if info.resource_report.is_empty() { "No completed measurement resource samples." } else { &info.resource_report }),
        };
        machines.push(format!(
            "<details><summary>{}</summary>\n\n{details}\n\n</details>",
            escape(&label)
        ));
    }
    let has_results = workers.iter().any(|w| w.result.is_some());
    let report = match merged {
        Ok((base, changed)) if has_results => {
            match crate::shard_reporting::compare(&base, &changed).await {
                Ok(report) => Some(report),
                Err(error) => {
                    errors.push(format!("Could not compare results: {error:#}"));
                    None
                }
            }
        }
        Ok(_) => None,
        Err(error) => {
            errors.push(format!("Could not combine results: {error:#}"));
            None
        }
    };
    let outcome = if errors.is_empty() {
        "completed"
    } else if report.is_some() {
        "failed or incomplete — partial results"
    } else {
        "failed or incomplete"
    };
    let mut body = format!(
        "🤖 Benchmark {outcome} (GKE) | [trigger]({})\n\n**Target:** {}\n\n{}\n\n",
        context.comment_url,
        escape(&context.target),
        context.configuration()?
    );
    if !errors.is_empty() {
        body.push_str(&format!(
            "<details open><summary>Errors / missing shards</summary>\n\n{}\n\n</details>\n\n",
            fenced(&errors.join("\n\n"), "text")
        ));
    }
    if !branch_only.is_empty() {
        body.push_str(&format!("**Baseline build unavailable for shard(s) {}; changed-only measurements for those workers.**\n\n", branch_only.join(", ")));
    }
    let completed = workers
        .iter()
        .filter(|w| w.error.is_none() && w.result.is_some())
        .count();
    let exports = workers.iter().filter(|w| w.result.is_some()).count();
    let target: String = context.target.chars().take(200).collect();
    let mut summary = format!("🤖 Benchmark {outcome} (GKE) | [trigger]({})\n\n**Target:** {}\n\n**Workers:** {completed}/{} completed successfully; {exports}/{} exports received.", context.comment_url, escape(&target), context.shards, context.shards);
    if !branch_only.is_empty() {
        summary.push_str(&format!(
            "\n\nBaseline build unavailable for shard(s) {}.",
            branch_only.join(", ")
        ));
    }
    Ok(Report {
        prefix: body,
        comparison: report,
        suffix: format!(
            "<details><summary>Per-runner information</summary>\n\n{}\n\n</details>{}",
            machines.join("\n\n"),
            github::issues_footer(runner_repo)
        ),
        summary,
        gist_description: format!("{} — {}", context.target, context.comment_url),
    })
}

pub const MAX_GITHUB_COMMENT_CHARS: usize = 1 << 16;

/// The marker suffix is part of the GitHub limit too. Never publish a partial table.
pub fn github_body(context: &ExecutionContext, body: String, suffix_chars: usize) -> String {
    let length = body.chars().count() + suffix_chars;
    if length <= MAX_GITHUB_COMMENT_CHARS {
        return body;
    }
    let target: String = context.target.chars().take(200).collect();
    format!("🤖 Benchmark report could not be posted | [trigger]({})\n\n**Target:** {}\n\n**Sharding factor: {}**\n\nThe complete report contains {length} characters, exceeding GitHub's {MAX_GITHUB_COMMENT_CHARS}-character comment limit. No partial results or runner information have been posted. Use a narrower `BENCH_FILTER` to reduce the report size.", context.comment_url, escape(&target), context.shards)
}

fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '|' => "&#124;".into(),
            '`' => "&#96;".into(),
            '\\' => "&#92;".into(),
            '*' => "&#42;".into(),
            '_' => "&#95;".into(),
            '[' => "&#91;".into(),
            ']' => "&#93;".into(),
            _ => c.to_string(),
        })
        .collect()
}

pub(crate) fn fenced(text: &str, language: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}{language}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shard_reporting::tests::github_with_gists;

    fn large_report() -> Report {
        Report {
            prefix: "Benchmark completed\n\nconfiguration\n\n".into(),
            comparison: Some("case/λ ` 1.00 100ns 1.20 120ns\n".repeat(3000)),
            suffix: "full CPU diagnostics and resource usage".into(),
            summary: "Benchmark completed; 4/4 completed successfully; 4/4 exports received".into(),
            gist_description: "target comparison".into(),
        }
    }

    #[tokio::test]
    async fn gist_fallback_counts_characters_and_reserves_the_comment_marker() {
        let (gh, _, gists, server) = github_with_gists().await;
        let mut report = large_report();
        report.comparison = Some(String::new());
        let available = MAX_GITHUB_COMMENT_CHARS - report.inline_body().chars().count() - 80;
        report.comparison = Some("界".repeat(available));
        let inline = report.inline_body();
        let (body, url) = report.github_body(&gh, "key", 80, None).await.unwrap();
        assert_eq!(body, inline);
        assert!(url.is_none());
        assert_eq!(gists.lock().await.gets, 0);
        let (body, url) = report.github_body(&gh, "key", 81, None).await.unwrap();
        assert!(body.contains("Benchmark completed"));
        assert!(body.contains("full CPU diagnostics"));
        assert!(body.contains(url.as_deref().unwrap()));
        assert!(body.chars().count() + 81 <= MAX_GITHUB_COMMENT_CHARS);
        assert_eq!(
            gists.lock().await.gists[0]["files"]["comparison.txt"]["content"],
            report.comparison.as_deref().unwrap()
        );
        report
            .github_body(&gh, "key", 81, url.clone())
            .await
            .unwrap();
        // A restart must not discard the saved artifact if comparison rendering
        // subsequently fails or produces a shorter body.
        report.comparison = None;
        let (body, saved) = report
            .github_body(&gh, "key", 81, url.clone())
            .await
            .unwrap();
        assert_eq!(saved, url);
        assert!(body.contains(saved.as_deref().unwrap()));
        assert_eq!(gists.lock().await.gets, 1);
        assert_eq!(gists.lock().await.posts, 1);
        server.abort();
    }

    #[tokio::test]
    async fn permanent_gist_failure_keeps_execution_status_and_diagnostics() {
        let (gh, _, gists, server) = github_with_gists().await;
        gists.lock().await.next_status = Some(403);
        let (body, url) = large_report()
            .github_body(&gh, "key", 100, None)
            .await
            .unwrap();
        assert!(url.is_none());
        assert!(body.contains("Benchmark completed"));
        assert!(body.contains("full CPU diagnostics"));
        assert!(body.contains("Could not publish the comparison Gist"));
        assert!(!body.contains("case/λ"));
        assert!(body.chars().count() + 100 < MAX_GITHUB_COMMENT_CHARS);
        assert!(gists.lock().await.gists.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn transient_gist_failure_remains_retryable() {
        let (gh, _, gists, server) = github_with_gists().await;
        gists.lock().await.next_status = Some(503);
        let report = large_report();
        assert!(report.github_body(&gh, "key", 100, None).await.is_err());
        assert!(gists.lock().await.gists.is_empty());
        let (body, url) = report.github_body(&gh, "key", 100, None).await.unwrap();
        assert!(url.is_some());
        assert!(body.contains("Benchmark completed"));
        assert!(!body.contains("Reporting error"));
        server.abort();
    }

    #[tokio::test]
    async fn oversized_failure_without_comparison_preserves_status_without_a_gist() {
        let (gh, _, gists, server) = github_with_gists().await;
        let mut report = large_report();
        report.comparison = None;
        report.suffix = "diagnostics".repeat(10000);
        report.summary =
            "Benchmark failed or incomplete; 0/4 completed successfully; 0/4 exports received"
                .into();
        let (body, url) = report.github_body(&gh, "key", 100, None).await.unwrap();
        assert!(url.is_none());
        assert!(body.contains("failed or incomplete"));
        assert!(body.contains("0/4 exports received"));
        assert!(body.contains("Full runner diagnostics could not be posted"));
        assert!(body.chars().count() + 100 < MAX_GITHUB_COMMENT_CHARS);
        assert_eq!(gists.lock().await.gets, 0);
        server.abort();
    }

    #[tokio::test]
    async fn gist_recovery_paginates_without_creating_another_gist() {
        let (gh, _, gists, server) = github_with_gists().await;
        {
            let mut state = gists.lock().await;
            state.gists = (0..100).map(|_| serde_json::json!({"description": null, "html_url": "https://gist.github.com/unrelated"})).collect();
            state.gists.push(serde_json::json!({"description": "old description [benchmark-comparison:key]", "html_url": "https://gist.github.com/recovered"}));
        }
        let (body, _) = large_report()
            .github_body(&gh, "key", 0, None)
            .await
            .unwrap();
        assert!(body.contains("https://gist.github.com/recovered"));
        assert_eq!(gists.lock().await.gets, 2);
        assert_eq!(gists.lock().await.posts, 0);
        server.abort();
    }
}
