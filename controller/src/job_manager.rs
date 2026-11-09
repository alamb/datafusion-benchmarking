//! Kubernetes Job reconciliation loop.
//!
//! Drives benchmark jobs through their lifecycle: creates K8s Jobs for
//! pending rows and polls running jobs for completion.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EnvVar, EnvVarSource, EphemeralVolumeSource,
    PersistentVolumeClaimTemplate, Pod, PodSpec, PodTemplateSpec, ResourceRequirements,
    SeccompProfile, SecurityContext, Toleration, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, ListParams, LogParams, PostParams};
use kube::Client as KubeClient;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::config::Config;
use crate::db;
use crate::github::{self, GitHubClient};
use crate::models::{BenchmarkJob, JobStatus};

/// Infinite reconciliation loop that drives jobs through their lifecycle.
///
/// Returns `Err` if the K8s client fails to initialize, causing the controller
/// to shut down so K8s can restart the pod.
///
/// ```text
/// ┌─────────────────────────────────────────────────────────┐
/// │  reconcile_loop (every RECONCILE_INTERVAL_SECS)         │
/// │                                                         │
/// │  reconcile_pending():                                   │
/// │    pending jobs ──► create K8s Job ──► status = running │
/// │                 └─► on error       ──► status = failed  │
/// │                                                         │
/// │  reconcile_active():                                    │
/// │    running jobs ──► K8s succeeded  ──► status = completed│
/// │                 ├─► K8s failed     ──► status = failed  │
/// │                 └─► K8s 404        ──► status = failed  │
/// └─────────────────────────────────────────────────────────┘
/// ```
pub async fn reconcile_loop(
    config: Config,
    pool: SqlitePool,
    gh: GitHubClient,
    token: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let interval = tokio::time::Duration::from_secs(config.reconcile_interval_secs);

    let kube_client = KubeClient::try_default()
        .await
        .context("failed to create kube client")?;

    loop {
        if let Err(e) = reconcile_pending(&config, &pool, &gh, &kube_client).await {
            warn!(error = %e, "reconcile pending error");
        }
        if let Err(e) = reconcile_active(&config, &pool, &gh, &kube_client).await {
            warn!(error = %e, "reconcile active error");
        }
        if let Err(e) =
            crate::shard_reporting::reconcile(&pool, &gh, config.runner_repo_url.as_deref()).await
        {
            warn!(error = %e, "reconcile shard reporting error");
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = token.cancelled() => {
                info!("reconciler shutting down");
                break;
            }
        }
    }
    Ok(())
}

/// Create K8s Jobs for all pending benchmark rows and transition them to running/failed.
#[tracing::instrument(skip_all)]
async fn reconcile_pending(
    config: &Config,
    pool: &SqlitePool,
    gh: &GitHubClient,
    kube: &KubeClient,
) -> Result<()> {
    let pending = db::get_pending_jobs(pool).await?;
    let jobs_api: Api<Job> = Api::namespaced(kube.clone(), &config.k8s_namespace);

    for job in pending {
        if job.uses_collected_results() {
            if let Err(e) = crate::shard_reporting::ensure_started(
                pool,
                gh,
                &job,
                config.runner_repo_url.as_deref(),
            )
            .await
            {
                warn!(comment_id = job.comment_id, error = %e, "failed to post request start; leaving worker pending");
                continue;
            }
        }
        // Generate the per-job runner token before creating the pod so the
        // token the pod ships with matches the row in SQLite. If the DB
        // write fails we don't attempt to create the K8s Job.
        let runner_token = generate_runner_token();
        if let Err(e) = db::set_runner_token(pool, job.id, &runner_token).await {
            warn!(comment_id = job.comment_id, error = %e, "failed to store runner token");
            continue;
        }

        // Resolve the PR's source branch here so the runner pod (which has
        // no GitHub credentials) doesn't need to call `gh pr view`. A
        // lookup failure is not fatal — we fall back to an empty value and
        // the runner will error cleanly if it actually needs it.
        let pr_head_ref = if job.uses_collected_results() {
            // Sharded workers use the source identities frozen at ingestion.
            String::new()
        } else {
            match gh.get_pr_head_ref(&job.repo, job.pr_number).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(comment_id = job.comment_id, error = %e, "failed to resolve PR head ref");
                    String::new()
                }
            }
        };

        match create_k8s_job(config, &jobs_api, &job, &runner_token, &pr_head_ref).await {
            Ok(k8s_name) => {
                info!(comment_id = job.comment_id, k8s_name = %k8s_name, "created k8s job");
                db::update_job_status(pool, job.id, JobStatus::Running, Some(&k8s_name), None)
                    .await?;
            }
            Err(e) => {
                warn!(comment_id = job.comment_id, error = %e, "failed to create k8s job");
                db::update_job_status(
                    pool,
                    job.id,
                    JobStatus::Failed,
                    None,
                    Some(&format!("Failed to create K8s Job: {e}")),
                )
                .await?;

                let comment_url = format!("{}#issuecomment-{}", job.pr_url, job.comment_id);
                let footer = github::issues_footer(config.runner_repo_url.as_deref());
                let msg = format!(
                    "Failed to start benchmark{} for [this request]({comment_url}): {e}{footer}",
                    job.shard_label()
                );
                if !job.uses_collected_results() {
                    if let Err(e) = gh.post_comment(&job.repo, job.pr_number, &msg).await {
                        warn!(error = %e, "failed to post error comment");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check K8s Job status for all running benchmark rows and transition to completed/failed.
#[tracing::instrument(skip_all)]
async fn reconcile_active(
    config: &Config,
    pool: &SqlitePool,
    gh: &GitHubClient,
    kube: &KubeClient,
) -> Result<()> {
    let active = db::get_active_jobs(pool).await?;
    let jobs_api: Api<Job> = Api::namespaced(kube.clone(), &config.k8s_namespace);

    for job in active {
        let k8s_name = match &job.k8s_job_name {
            Some(n) => n.clone(),
            None => continue,
        };

        match jobs_api.get(&k8s_name).await {
            Ok(k8s_job) => {
                let status = k8s_job.status.as_ref();
                let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);

                // Check the job-level conditions for a terminal state.
                // We cannot simply check `.status.failed > 0` because with
                // backoffLimit > 0, K8s increments that counter on each pod
                // failure while retries are still in progress. The "Failed"
                // condition is only added once all retries are exhausted.
                let failed_cond = status
                    .and_then(|s| s.conditions.as_ref())
                    .and_then(|conds| {
                        conds
                            .iter()
                            .find(|c| c.type_ == "Failed" && c.status == "True")
                    });

                if succeeded > 0 {
                    info!(comment_id = job.comment_id, "job completed successfully");
                    db::update_job_status(pool, job.id, JobStatus::Completed, None, None).await?;
                } else if let Some(cond) = failed_cond {
                    let failed_count = status.and_then(|s| s.failed).unwrap_or(0);
                    let reason = cond.reason.as_deref().unwrap_or("");
                    let message = cond.message.as_deref().unwrap_or("");
                    info!(
                        comment_id = job.comment_id,
                        failed_count, reason, message, "job failed terminally"
                    );
                    let err_msg = if reason.is_empty() {
                        "K8s job failed".to_string()
                    } else {
                        format!("K8s job failed: {reason}")
                    };
                    db::update_job_status(pool, job.id, JobStatus::Failed, None, Some(&err_msg))
                        .await?;

                    // A terminal K8s failure can kill the runner before it
                    // posts its own error comment. Notify from this reliable
                    // controller-owned lifecycle boundary instead.
                    post_terminal_failure_comment(config, pool, gh, kube, &job, reason, message)
                        .await;
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                warn!(
                    comment_id = job.comment_id,
                    k8s_name, "k8s job not found, marking failed"
                );
                db::update_job_status(
                    pool,
                    job.id,
                    JobStatus::Failed,
                    None,
                    Some("K8s Job not found"),
                )
                .await?;
            }
            Err(e) => {
                warn!(comment_id = job.comment_id, error = %e, "error checking k8s job");
            }
        }
    }

    Ok(())
}

/// Post a PR comment for a terminal Kubernetes Job failure.
///
/// The runner can be killed before its own error handler posts a comment.
/// Failures to post are logged but not propagated: the DB is already marked
/// failed, and retrying here could create duplicate comments.
async fn post_terminal_failure_comment(
    config: &Config,
    pool: &SqlitePool,
    gh: &GitHubClient,
    kube: &KubeClient,
    job: &BenchmarkJob,
    reason: &str,
    k8s_message: &str,
) {
    let log_tail = match job.k8s_job_name.as_deref() {
        Some(name) => failed_pod_log_tail(kube, &config.k8s_namespace, name).await,
        None => String::new(),
    };
    let body = terminal_failure_comment_body(
        config.active_deadline_secs,
        config.runner_repo_url.as_deref(),
        job,
        reason,
        k8s_message,
        &log_tail,
    );
    if job.uses_collected_results() {
        // Preserve the controller-owned log/deadline diagnostics for the one
        // final target notification, even when the runner never submitted.
        if let Err(e) =
            db::update_job_status(pool, job.id, JobStatus::Failed, None, Some(&body)).await
        {
            warn!(job_id = job.id, error = %e, "failed to persist shard failure detail");
        }
        return;
    }
    if let Err(e) = gh.post_comment(&job.repo, job.pr_number, &body).await {
        warn!(
            comment_id = job.comment_id,
            error = %e,
            reason,
            "failed to post terminal failure comment"
        );
    }
}

/// Format the controller-owned notification for a terminal Kubernetes failure.
fn terminal_failure_comment_body(
    active_deadline_secs: i64,
    runner_repo_url: Option<&str>,
    job: &BenchmarkJob,
    reason: &str,
    k8s_message: &str,
    log_tail: &str,
) -> String {
    let benchmarks = format!(
        "{}{}",
        serde_json::from_str::<Vec<String>>(&job.benchmarks)
            .map(|v| v.join(", "))
            .unwrap_or_else(|_| job.benchmarks.clone()),
        job.shard_label()
    );
    let comment_url = format!("{}#issuecomment-{}", job.pr_url, job.comment_id);
    let footer = github::issues_footer(runner_repo_url);
    let k8s_detail = if k8s_message.is_empty() {
        String::new()
    } else {
        let fence = markdown_code_fence(k8s_message);
        format!(
            "\n\n<details><summary>Kubernetes message</summary>\n\n{fence}\n{k8s_message}\n{fence}\n\n</details>"
        )
    };
    // The Kubernetes reason says the pod exited non-zero, never why. The
    // runner's own last words do, so lead the details with them.
    let log_detail = if log_tail.is_empty() {
        String::new()
    } else {
        let fence = markdown_code_fence(log_tail);
        format!(
            "\n\n<details><summary>Runner log (last {POD_LOG_TAIL_LINES} lines)</summary>\n\n{fence}\n{log_tail}\n{fence}\n\n</details>"
        )
    };
    let k8s_detail = format!("{log_detail}{k8s_detail}");

    if reason == "DeadlineExceeded" {
        format!(
            "Benchmark for [this request]({comment_url}) hit the {active_deadline_secs}s job deadline before finishing.\n\n\
             Benchmarks requested: `{benchmarks}`{k8s_detail}{footer}",
        )
    } else {
        let reason = if reason.is_empty() {
            "an unspecified Kubernetes error"
        } else {
            reason
        };
        format!(
            "Benchmark for [this request]({comment_url}) failed before finishing (Kubernetes reason: `{reason}`).\n\n\
             Benchmarks requested: `{benchmarks}`{k8s_detail}{footer}",
        )
    }
}

/// How much of the failed pod's log to quote back into the PR comment. Enough
/// to carry a cargo error and the command that produced it, short enough that
/// the comment stays readable when expanded.
const POD_LOG_TAIL_LINES: i64 = 40;

/// Tail of the failed pod's log, for the terminal-failure comment.
///
/// A benchmark that fails on its own terms (an unknown bench name, a target
/// whose `required-features` are not enabled, a compile error) dies inside the
/// pod, and the Job condition the controller sees carries only
/// `BackoffLimitExceeded`. Quoting the log puts the actual cause in the PR,
/// where the person who triggered the run can read it without cluster access.
///
/// Best effort: any failure here yields an empty string and the comment simply
/// omits the section, rather than losing the notification altogether.
async fn failed_pod_log_tail(kube: &KubeClient, namespace: &str, k8s_job_name: &str) -> String {
    let pods: Api<Pod> = Api::namespaced(kube.clone(), namespace);
    let list_params = ListParams::default().labels(&format!("job-name={k8s_job_name}"));
    let pod_list = match pods.list(&list_params).await {
        Ok(list) => list,
        Err(e) => {
            warn!(k8s_job_name, error = %e, "failed to list pods for failed job");
            return String::new();
        }
    };

    // The newest pod is the one that failed last. `backoffLimit` is 0, so there
    // is normally exactly one, but a preempted spot node can leave an earlier
    // pod behind whose log describes a different run.
    let Some(pod_name) = pod_list
        .items
        .iter()
        .max_by_key(|p| p.metadata.creation_timestamp.clone().map(|t| t.0))
        .and_then(|p| p.metadata.name.clone())
    else {
        warn!(k8s_job_name, "no pods found for failed job");
        return String::new();
    };

    let log_params = LogParams {
        tail_lines: Some(POD_LOG_TAIL_LINES),
        ..Default::default()
    };
    match pods.logs(&pod_name, &log_params).await {
        Ok(logs) => logs.trim_end().to_string(),
        Err(e) => {
            warn!(pod_name, error = %e, "failed to read log of failed pod");
            String::new()
        }
    }
}

/// Return a Markdown code fence that cannot be closed by `content`.
fn markdown_code_fence(content: &str) -> String {
    let longest_run = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(3.max(longest_run + 1))
}

/// Shorthand for constructing a plain-value [`EnvVar`].
fn env_var(name: &str, value: impl Into<String>) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        ..Default::default()
    }
}

/// Keep all new protocol metadata out of the benchmark's environment.
fn shard_args(job: &BenchmarkJob) -> Result<Option<Vec<String>>> {
    if !job.uses_collected_results() {
        return Ok(None);
    }
    let shard = job.shard()?;
    let sources: crate::sharding::FrozenSources = serde_json::from_str(
        job.resolved_source_json
            .as_deref()
            .context("missing frozen shard sources")?,
    )?;
    sources.validate()?;
    let config = crate::sharding::WorkerShard {
        version: crate::sharding::ASSIGNMENT_VERSION.into(),
        shard,
        sources,
    };
    Ok(Some(vec![
        crate::sharding::WORKER_FLAG.into(),
        serde_json::to_string(&config)?,
    ]))
}

/// Generate a random 32-byte hex token used to authenticate the runner
/// pod to the controller's `POST /jobs/{id}/comment` endpoint. Uses tokio's
/// `Instant` + process id + sqlx rowid entropy would be insufficient;
/// instead we pull from the OS via `std::fs::read`.
fn generate_runner_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Best-effort OS entropy; fall back to time-based mixing if /dev/urandom
    // is unavailable (shouldn't happen on Linux nodes).
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    // Mix in the nanosecond timestamp so duplicate opens/races still differ.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte ^= ((now >> (i % 16 * 8)) & 0xff) as u8;
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Kubernetes DNS name for the controller service. Must match the Service
/// defined in `services/controller.ts` (headless service `benchmark-controller`
/// governing the StatefulSet). The controller listens on 8080 for health
/// checks and the comment-proxy endpoint.
fn controller_url(namespace: &str) -> String {
    format!("http://benchmark-controller.{namespace}.svc.cluster.local:8080")
}

/// Coerce a GitHub login into a valid Kubernetes label value: lowercase
/// alphanumerics and `-_.`, max 63 chars. Labels are for audit only; an
/// unexpected char pattern gets silently squashed rather than failing the
/// whole Job creation.
fn sanitize_label(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(63);
    // Labels must start and end with an alphanumeric.
    let trimmed = out
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

/// Where a benchmark pod runs and how big it is.
///
/// Each field is the job's own request when it has one, and the controller
/// default otherwise.
struct Placement<'a> {
    cpu: &'a str,
    memory: &'a str,
    /// GCE machine family for the node selector.
    machine_family: &'a str,
    /// `kubernetes.io/arch` value matching `machine_family`.
    arch: &'a str,
}

impl<'a> Placement<'a> {
    fn resolve(
        job: &'a BenchmarkJob,
        default_cpu: &'a str,
        default_memory: &'a str,
        default_machine_family: &'a str,
    ) -> Self {
        // `x86_64` is not trigger syntax; it is accepted for rows written
        // before the trigger could ask for an architecture.
        let machine_family = match job.cpu_arch.as_deref() {
            Some("arm64") => "c4a",
            Some("amd64") | Some("x86_64") => "c4",
            _ => default_machine_family,
        };
        Self {
            cpu: job.cpu_request.as_deref().unwrap_or(default_cpu),
            memory: job.memory_request.as_deref().unwrap_or(default_memory),
            machine_family,
            arch: if machine_family == "c4a" {
                "arm64"
            } else {
                "amd64"
            },
        }
    }
}

/// Build and submit a K8s Job spec for a benchmark row.
///
/// Resource defaults come from [`Config`]. Tolerates GKE spot instances.
/// Uses an ephemeral volume at `/workspace` for build artifacts.
/// The runner has no GitHub credentials — it proxies PR comments through the
/// controller using `RUNNER_TOKEN` + `CONTROLLER_URL` + `JOB_ID`.
#[tracing::instrument(skip(config, jobs_api, runner_token, pr_head_ref), fields(comment_id = job.comment_id, pr_number = job.pr_number))]
async fn create_k8s_job(
    config: &Config,
    jobs_api: &Api<Job>,
    job: &BenchmarkJob,
    runner_token: &str,
    pr_head_ref: &str,
) -> Result<String> {
    let benchmarks: Vec<String> = serde_json::from_str(&job.benchmarks)?;
    let args = shard_args(job)?;

    // Parse shared env vars — accept both legacy `["K=V"]` array and new `{"K":"V"}` map
    let shared_env_vars: std::collections::HashMap<String, String> =
        if job.env_vars.trim_start().starts_with('[') {
            // Legacy format: JSON array of "KEY=VALUE" strings
            let arr: Vec<String> = serde_json::from_str(&job.env_vars)?;
            arr.iter()
                .filter_map(|s| {
                    s.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect()
        } else {
            serde_json::from_str(&job.env_vars)?
        };

    let job_name = format!("bench-c{}-{}", job.comment_id, job.id);

    let placement = Placement::resolve(
        job,
        &config.default_cpu,
        &config.default_memory,
        &config.default_machine_family,
    );
    let Placement {
        cpu,
        memory,
        machine_family,
        arch,
    } = placement;

    let mut env = vec![
        env_var("PR_URL", job.pr_url.clone()),
        env_var("COMMENT_ID", job.comment_id.to_string()),
        env_var("BENCHMARKS", benchmarks.join(" ")),
        env_var("BENCH_TYPE", job.job_type.clone()),
        env_var("REPO", job.repo.clone()),
        // Runner posts comments via the controller proxy — no GitHub creds
        // in the pod.
        env_var("JOB_ID", job.id.to_string()),
        env_var("RUNNER_TOKEN", runner_token),
        env_var("CONTROLLER_URL", controller_url(&config.k8s_namespace)),
        env_var(
            "RUNNER_JOB_DEADLINE_SECS",
            config.active_deadline_secs.to_string(),
        ),
        // NOTE: we deliberately do NOT set CARGO_PROFILE_RELEASE_DEBUG here.
        // Building with debuginfo (`debug=1`) makes rustc and the linker use
        // dramatically more memory: a single benchmark compile was observed
        // driving one rustc to ~16 GB and the pod cgroup to its 65 GiB limit
        // (OOM-kill) — before any benchmark query ran. Default builds stay
        // fast and lean. A specific run can re-enable DWARF symbols for live
        // gdb dumps from hung jobs by adding a shared `env:` block to the
        // trigger comment (shared env is inherited by the build processes,
        // unlike per-side `baseline:`/`changed:` env which only reaches the
        // benchmark run):
        //
        //   run benchmark <name>
        //   env:
        //     CARGO_PROFILE_RELEASE_DEBUG: "1"
        //
        // The job cap that used to pair with this is now forced by the runner
        // (see runner/build_env.rs), and a trigger cannot lower it. Debuginfo
        // makes each rustc much larger, so if such a run is OOM-killed, lower
        // the cap there instead.
    ];

    // The controller resolves the PR's source branch and hands it to the
    // runner so `git.rs::checkout_pr` doesn't need `gh pr view` (which
    // would require a GitHub token we no longer ship in the pod).
    if !pr_head_ref.is_empty() {
        env.push(env_var("PR_HEAD_REF", pr_head_ref));
    }

    // Add shared env vars directly on the pod (backward compat)
    for (k, v) in &shared_env_vars {
        env.push(env_var(k, v));
    }

    // Set after the shared env block so a trigger cannot replace its own quote.
    if let Some(body) = &job.trigger_comment {
        env.push(env_var("TRIGGER_COMMENT", body.clone()));
    }

    // Also pass the shared block as a JSON map. The pod already has these set
    // individually (above); the map lets the runner report them in PR comments
    // without having to guess which of its env vars came from the trigger.
    env.push(env_var(
        "SHARED_ENV_VARS",
        serde_json::to_string(&shared_env_vars)?,
    ));

    // Pass per-side env vars as JSON maps for the runner to apply
    env.push(env_var("BASELINE_ENV_VARS", &job.baseline_env_vars));
    env.push(env_var("CHANGED_ENV_VARS", &job.changed_env_vars));

    // Pass custom refs if set
    if let Some(ref baseline_ref) = job.baseline_ref {
        env.push(env_var("BASELINE_REF", baseline_ref));
    }
    if let Some(ref changed_ref) = job.changed_ref {
        env.push(env_var("CHANGED_REF", changed_ref));
    }

    // The arrow runner benches a single named target; the datafusion runner
    // reads the BENCHMARKS list and resolves each name itself.
    if job.job_type == "arrow_criterion" && !benchmarks.is_empty() {
        env.push(env_var("BENCH_NAME", benchmarks[0].clone()));
    }

    // sccache: inject GCS cache env vars when configured
    if let Some(bucket) = &config.sccache_gcs_bucket {
        env.push(env_var("SCCACHE_GCS_BUCKET", bucket.clone()));
        env.push(env_var("RUSTC_WRAPPER", "sccache"));
        env.push(env_var("SCCACHE_GCS_RW_MODE", "READ_WRITE"));
    }

    // Benchmark data cache: inject bucket name when configured
    if let Some(bucket) = &config.data_cache_bucket {
        env.push(env_var("DATA_CACHE_BUCKET", bucket.clone()));
    }

    // Pass the benchmark runner repo URL for "file an issue" links
    if let Some(url) = &config.runner_repo_url {
        env.push(env_var("RUNNER_REPO_URL", url.clone()));
    }

    // Pass the trigger's own resource request, so the runner's "Run
    // configuration" block reproduces the run instead of implying the
    // defaults. The pod's actual limits reach the runner separately, through
    // the Downward API vars below.
    for (name, value) in [
        ("REQUESTED_CPU", &job.cpu_request),
        ("REQUESTED_MEMORY", &job.memory_request),
        ("REQUESTED_ARCH", &job.cpu_arch),
    ] {
        if let Some(value) = value {
            env.push(env_var(name, value.clone()));
        }
    }

    // Expose pod metadata via the Downward API so the runner can include
    // instance details in PR comments.
    env.push(EnvVar {
        name: "NODE_NAME".into(),
        value_from: Some(EnvVarSource {
            field_ref: Some(k8s_openapi::api::core::v1::ObjectFieldSelector {
                field_path: "spec.nodeName".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    env.push(EnvVar {
        name: "POD_CPU_LIMIT".into(),
        value_from: Some(EnvVarSource {
            resource_field_ref: Some(k8s_openapi::api::core::v1::ResourceFieldSelector {
                resource: "limits.cpu".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    env.push(EnvVar {
        name: "POD_MEM_LIMIT".into(),
        value_from: Some(EnvVarSource {
            resource_field_ref: Some(k8s_openapi::api::core::v1::ResourceFieldSelector {
                resource: "limits.memory".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });

    let mut resource_requests = BTreeMap::new();
    resource_requests.insert("cpu".to_string(), Quantity(cpu.to_string()));
    resource_requests.insert("memory".to_string(), Quantity(memory.to_string()));
    resource_requests.insert(
        "ephemeral-storage".to_string(),
        Quantity(config.ephemeral_storage.clone()),
    );

    // CPU + memory limits equal to requests → Guaranteed QoS for better isolation.
    // Ephemeral storage is request-only (limits cause pod evictions instead of throttling).
    let mut resource_limits = BTreeMap::new();
    resource_limits.insert("cpu".to_string(), Quantity(cpu.to_string()));
    resource_limits.insert("memory".to_string(), Quantity(memory.to_string()));

    let mut node_selector = BTreeMap::new();
    node_selector.insert(
        "cloud.google.com/compute-class".to_string(),
        "Performance".to_string(),
    );
    node_selector.insert(
        "cloud.google.com/machine-family".to_string(),
        machine_family.to_string(),
    );
    node_selector.insert("kubernetes.io/arch".to_string(), arch.to_string());

    let k8s_job = Job {
        metadata: ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(config.k8s_namespace.clone()),
            labels: Some({
                let mut l = BTreeMap::new();
                l.insert("app".to_string(), "benchmark-runner".to_string());
                l.insert("comment-id".to_string(), job.comment_id.to_string());
                l.insert("triggered-by".to_string(), sanitize_label(&job.login));
                l
            }),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            active_deadline_seconds: Some(config.active_deadline_secs),
            ttl_seconds_after_finished: Some(config.ttl_after_finished_secs),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some({
                        let mut l = BTreeMap::new();
                        l.insert("app".to_string(), "benchmark-runner".to_string());
                        l.insert("comment-id".to_string(), job.comment_id.to_string());
                        l
                    }),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    service_account_name: Some("benchmark-runner".into()),
                    restart_policy: Some("Never".into()),
                    node_selector: Some(node_selector),
                    tolerations: Some(vec![Toleration {
                        key: Some("cloud.google.com/gke-spot".into()),
                        operator: Some("Exists".into()),
                        effect: Some("NoSchedule".into()),
                        ..Default::default()
                    }]),
                    containers: vec![Container {
                        name: "runner".into(),
                        image: Some(config.runner_image.clone()),
                        args,
                        env: Some(env),
                        resources: Some(ResourceRequirements {
                            requests: Some(resource_requests),
                            limits: Some(resource_limits),
                            ..Default::default()
                        }),
                        volume_mounts: Some(vec![VolumeMount {
                            name: "workspace".into(),
                            mount_path: "/workspace".into(),
                            ..Default::default()
                        }]),
                        // Unconfined seccomp: GKE Autopilot rejects custom
                        // node-side seccomp profiles, and DataFusion's
                        // io_uring-based file I/O needs the `io_uring_*`
                        // syscalls that the default runtime profile blocks.
                        // Workload is already isolated via namespaces,
                        // cgroups, non-privileged containers, and dropped
                        // NET_RAW (see below); comments are proxied through
                        // the controller so the pod has no GitHub creds.
                        security_context: Some(SecurityContext {
                            seccomp_profile: Some(SeccompProfile {
                                type_: "Unconfined".into(),
                                localhost_profile: None,
                            }),
                            capabilities: Some(Capabilities {
                                add: Some(vec!["SYS_PTRACE".into()]),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    volumes: Some(vec![Volume {
                        name: "workspace".into(),
                        ephemeral: Some(EphemeralVolumeSource {
                            volume_claim_template: Some(PersistentVolumeClaimTemplate {
                                metadata: Some(ObjectMeta::default()),
                                spec: k8s_openapi::api::core::v1::PersistentVolumeClaimSpec {
                                    access_modes: Some(vec!["ReadWriteOnce".into()]),
                                    storage_class_name: Some(config.storage_class.clone()),
                                    resources: Some(
                                        k8s_openapi::api::core::v1::VolumeResourceRequirements {
                                            requests: Some({
                                                let mut m = BTreeMap::new();
                                                m.insert(
                                                    "storage".to_string(),
                                                    Quantity(config.ephemeral_storage.clone()),
                                                );
                                                m
                                            }),
                                            ..Default::default()
                                        },
                                    ),
                                    ..Default::default()
                                },
                            }),
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    jobs_api.create(&PostParams::default(), &k8s_job).await?;
    Ok(job_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_lowercases_and_replaces_bad_chars() {
        assert_eq!(sanitize_label("Alice"), "alice");
        assert_eq!(sanitize_label("user@github"), "user-github");
    }

    #[test]
    fn sanitize_label_trims_to_63() {
        let long = "x".repeat(80);
        let out = sanitize_label(&long);
        assert!(out.len() <= 63);
        assert_eq!(&out, &long[..63]);
    }

    #[test]
    fn sanitize_label_empty_falls_back() {
        assert_eq!(sanitize_label(""), "unknown");
        assert_eq!(sanitize_label("@@@"), "unknown");
    }

    #[test]
    fn runner_token_is_64_hex_chars() {
        let tok = generate_runner_token();
        assert_eq!(tok.len(), 64);
        assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
        // Very unlikely to repeat.
        let tok2 = generate_runner_token();
        assert_ne!(tok, tok2);
    }

    fn test_job() -> BenchmarkJob {
        BenchmarkJob {
            trigger_comment: None,
            id: 1,
            comment_id: 123,
            repo: "apache/datafusion".to_string(),
            pr_number: 42,
            pr_url: "https://github.com/apache/datafusion/pull/42".to_string(),
            login: "alice".to_string(),
            benchmarks: r#"["tpch"]"#.to_string(),
            env_vars: "{}".to_string(),
            baseline_env_vars: "{}".to_string(),
            changed_env_vars: "{}".to_string(),
            baseline_ref: None,
            changed_ref: None,
            job_type: "standard".to_string(),
            cpu_request: None,
            memory_request: None,
            cpu_arch: None,
            k8s_job_name: Some("bench-c123-1".to_string()),
            status: "running".to_string(),
            error_message: None,
            created_at: String::new(),
            updated_at: String::new(),
            runner_token: None,
            effective_shards: 1,
            shard_index: 0,
            assignment_version: None,
            resolved_source_json: None,
        }
    }

    #[test]
    fn criterion_worker_arguments_use_one_protocol_at_every_count() {
        let mut job = test_job();
        assert!(shard_args(&job).unwrap().is_none());
        assert!(job.shard_label().is_empty());
        job.job_type = "arrow_criterion".into();
        job.assignment_version = Some(crate::sharding::ASSIGNMENT_VERSION.into());
        job.resolved_source_json = Some(
            serde_json::to_string(&crate::sharding::FrozenSources {
                baseline_sha: "a".repeat(40),
                changed_sha: "b".repeat(40),
                pr_head_ref: "branch".into(),
            })
            .unwrap(),
        );
        for count in [1, 2, 4, 8] {
            job.effective_shards = count;
            let args = shard_args(&job).unwrap().unwrap();
            let worker = crate::sharding::WorkerShard::from_args(args.into_iter())
                .unwrap()
                .unwrap();
            assert_eq!(worker.shard.count(), count);
            assert_eq!(worker.sources.changed_sha, "b".repeat(40));
        }
    }

    /// Every field falls back to the controller default.
    #[test]
    fn placement_without_a_request_uses_the_defaults() {
        let job = test_job();
        let placement = Placement::resolve(&job, "12", "65Gi", "c4a");
        assert_eq!(placement.cpu, "12");
        assert_eq!(placement.memory, "65Gi");
        assert_eq!(placement.machine_family, "c4a");
        assert_eq!(placement.arch, "arm64");
    }

    #[test]
    fn placement_uses_the_requested_cpu_and_memory() {
        let mut job = test_job();
        job.cpu_request = Some("16".into());
        job.memory_request = Some("128Gi".into());
        let placement = Placement::resolve(&job, "12", "65Gi", "c4a");
        assert_eq!(placement.cpu, "16");
        assert_eq!(placement.memory, "128Gi");
        // Untouched by a size-only request.
        assert_eq!(placement.machine_family, "c4a");
    }

    /// One field at a time: a CPU request must not also move the pod off the
    /// default memory.
    #[test]
    fn placement_mixes_a_request_with_the_defaults() {
        let mut job = test_job();
        job.cpu_request = Some("32".into());
        let placement = Placement::resolve(&job, "12", "65Gi", "c4a");
        assert_eq!(placement.cpu, "32");
        assert_eq!(placement.memory, "65Gi");
    }

    #[test]
    fn placement_maps_arch_to_a_machine_family() {
        let mut job = test_job();

        job.cpu_arch = Some("amd64".into());
        let placement = Placement::resolve(&job, "12", "65Gi", "c4a");
        assert_eq!(placement.machine_family, "c4");
        assert_eq!(placement.arch, "amd64");

        job.cpu_arch = Some("arm64".into());
        let placement = Placement::resolve(&job, "12", "65Gi", "c4");
        assert_eq!(placement.machine_family, "c4a");
        assert_eq!(placement.arch, "arm64");
    }

    /// Rows written before the trigger could ask for an architecture.
    #[test]
    fn placement_accepts_the_legacy_x86_64_alias() {
        let mut job = test_job();
        job.cpu_arch = Some("x86_64".into());
        let placement = Placement::resolve(&job, "12", "65Gi", "c4a");
        assert_eq!(placement.machine_family, "c4");
        assert_eq!(placement.arch, "amd64");
    }

    #[test]
    fn shard_timeout_identifies_the_independent_worker() {
        let mut job = test_job();
        job.effective_shards = 4;
        job.shard_index = 2;
        job.assignment_version = Some(crate::sharding::ASSIGNMENT_VERSION.into());
        let body = terminal_failure_comment_body(7200, None, &job, "DeadlineExceeded", "", "");
        assert!(body.contains("shard 3/4"));
        assert!(body.contains("7200s job deadline"));
    }

    #[test]
    fn terminal_failure_comment_includes_non_deadline_reason_and_message() {
        let body = terminal_failure_comment_body(
            7200,
            None,
            &test_job(),
            "BackoffLimitExceeded",
            "Pod runner exited with status 1",
            "",
        );

        assert!(body.contains("failed before finishing"));
        assert!(body.contains("BackoffLimitExceeded"));
        assert!(body.contains("Pod runner exited with status 1"));
    }

    /// The Kubernetes reason never names a cause, so the runner's own log tail
    /// is what makes the comment actionable. It leads the details, ahead of the
    /// Kubernetes message.
    #[test]
    fn terminal_failure_comment_quotes_the_runner_log() {
        let body = terminal_failure_comment_body(
            7200,
            None,
            &test_job(),
            "BackoffLimitExceeded",
            "Job has reached the specified backoff limit",
            "error: target `multi_group_by` in package `datafusion-physical-plan` requires the features: `test_utils`",
        );

        assert!(body.contains("<details><summary>Runner log (last 40 lines)</summary>"));
        assert!(body.contains("requires the features: `test_utils`"));
        assert!(
            body.find("Runner log").unwrap() < body.find("Kubernetes message").unwrap(),
            "the cause should come before the Kubernetes reason"
        );
    }

    /// An unreadable pod log drops the section rather than the notification.
    #[test]
    fn terminal_failure_comment_omits_an_empty_runner_log() {
        let body = terminal_failure_comment_body(
            7200,
            None,
            &test_job(),
            "BackoffLimitExceeded",
            "Job has reached the specified backoff limit",
            "",
        );

        assert!(!body.contains("Runner log"));
        assert!(body.contains("Kubernetes message"));
    }

    /// A log tail is pod output, so it gets the same fence widening as the
    /// Kubernetes message: a benchmark that prints a fence cannot break out of
    /// the block and inject Markdown into the comment.
    #[test]
    fn terminal_failure_comment_uses_safe_fence_for_runner_log() {
        let body = terminal_failure_comment_body(
            7200,
            None,
            &test_job(),
            "BackoffLimitExceeded",
            "",
            "runner output:\n```\nuntrusted markdown\n```",
        );

        assert!(body.contains("````\nrunner output:\n```\nuntrusted markdown\n```\n````"));
    }

    #[test]
    fn deadline_exceeded_comment_keeps_existing_text() {
        let body =
            terminal_failure_comment_body(7200, None, &test_job(), "DeadlineExceeded", "", "");

        assert_eq!(
            body,
            "Benchmark for [this request](https://github.com/apache/datafusion/pull/42#issuecomment-123) hit the 7200s job deadline before finishing.\n\nBenchmarks requested: `tpch`"
        );
    }

    #[test]
    fn terminal_failure_comment_uses_safe_fence_for_kubernetes_message() {
        let body = terminal_failure_comment_body(
            7200,
            None,
            &test_job(),
            "BackoffLimitExceeded",
            "pod output:\n```\nuntrusted markdown\n```",
            "",
        );

        assert!(body.contains("````\npod output:\n```\nuntrusted markdown\n```\n````"));
    }
}
