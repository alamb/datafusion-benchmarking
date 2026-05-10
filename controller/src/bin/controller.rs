//! Benchmark controller entry point.
//!
//! Spawns two long-running tasks — the GitHub comment poller and the K8s Job
//! reconciler — and exits gracefully on SIGTERM/SIGINT.

use anyhow::Result;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use benchmark_controller::{config, db, github, github_poller, health, job_manager};

#[tokio::main]
async fn main() -> Result<()> {
    let logfire = logfire::configure()
        .with_service_name("benchmark-controller")
        .send_to_logfire(logfire::config::SendToLogfire::IfTokenPresent)
        .with_console(Some(logfire::config::ConsoleOptions::default()))
        .finish()?;
    let _logfire_guard = logfire.shutdown_guard();

    let config = config::Config::from_env()?;
    let watched: Vec<&String> = config.benchmark_config.repos.keys().collect();
    info!(
        repos = ?watched,
        poll_interval = config.poll_interval_secs,
        "starting benchmark controller"
    );

    let pool = db::connect(&config.database_url).await?;
    info!("database connected");

    let gh = github::GitHubClient::new(&config.github_token);

    let token = CancellationToken::new();
    let ready = health::ready_flag();

    let health_handle = tokio::spawn(health::serve(
        token.clone(),
        ready.clone(),
        pool.clone(),
        gh.clone(),
    ));

    let poller = tokio::spawn(github_poller::poll_loop(
        config.clone(),
        pool.clone(),
        gh.clone(),
        token.clone(),
    ));

    let reconciler = tokio::spawn(job_manager::reconcile_loop(
        config.clone(),
        pool.clone(),
        gh.clone(),
        token.clone(),
    ));

    let cleanup = tokio::spawn({
        let pool = pool.clone();
        let token = token.clone();
        let poll_interval_secs = config.poll_interval_secs;
        async move {
            let interval = tokio::time::Duration::from_secs(3600);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = token.cancelled() => {
                        info!("cleanup task shutting down");
                        break;
                    }
                }
                match db::run_cleanup(&pool, poll_interval_secs).await {
                    Ok((comments, jobs)) => {
                        if comments > 0 || jobs > 0 {
                            info!(comments, jobs, "cleanup: deleted old rows");
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "cleanup failed");
                    }
                }
            }
        }
    });

    ready.store(true, Ordering::Relaxed);
    info!("controller running");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received shutdown signal");
        }
        r = poller => {
            error!("poller exited: {:?}", r);
        }
        r = reconciler => {
            match r {
                Ok(Err(e)) => error!("reconciler failed: {e:#}"),
                r => error!("reconciler exited: {r:?}"),
            }
        }
        r = cleanup => {
            error!("cleanup exited: {:?}", r);
        }
    }

    info!("shutting down, waiting for tasks to finish");
    token.cancel();

    // Give the health server a moment to stop
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(5), health_handle).await;

    Ok(())
}
