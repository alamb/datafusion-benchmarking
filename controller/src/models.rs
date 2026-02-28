//! Domain types shared across the controller.
//!
//! Includes database row structs, GitHub API response types, and enums for
//! job status and job type.

use serde::Deserialize;

/// Fields for inserting a new benchmark job into SQLite.
pub struct JobInsert<'a> {
    pub comment_id: i64,
    pub repo: &'a str,
    pub pr_number: i64,
    pub pr_url: &'a str,
    pub login: &'a str,
    pub benchmarks: &'a str,
    pub env_vars: &'a str,
    pub job_type: &'a str,
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
    pub job_type: String,
    pub cpu_request: Option<String>,
    pub memory_request: Option<String>,
    pub cpu_arch: Option<String>,
    pub k8s_job_name: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Parsed user intent from a PR comment (e.g. `run benchmark tpch_mem`).
/// Empty `benchmarks` means "run the default suite".
#[derive(Debug, Clone)]
pub struct BenchmarkRequest {
    pub benchmarks: Vec<String>,
    pub env_vars: Vec<String>,
}

/// Benchmark runner variant.
///
/// - `Standard` — shell-based DataFusion benchmarks (tpch, clickbench, etc.)
/// - `Criterion` — `cargo bench` criterion benchmarks in DataFusion
/// - `ArrowCriterion` — `cargo bench` criterion benchmarks in arrow-rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    Standard,
    Criterion,
    ArrowCriterion,
}

impl JobType {
    /// Returns the string stored in the `job_type` SQLite column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Criterion => "criterion",
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
