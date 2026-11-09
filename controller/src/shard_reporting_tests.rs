use super::*;
use crate::{
    db,
    models::{JobInsert, JobStatus},
    resources::PodResources,
    sharding::Shard,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};

pub(crate) async fn request(
    pool: &SqlitePool,
    comment_id: i64,
    count: u32,
    targets: &[&str],
) -> Vec<BenchmarkJob> {
    let resources = PodResources::default();
    let sources = serde_json::to_string(&crate::sharding::FrozenSources {
        baseline_sha: "a".repeat(40),
        changed_sha: "b".repeat(40),
        pr_head_ref: "branch".into(),
    })
    .unwrap();
    let names: Vec<_> = targets
        .iter()
        .map(|name| serde_json::to_string(&[name]).unwrap())
        .collect();
    let mut jobs = Vec::new();
    for name in &names {
        for index in 0..count {
            jobs.push(JobInsert {
                trigger_comment: None,
                comment_id,
                repo: "apache/arrow-rs",
                pr_number: 42,
                pr_url: "https://github.com/apache/arrow-rs/pull/42",
                login: "alice",
                benchmarks: name,
                env_vars: "{}",
                baseline_env_vars: "{}",
                changed_env_vars: "{}",
                baseline_ref: None,
                changed_ref: None,
                job_type: "arrow_criterion",
                resources: &resources,
                shard: Shard { count, index },
                resolved_source_json: Some(&sources),
            });
        }
    }
    db::enqueue_jobs(pool, &jobs, "2026-01-01").await.unwrap();
    sqlx::query_as(
        "SELECT * FROM benchmark_jobs WHERE comment_id = ? ORDER BY benchmarks, shard_index",
    )
    .bind(comment_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn record(name: &str, id: &str, ns: f64) -> Value {
    let stats = serde_json::json!({
        "confidence_interval": { "confidence_level": 0.95, "lower_bound": ns - 1.0, "upper_bound": ns + 1.0 },
        "point_estimate": ns, "standard_error": 1.0,
    });
    serde_json::json!({
        "baseline": name, "fullname": format!("{name}/{id}"),
        "criterion_benchmark_v1": { "group_id": id, "function_id": null, "value_str": null,
            "throughput": null, "full_id": id, "directory_name": id },
        "criterion_estimates_v1": { "mean": stats, "median": stats, "median_abs_dev": stats,
            "slope": null, "std_dev": stats },
        "future_metadata": { "preserved": true },
    })
}

pub(crate) fn result(job: &BenchmarkJob) -> ShardResult {
    let mut base = Baseline::empty("base");
    let mut changed = Baseline::empty("changed");
    let target = target(job).unwrap();
    for id in ["common", "界/λ [10]+?(x)|`", "added", "removed"] {
        if job.shard().unwrap().owns(&target, id) {
            if id != "added" {
                base.benchmarks.insert(id.into(), record("base", id, 100.0));
            }
            if id != "removed" {
                changed
                    .benchmarks
                    .insert(id.into(), record("changed", id, 80.0));
            }
        }
    }
    ShardResult {
        base: Some(base),
        changed,
        info: RunnerInfo {
            node_name: format!("node-{}", job.id),
            instance: "c4a".into(),
            resources: "12 CPUs".into(),
            uname: format!("Linux runner-{}", job.id),
            resource_report: "peak RSS: 1 GiB".into(),
            cpu_details: format!("CPU dump for runner {}", job.id),
            bench_command: format!("cargo bench --bench {}", target),
        },
    }
}

/// Stateful mock accepts GitHub list/create requests and records actual posts.
/// Reusing it with reopened pools exercises recovery without posting to GitHub.
pub(crate) async fn github() -> (
    GitHubClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let (client, comments, _, server) = github_with_gists().await;
    (client, comments, server)
}

#[derive(Default)]
pub(crate) struct GistMock {
    pub gists: Vec<Value>,
    pub gets: usize,
    pub posts: usize,
    pub next_status: Option<u16>,
    pub lose_gist_response: bool,
    pub lose_comment_response: bool,
}

pub(crate) async fn github_with_gists() -> (
    GitHubClient,
    Arc<Mutex<Vec<Value>>>,
    Arc<Mutex<GistMock>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = GitHubClient::test_client(format!("http://{}", listener.local_addr().unwrap()));
    let comments = Arc::new(Mutex::new(Vec::<Value>::new()));
    let stored = comments.clone();
    let gists = Arc::new(Mutex::new(GistMock::default()));
    let gist_state = gists.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let header_end = loop {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let header = String::from_utf8_lossy(&request[..header_end]).to_string();
            let length: usize = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .map(|n| n.parse().unwrap())
                .unwrap_or(0);
            while request.len() < header_end + length {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
            }
            let path = header.split_whitespace().nth(1).unwrap();
            let mut status = 200;
            let response = if path.starts_with("/gists") {
                assert!(header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token"));
                let mut state = gist_state.lock().await;
                if header.starts_with("POST ") {
                    state.posts += 1;
                    if let Some(code) = state.next_status.take() {
                        status = code;
                        serde_json::json!({"message": "Gist publication rejected"})
                    } else {
                        let mut gist: Value =
                            serde_json::from_slice(&request[header_end..header_end + length])
                                .unwrap();
                        assert_eq!(gist["public"], false);
                        assert_eq!(gist["files"].as_object().unwrap().len(), 1);
                        assert!(gist["files"]["comparison.txt"]["content"].is_string());
                        gist["html_url"] = format!(
                            "https://gist.github.com/benchmark/{}",
                            state.gists.len() + 1
                        )
                        .into();
                        state.gists.push(gist.clone());
                        if std::mem::take(&mut state.lose_gist_response) {
                            continue;
                        }
                        gist
                    }
                } else {
                    assert!(header.starts_with("GET "));
                    state.gets += 1;
                    let url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
                    let page: usize = url
                        .query_pairs()
                        .find(|(key, _)| key == "page")
                        .unwrap()
                        .1
                        .parse()
                        .unwrap();
                    serde_json::to_value(
                        state
                            .gists
                            .iter()
                            .skip((page - 1) * 100)
                            .take(100)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap()
                }
            } else if header.starts_with("POST ") {
                let body: Value =
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                let mut comments = stored.lock().await;
                let comment =
                    serde_json::json!({ "id": comments.len() + 100, "body": body["body"] });
                comments.push(comment.clone());
                if std::mem::take(&mut gist_state.lock().await.lose_comment_response) {
                    continue;
                }
                comment
            } else {
                assert!(header.starts_with("GET "), "{header}");
                if header.lines().next().unwrap().contains("/issues/") {
                    serde_json::to_value(&*stored.lock().await).unwrap()
                } else {
                    serde_json::json!({
                        "head": { "sha": "b".repeat(40), "ref": "feature" },
                        "merge_base_commit": { "sha": "a".repeat(40) },
                        "sha": "a".repeat(40), "default_branch": "main"
                    })
                }
            };
            let json = serde_json::to_string(&response).unwrap();
            let response = format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len());
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (client, comments, gists, server)
}

#[tokio::test]
async fn disjoint_union_preserves_estimates_added_removed_and_empty_shards() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let mut jobs = request(&pool, 1, 8, &["target"]).await;
    let mut results = BTreeMap::new();
    for job in &mut jobs {
        job.status = "completed".into();
        results.insert(job.id, result(job));
    }
    assert!(results.values().any(|r| r.changed.benchmarks.is_empty()));
    let (base, changed) = merge(&jobs.iter().collect::<Vec<_>>(), &results).unwrap();
    assert_eq!(base.benchmarks.len(), 3);
    assert_eq!(changed.benchmarks.len(), 3);
    assert_eq!(base.benchmarks["common"], record("base", "common", 100.0));
    assert_eq!(
        changed.benchmarks["common"],
        record("changed", "common", 80.0)
    );
    assert!(!base.benchmarks.contains_key("added"));
    assert!(!changed.benchmarks.contains_key("removed"));
}

#[tokio::test]
async fn direct_arrow_reports_use_the_shared_formatter_without_a_controller() {
    use crate::runner::{
        config::{BenchType, PosterMode, RunnerConfig},
        poster::CommentPoster,
    };
    let (gh, comments, gists, server) = github_with_gists().await;
    let poster = CommentPoster::Direct(gh);
    let mut config = RunnerConfig {
        trigger_comment: None,
        pr_url: "https://github.com/apache/arrow-rs/pull/42".into(),
        comment_id: "123".into(),
        comment_url: "https://github.com/apache/arrow-rs/pull/42#issuecomment-123".into(),
        benchmarks: "target".into(),
        bench_type: BenchType::ArrowCriterion,
        bench_name: "target".into(),
        bench_filter: "".into(),
        repo: "apache/arrow-rs".into(),
        poster_mode: PosterMode::Direct {
            github_token: "unused".into(),
        },
        sccache_gcs_bucket: None,
        data_cache_bucket: None,
        shared_env_vars: Default::default(),
        baseline_env_vars: Default::default(),
        changed_env_vars: Default::default(),
        baseline_ref: None,
        changed_ref: None,
        runner_repo_url: None,
        resources: Default::default(),
        shard: Default::default(),
        frozen_sources: None,
    };
    let sources = poster.criterion_sources(&config).await.unwrap();
    assert_eq!(sources.changed_sha, "b".repeat(40));
    assert_eq!(sources.baseline_sha, "a".repeat(40));
    let mut context = ExecutionContext::from_config(&config);
    context.sources = Some(sources);
    let result = ShardResult {
        base: Some(Baseline::empty("base")),
        changed: Baseline::empty("changed"),
        info: RunnerInfo {
            node_name: "direct-node".into(),
            uname: "direct-uname".into(),
            cpu_details: "direct-cpu".into(),
            bench_command: "cargo bench --bench target".into(),
            ..Default::default()
        },
    };
    for error in [None, Some("compile failed")] {
        comments.lock().await.clear();
        poster.criterion_started(&config, &context).await.unwrap();
        poster.post_runner_info(&result.info).await.unwrap();
        if let Some(error) = error {
            poster
                .criterion_error(&config, &context, &result.info, error)
                .await
                .unwrap();
        } else {
            poster
                .criterion_result(&config, &context, &result)
                .await
                .unwrap();
        }
        let comments = comments.lock().await;
        assert_eq!(comments.len(), 2);
        assert!(!comments[0]["body"].as_str().unwrap().contains("direct-cpu"));
        let body = comments[1]["body"].as_str().unwrap();
        assert!(body.contains("direct-cpu"));
        assert!(body.contains("direct-uname"));
        assert!(body.contains("direct-node"));
        assert!(body.contains("cargo bench --bench target"));
        assert!(body.contains("feature"));
        assert!(body.contains(&"a".repeat(40)));
        assert!(body.contains(&"b".repeat(40)));
        if error.is_some() {
            assert!(body.contains("compile failed"));
        } else {
            assert!(body.contains("No matching cases; no measurements run."));
        }
    }
    let mut oversized = result;
    oversized.info.cpu_details = "CPU details\n".repeat(7000);
    gists.lock().await.lose_gist_response = true;
    poster
        .criterion_result(&config, &context, &oversized)
        .await
        .unwrap();
    let body = comments.lock().await.last().unwrap()["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("Benchmark completed"));
    assert!(body.contains("https://gist.github.com/benchmark/1"));
    assert_eq!(gists.lock().await.posts, 1);
    assert_eq!(
        gists.lock().await.gists[0]["files"]["comparison.txt"]["content"],
        "No matching cases; no measurements run.\n"
    );
    config.shard = crate::sharding::Shard { count: 2, index: 0 };
    assert!(poster.criterion_sources(&config).await.is_err());
    server.abort();
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn oversized_diagnostics_keep_execution_status_and_link_to_complete_comparison() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let jobs = request(&pool, 302, 1, &["target"]).await;
    let job = &jobs[0];
    let mut data = result(job);
    data.info.cpu_details = "full CPU diagnostics\n".repeat(4000);
    store_result(&pool, job, &data).await.unwrap();
    db::update_job_status(&pool, job.id, JobStatus::Completed, None, None)
        .await
        .unwrap();
    let (gh, comments, server) = github().await;
    reconcile(&pool, &gh, None).await.unwrap();
    reconcile(&pool, &gh, None).await.unwrap();
    let comments = comments.lock().await;
    assert_eq!(comments.len(), 2);
    let body = comments[1]["body"].as_str().unwrap();
    assert!(body.contains("Benchmark completed"));
    assert!(body.contains("1/1 completed successfully; 1/1 exports received"));
    assert!(body.contains("https://gist.github.com/benchmark/1"));
    assert!(body.contains("Full runner diagnostics could not be posted"));
    assert!(!body.contains("CPU diagnostics"));
    assert!(body.chars().count() <= criterion_report::MAX_GITHUB_COMMENT_CHARS);
    let stored: String =
        sqlx::query_scalar("SELECT result_json FROM shard_results WHERE job_id = ?")
            .bind(job.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(serde_json::from_str::<ShardResult>(&stored).unwrap(), data);
    server.abort();
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn oversized_comparisons_publish_once_per_target_and_recover_lost_responses() {
    for count in [1, 4] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("db.sqlite").display());
        let pool = db::connect(&url).await.unwrap();
        let jobs = request(&pool, 304, count, &["target", "another"]).await;
        let (gh, comments, gists, server) = github_with_gists().await;
        let mut exports = BTreeMap::new();
        for job in &jobs {
            ensure_started(&pool, &gh, job, None).await.unwrap();
            let mut data = result(job);
            for index in 0..400 {
                let id = format!("case/{index:04}/{}", "long-name-λ-".repeat(15));
                if job.shard().unwrap().owns(&target(job).unwrap(), &id) {
                    data.base
                        .as_mut()
                        .unwrap()
                        .benchmarks
                        .insert(id.clone(), record("base", &id, 100.0));
                    data.changed
                        .benchmarks
                        .insert(id.clone(), record("changed", &id, 80.0));
                }
            }
            store_result(&pool, job, &data).await.unwrap();
            exports.insert(job.id, data);
            db::update_job_status(&pool, job.id, JobStatus::Completed, None, None)
                .await
                .unwrap();
        }
        gists.lock().await.lose_gist_response = true;
        reconcile(&pool, &gh, None).await.unwrap();
        // One Gist POST succeeded but its response was lost. The other target
        // still finishes independently, without waiting for the retry.
        assert_eq!(gists.lock().await.gists.len(), 2);
        assert_eq!(comments.lock().await.len(), 3);
        gists.lock().await.lose_comment_response = true;
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(gists.lock().await.posts, 2);
        assert_eq!(comments.lock().await.len(), 4);
        let gets = gists.lock().await.gets;
        pool.close().await;
        let pool = db::connect(&url).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(
            gists.lock().await.gets,
            gets,
            "persisted Gists need no lookup"
        );
        assert_eq!(gists.lock().await.posts, 2);
        assert_eq!(comments.lock().await.len(), 4);
        for name in ["target", "another"] {
            let (link, finished): (String, i64) = sqlx::query_as("SELECT comparison_gist_url, finished_comment_id FROM sharded_runs WHERE comment_id = 304 AND benchmarks = ?")
                .bind(serde_json::to_string(&[name]).unwrap()).fetch_one(&pool).await.unwrap();
            let group: Vec<_> = jobs.iter().filter(|j| target(j).unwrap() == name).collect();
            let (base, changed) = merge(&group, &exports).unwrap();
            let native = compare(&base, &changed).await.unwrap();
            assert!(native.chars().count() > criterion_report::MAX_GITHUB_COMMENT_CHARS);
            let state = gists.lock().await;
            let gist = state.gists.iter().find(|g| g["html_url"] == link).unwrap();
            assert_eq!(gist["files"]["comparison.txt"]["content"], native);
            let comments = comments.lock().await;
            let body = comments.iter().find(|c| c["id"] == finished).unwrap()["body"]
                .as_str()
                .unwrap();
            assert!(body.contains("Benchmark completed"));
            assert!(body.contains(&link));
            assert!(!body.contains("case/0000"));
            assert!(body.chars().count() <= criterion_report::MAX_GITHUB_COMMENT_CHARS);
            for job in group {
                assert!(body.contains(&exports[&job.id].info.cpu_details));
                assert!(body.contains(&exports[&job.id].info.resource_report));
            }
        }
        server.abort();
    }
}

#[tokio::test]
async fn gist_migration_upgrades_schema_five_without_reposting_completed_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let url = format!("sqlite://{}", path.display());
    let pool = SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    let mut deployed = sqlx::migrate!("./migrations");
    deployed.migrations.to_mut().retain(|m| m.version <= 5);
    deployed.run(&pool).await.unwrap();
    // Raw SQL, because `db::enqueue_jobs` writes columns that schema 5 lacks.
    let jobs = legacy_request(&pool, 305, 1, &["target"], "arrow_criterion").await;
    sqlx::query("INSERT INTO sharded_runs (comment_id, benchmarks) VALUES (305, ?)")
        .bind(&jobs[0].benchmarks)
        .execute(&pool)
        .await
        .unwrap();
    let data = result(&jobs[0]);
    store_result(&pool, &jobs[0], &data).await.unwrap();
    db::update_job_status(&pool, jobs[0].id, JobStatus::Completed, None, None)
        .await
        .unwrap();
    sqlx::query("UPDATE sharded_runs SET started_comment_id = 100, finished_comment_id = 101 WHERE comment_id = 305").execute(&pool).await.unwrap();
    let keys: (String, String) = sqlx::query_as("SELECT start_key, finish_key FROM sharded_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    let pool = db::connect(&url).await.unwrap();
    let after: (String, String, i64, i64, Option<String>) = sqlx::query_as("SELECT start_key, finish_key, started_comment_id, finished_comment_id, comparison_gist_url FROM sharded_runs").fetch_one(&pool).await.unwrap();
    assert_eq!(after, (keys.0, keys.1, 100, 101, None));
    let stored: String = sqlx::query_scalar("SELECT result_json FROM shard_results")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(serde_json::from_str::<ShardResult>(&stored).unwrap(), data);
    let (gh, comments, gists, server) = github_with_gists().await;
    reconcile(&pool, &gh, None).await.unwrap();
    assert!(comments.lock().await.is_empty());
    assert_eq!(gists.lock().await.gets, 0);
    server.abort();
}

#[test]
fn legacy_exports_deserialize_with_optional_runner_metadata() {
    let json = serde_json::json!({ "base": null, "changed": Baseline::empty("changed"),
        "instance": "c4", "resources": "4 CPU", "resource_report": "usage", "cpu_details": "CPU" });
    let result: ShardResult = serde_json::from_value(json).unwrap();
    assert_eq!(result.info.cpu_details, "CPU");
    assert_eq!(result.info.node_name, "");
}

#[test]
fn duplicate_baselines_and_wrong_identities_are_rejected() {
    let mut base = Baseline::empty("base");
    base.benchmarks
        .insert("case".into(), record("base", "case", 100.0));
    assert!(base.extend(&base.clone()).is_err());
    assert!(base.validate("changed").is_err());
    base.benchmarks.get_mut("case").unwrap()["baseline"] = "changed".into();
    assert!(base.validate("base").is_err());
}

#[tokio::test]
async fn mismatched_jobs_and_unowned_cases_are_rejected() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let mut jobs = request(&pool, 2, 4, &["target"]).await;
    let refs = jobs.iter().collect::<Vec<_>>();
    assert!(validate_jobs(&refs[..3]).is_err());
    assert!(validate_jobs(&[refs[0], refs[0], refs[2], refs[3]]).is_err());
    jobs[0].resolved_source_json = Some("different commits".into());
    assert!(validate_jobs(&jobs.iter().collect::<Vec<_>>()).is_err());
    let foreign = (0..100)
        .map(|i| format!("case{i}"))
        .find(|id| !jobs[0].shard().unwrap().owns("target", id))
        .unwrap();
    let mut data = result(&jobs[0]);
    data.changed
        .benchmarks
        .insert(foreign.clone(), record("changed", &foreign, 80.0));
    assert!(data.validate(&jobs[0]).is_err());
}

#[tokio::test]
async fn submissions_are_immutable_idempotent_and_reject_late_first_results() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let jobs = request(&pool, 3, 4, &["target"]).await;
    let data = result(&jobs[0]);
    let (a, b) = tokio::join!(
        store_result(&pool, &jobs[0], &data),
        store_result(&pool, &jobs[0], &data)
    );
    assert!(a.unwrap() && b.unwrap());
    db::update_job_status(&pool, jobs[0].id, JobStatus::Completed, None, None)
        .await
        .unwrap();
    assert!(store_result(&pool, &jobs[0], &data).await.unwrap());
    let mut different = data;
    different.info.instance = "changed".into();
    assert!(!store_result(&pool, &jobs[0], &different).await.unwrap());
    db::update_job_status(&pool, jobs[1].id, JobStatus::Failed, None, None)
        .await
        .unwrap();
    assert!(!store_result(&pool, &jobs[1], &result(&jobs[1]))
        .await
        .unwrap());
}

#[tokio::test]
async fn failed_workers_keep_their_full_diagnostics() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let mut jobs = request(&pool, 30, 2, &["target"]).await;
    let mut infos = BTreeMap::new();
    for job in &mut jobs {
        let mut info = result(job).info;
        info.cpu_details
            .push_str("\n```\nCPU details after backticks");
        info.resource_report = "resource detail\n".repeat(200);
        store_runner_info(&pool, job.id, &info).await.unwrap();
        job.status = "failed".into();
        job.error_message = Some("compile failed".into());
        infos.insert(job.id, info);
    }
    let body = final_body(&jobs, &BTreeMap::new(), &infos, None)
        .await
        .unwrap()
        .inline_body();
    assert!(body.contains("failed or incomplete"));
    assert!(!body.contains("partial results"));
    assert!(!body.contains("Sharding factor:"));
    for info in infos.values() {
        for detail in [
            &info.node_name,
            &info.uname,
            &info.cpu_details,
            &info.bench_command,
            &info.resource_report,
        ] {
            assert!(body.contains(detail), "missing {detail}");
        }
    }
    assert!(!body.contains("truncated"));
    assert!(!body.contains("<summary>Details</summary>"));
    let context = ExecutionContext::from_job(&jobs[0]).unwrap();
    let fits = "界".repeat(criterion_report::MAX_GITHUB_COMMENT_CHARS - 100);
    assert_eq!(
        criterion_report::github_body(&context, fits.clone(), 100),
        fits
    );
    assert!(
        criterion_report::github_body(&context, format!("{fits}界"), 100)
            .contains("report could not be posted")
    );
}

#[tokio::test]
async fn each_target_reports_failures_independently_and_recovers_after_restart() {
    for count in [1, 4] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("db.sqlite").display());
        let pool = db::connect(&url).await.unwrap();
        let jobs = request(&pool, 4, count, &["target", "another"]).await;
        let (gh, comments, server) = github().await;
        for job in &jobs {
            ensure_started(&pool, &gh, job, None).await.unwrap();
            store_runner_info(&pool, job.id, &result(job).info)
                .await
                .unwrap();
        }
        assert_eq!(comments.lock().await.len(), 2);
        assert!(comments.lock().await[0]["body"]
            .as_str()
            .unwrap()
            .contains(&format!("Sharding factor: {count}")));
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(comments.lock().await.len(), 2);
        for job in &jobs[..jobs.len() - 1] {
            db::update_job_status(
                &pool,
                job.id,
                JobStatus::Failed,
                None,
                Some("DeadlineExceeded"),
            )
            .await
            .unwrap();
        }
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(comments.lock().await.len(), 3);
        let completed = comments.lock().await[2]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(completed.contains("**Target:** another\n"));
        assert!(completed.contains("DeadlineExceeded"));
        assert!(!completed.contains("run benchmark target\n"));
        db::update_job_status(
            &pool,
            jobs.last().unwrap().id,
            JobStatus::Completed,
            None,
            None,
        )
        .await
        .unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(comments.lock().await.len(), 4);
        let body = comments.lock().await[3]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(body.contains("**Target:** target\n"));
        assert!(!body.contains("run benchmark another\n"));
        assert!(body.contains("without submitting Criterion exports"));
        assert!(body.contains("failed or incomplete"));
        assert!(body.contains(&result(jobs.last().unwrap()).info.cpu_details));
        assert!(!body.contains(&result(&jobs[0]).info.cpu_details));
        // Simulate a crash after the GitHub POST but before recording its ID.
        sqlx::query(
            "UPDATE sharded_runs SET started_comment_id = NULL, finished_comment_id = NULL",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = db::connect(&url).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        assert_eq!(comments.lock().await.len(), 4);
        server.abort();
    }
}

#[tokio::test]
async fn cleanup_removes_only_expired_execution_units() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let jobs = request(&pool, 7, 2, &["target", "another"]).await;
    for job in &jobs {
        store_result(&pool, job, &result(job)).await.unwrap();
        store_runner_info(&pool, job.id, &result(job).info)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE benchmark_jobs SET updated_at = '2000-01-01' WHERE benchmarks = ?")
        .bind(&jobs[0].benchmarks)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(db::cleanup_old_jobs(&pool, 30).await.unwrap(), 2);
    let remaining: Vec<String> = sqlx::query_scalar("SELECT benchmarks FROM sharded_runs")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, vec![jobs[2].benchmarks.clone()]);
    let exports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shard_results")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(exports, 2);
    sqlx::query("UPDATE benchmark_jobs SET updated_at = '2000-01-01'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(db::cleanup_old_jobs(&pool, 30).await.unwrap(), 2);
    for table in ["shard_results", "sharded_runs", "runner_metadata"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

/// Seed the deployed schema without the collector's enqueue/notification tables.
async fn legacy_request(
    pool: &SqlitePool,
    comment_id: i64,
    count: u32,
    targets: &[&str],
    job_type: &str,
) -> Vec<BenchmarkJob> {
    let repo = if job_type == "arrow_criterion" {
        "apache/arrow-rs"
    } else {
        "apache/datafusion"
    };
    db::mark_comment_seen(pool, comment_id, repo, 42, "alice", "2026-01-01")
        .await
        .unwrap();
    let sources = serde_json::json!({
        "baseline_sha": "a".repeat(40), "changed_sha": "b".repeat(40), "pr_head_ref": "branch"
    })
    .to_string();
    for target in targets {
        for index in 0..count {
            sqlx::query("INSERT INTO benchmark_jobs (comment_id, repo, pr_number, pr_url, login, benchmarks, job_type, effective_shards, shard_index, assignment_version, resolved_source_json) VALUES (?, ?, 42, ?, 'alice', ?, ?, ?, ?, ?, ?)")
                .bind(comment_id).bind(repo).bind(format!("https://github.com/{repo}/pull/42"))
                .bind(serde_json::to_string(&[target]).unwrap()).bind(job_type).bind(count).bind(index)
                .bind((count > 1).then_some(crate::sharding::ASSIGNMENT_VERSION))
                .bind((count > 1).then_some(&sources))
                .execute(pool).await.unwrap();
        }
    }
    sqlx::query_as(
        "SELECT * FROM benchmark_jobs WHERE comment_id = ? ORDER BY benchmarks, shard_index",
    )
    .bind(comment_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn migration_from_deployed_schema_preserves_jobs_and_reports_only_outstanding_targets() {
    for count in [1, 4] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let url = format!("sqlite://{}", path.display());
        let pool = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        let mut deployed = sqlx::migrate!("./migrations");
        deployed.migrations.to_mut().retain(|m| m.version <= 4);
        deployed.run(&pool).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![1, 2, 3, 4]);

        let history = legacy_request(&pool, 80, count, &["alpha", "beta"], "arrow_criterion").await;
        for (index, job) in history.iter().enumerate() {
            let status = if index % 2 == 0 {
                JobStatus::Completed
            } else {
                JobStatus::Failed
            };
            db::update_job_status(&pool, job.id, status, None, None)
                .await
                .unwrap();
        }
        let active = legacy_request(
            &pool,
            81,
            count,
            &["alpha", "beta", "queued"],
            "arrow_criterion",
        )
        .await;
        for job in &active {
            let status = match target(job).unwrap().as_str() {
                "alpha" => JobStatus::Completed,
                "beta" => JobStatus::Running,
                _ => JobStatus::Pending,
            };
            db::update_job_status(&pool, job.id, status, None, None)
                .await
                .unwrap();
        }
        legacy_request(&pool, 82, 1, &["tpch"], "datafusion").await;
        type JobState = (i64, String, Option<String>, Option<String>);
        let job_state = "SELECT id, status, assignment_version, resolved_source_json FROM benchmark_jobs ORDER BY id";
        let before: Vec<JobState> = sqlx::query_as(job_state).fetch_all(&pool).await.unwrap();
        pool.close().await;

        // Use the production upgrade path, including SQLx's checksum checks.
        let pool = db::connect(&url).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7]);
        let after: Vec<JobState> = sqlx::query_as(job_state).fetch_all(&pool).await.unwrap();
        assert_eq!(before, after);
        let jobs: Vec<BenchmarkJob> = sqlx::query_as("SELECT * FROM benchmark_jobs")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(jobs.iter().all(|j| j.shard().is_ok()));

        type ExecutionState = (i64, String, Option<i64>, Option<i64>, String, String);
        let rows: Vec<ExecutionState> = sqlx::query_as(
            "SELECT comment_id, benchmarks, started_comment_id, finished_comment_id, start_key, finish_key FROM sharded_runs ORDER BY comment_id, benchmarks")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 5); // DataFusion does not acquire collector state.
        let mut markers = BTreeSet::new();
        for (comment_id, benchmarks, start, finish, start_key, finish_key) in rows {
            assert_eq!(start, Some(0));
            let terminal = comment_id == 80 || benchmarks == r#"["alpha"]"#;
            assert_eq!(finish, terminal.then_some(0));
            assert!(markers.insert(start_key));
            assert!(markers.insert(finish_key));
        }
        let (gh, comments, server) = github().await;
        reconcile(&pool, &gh, None).await.unwrap();
        assert!(comments.lock().await.is_empty());
        for job in active.iter().filter(|j| target(j).unwrap() == "beta") {
            db::update_job_status(&pool, job.id, JobStatus::Completed, None, None)
                .await
                .unwrap();
        }
        reconcile(&pool, &gh, None).await.unwrap();
        pool.close().await;
        let pool = db::connect(&url).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        {
            let comments = comments.lock().await;
            assert_eq!(comments.len(), 1);
            let body = comments[0]["body"].as_str().unwrap();
            assert!(body.contains("**Target:** beta"));
            assert!(body.contains("without submitting Criterion exports"));
            if count == 1 {
                assert!(body.contains("Commit identities unavailable"));
            }
        }

        let new = request(&pool, 83, count, &["new"]).await;
        let job = &new[0];
        assert!(store_result(&pool, job, &result(job)).await.unwrap());
        assert!(store_runner_info(&pool, job.id, &result(job).info)
            .await
            .unwrap());
        ensure_started(&pool, &gh, job, None).await.unwrap();
        assert_eq!(comments.lock().await.len(), 2);
        assert!(comments.lock().await[1]["body"]
            .as_str()
            .unwrap()
            .contains("**Target:** new"));
        server.abort();
    }
}

#[tokio::test]
async fn the_same_target_in_different_requests_has_independent_notifications() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let first = request(&pool, 100, 2, &["target"]).await;
    let second = request(&pool, 101, 2, &["target"]).await;
    let (gh, comments, server) = github().await;
    for job in first.iter().chain(&second) {
        ensure_started(&pool, &gh, job, None).await.unwrap();
    }
    assert_eq!(comments.lock().await.len(), 2);
    for jobs in [&first, &second] {
        for job in jobs {
            db::update_job_status(
                &pool,
                job.id,
                JobStatus::Failed,
                None,
                Some("DeadlineExceeded"),
            )
            .await
            .unwrap();
        }
        reconcile(&pool, &gh, None).await.unwrap();
        let comments = comments.lock().await;
        let body = comments.last().unwrap()["body"].as_str().unwrap();
        assert!(body.contains(&format!("#issuecomment-{}", jobs[0].comment_id)));
    }
    assert_eq!(comments.lock().await.len(), 4);
    server.abort();
}

#[tokio::test]
async fn reports_reject_mixed_execution_units() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let jobs = request(&pool, 102, 1, &["alpha", "beta"]).await;
    assert!(final_body(&jobs, &BTreeMap::new(), &BTreeMap::new(), None)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn real_critcmp_reports_each_execution_unit_after_its_shards_complete() {
    for (count, targets) in [
        (1, &["target", "another"][..]),
        (2, &["target", "another"][..]),
        (4, &["target", "another"][..]),
        (8, &["target"][..]), // Keep the request within the per-user queue cap.
    ] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("db.sqlite").display());
        let pool = db::connect(&url).await.unwrap();
        let jobs = request(&pool, 5, count, targets).await;
        assert_eq!(jobs.len(), targets.len() * count as usize);
        let units: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sharded_runs WHERE comment_id = 5")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(units as usize, targets.len());
        let (gh, comments, server) = github().await;
        for job in &jobs {
            ensure_started(&pool, &gh, job, None).await.unwrap();
        }
        assert_eq!(comments.lock().await.len(), targets.len());
        for name in targets {
            let comments = comments.lock().await;
            let body = comments
                .iter()
                .map(|c| c["body"].as_str().unwrap())
                .find(|body| body.contains(&format!("**Target:** {name}\n")))
                .unwrap();
            assert!(body.contains(&format!("Sharding factor: {count}** ({count} workers)")));
            assert!(body.contains(&format!("run benchmark {name}\n")));
        }
        let mut completed = BTreeMap::<String, usize>::new();
        for job in jobs.iter().rev() {
            let mut data = result(job);
            if target(job).unwrap() == "another" && data.changed.benchmarks.contains_key("common") {
                data.changed
                    .benchmarks
                    .insert("common".into(), record("changed", "common", 120.0));
            }
            store_result(&pool, job, &data).await.unwrap();
            db::update_job_status(&pool, job.id, JobStatus::Completed, None, None)
                .await
                .unwrap();
            reconcile(&pool, &gh, None).await.unwrap();
            *completed.entry(job.benchmarks.clone()).or_default() += 1;
            let reported = completed.values().filter(|&&n| n == count as usize).count();
            assert_eq!(comments.lock().await.len(), targets.len() + reported);
        }
        pool.close().await;
        let pool = db::connect(&url).await.unwrap();
        reconcile(&pool, &gh, None).await.unwrap();
        let comments = comments.lock().await;
        assert_eq!(comments.len(), targets.len() * 2);
        let ids: BTreeSet<_> = comments.iter().map(|c| c["id"].as_i64().unwrap()).collect();
        assert_eq!(ids.len(), comments.len());
        for name in targets {
            let matching: Vec<_> = comments
                .iter()
                .map(|c| c["body"].as_str().unwrap())
                .filter(|body| body.contains(&format!("**Target:** {name}\n")))
                .collect();
            assert_eq!(matching.len(), 2);
            let body = matching
                .iter()
                .find(|body| body.contains("Benchmark completed"))
                .unwrap();
            assert_eq!(body.lines().filter(|l| l.starts_with("common ")).count(), 1);
            assert_eq!(body.lines().filter(|l| l.starts_with("added ")).count(), 1);
            assert!(body.contains("<summary>Details</summary>\n\n```\ngroup"));
            assert!(!body.contains("| Benchmark |"));
            let group: Vec<_> = jobs
                .iter()
                .filter(|j| target(j).unwrap() == *name)
                .collect();
            let mut exports = BTreeMap::new();
            for job in &group {
                let json: String =
                    sqlx::query_scalar("SELECT result_json FROM shard_results WHERE job_id = ?")
                        .bind(job.id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                exports.insert(job.id, serde_json::from_str::<ShardResult>(&json).unwrap());
            }
            let (base, changed) = merge(&group, &exports).unwrap();
            let native = compare(&base, &changed).await.unwrap();
            assert_eq!(
                body.matches(&native).count(),
                1,
                "native output must be preserved exactly once"
            );
            assert!(!body.contains("Sharding factor:"));
            assert!(body.contains(&format!("shards: {count}\n")));
            for index in 1..=count {
                assert!(body.contains(&format!("shard {index}/{count}</summary>")));
            }
            assert!(body.contains(&format!("run benchmark {name}\n")));
            assert!(body.contains("界/λ"));
            for other in targets.iter().filter(|other| *other != name) {
                assert!(!body.contains(&format!("run benchmark {other}\n")));
                assert!(!body.contains(&format!("**Target:** {other}\n")));
            }
            let row = if *name == "another" {
                "common 1.00 100.0±100.00ns ? ?/sec 1.20 120.0±120.00ns ? ?/sec"
            } else {
                "common 1.25 100.0±100.00ns ? ?/sec 1.00 80.0±80.00ns ? ?/sec"
            };
            let actual = body.lines().find(|l| l.starts_with("common ")).unwrap();
            assert_eq!(actual.split_whitespace().collect::<Vec<_>>().join(" "), row);
            for job in jobs.iter().filter(|j| target(j).unwrap() == *name) {
                let info = result(job).info;
                assert!(body.contains(&info.cpu_details));
                assert!(body.contains(&info.uname));
                assert!(body.contains(&info.bench_command));
            }
        }
        server.abort();
    }
}

async fn criterion_artifact(target: &std::path::Path, label: &str, id: &str, ns: f64) {
    let artifact = target.join("criterion").join(id).join(label);
    tokio::fs::create_dir_all(&artifact).await.unwrap();
    let data = record(label, id, ns);
    for (file, key) in [
        ("benchmark.json", "criterion_benchmark_v1"),
        ("estimates.json", "criterion_estimates_v1"),
    ] {
        tokio::fs::write(artifact.join(file), serde_json::to_vec(&data[key]).unwrap())
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn native_criterion_artifacts_export_through_the_runner_protocol() {
    let dir = tempfile::tempdir().unwrap();
    criterion_artifact(&dir.path().join("target"), "base", "common", 100.0).await;
    let data = record("base", "common", 100.0);
    let env = ["CARGO_TARGET_DIR=target".into()];
    let run = crate::runner::criterion::Run::new("base", "", dir.path(), &env);
    let harness = crate::runner::criterion::Criterion { bench_args: vec![] };
    // This directory has Criterion artifacts but no Cargo manifest to discover.
    let plan = crate::runner::execution::partition(&harness, run, Shard::default(), "target")
        .await
        .unwrap();
    assert!(plan.coverage.is_none());
    let run = plan.plan;
    let export = run.export(true).await.unwrap();
    assert_eq!(export.name, "base");
    assert_eq!(export.benchmarks.len(), 1);
    assert_eq!(
        export.benchmarks["common"]["criterion_estimates_v1"],
        data["criterion_estimates_v1"]
    );
    assert!(run.export(false).await.unwrap().benchmarks.is_empty());
    let absent = crate::runner::criterion::Run::new("changed", "", dir.path(), &env);
    assert!(absent.export(true).await.unwrap().benchmarks.is_empty());

    tokio::fs::write(
        dir.path()
            .join("target/criterion/common/base/estimates.json"),
        b"invalid JSON",
    )
    .await
    .unwrap();
    assert!(run.export(true).await.is_err());
    assert!(absent.export(true).await.is_err());
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn single_worker_empty_sides_preserve_the_available_comparison() {
    use crate::runner::{
        criterion::{Criterion, Run},
        execution::partition,
    };
    for (has_base, has_changed) in [(false, true), (true, false), (false, false), (true, true)] {
        let dir = tempfile::tempdir().unwrap();
        let env = ["CARGO_TARGET_DIR=artifacts".into()];
        let mut exports = Vec::new();
        for (label, present, ns) in [("base", has_base, 100.0), ("changed", has_changed, 80.0)] {
            let checkout = dir.path().join(label);
            tokio::fs::create_dir_all(&checkout).await.unwrap();
            let target = checkout.join("artifacts");
            if present {
                criterion_artifact(&target, label, "selected-case", ns).await;
            } else if label == "base" {
                // A successful zero-match run may leave an empty Criterion directory.
                tokio::fs::create_dir_all(target.join("criterion"))
                    .await
                    .unwrap();
            }
            // No Cargo manifest: exporting results must not invoke case discovery.
            let plan = partition(
                &Criterion { bench_args: vec![] },
                Run::new(label, "selected-case", &checkout, &env),
                Shard::default(),
                "target",
            )
            .await
            .unwrap();
            assert!(plan.coverage.is_none());
            let export = plan.plan.export(true).await.unwrap();
            assert_eq!(export.benchmarks.is_empty(), !present);
            exports.push(export);
        }
        let pool = db::connect("sqlite::memory:").await.unwrap();
        let mut jobs = request(&pool, 303, 1, &["target"]).await;
        let submission = ShardResult {
            base: Some(exports.remove(0)),
            changed: exports.remove(0),
            info: result(&jobs[0]).info,
        };
        assert!(store_result(&pool, &jobs[0], &submission).await.unwrap());
        let stored: String =
            sqlx::query_scalar("SELECT result_json FROM shard_results WHERE job_id = ?")
                .bind(jobs[0].id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let results = BTreeMap::from([(jobs[0].id, serde_json::from_str(&stored).unwrap())]);
        jobs[0].status = "completed".into();
        let body = final_body(&jobs, &results, &BTreeMap::new(), None)
            .await
            .unwrap()
            .inline_body();
        assert!(body.contains("Benchmark completed"), "{body}");
        assert!(!body.contains("Baseline build unavailable"));
        if has_base || has_changed {
            assert_eq!(
                body.lines()
                    .filter(|l| l.starts_with("selected-case "))
                    .count(),
                1
            );
            let columns: Vec<_> = body
                .lines()
                .find(|l| l.starts_with("group "))
                .unwrap()
                .split_whitespace()
                .collect();
            assert_eq!(columns.contains(&"base"), has_base);
            assert_eq!(columns.contains(&"changed"), has_changed);
        } else {
            assert!(body.contains("No matching cases; no measurements run."));
        }
    }
}

#[tokio::test]
#[ignore = "requires critcmp 0.1.8 on PATH"]
async fn real_critcmp_handles_partial_failure_branch_only_and_comment_overflow() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let mut jobs = request(&pool, 6, 4, &["target"]).await;
    let mut results = BTreeMap::new();
    for job in &mut jobs {
        job.status = "completed".into();
        let mut data = result(job);
        data.base = None;
        for i in 0..2000 {
            let id = format!("long-benchmark-name/{i:04}");
            if job.shard().unwrap().owns("target", &id) {
                data.changed
                    .benchmarks
                    .insert(id.clone(), record("changed", &id, 80.0));
            }
        }
        results.insert(job.id, data);
    }
    jobs[0].status = "failed".into();
    jobs[0].error_message = Some("K8s Job not found".into());
    let report = final_body(&jobs, &results, &BTreeMap::new(), None)
        .await
        .unwrap();
    let body = report.inline_body();
    assert!(body.contains("partial results"));
    assert!(body.contains("K8s Job not found"));
    assert!(body.contains("changed-only"));
    let failed_case = results[&jobs[0].id]
        .changed
        .benchmarks
        .keys()
        .next()
        .unwrap();
    assert!(body
        .lines()
        .any(|line| line.starts_with(&format!("{failed_case} "))));
    assert!(body.chars().count() > criterion_report::MAX_GITHUB_COMMENT_CHARS);
    assert!(!body.contains("lines omitted"));
    let (gh, _, gists, server) = github_with_gists().await;
    let (body, url) = report
        .github_body(&gh, "partial-key", 100, None)
        .await
        .unwrap();
    assert!(body.contains("failed or incomplete — partial results"));
    assert!(body.contains("K8s Job not found"));
    assert!(body.contains("changed-only"));
    assert!(body.contains(url.as_deref().unwrap()));
    assert!(!body.contains("long-benchmark-name"));
    assert!(body.chars().count() + 100 < criterion_report::MAX_GITHUB_COMMENT_CHARS);
    let (base, changed) = merge(&jobs.iter().collect::<Vec<_>>(), &results).unwrap();
    assert_eq!(
        gists.lock().await.gists[0]["files"]["comparison.txt"]["content"],
        compare(&base, &changed).await.unwrap()
    );
    server.abort();
}
