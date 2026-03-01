//! Git and gh CLI operations for the benchmark runner.

use std::path::Path;

use anyhow::{Context, Result};

use super::shell::run_command;

/// Shallow-clone a repository.
pub async fn clone_shallow(repo_url: &str, dest: &Path, depth: u32) -> Result<()> {
    let depth_str = depth.to_string();
    run_command(
        "git",
        &[
            "clone",
            &format!("--depth={depth_str}"),
            repo_url,
            &dest.to_string_lossy(),
        ],
        Path::new("/"),
    )
    .await
    .context("git clone")?;
    Ok(())
}

/// Fetch the PR ref and main branch, then checkout the PR branch.
/// Returns the branch name.
pub async fn checkout_pr(pr_url: &str, dir: &Path) -> Result<String> {
    let pr_number = pr_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .context("failed to extract PR number from URL")?;

    let branch_name = run_command(
        "gh",
        &[
            "pr",
            "view",
            pr_url,
            "--json",
            "headRefName",
            "--jq",
            ".headRefName",
        ],
        dir,
    )
    .await
    .context("gh pr view")?
    .trim()
    .to_string();

    run_command(
        "git",
        &[
            "fetch",
            "origin",
            &format!("refs/pull/{pr_number}/head:{branch_name}"),
            "main",
        ],
        dir,
    )
    .await
    .context("git fetch PR ref")?;

    run_command("git", &["checkout", &branch_name], dir)
        .await
        .context("git checkout branch")?;

    Ok(branch_name)
}

/// Find the merge-base between HEAD and origin/main.
pub async fn merge_base(dir: &Path) -> Result<String> {
    let output = run_command("git", &["merge-base", "HEAD", "origin/main"], dir)
        .await
        .context("git merge-base")?;
    Ok(output.trim().to_string())
}

/// Get the current HEAD commit SHA.
pub async fn rev_parse_head(dir: &Path) -> Result<String> {
    let output = run_command("git", &["rev-parse", "HEAD"], dir)
        .await
        .context("git rev-parse HEAD")?;
    Ok(output.trim().to_string())
}

/// Checkout a specific ref with detached HEAD advice suppressed.
pub async fn checkout(dir: &Path, ref_: &str) -> Result<()> {
    run_command(
        "git",
        &["-c", "advice.detachedHead=false", "checkout", ref_],
        dir,
    )
    .await
    .context("git checkout")?;
    Ok(())
}

/// Fetch from origin.
pub async fn fetch_origin(dir: &Path) -> Result<()> {
    run_command("git", &["fetch", "origin"], dir)
        .await
        .context("git fetch origin")?;
    Ok(())
}

/// Initialize and update git submodules.
pub async fn submodule_update(dir: &Path) -> Result<()> {
    run_command("git", &["submodule", "update", "--init"], dir)
        .await
        .context("git submodule update")?;
    Ok(())
}

/// Run `cargo update` in a directory.
pub async fn cargo_update(dir: &Path) -> Result<()> {
    run_command("cargo", &["update"], dir)
        .await
        .context("cargo update")?;
    Ok(())
}

/// Pre-install the stable Rust toolchain to avoid race conditions in parallel builds.
pub async fn rustup_stable() -> Result<()> {
    run_command(
        "rustup",
        &["toolchain", "install", "stable", "--no-self-update"],
        Path::new("/"),
    )
    .await
    .context("rustup toolchain install")?;
    Ok(())
}

/// Sanitize a branch name for use as a criterion baseline name (replace `/` with `_`).
pub fn sanitize_branch_name(name: &str) -> String {
    name.replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_branch_name("my-branch"), "my-branch");
    }

    #[test]
    fn sanitize_with_slashes() {
        assert_eq!(
            sanitize_branch_name("feature/my-branch"),
            "feature_my-branch"
        );
    }

    #[test]
    fn sanitize_multiple_slashes() {
        assert_eq!(
            sanitize_branch_name("user/feature/sub"),
            "user_feature_sub"
        );
    }
}
