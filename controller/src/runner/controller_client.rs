//! HTTP client the runner uses to post PR comments via the controller's
//! `POST /jobs/{id}/comment` endpoint. The runner has no GitHub credentials;
//! the controller authenticates the caller with a per-job random token
//! injected into the pod at creation time and posts on its behalf.

use std::time::Duration;

use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use reqwest::Client;
use serde_json::json;

#[derive(Clone)]
pub struct ControllerClient {
    client: Client,
    base_url: String,
    job_id: String,
    token: String,
}

impl ControllerClient {
    pub fn new(base_url: String, job_id: String, token: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            job_id,
            token,
        }
    }

    /// Replace this job's section of the trigger comment. The controller
    /// merges sibling-job sections and PATCHes the comment.
    #[tracing::instrument(skip(self, body), fields(job_id = %self.job_id))]
    pub async fn post_section(&self, body: &str) -> Result<()> {
        let url = format!("{}/jobs/{}/comment", self.base_url, self.job_id);
        let payload = json!({ "body": body });

        // Retry everything: the controller is an internal service and a brief
        // outage (e.g. during a redeploy or Autopilot preemption) shouldn't
        // fail the whole benchmark run.
        (|| {
            let url = url.clone();
            let payload = payload.clone();
            async move {
                let resp = self
                    .client
                    .post(&url)
                    .bearer_auth(&self.token)
                    .json(&payload)
                    .send()
                    .await
                    .context("send request")?;
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("controller comment endpoint returned {status}: {body}");
            }
        })
        .retry(
            ExponentialBuilder::default()
                .with_max_times(8)
                .with_max_delay(Duration::from_secs(15)),
        )
        .sleep(tokio::time::sleep)
        .await
    }
}
