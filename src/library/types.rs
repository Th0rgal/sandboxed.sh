//! Types for the configuration library.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────────
// MCP Server Types (OpenCode-aligned format)
// ─────────────────────────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

/// MCP server definition from mcp/servers.json.
/// Aligned with OpenCode format: "local" (stdio) and "remote" (http).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServer {
    /// Local MCP server (stdio-based)
    Local {
        /// Command array: ["npx", "@playwright/mcp@latest"]
        command: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// Remote MCP server (HTTP-based)
    Remote {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default = "default_true")]
        enabled: bool,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin Types
// ─────────────────────────────────────────────────────────────────────────────

/// UI metadata for a plugin (used by dashboard).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginUI {
    /// Lucide icon name (e.g., "zap", "refresh-cw")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Display name (e.g., "Ralph Wiggum")
    pub label: String,
    /// Short description/hint (e.g., "continuous running")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Category for grouping (e.g., "automation", "observability")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Plugin definition from plugins.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    /// npm package name (e.g., "oh-my-opencode", "@opencode/ralph-wiggum")
    pub package: String,
    /// Description of what this plugin does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the plugin is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// UI metadata for dashboard display
    pub ui: PluginUI,
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule Types (AGENTS.md style instructions)
// ─────────────────────────────────────────────────────────────────────────────

/// Rule summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSummary {
    /// Rule name (filename without .md)
    pub name: String,
    /// Description from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "rule/code-style.md")
    pub path: String,
}

/// Full rule with content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Rule name
    pub name: String,
    /// Description from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Full markdown content
    pub content: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Agent Types (OpenCode agent definitions)
// ─────────────────────────────────────────────────────────────────────────────

/// Library agent summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryAgentSummary {
    /// Agent name (filename without .md)
    pub name: String,
    /// Description from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
}

/// Full library agent definition.
/// These are OpenCode agent definitions stored as markdown with YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryAgent {
    /// Agent name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Full markdown content (frontmatter + body)
    pub content: String,
    /// Model ID (e.g., "claude-sonnet-4-20250514") - extracted from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tool patterns: {"read": true, "write": false, "playwright_*": true}
    #[serde(default)]
    pub tools: HashMap<String, bool>,
    /// Permission levels: {"bash": "ask", "write": "allow"}
    #[serde(default)]
    pub permissions: HashMap<String, String>,
    /// Rules to include by name
    #[serde(default)]
    pub rules: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Tool Types (TypeScript tool definitions)
// ─────────────────────────────────────────────────────────────────────────────

/// Library tool summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryToolSummary {
    /// Tool name (filename without .ts)
    pub name: String,
    /// Description extracted from code comments or export
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "tool/database-query.ts")
    pub path: String,
}

/// Full library tool with content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryTool {
    /// Tool name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Full TypeScript content
    pub content: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Template Types
// ─────────────────────────────────────────────────────────────────────────────

/// Workspace template summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTemplateSummary {
    /// Template name
    pub name: String,
    /// Description from template file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "workspace-template/basic-ubuntu.json")
    pub path: String,
    /// Preferred distro (if set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Skills enabled for this template (optional summary)
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Full workspace template definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTemplate {
    /// Template name
    pub name: String,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Preferred distro (if set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Skills enabled for this workspace template
    #[serde(default)]
    pub skills: Vec<String>,
    /// Environment variables for the workspace
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    /// Init script to run on build
    #[serde(default)]
    pub init_script: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Skill Types (supports multiple .md files per skill)
// ─────────────────────────────────────────────────────────────────────────────

/// A single markdown file within a skill folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFile {
    /// File name (e.g., "SKILL.md", "examples.md")
    pub name: String,
    /// Path relative to skill folder
    pub path: String,
    /// Full file content
    pub content: String,
}

/// Skill summary for listing (without full content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSummary {
    /// Skill name (folder name, e.g., "frontend-development")
    pub name: String,
    /// Description from SKILL.md frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "skill/frontend-development")
    pub path: String,
}

/// Full skill with content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name (folder name)
    pub name: String,
    /// Description from SKILL.md frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Primary SKILL.md content (for backwards compatibility)
    pub content: String,
    /// All markdown files in the skill folder
    #[serde(default)]
    pub files: Vec<SkillFile>,
    /// List of non-.md reference files
    #[serde(default)]
    pub references: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Command Types
// ─────────────────────────────────────────────────────────────────────────────

/// Command summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSummary {
    /// Command name (filename without .md, e.g., "review-pr")
    pub name: String,
    /// Description from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "command/review-pr.md")
    pub path: String,
}

/// Full command with content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    /// Command name
    pub name: String,
    /// Description from frontmatter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Full markdown content
    pub content: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Status
// ─────────────────────────────────────────────────────────────────────────────

/// Git status for the library repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryStatus {
    /// Absolute path to the library
    pub path: String,
    /// Git remote URL if configured
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Current branch name
    pub branch: String,
    /// True if working directory is clean
    pub clean: bool,
    /// Number of commits ahead of remote
    pub ahead: u32,
    /// Number of commits behind remote
    pub behind: u32,
    /// List of modified/untracked files
    pub modified_files: Vec<String>,
}

/// Migration report showing what changed during library structure migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationReport {
    /// Directories that were renamed
    pub directories_renamed: Vec<(String, String)>,
    /// Files that were converted (e.g., MCP format changes)
    pub files_converted: Vec<String>,
    /// Errors encountered during migration
    pub errors: Vec<String>,
    /// Whether migration was successful overall
    pub success: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenAgent Config Types
// ─────────────────────────────────────────────────────────────────────────────

/// Desktop session lifecycle configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// Grace period in seconds before auto-closing orphaned desktop sessions.
    /// Orphaned sessions are those where the owning mission has completed.
    /// Set to 0 to disable auto-close. Default: 7200 (2 hours).
    #[serde(default = "default_auto_close_grace_period")]
    pub auto_close_grace_period_secs: u64,

    /// Interval in seconds for the background cleanup sweep.
    /// Default: 900 (15 minutes).
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_secs: u64,

    /// Number of seconds before auto-close to show a warning notification.
    /// Set to 0 to disable warnings. Default: 300 (5 minutes).
    #[serde(default = "default_warning_before_close")]
    pub warning_before_close_secs: u64,
}

fn default_auto_close_grace_period() -> u64 {
    7200 // 2 hours
}

fn default_cleanup_interval() -> u64 {
    900 // 15 minutes
}

fn default_warning_before_close() -> u64 {
    300 // 5 minutes
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            auto_close_grace_period_secs: default_auto_close_grace_period(),
            cleanup_interval_secs: default_cleanup_interval(),
            warning_before_close_secs: default_warning_before_close(),
        }
    }
}

/// OpenAgent configuration stored in the Library.
/// Controls agent visibility and defaults in the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAgentConfig {
    /// Agents to hide from the mission dialog selector.
    /// These are typically internal/system agents that users shouldn't select directly.
    #[serde(default)]
    pub hidden_agents: Vec<String>,
    /// Default agent to pre-select in the mission dialog.
    #[serde(default)]
    pub default_agent: Option<String>,
    /// Desktop session lifecycle configuration.
    #[serde(default)]
    pub desktop: DesktopConfig,
}

impl Default for OpenAgentConfig {
    fn default() -> Self {
        Self {
            hidden_agents: vec![
                "build".to_string(),
                "plan".to_string(),
                "general".to_string(),
                "explore".to_string(),
                "compaction".to_string(),
                "title".to_string(),
                "summary".to_string(),
                "Metis (Plan Consultant)".to_string(),
                "Momus (Plan Reviewer)".to_string(),
                "orchestrator-sisyphus".to_string(),
            ],
            default_agent: Some("Sisyphus".to_string()),
            desktop: DesktopConfig::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ─────────────────────────────────────────────────────────────────────────────

/// Parse YAML frontmatter from markdown content.
/// Returns (frontmatter, body) where frontmatter is the parsed YAML.
pub fn parse_frontmatter(content: &str) -> (Option<serde_yaml::Value>, &str) {
    if !content.starts_with("---") {
        return (None, content);
    }

    let rest = &content[3..];
    if let Some(end_pos) = rest.find("\n---") {
        let yaml_str = &rest[..end_pos];
        let body = &rest[end_pos + 4..].trim_start();

        match serde_yaml::from_str(yaml_str) {
            Ok(value) => (Some(value), body),
            Err(_) => (None, content),
        }
    } else {
        (None, content)
    }
}

/// Extract description from YAML frontmatter.
pub fn extract_description(frontmatter: &Option<serde_yaml::Value>) -> Option<String> {
    frontmatter.as_ref().and_then(|fm| {
        fm.get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract name from YAML frontmatter (optional, usually from filename).
pub fn extract_name(frontmatter: &Option<serde_yaml::Value>) -> Option<String> {
    frontmatter.as_ref().and_then(|fm| {
        fm.get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract model from YAML frontmatter.
pub fn extract_model(frontmatter: &Option<serde_yaml::Value>) -> Option<String> {
    frontmatter.as_ref().and_then(|fm| {
        fm.get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract tools map from YAML frontmatter.
pub fn extract_tools(frontmatter: &Option<serde_yaml::Value>) -> HashMap<String, bool> {
    frontmatter
        .as_ref()
        .and_then(|fm| fm.get("tools"))
        .and_then(|v| v.as_mapping())
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(k, v)| {
                    let key = k.as_str()?.to_string();
                    let value = v.as_bool()?;
                    Some((key, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract permissions map from YAML frontmatter.
pub fn extract_permissions(frontmatter: &Option<serde_yaml::Value>) -> HashMap<String, String> {
    frontmatter
        .as_ref()
        .and_then(|fm| fm.get("permissions"))
        .and_then(|v| v.as_mapping())
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(k, v)| {
                    let key = k.as_str()?.to_string();
                    let value = v.as_str()?.to_string();
                    Some((key, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract string array from YAML frontmatter field.
pub fn extract_string_array(frontmatter: &Option<serde_yaml::Value>, field: &str) -> Vec<String> {
    frontmatter
        .as_ref()
        .and_then(|fm| fm.get(field))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
