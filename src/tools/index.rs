//! Local filesystem indexing tools.
//!
//! These tools exist to make "search the machine" fast without repeatedly walking very large
//! directory trees. They build and query a simple on-disk index under:
//! `{working_dir}/.sandboxed_sh/index/`
//!
//! Note: the agent still has full system access; indexing is an optimization and a convention.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

use super::{resolve_path_simple as resolve_path, Tool};

fn default_index_dir(working_dir: &Path) -> PathBuf {
    working_dir.join(".sandboxed_sh").join("index")
}

fn default_index_file(working_dir: &Path) -> PathBuf {
    default_index_dir(working_dir).join("files.txt")
}

fn default_meta_file(working_dir: &Path) -> PathBuf {
    default_index_dir(working_dir).join("meta.json")
}

fn default_ignore_dirs() -> Vec<String> {
    vec![
        ".git".into(),
        ".sandboxed_sh".into(),
        ".next".into(),
        "node_modules".into(),
        "target".into(),
        "dist".into(),
        "build".into(),
        "__pycache__".into(),
        ".venv".into(),
        "venv".into(),
        // Common pseudo/volatile dirs on Linux; harmless if not present.
        "proc".into(),
        "sys".into(),
        "dev".into(),
        "run".into(),
    ]
}

fn is_ignored_dir(name: &str, ignore_dirs: &[String]) -> bool {
    ignore_dirs.iter().any(|d| d == name)
}

/// Build/refresh an on-disk index of file paths under a directory.
pub struct IndexFiles;

#[async_trait]
impl Tool for IndexFiles {
    fn name(&self) -> &str {
        "index_files"
    }

    fn description(&self) -> &str {
        "Build or refresh a file-path index for fast machine search. Writes an index file under {working_dir}/.sandboxed_sh/index/ by default. Use this before searching huge directories repeatedly."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to index. Can be absolute (e.g., /root, /etc) or relative to working_dir. Defaults to working_dir."
                },
                "output_path": {
                    "type": "string",
                    "description": "Optional: where to write the index file (one path per line). Defaults to {working_dir}/.sandboxed_sh/index/files.txt"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Optional: maximum directory depth to traverse (default: unlimited)."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Optional: stop after indexing this many files (default: 200000)."
                },
                "ignore_dirs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: directory names to skip (exact match). If omitted, uses a sensible default (e.g., .git, node_modules, target, proc, sys)."
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Whether to include hidden directories (starting with '.') (default: false; except '.' itself)."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let path = args["path"].as_str().unwrap_or(".");
        let out_path = args["output_path"].as_str();
        let max_depth = args["max_depth"].as_u64().map(|n| n as usize);
        let max_files = args["max_files"].as_u64().unwrap_or(200_000) as usize;
        let include_hidden = args["include_hidden"].as_bool().unwrap_or(false);
        let ignore_dirs: Vec<String> = args["ignore_dirs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_else(default_ignore_dirs);

        let root = resolve_path(path, working_dir);
        if !root.exists() {
            return Err(anyhow::anyhow!("Directory not found: {}", path));
        }
        if !root.is_dir() {
            return Err(anyhow::anyhow!("Not a directory: {}", path));
        }

        let index_path = out_path
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| default_index_file(working_dir));

        if let Some(parent) = index_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut f = tokio::fs::File::create(&index_path).await?;

        let mut count = 0usize;
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .max_depth(max_depth.unwrap_or(usize::MAX))
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    if !include_hidden && name.starts_with('.') {
                        return false;
                    }
                    if is_ignored_dir(&name, &ignore_dirs) {
                        return false;
                    }
                }
                true
            });

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path().to_string_lossy();
            f.write_all(p.as_bytes()).await?;
            f.write_all(b"\n").await?;
            count += 1;
            if count >= max_files {
                break;
            }
        }

        f.flush().await?;

        // Write a small metadata file next to the index (best effort).
        let meta_path = default_meta_file(working_dir);
        if let Some(parent) = meta_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let meta = json!({
            "root": root.to_string_lossy(),
            "index_path": index_path.to_string_lossy(),
            "created_at": Utc::now().to_rfc3339(),
            "file_count": count,
            "max_files": max_files,
            "max_depth": max_depth,
            "include_hidden": include_hidden,
            "ignore_dirs": ignore_dirs,
        });
        let _ = tokio::fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?).await;

        Ok(format!(
            "Indexed {} files under {} into {}",
            count,
            root.to_string_lossy(),
            index_path.to_string_lossy()
        ))
    }
}

/// Search the on-disk file index built by `index_files`.
pub struct SearchFileIndex;

#[async_trait]
impl Tool for SearchFileIndex {
    fn name(&self) -> &str {
        "search_file_index"
    }

    fn description(&self) -> &str {
        "Search the file-path index produced by index_files. Much faster than walking the filesystem repeatedly. Supports substring search or simple '*' glob patterns."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Substring or simple glob (supports '*') to match against indexed paths"
                },
                "index_path": {
                    "type": "string",
                    "description": "Optional: index file to read. Defaults to {working_dir}/.sandboxed_sh/index/files.txt"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max matches to return (default: 100)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether matching is case-sensitive (default: false)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' argument"))?;
        let index_path = args["index_path"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| default_index_file(working_dir));
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;
        let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);

        if !index_path.exists() {
            return Ok(format!(
                "Index file not found: {}.\nRun index_files first (e.g., index_files {{\"path\":\"/root\"}}).",
                index_path.to_string_lossy()
            ));
        }

        let content = tokio::fs::read_to_string(&index_path).await?;
        let is_glob = query.contains('*');

        let q = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };

        let mut matches = Vec::new();
        for line in content.lines() {
            let hay = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };

            let matched = if is_glob {
                glob_match(&q, &hay)
            } else {
                hay.contains(&q)
            };

            if matched {
                matches.push(line.to_string());
                if matches.len() >= limit {
                    break;
                }
            }
        }

        if matches.is_empty() {
            Ok(format!(
                "No matches for '{}' in {}",
                query,
                index_path.to_string_lossy()
            ))
        } else if matches.len() >= limit {
            Ok(format!(
                "{}\n\n... (showing first {} matches)",
                matches.join("\n"),
                limit
            ))
        } else {
            Ok(matches.join("\n"))
        }
    }
}

/// Simple glob pattern matching (supports '*' only).
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match text[pos..].find(part) {
            Some(idx) => {
                if i == 0 && idx != 0 {
                    return false;
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }

    if !pattern.ends_with('*') && !parts.last().unwrap().is_empty() {
        return text.ends_with(parts.last().unwrap());
    }

    true
}
