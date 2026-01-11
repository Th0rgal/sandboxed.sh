//! Configuration library management.
//!
//! This module manages a git-based configuration library containing:
//! - MCP server definitions (`mcp/servers.json`)
//! - Skills (`skill/*/SKILL.md` with additional .md files and references)
//! - Commands/prompts (`command/*.md`)
//! - Plugins registry (`plugins.json`)
//! - Rules (`rule/*.md`)
//! - Library agents (`agent/*.md`)
//! - Library tools (`tool/*.ts`)
//! - OpenCode settings (`opencode/oh-my-opencode.json`)
//! - OpenAgent config (`openagent/config.json`)

mod git;
pub mod types;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs;

pub use git::GitAuthor;
pub use types::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceTemplateConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distro: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    env_vars: HashMap<String, String>,
    #[serde(default)]
    init_script: String,
}

// Directory constants (OpenCode-aligned structure)
const SKILL_DIR: &str = "skill";
const COMMAND_DIR: &str = "command";
const AGENT_DIR: &str = "agent";
const TOOL_DIR: &str = "tool";
const RULE_DIR: &str = "rule";
const PLUGINS_FILE: &str = "plugins.json";
const WORKSPACE_TEMPLATE_DIR: &str = "workspace-template";
const OPENCODE_DIR: &str = "opencode";
const OPENAGENT_DIR: &str = "openagent";

/// Store for managing the configuration library.
pub struct LibraryStore {
    /// Path to the library directory
    path: PathBuf,
    /// Git remote URL
    remote: String,
}

impl LibraryStore {
    /// Create a new LibraryStore, cloning the repo if needed.
    pub async fn new(path: PathBuf, remote: &str) -> Result<Self> {
        // Clone if the repo doesn't exist
        git::clone_if_needed(&path, remote).await?;
        git::ensure_remote(&path, remote).await?;

        Ok(Self {
            path,
            remote: remote.to_string(),
        })
    }

    /// Get the library path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the remote URL.
    pub fn remote(&self) -> &str {
        &self.remote
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Git Operations
    // ─────────────────────────────────────────────────────────────────────────

    /// Get the current git status of the library.
    pub async fn status(&self) -> Result<LibraryStatus> {
        git::status(&self.path).await
    }

    /// Pull latest changes from remote.
    pub async fn sync(&self) -> Result<()> {
        git::pull(&self.path).await
    }

    /// Commit all changes with a message and optional author.
    pub async fn commit(&self, message: &str, author: Option<&git::GitAuthor>) -> Result<()> {
        git::commit(&self.path, message, author).await
    }

    /// Push changes to remote.
    pub async fn push(&self) -> Result<()> {
        git::push(&self.path).await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // MCP Servers (mcp/servers.json)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get all MCP server definitions.
    pub async fn get_mcp_servers(&self) -> Result<HashMap<String, McpServer>> {
        let path = self.path.join("mcp/servers.json");

        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read mcp/servers.json")?;

        // Be lenient with parse errors - log warning and return empty
        match serde_json::from_str::<HashMap<String, McpServer>>(&content) {
            Ok(servers) => Ok(servers),
            Err(e) => {
                tracing::warn!(
                    "Failed to parse mcp/servers.json, returning empty map: {}",
                    e
                );
                Ok(HashMap::new())
            }
        }
    }

    /// Save MCP server definitions.
    pub async fn save_mcp_servers(&self, servers: &HashMap<String, McpServer>) -> Result<()> {
        let path = self.path.join("mcp/servers.json");

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(servers)?;
        fs::write(&path, content)
            .await
            .context("Failed to write mcp/servers.json")?;

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Skills (skill/*/SKILL.md with additional .md files)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get the skills directory path.
    fn skills_dir(&self) -> PathBuf {
        self.path.join(SKILL_DIR)
    }

    /// List all skills with their summaries.
    pub async fn list_skills(&self) -> Result<Vec<SkillSummary>> {
        let skills_dir = self.skills_dir();

        if !skills_dir.exists() {
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        let mut entries = fs::read_dir(&skills_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process directories
            if !entry_path.is_dir() {
                continue;
            }

            let skill_md = entry_path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();

            // Read and parse frontmatter for description
            let content = fs::read_to_string(&skill_md).await.ok();
            let (frontmatter, _) = content
                .as_ref()
                .map(|c| parse_frontmatter(c))
                .unwrap_or((None, ""));

            let description = extract_description(&frontmatter);

            skills.push(SkillSummary {
                name,
                description,
                path: format!("{}/{}", SKILL_DIR, entry.file_name().to_string_lossy()),
            });
        }

        // Sort by name
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(skills)
    }

    /// Get a skill by name with full content.
    pub async fn get_skill(&self, name: &str) -> Result<Skill> {
        Self::validate_name(name)?;
        let skill_dir = self.skills_dir().join(name);
        let skill_md = skill_dir.join("SKILL.md");

        if !skill_md.exists() {
            anyhow::bail!("Skill not found: {}", name);
        }

        let content = fs::read_to_string(&skill_md)
            .await
            .context("Failed to read SKILL.md")?;

        let (frontmatter, _body) = parse_frontmatter(&content);
        let description = extract_description(&frontmatter);

        // Collect all .md files and non-.md reference files
        let (files, references) = self.collect_skill_files(&skill_dir).await?;

        Ok(Skill {
            name: name.to_string(),
            description,
            path: format!("{}/{}", SKILL_DIR, name),
            content,
            files,
            references,
        })
    }

    /// Collect all .md files and reference files from a skill directory.
    async fn collect_skill_files(&self, skill_dir: &Path) -> Result<(Vec<SkillFile>, Vec<String>)> {
        let mut md_files = Vec::new();
        let mut references = Vec::new();
        let mut visited = HashSet::new();

        self.collect_skill_files_recursive(
            skill_dir,
            skill_dir,
            &mut md_files,
            &mut references,
            &mut visited,
        )
        .await?;

        // Sort for consistent ordering
        md_files.sort_by(|a, b| a.name.cmp(&b.name));
        references.sort();

        Ok((md_files, references))
    }

    /// Recursively collect .md files and references.
    #[async_recursion::async_recursion]
    async fn collect_skill_files_recursive(
        &self,
        base_dir: &Path,
        current_dir: &Path,
        md_files: &mut Vec<SkillFile>,
        references: &mut Vec<String>,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        if !current_dir.exists() {
            return Ok(());
        }

        let canonical_path = match current_dir.canonicalize() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        if !visited.insert(canonical_path) {
            return Ok(());
        }

        let mut entries = fs::read_dir(current_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files
            if file_name.starts_with('.') {
                continue;
            }

            let metadata = match fs::symlink_metadata(&entry_path).await {
                Ok(m) => m,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                self.collect_skill_files_recursive(
                    base_dir,
                    &entry_path,
                    md_files,
                    references,
                    visited,
                )
                .await?;
            } else if metadata.is_file() {
                let relative_path = entry_path
                    .strip_prefix(base_dir)
                    .unwrap_or(&entry_path)
                    .to_string_lossy()
                    .to_string();

                if file_name.ends_with(".md") {
                    // Skip SKILL.md from the files list (it's in the content field)
                    if file_name != "SKILL.md" {
                        let file_content =
                            fs::read_to_string(&entry_path).await.unwrap_or_default();
                        md_files.push(SkillFile {
                            name: file_name,
                            path: relative_path,
                            content: file_content,
                        });
                    }
                } else {
                    // Non-.md files go to references
                    references.push(relative_path);
                }
            }
        }

        Ok(())
    }

    /// Save a skill's SKILL.md content.
    pub async fn save_skill(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;

        let skill_dir = self.skills_dir().join(name);
        let skill_md = skill_dir.join("SKILL.md");

        // Ensure directory exists
        fs::create_dir_all(&skill_dir).await?;

        fs::write(&skill_md, content)
            .await
            .context("Failed to write SKILL.md")?;

        Ok(())
    }

    /// Delete a skill and its directory.
    pub async fn delete_skill(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;

        let skill_dir = self.skills_dir().join(name);

        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir)
                .await
                .context("Failed to delete skill directory")?;
        }

        Ok(())
    }

    /// Validate that a name doesn't contain path traversal sequences.
    /// Names should be simple identifiers without directory separators.
    fn validate_name(name: &str) -> Result<()> {
        // Reject empty names
        if name.is_empty() {
            anyhow::bail!("Name cannot be empty");
        }

        // Reject path traversal sequences
        if name.contains("..") || name.contains('/') || name.contains('\\') {
            anyhow::bail!("Name contains invalid characters");
        }

        // Reject names that start with a dot (hidden files)
        if name.starts_with('.') {
            anyhow::bail!("Name cannot start with a dot");
        }

        Ok(())
    }

    /// Validate that a path doesn't escape the base directory via traversal.
    fn validate_path_within(&self, base: &std::path::Path, target: &std::path::Path) -> Result<()> {
        // Canonicalize what we can, but for non-existent paths we need to check components
        let base_canonical = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());

        // Check for path traversal in the target path components
        for component in target.components() {
            if let std::path::Component::ParentDir = component {
                anyhow::bail!("Path traversal not allowed");
            }
        }

        // If the file exists, verify it's within the base directory
        if target.exists() {
            let target_canonical = target.canonicalize()?;
            if !target_canonical.starts_with(&base_canonical) {
                anyhow::bail!("Path escapes allowed directory");
            }
        } else {
            // For new files, verify the parent directory exists and is within base
            // This prevents symlink bypass attacks where a symlinked parent could escape
            let mut current = target.to_path_buf();
            while let Some(parent) = current.parent() {
                if parent.exists() {
                    let parent_canonical = parent.canonicalize()?;
                    if !parent_canonical.starts_with(&base_canonical) {
                        anyhow::bail!("Path escapes allowed directory");
                    }
                    break;
                }
                current = parent.to_path_buf();
            }
        }

        Ok(())
    }

    /// Get a reference file from a skill.
    pub async fn get_skill_reference(&self, skill_name: &str, ref_path: &str) -> Result<String> {
        Self::validate_name(skill_name)?;
        let skill_dir = self.skills_dir().join(skill_name);
        let file_path = skill_dir.join(ref_path);

        // Validate path doesn't escape skill directory
        self.validate_path_within(&skill_dir, &file_path)?;

        if !file_path.exists() {
            anyhow::bail!("Reference file not found: {}/{}", skill_name, ref_path);
        }

        fs::read_to_string(&file_path)
            .await
            .context("Failed to read reference file")
    }

    /// Save a reference file for a skill.
    pub async fn save_skill_reference(
        &self,
        skill_name: &str,
        ref_path: &str,
        content: &str,
    ) -> Result<()> {
        Self::validate_name(skill_name)?;
        let skill_dir = self.skills_dir().join(skill_name);
        let file_path = skill_dir.join(ref_path);

        // Validate path doesn't escape skill directory
        self.validate_path_within(&skill_dir, &file_path)?;

        // Ensure parent directories exist
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&file_path, content)
            .await
            .context("Failed to write reference file")?;

        Ok(())
    }

    /// Delete a reference file from a skill.
    pub async fn delete_skill_reference(&self, skill_name: &str, ref_path: &str) -> Result<()> {
        Self::validate_name(skill_name)?;
        let skill_dir = self.skills_dir().join(skill_name);
        let file_path = skill_dir.join(ref_path);

        // Validate path doesn't escape skill directory
        self.validate_path_within(&skill_dir, &file_path)?;

        // Don't allow deleting SKILL.md via this method
        if ref_path == "SKILL.md" || ref_path.ends_with("/SKILL.md") {
            anyhow::bail!("Cannot delete SKILL.md via reference API - use delete_skill instead");
        }

        if !file_path.exists() {
            anyhow::bail!("Reference file not found: {}/{}", skill_name, ref_path);
        }

        // Check if it's a directory
        let metadata = fs::metadata(&file_path).await?;
        if metadata.is_dir() {
            fs::remove_dir_all(&file_path)
                .await
                .context("Failed to delete directory")?;
        } else {
            fs::remove_file(&file_path)
                .await
                .context("Failed to delete reference file")?;
        }

        Ok(())
    }

    /// Import a skill from a Git repository URL.
    /// Clones the specified path from the repo into the skills directory.
    pub async fn import_skill_from_git(
        &self,
        git_url: &str,
        skill_path: Option<&str>,
        target_name: &str,
    ) -> Result<Skill> {
        Self::validate_name(target_name)?;

        // Use new path for imports
        let skills_dir = self.path.join(SKILL_DIR);
        let target_dir = skills_dir.join(target_name);

        if target_dir.exists() {
            anyhow::bail!("Skill '{}' already exists", target_name);
        }

        // Ensure skills directory exists
        fs::create_dir_all(&skills_dir).await?;

        // Create a temp directory for cloning
        let temp_dir = self.path.join(".tmp-import");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).await?;
        }

        // Clone the repository (sparse checkout if path specified)
        let clone_result = if let Some(path) = skill_path {
            // For paths like "owner/repo/path/to/skill", we need to handle GitHub URLs
            git::sparse_clone(&temp_dir, git_url, path).await
        } else {
            git::clone(&temp_dir, git_url).await
        };

        if let Err(e) = clone_result {
            // Clean up temp dir on failure
            let _ = fs::remove_dir_all(&temp_dir).await;
            return Err(e);
        }

        // Find the SKILL.md file
        let source_dir = if let Some(path) = skill_path {
            let joined = temp_dir.join(path);
            // Validate path doesn't escape temp_dir via traversal
            let canonical_temp = temp_dir.canonicalize()?;
            let canonical_source = joined
                .canonicalize()
                .map_err(|_| anyhow::anyhow!("Skill path '{}' not found in repository", path))?;
            if !canonical_source.starts_with(&canonical_temp) {
                let _ = fs::remove_dir_all(&temp_dir).await;
                anyhow::bail!("Invalid skill path: path traversal detected");
            }
            joined
        } else {
            temp_dir.clone()
        };

        let skill_md = source_dir.join("SKILL.md");
        if !skill_md.exists() {
            let _ = fs::remove_dir_all(&temp_dir).await;
            anyhow::bail!("No SKILL.md found at the specified path");
        }

        // Copy the skill directory to target
        if let Err(e) = Self::copy_dir_recursive(&source_dir, &target_dir).await {
            let _ = fs::remove_dir_all(&temp_dir).await;
            return Err(e);
        }

        // Clean up temp directory
        let _ = fs::remove_dir_all(&temp_dir).await;

        // Return the imported skill
        self.get_skill(target_name).await
    }

    /// Recursively copy a directory.
    #[async_recursion::async_recursion]
    async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        fs::create_dir_all(dst).await?;

        let mut entries = fs::read_dir(src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst.join(&file_name);

            // Skip .git directory
            if file_name == ".git" {
                continue;
            }

            let metadata = fs::metadata(&entry_path).await?;
            if metadata.is_dir() {
                Self::copy_dir_recursive(&entry_path, &dst_path).await?;
            } else {
                fs::copy(&entry_path, &dst_path).await?;
            }
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Commands (command/*.md)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get the commands directory path.
    fn commands_dir(&self) -> PathBuf {
        self.path.join(COMMAND_DIR)
    }

    /// List all commands with their summaries.
    pub async fn list_commands(&self) -> Result<Vec<CommandSummary>> {
        let commands_dir = self.commands_dir();

        if !commands_dir.exists() {
            return Ok(Vec::new());
        }

        let mut commands = Vec::new();
        let mut entries = fs::read_dir(&commands_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process .md files
            let Some(ext) = entry_path.extension() else {
                continue;
            };
            if ext != "md" {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".md").to_string();

            // Read and parse frontmatter for description
            let content = fs::read_to_string(&entry_path).await.ok();
            let (frontmatter, _) = content
                .as_ref()
                .map(|c| parse_frontmatter(c))
                .unwrap_or((None, ""));

            let description = extract_description(&frontmatter);

            commands.push(CommandSummary {
                name,
                description,
                path: format!("{}/{}", COMMAND_DIR, file_name),
            });
        }

        // Sort by name
        commands.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(commands)
    }

    /// Get a command by name with full content.
    pub async fn get_command(&self, name: &str) -> Result<Command> {
        Self::validate_name(name)?;
        let command_path = self.commands_dir().join(format!("{}.md", name));

        if !command_path.exists() {
            anyhow::bail!("Command not found: {}", name);
        }

        let content = fs::read_to_string(&command_path)
            .await
            .context("Failed to read command file")?;

        let (frontmatter, _body) = parse_frontmatter(&content);
        let description = extract_description(&frontmatter);

        Ok(Command {
            name: name.to_string(),
            description,
            path: format!("{}/{}.md", COMMAND_DIR, name),
            content,
        })
    }

    /// Save a command's content.
    pub async fn save_command(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;
        let commands_dir = self.commands_dir();
        let command_path = commands_dir.join(format!("{}.md", name));

        // Ensure directory exists
        fs::create_dir_all(&commands_dir).await?;

        fs::write(&command_path, content)
            .await
            .context("Failed to write command file")?;

        Ok(())
    }

    /// Delete a command.
    pub async fn delete_command(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;

        let command_path = self.commands_dir().join(format!("{}.md", name));

        if command_path.exists() {
            fs::remove_file(&command_path)
                .await
                .context("Failed to delete command file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Plugins (plugins.json)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get all plugins from plugins.json.
    pub async fn get_plugins(&self) -> Result<HashMap<String, Plugin>> {
        let path = self.path.join(PLUGINS_FILE);

        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read plugins.json")?;

        // Be lenient with parse errors - log warning and return empty
        match serde_json::from_str::<HashMap<String, Plugin>>(&content) {
            Ok(plugins) => Ok(plugins),
            Err(e) => {
                tracing::warn!("Failed to parse plugins.json, returning empty map: {}", e);
                Ok(HashMap::new())
            }
        }
    }

    /// Save all plugins to plugins.json.
    pub async fn save_plugins(&self, plugins: &HashMap<String, Plugin>) -> Result<()> {
        let path = self.path.join(PLUGINS_FILE);

        let content = serde_json::to_string_pretty(plugins)?;
        fs::write(&path, content)
            .await
            .context("Failed to write plugins.json")?;

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rules (rule/*.md)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all rules with their summaries.
    pub async fn list_rules(&self) -> Result<Vec<RuleSummary>> {
        let rules_dir = self.path.join(RULE_DIR);

        if !rules_dir.exists() {
            return Ok(Vec::new());
        }

        let mut rules = Vec::new();
        let mut entries = fs::read_dir(&rules_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process .md files
            let Some(ext) = entry_path.extension() else {
                continue;
            };
            if ext != "md" {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".md").to_string();

            // Read and parse frontmatter for description
            let content = fs::read_to_string(&entry_path).await.ok();
            let (frontmatter, _) = content
                .as_ref()
                .map(|c| parse_frontmatter(c))
                .unwrap_or((None, ""));

            let description = extract_description(&frontmatter);

            rules.push(RuleSummary {
                name,
                description,
                path: format!("{}/{}", RULE_DIR, file_name),
            });
        }

        rules.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rules)
    }

    /// Get a rule by name with full content.
    pub async fn get_rule(&self, name: &str) -> Result<Rule> {
        Self::validate_name(name)?;
        let rule_path = self.path.join(RULE_DIR).join(format!("{}.md", name));

        if !rule_path.exists() {
            anyhow::bail!("Rule not found: {}", name);
        }

        let content = fs::read_to_string(&rule_path)
            .await
            .context("Failed to read rule file")?;

        let (frontmatter, _body) = parse_frontmatter(&content);
        let description = extract_description(&frontmatter);

        Ok(Rule {
            name: name.to_string(),
            description,
            path: format!("{}/{}.md", RULE_DIR, name),
            content,
        })
    }

    /// Save a rule's content.
    pub async fn save_rule(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;
        let rules_dir = self.path.join(RULE_DIR);
        let rule_path = rules_dir.join(format!("{}.md", name));

        fs::create_dir_all(&rules_dir).await?;

        fs::write(&rule_path, content)
            .await
            .context("Failed to write rule file")?;

        Ok(())
    }

    /// Delete a rule.
    pub async fn delete_rule(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        let rule_path = self.path.join(RULE_DIR).join(format!("{}.md", name));

        if rule_path.exists() {
            fs::remove_file(&rule_path)
                .await
                .context("Failed to delete rule file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Library Agents (agent/*.md)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all library agents with their summaries.
    pub async fn list_library_agents(&self) -> Result<Vec<LibraryAgentSummary>> {
        let agents_dir = self.path.join(AGENT_DIR);

        if !agents_dir.exists() {
            return Ok(Vec::new());
        }

        let mut agents = Vec::new();
        let mut entries = fs::read_dir(&agents_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process .md files
            let Some(ext) = entry_path.extension() else {
                continue;
            };
            if ext != "md" {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".md").to_string();

            // Read and parse frontmatter for description
            let content = fs::read_to_string(&entry_path).await.ok();
            let (frontmatter, _) = content
                .as_ref()
                .map(|c| parse_frontmatter(c))
                .unwrap_or((None, ""));

            let description = extract_description(&frontmatter);

            agents.push(LibraryAgentSummary {
                name,
                description,
                path: format!("{}/{}", AGENT_DIR, file_name),
            });
        }

        agents.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(agents)
    }

    /// Get a library agent by name with full content and parsed metadata.
    pub async fn get_library_agent(&self, name: &str) -> Result<LibraryAgent> {
        Self::validate_name(name)?;
        let agent_path = self.path.join(AGENT_DIR).join(format!("{}.md", name));

        if !agent_path.exists() {
            anyhow::bail!("Library agent not found: {}", name);
        }

        let content = fs::read_to_string(&agent_path)
            .await
            .context("Failed to read agent file")?;

        let (frontmatter, _body) = parse_frontmatter(&content);
        let description = extract_description(&frontmatter);
        let model = extract_model(&frontmatter);
        let tools = extract_tools(&frontmatter);
        let permissions = extract_permissions(&frontmatter);
        let rules = extract_string_array(&frontmatter, "rules");

        Ok(LibraryAgent {
            name: name.to_string(),
            description,
            path: format!("{}/{}.md", AGENT_DIR, name),
            content,
            model,
            tools,
            permissions,
            rules,
        })
    }

    /// Save a library agent definition.
    pub async fn save_library_agent(&self, name: &str, agent: &LibraryAgent) -> Result<()> {
        Self::validate_name(name)?;
        let agents_dir = self.path.join(AGENT_DIR);
        let agent_path = agents_dir.join(format!("{}.md", name));

        fs::create_dir_all(&agents_dir).await?;

        // Write the full content (should include frontmatter)
        fs::write(&agent_path, &agent.content)
            .await
            .context("Failed to write agent file")?;

        Ok(())
    }

    /// Delete a library agent.
    pub async fn delete_library_agent(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        let agent_path = self.path.join(AGENT_DIR).join(format!("{}.md", name));

        if agent_path.exists() {
            fs::remove_file(&agent_path)
                .await
                .context("Failed to delete agent file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Library Tools (tool/*.ts)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all library tools with their summaries.
    pub async fn list_library_tools(&self) -> Result<Vec<LibraryToolSummary>> {
        let tools_dir = self.path.join(TOOL_DIR);

        if !tools_dir.exists() {
            return Ok(Vec::new());
        }

        let mut tools = Vec::new();
        let mut entries = fs::read_dir(&tools_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process .ts files
            let Some(ext) = entry_path.extension() else {
                continue;
            };
            if ext != "ts" {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".ts").to_string();

            // Try to extract description from first comment block
            let content = fs::read_to_string(&entry_path).await.ok();
            let description = content.as_ref().and_then(|c| {
                // Look for /** ... */ or // description pattern
                if let Some(start) = c.find("/**") {
                    if let Some(end) = c[start..].find("*/") {
                        let comment = &c[start + 3..start + end];
                        let desc = comment
                            .lines()
                            .map(|l| l.trim().trim_start_matches('*').trim())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !desc.is_empty() {
                            return Some(desc);
                        }
                    }
                }
                None
            });

            tools.push(LibraryToolSummary {
                name,
                description,
                path: format!("{}/{}", TOOL_DIR, file_name),
            });
        }

        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tools)
    }

    /// Get a library tool by name with full content.
    pub async fn get_library_tool(&self, name: &str) -> Result<LibraryTool> {
        Self::validate_name(name)?;
        let tool_path = self.path.join(TOOL_DIR).join(format!("{}.ts", name));

        if !tool_path.exists() {
            anyhow::bail!("Library tool not found: {}", name);
        }

        let content = fs::read_to_string(&tool_path)
            .await
            .context("Failed to read tool file")?;

        // Extract description from first comment block
        let description = if let Some(start) = content.find("/**") {
            if let Some(end) = content[start..].find("*/") {
                let comment = &content[start + 3..start + end];
                let desc = comment
                    .lines()
                    .map(|l| l.trim().trim_start_matches('*').trim())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !desc.is_empty() {
                    Some(desc)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(LibraryTool {
            name: name.to_string(),
            description,
            path: format!("{}/{}.ts", TOOL_DIR, name),
            content,
        })
    }

    /// Save a library tool's content.
    pub async fn save_library_tool(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;
        let tools_dir = self.path.join(TOOL_DIR);
        let tool_path = tools_dir.join(format!("{}.ts", name));

        fs::create_dir_all(&tools_dir).await?;

        fs::write(&tool_path, content)
            .await
            .context("Failed to write tool file")?;

        Ok(())
    }

    /// Delete a library tool.
    pub async fn delete_library_tool(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        let tool_path = self.path.join(TOOL_DIR).join(format!("{}.ts", name));

        if tool_path.exists() {
            fs::remove_file(&tool_path)
                .await
                .context("Failed to delete tool file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Workspace Templates (workspace-template/*.json)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all workspace templates with their summaries.
    pub async fn list_workspace_templates(&self) -> Result<Vec<WorkspaceTemplateSummary>> {
        let templates_dir = self.path.join(WORKSPACE_TEMPLATE_DIR);

        if !templates_dir.exists() {
            return Ok(Vec::new());
        }

        let mut templates = Vec::new();
        let mut entries = fs::read_dir(&templates_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process .json files
            let Some(ext) = entry_path.extension() else {
                continue;
            };
            if ext != "json" {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".json").to_string();

            let content = fs::read_to_string(&entry_path).await.ok();
            let config = content
                .as_ref()
                .and_then(|c| serde_json::from_str::<WorkspaceTemplateConfig>(c).ok());

            let description = config.as_ref().and_then(|c| c.description.clone());
            let distro = config.as_ref().and_then(|c| c.distro.clone());
            let skills = config
                .as_ref()
                .map(|c| c.skills.clone())
                .unwrap_or_default();
            let template_name = config
                .as_ref()
                .and_then(|c| c.name.clone())
                .unwrap_or_else(|| name.clone());

            templates.push(WorkspaceTemplateSummary {
                name: template_name,
                description,
                distro,
                skills,
                path: format!("{}/{}", WORKSPACE_TEMPLATE_DIR, file_name),
            });
        }

        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    /// Get a workspace template by name with full content.
    pub async fn get_workspace_template(&self, name: &str) -> Result<WorkspaceTemplate> {
        Self::validate_name(name)?;
        let template_path = self
            .path
            .join(WORKSPACE_TEMPLATE_DIR)
            .join(format!("{}.json", name));

        if !template_path.exists() {
            anyhow::bail!("Workspace template not found: {}", name);
        }

        let content = fs::read_to_string(&template_path)
            .await
            .context("Failed to read workspace template file")?;

        let config: WorkspaceTemplateConfig =
            serde_json::from_str(&content).context("Failed to parse workspace template file")?;

        Ok(WorkspaceTemplate {
            name: config.name.unwrap_or_else(|| name.to_string()),
            description: config.description,
            path: format!("{}/{}.json", WORKSPACE_TEMPLATE_DIR, name),
            distro: config.distro,
            skills: config.skills,
            env_vars: config.env_vars,
            init_script: config.init_script,
        })
    }

    /// Save a workspace template.
    pub async fn save_workspace_template(
        &self,
        name: &str,
        template: &WorkspaceTemplate,
    ) -> Result<()> {
        Self::validate_name(name)?;
        let templates_dir = self.path.join(WORKSPACE_TEMPLATE_DIR);
        let template_path = templates_dir.join(format!("{}.json", name));

        fs::create_dir_all(&templates_dir).await?;

        let config = WorkspaceTemplateConfig {
            name: Some(name.to_string()),
            description: template.description.clone(),
            distro: template.distro.clone(),
            skills: template.skills.clone(),
            env_vars: template.env_vars.clone(),
            init_script: template.init_script.clone(),
        };

        let content = serde_json::to_string_pretty(&config)?;
        fs::write(&template_path, content)
            .await
            .context("Failed to write workspace template file")?;

        Ok(())
    }

    /// Delete a workspace template.
    pub async fn delete_workspace_template(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        let template_path = self
            .path
            .join(WORKSPACE_TEMPLATE_DIR)
            .join(format!("{}.json", name));

        if template_path.exists() {
            fs::remove_file(&template_path)
                .await
                .context("Failed to delete workspace template file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // OpenCode Settings (opencode/oh-my-opencode.json)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get oh-my-opencode settings from the Library.
    /// If the Library file doesn't exist, attempts to read from the system location
    /// (~/.config/opencode/oh-my-opencode.json) and returns that.
    /// Returns an empty object if neither file exists.
    pub async fn get_opencode_settings(&self) -> Result<serde_json::Value> {
        let path = self.path.join(OPENCODE_DIR).join("oh-my-opencode.json");

        if path.exists() {
            let content = fs::read_to_string(&path)
                .await
                .context("Failed to read opencode/oh-my-opencode.json")?;
            return serde_json::from_str(&content).context("Failed to parse oh-my-opencode.json");
        }

        // Library file doesn't exist - try to read from system location
        let system_path = Self::resolve_system_opencode_path();
        if system_path.exists() {
            let content = fs::read_to_string(&system_path)
                .await
                .context("Failed to read system oh-my-opencode.json")?;
            return serde_json::from_str(&content)
                .context("Failed to parse system oh-my-opencode.json");
        }

        // Neither file exists
        Ok(serde_json::json!({}))
    }

    /// Resolve the system path to oh-my-opencode.json.
    fn resolve_system_opencode_path() -> std::path::PathBuf {
        if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
            if !dir.trim().is_empty() {
                return std::path::PathBuf::from(dir).join("oh-my-opencode.json");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        std::path::PathBuf::from(home)
            .join(".config")
            .join("opencode")
            .join("oh-my-opencode.json")
    }

    /// Save oh-my-opencode settings to the Library.
    pub async fn save_opencode_settings(&self, settings: &serde_json::Value) -> Result<()> {
        let opencode_dir = self.path.join(OPENCODE_DIR);
        let path = opencode_dir.join("oh-my-opencode.json");

        // Ensure directory exists
        fs::create_dir_all(&opencode_dir).await?;

        let content = serde_json::to_string_pretty(settings)?;
        fs::write(&path, content)
            .await
            .context("Failed to write opencode/oh-my-opencode.json")?;

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // OpenAgent Config (openagent/config.json)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get OpenAgent configuration from the Library.
    /// Returns default config if the file doesn't exist.
    pub async fn get_openagent_config(&self) -> Result<OpenAgentConfig> {
        let path = self.path.join(OPENAGENT_DIR).join("config.json");

        if !path.exists() {
            return Ok(OpenAgentConfig::default());
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read openagent/config.json")?;

        serde_json::from_str(&content).context("Failed to parse openagent/config.json")
    }

    /// Save OpenAgent configuration to the Library.
    pub async fn save_openagent_config(&self, config: &OpenAgentConfig) -> Result<()> {
        let openagent_dir = self.path.join(OPENAGENT_DIR);
        let path = openagent_dir.join("config.json");

        // Ensure directory exists
        fs::create_dir_all(&openagent_dir).await?;

        let content = serde_json::to_string_pretty(config)?;
        fs::write(&path, content)
            .await
            .context("Failed to write openagent/config.json")?;

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Migration
    // ─────────────────────────────────────────────────────────────────────────

    /// Ensure library directory structure exists.
    pub async fn migrate_structure(&self) -> Result<MigrationReport> {
        let mut report = MigrationReport::default();

        // Ensure directories exist
        let _ = fs::create_dir_all(self.path.join(SKILL_DIR)).await;
        let _ = fs::create_dir_all(self.path.join(COMMAND_DIR)).await;
        let _ = fs::create_dir_all(self.path.join(AGENT_DIR)).await;
        let _ = fs::create_dir_all(self.path.join(TOOL_DIR)).await;
        let _ = fs::create_dir_all(self.path.join(RULE_DIR)).await;

        report.success = true;
        Ok(report)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// List reference files in a skill directory (excluding SKILL.md).
    #[allow(dead_code)]
    async fn list_references(&self, skill_dir: &Path) -> Result<Vec<String>> {
        let mut references = Vec::new();
        let mut visited = HashSet::new();

        // Recursively walk the directory
        self.collect_references(skill_dir, skill_dir, &mut references, &mut visited)
            .await?;

        references.sort();
        Ok(references)
    }

    /// Recursively collect reference file paths.
    /// Uses a visited set to prevent symlink loops from causing infinite recursion.
    #[async_recursion::async_recursion]
    async fn collect_references(
        &self,
        base_dir: &Path,
        current_dir: &Path,
        references: &mut Vec<String>,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        if !current_dir.exists() {
            return Ok(());
        }

        // Canonicalize to get the real path, detecting symlinks
        let canonical_path = match current_dir.canonicalize() {
            Ok(p) => p,
            Err(_) => return Ok(()), // Skip if we can't resolve the path
        };

        // Skip if we've already visited this directory (symlink loop detection)
        if !visited.insert(canonical_path) {
            return Ok(());
        }

        let mut entries = fs::read_dir(current_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip SKILL.md and hidden files
            if file_name == "SKILL.md" || file_name.starts_with('.') {
                continue;
            }

            // Use symlink_metadata to check file type without following symlinks
            let metadata = match fs::symlink_metadata(&entry_path).await {
                Ok(m) => m,
                Err(_) => continue, // Skip if we can't get metadata
            };

            if metadata.is_dir() {
                // Recurse into subdirectories (will detect loops via visited set)
                self.collect_references(base_dir, &entry_path, references, visited)
                    .await?;
            } else if metadata.is_file() {
                // Only add regular files (not symlinks)
                let relative_path = entry_path
                    .strip_prefix(base_dir)
                    .unwrap_or(&entry_path)
                    .to_string_lossy()
                    .to_string();
                references.push(relative_path);
            }
            // Skip symlinks to files to prevent symlink attacks
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
---

# Test Skill

This is the body."#;

        let (frontmatter, body) = parse_frontmatter(content);

        assert!(frontmatter.is_some());
        let fm = frontmatter.unwrap();
        assert_eq!(fm.get("name").unwrap().as_str().unwrap(), "test-skill");
        assert_eq!(
            fm.get("description").unwrap().as_str().unwrap(),
            "A test skill"
        );
        assert!(body.contains("# Test Skill"));
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nSome content.";

        let (frontmatter, body) = parse_frontmatter(content);

        assert!(frontmatter.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_validate_name_valid() {
        assert!(LibraryStore::validate_name("my-skill").is_ok());
        assert!(LibraryStore::validate_name("skill_name").is_ok());
        assert!(LibraryStore::validate_name("skill123").is_ok());
    }

    #[test]
    fn test_validate_name_rejects_path_traversal() {
        assert!(LibraryStore::validate_name("..").is_err());
        assert!(LibraryStore::validate_name("../etc").is_err());
        assert!(LibraryStore::validate_name("skill/../etc").is_err());
        assert!(LibraryStore::validate_name("skill/subdir").is_err());
        assert!(LibraryStore::validate_name("skill\\subdir").is_err());
    }

    #[test]
    fn test_validate_name_rejects_hidden() {
        assert!(LibraryStore::validate_name(".hidden").is_err());
        assert!(LibraryStore::validate_name(".").is_err());
    }

    #[test]
    fn test_validate_name_rejects_empty() {
        assert!(LibraryStore::validate_name("").is_err());
    }
}
