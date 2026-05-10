//! `CommentPoster` chooses between posting PR comments directly to GitHub
//! (with a `GITHUB_TOKEN` — used by the scheduled main-tracking workflow)
//! and proxying through the controller (used by PR-triggered runs, which
//! have no GitHub creds in the pod).

use anyhow::Result;

use crate::github::GitHubClient;
use crate::runner::controller_client::ControllerClient;

#[derive(Clone)]
pub enum CommentPoster {
    /// Post directly to GitHub using a `GITHUB_TOKEN`.
    Direct(GitHubClient),
    /// Proxy through the controller's `POST /jobs/{id}/comment` endpoint.
    Proxy(ControllerClient),
}

impl CommentPoster {
    pub async fn post_comment(&self, repo: &str, pr_number: i64, body: &str) -> Result<()> {
        match self {
            Self::Direct(c) => c.post_comment(repo, pr_number, body).await,
            Self::Proxy(c) => c.post_comment(repo, pr_number, body).await,
        }
    }
}
