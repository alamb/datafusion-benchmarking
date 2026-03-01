//! Criterion benchmark runner — ports `run_criterion.sh`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::github::GitHubClient;
use crate::runner::config::RunnerConfig;
use crate::runner::git;
use crate::runner::monitor;
use crate::runner::shell;

/// Run a criterion benchmark comparing a PR branch to its merge-base.
pub async fn run(config: &RunnerConfig, gh: &GitHubClient) -> Result<()> {
    let repo_url = config.repo_url();
    let bench_name = &config.bench_name;
    let bench_filter = &config.bench_filter;
    let bench_command_args = bench_command_args(bench_name);

    let branch_dir = PathBuf::from("/workspace/datafusion-branch");
    let base_dir = PathBuf::from("/workspace/datafusion-base");

    // Clone and checkout PR branch
    info!("=== Cloning PR branch ===");
    git::clone_shallow(&repo_url, &branch_dir, 200).await?;
    let branch_name = git::checkout_pr(&config.pr_url, &branch_dir).await?;
    let merge_base = git::merge_base(&branch_dir).await?;
    let branch_base = git::rev_parse_head(&branch_dir).await?;
    let bench_branch_name = git::sanitize_branch_name(&branch_name);

    // Clone and checkout merge-base
    info!("=== Cloning merge-base ===");
    git::clone_shallow(&repo_url, &base_dir, 200).await?;
    git::checkout(&base_dir, &merge_base).await?;

    // Pre-install stable toolchain to avoid rustup race in parallel builds
    git::rustup_stable().await?;

    // Post "running" comment
    let uname = shell::uname().await;
    let bench_command_display = format!("cargo bench --features=parquet --bench {bench_name}");
    let running_body = format!(
        "\u{1f916} Criterion benchmark running (GKE) | [trigger]({})\n\
         `{uname}`\n\
         Comparing {branch_name} ({branch_base}) to {merge_base} \
         [diff](https://github.com/{repo}/compare/{merge_base}..{branch_base})\n\
         BENCH_NAME={bench_name}\n\
         BENCH_COMMAND={bench_command_display}\n\
         BENCH_FILTER={bench_filter}\n\
         Results will be posted here when complete",
        config.comment_url,
        repo = config.repo,
    );
    let pr_number = config.pr_number()?;
    gh.post_comment(&config.repo, pr_number, &running_body)
        .await?;

    // Compile both in parallel
    info!("=== Compiling PR branch and merge-base in parallel ===");
    let mut branch_args = bench_command_args.clone();
    branch_args.push("--no-run".to_string());
    let mut base_args = bench_command_args.clone();
    base_args.push("--no-run".to_string());

    let branch_build = shell::spawn_command(
        "cargo",
        &str_slice(&branch_args),
        &branch_dir,
        "/tmp/branch_build.log",
    );
    let base_build = shell::spawn_command(
        "cargo",
        &str_slice(&base_args),
        &base_dir,
        "/tmp/base_build.log",
    );

    branch_build
        .await
        .context("branch build task panicked")?
        .context("branch build failed")?;
    base_build
        .await
        .context("base build task panicked")?
        .context("base build failed")?;
    info!("=== Compilation complete ===");

    // Run benchmarks sequentially
    info!("=== Running benchmark on merge-base ===");
    let mut base_run_args = bench_command_args.clone();
    base_run_args.extend(["--", "--save-baseline", "main"].map(String::from));
    if !bench_filter.is_empty() {
        base_run_args.push(bench_filter.clone());
    }
    let (_, base_stats) =
        shell::run_command_monitored("cargo", &str_slice(&base_run_args), &base_dir).await?;

    info!("=== Running benchmark on PR branch ===");
    let mut branch_run_args = bench_command_args.clone();
    branch_run_args.extend(["--", "--save-baseline"].map(String::from));
    branch_run_args.push(bench_branch_name.clone());
    if !bench_filter.is_empty() {
        branch_run_args.push(bench_filter.clone());
    }
    let (_, branch_stats) =
        shell::run_command_monitored("cargo", &str_slice(&branch_run_args), &branch_dir).await?;

    // Copy baselines into one target dir for critcmp
    copy_criterion_baselines(&base_dir, &branch_dir).await;

    // Compare and post results
    let report = shell::run_command("critcmp", &["main", &bench_branch_name], &branch_dir)
        .await
        .context("critcmp")?;

    let resource_section = format!(
        "{}\n{}",
        monitor::format_resource_comment("base (merge-base)", &base_stats),
        monitor::format_resource_comment("branch", &branch_stats),
    );
    let result_body = format_result_comment(&config.comment_url, &report, &resource_section);
    gh.post_comment(&config.repo, pr_number, &result_body)
        .await?;

    Ok(())
}

/// Build cargo bench args for criterion.
fn bench_command_args(bench_name: &str) -> Vec<String> {
    vec![
        "bench".to_string(),
        "--features=parquet".to_string(),
        "--bench".to_string(),
        bench_name.to_string(),
    ]
}

/// Copy criterion baselines from base to branch target directory.
async fn copy_criterion_baselines(base_dir: &Path, branch_dir: &Path) {
    let src = base_dir.join("target/criterion");
    let dst = branch_dir.join("target/criterion");
    if src.exists() {
        let _ = shell::run_command(
            "cp",
            &[
                "-r",
                &format!("{}/.", src.to_string_lossy()),
                &dst.to_string_lossy(),
            ],
            Path::new("/"),
        )
        .await;
    }
}

/// Format the result comment body.
fn format_result_comment(comment_url: &str, report: &str, resource_section: &str) -> String {
    format!(
        "\u{1f916} Criterion benchmark completed (GKE) | [trigger]({comment_url})\n\n\
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

/// Convert Vec<String> to a slice of &str for run_command.
fn str_slice(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_args_construction() {
        let args = bench_command_args("sql_planner");
        assert_eq!(
            args,
            vec!["bench", "--features=parquet", "--bench", "sql_planner"]
        );
    }

    #[test]
    fn result_comment_format() {
        let comment = format_result_comment(
            "https://example.com/comment",
            "test report\n",
            "resources\n",
        );
        assert!(comment.contains("Criterion benchmark completed"));
        assert!(comment.contains("[trigger](https://example.com/comment)"));
        assert!(comment.contains("test report"));
        assert!(comment.contains("<details>"));
        assert!(comment.contains("Resource Usage"));
    }
}
