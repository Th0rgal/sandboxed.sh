//! Types for the configuration library.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::workspace::TailscaleMode;

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
    /// Init script fragment names to include (executed in order)
    #[serde(default)]
    pub init_scripts: Vec<String>,
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
    /// Keys of env vars that should be encrypted at rest
    #[serde(default)]
    pub encrypted_keys: Vec<String>,
    /// Init script fragment names to include (executed in order)
    #[serde(default)]
    pub init_scripts: Vec<String>,
    /// Custom init script to run on build (appended after fragments)
    #[serde(default)]
    pub init_script: String,
    /// Whether to share the host network (default: true).
    /// When true, bind-mounts /etc/resolv.conf for DNS.
    /// Set to false for isolated networking (e.g., Tailscale).
    #[serde(default)]
    pub shared_network: Option<bool>,
    /// Tailscale networking mode when shared_network is false.
    /// - `exit_node`: Route all traffic through Tailscale exit node
    /// - `tailnet_only`: Connect to tailnet but use host gateway for internet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale_mode: Option<TailscaleMode>,
    /// MCP server names to enable for workspaces created from this template.
    /// Empty = use default MCPs (those with `default_enabled = true`).
    #[serde(default)]
    pub mcps: Vec<String>,
    /// Config profile to use for workspaces created from this template.
    /// Defaults to "default" if not specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_profile: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Init Script Fragment Types
// ─────────────────────────────────────────────────────────────────────────────

/// Init script fragment summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitScriptSummary {
    /// Fragment name (folder name, e.g., "base", "ssh-keys")
    pub name: String,
    /// Description extracted from first comment line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root (e.g., "init-script/base/SCRIPT.sh")
    pub path: String,
}

/// Full init script fragment with content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitScript {
    /// Fragment name
    pub name: String,
    /// Description extracted from first comment line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to library root
    pub path: String,
    /// Full script content
    pub content: String,
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

/// Source/provenance of a skill - local or from skills.sh registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum SkillSource {
    /// Locally created skill
    Local,
    /// Skill installed from skills.sh registry
    SkillsRegistry {
        /// Repository identifier (e.g., "vercel-labs/agent-skills")
        identifier: String,
        /// Specific skill name within the repo (for multi-skill repos)
        #[serde(skip_serializing_if = "Option::is_none")]
        skill_name: Option<String>,
        /// Pinned version/commit hash
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        /// When the skill was first installed
        #[serde(skip_serializing_if = "Option::is_none")]
        installed_at: Option<String>,
        /// When the skill was last updated
        #[serde(skip_serializing_if = "Option::is_none")]
        updated_at: Option<String>,
    },
}

impl Default for SkillSource {
    fn default() -> Self {
        SkillSource::Local
    }
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
    /// Source/provenance of the skill
    #[serde(default)]
    pub source: SkillSource,
    /// Shell commands to run during workspace setup (e.g., install dependencies)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_commands: Vec<String>,
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
    /// Source/provenance of the skill
    #[serde(default)]
    pub source: SkillSource,
    /// Primary SKILL.md content (for backwards compatibility)
    pub content: String,
    /// All markdown files in the skill folder
    #[serde(default)]
    pub files: Vec<SkillFile>,
    /// List of non-.md reference files
    #[serde(default)]
    pub references: Vec<String>,
    /// Shell commands to run during workspace setup (e.g., install dependencies)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_commands: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Command Types
// ─────────────────────────────────────────────────────────────────────────────

/// A single command parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandParam {
    /// Parameter name (e.g., "repo-path")
    pub name: String,
    /// Whether this parameter is required
    #[serde(default)]
    pub required: bool,
    /// Description of the parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

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
    /// Parameters this command accepts (from frontmatter)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<CommandParam>,
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
    /// Parameters this command accepts (from frontmatter)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<CommandParam>,
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
// Sandboxed Config Types
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

/// Sandboxed configuration stored in the Library.
/// Controls agent visibility and defaults in the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxedConfig {
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

impl Default for SandboxedConfig {
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

/// Claude Code configuration stored in the Library.
/// Controls default model, agent preferences, and visibility for Claude Code backend.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeCodeConfig {
    /// Default model to use for Claude Code missions.
    /// Example: "claude-sonnet-4-20250514", "claude-opus-4-20250514"
    #[serde(default)]
    pub default_model: Option<String>,
    /// Default agent to pre-select for Claude Code missions.
    #[serde(default)]
    pub default_agent: Option<String>,
    /// List of agents to hide from the mission dialog.
    /// These agents won't appear in the dropdown but can still be used via API.
    #[serde(default)]
    pub hidden_agents: Vec<String>,
}

/// Amp Code configuration stored in the Library.
/// Controls default mode and settings for Amp backend.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AmpCodeConfig {
    /// Default mode to use for Amp missions ("smart" or "rush").
    #[serde(default)]
    pub default_mode: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Config Profile Types
// ─────────────────────────────────────────────────────────────────────────────

/// Config profile summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProfileSummary {
    /// Profile name (folder name, e.g., "default", "development", "production")
    pub name: String,
    /// Whether this is the default profile used when creating new workspaces
    #[serde(default)]
    pub is_default: bool,
    /// Path relative to library root (e.g., "configs/default")
    pub path: String,
}

/// A file within a config profile (for file-based editing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProfileFile {
    /// Relative path within the profile (e.g., ".opencode/settings.json")
    pub path: String,
    /// File content as string
    pub content: String,
}

/// Full config profile with all harness configurations.
/// A profile is an instance of configs for OpenCode, Claude Code, Amp, and Sandboxed.
///
/// Directory structure mirrors actual harness config directories:
/// - `.opencode/` - OpenCode settings (settings.json, oh-my-opencode.json)
/// - `.claudecode/` - Claude Code settings (settings.json)
/// - `.ampcode/` - Amp settings (settings.json)
/// - `.sandboxed-sh/` - Sandboxed config (config.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProfile {
    /// Profile name
    pub name: String,
    /// Whether this is the default profile
    #[serde(default)]
    pub is_default: bool,
    /// Path relative to library root
    pub path: String,
    /// All files in the profile (for file-based editing)
    #[serde(default)]
    pub files: Vec<ConfigProfileFile>,
    /// OpenCode settings (oh-my-opencode.json content) - legacy, for backward compat
    #[serde(default)]
    pub opencode_settings: serde_json::Value,
    /// Sandboxed config - legacy, for backward compat
    #[serde(default)]
    pub sandboxed_config: SandboxedConfig,
    /// Claude Code config - legacy, for backward compat
    #[serde(default)]
    pub claudecode_config: ClaudeCodeConfig,
    /// Amp Code config
    #[serde(default)]
    pub ampcode_config: AmpCodeConfig,
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

    // Normalize line endings to handle Windows-style \r\n
    let normalized = content.replace("\r\n", "\n");
    let normalized_ref = normalized.as_str();

    let rest = &normalized_ref[3..];

    // Find closing --- (could be \n--- or end of yaml section)
    if let Some(end_pos) = rest.find("\n---") {
        let yaml_str = rest[..end_pos].trim();

        // Try serde_yaml first
        match serde_yaml::from_str(yaml_str) {
            Ok(value) => {
                // Return body slice from original content if possible, otherwise use normalized
                let original_body = find_body_in_original(content);
                return (Some(value), original_body);
            }
            Err(e) => {
                // Log the error for debugging but try fallback parser
                tracing::debug!("serde_yaml parse failed, trying fallback: {}", e);
            }
        }

        // Fallback: simple line-by-line parser for basic key: value pairs
        if let Some(fallback) = parse_simple_frontmatter(yaml_str) {
            let original_body = find_body_in_original(content);
            return (Some(fallback), original_body);
        }

        // Both parsers failed
        (None, content)
    } else {
        (None, content)
    }
}

/// Find the body content in the original (non-normalized) content.
fn find_body_in_original(content: &str) -> &str {
    // Look for closing --- after the opening ---
    if let Some(start) = content.find("---") {
        let after_open = &content[start + 3..];
        // Find closing delimiter (handles both \n--- and \r\n---)
        if let Some(close_pos) = after_open.find("\n---") {
            let body_start = start + 3 + close_pos + 4;
            if body_start < content.len() {
                return content[body_start..].trim_start();
            }
        } else if let Some(close_pos) = after_open.find("\r\n---") {
            let body_start = start + 3 + close_pos + 5;
            if body_start < content.len() {
                return content[body_start..].trim_start();
            }
        }
    }
    content
}

/// Simple fallback parser for basic YAML frontmatter.
/// Handles simple key: value pairs when serde_yaml fails.
fn parse_simple_frontmatter(yaml_str: &str) -> Option<serde_yaml::Value> {
    use serde_yaml::{Mapping, Value};

    let mut map = Mapping::new();
    let mut current_key: Option<String> = None;
    let mut current_value = String::new();
    let mut in_multiline = false;

    for line in yaml_str.lines() {
        let trimmed = line.trim();

        // Skip empty lines at the start
        if trimmed.is_empty() && current_key.is_none() {
            continue;
        }

        // Check for new key: value pair
        if let Some(colon_pos) = line.find(':') {
            let potential_key = line[..colon_pos].trim();
            // Only treat as new key if it starts at beginning (not indented) and is alphanumeric
            if !line.starts_with(' ') && !line.starts_with('\t') && !potential_key.is_empty() {
                // Save previous key-value if exists
                if let Some(key) = current_key.take() {
                    let val = current_value.trim().to_string();
                    map.insert(Value::String(key), Value::String(val));
                    current_value.clear();
                }

                current_key = Some(potential_key.to_string());
                let value_part = line[colon_pos + 1..].trim();

                // Check for multiline indicators
                if value_part == ">"
                    || value_part == "|"
                    || value_part == ">-"
                    || value_part == "|-"
                {
                    in_multiline = true;
                    current_value.clear();
                } else {
                    in_multiline = false;
                    // Remove surrounding quotes if present
                    let cleaned = value_part
                        .trim_start_matches('"')
                        .trim_end_matches('"')
                        .trim_start_matches('\'')
                        .trim_end_matches('\'');
                    current_value = cleaned.to_string();
                }
                continue;
            }
        }

        // Continuation of multiline value
        if in_multiline && current_key.is_some() {
            if !current_value.is_empty() {
                current_value.push(' ');
            }
            current_value.push_str(trimmed);
        }
    }

    // Save last key-value
    if let Some(key) = current_key {
        let val = current_value.trim().to_string();
        map.insert(Value::String(key), Value::String(val));
    }

    if map.is_empty() {
        None
    } else {
        Some(Value::Mapping(map))
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

/// Extract command params from YAML frontmatter.
/// Supports two formats:
/// 1. Simple list: `params: [repo-path, pr-number]`
/// 2. Detailed objects: `params: [{name: repo-path, required: true, description: "..."}]`
pub fn extract_params(frontmatter: &Option<serde_yaml::Value>) -> Vec<CommandParam> {
    frontmatter
        .as_ref()
        .and_then(|fm| fm.get("params"))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| {
                    // Format 1: Simple string
                    if let Some(name) = item.as_str() {
                        return Some(CommandParam {
                            name: name.to_string(),
                            required: true, // Default to required for simple format
                            description: None,
                        });
                    }

                    // Format 2: Object with name, required, description
                    if let Some(mapping) = item.as_mapping() {
                        let name = mapping
                            .get(serde_yaml::Value::String("name".to_string()))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())?;

                        let required = mapping
                            .get(serde_yaml::Value::String("required".to_string()))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);

                        let description = mapping
                            .get(serde_yaml::Value::String("description".to_string()))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        return Some(CommandParam {
                            name,
                            required,
                            description,
                        });
                    }

                    None
                })
                .collect()
        })
        .unwrap_or_default()
}
