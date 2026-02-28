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

async fn process_comment(
    pool: &SqlitePool,
    gh: &GitHubClient,
    repo_cfg: &RepoConfig,
    comment: &GitHubComment,
) -> Result<()> {
    if db::is_comment_seen(pool, comment.id).await? {
        return Ok(());
    }

    let body = comment.body.as_deref().unwrap_or("");
    let login = comment
        .user
        .as_ref()
        .map(|u| u.login.as_str())
        .unwrap_or("");
    let comment_url = comment.html_url.as_deref().unwrap_or("");
    let created_at = comment.created_at.as_deref().unwrap_or("");
    let issue_url = comment.issue_url.as_deref().unwrap_or("");

    let pr_number = pr_number_from_url(issue_url);
    if pr_number == 0 {
        return Ok(());
    }

    // Handle queue requests
    if is_queue_request(body) {
        info!(pr_number, login, "queue request");
        db::mark_comment_seen(
            pool,
            comment.id,
            &repo_cfg.repo,
            pr_number,
            login,
            created_at,
        )
        .await?;

        let jobs = db::get_queue_summary(pool).await?;
        let msg = format_queue_message(login, comment_url, &jobs);
        gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
        return Ok(());
    }

    // Try to detect benchmark trigger
    let request = detect_benchmark(repo_cfg, body);

    if request.is_none() {
        // Check if it looks like a failed trigger attempt
        if is_benchmark_trigger(body) {
            db::mark_comment_seen(
                pool,
                comment.id,
                &repo_cfg.repo,
                pr_number,
                login,
                created_at,
            )
            .await?;

            if !ALLOWED_USERS.contains(login) {
                let msg = format!(
                    "Hi @{login}, thanks for the request ({comment_url}). \
                     Only whitelisted users can trigger benchmarks. \
                     Allowed users: {}.",
                    allowed_users_markdown()
                );
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
    }

    let request = request.unwrap();

    // User must be allowed
    if !ALLOWED_USERS.contains(login) {
        db::mark_comment_seen(
            pool,
            comment.id,
            &repo_cfg.repo,
            pr_number,
            login,
            created_at,
        )
        .await?;
        let msg = format!(
            "Hi @{login}, thanks for the request ({comment_url}). \
             Only whitelisted users can trigger benchmarks. Allowed users: {}.",
            allowed_users_markdown()
        );
        gh.post_comment(&repo_cfg.repo, pr_number, &msg).await?;
        return Ok(());
    }

    info!(pr_number, login, benchmarks = ?request.benchmarks, "scheduling benchmark");

    db::mark_comment_seen(
        pool,
        comment.id,
        &repo_cfg.repo,
        pr_number,
        login,
        created_at,
    )
    .await?;

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

fn pr_number_from_url(url: &str) -> i64 {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

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
