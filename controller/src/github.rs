//! Thin HTTP client for the GitHub REST API.
//!
//! Wraps [`reqwest::Client`] with authentication, standard headers, and
//! response-status checking. Retries transient errors with exponential backoff.

use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

use crate::models::GitHubComment;

const API_BASE: &str = "https://api.github.com";

/// Append the benchmark runner issues link if configured.
pub fn issues_footer(runner_repo_url: Option<&str>) -> String {
    match runner_repo_url {
        Some(url) if !url.is_empty() => {
            format!("\n\n---\n[File an issue]({url}/issues) against this benchmark runner")
        }
        _ => String::new(),
    }
}

/// Maximum number of pages to fetch when paginating (10,000 comments at 100/page).
const MAX_PAGES: usize = 100;

/// Thin HTTP client for the GitHub REST API.
#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    token: String,
}

/// Determine whether an error (or status) is worth retrying.
fn is_retryable(err: &anyhow::Error) -> bool {
    // Check for reqwest errors (network / connection failures)
    if let Some(re) = err.downcast_ref::<reqwest::Error>() {
        if re.is_connect() || re.is_timeout() || re.is_request() {
            return true;
        }
        if let Some(status) = re.status() {
            return status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
        }
        return true; // unknown reqwest errors → retry
    }

    // Check for our own "GitHub API … error …" messages (from check_response)
    let msg = err.to_string();
    if msg.contains("error 5") || msg.contains("error 429") {
        return true;
    }
    false
}

/// Parse the `Retry-After` header (seconds) from a 429 response.
fn parse_retry_after(resp: &Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Parse the `next` URL from a `Link` header value.
fn parse_next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let part = part.trim();
        if part.ends_with("rel=\"next\"") {
            if let Some(url) = part.strip_suffix(">; rel=\"next\"") {
                if let Some(url) = url.strip_prefix('<') {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
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
    /// response body on failure. For 429 responses, sleeps for `Retry-After`
    /// before returning the error (so backon's retry fires after the wait).
    async fn check_response(resp: Response, context: &str) -> Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();

        // Respect Retry-After on 429
        if status == StatusCode::TOO_MANY_REQUESTS {
            if let Some(secs) = parse_retry_after(&resp) {
                tracing::warn!(retry_after = secs, "rate limited, sleeping");
                tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
            }
        }

        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API {context} error {status}: {body}");
    }

    /// Send a GET request with retry logic. Returns the successful response.
    async fn get_with_retry(&self, url: &str, query: &[(&str, &str)]) -> Result<Response> {
        let url = url.to_string();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        (|| {
            let url = url.clone();
            let query = query.clone();
            async move {
                let resp = self
                    .request_builder(self.client.get(&url))
                    .query(&query)
                    .send()
                    .await
                    .context("send request")?;
                Self::check_response(resp, "GET").await
            }
        })
        .retry(ExponentialBuilder::default().with_max_times(3))
        .sleep(tokio::time::sleep)
        .when(is_retryable)
        .await
    }

    /// Send a POST request with retry logic. Returns the successful response.
    async fn post_with_retry(&self, url: &str, body: serde_json::Value) -> Result<Response> {
        let url = url.to_string();

        (|| {
            let url = url.clone();
            let body = body.clone();
            async move {
                let resp = self
                    .request_builder(self.client.post(&url))
                    .json(&body)
                    .send()
                    .await
                    .context("send request")?;
                Self::check_response(resp, "POST").await
            }
        })
        .retry(ExponentialBuilder::default().with_max_times(3))
        .sleep(tokio::time::sleep)
        .when(is_retryable)
        .await
    }

    /// Fetch issue/PR comments updated since `since` (ISO 8601), paginating through all results.
    /// Caps at MAX_PAGES pages (10,000 comments).
    #[tracing::instrument(skip(self, since))]
    pub async fn fetch_recent_comments(
        &self,
        repo: &str,
        since: &str,
    ) -> Result<Vec<GitHubComment>> {
        let mut all_comments = Vec::new();
        let mut next_url: Option<String> = None;

        for page in 0..MAX_PAGES {
            let resp = if page == 0 {
                let url = format!("{API_BASE}/repos/{repo}/issues/comments");
                self.get_with_retry(
                    &url,
                    &[
                        ("per_page", "100"),
                        ("sort", "updated"),
                        ("direction", "desc"),
                        ("since", since),
                    ],
                )
                .await
                .context("fetch comments")?
            } else {
                let url = next_url.as_deref().unwrap();
                self.get_with_retry(url, &[])
                    .await
                    .context("fetch comments page")?
            };

            // Parse next link before consuming the body
            let link_header = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_next_link);

            let comments: Vec<GitHubComment> = resp.json().await.context("parse comments")?;
            let count = comments.len();
            all_comments.extend(comments);

            match link_header {
                Some(url) if count > 0 => next_url = Some(url),
                _ => break,
            }
        }

        Ok(all_comments)
    }

    /// Post a comment on a PR/issue.
    #[tracing::instrument(skip(self, body))]
    pub async fn post_comment(&self, repo: &str, pr_number: i64, body: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/{pr_number}/comments");
        self.post_with_retry(&url, serde_json::json!({ "body": body }))
            .await
            .context("post comment")?;
        Ok(())
    }

    /// Look up a PR and return its `head.ref` (the source branch name).
    /// Runner pods no longer have a `GITHUB_TOKEN`, so the controller
    /// resolves this once and passes it to the pod via `PR_HEAD_REF`.
    #[tracing::instrument(skip(self))]
    pub async fn get_pr_head_ref(&self, repo: &str, pr_number: i64) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct PullHead {
            #[serde(rename = "ref")]
            ref_: String,
        }
        #[derive(serde::Deserialize)]
        struct Pull {
            head: PullHead,
        }
        let url = format!("{API_BASE}/repos/{repo}/pulls/{pr_number}");
        let resp = self.get_with_retry(&url, &[]).await?;
        let pull: Pull = resp.json().await.context("parse pull json")?;
        Ok(pull.head.ref_)
    }

    /// Add a reaction (e.g. "rocket") to a comment. Logs a warning on failure instead of erroring.
    pub async fn post_reaction(&self, repo: &str, comment_id: i64, content: &str) -> Result<()> {
        let url = format!("{API_BASE}/repos/{repo}/issues/comments/{comment_id}/reactions");
        let body = serde_json::json!({ "content": content });

        match self.post_with_retry(&url, body).await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "failed to post reaction");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_next_link_standard() {
        let header = r#"<https://api.github.com/repos/foo/bar/issues/comments?page=2>; rel="next", <https://api.github.com/repos/foo/bar/issues/comments?page=5>; rel="last""#;
        assert_eq!(
            parse_next_link(header),
            Some("https://api.github.com/repos/foo/bar/issues/comments?page=2".to_string())
        );
    }

    #[test]
    fn parse_next_link_missing() {
        let header = r#"<https://api.github.com/repos/foo/bar/issues/comments?page=5>; rel="last""#;
        assert_eq!(parse_next_link(header), None);
    }

    #[test]
    fn parse_next_link_empty() {
        assert_eq!(parse_next_link(""), None);
    }

    #[test]
    fn retryable_on_server_error_message() {
        let err = anyhow::anyhow!("GitHub API GET error 500 Internal Server Error: oops");
        assert!(is_retryable(&err));
    }

    #[test]
    fn retryable_on_429_message() {
        let err = anyhow::anyhow!("GitHub API GET error 429 Too Many Requests: slow down");
        assert!(is_retryable(&err));
    }

    #[test]
    fn not_retryable_on_404() {
        let err = anyhow::anyhow!("GitHub API GET error 404 Not Found: nope");
        assert!(!is_retryable(&err));
    }

    #[test]
    fn not_retryable_on_401() {
        let err = anyhow::anyhow!("GitHub API GET error 401 Unauthorized: bad token");
        assert!(!is_retryable(&err));
    }
}
