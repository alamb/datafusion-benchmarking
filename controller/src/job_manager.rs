//! Kubernetes Job reconciliation loop.
//!
//! Drives benchmark jobs through their lifecycle: creates K8s Jobs for
//! pending rows and polls running jobs for completion.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, EnvVarSource, EphemeralVolumeSource, PersistentVolumeClaimTemplate, PodSpec,
    PodTemplateSpec, ResourceRequirements, SecretKeySelector, Toleration, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};
use kube::Client as KubeClient;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::config::Config;
use crate::db;
use crate::github::GitHubClient;
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
        if let Err(e) = reconcile_active(&config, &pool, &kube_client).await {
            warn!(error = %e, "reconcile active error");
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
        match create_k8s_job(config, &jobs_api, &job).await {
            Ok(k8s_name) => {
                info!(job_id = job.id, k8s_name = %k8s_name, "created k8s job");
                db::update_job_status(pool, job.id, JobStatus::Running, Some(&k8s_name), None)
                    .await?;

                // Post "running" comment, linking back to the triggering comment
                let comment_url = format!("{}#issuecomment-{}", job.pr_url, job.comment_id);
                let msg = format!(
                    "Benchmark job started for [this request]({comment_url}) (job `{k8s_name}`). \
                     Results will be posted here when complete.",
                );
                if let Err(e) = gh.post_comment(&job.repo, job.pr_number, &msg).await {
                    warn!(error = %e, "failed to post running comment");
                }
            }
            Err(e) => {
                warn!(job_id = job.id, error = %e, "failed to create k8s job");
                db::update_job_status(
                    pool,
                    job.id,
                    JobStatus::Failed,
                    None,
                    Some(&format!("Failed to create K8s Job: {e}")),
                )
                .await?;

                let comment_url = format!("{}#issuecomment-{}", job.pr_url, job.comment_id);
                let msg =
                    format!("Failed to start benchmark for [this request]({comment_url}): {e}",);
                if let Err(e) = gh.post_comment(&job.repo, job.pr_number, &msg).await {
                    warn!(error = %e, "failed to post error comment");
                }
            }
        }
    }

    Ok(())
}

/// Check K8s Job status for all running benchmark rows and transition to completed/failed.
#[tracing::instrument(skip_all)]
async fn reconcile_active(config: &Config, pool: &SqlitePool, kube: &KubeClient) -> Result<()> {
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
                let is_terminal_failure = status
                    .and_then(|s| s.conditions.as_ref())
                    .map(|conds| {
                        conds
                            .iter()
                            .any(|c| c.type_ == "Failed" && c.status == "True")
                    })
                    .unwrap_or(false);

                if succeeded > 0 {
                    info!(job_id = job.id, "job completed successfully");
                    db::update_job_status(pool, job.id, JobStatus::Completed, None, None).await?;
                } else if is_terminal_failure {
                    let failed_count = status.and_then(|s| s.failed).unwrap_or(0);
                    info!(
                        job_id = job.id,
                        failed_count, "job failed after all retries"
                    );
                    db::update_job_status(
                        pool,
                        job.id,
                        JobStatus::Failed,
                        None,
                        Some("K8s job failed after all retries"),
                    )
                    .await?;
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                warn!(
                    job_id = job.id,
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
                warn!(job_id = job.id, error = %e, "error checking k8s job");
            }
        }
    }

    Ok(())
}

/// Shorthand for constructing a plain-value [`EnvVar`].
fn env_var(name: &str, value: impl Into<String>) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        ..Default::default()
    }
}

/// Build and submit a K8s Job spec for a benchmark row.
///
/// Resource defaults come from [`Config`]. Tolerates GKE spot instances.
/// Uses an ephemeral volume at `/workspace` for build artifacts.
/// `GITHUB_TOKEN` is injected from the `github-token` Secret.
#[tracing::instrument(skip(config, jobs_api), fields(job_id = job.id, pr_number = job.pr_number))]
async fn create_k8s_job(
    config: &Config,
    jobs_api: &Api<Job>,
    job: &BenchmarkJob,
) -> Result<String> {
    let benchmarks: Vec<String> = serde_json::from_str(&job.benchmarks)?;
    let user_env_vars: Vec<String> = serde_json::from_str(&job.env_vars)?;

    let job_name = format!("bench-{}", job.id);

    let cpu = job.cpu_request.as_deref().unwrap_or(&config.default_cpu);
    let memory = job
        .memory_request
        .as_deref()
        .unwrap_or(&config.default_memory);

    let mut env = vec![
        env_var("PR_URL", job.pr_url.clone()),
        env_var("COMMENT_ID", job.comment_id.to_string()),
        env_var("BENCHMARKS", benchmarks.join(" ")),
        env_var("BENCH_TYPE", job.job_type.clone()),
        env_var("REPO", job.repo.clone()),
        EnvVar {
            name: "GITHUB_TOKEN".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: "github-token".into(),
                    key: "token".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];

    // Add user-specified env vars
    for ev in &user_env_vars {
        if let Some((k, v)) = ev.split_once('=') {
            env.push(env_var(k, v));
        }
    }

    // For criterion benchmarks, set BENCH_NAME to the first benchmark
    if (job.job_type == "criterion" || job.job_type == "arrow_criterion") && !benchmarks.is_empty()
    {
        env.push(env_var("BENCH_NAME", benchmarks[0].clone()));
    }

    let mut resource_requests = BTreeMap::new();
    resource_requests.insert("cpu".to_string(), Quantity(cpu.to_string()));
    resource_requests.insert("memory".to_string(), Quantity(memory.to_string()));
    resource_requests.insert(
        "ephemeral-storage".to_string(),
        Quantity(config.ephemeral_storage.clone()),
    );

    // Map per-job cpu_arch (arm64/amd64) to a machine family, or use the config default.
    let machine_family = match job.cpu_arch.as_deref() {
        Some("arm64") => "c4a",
        Some("amd64") | Some("x86_64") => "c4",
        _ => &config.default_machine_family,
    };
    let arch = if machine_family == "c4a" { "arm64" } else { "amd64" };

    let mut node_selector = BTreeMap::new();
    node_selector.insert(
        "cloud.google.com/compute-class".to_string(),
        "Performance".to_string(),
    );
    node_selector.insert("cloud.google.com/machine-family".to_string(), machine_family.to_string());
    node_selector.insert("kubernetes.io/arch".to_string(), arch.to_string());

    let k8s_job = Job {
        metadata: ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(config.k8s_namespace.clone()),
            labels: Some({
                let mut l = BTreeMap::new();
                l.insert("app".to_string(), "benchmark-runner".to_string());
                l.insert("job-id".to_string(), job.id.to_string());
                l.insert("comment-id".to_string(), job.comment_id.to_string());
                l
            }),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(3),
            active_deadline_seconds: Some(config.active_deadline_secs),
            ttl_seconds_after_finished: Some(config.ttl_after_finished_secs),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some({
                        let mut l = BTreeMap::new();
                        l.insert("app".to_string(), "benchmark-runner".to_string());
                        l.insert("job-id".to_string(), job.id.to_string());
                        l.insert("comment-id".to_string(), job.comment_id.to_string());
                        l
                    }),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
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
                        env: Some(env),
                        resources: Some(ResourceRequirements {
                            requests: Some(resource_requests),
                            ..Default::default()
                        }),
                        volume_mounts: Some(vec![VolumeMount {
                            name: "workspace".into(),
                            mount_path: "/workspace".into(),
                            ..Default::default()
                        }]),
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
