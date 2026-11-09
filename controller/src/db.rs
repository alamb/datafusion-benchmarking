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
pub async fn insert_job<'e, E: sqlx::Executor<'e, Database = sqlx::Sqlite>>(
    executor: E,
    job: &JobInsert<'_>,
) -> Result<i64> {
    job.shard.validate()?;
    let result = sqlx::query(
        "INSERT INTO benchmark_jobs \
         (comment_id, repo, pr_number, pr_url, login, benchmarks, env_vars, \
          baseline_env_vars, changed_env_vars, baseline_ref, changed_ref, job_type, \
          cpu_request, memory_request, cpu_arch, effective_shards, shard_index, \
          assignment_version, resolved_source_json, trigger_comment) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(job.resources.cpu.as_deref())
    .bind(job.resources.memory.as_deref())
    .bind(job.resources.arch.as_deref())
    .bind(job.shard.count())
    .bind(job.shard.index)
    .bind((job.shard.count() > 1).then_some(crate::sharding::ASSIGNMENT_VERSION))
    .bind(job.resolved_source_json)
    .bind(job.trigger_comment)
    .execute(executor)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Atomically acknowledge a trigger and insert all of its independent jobs.
/// A duplicate trigger is a no-op; a partial fan-out is never committed.
pub async fn enqueue_jobs(
    pool: &SqlitePool,
    jobs: &[JobInsert<'_>],
    created_at: &str,
) -> Result<()> {
    let first = jobs
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty job batch"))?;
    let mut tx = pool.begin().await?;
    let seen = sqlx::query("INSERT OR IGNORE INTO seen_comments (comment_id, repo, pr_number, login, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(first.comment_id).bind(first.repo).bind(first.pr_number).bind(first.login).bind(created_at)
        .execute(&mut *tx).await?;
    if seen.rows_affected() == 0 {
        return Ok(());
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM benchmark_jobs WHERE login = ? AND status = 'pending'",
    )
    .bind(first.login)
    .fetch_one(&mut *tx)
    .await?;
    anyhow::ensure!(
        pending + jobs.len() as i64 <= crate::config::max_queued_for(first.login),
        "per-user queue limit exceeded"
    );
    for job in jobs {
        anyhow::ensure!(
            job.comment_id == first.comment_id
                && job.login == first.login
                && job.repo == first.repo
                && job.pr_number == first.pr_number
                && job.job_type == first.job_type
                && job.shard.count() == first.shard.count(),
            "mixed trigger batch"
        );
        insert_job(&mut *tx, job).await?;
    }
    if first.job_type == crate::models::JobType::ArrowCriterion.as_str() {
        sqlx::query("INSERT INTO sharded_runs (comment_id, benchmarks) SELECT DISTINCT comment_id, benchmarks FROM benchmark_jobs WHERE comment_id = ?")
            .bind(first.comment_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Return up to 5 oldest `pending` jobs, ordered by ID. Skips jobs whose
/// author already has `MAX_RUNNING_PER_USER` running jobs — those stay
/// pending until an earlier run finishes.
#[tracing::instrument(skip(pool))]
pub async fn get_pending_jobs(pool: &SqlitePool) -> Result<Vec<BenchmarkJob>> {
    let jobs = sqlx::query_as::<_, BenchmarkJob>(
        "SELECT * FROM benchmark_jobs p \
         WHERE p.status = 'pending' \
           AND ((SELECT COUNT(*) FROM benchmark_jobs r \
                 WHERE r.login = p.login AND r.status = 'running') \
                + CASE WHEN p.effective_shards > 1 THEN \
                  (SELECT COUNT(*) FROM benchmark_jobs earlier \
                   WHERE earlier.login = p.login AND earlier.status = 'pending' AND earlier.id < p.id) \
                  ELSE 0 END) < ? \
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
    sqlx::query("DELETE FROM sharded_runs WHERE NOT EXISTS (SELECT 1 FROM benchmark_jobs WHERE benchmark_jobs.comment_id = sharded_runs.comment_id AND benchmark_jobs.benchmarks = sharded_runs.benchmarks)")
        .execute(pool).await?;
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
    use crate::resources::PodResources;

    async fn test_pool() -> SqlitePool {
        connect("sqlite::memory:").await.unwrap()
    }

    fn test_job(comment_id: i64) -> JobInsert<'static> {
        JobInsert {
            trigger_comment: None,
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
            resources: &NO_RESOURCES,
            shard: crate::sharding::Shard::default(),
            resolved_source_json: None,
        }
    }

    /// A job whose pod sizing is left entirely to the controller defaults.
    static NO_RESOURCES: PodResources = PodResources {
        cpu: None,
        memory: None,
        arch: None,
    };

    #[tokio::test]
    async fn shard_fanout_is_atomic_idempotent_and_persists_frozen_sources() {
        let pool = test_pool().await;
        let sources = r#"{"baseline_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","changed_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","pr_head_ref":"feature"}"#;
        let jobs: Vec<_> = (0..4)
            .map(|index| {
                let mut job = test_job(5000);
                job.shard = crate::sharding::Shard { count: 4, index };
                job.resolved_source_json = Some(sources);
                job
            })
            .collect();
        enqueue_jobs(&pool, &jobs, "2024-01-01").await.unwrap();
        enqueue_jobs(&pool, &jobs, "2024-01-01").await.unwrap();
        let rows = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(rows.len(), 4);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.shard_index, i as u32);
            assert_eq!(row.shard().unwrap().count(), 4);
            assert_eq!(row.resolved_source_json.as_deref(), Some(sources));
        }
    }

    #[tokio::test]
    async fn failed_fanout_rolls_back_jobs_and_seen_comment() {
        let pool = test_pool().await;
        let a = test_job(5001);
        let mut invalid = test_job(5001);
        invalid.shard.index = 1; // Invalid for the default count of one.
        assert!(enqueue_jobs(&pool, &[a, invalid], "2024-01-01")
            .await
            .is_err());
        assert!(!is_comment_seen(&pool, 5001).await.unwrap());
        assert!(get_pending_jobs(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn shard_jobs_do_not_overshoot_running_slots_in_a_batch() {
        let pool = test_pool().await;
        for cid in 6000..6004 {
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();
            update_job_status(&pool, id, JobStatus::Running, Some("running"), None)
                .await
                .unwrap();
        }
        let jobs: Vec<_> = (0..8)
            .map(|index| {
                let mut job = test_job(7000);
                job.shard = crate::sharding::Shard { count: 8, index };
                job
            })
            .collect();
        enqueue_jobs(&pool, &jobs, "2024-01-01").await.unwrap();
        assert_eq!(get_pending_jobs(&pool).await.unwrap().len(), 1);
        assert_eq!(count_user_pending(&pool, "alice").await.unwrap(), 8);
    }

    #[tokio::test]
    async fn ordinary_pending_selection_keeps_original_batch_behavior() {
        let pool = test_pool().await;
        for cid in 9000..9004 {
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            let id = insert_job(&pool, &test_job(cid)).await.unwrap();
            update_job_status(&pool, id, JobStatus::Running, Some("running"), None)
                .await
                .unwrap();
        }
        for cid in 9100..9105 {
            mark_comment_seen(&pool, cid, "apache/datafusion", 42, "alice", "2024-01-01")
                .await
                .unwrap();
            insert_job(&pool, &test_job(cid)).await.unwrap();
        }
        // The pre-sharding query picked a batch of five once the user was
        // below the running cap. Do not change that policy for ordinary jobs.
        assert_eq!(get_pending_jobs(&pool).await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn effective_count_is_persisted_and_not_reinterpreted() {
        let pool = test_pool().await;
        // Includes single-worker plans stored before unsupported requests were rejected.
        let job = test_job(9200);
        let requested = Some(8);
        enqueue_jobs(&pool, &[job], "2024-01-01").await.unwrap();
        let rows = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.effective_shards, 1);
        assert_eq!(row.shard().unwrap(), crate::sharding::Shard::default());
        assert!(row.shard_label().is_empty());
        assert!(row.assignment_version.is_none());
        assert!(row.resolved_source_json.is_none());
        // Changes in setup eligibility must not reinterpret a stored execution plan.
        assert!(crate::models::JobType::Datafusion
            .shard_support()
            .resolve(requested)
            .is_err());
        assert_eq!(
            crate::sharding::ShardSupport::Partitioned
                .resolve(requested)
                .unwrap(),
            8
        );
        assert_eq!(row.shard().unwrap().count(), 1);
        assert_eq!(count_user_pending(&pool, "alice").await.unwrap(), 1);
        assert!(
            sqlx::query("UPDATE benchmark_jobs SET shard_index = 1 WHERE id = ?")
                .bind(row.id)
                .execute(&pool)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn queue_limit_counts_all_shards() {
        let pool = test_pool().await;
        let jobs: Vec<_> = (0..16).map(|_| test_job(8000)).collect();
        assert!(enqueue_jobs(&pool, &jobs, "2024-01-01").await.is_err());
        assert!(!is_comment_seen(&pool, 8000).await.unwrap());
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

    /// The pod sizing a trigger asked for has to survive the insert, or the
    /// pod builder silently falls back to the controller defaults.
    #[tokio::test]
    async fn insert_stores_the_requested_resources() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 150, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();

        let resources = PodResources {
            cpu: Some("16".into()),
            memory: Some("128Gi".into()),
            arch: Some("amd64".into()),
        };
        let mut job = test_job(150);
        job.resources = &resources;
        insert_job(&pool, &job).await.unwrap();

        let pending = get_pending_jobs(&pool).await.unwrap();
        assert_eq!(pending[0].cpu_request.as_deref(), Some("16"));
        assert_eq!(pending[0].memory_request.as_deref(), Some("128Gi"));
        assert_eq!(pending[0].cpu_arch.as_deref(), Some("amd64"));
    }

    /// An unconfigured job leaves the columns NULL so the pod builder uses the
    /// controller defaults.
    #[tokio::test]
    async fn insert_leaves_unrequested_resources_null() {
        let pool = test_pool().await;
        mark_comment_seen(&pool, 160, "apache/datafusion", 42, "alice", "2024-01-01")
            .await
            .unwrap();
        insert_job(&pool, &test_job(160)).await.unwrap();

        let pending = get_pending_jobs(&pool).await.unwrap();
        assert!(pending[0].cpu_request.is_none());
        assert!(pending[0].memory_request.is_none());
        assert!(pending[0].cpu_arch.is_none());
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
