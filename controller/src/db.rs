use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

use crate::models::BenchmarkJob;

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;

    // Run migrations
    sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
        .execute(&pool)
        .await?;

    Ok(pool)
}

pub async fn is_comment_seen(pool: &SqlitePool, comment_id: i64) -> Result<bool> {
    let row =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM seen_comments WHERE comment_id = ?")
            .bind(comment_id)
            .fetch_one(pool)
            .await?;
    Ok(row > 0)
}

pub async fn mark_comment_seen(
    pool: &SqlitePool,
    comment_id: i64,
    repo: &str,
    pr_number: i64,
    login: &str,
    created_at: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO seen_comments (comment_id, repo, pr_number, login, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(comment_id)
    .bind(repo)
    .bind(pr_number)
    .bind(login)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_job(
    pool: &SqlitePool,
    comment_id: i64,
    repo: &str,
    pr_number: i64,
    pr_url: &str,
    login: &str,
    benchmarks: &str,
    env_vars: &str,
    job_type: &str,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO benchmark_jobs (comment_id, repo, pr_number, pr_url, login, benchmarks, env_vars, job_type) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(comment_id)
    .bind(repo)
    .bind(pr_number)
    .bind(pr_url)
    .bind(login)
    .bind(benchmarks)
    .bind(env_vars)
    .bind(job_type)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

pub async fn get_pending_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status = 'pending' ORDER BY id LIMIT 5",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

pub async fn get_active_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status IN ('creating', 'running') ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

pub async fn update_job_status(
    pool: &SqlitePool,
    job_id: i64,
    status: &str,
    k8s_job_name: Option<&str>,
    error_message: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE benchmark_jobs SET status = ?, k8s_job_name = COALESCE(?, k8s_job_name), \
         error_message = COALESCE(?, error_message), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(status)
    .bind(k8s_job_name)
    .bind(error_message)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_last_scan(pool: &SqlitePool, repo: &str) -> Result<Option<String>> {
    let row = sqlx::query_scalar::<_, String>("SELECT last_scan_at FROM scan_state WHERE repo = ?")
        .bind(repo)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn set_last_scan(pool: &SqlitePool, repo: &str, timestamp: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO scan_state (repo, last_scan_at) VALUES (?, ?) \
         ON CONFLICT(repo) DO UPDATE SET last_scan_at = excluded.last_scan_at",
    )
    .bind(repo)
    .bind(timestamp)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_queue_summary(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status NOT IN ('completed', 'failed') ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}
