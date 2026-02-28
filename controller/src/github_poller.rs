//! GitHub comment poller.
//!
//! Periodically fetches new PR comments from watched repositories, detects
//! benchmark trigger phrases, and inserts corresponding jobs into SQLite.

use anyhow::Result;
use chrono::{Duration, Utc};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::benchmarks::{
    allowed_users_markdown, detect_benchmark, is_benchmark_trigger, is_queue_request,
    supported_benchmarks_message, RepoConfig, ALLOWED_USERS,
};
use crate::config::Config;
use crate::db;
use crate::github::GitHubClient;
use crate::models::{GitHubComment, JobInsert};

/// Infinite loop that polls GitHub for new PR comments on each watched repo.
///
/// ```text
/// ┌──────────────────────────────────────────────┐
/// │  poll_loop (every POLL_INTERVAL_SECS)        │
/// │  ┌────────────────────────────────────────┐  │
/// │  │ for each repo in WATCHED_REPOS         │  │
/// │  │   fetch comments since last_scan       │  │
/// │  │   for each unseen comment              │  │
/// │  │     "show benchmark queue" → reply      │  │
/// │  │     "run benchmark X"     → insert job  │  │
/// │  │   update last_scan                     │  │
/// │  └────────────────────────────────────────┘  │
/// └──────────────────────────────────────────────┘
/// ```
pub async fn poll_loop(config: Config, pool: SqlitePool, gh: GitHubClient) {
    let interval = tokio::time::Duration::from_secs(config.poll_interval_secs);
    loop {
        for repo in &config.watched_repos {
            if let Err(e) = poll_repo(&pool, &gh, repo).await {
                warn!(repo, error = %e, "poll error");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Fetch and process recent comments for a single repo.
async fn poll_repo(pool: &SqlitePool, gh: &GitHubClient, repo: &str) -> Result<()> {
    let repo_cfg = match RepoConfig::for_repo(repo) {
        Some(c) => c,
        None => {
            warn!(repo, "unknown repo, skipping");
            return Ok(());
        }
    };

    let since = match db::get_last_scan(pool, repo).await? {
        Some(ts) => ts,
        None => {
            let default = Utc::now() - Duration::hours(1);
            default.format("%Y-%m-%dT%H:%M:%SZ").to_string()
        }
    };

    let comments = gh.fetch_recent_comments(repo, &since).await?;
    info!(repo, count = comments.len(), "fetched comments");

    for comment in &comments {
        if let Err(e) = process_comment(pool, gh, &repo_cfg, comment).await {
            warn!(comment_id = comment.id, error = %e, "process comment error");
        }
    }

    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    db::set_last_scan(pool, repo, &now).await?;

    Ok(())
}

/// Build the "not allowed" reply posted when a non-whitelisted user triggers a benchmark.
fn not_allowed_message(login: &str, comment_url: &str) -> String {
    format!(
        "Hi @{login}, thanks for the request ({comment_url}). \
         Only whitelisted users can trigger benchmarks. \
         Allowed users: {}.",
        allowed_users_markdown()
    )
}

/// Handle a single comment: skip if seen, detect triggers, insert jobs.
async fn process_comment(
    pool: &SqlitePool,
    gh: &GitHubClient,
    repo_cfg: &RepoConfig,
    comment: &GitHubComment,
) -> Result<()> {
    if db::is_comment_seen(pool, comment.id).await? {
        return Ok(());
    }

    let body = comment.body_text();
    let login = comment.login();
    let comment_url = comment.url();
    let created_at = comment.created_at_str();
    let issue_url = comment.issue_url_str();

    let Some(pr_number) = pr_number_from_url(issue_url) else {
        return Ok(());
    };

    // Mark seen unconditionally — we already bailed if previously seen.
    db::mark_comment_seen(
        pool,
        comment.id,
        &repo_cfg.repo,
        pr_number,
        login,
        created_at,
    )
    .await?;

    // Handle queue requests
    if is_queue_request(body) {
        info!(pr_number, login, "queue request");
        let jobs = db::get_queue_summary(pool).await?;
        let msg = format_queue_message(login, comment_url, &jobs);
        gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
        return Ok(());
    }

    // Try to detect benchmark trigger
    let Some(request) = detect_benchmark(repo_cfg, body) else {
        // Check if it looks like a failed trigger attempt
        if is_benchmark_trigger(body) {
            if !ALLOWED_USERS.contains(login) {
                let msg = not_allowed_message(login, comment_url);
                gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
            } else {
                let requested: Vec<String> = body
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .skip(2)
                    .map(|s| s.to_string())
                    .collect();
                let msg = format!(
                    "Hi @{login}, thanks for the request ({comment_url}).\n\n{}",
                    supported_benchmarks_message(repo_cfg, &requested)
                );
                gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
            }
        }
        return Ok(());
    };

    // User must be allowed
    if !ALLOWED_USERS.contains(login) {
        let msg = not_allowed_message(login, comment_url);
        gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
        return Ok(());
    }

    info!(pr_number, login, benchmarks = ?request.benchmarks, "scheduling benchmark");

    let pr_url = format!("https://github.com/{}/pull/{}", repo_cfg.repo, pr_number);
    let benchmarks_json = serde_json::to_string(&request.benchmarks)?;
    let env_vars_json = serde_json::to_string(&request.env_vars)?;

    // Determine job type(s) and insert jobs
    if request.benchmarks.is_empty() {
        // Default suite — standard job
        db::insert_job(
            pool,
            &JobInsert {
                comment_id: comment.id,
                repo: &repo_cfg.repo,
                pr_number,
                pr_url: &pr_url,
                login,
                benchmarks: &benchmarks_json,
                env_vars: &env_vars_json,
                job_type: "standard",
            },
        )
        .await?;
    } else {
        // Group benchmarks by type
        for bench in &request.benchmarks {
            let job_type = repo_cfg
                .classify_benchmark(bench)
                .map(|jt| jt.as_str())
                .unwrap_or("standard");

            let single_bench = serde_json::to_string(&[bench])?;
            db::insert_job(
                pool,
                &JobInsert {
                    comment_id: comment.id,
                    repo: &repo_cfg.repo,
                    pr_number,
                    pr_url: &pr_url,
                    login,
                    benchmarks: &single_bench,
                    env_vars: &env_vars_json,
                    job_type,
                },
            )
            .await?;
        }
    }

    // React with rocket
    if let Err(e) = gh.post_reaction(&repo_cfg.repo, comment.id, "rocket").await {
        warn!(error = %e, "failed to post reaction");
    }

    Ok(())
}

/// Extract the PR/issue number from a GitHub `issue_url` (last path segment).
/// Returns `None` if the URL doesn't end with a numeric segment.
fn pr_number_from_url(url: &str) -> Option<i64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|s| s.parse().ok())
}

/// Build a markdown table of pending/active jobs for a "show benchmark queue" reply.
fn format_queue_message(
    login: &str,
    comment_url: &str,
    jobs: &[crate::models::BenchmarkJob],
) -> String {
    let mut lines = vec![format!(
        "Hi @{login}, you asked to view the benchmark queue ({comment_url}).\n"
    )];

    if jobs.is_empty() {
        lines.push("No pending jobs.".to_string());
    } else {
        lines.push("| ID | Repo | PR | User | Benchmarks | Status |".to_string());
        lines.push("| --- | --- | --- | --- | --- | --- |".to_string());
        for job in jobs {
            lines.push(format!(
                "| {} | {} | #{} | {} | {} | {} |",
                job.id, job.repo, job.pr_number, job.login, job.benchmarks, job.status
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BenchmarkJob;

    // ── pr_number_from_url ──────────────────────────────────────────

    #[test]
    fn pr_number_standard_url() {
        let url = "https://api.github.com/repos/apache/datafusion/issues/42";
        assert_eq!(pr_number_from_url(url), Some(42));
    }

    #[test]
    fn pr_number_trailing_slash() {
        let url = "https://api.github.com/repos/apache/datafusion/issues/42/";
        assert_eq!(pr_number_from_url(url), Some(42));
    }

    #[test]
    fn pr_number_empty() {
        assert_eq!(pr_number_from_url(""), None);
    }

    #[test]
    fn pr_number_not_a_number() {
        assert_eq!(pr_number_from_url("https://example.com/not-a-number"), None);
    }

    #[test]
    fn pr_number_trailing_slash_only() {
        assert_eq!(pr_number_from_url("https://example.com/"), None);
    }

    // ── not_allowed_message ─────────────────────────────────────────

    #[test]
    fn not_allowed_msg_contains_fields() {
        let msg = not_allowed_message("testuser", "https://example.com/comment/1");
        assert!(msg.contains("@testuser"));
        assert!(msg.contains("https://example.com/comment/1"));
        assert!(msg.contains("whitelisted") || msg.contains("Whitelisted"));
    }

    // ── format_queue_message ────────────────────────────────────────

    #[test]
    fn format_queue_empty() {
        let msg = format_queue_message("alice", "https://example.com/c/1", &[]);
        assert!(msg.contains("No pending jobs."));
    }

    #[test]
    fn format_queue_with_jobs() {
        let job = BenchmarkJob {
            id: 1,
            comment_id: 100,
            repo: "apache/datafusion".to_string(),
            pr_number: 42,
            pr_url: "https://github.com/apache/datafusion/pull/42".to_string(),
            login: "alice".to_string(),
            benchmarks: "[\"tpch\"]".to_string(),
            env_vars: "[]".to_string(),
            job_type: "standard".to_string(),
            cpu_request: None,
            memory_request: None,
            cpu_arch: None,
            k8s_job_name: None,
            status: "pending".to_string(),
            error_message: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let msg = format_queue_message("bob", "https://example.com/c/2", &[job]);
        assert!(msg.contains("| ID |"));
        assert!(msg.contains("| 1 |"));
        assert!(msg.contains("apache/datafusion"));
        assert!(msg.contains("#42"));
    }
}
