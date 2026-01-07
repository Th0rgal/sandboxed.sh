//! Git operation tools.
//!
//! ## Workspace-First Design
//!
//! Git tools operate on the workspace by default:
//! - `git_status()` → status of workspace repo
//! - `git_status("subproject/")` → status of nested repo

use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{resolve_path_simple as resolve_path, Tool};

/// Get git status.
pub struct GitStatus;

#[async_trait]
impl Tool for GitStatus {
    fn name(&self) -> &str {
        "git_status"
    }

    fn description(&self) -> &str {
        "Get the current git status, showing modified, staged, and untracked files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": {
                    "type": "string",
                    "description": "Optional: path to the git repository. Can be absolute or relative. Defaults to working directory."
                }
            }
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let repo_path = args["repo_path"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        run_git_command(&["status", "--porcelain=v2", "--branch"], &repo_path).await
    }
}

/// Get git diff.
pub struct GitDiff;

#[async_trait]
impl Tool for GitDiff {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "Show git diff of changes. Can diff staged changes, specific files, or commits."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": {
                    "type": "string",
                    "description": "Optional: path to the git repository. Can be absolute or relative. Defaults to working directory."
                },
                "staged": {
                    "type": "boolean",
                    "description": "Show staged changes instead of unstaged (default: false)"
                },
                "file": {
                    "type": "string",
                    "description": "Optional: show diff for specific file only"
                }
            }
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let repo_path = args["repo_path"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let staged = args["staged"].as_bool().unwrap_or(false);
        let file = args["file"].as_str();

        let mut git_args = vec!["diff"];

        if staged {
            git_args.push("--staged");
        }

        if let Some(f) = file {
            git_args.push("--");
            git_args.push(f);
        }

        let result = run_git_command(&git_args, &repo_path).await?;

        if result.is_empty() {
            Ok("No changes".to_string())
        } else if result.len() > 10000 {
            let safe_end = super::safe_truncate_index(&result, 10000);
            Ok(format!(
                "{}... [diff truncated, showing first {} chars]",
                &result[..safe_end],
                safe_end
            ))
        } else {
            Ok(result)
        }
    }
}

/// Create a git commit.
pub struct GitCommit;

#[async_trait]
impl Tool for GitCommit {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Stage all changes and create a git commit with the given message."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": {
                    "type": "string",
                    "description": "Optional: path to the git repository. Can be absolute or relative. Defaults to working directory."
                },
                "message": {
                    "type": "string",
                    "description": "The commit message"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: specific files to stage. If not provided, stages all changes."
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let repo_path = args["repo_path"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let message = args["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' argument"))?;

        let files: Vec<&str> = args["files"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // Stage files
        if files.is_empty() {
            run_git_command(&["add", "-A"], &repo_path).await?;
        } else {
            let mut git_args = vec!["add", "--"];
            git_args.extend(files);
            run_git_command(&git_args, &repo_path).await?;
        }

        // Commit
        run_git_command(&["commit", "-m", message], &repo_path).await
    }
}

/// Get git log.
pub struct GitLog;

#[async_trait]
impl Tool for GitLog {
    fn name(&self) -> &str {
        "git_log"
    }

    fn description(&self) -> &str {
        "Show recent git commits."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": {
                    "type": "string",
                    "description": "Optional: path to the git repository. Can be absolute or relative. Defaults to working directory."
                },
                "num_commits": {
                    "type": "integer",
                    "description": "Number of commits to show (default: 10)"
                },
                "oneline": {
                    "type": "boolean",
                    "description": "Show condensed one-line format (default: true)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let repo_path = args["repo_path"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let num_commits = args["num_commits"].as_u64().unwrap_or(10);
        let oneline = args["oneline"].as_bool().unwrap_or(true);

        let mut git_args = vec!["log", "-n"];
        let num_str = num_commits.to_string();
        git_args.push(&num_str);

        if oneline {
            git_args.push("--oneline");
        }

        run_git_command(&git_args, &repo_path).await
    }
}

/// Run a git command and return its output.
async fn run_git_command(args: &[&str], repo_path: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run git: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        if stderr.is_empty() {
            return Err(anyhow::anyhow!("Git command failed: {}", stdout.trim()));
        }
        return Err(anyhow::anyhow!("Git error: {}", stderr.trim()));
    }

    Ok(stdout.to_string())
}
