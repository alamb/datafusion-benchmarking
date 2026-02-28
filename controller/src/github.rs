//! Thin HTTP client for the GitHub REST API.
//!
//! Wraps [`reqwest::Client`] with authentication, standard headers, and
//! response-status checking.

use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response};

use crate::models::GitHubComment;

const API_BASE: &str = "https://api.github.com";

/// Thin HTTP client for the GitHub REST API.
#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    token: String,
}

impl GitHubClient {
    /// Create a new client with the given personal access token.
    pub fn new(token: &str) -> Self {
        Self {
            client: Client::new(),
            token: token.to_string(),
        }
    }

    /// Attach standard GitHub API headers and bearer auth to a request.
    fn request_builder(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "datafusion-benchmark-controller")
    }

    /// Check that a response has a 2xx status, returning an error with the
    /// response body on failure.
    async fn check_response(resp: Response, context: &str) -> Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API {context} error {status}: {body}");
    }

    /// Fetch up to 100 issue/PR comments updated since `since` (ISO 8601).
    pub async fn fetch_recent_comments(
        &self,
        repo: &str,
        since: &str,
    ) -> Result<Vec<GitHubComment>> {
        let url = format!("{API_BASE}/repos/{repo}/issues/comments");
        let resp = self
            .request_builder(self.client.get(&url))
            .query(&[
                ("per_page", "100"),
                ("sort", "updated"),
                ("direction", "desc"),
                ("since", since),
            ])
            .send()
            .await
            .context("fetch comments")?;

        let resp = Self::check_response(resp, "fetch_comments").await?;
        let comments: Vec<GitHubComment> = resp.json().await.context("parse comments")?;
        Ok(comments)
    }

    /// Post a comment on a PR/issue.
    pub async fn post_comment(&self, repo: &str, pr_number: i64, body: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/{pr_number}/comments");
        let resp = self
            .request_builder(self.client.post(&url))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .context("post comment")?;

        Self::check_response(resp, "post_comment").await?;
        Ok(())
    }

    /// Add a reaction (e.g. "rocket") to a comment. Logs a warning on failure instead of erroring.
    pub async fn post_reaction(&self, repo: &str, comment_id: i64, content: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/comments/{comment_id}/reactions");
        let resp = self
            .request_builder(self.client.post(&url))
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await
            .context("post reaction")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!("Failed to post reaction: {status}: {body}");
        }
        Ok(())
    }
}
