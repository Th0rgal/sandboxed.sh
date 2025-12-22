//! Terminal/shell command execution tool.
//!
//! ## Workspace-First Design
//! 
//! Commands run in the workspace by default:
//! - `run_command("ls")` → lists workspace contents
//! - `run_command("cat output/report.md")` → reads workspace file

use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{resolve_path_simple as resolve_path, Tool};

/// Sanitize command output to be safe for LLM consumption.
/// Removes binary garbage while preserving valid text.
fn sanitize_output(bytes: &[u8]) -> String {
    // Check if output appears to be mostly binary
    let non_printable_count = bytes.iter()
        .filter(|&&b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
        .count();
    
    // If more than 10% is non-printable (excluding newlines/tabs), it's likely binary
    if bytes.len() > 100 && non_printable_count > bytes.len() / 10 {
        return format!(
            "[Binary output detected - {} bytes, {}% non-printable. \
            Use appropriate tools to process binary data.]",
            bytes.len(),
            non_printable_count * 100 / bytes.len()
        );
    }
    
    // Convert to string, replacing invalid UTF-8
    let text = String::from_utf8_lossy(bytes);
    
    // Remove null bytes and other problematic control characters
    // Keep: newlines, tabs, carriage returns
    text.chars()
        .filter(|&c| c == '\n' || c == '\r' || c == '\t' || (c >= ' ' && c != '\u{FFFD}'))
        .collect()
}

/// Dangerous command patterns that should be blocked.
/// These patterns cause infinite loops or could damage the system.
const DANGEROUS_PATTERNS: &[(&str, &str)] = &[
    ("find /", "Use 'find /root/work/' or a specific directory path"),
    ("find / ", "Use 'find /root/work/' or a specific directory path"),
    ("grep -r /", "Use 'grep -r /root/' or a specific directory path"),
    ("grep -rn /", "Use 'grep -rn /root/' or a specific directory path"),
    ("grep -R /", "Use 'grep -R /root/' or a specific directory path"),
    ("ls -laR /", "Use a specific directory path instead of root"),
    ("du -sh /", "Use a specific directory path instead of root"),
    ("du -a /", "Use a specific directory path instead of root"),
    ("rm -rf /", "This would destroy the entire system"),
    ("rm -rf /*", "This would destroy the entire system"),
    ("> /dev/", "Writing to device files is blocked"),
    ("dd if=/dev/", "Direct disk operations are blocked"),
];

/// Validate a command against dangerous patterns.
/// Returns Ok(()) if safe, Err with suggestion if blocked.
fn validate_command(cmd: &str) -> Result<(), String> {
    let cmd_trimmed = cmd.trim();
    
    for (pattern, suggestion) in DANGEROUS_PATTERNS {
        // Check if command starts with the dangerous pattern
        if cmd_trimmed.starts_with(pattern) {
            return Err(format!(
                "Blocked dangerous command pattern '{}'. {}",
                pattern, suggestion
            ));
        }
        // Also check for the pattern after common prefixes (sudo, time, etc.)
        let prefixes = ["sudo ", "time ", "nice ", "nohup "];
        for prefix in prefixes {
            if cmd_trimmed.starts_with(prefix) {
                let after_prefix = &cmd_trimmed[prefix.len()..];
                if after_prefix.starts_with(pattern) {
                    return Err(format!(
                        "Blocked dangerous command pattern '{}'. {}",
                        pattern, suggestion
                    ));
                }
            }
        }
    }
    
    Ok(())
}

/// Run a shell command.
pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Runs in workspace by default. Use for tests, builds, package installs, etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute. Relative paths in commands resolve from workspace."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional: working directory. Defaults to workspace. Use relative paths (e.g., 'subdir/') or absolute for system access."
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
        
        // Validate command against dangerous patterns
        if let Err(msg) = validate_command(command) {
            tracing::warn!("Blocked dangerous command: {}", command);
            return Err(anyhow::anyhow!("{}", msg));
        }
        
        let cwd = args["cwd"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(60);

        tracing::info!("Executing command in {:?}: {}", cwd, command);

        // Determine shell based on OS - use absolute paths to ensure shell is found
        let (shell, shell_arg) = if cfg!(target_os = "windows") {
            ("cmd", "/C")
        } else {
            // Use absolute path to shell to avoid PATH issues
            ("/bin/sh", "-c")
        };

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new(shell)
                .arg(shell_arg)
                .arg(command)
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                tracing::error!("Command execution failed: {}", e);
                return Err(anyhow::anyhow!("Failed to execute command: {}", e));
            }
            Err(_) => {
                tracing::error!("Command timed out after {} seconds", timeout_secs);
                return Err(anyhow::anyhow!("Command timed out after {} seconds", timeout_secs));
            }
        };

        let stdout = sanitize_output(&output.stdout);
        let stderr = sanitize_output(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        
        tracing::debug!("Command completed: exit={}, stdout_len={}, stderr_len={}", 
            exit_code, stdout.len(), stderr.len());

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

