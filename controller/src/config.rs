//! Controller configuration loaded from environment variables.

use anyhow::{Context, Result};

/// Controller configuration loaded from environment variables.
///
/// ```text
/// Variable                  Default                              Required
/// ─────────────────────────────────────────────────────────────────────────
/// GITHUB_TOKEN              —                                    yes
/// RUNNER_IMAGE              —                                    yes
/// DATABASE_URL              sqlite:///data/benchmark.db           no
/// WATCHED_REPOS             pydantic/datafusion                   no
/// POLL_INTERVAL_SECS        5                                     no
/// RECONCILE_INTERVAL_SECS   10                                    no
/// K8S_NAMESPACE             benchmarking                          no
/// DEFAULT_CPU               30                                    no
/// DEFAULT_MEMORY            60Gi                                  no
/// EPHEMERAL_STORAGE         100Gi                                 no
/// ACTIVE_DEADLINE_SECS      7200                                  no
/// TTL_AFTER_FINISHED_SECS   3600                                  no
/// STORAGE_CLASS             hyperdisk-balanced                    no
/// ```
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
    /// Ephemeral storage request for benchmark pods (e.g. `"100Gi"`).
    pub ephemeral_storage: String,
    /// Maximum wall-clock seconds a K8s Job may run before being killed.
    pub active_deadline_secs: i64,
    /// Seconds after completion before the K8s Job object is garbage-collected.
    pub ttl_after_finished_secs: i32,
    /// StorageClass for the ephemeral workspace volume.
    pub storage_class: String,
}

impl Config {
    /// Load configuration from environment variables. Fails if required vars are missing.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            github_token: env_required("GITHUB_TOKEN")?,
            database_url: env_or("DATABASE_URL", "sqlite:///data/benchmark.db"),
            watched_repos: env_or("WATCHED_REPOS", "pydantic/datafusion")
                .split(':')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            poll_interval_secs: env_or("POLL_INTERVAL_SECS", "5")
                .parse()
                .context("POLL_INTERVAL_SECS")?,
            reconcile_interval_secs: env_or("RECONCILE_INTERVAL_SECS", "10")
                .parse()
                .context("RECONCILE_INTERVAL_SECS")?,
            k8s_namespace: env_or("K8S_NAMESPACE", "benchmarking"),
            runner_image: env_required("RUNNER_IMAGE")?,
            default_cpu: env_or("DEFAULT_CPU", "30"),
            default_memory: env_or("DEFAULT_MEMORY", "60Gi"),
            ephemeral_storage: env_or("EPHEMERAL_STORAGE", "100Gi"),
            active_deadline_secs: env_or("ACTIVE_DEADLINE_SECS", "7200")
                .parse()
                .context("ACTIVE_DEADLINE_SECS")?,
            ttl_after_finished_secs: env_or("TTL_AFTER_FINISHED_SECS", "3600")
                .parse()
                .context("TTL_AFTER_FINISHED_SECS")?,
            storage_class: env_or("STORAGE_CLASS", "hyperdisk-balanced"),
        })
    }
}

/// Read a required environment variable, returning an error if missing.
fn env_required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

/// Read an environment variable with a fallback default.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use unique env var names to avoid conflicts with parallel tests.

    #[test]
    fn env_or_returns_default_when_unset() {
        let val = env_or("__TEST_UNSET_VAR_12345__", "fallback");
        assert_eq!(val, "fallback");
    }

    #[test]
    fn env_or_returns_value_when_set() {
        std::env::set_var("__TEST_ENV_OR_SET__", "hello");
        let val = env_or("__TEST_ENV_OR_SET__", "fallback");
        assert_eq!(val, "hello");
        std::env::remove_var("__TEST_ENV_OR_SET__");
    }

    #[test]
    fn env_required_errors_when_unset() {
        assert!(env_required("__TEST_REQUIRED_MISSING__").is_err());
    }

    #[test]
    fn env_required_returns_value_when_set() {
        std::env::set_var("__TEST_REQUIRED_SET__", "world");
        let val = env_required("__TEST_REQUIRED_SET__").unwrap();
        assert_eq!(val, "world");
        std::env::remove_var("__TEST_REQUIRED_SET__");
    }
}
