//! Tool system for the agent.
//!
//! Tools are the "hands and eyes" of the agent - they allow it to interact with
//! the entire file system, run commands anywhere, search the machine, and access the web.
//!
//! The agent has **full system access** - it can read/write any file, execute any command,
//! and search anywhere on the machine. The `working_dir` parameter is the default directory
//! for relative paths (typically `/root` in production).

mod directory;
mod file_ops;
mod git;
mod index;
mod search;
mod terminal;
mod ui;
mod web;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

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
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();

        // File operations
        tools.insert("read_file".to_string(), Arc::new(file_ops::ReadFile));
        tools.insert("write_file".to_string(), Arc::new(file_ops::WriteFile));
        tools.insert("delete_file".to_string(), Arc::new(file_ops::DeleteFile));

        // Directory operations
        tools.insert("list_directory".to_string(), Arc::new(directory::ListDirectory));
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

        // Frontend Tool UI (schemas for rich rendering in the dashboard)
        tools.insert("ui_optionList".to_string(), Arc::new(ui::UiOptionList));
        tools.insert("ui_dataTable".to_string(), Arc::new(ui::UiDataTable));

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

