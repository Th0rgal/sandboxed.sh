//! Directory operation tools: list directory, search files by name.
//!
//! These tools have full system access - they can browse any directory on the machine.
//! Paths can be absolute (e.g., `/var/log`) or relative to the working directory.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};
use walkdir::WalkDir;

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

/// List contents of a directory.
pub struct ListDirectory;

#[async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List files and directories anywhere on the system. Returns a tree-like view of the directory structure."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory. Can be absolute (e.g., /var/log) or relative to working directory. Use '.' for working directory."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth to traverse (default: 3)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        let max_depth = args["max_depth"].as_u64().unwrap_or(3) as usize;

        let full_path = resolve_path(path, working_dir);

        if !full_path.exists() {
            return Err(anyhow::anyhow!("Directory not found: {}", path));
        }

        if !full_path.is_dir() {
            return Err(anyhow::anyhow!("Not a directory: {}", path));
        }

        let mut entries = Vec::new();
        let walker = WalkDir::new(&full_path)
            .max_depth(max_depth)
            .sort_by_file_name();

        for entry in walker.into_iter().filter_map(|e| e.ok()) {
            let depth = entry.depth();
            let path = entry.path();
            let relative = path.strip_prefix(&full_path).unwrap_or(path);

            if relative.as_os_str().is_empty() {
                continue;
            }

            let prefix = "  ".repeat(depth.saturating_sub(1));
            let name = relative
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let suffix = if path.is_dir() { "/" } else { "" };
            entries.push(format!("{}{}{}", prefix, name, suffix));
        }

        if entries.is_empty() {
            Ok("Directory is empty".to_string())
        } else {
            Ok(entries.join("\n"))
        }
    }
}

/// Search for files by name pattern.
pub struct SearchFiles;

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search for files by name pattern (glob-style) anywhere on the system. Returns matching file paths."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "File name pattern to search for (e.g., '*.rs', 'test_*.py', 'README*')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Can be absolute (e.g., /home) or relative to working directory. Defaults to working directory."
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

        let full_path = resolve_path(path, working_dir);

        if !full_path.exists() {
            return Err(anyhow::anyhow!("Directory not found: {}", path));
        }

        // Convert glob pattern to simple matching
        let pattern_lower = pattern.to_lowercase();
        let is_glob = pattern.contains('*');

        let mut matches = Vec::new();
        let walker = WalkDir::new(&full_path).into_iter().filter_map(|e| e.ok());

        for entry in walker {
            if !entry.file_type().is_file() {
                continue;
            }

            let file_name = entry
                .file_name()
                .to_string_lossy()
                .to_lowercase();

            let matched = if is_glob {
                // Simple glob matching
                glob_match(&pattern_lower, &file_name)
            } else {
                file_name.contains(&pattern_lower)
            };

            if matched {
                // Show absolute path for system-wide clarity
                matches.push(entry.path().to_string_lossy().to_string());
            }

            // Limit results
            if matches.len() >= 100 {
                matches.push("... (results truncated, showing first 100)".to_string());
                break;
            }
        }

        if matches.is_empty() {
            Ok(format!("No files matching '{}' found", pattern))
        } else {
            Ok(matches.join("\n"))
        }
    }
}

/// Simple glob pattern matching.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        // No wildcards
        return pattern == text;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        match text[pos..].find(part) {
            Some(idx) => {
                // First part must be at start if pattern doesn't start with *
                if i == 0 && idx != 0 {
                    return false;
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }

    // Last part must be at end if pattern doesn't end with *
    if !pattern.ends_with('*') && !parts.last().unwrap().is_empty() {
        return text.ends_with(parts.last().unwrap());
    }

    true
}

