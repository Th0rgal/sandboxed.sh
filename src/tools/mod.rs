//! Tool system for the agent.
//!
//! Tools are the "hands and eyes" of the agent - they allow it to interact with
//! the entire file system, run commands anywhere, search the machine, and access the web.
//!
//! ## Workspace-First Design
//!
//! Tools are designed to work **relative to the workspace** by default:
//! - Relative paths (e.g., `output/report.md`) resolve from the workspace directory
//! - Absolute paths (e.g., `/etc/hosts`) work as an escape hatch for system access
//!
//! This encourages agents to stay within their assigned workspace while preserving
//! flexibility for tasks that require broader access.

mod composite;
mod desktop;
mod directory;
mod file_ops;
mod index;
pub mod mission;
mod search;
mod storage;
mod terminal;
mod ui;
mod web;

pub use directory::{ListDirectory, SearchFiles};
pub use file_ops::{DeleteFile, ReadFile, WriteFile};
pub use search::GrepSearch;
pub use terminal::RunCommand;
pub use web::FetchUrl;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ============================================================================
// Path Resolution Utilities
// ============================================================================

/// Result of resolving a path relative to the workspace.
#[derive(Debug, Clone)]
pub struct PathResolution {
    /// The original path string provided by the agent.
    pub original: String,
    /// The fully resolved absolute path.
    pub resolved: PathBuf,
    /// Whether the resolved path is outside the workspace.
    pub is_outside_workspace: bool,
    /// Whether the original path was absolute.
    pub was_absolute: bool,
}

impl PathResolution {
    /// Format a note about path resolution for tool output.
    ///
    /// Returns empty string if path was relative and inside workspace (normal case).
    /// Returns a note if path was absolute or outside workspace.
    pub fn note(&self) -> String {
        if self.was_absolute {
            format!("[absolute path: {}]", self.resolved.display())
        } else if self.is_outside_workspace {
            format!("[resolved to: {}]", self.resolved.display())
        } else {
            String::new()
        }
    }
}

/// Resolve a path relative to the workspace.
///
/// - Relative paths are joined with `workspace`
/// - Absolute paths are used as-is (escape hatch)
///
/// Returns a `PathResolution` with metadata about the resolution.
pub fn resolve_path(path_str: &str, workspace: &Path) -> PathResolution {
    let path = Path::new(path_str);
    let was_absolute = path.is_absolute();

    let resolved = if was_absolute {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };

    // Canonicalize for accurate comparison (handles .., symlinks, etc.)
    let canonical_resolved = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    let is_outside_workspace = !canonical_resolved.starts_with(&canonical_workspace);

    PathResolution {
        original: path_str.to_string(),
        resolved,
        is_outside_workspace,
        was_absolute,
    }
}

/// Simple path resolution that just returns the resolved path.
///
/// Use this when you don't need the full `PathResolution` metadata.
pub fn resolve_path_simple(path_str: &str, workspace: &Path) -> PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

/// Safely truncate a string to a maximum number of bytes at a valid UTF-8 boundary.
///
/// Returns an index that is safe to use for slicing without breaking multi-byte characters.
pub fn safe_truncate_index(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

// ============================================================================
// Tool Trait and Registry
// ============================================================================

use async_trait::async_trait;
use serde_json::Value;

/// Information about a tool for display purposes.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

/// Trait for implementing tools.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The unique name of this tool.
    fn name(&self) -> &str;

    /// A description of what this tool does.
    fn description(&self) -> &str;

    /// JSON schema for the tool's parameters.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool with the given arguments.
    ///
    /// The `working_dir` is the default directory for relative paths.
    /// Tools can accept absolute paths to operate anywhere on the system.
    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String>;
}

/// Registry of available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new registry with all default tools.
    pub fn new() -> Self {
        Self::with_mission_control(None)
    }

    /// Create an empty registry (no built-in tools).
    pub fn empty() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Create a new registry with all default tools and optional mission control.
    pub fn with_mission_control(mission_control: Option<mission::MissionControl>) -> Self {
        let registry_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        tracing::debug!("Creating ToolRegistry {}", registry_id);
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();

        // File operations
        tools.insert("read_file".to_string(), Arc::new(file_ops::ReadFile));
        tools.insert("write_file".to_string(), Arc::new(file_ops::WriteFile));
        tools.insert("delete_file".to_string(), Arc::new(file_ops::DeleteFile));

        // Directory operations
        tools.insert(
            "list_directory".to_string(),
            Arc::new(directory::ListDirectory),
        );
        tools.insert("search_files".to_string(), Arc::new(directory::SearchFiles));

        // Indexing (optional performance optimization for large trees)
        tools.insert("index_files".to_string(), Arc::new(index::IndexFiles));
        tools.insert(
            "search_file_index".to_string(),
            Arc::new(index::SearchFileIndex),
        );

        // Terminal
        tools.insert("run_command".to_string(), Arc::new(terminal::RunCommand));

        // Search
        tools.insert("grep_search".to_string(), Arc::new(search::GrepSearch));

        // Web (fetch only; web search removed in favor of OMO/Exa)
        tools.insert("fetch_url".to_string(), Arc::new(web::FetchUrl));

        // Frontend Tool UI (schemas for rich rendering in the dashboard)
        tools.insert("ui_optionList".to_string(), Arc::new(ui::UiOptionList));
        tools.insert("ui_dataTable".to_string(), Arc::new(ui::UiDataTable));

        // Storage (file sharing - requires Supabase)
        tools.insert("share_file".to_string(), Arc::new(storage::ShareFile));

        // Composite tools (higher-level workflow operations)
        tools.insert(
            "analyze_codebase".to_string(),
            Arc::new(composite::AnalyzeCodebase),
        );
        tools.insert("deep_search".to_string(), Arc::new(composite::DeepSearch));
        tools.insert(
            "prepare_project".to_string(),
            Arc::new(composite::PrepareProject),
        );
        tools.insert("debug_error".to_string(), Arc::new(composite::DebugError));

        // Desktop automation (conditional on DESKTOP_ENABLED)
        if std::env::var("DESKTOP_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false)
        {
            tools.insert(
                "desktop_start_session".to_string(),
                Arc::new(desktop::StartSession),
            );
            tools.insert(
                "desktop_stop_session".to_string(),
                Arc::new(desktop::StopSession),
            );
            tools.insert(
                "desktop_screenshot".to_string(),
                Arc::new(desktop::Screenshot),
            );
            tools.insert("desktop_type".to_string(), Arc::new(desktop::TypeText));
            tools.insert("desktop_click".to_string(), Arc::new(desktop::Click));
            tools.insert("desktop_get_text".to_string(), Arc::new(desktop::GetText));
            tools.insert(
                "desktop_mouse_move".to_string(),
                Arc::new(desktop::MouseMove),
            );
            tools.insert("desktop_scroll".to_string(), Arc::new(desktop::Scroll));
            tools.insert(
                "desktop_i3_command".to_string(),
                Arc::new(desktop::I3Command),
            );
        }

        // Mission control (allows agent to complete/fail missions)
        let mission_tool: Arc<dyn Tool> = match mission_control {
            Some(ctrl) => Arc::new(mission::CompleteMission::with_control(ctrl)),
            None => Arc::new(mission::CompleteMission::new()),
        };
        tools.insert("complete_mission".to_string(), mission_tool);

        tracing::info!(
            "Registry {} complete with {} total tools",
            registry_id,
            tools.len()
        );
        Self { tools }
    }

    /// List all available tools.
    pub fn list_tools(&self) -> Vec<ToolInfo> {
        self.tools
            .values()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
            })
            .collect()
    }

    /// Check if a tool exists by name.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Execute a tool by name.
    ///
    /// The `working_dir` is the default directory for relative paths.
    /// Tools accept absolute paths to operate anywhere on the system.
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        working_dir: &Path,
    ) -> anyhow::Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", name))?;

        tool.execute(args, working_dir).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
