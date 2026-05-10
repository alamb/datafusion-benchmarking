//! Benchmark runner entry point.
//!
//! Parses environment variables, dispatches to the appropriate benchmark
//! workflow, and posts error comments on failure.

use anyhow::Result;
use tracing::{error, info};

use benchmark_controller::github;
use benchmark_controller::runner::config::{BenchType, RunnerConfig};
use benchmark_controller::runner::poster::CommentPoster;
use benchmark_controller::runner::{bench_arrow, bench_criterion, bench_standard, shell};

#[tokio::main]
async fn main() {
    // Initialize tracing
    let logfire = logfire::configure()
        .with_service_name("benchmark-runner")
        .send_to_logfire(logfire::config::SendToLogfire::IfTokenPresent)
        .with_console(Some(logfire::config::ConsoleOptions::default()))
        .finish()
        .expect("failed to configure tracing");
    let _logfire_guard = logfire.shutdown_guard();

    // Initialize the output log file
    let _ = tokio::fs::write(shell::OUTPUT_FILE, b"").await;

    // Parse config
    let config = match RunnerConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to parse runner config");
            std::process::exit(1);
        }
    };

    // Set up sccache if configured
    config.setup_sccache();

    info!(
        bench_type = ?config.bench_type,
        pr_url = %config.pr_url,
        benchmarks = %config.benchmarks,
        "starting benchmark runner"
    );

    let poster = config.build_poster();

    if let Err(e) = run_benchmark(&config, &poster).await {
        error!(error = %e, "benchmark failed");
        post_error_comment(&config, &poster).await;
        std::process::exit(1);
    }

    // Log sccache stats if enabled
    shell::log_sccache_stats().await;
}

async fn run_benchmark(config: &RunnerConfig, poster: &CommentPoster) -> Result<()> {
    match config.bench_type {
        BenchType::Standard | BenchType::MainTracking => bench_standard::run(config, poster).await,
        BenchType::Criterion => bench_criterion::run(config, poster).await,
        BenchType::ArrowCriterion => bench_arrow::run(config, poster).await,
    }
}

async fn post_error_comment(config: &RunnerConfig, poster: &CommentPoster) {
    let tail = shell::tail_log(20).await;

    let footer = github::issues_footer(config.runner_repo_url.as_deref());
    let body = format!(
        "Benchmark for [this request]({}) failed.\n\n\
         Last 20 lines of output:\n\
         <details><summary>Click to expand</summary>\n\n\
         ```\n\
         {tail}\n\
         ```\n\n\
         </details>{footer}",
        config.comment_url,
    );

    let pr_number = match config.pr_number() {
        Ok(n) => n,
        Err(e) => {
            error!(error = %e, "cannot post error comment: failed to parse PR number");
            return;
        }
    };

    if let Err(e) = poster.post_comment(&config.repo, pr_number, &body).await {
        error!(error = %e, "failed to post error comment");
    }
}
