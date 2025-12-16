//! Code search tools: grep/regex search.
//!
//! These tools have full system access - they can search any directory on the machine.
//! Paths can be absolute (e.g., `/var/log`) or relative to the working directory.

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

/// Search file contents with regex/grep.
pub struct GrepSearch;

#[async_trait]
impl Tool for GrepSearch {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Search for a pattern in file contents anywhere on the system using regex. Returns matching lines with file paths and line numbers. Great for finding function definitions, usages, config values, or specific patterns."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Can be absolute (e.g., /var/log, /etc) or relative to working directory. Defaults to working directory."
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Optional: only search files matching this glob (e.g., '*.rs', '*.py', '*.log')"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether search is case-sensitive (default: false)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;
        let path = args["path"].as_str().unwrap_or(".");
        let file_pattern = args["file_pattern"].as_str();
        let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);

        let search_path = resolve_path(path, working_dir);

        // Try to use ripgrep (rg) if available, fall back to grep
        let mut cmd = if which_exists("rg") {
            let mut c = Command::new("rg");
            c.arg("--line-number");
            c.arg("--no-heading");
            c.arg("--color=never");

            if !case_sensitive {
                c.arg("-i");
            }

            if let Some(fp) = file_pattern {
                c.arg("-g").arg(fp);
            }

            c.arg("--").arg(pattern).arg(&search_path);
            c
        } else {
            let mut c = Command::new("grep");
            c.arg("-rn");

            if !case_sensitive {
                c.arg("-i");
            }

            if let Some(fp) = file_pattern {
                c.arg("--include").arg(fp);
            }

            c.arg(pattern).arg(&search_path);
            c
        };

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to execute search: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // grep returns exit code 1 when no matches found
        if !output.status.success() && output.status.code() != Some(1) {
            if !stderr.is_empty() {
                return Err(anyhow::anyhow!("Search error: {}", stderr));
            }
        }

        if stdout.is_empty() {
            return Ok(format!("No matches found for pattern: {}", pattern));
        }

        // Show results with full paths for system-wide clarity
        let result: String = stdout
            .lines()
            .take(100) // Limit results
            .collect::<Vec<_>>()
            .join("\n");

        let line_count = result.lines().count();
        if line_count >= 100 {
            Ok(format!(
                "{}\n\n... (showing first 100 matches)",
                result
            ))
        } else {
            Ok(result)
        }
    }
}

/// Check if a command exists in PATH.
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

