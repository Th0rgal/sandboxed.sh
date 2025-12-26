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

mod browser;
mod composite;
mod desktop;
mod directory;
mod file_ops;
mod git;
mod github;
mod index;
pub mod memory;
pub mod mission;
mod search;
mod storage;
mod terminal;
mod ui;
mod web;

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

// ============================================================================
// Tool Trait and Registry
// ============================================================================

use async_trait::async_trait;
use serde_json::Value;

use crate::llm::{FunctionDefinition, ToolDefinition};

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
        Self::with_options(None, None)
    }

    /// Create a new registry with all default tools and optional mission control.
    pub fn with_mission_control(mission_control: Option<mission::MissionControl>) -> Self {
        Self::with_options(mission_control, None)
    }

    /// Create a new registry with all options.
    pub fn with_options(
        mission_control: Option<mission::MissionControl>,
        shared_memory: Option<memory::SharedMemory>,
    ) -> Self {
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

        // Web
        tools.insert("web_search".to_string(), Arc::new(web::WebSearch));
        tools.insert("fetch_url".to_string(), Arc::new(web::FetchUrl));

        // Git
        tools.insert("git_status".to_string(), Arc::new(git::GitStatus));
        tools.insert("git_diff".to_string(), Arc::new(git::GitDiff));
        tools.insert("git_commit".to_string(), Arc::new(git::GitCommit));
        tools.insert("git_log".to_string(), Arc::new(git::GitLog));

        // GitHub (uses `gh` CLI)
        tools.insert("github_clone".to_string(), Arc::new(github::GitHubClone));
        tools.insert(
            "github_list_repos".to_string(),
            Arc::new(github::GitHubListRepos),
        );
        tools.insert(
            "github_get_file".to_string(),
            Arc::new(github::GitHubGetFile),
        );
        tools.insert(
            "github_search_code".to_string(),
            Arc::new(github::GitHubSearchCode),
        );

        // Frontend Tool UI (schemas for rich rendering in the dashboard)
        tools.insert("ui_optionList".to_string(), Arc::new(ui::UiOptionList));
        tools.insert("ui_dataTable".to_string(), Arc::new(ui::UiDataTable));

        // Storage (image upload - requires Supabase)
        tools.insert("upload_image".to_string(), Arc::new(storage::UploadImage));

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

        // Browser automation (conditional on BROWSER_ENABLED)
        let browser_enabled = std::env::var("BROWSER_ENABLED").unwrap_or_default();
        tracing::info!("BROWSER_ENABLED env var = '{}'", browser_enabled);
        if browser_enabled.to_lowercase() == "true" || browser_enabled == "1" {
            tracing::info!("Registering browser automation tools");
            tools.insert(
                "browser_navigate".to_string(),
                Arc::new(browser::BrowserNavigate),
            );
            tools.insert(
                "browser_screenshot".to_string(),
                Arc::new(browser::BrowserScreenshot),
            );
            tools.insert(
                "browser_get_content".to_string(),
                Arc::new(browser::BrowserGetContent),
            );
            tools.insert("browser_click".to_string(), Arc::new(browser::BrowserClick));
            tools.insert("browser_type".to_string(), Arc::new(browser::BrowserType));
            tools.insert(
                "browser_evaluate".to_string(),
                Arc::new(browser::BrowserEvaluate),
            );
            tools.insert("browser_wait".to_string(), Arc::new(browser::BrowserWait));
            tools.insert("browser_close".to_string(), Arc::new(browser::BrowserClose));
            tools.insert(
                "browser_list_elements".to_string(),
                Arc::new(browser::BrowserListElements),
            );
            tracing::info!(
                "Registry {}: Added 9 browser tools, total now: {}",
                registry_id,
                tools.len()
            );
        }

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

        // Memory tools (if memory system is available)
        if let Some(mem) = shared_memory {
            tools.insert(
                "search_memory".to_string(),
                Arc::new(memory::SearchMemory::new(Arc::clone(&mem))),
            );
            tools.insert(
                "store_fact".to_string(),
                Arc::new(memory::StoreFact::new(Arc::clone(&mem))),
            );
        }

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

    /// Get tool schemas in LLM-compatible format.
    pub fn get_tool_schemas(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.parameters_schema(),
                },
            })
            .collect()
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
