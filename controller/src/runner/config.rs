//! Runner environment variable parsing.
//!
//! The controller passes these env vars to the runner K8s pod (see `job_manager.rs`):
//! `PR_URL`, `COMMENT_ID`, `BENCHMARKS`, `BENCH_TYPE`, `BENCH_NAME`,
//! `BENCH_FILTER`, `REPO`, `GITHUB_TOKEN`, `JOB_NAME`.

use anyhow::{Context, Result};

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
    pub github_token: String,
    pub sccache_gcs_bucket: Option<String>,
    pub data_cache_bucket: Option<String>,
}

impl RunnerConfig {
    /// Parse runner configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let pr_url = env_required("PR_URL")?;
        let comment_id = env_required("COMMENT_ID")?;
        let comment_url = format!("{pr_url}#issuecomment-{comment_id}");
        let bench_type_str = env_required("BENCH_TYPE")?;

        Ok(Self {
            pr_url,
            comment_id,
            comment_url,
            benchmarks: env_or("BENCHMARKS", ""),
            bench_type: BenchType::from_str(&bench_type_str)?,
            bench_name: env_or("BENCH_NAME", "sql_planner"),
            bench_filter: env_or("BENCH_FILTER", ""),
            repo: env_required("REPO")?,
            github_token: env_required("GITHUB_TOKEN")?,
            sccache_gcs_bucket: std::env::var("SCCACHE_GCS_BUCKET").ok(),
            data_cache_bucket: std::env::var("DATA_CACHE_BUCKET").ok(),
        })
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
