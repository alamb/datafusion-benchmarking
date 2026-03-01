//! Command execution helpers for the benchmark runner.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::info;

use crate::runner::monitor::{CgroupMonitor, ResourceStats};

/// Path to the shared output log that captures all runner output.
pub const OUTPUT_FILE: &str = "/tmp/benchmark_output.txt";

/// Run a command, log it, stream output to the log file, and return stdout as a string.
/// Fails if the command exits with a non-zero status.
pub async fn run_command(cmd: &str, args: &[&str], cwd: &Path) -> Result<String> {
    info!(cmd, ?args, ?cwd, "running command");

    let output = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to spawn: {cmd} {}", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Append to output log
    append_to_log(&stdout).await;
    append_to_log(&stderr).await;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        anyhow::bail!(
            "{cmd} {} exited with code {code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            args.join(" ")
        );
    }

    Ok(stdout)
}

/// Run a command with cgroup resource monitoring. Returns both stdout and resource stats.
pub async fn run_command_monitored(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
) -> Result<(String, ResourceStats)> {
    let monitor = CgroupMonitor::start();
    let output = run_command(cmd, args, cwd).await?;
    let stats = monitor.finish().await;
    Ok((output, stats))
}

/// Spawn a command in the background, returning a JoinHandle that resolves to the Result.
/// Output is redirected to a log file at `log_path`.
pub fn spawn_command(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    log_path: &str,
) -> tokio::task::JoinHandle<Result<()>> {
    let cmd = cmd.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let cwd = cwd.to_path_buf();
    let log_path = log_path.to_string();

    tokio::spawn(async move {
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("failed to create log file: {log_path}"))?;

        let status = Command::new(&cmd)
            .args(&args)
            .current_dir(&cwd)
            .stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file))
            .status()
            .await
            .with_context(|| format!("failed to spawn: {cmd} {}", args.join(" ")))?;

        if !status.success() {
            let log_content = tokio::fs::read_to_string(&log_path).await.unwrap_or_default();
            append_to_log(&log_content).await;
            let code = status.code().unwrap_or(-1);
            anyhow::bail!(
                "{cmd} {} exited with code {code}\n{log_content}",
                args.join(" ")
            );
        }

        Ok(())
    })
}

/// Append text to the shared output log file.
pub async fn append_to_log(text: &str) {
    if text.is_empty() {
        return;
    }
    use tokio::io::AsyncWriteExt;
    if let Ok(mut f) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(OUTPUT_FILE)
        .await
    {
        let _ = f.write_all(text.as_bytes()).await;
    }
}

/// Read the last N lines from the output log file.
pub async fn tail_log(n: usize) -> String {
    match tokio::fs::read_to_string(OUTPUT_FILE).await {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

/// Get the system uname string.
pub async fn uname() -> String {
    match Command::new("uname").arg("-a").output().await {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    }
}

/// Log sccache stats if sccache is in use.
pub async fn log_sccache_stats() {
    if std::env::var("RUSTC_WRAPPER").is_ok() {
        let _ = run_command("sccache", &["--show-stats"], Path::new("/")).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_file_path() {
        assert_eq!(OUTPUT_FILE, "/tmp/benchmark_output.txt");
    }

    #[tokio::test]
    async fn tail_log_empty_file() {
        let result = tail_log(20).await;
        let _ = result;
    }

    #[tokio::test]
    async fn uname_returns_something() {
        let result = uname().await;
        assert!(!result.is_empty());
    }

    #[tokio::test]
    async fn run_command_echo() {
        let output = run_command("echo", &["hello"], Path::new("/tmp"))
            .await
            .unwrap();
        assert_eq!(output.trim(), "hello");
    }

    #[tokio::test]
    async fn run_command_failure() {
        let result = run_command("false", &[], Path::new("/tmp")).await;
        assert!(result.is_err());
    }
}
