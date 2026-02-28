use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub github_token: String,
    pub database_url: String,
    pub watched_repos: Vec<String>,
    pub poll_interval_secs: u64,
    pub reconcile_interval_secs: u64,
    pub k8s_namespace: String,
    pub runner_image: String,
    pub default_cpu: String,
    pub default_memory: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            github_token: env_required("GITHUB_TOKEN")?,
            database_url: env_or("DATABASE_URL", "sqlite:///data/benchmark.db"),
            watched_repos: env_or("WATCHED_REPOS", "apache/datafusion:apache/arrow-rs")
                .split(':')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            poll_interval_secs: env_or("POLL_INTERVAL_SECS", "30")
                .parse()
                .context("POLL_INTERVAL_SECS")?,
            reconcile_interval_secs: env_or("RECONCILE_INTERVAL_SECS", "10")
                .parse()
                .context("RECONCILE_INTERVAL_SECS")?,
            k8s_namespace: env_or("K8S_NAMESPACE", "benchmarking"),
            runner_image: env_required("RUNNER_IMAGE")?,
            default_cpu: env_or("DEFAULT_CPU", "30"),
            default_memory: env_or("DEFAULT_MEMORY", "60Gi"),
        })
    }
}

fn env_required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
