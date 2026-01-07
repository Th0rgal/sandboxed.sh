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
/// Postcondition: if a git repository exists at `path`, its `origin` remote URL equals `remote`
/// and the repository is tracking content from that remote.
pub async fn ensure_remote(path: &Path, remote: &str) -> Result<()> {
    if !path.exists() || !path.join(".git").exists() {
        return Ok(());
    }

    let current = get_remote(path).await.ok();
    if current.as_deref() == Some(remote) {
        return Ok(());
    }

    tracing::info!(
        old_remote = ?current,
        new_remote = %remote,
        "Switching library remote"
    );

    // Update the remote URL
    let output = Command::new("git")
        .current_dir(path)
        .args(["remote", "set-url", "origin", remote])
        .output()
        .await
        .context("Failed to execute git remote set-url")?;

    if !output.status.success() {
        // Try adding remote if it doesn't exist
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
    }

    // Fetch from the new remote
    tracing::info!("Fetching from new remote");
    let output = Command::new("git")
        .current_dir(path)
        .args(["fetch", "origin"])
        .output()
        .await
        .context("Failed to execute git fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git fetch failed: {}", stderr);
    }

    // Try to find the default branch (main or master)
    let default_branch = detect_default_branch(path).await?;

    // Reset to the new remote's default branch
    tracing::info!(branch = %default_branch, "Resetting to remote's default branch");
    let output = Command::new("git")
        .current_dir(path)
        .args(["checkout", "-B", &default_branch, &format!("origin/{}", default_branch)])
        .output()
        .await
        .context("Failed to execute git checkout")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git checkout failed: {}", stderr);
    }

    Ok(())
}

/// Detect the default branch of the remote (main or master).
async fn detect_default_branch(path: &Path) -> Result<String> {
    // Try 'main' first
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--verify", "origin/main"])
        .output()
        .await?;

    if output.status.success() {
        return Ok("main".to_string());
    }

    // Fall back to 'master'
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--verify", "origin/master"])
        .output()
        .await?;

    if output.status.success() {
        return Ok("master".to_string());
    }

    // Default to 'main' if neither exists (new repo)
    Ok("main".to_string())
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

/// Clone a git repository to a path.
pub async fn clone(path: &Path, remote: &str) -> Result<()> {
    tracing::info!(remote = %remote, path = %path.display(), "Cloning repository");

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", remote, &path.to_string_lossy()])
        .output()
        .await
        .context("Failed to execute git clone")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone failed: {}", stderr);
    }

    Ok(())
}

/// Clone a specific path from a git repository using sparse checkout.
pub async fn sparse_clone(path: &Path, remote: &str, subpath: &str) -> Result<()> {
    tracing::info!(
        remote = %remote,
        path = %path.display(),
        subpath = %subpath,
        "Sparse cloning repository"
    );

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Initialize empty repo
    tokio::fs::create_dir_all(path).await?;

    let output = Command::new("git")
        .current_dir(path)
        .args(["init"])
        .output()
        .await
        .context("Failed to init git repo")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git init failed: {}", stderr);
    }

    // Add remote
    let output = Command::new("git")
        .current_dir(path)
        .args(["remote", "add", "origin", remote])
        .output()
        .await
        .context("Failed to add remote")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git remote add failed: {}", stderr);
    }

    // Enable sparse checkout
    let output = Command::new("git")
        .current_dir(path)
        .args(["config", "core.sparseCheckout", "true"])
        .output()
        .await
        .context("Failed to enable sparse checkout")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git config failed: {}", stderr);
    }

    // Write sparse-checkout file
    let sparse_checkout_path = path.join(".git/info/sparse-checkout");
    if let Some(parent) = sparse_checkout_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&sparse_checkout_path, format!("{}\n", subpath)).await?;

    // Fetch and checkout
    let output = Command::new("git")
        .current_dir(path)
        .args(["fetch", "--depth", "1", "origin"])
        .output()
        .await
        .context("Failed to fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git fetch failed: {}", stderr);
    }

    // Try to checkout the default branch
    let default_branch = detect_default_branch(path).await.unwrap_or_else(|_| "main".to_string());

    let output = Command::new("git")
        .current_dir(path)
        .args(["checkout", &format!("origin/{}", default_branch)])
        .output()
        .await
        .context("Failed to checkout")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git checkout failed: {}", stderr);
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
