use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeenComment {
    pub comment_id: i64,
    pub repo: String,
    pub pr_number: i64,
    pub login: String,
    pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
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

#[derive(Debug, Clone)]
pub struct BenchmarkRequest {
    pub benchmarks: Vec<String>,
    pub env_vars: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    Standard,
    Criterion,
    ArrowCriterion,
}

impl JobType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Criterion => "criterion",
            Self::ArrowCriterion => "arrow_criterion",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubComment {
    pub id: i64,
    pub body: Option<String>,
    pub user: Option<GitHubUser>,
    pub html_url: Option<String>,
    pub created_at: Option<String>,
    pub issue_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}
