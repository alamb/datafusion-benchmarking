//! Criterion benchmark runner — ports `run_criterion.sh`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::github;
use crate::runner::config::RunnerConfig;
use crate::runner::git;
use crate::runner::monitor;
use crate::runner::poster::CommentPoster;
use crate::runner::shell;

/// Run a criterion benchmark comparing a PR branch to its merge-base.
pub async fn run(config: &RunnerConfig, poster: &CommentPoster) -> Result<()> {
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
    let bench_branch_name = git::sanitize_branch_name(&branch_name);

    // If a custom changed ref is specified, checkout that instead of PR head
    if let Some(ref changed_ref) = config.changed_ref {
        info!(changed_ref, "=== Checking out custom changed ref ===");
        git::fetch_pr_ref(&config.pr_url, &branch_dir).await?;
        git::fetch_origin(&branch_dir).await?;
        git::checkout(&branch_dir, changed_ref).await?;
    }

    // Determine baseline: custom ref or merge-base
    let baseline_display: String;
    info!("=== Cloning merge-base ===");
    git::clone_shallow(&repo_url, &base_dir, 200).await?;
    if let Some(ref baseline_ref) = config.baseline_ref {
        info!(baseline_ref, "=== Checking out custom baseline ref ===");
        git::fetch_pr_ref(&config.pr_url, &base_dir).await?;
        git::fetch_origin(&base_dir).await?;
        git::checkout(&base_dir, baseline_ref).await?;
        baseline_display = baseline_ref.clone();
    } else {
        git::checkout(&base_dir, &merge_base).await?;
        baseline_display = merge_base.clone();
    }

    // Pre-install stable toolchain to avoid rustup race in parallel builds
    git::rustup_stable().await?;

    // Set up required benchmark data (e.g. sql_planner needs clickbench_partitioned)
    setup_benchmark_data(bench_name, &branch_dir, &base_dir).await;

    // Post "running" comment
    let uname = shell::uname().await;
    let instance_type = shell::node_instance_type().await;
    let pod_resources = shell::pod_resources();
    let lscpu = shell::lscpu().await;
    let bench_command_display = format!("cargo bench --features=parquet --bench {bench_name}");
    let changed_display = config.changed_ref.as_deref().unwrap_or(&branch_name);
    let changed_sha = git::rev_parse_head(&branch_dir).await?;
    let base_sha = git::rev_parse_head(&base_dir).await?;
    let baseline_label = if config.baseline_ref.is_some() {
        baseline_display.clone()
    } else {
        format!("{} (merge-base)", &base_sha[..7.min(base_sha.len())])
    };
    let footer = github::issues_footer(config.runner_repo_url.as_deref());
    let running_body = format!(
        "\u{1f916} Criterion benchmark running (GKE) | [trigger]({})\n\
         **Instance:** `{instance_type}` ({pod_resources}) | `{uname}`\n\
         <details><summary>CPU Details (lscpu)</summary>\n\n\
         ```\n\
         {lscpu}\n\
         ```\n\n\
         </details>\n\n\
         Comparing {changed_display} ({changed_sha}) to {baseline_label} \
         [diff](https://github.com/{repo}/compare/{base_sha}..{changed_sha})\n\
         BENCH_NAME={bench_name}\n\
         BENCH_COMMAND={bench_command_display}\n\
         BENCH_FILTER={bench_filter}\n\
         Results will be posted here when complete{footer}",
        config.comment_url,
        repo = config.repo,
    );
    let pr_number = config.pr_number()?;
    poster
        .post_comment(&config.repo, pr_number, &running_body)
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
    let baseline_available = match base_build.await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            warn!("Baseline build failed (benchmark may be new): {e:#}");
            false
        }
        Err(e) => {
            warn!("Baseline build task panicked (benchmark may be new): {e:#}");
            false
        }
    };
    info!("=== Compilation complete ===");

    // Run benchmarks sequentially, applying per-side env vars via `env` wrapper
    let base_stats = if baseline_available {
        info!("=== Running benchmark on merge-base ===");
        let mut base_run_args = bench_command_args.clone();
        base_run_args.extend(["--", "--save-baseline", "main"].map(String::from));
        if !bench_filter.is_empty() {
            base_run_args.push(bench_filter.clone());
        }
        let baseline_extra_env = config.baseline_env_args();
        let (_, stats) = if baseline_extra_env.is_empty() {
            shell::run_command_monitored("cargo", &str_slice(&base_run_args), &base_dir, None)
                .await?
        } else {
            let mut env_args: Vec<String> = baseline_extra_env;
            env_args.push("cargo".to_string());
            env_args.extend(base_run_args);
            shell::run_command_monitored("env", &str_slice(&env_args), &base_dir, None).await?
        };
        Some(stats)
    } else {
        info!("=== Skipping merge-base benchmark (baseline build failed) ===");
        None
    };

    info!("=== Running benchmark on PR branch ===");
    let mut branch_run_args = bench_command_args.clone();
    branch_run_args.extend(["--", "--save-baseline"].map(String::from));
    branch_run_args.push(bench_branch_name.clone());
    if !bench_filter.is_empty() {
        branch_run_args.push(bench_filter.clone());
    }
    let changed_extra_env = config.changed_env_args();
    let (_, branch_stats) = if changed_extra_env.is_empty() {
        shell::run_command_monitored("cargo", &str_slice(&branch_run_args), &branch_dir, None)
            .await?
    } else {
        let mut env_args: Vec<String> = changed_extra_env;
        env_args.push("cargo".to_string());
        env_args.extend(branch_run_args);
        shell::run_command_monitored("env", &str_slice(&env_args), &branch_dir, None).await?
    };

    // Compare and post results
    let result_body = if baseline_available {
        // Copy baselines into one target dir for critcmp
        copy_criterion_baselines(&base_dir, &branch_dir).await;

        let report = shell::run_command("critcmp", &["main", &bench_branch_name], &branch_dir)
            .await
            .context("critcmp")?;

        let resource_section = format!(
            "{}\n{}",
            monitor::format_resource_comment("base (merge-base)", &base_stats.unwrap()),
            monitor::format_resource_comment("branch", &branch_stats),
        );
        format_result_comment(
            &config.comment_url,
            &report,
            &resource_section,
            &instance_type,
            &pod_resources,
            &lscpu,
            &footer,
        )
    } else {
        let report = shell::run_command("critcmp", &[bench_branch_name.as_str()], &branch_dir)
            .await
            .context("critcmp")?;

        let resource_section =
            monitor::format_resource_comment("branch", &branch_stats).to_string();
        format_branch_only_result_comment(
            &config.comment_url,
            &report,
            &resource_section,
            &instance_type,
            &pod_resources,
            &lscpu,
            &footer,
        )
    };
    poster
        .post_comment(&config.repo, pr_number, &result_body)
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
fn format_result_comment(
    comment_url: &str,
    report: &str,
    resource_section: &str,
    instance_type: &str,
    pod_resources: &str,
    lscpu: &str,
    footer: &str,
) -> String {
    format!(
        "\u{1f916} Criterion benchmark completed (GKE) | [trigger]({comment_url})\n\n\
         **Instance:** `{instance_type}` ({pod_resources})\n\n\
         <details><summary>CPU Details (lscpu)</summary>\n\n\
         ```\n\
         {lscpu}\n\
         ```\n\n\
         </details>\n\n\
         <details><summary>Details</summary>\n\
         <p>\n\n\
         ```\n\
         {report}\
         ```\n\n\
         </p>\n\
         </details>\n\n\
         <details><summary>Resource Usage</summary>\n\n\
         {resource_section}\
         </details>\n\
         {footer}"
    )
}

/// Format the result comment body for branch-only runs (no baseline comparison).
fn format_branch_only_result_comment(
    comment_url: &str,
    report: &str,
    resource_section: &str,
    instance_type: &str,
    pod_resources: &str,
    lscpu: &str,
    footer: &str,
) -> String {
    format!(
        "\u{1f916} Criterion benchmark completed (GKE) | [trigger]({comment_url})\n\n\
         **Instance:** `{instance_type}` ({pod_resources})\n\n\
         <details><summary>CPU Details (lscpu)</summary>\n\n\
         ```\n\
         {lscpu}\n\
         ```\n\n\
         </details>\n\n\
         **New benchmark — branch-only results (no baseline comparison)**\n\n\
         <details><summary>Details</summary>\n\
         <p>\n\n\
         ```\n\
         {report}\
         ```\n\n\
         </p>\n\
         </details>\n\n\
         <details><summary>Resource Usage</summary>\n\n\
         {resource_section}\
         </details>\n\
         {footer}"
    )
}

/// Return the dataset names required by a given criterion benchmark.
fn required_datasets(bench_name: &str) -> &'static [&'static str] {
    match bench_name {
        "sql_planner" => &["clickbench_partitioned"],
        _ => &[],
    }
}

/// Set up benchmark data for criterion benchmarks that need it.
///
/// Generates data once in the branch directory and symlinks it into the base
/// directory so both sides share the same input data.
async fn setup_benchmark_data(bench_name: &str, branch_dir: &Path, base_dir: &Path) {
    let datasets = required_datasets(bench_name);
    if datasets.is_empty() {
        return;
    }

    let branch_benchmarks = branch_dir.join("benchmarks");
    let bench_dir_str = branch_benchmarks.to_string_lossy().to_string();

    for dataset in datasets {
        info!("Setting up data for {dataset}");
        cache_data(dataset, &bench_dir_str).await;
    }

    // Symlink the data directory into the base checkout so both sides can
    // find it without duplicating storage.
    let branch_data = branch_benchmarks.join("data");
    let base_data = base_dir.join("benchmarks/data");
    if branch_data.exists() && !base_data.exists() {
        info!("Symlinking benchmark data into base directory");
        let _ = tokio::fs::symlink(&branch_data, &base_data).await;
    }
}

/// Run data generation with cache support via /scripts/cache_data.sh.
async fn cache_data(bench: &str, bench_dir: &str) {
    let cache_script = Path::new("/scripts/cache_data.sh");
    if cache_script.exists() {
        let _ = shell::run_command(
            cache_script.to_str().unwrap(),
            &[bench, bench_dir],
            Path::new(bench_dir),
        )
        .await;
    } else {
        let _ = shell::run_command("./bench.sh", &["data", bench], Path::new(bench_dir)).await;
    }
}

/// Convert Vec<String> to a slice of &str for run_command.
fn str_slice(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_datasets_sql_planner() {
        assert_eq!(
            required_datasets("sql_planner"),
            &["clickbench_partitioned"]
        );
    }

    #[test]
    fn required_datasets_unknown_returns_empty() {
        assert!(required_datasets("unknown_bench").is_empty());
    }

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
            "c4a-standard-48",
            "12 vCPU / 65 GiB",
            "lscpu output",
            "",
        );
        assert!(comment.contains("Criterion benchmark completed"));
        assert!(comment.contains("[trigger](https://example.com/comment)"));
        assert!(comment.contains("test report"));
        assert!(comment.contains("<details>"));
        assert!(comment.contains("Resource Usage"));
        assert!(comment.contains("c4a-standard-48"));
        assert!(comment.contains("12 vCPU / 65 GiB"));
        assert!(comment.contains("lscpu output"));
    }

    #[test]
    fn branch_only_result_comment_format() {
        let comment = format_branch_only_result_comment(
            "https://example.com/comment",
            "branch report\n",
            "branch resources\n",
            "c4a-standard-48",
            "12 vCPU / 65 GiB",
            "lscpu output",
            "",
        );
        assert!(comment.contains("Criterion benchmark completed"));
        assert!(comment.contains("New benchmark — branch-only results"));
        assert!(comment.contains("[trigger](https://example.com/comment)"));
        assert!(comment.contains("branch report"));
        assert!(comment.contains("Resource Usage"));
        assert!(comment.contains("branch resources"));
        assert!(comment.contains("c4a-standard-48"));
        assert!(comment.contains("12 vCPU / 65 GiB"));
        assert!(comment.contains("lscpu output"));
    }
}
