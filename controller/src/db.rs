//! SQLite persistence layer.
//!
//! Manages benchmark jobs, seen-comment deduplication, and per-repo scan
//! timestamps. All queries use the [`sqlx`] async SQLite driver.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

use crate::models::{BenchmarkJob, JobInsert, JobStatus};

/// Open (or create) the SQLite database and run migrations. Uses WAL journal mode.
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

/// Check if a GitHub comment ID has already been processed.
pub async fn is_comment_seen(pool: &SqlitePool, comment_id: i64) -> Result<bool> {
    let row =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM seen_comments WHERE comment_id = ?")
            .bind(comment_id)
            .fetch_one(pool)
            .await?;
    Ok(row > 0)
}

/// Record a comment ID so it won't be processed again (INSERT OR IGNORE).
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

/// Insert a new benchmark job with status `pending`. Returns the new row ID.
pub async fn insert_job(pool: &SqlitePool, job: &JobInsert<'_>) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO benchmark_jobs (comment_id, repo, pr_number, pr_url, login, benchmarks, env_vars, job_type) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job.comment_id)
    .bind(job.repo)
    .bind(job.pr_number)
    .bind(job.pr_url)
    .bind(job.login)
    .bind(job.benchmarks)
    .bind(job.env_vars)
    .bind(job.job_type)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Return up to 5 oldest `pending` jobs, ordered by ID.
pub async fn get_pending_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status = 'pending' ORDER BY id LIMIT 5",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

/// Return all jobs with status `running`.
pub async fn get_active_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status = 'running' ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

/// Transition a job's status. Optionally sets `k8s_job_name` and `error_message`
/// (uses COALESCE to keep existing values when `None`).
pub async fn update_job_status(
    pool: &SqlitePool,
    job_id: i64,
    status: JobStatus,
    k8s_job_name: Option<&str>,
    error_message: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE benchmark_jobs SET status = ?, k8s_job_name = COALESCE(?, k8s_job_name), \
         error_message = COALESCE(?, error_message), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(k8s_job_name)
    .bind(error_message)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get the ISO 8601 timestamp of the last successful comment scan for a repo.
pub async fn get_last_scan(pool: &SqlitePool, repo: &str) -> Result<Option<String>> {
    let row = sqlx::query_scalar::<_, String>("SELECT last_scan_at FROM scan_state WHERE repo = ?")
        .bind(repo)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Upsert the last scan timestamp for a repo.
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

/// Number of days to retain old seen comments and terminal jobs.
const RETENTION_DAYS: i64 = 30;

/// Delete seen comments older than `retention_days`. Returns the number of rows removed.
pub async fn cleanup_seen_comments(pool: &SqlitePool, retention_days: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM seen_comments WHERE processed_at < datetime('now', '-' || ? || ' days')",
    )
    .bind(retention_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete terminal (`completed`/`failed`) jobs older than `retention_days`. Returns the number of rows removed.
pub async fn cleanup_old_jobs(pool: &SqlitePool, retention_days: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM benchmark_jobs WHERE status IN ('completed', 'failed') \
         AND updated_at < datetime('now', '-' || ? || ' days')",
    )
    .bind(retention_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Run all cleanup tasks with the default retention period. Returns (comments_deleted, jobs_deleted).
pub async fn run_cleanup(pool: &SqlitePool) -> Result<(u64, u64)> {
    let comments = cleanup_seen_comments(pool, RETENTION_DAYS).await?;
    let jobs = cleanup_old_jobs(pool, RETENTION_DAYS).await?;
    Ok((comments, jobs))
}

/// Return all non-terminal jobs (not `completed` or `failed`) for the queue display.
pub async fn get_queue_summary(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs WHERE status NOT IN ('completed', 'failed') ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        connect("sqlite::memory:").await.unwrap()
    }

    fn test_job(comment_id: i64) -> JobInsert<'static> {
        JobInsert {
            comment_id,
            repo: "apache/datafusion",
            pr_number: 42,
            pr_url: "https://github.com/apache/datafusion/pull/42",
            login: "alice",
            benchmarks: "[\"tpch\"]",
            env_vars: "[]",
            job_type: "standard",
        }
    }

    // ── mark_comment_seen + is_comment_seen ─────────────────────────

    #[tokio::test]
    async fn comment_seen_lifecycle() {
        let pool = test_pool().await;

        assert!(!is_comment_seen(&pool, 1).await.unwrap());

        mark_comment_seen(&pool, 1, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        assert!(is_comment_seen(&pool, 1).await.unwrap());

        // Idempotent (INSERT OR IGNORE)
        mark_comment_seen(&pool, 1, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        assert!(is_comment_seen(&pool, 1).await.unwrap());
    }

    // ── insert_job + get_pending_jobs ───────────────────────────────

    #[tokio::test]
    async fn insert_and_get_pending() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 100, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();

        let id = insert_job(&pool, &test_job(100)).await.unwrap();
        assert!(id > 0);

        let pending = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].repo, "apache/datafusion");
        assert_eq!(pending[0].pr_number, 42);
        assert_eq!(pending[0].login, "alice");
        assert_eq!(pending[0].status, "pending");
    }

    #[tokio::test]
    async fn get_pending_limit_five() {
        let pool = test_pool().await;
        for i in 0..6 {
            let cid = 200 + i;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            insert_job(&pool, &test_job(cid)).await.unwrap();
        }
        let pending = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(pending.len(), 5);
    }

    // ── update_job_status + get_active_jobs ─────────────────────────

    #[tokio::test]
    async fn update_to_running_shows_active() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 300, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let id = insert_job(&pool, &test_job(300)).await.unwrap();

        update_job_status(&pool, id, JobStatus::Running, Some("k8s-bench-1"), None)
            .await
            .unwrap();

        let active = get_active_jobs(&pool).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].k8s_job_name.as_deref(), Some("k8s-bench-1"));
    }

    #[tokio::test]
    async fn completed_not_in_active() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 400, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let id = insert_job(&pool, &test_job(400)).await.unwrap();

        update_job_status(&pool, id, JobStatus::Running, None, None)
            .await
            .unwrap();
        update_job_status(&pool, id, JobStatus::Completed, None, None)
            .await
            .unwrap();

        let active = get_active_jobs(&pool).await.unwrap();
        assert!(active.is_empty());
    }

    // ── COALESCE behavior ───────────────────────────────────────────

    #[tokio::test]
    async fn coalesce_preserves_k8s_name() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 500, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let id = insert_job(&pool, &test_job(500)).await.unwrap();

        update_job_status(&pool, id, JobStatus::Running, Some("my-job"), None)
            .await
            .unwrap();
        // Update status with None k8s_job_name — should preserve "my-job"
        update_job_status(&pool, id, JobStatus::Completed, None, None)
            .await
            .unwrap();

        let jobs = sqlx::query_as::<_, BenchmarkJob>("SELECT * FROM benchmark_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(jobs.k8s_job_name.as_deref(), Some("my-job"));
    }

    // ── get_queue_summary ───────────────────────────────────────────

    #[tokio::test]
    async fn queue_summary_excludes_terminal() {
        let pool = test_pool().await;

        for i in 0..3 {
            let cid = 600 + i;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();

            match i {
                1 => {
                    update_job_status(&pool, id, JobStatus::Running, None, None)
                        .await
                        .unwrap();
                }
                2 => {
                    update_job_status(&pool, id, JobStatus::Completed, None, None)
                        .await
                        .unwrap();
                }
                _ => {} // stays pending
            }
        }

        let summary = get_queue_summary(&pool).await.unwrap();
        // pending + running visible, completed excluded
        assert_eq!(summary.len(), 2);
    }

    // ── set_last_scan + get_last_scan ───────────────────────────────

    // ── cleanup_seen_comments ────────────────────────────────────

    #[tokio::test]
    async fn cleanup_seen_comments_deletes_old_keeps_recent() {
        let pool = test_pool().await;

        // Insert a comment, then backdate its processed_at to 60 days ago
        mark_comment_seen(&pool, 700, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        sqlx::query("UPDATE seen_comments SET processed_at = datetime('now', '-60 days') WHERE comment_id = 700")
            .execute(&pool)
            .await
            .unwrap();

        // Insert a recent comment
        mark_comment_seen(&pool, 701, "apache/datafusion", 43, "bob", "2024-06-01")
            .await
            .unwrap();

        let deleted = cleanup_seen_comments(&pool, 30).await.unwrap();
        assert_eq!(deleted, 1);

        // Recent one still exists
        assert!(is_comment_seen(&pool, 701).await.unwrap());
        // Old one is gone
        assert!(!is_comment_seen(&pool, 700).await.unwrap());
    }

    // ── cleanup_old_jobs ────────────────────────────────────────

    #[tokio::test]
    async fn cleanup_old_jobs_deletes_terminal_keeps_active() {
        let pool = test_pool().await;

        // Create three jobs: completed (old), failed (old), pending (old)
        let statuses = ["completed", "failed", "pending"];
        for (i, status_str) in statuses.iter().enumerate() {
            let cid = 800 + i as i64;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();
            match *status_str {
                "completed" => {
                    update_job_status(&pool, id, JobStatus::Completed, None, None)
                        .await
                        .unwrap();
                }
                "failed" => {
                    update_job_status(&pool, id, JobStatus::Failed, None, None)
                        .await
                        .unwrap();
                }
                _ => {} // stays pending
            }
            // Backdate updated_at to 60 days ago
            sqlx::query("UPDATE benchmark_jobs SET updated_at = datetime('now', '-60 days') WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }

        // Also create an old running job
        mark_comment_seen(&pool, 803, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let running_id = insert_job(&pool, &test_job(803)).await.unwrap();
        update_job_status(&pool, running_id, JobStatus::Running, None, None)
            .await
            .unwrap();
        sqlx::query("UPDATE benchmark_jobs SET updated_at = datetime('now', '-60 days') WHERE id = ?")
            .bind(running_id)
            .execute(&pool)
            .await
            .unwrap();

        let deleted = cleanup_old_jobs(&pool, 30).await.unwrap();
        // Only completed and failed should be deleted
        assert_eq!(deleted, 2);

        // Pending and running jobs should still exist
        let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benchmark_jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 2);
    }

    // ── set_last_scan + get_last_scan ───────────────────────────

    #[tokio::test]
    async fn last_scan_lifecycle() {
        let pool = test_pool().await;

        assert!(get_last_scan(&pool, "apache/datafusion")
            .await
            .unwrap()
            .is_none());

        set_last_scan(&pool, "apache/datafusion", "2024-01-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            get_last_scan(&pool, "apache/datafusion")
                .await
                .unwrap()
                .as_deref(),
            Some("2024-01-01T00:00:00Z")
        );

        // Upsert overwrites
        set_last_scan(&pool, "apache/datafusion", "2024-06-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            get_last_scan(&pool, "apache/datafusion")
                .await
                .unwrap()
                .as_deref(),
            Some("2024-06-01T00:00:00Z")
        );
    }
}
