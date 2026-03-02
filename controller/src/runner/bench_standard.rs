//! Standard bench.sh benchmark runner — ports `run_bench_sh.sh`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::github::GitHubClient;
use crate::runner::config::RunnerConfig;
use crate::runner::git;
use crate::runner::monitor::{self, ResourceStats};
use crate::runner::shell;

/// Run standard bench.sh benchmarks comparing a PR branch to its merge-base.
pub async fn run(config: &RunnerConfig, gh: &GitHubClient) -> Result<()> {
    let repo_url = config.repo_url();
    let benchmarks = &config.benchmarks;

    let branch_dir = PathBuf::from("/workspace/datafusion-branch");
    let base_dir = PathBuf::from("/workspace/datafusion-base");
    let bench_dir = PathBuf::from("/workspace/datafusion-bench");

    // Clone and checkout PR branch
    info!("=== Cloning PR branch ===");
    git::clone_shallow(&repo_url, &branch_dir, 200).await?;
    let branch_name = git::checkout_pr(&config.pr_url, &branch_dir).await?;
    let merge_base = git::merge_base(&branch_dir).await?;

    // If a custom changed ref is specified, checkout that instead of PR head
    if let Some(ref changed_ref) = config.changed_ref {
        info!(changed_ref, "=== Checking out custom changed ref ===");
        git::fetch_origin(&branch_dir).await?;
        git::checkout(&branch_dir, changed_ref).await?;
    }

    // Determine baseline: custom ref or merge-base
    let baseline_display: String;
    info!("=== Cloning merge-base ===");
    git::clone_shallow(&repo_url, &base_dir, 200).await?;
    if let Some(ref baseline_ref) = config.baseline_ref {
        info!(baseline_ref, "=== Checking out custom baseline ref ===");
        git::fetch_origin(&base_dir).await?;
        git::checkout(&base_dir, baseline_ref).await?;
        baseline_display = baseline_ref.clone();
    } else {
        git::checkout(&base_dir, &merge_base).await?;
        baseline_display = merge_base.clone();
    }

    // Pre-install stable toolchain to avoid rustup race in parallel builds
    git::rustup_stable().await?;

    // Compile both in parallel
    info!("=== Compiling PR branch and merge-base in parallel ===");
    let branch_benchmarks = branch_dir.join("benchmarks");
    let branch_build = shell::spawn_command(
        "cargo",
        &["build", "--release", "--bin", "dfbench"],
        &branch_benchmarks,
        "/tmp/branch_build.log",
    );

    let base_benchmarks = base_dir.join("benchmarks");
    let base_build = shell::spawn_command(
        "cargo",
        &["build", "--release", "--bin", "dfbench"],
        &base_benchmarks,
        "/tmp/base_build.log",
    );

    // Post "running" comment
    let uname = shell::uname().await;
    let pr_number = config.pr_number()?;

    // Resolve display names for the comparison
    let changed_display = config.changed_ref.as_deref().unwrap_or(&branch_name);
    let changed_sha = git::rev_parse_head(&branch_dir).await?;
    let base_sha = git::rev_parse_head(&base_dir).await?;
    let baseline_label = if config.baseline_ref.is_some() {
        baseline_display.clone()
    } else {
        format!("{} (merge-base)", &base_sha[..7.min(base_sha.len())])
    };

    let running_body = format!(
        "\u{1f916} Benchmark running (GKE) | [trigger]({})\n\
         `{uname}`\n\
         Comparing {changed_display} ({changed_sha}) to {baseline_label} \
         [diff](https://github.com/{repo}/compare/{base_sha}..{changed_sha}) \
         using: {benchmarks}\n\
         Results will be posted here when complete",
        config.comment_url,
        repo = config.repo,
    );
    gh.post_comment(&config.repo, pr_number, &running_body)
        .await?;

    // Wait for builds
    info!("=== Waiting for builds ===");
    branch_build
        .await
        .context("branch build task panicked")?
        .context("branch build failed")?;
    base_build
        .await
        .context("base build task panicked")?
        .context("base build failed")?;
    info!("=== Builds complete ===");

    // Set up bench runner from a third checkout
    info!("=== Setting up bench runner ===");
    git::clone_shallow(&repo_url, &bench_dir, 200).await?;
    git::checkout(&bench_dir, "origin/main").await?;

    let bench_benchmarks = bench_dir.join("benchmarks");

    // Clean any prior results
    let results_dir = bench_benchmarks.join("results");
    if results_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&results_dir).await;
    }

    // Copy TPC-H expected answer files so bench.sh skips the docker-based copy
    copy_tpch_answers(&bench_benchmarks).await;

    // Run each benchmark
    let mut base_stats_list: Vec<(&str, ResourceStats)> = Vec::new();
    let mut branch_stats_list: Vec<(&str, ResourceStats)> = Vec::new();

    let bench_dir_str = bench_benchmarks.to_string_lossy().to_string();

    let baseline_extra_env = config.baseline_env_args();
    let changed_extra_env = config.changed_env_args();

    for bench in benchmarks.split_whitespace() {
        info!("** Creating data if needed for {bench} **");
        cache_data(bench, &bench_dir_str).await;

        info!("** Running {bench} baseline **");
        let base_dir_str = base_dir.to_string_lossy().to_string();
        let mut base_args: Vec<String> = vec![format!("DATAFUSION_DIR={base_dir_str}")];
        base_args.extend(baseline_extra_env.iter().cloned());
        base_args.extend([
            "./bench.sh".to_string(),
            "run".to_string(),
            bench.to_string(),
        ]);
        let base_args_ref: Vec<&str> = base_args.iter().map(|s| s.as_str()).collect();
        let (_, base_stats) =
            shell::run_command_monitored("env", &base_args_ref, &bench_benchmarks)
                .await
                .with_context(|| format!("bench.sh run {bench} (base)"))?;
        base_stats_list.push((bench, base_stats));

        info!("** Running {bench} branch **");
        let branch_dir_str = branch_dir.to_string_lossy().to_string();
        let mut branch_args: Vec<String> = vec![format!("DATAFUSION_DIR={branch_dir_str}")];
        branch_args.extend(changed_extra_env.iter().cloned());
        branch_args.extend([
            "./bench.sh".to_string(),
            "run".to_string(),
            bench.to_string(),
        ]);
        let branch_args_ref: Vec<&str> = branch_args.iter().map(|s| s.as_str()).collect();
        let (_, branch_stats) =
            shell::run_command_monitored("env", &branch_args_ref, &bench_benchmarks)
                .await
                .with_context(|| format!("bench.sh run {bench} (branch)"))?;
        branch_stats_list.push((bench, branch_stats));
    }

    // Compare and post results
    let bench_branch_name = git::sanitize_branch_name(&branch_name);
    let report = shell::run_command(
        "./bench.sh",
        &["compare_detail", "HEAD", &bench_branch_name],
        &bench_benchmarks,
    )
    .await
    .context("bench.sh compare")?;

    let resource_section = format_resource_section(&base_stats_list, &branch_stats_list);
    let result_body = format_result_comment(&config.comment_url, &report, &resource_section);
    gh.post_comment(&config.repo, pr_number, &result_body)
        .await?;

    Ok(())
}

/// Copy TPC-H answer files from the baked-in location into the benchmark data dirs.
async fn copy_tpch_answers(bench_dir: &Path) {
    let answers_src = Path::new("/data/tpch-answers");
    if !answers_src.exists() {
        return;
    }
    for sf in &["1", "10"] {
        let dest = bench_dir.join(format!("data/tpch_sf{sf}/answers"));
        let _ = tokio::fs::create_dir_all(&dest).await;
        let _ = shell::run_command(
            "cp",
            &[
                "-r",
                &format!("{}/.", answers_src.to_string_lossy()),
                &dest.to_string_lossy(),
            ],
            Path::new("/"),
        )
        .await;
    }
}

/// Run data generation with cache support via /scripts/cache_data.sh.
async fn cache_data(bench: &str, bench_dir: &str) {
    let cache_script = Path::new("/scripts/cache_data.sh");
    if cache_script.exists() {
        let _ = shell::run_command(
            "/scripts/cache_data.sh",
            &[bench, bench_dir],
            Path::new(bench_dir),
        )
        .await;
    } else {
        // Fallback: run bench.sh data directly
        let _ = shell::run_command("./bench.sh", &["data", bench], Path::new(bench_dir)).await;
    }
}

/// Build the resource usage section from collected stats.
fn format_resource_section(
    base_stats: &[(&str, ResourceStats)],
    branch_stats: &[(&str, ResourceStats)],
) -> String {
    let mut section = String::new();
    for (bench, stats) in base_stats {
        section.push_str(&monitor::format_resource_comment(
            &format!("{bench} \u{2014} base (merge-base)"),
            stats,
        ));
        section.push('\n');
    }
    for (bench, stats) in branch_stats {
        section.push_str(&monitor::format_resource_comment(
            &format!("{bench} \u{2014} branch"),
            stats,
        ));
        section.push('\n');
    }
    section
}

/// Format the result comment body.
fn format_result_comment(comment_url: &str, report: &str, resource_section: &str) -> String {
    format!(
        "\u{1f916} Benchmark completed (GKE) | [trigger]({comment_url})\n\n\
         <details><summary>Details</summary>\n\
         <p>\n\n\
         ```\n\
         {report}\
         ```\n\n\
         </p>\n\
         </details>\n\n\
         <details><summary>Resource Usage</summary>\n\n\
         {resource_section}\
         </details>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_comment_format() {
        let comment = format_result_comment(
            "https://example.com/comment",
            "test report\n",
            "resources\n",
        );
        assert!(comment.contains("Benchmark completed"));
        assert!(comment.contains("[trigger](https://example.com/comment)"));
        assert!(comment.contains("test report"));
        assert!(comment.contains("<details>"));
        assert!(comment.contains("Resource Usage"));
        assert!(comment.contains("resources"));
    }
}
