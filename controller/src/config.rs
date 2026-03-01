//! Controller configuration loaded from environment variables.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Per-repo benchmark allowlists loaded from JSON config.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoEntry {
    #[serde(default)]
    pub standard: Vec<String>,
    #[serde(default)]
    pub criterion: Vec<String>,
    /// `"datafusion"` (default) or `"arrow"` — controls criterion `JobType`.
    #[serde(default = "default_criterion_type")]
    pub criterion_type: String,
}

fn default_criterion_type() -> String {
    "datafusion".to_string()
}

impl RepoEntry {
    pub fn standard_set(&self) -> HashSet<&str> {
        self.standard.iter().map(|s| s.as_str()).collect()
    }

    pub fn criterion_set(&self) -> HashSet<&str> {
        self.criterion.iter().map(|s| s.as_str()).collect()
    }
}

/// Top-level benchmark configuration deserialized from the `BENCHMARK_CONFIG` env var.
#[derive(Debug, Clone, Deserialize)]
pub struct BenchmarkConfig {
    pub allowed_users: HashSet<String>,
    pub repos: HashMap<String, RepoEntry>,
}

/// Controller configuration loaded from environment variables.
///
/// ```text
/// Variable                  Default                              Required
/// ─────────────────────────────────────────────────────────────────────────
/// GITHUB_TOKEN              —                                    yes
/// RUNNER_IMAGE              —                                    yes
/// DATABASE_URL              sqlite:///data/benchmark.db           no
/// BENCHMARK_CONFIG           —                                    yes (JSON)
/// POLL_INTERVAL_SECS        5                                     no
/// RECONCILE_INTERVAL_SECS   10                                    no
/// K8S_NAMESPACE             benchmarking                          no
/// DEFAULT_CPU               12                                    no
/// DEFAULT_MEMORY            65Gi                                  no
/// EPHEMERAL_STORAGE         128Gi                                 no
/// ACTIVE_DEADLINE_SECS      7200                                  no
/// TTL_AFTER_FINISHED_SECS   3600                                  no
/// DEFAULT_MACHINE_FAMILY    c4a                                   no
/// STORAGE_CLASS             hyperdisk-balanced                    no
/// SCCACHE_GCS_BUCKET        —                                     no
/// DATA_CACHE_BUCKET          —                                     no
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    pub github_token: String,
    pub database_url: String,
    pub benchmark_config: BenchmarkConfig,
    pub poll_interval_secs: u64,
    pub reconcile_interval_secs: u64,
    pub k8s_namespace: String,
    pub runner_image: String,
    pub default_cpu: String,
    pub default_memory: String,
    /// Ephemeral storage request for benchmark pods (e.g. `"128Gi"`).
    pub ephemeral_storage: String,
    /// Default GCE machine family for benchmark pods (`"c4a"` for ARM, `"c4"` for x86).
    pub default_machine_family: String,
    /// Maximum wall-clock seconds a K8s Job may run before being killed.
    pub active_deadline_secs: i64,
    /// Seconds after completion before the K8s Job object is garbage-collected.
    pub ttl_after_finished_secs: i32,
    /// StorageClass for the ephemeral workspace volume.
    pub storage_class: String,
    /// GCS bucket name for sccache. When set, runner pods get `RUSTC_WRAPPER=sccache`.
    pub sccache_gcs_bucket: Option<String>,
    /// GCS bucket for caching benchmark data (TPC-H, ClickBench, etc.).
    pub data_cache_bucket: Option<String>,
}

impl Config {
    /// Load configuration from environment variables. Fails if required vars are missing.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            github_token: env_required("GITHUB_TOKEN")?,
            database_url: env_or("DATABASE_URL", "sqlite:///data/benchmark.db"),
            benchmark_config: serde_json::from_str(&env_required("BENCHMARK_CONFIG")?)
                .context("failed to parse BENCHMARK_CONFIG JSON")?,
            poll_interval_secs: env_or("POLL_INTERVAL_SECS", "5")
                .parse()
                .context("POLL_INTERVAL_SECS")?,
            reconcile_interval_secs: env_or("RECONCILE_INTERVAL_SECS", "10")
                .parse()
                .context("RECONCILE_INTERVAL_SECS")?,
            k8s_namespace: env_or("K8S_NAMESPACE", "benchmarking"),
            runner_image: env_required("RUNNER_IMAGE")?,
            default_cpu: env_or("DEFAULT_CPU", "12"),
            default_memory: env_or("DEFAULT_MEMORY", "65Gi"),
            ephemeral_storage: env_or("EPHEMERAL_STORAGE", "128Gi"),
            default_machine_family: env_or("DEFAULT_MACHINE_FAMILY", "c4a"),
            active_deadline_secs: env_or("ACTIVE_DEADLINE_SECS", "3600")
                .parse()
                .context("ACTIVE_DEADLINE_SECS")?,
            ttl_after_finished_secs: env_or("TTL_AFTER_FINISHED_SECS", "3600")
                .parse()
                .context("TTL_AFTER_FINISHED_SECS")?,
            storage_class: env_or("STORAGE_CLASS", "hyperdisk-balanced"),
            sccache_gcs_bucket: std::env::var("SCCACHE_GCS_BUCKET").ok(),
            data_cache_bucket: std::env::var("DATA_CACHE_BUCKET").ok(),
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
