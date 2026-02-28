//! Benchmark controller entry point.
//!
//! Spawns two long-running tasks — the GitHub comment poller and the K8s Job
//! reconciler — and exits if either panics.

mod benchmarks;
mod config;
mod db;
mod github;
mod github_poller;
mod job_manager;
mod models;

use anyhow::Result;
use tracing::info;

/// Entry point. Spawns two long-running tasks:
///
/// - **GitHub poller** — scans PR comments for benchmark triggers
/// - **Job reconciler** — creates K8s Jobs for pending work, monitors active ones
///
/// Exits if either task panics.
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = config::Config::from_env()?;
    info!(
        repos = ?config.watched_repos,
        poll_interval = config.poll_interval_secs,
        "starting benchmark controller"
    );

    let pool = db::connect(&config.database_url).await?;
    info!("database connected");

    let gh = github::GitHubClient::new(&config.github_token);

    let poller = tokio::spawn(github_poller::poll_loop(
        config.clone(),
        pool.clone(),
        gh.clone(),
    ));

    let reconciler = tokio::spawn(job_manager::reconcile_loop(
        config.clone(),
        pool.clone(),
        gh.clone(),
    ));

    info!("controller running");

    tokio::select! {
        r = poller => { r?; }
        r = reconciler => { r?; }
    }

    Ok(())
}
