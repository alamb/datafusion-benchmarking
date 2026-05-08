//! `CommentPoster` chooses between editing the trigger comment directly on
//! GitHub (with a `GITHUB_TOKEN` — used by the scheduled main-tracking
//! workflow) and proxying through the controller (used by PR-triggered runs,
//! which have no GitHub creds in the pod).
//!
//! Each runner job owns a single "section" of the trigger comment (the part
//! that changes from "running" to "completed"/"failed"). Updating a section
//! re-renders the trigger comment in place rather than posting a new comment.

use anyhow::{Context, Result};

use crate::comment_render;
use crate::github::{self, GitHubClient};
use crate::runner::controller_client::ControllerClient;

#[derive(Clone)]
pub enum CommentPoster {
    /// Edit comments directly on GitHub using a `GITHUB_TOKEN`. Used by the
    /// main-tracking workflow where one runner owns the entire trigger
    /// comment, so no inter-job coordination is required.
    Direct {
        client: GitHubClient,
        runner_repo_url: Option<String>,
    },
    /// Proxy through the controller's `POST /jobs/{id}/comment` endpoint —
    /// the controller serializes sibling-job updates per trigger comment.
    Proxy(ControllerClient),
}

impl CommentPoster {
    /// Set the section this runner contributes to the trigger comment,
    /// re-rendering the comment so the new section becomes visible.
    /// Replaces any prior section the same runner posted.
    pub async fn update_section(&self, repo: &str, comment_id: i64, section: &str) -> Result<()> {
        match self {
            Self::Direct {
                client,
                runner_repo_url,
            } => {
                let original = client
                    .get_comment_body(repo, comment_id)
                    .await
                    .context("fetch trigger comment")?;
                let footer = github::issues_footer(runner_repo_url.as_deref());
                let body = comment_render::render(&original, &[section.to_string()], &footer);
                client
                    .update_comment(repo, comment_id, &body)
                    .await
                    .context("update trigger comment")
            }
            Self::Proxy(c) => c.post_section(section).await,
        }
    }
}
