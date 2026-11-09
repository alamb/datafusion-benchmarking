//! Domain types shared across the controller.
//!
//! Includes database row structs, GitHub API response types, and enums for
//! job status and job type.

use std::collections::HashMap;

use serde::Deserialize;

use crate::resources::PodResources;
use crate::sharding::{Shard, ASSIGNMENT_VERSION};

/// Fields for inserting a new benchmark job into SQLite.
pub struct JobInsert<'a> {
    pub comment_id: i64,
    pub repo: &'a str,
    pub pr_number: i64,
    pub pr_url: &'a str,
    pub login: &'a str,
    pub benchmarks: &'a str,
    pub env_vars: &'a str,
    pub baseline_env_vars: &'a str,
    pub changed_env_vars: &'a str,
    pub baseline_ref: Option<&'a str>,
    pub changed_ref: Option<&'a str>,
    pub job_type: &'a str,
    /// Pod sizing asked for by the trigger comment. Each `None` field falls
    /// back to the controller default when the pod is built.
    pub resources: &'a PodResources,
    pub shard: Shard,
    pub resolved_source_json: Option<&'a str>,
    /// Raw body of the trigger comment.
    pub trigger_comment: Option<&'a str>,
}

/// SQLite row for a benchmark job. Status follows this state machine:
///
/// ```text
/// pending ──► running ──► completed
///    │           │
///    └───────────┴──► failed
/// ```
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct BenchmarkJob {
    pub id: i64,
    pub comment_id: i64,
    pub repo: String,
    pub pr_number: i64,
    pub pr_url: String,
    pub login: String,
    pub benchmarks: String,
    pub env_vars: String,
    pub baseline_env_vars: String,
    pub changed_env_vars: String,
    pub baseline_ref: Option<String>,
    pub changed_ref: Option<String>,
    pub job_type: String,
    pub cpu_request: Option<String>,
    pub memory_request: Option<String>,
    pub cpu_arch: Option<String>,
    pub k8s_job_name: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Random per-job token the runner uses to authenticate its
    /// `POST /jobs/{id}/comment` calls against the controller. `None` for
    /// legacy rows created before migration 003.
    pub runner_token: Option<String>,
    /// Immutable resolved count, even when setup capabilities change later.
    pub effective_shards: u32,
    pub shard_index: u32,
    pub assignment_version: Option<String>,
    pub resolved_source_json: Option<String>,
    /// Raw body of the trigger comment. `None` for rows queued before
    /// migration 007. The default lets migration tests decode rows from older
    /// schemas.
    #[sqlx(default)]
    pub trigger_comment: Option<String>,
}

impl BenchmarkJob {
    pub fn uses_collected_results(&self) -> bool {
        self.job_type == JobType::ArrowCriterion.as_str()
    }

    pub fn shard(&self) -> anyhow::Result<Shard> {
        let shard = Shard {
            count: self.effective_shards,
            index: self.shard_index,
        }
        .validate()?;
        if shard.count() > 1 {
            anyhow::ensure!(
                self.assignment_version.as_deref() == Some(ASSIGNMENT_VERSION),
                "unsupported shard assignment version"
            );
        }
        Ok(shard)
    }

    pub fn shard_label(&self) -> String {
        Shard {
            count: self.effective_shards,
            index: self.shard_index,
        }
        .label()
    }
}

/// Parsed user intent from a PR comment (e.g. `run benchmark tpch_mem`).
/// Empty `benchmarks` means "run the default suite".
#[derive(Debug, Clone, Default)]
pub struct BenchmarkRequest {
    pub benchmarks: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub baseline_env_vars: HashMap<String, String>,
    pub changed_env_vars: HashMap<String, String>,
    pub baseline_ref: Option<String>,
    pub changed_ref: Option<String>,
    pub resources: PodResources,
    pub shards: Option<u32>,
}

/// Benchmark runner variant.
///
/// - `Datafusion` — DataFusion benchmarks. The runner resolves each requested
///   name per-benchmark: real `cargo bench` Criterion targets run via Criterion,
///   everything else runs through `bench.sh`.
/// - `ArrowCriterion` — `cargo bench` criterion benchmarks in arrow-rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    Datafusion,
    ArrowCriterion,
}

impl JobType {
    pub fn shard_support(self) -> crate::sharding::ShardSupport {
        match self {
            Self::Datafusion => crate::runner::bench_datafusion::SHARD_SUPPORT,
            Self::ArrowCriterion => crate::runner::bench_arrow::SHARD_SUPPORT,
        }
    }

    /// Returns the string stored in the `job_type` SQLite column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Datafusion => "datafusion",
            Self::ArrowCriterion => "arrow_criterion",
        }
    }
}

/// Lifecycle status of a benchmark job in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    #[allow(dead_code)]
    Pending,
    Running,
    Completed,
    Failed,
}

impl JobStatus {
    /// Returns the string stored in the `status` SQLite column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// GitHub API response for an issue/PR comment (`GET /repos/{owner}/{repo}/issues/comments`).
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubComment {
    pub id: i64,
    pub body: Option<String>,
    pub user: Option<GitHubUser>,
    pub html_url: Option<String>,
    pub created_at: Option<String>,
    pub issue_url: Option<String>,
}

impl GitHubComment {
    /// The comment body text, or `""` if absent.
    pub fn body_text(&self) -> &str {
        self.body.as_deref().unwrap_or("")
    }

    /// The author's login, or `""` if absent.
    pub fn login(&self) -> &str {
        self.user.as_ref().map(|u| u.login.as_str()).unwrap_or("")
    }

    /// The HTML URL of this comment, or `""` if absent.
    pub fn url(&self) -> &str {
        self.html_url.as_deref().unwrap_or("")
    }

    /// The ISO 8601 creation timestamp, or `""` if absent.
    pub fn created_at_str(&self) -> &str {
        self.created_at.as_deref().unwrap_or("")
    }

    /// The API URL of the parent issue/PR, or `""` if absent.
    pub fn issue_url_str(&self) -> &str {
        self.issue_url.as_deref().unwrap_or("")
    }
}

/// Nested user object inside a [`GitHubComment`].
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── JobType::as_str ─────────────────────────────────────────────

    #[test]
    fn job_type_datafusion() {
        assert_eq!(JobType::Datafusion.as_str(), "datafusion");
    }

    #[test]
    fn job_type_arrow_criterion() {
        assert_eq!(JobType::ArrowCriterion.as_str(), "arrow_criterion");
    }

    // ── JobStatus::as_str ───────────────────────────────────────────

    #[test]
    fn job_status_pending() {
        assert_eq!(JobStatus::Pending.as_str(), "pending");
    }

    #[test]
    fn job_status_running() {
        assert_eq!(JobStatus::Running.as_str(), "running");
    }

    #[test]
    fn job_status_completed() {
        assert_eq!(JobStatus::Completed.as_str(), "completed");
    }

    #[test]
    fn job_status_failed() {
        assert_eq!(JobStatus::Failed.as_str(), "failed");
    }

    // ── GitHubComment accessors ─────────────────────────────────────

    #[test]
    fn comment_all_some() {
        let c = GitHubComment {
            id: 1,
            body: Some("hello".to_string()),
            user: Some(GitHubUser {
                login: "alice".to_string(),
            }),
            html_url: Some("https://example.com".to_string()),
            created_at: Some("2024-01-01".to_string()),
            issue_url: Some("https://api.github.com/repos/o/r/issues/1".to_string()),
        };
        assert_eq!(c.body_text(), "hello");
        assert_eq!(c.login(), "alice");
        assert_eq!(c.url(), "https://example.com");
        assert_eq!(c.created_at_str(), "2024-01-01");
        assert_eq!(
            c.issue_url_str(),
            "https://api.github.com/repos/o/r/issues/1"
        );
    }

    #[test]
    fn comment_all_none() {
        let c = GitHubComment {
            id: 2,
            body: None,
            user: None,
            html_url: None,
            created_at: None,
            issue_url: None,
        };
        assert_eq!(c.body_text(), "");
        assert_eq!(c.login(), "");
        assert_eq!(c.url(), "");
        assert_eq!(c.created_at_str(), "");
        assert_eq!(c.issue_url_str(), "");
    }

    #[test]
    fn comment_missing_user() {
        let c = GitHubComment {
            id: 3,
            body: Some("body".to_string()),
            user: None,
            html_url: Some("url".to_string()),
            created_at: Some("ts".to_string()),
            issue_url: Some("iu".to_string()),
        };
        assert_eq!(c.login(), "");
    }
}
