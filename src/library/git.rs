//! Git operations for the configuration library.

use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;

use super::types::LibraryStatus;

/// Clone a git repository if it doesn't exist.
pub async fn clone_if_needed(path: &Path, remote: &str) -> Result<bool> {
    if path.exists() && path.join(".git").exists() {
        tracing::debug!(path = %path.display(), "Library repo already exists");
        return Ok(false);
    }

    tracing::info!(remote = %remote, path = %path.display(), "Cloning library repository");

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let output = Command::new("git")
        .args(["clone", remote, &path.to_string_lossy()])
        .output()
        .await
        .context("Failed to execute git clone")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone failed: {}", stderr);
    }

    Ok(true)
}

/// Ensure the repository has the expected remote configured.
///
/// Precondition: `path` is either a git repository or does not exist.
/// Postcondition: if a git repository exists at `path`, its `origin` remote URL equals `remote`.
pub async fn ensure_remote(path: &Path, remote: &str) -> Result<()> {
    if !path.exists() || !path.join(".git").exists() {
        return Ok(());
    }

    let current = get_remote(path).await.ok();
    if current.as_deref() == Some(remote) {
        return Ok(());
    }

    let output = Command::new("git")
        .current_dir(path)
        .args(["remote", "set-url", "origin", remote])
        .output()
        .await
        .context("Failed to execute git remote set-url")?;

    if output.status.success() {
        return Ok(());
    }

    let output = Command::new("git")
        .current_dir(path)
        .args(["remote", "add", "origin", remote])
        .output()
        .await
        .context("Failed to execute git remote add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git remote add failed: {}", stderr);
    }

    Ok(())
}

/// Get the current git status of a repository.
pub async fn status(path: &Path) -> Result<LibraryStatus> {
    // Get current branch
    let branch = get_branch(path).await?;

    // Get remote URL
    let remote = get_remote(path).await.ok();

    // Check if clean
    let (clean, modified_files) = get_status(path).await?;

    // Get ahead/behind counts
    let (ahead, behind) = get_ahead_behind(path).await.unwrap_or((0, 0));

    Ok(LibraryStatus {
        path: path.to_string_lossy().to_string(),
        remote,
        branch,
        clean,
        ahead,
        behind,
        modified_files,
    })
}

/// Pull latest changes from remote.
pub async fn pull(path: &Path) -> Result<()> {
    tracing::info!(path = %path.display(), "Pulling library changes");

    let output = Command::new("git")
        .current_dir(path)
        .args(["pull", "--ff-only"])
        .output()
        .await
        .context("Failed to execute git pull")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git pull failed: {}", stderr);
    }

    Ok(())
}

/// Commit all changes with a message.
pub async fn commit(path: &Path, message: &str) -> Result<()> {
    tracing::info!(path = %path.display(), message = %message, "Committing library changes");

    // Stage all changes
    let output = Command::new("git")
        .current_dir(path)
        .args(["add", "-A"])
        .output()
        .await
        .context("Failed to execute git add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git add failed: {}", stderr);
    }

    // Commit
    let output = Command::new("git")
        .current_dir(path)
        .args(["commit", "-m", message])
        .output()
        .await
        .context("Failed to execute git commit")?;

    // Exit code 1 means nothing to commit, which is fine
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git commit failed: {}", stderr);
    }

    Ok(())
}

/// Push changes to remote.
pub async fn push(path: &Path) -> Result<()> {
    tracing::info!(path = %path.display(), "Pushing library changes");

    let output = Command::new("git")
        .current_dir(path)
        .args(["push"])
        .output()
        .await
        .context("Failed to execute git push")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git push failed: {}", stderr);
    }

    Ok(())
}

// Helper functions

async fn get_branch(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .context("Failed to get current branch")?;

    if !output.status.success() {
        anyhow::bail!("Failed to get branch name");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn get_remote(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["remote", "get-url", "origin"])
        .output()
        .await
        .context("Failed to get remote URL")?;

    if !output.status.success() {
        anyhow::bail!("No remote origin configured");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn get_status(path: &Path) -> Result<(bool, Vec<String>)> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["status", "--porcelain"])
        .output()
        .await
        .context("Failed to get git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok((lines.is_empty(), lines))
}

async fn get_ahead_behind(path: &Path) -> Result<(u32, u32)> {
    // First, fetch to update remote tracking branches
    let _ = Command::new("git")
        .current_dir(path)
        .args(["fetch", "--quiet"])
        .output()
        .await;

    // Get ahead/behind counts
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-list", "--left-right", "--count", "@{u}...HEAD"])
        .output()
        .await
        .context("Failed to get ahead/behind count")?;

    if !output.status.success() {
        // No upstream configured
        return Ok((0, 0));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();

    if parts.len() == 2 {
        let behind = parts[0].parse().unwrap_or(0);
        let ahead = parts[1].parse().unwrap_or(0);
        Ok((ahead, behind))
    } else {
        Ok((0, 0))
    }
}
