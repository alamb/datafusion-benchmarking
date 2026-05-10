//! Runner environment variable parsing.
//!
//! The controller passes these env vars to PR-triggered runner pods (see
//! `job_manager.rs`): `PR_URL`, `COMMENT_ID`, `BENCHMARKS`, `BENCH_TYPE`,
//! `BENCH_NAME`, `BENCH_FILTER`, `REPO`, `JOB_ID`, `RUNNER_TOKEN`,
//! `CONTROLLER_URL`, `RUNNER_REPO_URL`. The scheduled main-tracking
//! workflow instead supplies `GITHUB_TOKEN` directly (no PR author to
//! distrust).

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::github::GitHubClient;
use crate::runner::controller_client::ControllerClient;
use crate::runner::poster::CommentPoster;

/// Benchmark runner variant, parsed from `BENCH_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchType {
    Standard,
    Criterion,
    ArrowCriterion,
    MainTracking,
}

impl BenchType {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "standard" => Ok(Self::Standard),
            "criterion" => Ok(Self::Criterion),
            "arrow_criterion" => Ok(Self::ArrowCriterion),
            "main_tracking" => Ok(Self::MainTracking),
            other => anyhow::bail!("unknown BENCH_TYPE: {other}"),
        }
    }
}

/// How PR comments should be posted from this runner.
#[derive(Debug, Clone)]
pub enum PosterMode {
    /// Direct-to-GitHub with a `GITHUB_TOKEN`. Used by the scheduled
    /// main-tracking workflow where the code under benchmark is trusted.
    Direct { github_token: String },
    /// Proxy through the controller. Used by PR-triggered runs so the pod
    /// never sees any GitHub credential. The controller authenticates the
    /// runner with `token` and looks up the job by `job_id`.
    Proxy {
        controller_url: String,
        job_id: String,
        token: String,
    },
}

/// Parsed runner configuration from environment variables.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub pr_url: String,
    pub comment_id: String,
    pub comment_url: String,
    pub benchmarks: String,
    pub bench_type: BenchType,
    pub bench_name: String,
    pub bench_filter: String,
    pub repo: String,
    pub poster_mode: PosterMode,
    pub sccache_gcs_bucket: Option<String>,
    pub data_cache_bucket: Option<String>,
    pub baseline_env_vars: HashMap<String, String>,
    pub changed_env_vars: HashMap<String, String>,
    pub baseline_ref: Option<String>,
    pub changed_ref: Option<String>,
    /// URL of the benchmark runner's own GitHub repo (for "file an issue" links).
    pub runner_repo_url: Option<String>,
}

impl RunnerConfig {
    /// Parse runner configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let pr_url = env_required("PR_URL")?;
        let comment_id = env_required("COMMENT_ID")?;
        let comment_url = format!("{pr_url}#issuecomment-{comment_id}");
        let bench_type_str = env_required("BENCH_TYPE")?;

        let baseline_env_vars: HashMap<String, String> = std::env::var("BASELINE_ENV_VARS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let changed_env_vars: HashMap<String, String> = std::env::var("CHANGED_ENV_VARS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok(Self {
            pr_url,
            comment_id,
            comment_url,
            benchmarks: env_or("BENCHMARKS", ""),
            bench_type: BenchType::from_str(&bench_type_str)?,
            bench_name: env_or("BENCH_NAME", "sql_planner"),
            bench_filter: env_or("BENCH_FILTER", ""),
            repo: env_required("REPO")?,
            poster_mode: parse_poster_mode()?,
            sccache_gcs_bucket: std::env::var("SCCACHE_GCS_BUCKET").ok(),
            data_cache_bucket: std::env::var("DATA_CACHE_BUCKET").ok(),
            baseline_env_vars,
            changed_env_vars,
            baseline_ref: std::env::var("BASELINE_REF").ok(),
            changed_ref: std::env::var("CHANGED_REF").ok(),
            runner_repo_url: std::env::var("RUNNER_REPO_URL").ok(),
        })
    }

    /// Build the [`CommentPoster`] implied by [`Self::poster_mode`].
    pub fn build_poster(&self) -> CommentPoster {
        match &self.poster_mode {
            PosterMode::Direct { github_token } => {
                CommentPoster::Direct(GitHubClient::new(github_token))
            }
            PosterMode::Proxy {
                controller_url,
                job_id,
                token,
            } => CommentPoster::Proxy(ControllerClient::new(
                controller_url.clone(),
                job_id.clone(),
                token.clone(),
            )),
        }
    }

    /// The repo clone URL.
    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}.git", self.repo)
    }

    /// Extract the PR number from the PR URL (last path segment).
    pub fn pr_number(&self) -> Result<i64> {
        self.pr_url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .context("failed to parse PR number from PR_URL")
    }

    /// Build the env var arguments for running a baseline benchmark.
    /// Merges shared env vars (already set on the pod) with baseline-specific ones.
    /// Returns args like `["KEY=VALUE", ...]` for passing to `env` command.
    pub fn baseline_env_args(&self) -> Vec<String> {
        self.baseline_env_vars
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    }

    /// Build the env var arguments for running a changed/branch benchmark.
    /// Returns args like `["KEY=VALUE", ...]` for passing to `env` command.
    pub fn changed_env_args(&self) -> Vec<String> {
        self.changed_env_vars
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    }

    /// Set up sccache environment variables if configured.
    pub fn setup_sccache(&self) {
        if let Some(bucket) = &self.sccache_gcs_bucket {
            std::env::set_var("RUSTC_WRAPPER", "sccache");
            std::env::set_var("SCCACHE_GCS_BUCKET", bucket);
            std::env::set_var("SCCACHE_GCS_RW_MODE", "READ_WRITE");
        }
    }
}

fn env_required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Pick the poster mode based on which env vars are set. Proxy mode is
/// preferred when both are available — in practice only one set will be
/// present in any given pod.
fn parse_poster_mode() -> Result<PosterMode> {
    let runner_token = std::env::var("RUNNER_TOKEN").ok();
    let controller_url = std::env::var("CONTROLLER_URL").ok();
    let job_id = std::env::var("JOB_ID").ok();

    if let (Some(token), Some(url), Some(id)) = (runner_token, controller_url, job_id) {
        return Ok(PosterMode::Proxy {
            controller_url: url,
            job_id: id,
            token,
        });
    }

    if let Ok(github_token) = std::env::var("GITHUB_TOKEN") {
        return Ok(PosterMode::Direct { github_token });
    }

    anyhow::bail!(
        "missing credentials: set RUNNER_TOKEN+CONTROLLER_URL+JOB_ID (proxy mode) \
         or GITHUB_TOKEN (direct mode)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_type_standard() {
        assert_eq!(
            BenchType::from_str("standard").unwrap(),
            BenchType::Standard
        );
    }

    #[test]
    fn bench_type_criterion() {
        assert_eq!(
            BenchType::from_str("criterion").unwrap(),
            BenchType::Criterion
        );
    }

    #[test]
    fn bench_type_arrow_criterion() {
        assert_eq!(
            BenchType::from_str("arrow_criterion").unwrap(),
            BenchType::ArrowCriterion
        );
    }

    #[test]
    fn bench_type_main_tracking() {
        assert_eq!(
            BenchType::from_str("main_tracking").unwrap(),
            BenchType::MainTracking
        );
    }

    #[test]
    fn bench_type_unknown() {
        assert!(BenchType::from_str("bogus").is_err());
    }

    #[test]
    fn comment_url_construction() {
        let pr_url = "https://github.com/apache/datafusion/pull/12345";
        let comment_id = "999";
        let url = format!("{pr_url}#issuecomment-{comment_id}");
        assert_eq!(
            url,
            "https://github.com/apache/datafusion/pull/12345#issuecomment-999"
        );
    }

    #[test]
    fn pr_number_parsing() {
        let url = "https://github.com/apache/datafusion/pull/12345";
        let num: i64 = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert_eq!(num, 12345);
    }

    #[test]
    fn env_or_uses_default() {
        assert_eq!(env_or("__RUNNER_TEST_UNSET__", "fallback"), "fallback");
    }

    #[test]
    fn env_required_errors_when_missing() {
        assert!(env_required("__RUNNER_TEST_MISSING__").is_err());
    }
}
