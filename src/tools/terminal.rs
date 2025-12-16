//! Terminal/shell command execution tool.
//!
//! This tool has full system access - it can execute any command on the machine.
//! The working directory can be specified explicitly or defaults to the agent's working directory.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::Tool;

/// Resolve a path - if absolute, use as-is; if relative, join with working_dir.
fn resolve_path(path_str: &str, working_dir: &Path) -> PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

/// Run a shell command.
pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Execute any shell command on the system. Returns stdout and stderr. Use for running tests, installing packages, compiling code, system administration, etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional: working directory for the command. Can be absolute (e.g., /var/log) or relative. Defaults to agent's working directory."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 60)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;
        let cwd = args["cwd"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(60);

        tracing::info!("Executing command in {:?}: {}", cwd, command);
        tracing::debug!("CWD exists: {}, is_dir: {}", cwd.exists(), cwd.is_dir());

        // Determine shell based on OS
        let (shell, shell_arg) = if cfg!(target_os = "windows") {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new(shell)
                .arg(shell_arg)
                .arg(command)
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {} seconds", timeout_secs))?
        .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        
        tracing::debug!("Command exit code: {}, stdout len: {}, stderr len: {}", 
            exit_code, stdout.len(), stderr.len());
        if !stdout.is_empty() {
            tracing::debug!("Command stdout (first 500 chars): {}", 
                &stdout[..stdout.len().min(500)]);
        }
        if !stderr.is_empty() {
            tracing::warn!("Command stderr: {}", &stderr[..stderr.len().min(500)]);
        }

        let mut result = String::new();

        result.push_str(&format!("Exit code: {}\n", exit_code));

        if !stdout.is_empty() {
            result.push_str("\n--- stdout ---\n");
            result.push_str(&stdout);
        }

        if !stderr.is_empty() {
            result.push_str("\n--- stderr ---\n");
            result.push_str(&stderr);
        }

        // Truncate if too long
        if result.len() > 10000 {
            result.truncate(10000);
            result.push_str("\n... [output truncated]");
        }

        Ok(result)
    }
}

