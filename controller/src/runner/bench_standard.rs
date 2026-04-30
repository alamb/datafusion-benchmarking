//! Standard bench.sh benchmark runner — ports `run_bench_sh.sh`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::github;
use crate::runner::config::RunnerConfig;
use crate::runner::git;
use crate::runner::monitor::{self, ResourceStats};
use crate::runner::poster::CommentPoster;
use crate::runner::shell;

/// Run standard bench.sh benchmarks comparing a PR branch to its merge-base.
pub async fn run(config: &RunnerConfig, poster: &CommentPoster) -> Result<()> {
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
    let instance_type = shell::node_instance_type().await;
    let pod_resources = shell::pod_resources();
    let lscpu = shell::lscpu().await;
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

    let footer = github::issues_footer(config.runner_repo_url.as_deref());
    let running_body = format!(
        "\u{1f916} Benchmark running (GKE) | [trigger]({})\n\
         **Instance:** `{instance_type}` ({pod_resources}) | `{uname}`\n\
         <details><summary>CPU Details (lscpu)</summary>\n\n\
         ```\n\
         {lscpu}\n\
         ```\n\n\
         </details>\n\n\
         Comparing {changed_display} ({changed_sha}) to {baseline_label} \
         [diff](https://github.com/{repo}/compare/{base_sha}..{changed_sha}) \
         using: {benchmarks}\n\
         Results will be posted here when complete{footer}",
        config.comment_url,
        repo = config.repo,
    );
    poster
        .post_comment(&config.repo, pr_number, &running_body)
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

    // Explicit RESULTS_NAME ensures bench.sh saves to a predictable directory,
    // regardless of whether DATAFUSION_DIR is on a branch or detached HEAD.
    let base_results_name = "HEAD".to_string();
    let bench_branch_name = git::sanitize_branch_name(&branch_name);

    for bench in benchmarks.split_whitespace() {
        info!("** Creating data if needed for {bench} **");
        cache_data(bench, &bench_dir_str).await;

        info!("** Running {bench} baseline **");
        let base_spill_dir = PathBuf::from(format!("/workspace/spill-base-{bench}"));
        let _ = tokio::fs::create_dir_all(&base_spill_dir).await;
        let base_stats = run_one_side(
            bench,
            &base_dir,
            &bench_benchmarks,
            &base_results_name,
            &base_spill_dir,
            &baseline_extra_env,
        )
        .await
        .with_context(|| format!("run {bench} (base)"))?;
        let _ = tokio::fs::remove_dir_all(&base_spill_dir).await;
        base_stats_list.push((bench, base_stats));

        info!("** Running {bench} branch **");
        let branch_spill_dir = PathBuf::from(format!("/workspace/spill-branch-{bench}"));
        let _ = tokio::fs::create_dir_all(&branch_spill_dir).await;
        let branch_stats = run_one_side(
            bench,
            &branch_dir,
            &bench_benchmarks,
            &bench_branch_name,
            &branch_spill_dir,
            &changed_extra_env,
        )
        .await
        .with_context(|| format!("run {bench} (branch)"))?;
        let _ = tokio::fs::remove_dir_all(&branch_spill_dir).await;
        branch_stats_list.push((bench, branch_stats));
    }

    // Compare and post results
    let report = shell::run_command(
        "./bench.sh",
        &["compare_detail", &base_results_name, &bench_branch_name],
        &bench_benchmarks,
    )
    .await
    .context("bench.sh compare")?;

    let resource_section = format_resource_section(&base_stats_list, &branch_stats_list);
    let result_body = format_result_comment(
        &config.comment_url,
        &report,
        &resource_section,
        &instance_type,
        &pod_resources,
        &lscpu,
        &footer,
    );
    poster
        .post_comment(&config.repo, pr_number, &result_body)
        .await?;

    Ok(())
}

/// Run a single benchmark on one side (base or branch).
///
/// For TPC-H variants we bypass `bench.sh run` and invoke the prebuilt `dfbench`
/// binary directly. Upstream PR apache/datafusion#21707 ported `bench.sh`'s
/// `run_tpch` to a Criterion-based SQL harness whose data paths are relative
/// to `${DATAFUSION_DIR}/benchmarks` and whose timings live under
/// `target/criterion/`, neither of which fits this controller's layout (data
/// is in `bench_dir/benchmarks/data`, comparisons read JSON from
/// `bench_dir/benchmarks/results/`). The `dfbench tpch` subcommand still
/// exists upstream, so we call it directly with the same args the old
/// `run_tpch` used. Other benchmarks continue through `bench.sh`.
async fn run_one_side(
    bench: &str,
    side_dir: &Path,
    bench_benchmarks: &Path,
    results_name: &str,
    spill_dir: &Path,
    extra_env: &[String],
) -> Result<ResourceStats> {
    if let Some((tpch_args, results_filename)) = tpch_direct_args(bench) {
        run_tpch_direct(
            side_dir,
            bench_benchmarks,
            results_name,
            spill_dir,
            extra_env,
            tpch_args,
            results_filename,
        )
        .await
    } else {
        let mut args: Vec<String> = vec![
            format!("DATAFUSION_DIR={}", side_dir.display()),
            format!("RESULTS_NAME={results_name}"),
            format!("DATAFUSION_RUNTIME_TEMP_DIRECTORY={}", spill_dir.display()),
        ];
        args.extend(extra_env.iter().cloned());
        args.extend([
            "./bench.sh".to_string(),
            "run".to_string(),
            bench.to_string(),
        ]);
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let (_, stats) = shell::run_command_monitored(
            "env",
            &args_ref,
            bench_benchmarks,
            Some(spill_dir.to_path_buf()),
        )
        .await?;
        Ok(stats)
    }
}

/// Map a TPC-H bench name to (dfbench tpch args, results JSON filename).
/// Returns `None` for non-TPC-H benchmarks.
fn tpch_direct_args(bench: &str) -> Option<(Vec<String>, String)> {
    let (sf, in_mem) = match bench {
        "tpch" => ("1", false),
        "tpch10" => ("10", false),
        "tpch_mem" => ("1", true),
        "tpch_mem10" => ("10", true),
        _ => return None,
    };
    let mut args: Vec<String> = vec![
        "tpch".into(),
        "--iterations".into(),
        "5".into(),
        "--scale-factor".into(),
        sf.into(),
        "--format".into(),
        "parquet".into(),
        "--prefer_hash_join".into(),
        "true".into(),
    ];
    if in_mem {
        args.push("-m".into());
    }
    let results_filename = if in_mem {
        format!("tpch_mem_sf{sf}.json")
    } else {
        format!("tpch_sf{sf}.json")
    };
    Some((args, results_filename))
}

#[allow(clippy::too_many_arguments)]
async fn run_tpch_direct(
    side_dir: &Path,
    bench_benchmarks: &Path,
    results_name: &str,
    spill_dir: &Path,
    extra_env: &[String],
    tpch_args: Vec<String>,
    results_filename: String,
) -> Result<ResourceStats> {
    let dfbench = side_dir.join("target/release/dfbench");
    if !dfbench.exists() {
        anyhow::bail!("dfbench binary not found at {}", dfbench.display());
    }

    // bench.sh writes results under SCRIPT_DIR/results/<RESULTS_NAME>; mimic that.
    let results_dir = bench_benchmarks.join("results").join(results_name);
    tokio::fs::create_dir_all(&results_dir)
        .await
        .with_context(|| format!("creating results dir {}", results_dir.display()))?;
    let results_file = results_dir.join(&results_filename);

    // Data lives in bench_benchmarks/data/tpch_sf<SF> (created by `bench.sh data tpch`).
    // Locate the right data dir from the args we built (--scale-factor SF).
    let sf = tpch_args
        .iter()
        .skip_while(|a| a.as_str() != "--scale-factor")
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("missing --scale-factor in tpch args"))?;
    let data_path = bench_benchmarks.join(format!("data/tpch_sf{sf}"));

    let mut env_args: Vec<String> = vec![format!(
        "DATAFUSION_RUNTIME_TEMP_DIRECTORY={}",
        spill_dir.display()
    )];
    env_args.extend(extra_env.iter().cloned());
    env_args.push(dfbench.to_string_lossy().into_owned());
    env_args.extend(tpch_args);
    env_args.extend([
        "--path".to_string(),
        data_path.to_string_lossy().into_owned(),
        "-o".to_string(),
        results_file.to_string_lossy().into_owned(),
    ]);

    let env_args_ref: Vec<&str> = env_args.iter().map(|s| s.as_str()).collect();
    let (_, stats) = shell::run_command_monitored(
        "env",
        &env_args_ref,
        bench_benchmarks,
        Some(spill_dir.to_path_buf()),
    )
    .await?;
    Ok(stats)
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
        "\u{1f916} Benchmark completed (GKE) | [trigger]({comment_url})\n\n\
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(comment.contains("Benchmark completed"));
        assert!(comment.contains("[trigger](https://example.com/comment)"));
        assert!(comment.contains("test report"));
        assert!(comment.contains("<details>"));
        assert!(comment.contains("Resource Usage"));
        assert!(comment.contains("resources"));
        assert!(comment.contains("c4a-standard-48"));
        assert!(comment.contains("12 vCPU / 65 GiB"));
        assert!(comment.contains("lscpu output"));
    }

    #[test]
    fn tpch_direct_args_maps_variants() {
        let (args, results) = tpch_direct_args("tpch").unwrap();
        assert!(args.contains(&"--scale-factor".to_string()));
        assert!(args.contains(&"1".to_string()));
        assert!(!args.contains(&"-m".to_string()));
        assert_eq!(results, "tpch_sf1.json");

        let (args, results) = tpch_direct_args("tpch10").unwrap();
        assert!(args.contains(&"10".to_string()));
        assert!(!args.contains(&"-m".to_string()));
        assert_eq!(results, "tpch_sf10.json");

        let (args, results) = tpch_direct_args("tpch_mem").unwrap();
        assert!(args.contains(&"-m".to_string()));
        assert_eq!(results, "tpch_mem_sf1.json");

        let (args, results) = tpch_direct_args("tpch_mem10").unwrap();
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"10".to_string()));
        assert_eq!(results, "tpch_mem_sf10.json");

        assert!(tpch_direct_args("clickbench_1").is_none());
        assert!(tpch_direct_args("topk_tpch").is_none());
    }
}
