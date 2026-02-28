use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::Client;

use crate::models::GitHubComment;

const API_BASE: &str = "https://api.github.com";

#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    token: String,
}

impl GitHubClient {
    pub fn new(token: &str) -> Self {
        Self {
            client: Client::new(),
            token: token.to_string(),
        }
    }

    pub async fn fetch_recent_comments(
        &self,
        repo: &str,
        since: &str,
    ) -> Result<Vec<GitHubComment>> {
        let url = format!("{API_BASE}/repos/{repo}/issues/comments");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "datafusion-benchmark-controller")
            .query(&[
                ("per_page", "100"),
                ("sort", "updated"),
                ("direction", "desc"),
                ("since", since),
            ])
            .send()
            .await
            .context("fetch comments")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {status}: {body}");
        }

        let comments: Vec<GitHubComment> = resp.json().await.context("parse comments")?;
        Ok(comments)
    }

    pub async fn post_comment(&self, repo: &str, pr_number: i64, body: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/{pr_number}/comments");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "datafusion-benchmark-controller")
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .context("post comment")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API post_comment error {status}: {body}");
        }
        Ok(())
    }

    pub async fn post_reaction(&self, repo: &str, comment_id: i64, content: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/comments/{comment_id}/reactions");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "datafusion-benchmark-controller")
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
