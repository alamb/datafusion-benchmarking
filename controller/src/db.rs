//! SQLite persistence layer.
//!
//! Manages benchmark jobs, seen-comment deduplication, and per-repo scan
//! timestamps. All queries use the [`sqlx`] async SQLite driver.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

use crate::config::MAX_RUNNING_PER_USER;
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
    sqlx::migrate!("./migrations").run(&pool).await?;

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
#[tracing::instrument(skip_all, fields(pr_number = job.pr_number, job_type = job.job_type))]
pub async fn insert_job(pool: &SqlitePool, job: &JobInsert<'_>) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO benchmark_jobs \
         (comment_id, repo, pr_number, pr_url, login, benchmarks, env_vars, \
          baseline_env_vars, changed_env_vars, baseline_ref, changed_ref, job_type) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job.comment_id)
    .bind(job.repo)
    .bind(job.pr_number)
    .bind(job.pr_url)
    .bind(job.login)
    .bind(job.benchmarks)
    .bind(job.env_vars)
    .bind(job.baseline_env_vars)
    .bind(job.changed_env_vars)
    .bind(job.baseline_ref)
    .bind(job.changed_ref)
    .bind(job.job_type)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Return up to 5 oldest `pending` jobs, ordered by ID. Skips jobs whose
/// author already has `MAX_RUNNING_PER_USER` running jobs — those stay
/// pending until an earlier run finishes.
#[tracing::instrument(skip(pool))]
pub async fn get_pending_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs p \
         WHERE p.status = 'pending' \
           AND (SELECT COUNT(*) FROM benchmark_jobs r \
                WHERE r.login = p.login AND r.status = 'running') < ? \
         ORDER BY p.id LIMIT 5",
    )
    .bind(MAX_RUNNING_PER_USER)
    .fetch_all(pool)
    .await?;
    Ok(jobs)
}

/// Count a user's currently-pending benchmark jobs. Used to enforce the
/// per-user queued-jobs cap at ingestion time.
pub async fn count_user_pending(pool: &SqlitePool, login: &str) -> Result<i64> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM benchmark_jobs WHERE login = ? AND status = 'pending'",
    )
    .bind(login)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Store the per-job runner token on a benchmark row.
pub async fn set_runner_token(pool: &SqlitePool, job_id: i64, token: &str) -> Result<()> {
    sqlx::query(
        "UPDATE benchmark_jobs SET runner_token = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(token)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Look up a running job by id and return its stored runner token, repo, and
/// PR number. Returns `None` if the job doesn't exist.
pub async fn get_job_for_comment(
    pool: &SqlitePool,
    job_id: i64,
) -> Result<Option<(String, i64, String, Option<String>)>> {
    let row = sqlx::query_as::<_, (String, i64, String, Option<String>)>(
        "SELECT repo, pr_number, status, runner_token FROM benchmark_jobs WHERE id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
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
#[tracing::instrument(skip(pool, k8s_job_name, error_message))]
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

/// Number of days to retain old benchmark jobs.
const JOB_RETENTION_DAYS: i64 = 30;

/// Delete seen comments that are older than the oldest `last_scan_at` in
/// `scan_state` minus one poll interval. These rows can never be re-fetched
/// by the poller, so they no longer serve a dedup purpose.
/// Returns the number of rows removed.
pub async fn cleanup_seen_comments(pool: &SqlitePool, poll_interval_secs: u64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM seen_comments \
         WHERE processed_at < datetime(\
             (SELECT MIN(last_scan_at) FROM scan_state), \
             '-' || ? || ' seconds')",
    )
    .bind(poll_interval_secs as i64)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete jobs older than `retention_days` regardless of status.
/// A job still pending or running after 30 days is stale.
/// Returns the number of rows removed.
pub async fn cleanup_old_jobs(pool: &SqlitePool, retention_days: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM benchmark_jobs \
         WHERE updated_at < datetime('now', '-' || ? || ' days')",
    )
    .bind(retention_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Run all cleanup tasks with default parameters. Returns (comments_deleted, jobs_deleted).
#[tracing::instrument(skip(pool))]
pub async fn run_cleanup(pool: &SqlitePool, poll_interval_secs: u64) -> Result<(u64, u64)> {
    let comments = cleanup_seen_comments(pool, poll_interval_secs).await?;
    let jobs = cleanup_old_jobs(pool, JOB_RETENTION_DAYS).await?;
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
            env_vars: "{}",
            baseline_env_vars: "{}",
            changed_env_vars: "{}",
            baseline_ref: None,
            changed_ref: None,
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

    // ── get_pending_jobs: per-user running-cap filter ─────────────

    #[tokio::test]
    async fn get_pending_skips_user_at_running_cap() {
        let pool = test_pool().await;

        // alice has 5 running jobs — her pending jobs should be skipped.
        for i in 0..5 {
            let cid = 900 + i;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();
            update_job_status(&pool, id, JobStatus::Running, Some("k"), None)
                .await
                .unwrap();
        }
        // alice has a pending job waiting
        mark_comment_seen(&pool, 910, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        insert_job(&pool, &test_job(910)).await.unwrap();

        // bob has a pending job — should still be picked up
        let mut bob_job = test_job(920);
        bob_job.login = "bob";
        mark_comment_seen(&pool, 920, "apache/datafusion", 42, "bob", "2024-01-01")
            .await
            .unwrap();
        insert_job(&pool, &bob_job).await.unwrap();

        let pending = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].login, "bob");
    }

    // ── count_user_pending ──────────────────────────────────────────

    #[tokio::test]
    async fn count_user_pending_groups_by_login_and_status() {
        let pool = test_pool().await;

        for i in 0..3 {
            let cid = 1000 + i;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            insert_job(&pool, &test_job(cid)).await.unwrap();
        }
        // one of alice's jobs is running, not pending
        mark_comment_seen(&pool, 1100, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let running_id = insert_job(&pool, &test_job(1100)).await.unwrap();
        update_job_status(&pool, running_id, JobStatus::Running, Some("k"), None)
            .await
            .unwrap();

        // bob has one pending job — should not be counted for alice
        let mut bob_job = test_job(1200);
        bob_job.login = "bob";
        mark_comment_seen(&pool, 1200, "apache/datafusion", 42, "bob", "2024-01-01")
            .await
            .unwrap();
        insert_job(&pool, &bob_job).await.unwrap();

        assert_eq!(count_user_pending(&pool, "alice").await.unwrap(), 3);
        assert_eq!(count_user_pending(&pool, "bob").await.unwrap(), 1);
        assert_eq!(count_user_pending(&pool, "nobody").await.unwrap(), 0);
    }

    // ── set_runner_token + get_job_for_comment ─────────────────

    #[tokio::test]
    async fn runner_token_roundtrip() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 2000, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        let id = insert_job(&pool, &test_job(2000)).await.unwrap();

        set_runner_token(&pool, id, "secret-abc").await.unwrap();

        let (repo, pr, status, token) = get_job_for_comment(&pool, id).await.unwrap().unwrap();
        assert_eq!(repo, "apache/datafusion");
        assert_eq!(pr, 42);
        assert_eq!(status, "pending");
        assert_eq!(token.as_deref(), Some("secret-abc"));

        // Missing job
        assert!(get_job_for_comment(&pool, 99_999).await.unwrap().is_none());
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
        let poll_interval: u64 = 5;

        // Set scan_state so the cleanup has a reference point
        set_last_scan(&pool, "apache/datafusion", "2024-06-15T00:00:00Z")
            .await
            .unwrap();

        // Insert a comment well before last_scan_at
        mark_comment_seen(&pool, 700, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE seen_comments SET processed_at = '2024-01-01T00:00:00Z' WHERE comment_id = 700",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert a recent comment (after last_scan_at)
        mark_comment_seen(&pool, 701, "apache/datafusion", 43, "bob", "2024-06-01")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE seen_comments SET processed_at = '2024-06-15T00:00:00Z' WHERE comment_id = 701",
        )
        .execute(&pool)
        .await
        .unwrap();

        let deleted = cleanup_seen_comments(&pool, poll_interval).await.unwrap();
        assert_eq!(deleted, 1);

        // Recent one still exists
        assert!(is_comment_seen(&pool, 701).await.unwrap());
        // Old one is gone
        assert!(!is_comment_seen(&pool, 700).await.unwrap());
    }

    #[tokio::test]
    async fn cleanup_seen_comments_noop_without_scan_state() {
        let pool = test_pool().await;

        // Insert a comment with no scan_state rows — cleanup should delete nothing
        mark_comment_seen(&pool, 710, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE seen_comments SET processed_at = '2020-01-01T00:00:00Z' WHERE comment_id = 710",
        )
        .execute(&pool)
        .await
        .unwrap();

        let deleted = cleanup_seen_comments(&pool, 5).await.unwrap();
        assert_eq!(deleted, 0);
    }

    // ── cleanup_old_jobs ────────────────────────────────────────

    #[tokio::test]
    async fn cleanup_old_jobs_deletes_all_old_keeps_recent() {
        let pool = test_pool().await;

        // Create four old jobs: completed, failed, pending, running — all 60 days old
        for (i, status) in [
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Pending,
            JobStatus::Running,
        ]
        .into_iter()
        .enumerate()
        {
            let cid = 800 + i as i64;
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();
            // Pending is the default, only update for others
            if !matches!(status, JobStatus::Pending) {
                update_job_status(&pool, id, status, None, None)
                    .await
                    .unwrap();
            }
            // Backdate to 60 days ago
            sqlx::query(
                "UPDATE benchmark_jobs SET updated_at = datetime('now', '-60 days') WHERE id = ?",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Create one recent job (stays at default updated_at = now)
        mark_comment_seen(&pool, 810, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        insert_job(&pool, &test_job(810)).await.unwrap();

        let deleted = cleanup_old_jobs(&pool, 30).await.unwrap();
        // All four old jobs deleted regardless of status
        assert_eq!(deleted, 4);

        // Only the recent one remains
        let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benchmark_jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 1);
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
