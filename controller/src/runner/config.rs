//! Runner environment variable parsing.
//!
//! The controller passes these env vars to PR-triggered runner pods (see
//! `job_manager.rs`): `PR_URL`, `COMMENT_ID`, `BENCHMARKS`, `BENCH_TYPE`,
//! `BENCH_NAME`, `BENCH_FILTER`, `REPO`, `JOB_ID`, `RUNNER_TOKEN`,
//! `CONTROLLER_URL`, `RUNNER_REPO_URL`, `SHARED_ENV_VARS`,
//! `BASELINE_ENV_VARS`, `CHANGED_ENV_VARS`, `REQUESTED_CPU`,
//! `REQUESTED_MEMORY`, `REQUESTED_ARCH`. The scheduled main-tracking
//! workflow instead supplies `GITHUB_TOKEN` directly (no PR author to
//! distrust).

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::github::GitHubClient;
use crate::resources::PodResources;
use crate::runner::controller_client::ControllerClient;
use crate::runner::poster::CommentPoster;
use crate::sharding::{FrozenSources, Shard, WorkerShard};

/// Benchmark runner variant, parsed from `BENCH_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchType {
    Datafusion,
    ArrowCriterion,
    MainTracking,
}

impl BenchType {
    pub fn shard_support(self) -> crate::sharding::ShardSupport {
        match self {
            Self::Datafusion | Self::MainTracking => {
                crate::models::JobType::Datafusion.shard_support()
            }
            Self::ArrowCriterion => crate::models::JobType::ArrowCriterion.shard_support(),
        }
    }

    fn from_str(s: &str) -> Result<Self> {
        match s {
            // `standard`/`criterion` are legacy job-type strings that may still
            // be present on in-flight DB rows; both map to the unified
            // datafusion runner.
            "datafusion" | "standard" | "criterion" => Ok(Self::Datafusion),
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
    /// Env vars from the trigger comment's shared `env:` block. These are
    /// already set on the pod (so they reach the build too); the map is kept
    /// only so comments can report what was requested.
    pub shared_env_vars: HashMap<String, String>,
    pub baseline_env_vars: HashMap<String, String>,
    pub changed_env_vars: HashMap<String, String>,
    pub baseline_ref: Option<String>,
    pub changed_ref: Option<String>,
    /// URL of the benchmark runner's own GitHub repo (for "file an issue" links).
    pub runner_repo_url: Option<String>,
    /// Pod sizing the trigger comment asked for. Set only for the fields the
    /// trigger named, so comments can report the request rather than the
    /// defaults it left alone.
    pub resources: PodResources,
    pub shard: Shard,
    pub frozen_sources: Option<FrozenSources>,
    /// Raw body of the trigger comment, quoted in result comments.
    pub trigger_comment: Option<String>,
}

impl RunnerConfig {
    /// Parse runner configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let pr_url = env_required("PR_URL")?;
        let comment_id = env_required("COMMENT_ID")?;
        let comment_url = format!("{pr_url}#issuecomment-{comment_id}");
        let bench_type_str = env_required("BENCH_TYPE")?;

        let shared_env_vars: HashMap<String, String> = std::env::var("SHARED_ENV_VARS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let baseline_env_vars: HashMap<String, String> = std::env::var("BASELINE_ENV_VARS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let changed_env_vars: HashMap<String, String> = std::env::var("CHANGED_ENV_VARS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let worker = WorkerShard::from_args(std::env::args().skip(1))?;
        let (shard, frozen_sources) = if let Some(worker) = worker {
            BenchType::from_str(&bench_type_str)?
                .shard_support()
                .validate_execution(worker.shard)?;
            (worker.shard, Some(worker.sources))
        } else {
            (Shard::default(), None)
        };

        Ok(Self {
            shard,
            frozen_sources,
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
            shared_env_vars,
            baseline_env_vars,
            changed_env_vars,
            baseline_ref: std::env::var("BASELINE_REF").ok(),
            changed_ref: std::env::var("CHANGED_REF").ok(),
            runner_repo_url: std::env::var("RUNNER_REPO_URL").ok(),
            trigger_comment: std::env::var("TRIGGER_COMMENT").ok(),
            resources: PodResources {
                cpu: std::env::var("REQUESTED_CPU").ok(),
                memory: std::env::var("REQUESTED_MEMORY").ok(),
                arch: std::env::var("REQUESTED_ARCH").ok(),
            },
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
    fn bench_type_datafusion() {
        assert_eq!(
            BenchType::from_str("datafusion").unwrap(),
            BenchType::Datafusion
        );
    }

    #[test]
    fn bench_type_legacy_strings_map_to_datafusion() {
        // In-flight DB rows may still carry the old job-type strings.
        assert_eq!(
            BenchType::from_str("standard").unwrap(),
            BenchType::Datafusion
        );
        assert_eq!(
            BenchType::from_str("criterion").unwrap(),
            BenchType::Datafusion
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
