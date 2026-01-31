//! Configuration library management.
//!
//! This module manages a git-based configuration library containing:
//! - MCP server definitions (`mcp/servers.json`)
//! - Skills (`skill/*/SKILL.md` with additional .md files and references)
//! - Commands/prompts (`command/*.md`)
//! - Plugins registry (`plugins.json`)
//! - Library agents (`agent/*.md`)
//! - Library tools (`tool/*.ts`)
//! - Config profiles (`configs/<profile>/`) with harness-specific settings:
//!   - `.opencode/` - OpenCode settings (settings.json, oh-my-opencode.json)
//!   - `.claudecode/` - Claude Code settings (settings.json)
//!   - `.ampcode/` - Amp settings (settings.json)
//!   - `.sandboxed-sh/` - Sandboxed config (config.json)

pub mod env_crypto;
mod git;
pub mod rename;
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
    /// Keys of env vars that should be encrypted at rest (stored alongside encrypted values)
    #[serde(default)]
    encrypted_keys: Vec<String>,
    /// Init script fragment names to include (executed in order)
    #[serde(default)]
    init_scripts: Vec<String>,
    /// Custom init script to run on build (appended after fragments)
    #[serde(default)]
    init_script: String,
    /// Whether to share the host network (default: true).
    #[serde(default)]
    shared_network: Option<bool>,
    /// Tailscale networking mode (only relevant when shared_network is false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tailscale_mode: Option<crate::workspace::TailscaleMode>,
    /// MCP server names to enable for workspaces created from this template.
    #[serde(default)]
    mcps: Vec<String>,
    /// Config profile to use for workspaces created from this template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_profile: Option<String>,
}

// Directory constants (OpenCode-aligned structure)
const SKILL_DIR: &str = "skill";
const COMMAND_DIR: &str = "command";
const AGENT_DIR: &str = "agent";
const INIT_SCRIPT_DIR: &str = "init-script";
const PLUGINS_FILE: &str = "plugins.json";
const WORKSPACE_TEMPLATE_DIR: &str = "workspace-template";
const CONFIGS_DIR: &str = "configs";
const DEFAULT_PROFILE: &str = "default";

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
    /// After pulling, encrypts any unversioned <encrypted> tags in skill files.
    ///
    /// Returns `Err` with a specific error if the pull fails due to diverged history
    /// (e.g., after a force push on the remote). In this case, use `force_sync` to
    /// reset the local branch to match remote.
    pub async fn sync(&self) -> Result<()> {
        match git::pull(&self.path).await {
            Ok(()) => {}
            Err(git::PullError::DivergedHistory { message }) => {
                // Return a specific error that the API layer can detect
                anyhow::bail!("DIVERGED_HISTORY: {}", message);
            }
            Err(git::PullError::Other(e)) => {
                return Err(e);
            }
        }

        // Encrypt any unversioned encrypted tags in all skills
        self.encrypt_all_skill_files().await?;

        Ok(())
    }

    /// Force sync: reset local branch to match remote, discarding local changes.
    /// Use this after a force push on the remote has caused history to diverge.
    pub async fn force_sync(&self) -> Result<()> {
        git::force_pull(&self.path).await?;

        // Encrypt any unversioned encrypted tags in all skills
        self.encrypt_all_skill_files().await?;

        Ok(())
    }

    /// Force push: overwrite remote with local changes.
    /// Use this when you want to keep local changes and discard remote history.
    pub async fn force_push(&self) -> Result<()> {
        git::force_push(&self.path).await
    }

    /// Encrypt unversioned <encrypted> tags in all skill files.
    /// This ensures secrets pulled from git are encrypted on disk.
    pub async fn encrypt_all_skill_files(&self) -> Result<()> {
        let skills_dir = self.skills_dir();
        if !skills_dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(&skills_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let name = entry.file_name();
                if let Some(name_str) = name.to_str() {
                    // Skip hidden directories
                    if name_str.starts_with('.') {
                        continue;
                    }
                    if let Err(e) = self.encrypt_skill_file(name_str).await {
                        tracing::warn!(
                            skill = %name_str,
                            error = %e,
                            "Failed to encrypt skill file"
                        );
                    }
                }
            }
        }

        Ok(())
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
            let setup_commands = extract_string_array(&frontmatter, "setup_commands");

            // Read skill source metadata if present
            let source_file = entry_path.join(".skill-source.json");
            let source = if source_file.exists() {
                fs::read_to_string(&source_file)
                    .await
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default()
            } else {
                SkillSource::default()
            };

            skills.push(SkillSummary {
                name,
                description,
                path: format!("{}/{}", SKILL_DIR, entry.file_name().to_string_lossy()),
                source,
                setup_commands,
            });
        }

        // Sort by name
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(skills)
    }

    /// Get a skill by name with full content.
    /// Encrypted values in <encrypted v="N">...</encrypted> tags are decrypted
    /// to <encrypted>...</encrypted> format for display/editing.
    pub async fn get_skill(&self, name: &str) -> Result<Skill> {
        Self::validate_name(name)?;
        let skill_dir = self.skills_dir().join(name);
        let skill_md = skill_dir.join("SKILL.md");

        if !skill_md.exists() {
            anyhow::bail!("Skill not found: {}", name);
        }

        let raw_content = fs::read_to_string(&skill_md)
            .await
            .context("Failed to read SKILL.md")?;

        // Decrypt any encrypted tags for display
        let content = if let Some(key) = env_crypto::load_private_key_from_env()? {
            env_crypto::decrypt_content_tags(&key, &raw_content)?
        } else {
            raw_content
        };

        let (frontmatter, _body) = parse_frontmatter(&content);
        let description = extract_description(&frontmatter);

        // Collect all .md files and non-.md reference files
        let (files, references) = self.collect_skill_files(&skill_dir).await?;

        // Read skill source metadata if present
        let source_file = skill_dir.join(".skill-source.json");
        let source = if source_file.exists() {
            fs::read_to_string(&source_file)
                .await
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            SkillSource::default()
        };

        // Extract setup_commands from frontmatter
        let setup_commands = extract_string_array(&frontmatter, "setup_commands");

        Ok(Skill {
            name: name.to_string(),
            description,
            path: format!("{}/{}", SKILL_DIR, name),
            source,
            content,
            files,
            references,
            setup_commands,
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
                        let raw_content = fs::read_to_string(&entry_path).await.unwrap_or_default();
                        // Decrypt any encrypted tags for display
                        let file_content =
                            if let Ok(Some(key)) = env_crypto::load_private_key_from_env() {
                                env_crypto::decrypt_content_tags(&key, &raw_content)
                                    .unwrap_or(raw_content)
                            } else {
                                raw_content
                            };
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

    /// Save a skill, encrypting any <encrypted>...</encrypted> tags.
    /// Unversioned <encrypted>value</encrypted> tags are encrypted to
    /// <encrypted v="1">ciphertext</encrypted> format.
    pub async fn save_skill(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;

        let skill_dir = self.skills_dir().join(name);
        let skill_md = skill_dir.join("SKILL.md");

        tracing::debug!(
            skill = %name,
            path = %skill_md.display(),
            has_encrypted_tags = env_crypto::has_encrypted_tags(content),
            content_len = content.len(),
            "Saving skill"
        );

        // Ensure directory exists
        fs::create_dir_all(&skill_dir).await?;

        // Encrypt any unversioned encrypted tags (lazily generates key if needed)
        let key = env_crypto::ensure_private_key()
            .await
            .context("Failed to ensure encryption key for saving skill")?;

        tracing::debug!(skill = %name, "Encryption key loaded, encrypting content tags");

        let encrypted_content = env_crypto::encrypt_content_tags(&key, content)?;

        let content_changed = encrypted_content != content;
        tracing::info!(
            skill = %name,
            content_changed = content_changed,
            original_len = content.len(),
            encrypted_len = encrypted_content.len(),
            "Skill content encryption complete"
        );

        fs::write(&skill_md, &encrypted_content)
            .await
            .context("Failed to write SKILL.md")?;

        tracing::debug!(skill = %name, path = %skill_md.display(), "Skill saved successfully");

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
    /// For .md files, encrypted tags are decrypted for display.
    pub async fn get_skill_reference(&self, skill_name: &str, ref_path: &str) -> Result<String> {
        Self::validate_name(skill_name)?;
        let skill_dir = self.skills_dir().join(skill_name);
        let file_path = skill_dir.join(ref_path);

        // Validate path doesn't escape skill directory
        self.validate_path_within(&skill_dir, &file_path)?;

        if !file_path.exists() {
            anyhow::bail!("Reference file not found: {}/{}", skill_name, ref_path);
        }

        let raw_content = fs::read_to_string(&file_path)
            .await
            .context("Failed to read reference file")?;

        // Decrypt encrypted tags in .md files
        if ref_path.ends_with(".md") {
            if let Some(key) = env_crypto::load_private_key_from_env()? {
                return env_crypto::decrypt_content_tags(&key, &raw_content);
            }
        }

        Ok(raw_content)
    }

    /// Save a reference file for a skill.
    /// For .md files, encrypted tags are encrypted before saving.
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

        // Encrypt tags in .md files (lazily generates key if needed)
        let content_to_write = if ref_path.ends_with(".md") {
            let key = env_crypto::ensure_private_key()
                .await
                .context("Failed to ensure encryption key for saving reference")?;
            env_crypto::encrypt_content_tags(&key, content)?
        } else {
            content.to_string()
        };

        fs::write(&file_path, content_to_write)
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

        // Encrypt any unversioned <encrypted> tags in the imported SKILL.md
        self.encrypt_skill_file(target_name).await?;

        // Return the imported skill
        self.get_skill(target_name).await
    }

    /// Encrypt unversioned <encrypted> tags in a skill's SKILL.md file.
    /// This is called after importing or syncing to ensure secrets are encrypted on disk.
    async fn encrypt_skill_file(&self, name: &str) -> Result<()> {
        let skill_md = self.skills_dir().join(name).join("SKILL.md");
        if !skill_md.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&skill_md).await?;

        // Check if there are any unversioned encrypted tags that need encryption
        if !env_crypto::has_encrypted_tags(&content) {
            return Ok(());
        }

        // Only encrypt if there are unversioned tags (user input format)
        let has_unversioned = content.contains("<encrypted>")
            && !content
                .lines()
                .filter(|l| l.contains("<encrypted>"))
                .all(|l| l.contains("<encrypted v=\""));

        if !has_unversioned {
            return Ok(());
        }

        tracing::info!(
            skill = %name,
            "Encrypting unversioned <encrypted> tags in imported skill"
        );

        let key = env_crypto::ensure_private_key()
            .await
            .context("Failed to ensure encryption key")?;
        let encrypted_content = env_crypto::encrypt_content_tags(&key, &content)?;

        if encrypted_content != content {
            fs::write(&skill_md, &encrypted_content).await?;
            tracing::debug!(skill = %name, "Skill file encrypted and saved");
        }

        Ok(())
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
            let params = extract_params(&frontmatter);

            commands.push(CommandSummary {
                name,
                description,
                path: format!("{}/{}", COMMAND_DIR, file_name),
                params,
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
        let params = extract_params(&frontmatter);

        Ok(Command {
            name: name.to_string(),
            description,
            path: format!("{}/{}.md", COMMAND_DIR, name),
            content,
            params,
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

        Ok(LibraryAgent {
            name: name.to_string(),
            description,
            path: format!("{}/{}.md", AGENT_DIR, name),
            content,
            model,
            tools,
            permissions,
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
            let init_scripts = config
                .as_ref()
                .map(|c| c.init_scripts.clone())
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
                init_scripts,
                path: format!("{}/{}", WORKSPACE_TEMPLATE_DIR, file_name),
            });
        }

        templates.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(templates)
    }

    /// Get a workspace template by name with full content.
    /// Env vars are decrypted if a PRIVATE_KEY is configured; plaintext values pass through.
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

        // Decrypt env vars if we have a key configured (file or env var)
        let has_encrypted = config
            .env_vars
            .values()
            .any(|v| env_crypto::is_encrypted(v));

        let env_vars = if has_encrypted {
            // Try to load key from env var or file
            let key = env_crypto::ensure_private_key()
                .await
                .context("Failed to load encryption key for decrypting template env vars")?;
            env_crypto::decrypt_env_vars(&key, &config.env_vars)
                .context("Failed to decrypt template env vars")?
        } else {
            // No encrypted values, pass through as-is
            config.env_vars.clone()
        };

        // Determine encrypted_keys: use stored list if available, otherwise detect from values
        // (for backwards compatibility with old templates where all vars were encrypted)
        let encrypted_keys = if !config.encrypted_keys.is_empty() {
            config.encrypted_keys
        } else {
            // Legacy: detect which keys have encrypted values
            config
                .env_vars
                .iter()
                .filter(|(_, v)| env_crypto::is_encrypted(v))
                .map(|(k, _)| k.clone())
                .collect()
        };

        Ok(WorkspaceTemplate {
            name: config.name.unwrap_or_else(|| name.to_string()),
            description: config.description,
            path: format!("{}/{}.json", WORKSPACE_TEMPLATE_DIR, name),
            distro: config.distro,
            skills: config.skills,
            env_vars,
            encrypted_keys,
            init_scripts: config.init_scripts,
            init_script: config.init_script,
            shared_network: config.shared_network,
            tailscale_mode: config.tailscale_mode,
            mcps: config.mcps,
            config_profile: config.config_profile,
        })
    }

    /// Save a workspace template.
    /// Only env vars with keys in `encrypted_keys` are encrypted (if PRIVATE_KEY is configured).
    pub async fn save_workspace_template(
        &self,
        name: &str,
        template: &WorkspaceTemplate,
    ) -> Result<()> {
        Self::validate_name(name)?;
        let templates_dir = self.path.join(WORKSPACE_TEMPLATE_DIR);
        let template_path = templates_dir.join(format!("{}.json", name));

        fs::create_dir_all(&templates_dir).await?;

        // Selectively encrypt only keys in encrypted_keys (lazily generates key if needed)
        let encrypted_set: std::collections::HashSet<_> =
            template.encrypted_keys.iter().cloned().collect();
        let env_vars = if encrypted_set.is_empty() {
            template.env_vars.clone()
        } else {
            let key = env_crypto::ensure_private_key()
                .await
                .context("Failed to ensure encryption key for saving template")?;
            let mut result = HashMap::with_capacity(template.env_vars.len());
            for (k, v) in &template.env_vars {
                if encrypted_set.contains(k) {
                    result.insert(
                        k.clone(),
                        env_crypto::encrypt_value(&key, v).context("Failed to encrypt env var")?,
                    );
                } else {
                    result.insert(k.clone(), v.clone());
                }
            }
            result
        };

        let config = WorkspaceTemplateConfig {
            name: Some(name.to_string()),
            description: template.description.clone(),
            distro: template.distro.clone(),
            skills: template.skills.clone(),
            env_vars,
            encrypted_keys: template.encrypted_keys.clone(),
            init_scripts: template.init_scripts.clone(),
            init_script: template.init_script.clone(),
            shared_network: template.shared_network,
            tailscale_mode: template.tailscale_mode,
            mcps: template.mcps.clone(),
            config_profile: template.config_profile.clone(),
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
    // Init Script Fragments (init-script/*/SCRIPT.sh)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all init script fragments with their summaries.
    pub async fn list_init_scripts(&self) -> Result<Vec<InitScriptSummary>> {
        let init_scripts_dir = self.path.join(INIT_SCRIPT_DIR);

        if !init_scripts_dir.exists() {
            return Ok(Vec::new());
        }

        let mut scripts = Vec::new();
        let mut entries = fs::read_dir(&init_scripts_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process directories
            if !entry_path.is_dir() {
                continue;
            }

            let script_sh = entry_path.join("SCRIPT.sh");
            if !script_sh.exists() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();

            // Read and extract description from first comment line
            let content = fs::read_to_string(&script_sh).await.ok();
            let description = content
                .as_ref()
                .and_then(|c| Self::extract_script_description(c));

            scripts.push(InitScriptSummary {
                name,
                description,
                path: format!(
                    "{}/{}/SCRIPT.sh",
                    INIT_SCRIPT_DIR,
                    entry.file_name().to_string_lossy()
                ),
            });
        }

        // Sort by name
        scripts.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(scripts)
    }

    /// Get an init script fragment by name with full content.
    pub async fn get_init_script(&self, name: &str) -> Result<InitScript> {
        Self::validate_name(name)?;
        let script_dir = self.path.join(INIT_SCRIPT_DIR).join(name);
        let script_sh = script_dir.join("SCRIPT.sh");

        if !script_sh.exists() {
            anyhow::bail!("Init script not found: {}", name);
        }

        let content = fs::read_to_string(&script_sh)
            .await
            .context("Failed to read SCRIPT.sh")?;

        let description = Self::extract_script_description(&content);

        Ok(InitScript {
            name: name.to_string(),
            description,
            path: format!("{}/{}/SCRIPT.sh", INIT_SCRIPT_DIR, name),
            content,
        })
    }

    /// Save an init script fragment.
    pub async fn save_init_script(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;

        let script_dir = self.path.join(INIT_SCRIPT_DIR).join(name);
        let script_sh = script_dir.join("SCRIPT.sh");

        // Ensure directory exists
        fs::create_dir_all(&script_dir).await?;

        fs::write(&script_sh, content)
            .await
            .context("Failed to write SCRIPT.sh")?;

        Ok(())
    }

    /// Delete an init script fragment and its directory.
    pub async fn delete_init_script(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;

        let script_dir = self.path.join(INIT_SCRIPT_DIR).join(name);

        if script_dir.exists() {
            fs::remove_dir_all(&script_dir)
                .await
                .context("Failed to delete init script directory")?;
        }

        Ok(())
    }

    /// Assemble a combined init script from fragments, skill setup commands, and optional custom script.
    /// Each fragment is prefixed with a header comment for debugging.
    pub async fn assemble_init_script(
        &self,
        fragment_names: &[String],
        custom_script: Option<&str>,
        skill_setup_commands: Option<&[(String, Vec<String>)]>,
    ) -> Result<String> {
        let mut assembled = String::new();

        // Add shebang
        assembled.push_str("#!/usr/bin/env bash\n");
        assembled.push_str("# Auto-assembled init script from fragments\n\n");

        // Add each fragment
        for name in fragment_names {
            let script = self.get_init_script(name).await?;

            // Add header for this fragment
            assembled.push_str(&format!("\n# === {} ===\n", name));

            // Strip shebang from fragment content if present
            let content = if script.content.starts_with("#!") {
                // Skip the first line (shebang)
                script
                    .content
                    .lines()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                script.content.clone()
            };

            assembled.push_str(&content);
            assembled.push_str("\n");
        }

        // Add skill setup commands if provided
        if let Some(skills) = skill_setup_commands {
            let has_commands = skills.iter().any(|(_, cmds)| !cmds.is_empty());
            if has_commands {
                assembled.push_str("\n# === Skill Setup Commands ===\n");
                assembled.push_str("# (npm commands auto-substituted to use bun if available)\n");
                for (skill_name, commands) in skills {
                    if !commands.is_empty() {
                        assembled.push_str(&format!("# Skill: {}\n", skill_name));
                        for cmd in commands {
                            // Auto-substitute npm with bun for faster installs
                            let cmd = Self::substitute_npm_with_bun(cmd);
                            assembled.push_str(&cmd);
                            assembled.push_str("\n");
                        }
                    }
                }
            }
        }

        // Add custom script at the end if provided
        if let Some(custom) = custom_script {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                assembled.push_str("\n# === Custom Script ===\n");

                // Strip shebang from custom script if present
                let content = if trimmed.starts_with("#!") {
                    trimmed.lines().skip(1).collect::<Vec<_>>().join("\n")
                } else {
                    trimmed.to_string()
                };

                assembled.push_str(&content);
                assembled.push_str("\n");
            }
        }

        Ok(assembled)
    }

    /// Collect setup commands from skills by name.
    /// Returns a list of (skill_name, setup_commands) pairs.
    pub async fn collect_skill_setup_commands(
        &self,
        skill_names: &[String],
    ) -> Vec<(String, Vec<String>)> {
        let mut result = Vec::new();
        for name in skill_names {
            match self.get_skill(name).await {
                Ok(skill) => {
                    if !skill.setup_commands.is_empty() {
                        result.push((skill.name, skill.setup_commands));
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        skill = %name,
                        error = %e,
                        "Failed to load skill for setup commands"
                    );
                }
            }
        }
        result
    }

    /// Substitute npm commands with bun equivalents for faster package installation.
    /// Generates a shell command that uses bun if available, falling back to npm.
    fn substitute_npm_with_bun(cmd: &str) -> String {
        // Check if this is an npm install command
        let trimmed = cmd.trim();
        if trimmed.starts_with("npm install") || trimmed.starts_with("npm i ") {
            // Extract the rest of the command after "npm install" or "npm i"
            let rest = if trimmed.starts_with("npm install") {
                trimmed.strip_prefix("npm install").unwrap_or("")
            } else {
                trimmed.strip_prefix("npm i").unwrap_or("")
            };

            // Generate command that prefers bun but falls back to npm
            format!(
                "if command -v bun >/dev/null 2>&1; then bun install{}; else npm install{}; fi",
                rest, rest
            )
        } else {
            cmd.to_string()
        }
    }

    /// Extract description from the first comment line after shebang.
    /// Supports formats like:
    /// - `# Description: Base logging and error handling`
    /// - `# Base logging and error handling`
    fn extract_script_description(content: &str) -> Option<String> {
        for line in content.lines() {
            let trimmed = line.trim();

            // Skip shebang
            if trimmed.starts_with("#!") {
                continue;
            }

            // Skip empty lines
            if trimmed.is_empty() {
                continue;
            }

            // Found a comment line - extract description
            if trimmed.starts_with('#') {
                let comment = trimmed.trim_start_matches('#').trim();

                // Handle "Description: ..." format
                if let Some(desc) = comment.strip_prefix("Description:") {
                    return Some(desc.trim().to_string());
                }

                // Otherwise use the whole comment as description
                if !comment.is_empty() {
                    return Some(comment.to_string());
                }
            }

            // Non-comment, non-empty line - stop looking
            break;
        }

        None
    }

    // ─────────────────────────────────────────────────────────────────────────
    // OpenCode Settings (delegates to default profile)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get oh-my-opencode settings from the Library (default profile).
    /// Returns an empty object if the file doesn't exist.
    pub async fn get_opencode_settings(&self) -> Result<serde_json::Value> {
        self.get_opencode_settings_for_profile(DEFAULT_PROFILE)
            .await
    }

    /// Save oh-my-opencode settings to the Library (default profile).
    pub async fn save_opencode_settings(&self, settings: &serde_json::Value) -> Result<()> {
        self.save_opencode_settings_for_profile(DEFAULT_PROFILE, settings)
            .await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Sandboxed Config (delegates to default profile)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get Sandboxed configuration from the Library (default profile).
    /// Returns default config if the file doesn't exist.
    pub async fn get_sandboxed_config(&self) -> Result<SandboxedConfig> {
        self.get_sandboxed_config_for_profile(DEFAULT_PROFILE).await
    }

    /// Save Sandboxed configuration to the Library (default profile).
    pub async fn save_sandboxed_config(&self, config: &SandboxedConfig) -> Result<()> {
        self.save_sandboxed_config_for_profile(DEFAULT_PROFILE, config)
            .await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Claude Code Config (delegates to default profile)
    // ─────────────────────────────────────────────────────────────────────────

    /// Get Claude Code configuration from the Library (default profile).
    /// Returns default config if the file doesn't exist.
    pub async fn get_claudecode_config(&self) -> Result<ClaudeCodeConfig> {
        self.get_claudecode_config_for_profile(DEFAULT_PROFILE)
            .await
    }

    /// Save Claude Code configuration to the Library (default profile).
    pub async fn save_claudecode_config(&self, config: &ClaudeCodeConfig) -> Result<()> {
        self.save_claudecode_config_for_profile(DEFAULT_PROFILE, config)
            .await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Config Profiles (configs/{profile}/...)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all config profiles.
    pub async fn list_config_profiles(&self) -> Result<Vec<ConfigProfileSummary>> {
        let configs_dir = self.path.join(CONFIGS_DIR);

        if !configs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut profiles = Vec::new();
        let mut entries = fs::read_dir(&configs_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();

            // Only process directories
            if !entry_path.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden directories
            if name.starts_with('.') {
                continue;
            }

            profiles.push(ConfigProfileSummary {
                name: name.clone(),
                is_default: name == DEFAULT_PROFILE,
                path: format!("{}/{}", CONFIGS_DIR, name),
            });
        }

        // Sort by name, but put "default" first
        profiles.sort_by(|a, b| {
            if a.name == DEFAULT_PROFILE {
                std::cmp::Ordering::Less
            } else if b.name == DEFAULT_PROFILE {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        Ok(profiles)
    }

    /// Get a config profile by name with full content.
    /// Uses new directory structure: .opencode/, .claudecode/, .ampcode/, .sandboxed-sh/
    pub async fn get_config_profile(&self, name: &str) -> Result<ConfigProfile> {
        Self::validate_name(name)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(name);

        // New paths (dot-prefixed to mirror harness directories)
        let opencode_settings_path = profile_dir.join(".opencode").join("settings.json");
        let claudecode_settings_path = profile_dir.join(".claudecode").join("settings.json");
        let ampcode_settings_path = profile_dir.join(".ampcode").join("settings.json");
        let sandboxed_config_path = profile_dir.join(".sandboxed-sh").join("config.json");

        // Legacy paths for backward compatibility
        let legacy_opencode_path = profile_dir.join("opencode").join("oh-my-opencode.json");
        let legacy_sandboxed_path = profile_dir.join("sandboxed").join("config.json");
        let legacy_claudecode_path = profile_dir.join("claudecode").join("config.json");

        // Collect all files in the profile for file-based editing
        let mut files = Vec::new();

        // Load OpenCode settings (try new path first, then legacy)
        let opencode_settings = if opencode_settings_path.exists() {
            let content = fs::read_to_string(&opencode_settings_path)
                .await
                .context("Failed to read opencode settings")?;
            files.push(ConfigProfileFile {
                path: ".opencode/settings.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else if legacy_opencode_path.exists() {
            let content = fs::read_to_string(&legacy_opencode_path)
                .await
                .context("Failed to read opencode settings")?;
            files.push(ConfigProfileFile {
                path: ".opencode/settings.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            serde_json::json!({})
        };

        // Load Sandboxed config (try new path first, then legacy)
        let sandboxed_config = if sandboxed_config_path.exists() {
            let content = fs::read_to_string(&sandboxed_config_path)
                .await
                .context("Failed to read sandboxed config")?;
            files.push(ConfigProfileFile {
                path: ".sandboxed-sh/config.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else if legacy_sandboxed_path.exists() {
            let content = fs::read_to_string(&legacy_sandboxed_path)
                .await
                .context("Failed to read sandboxed config")?;
            files.push(ConfigProfileFile {
                path: ".sandboxed-sh/config.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            SandboxedConfig::default()
        };

        // Load Claude Code config (try new path first, then legacy)
        let claudecode_config = if claudecode_settings_path.exists() {
            let content = fs::read_to_string(&claudecode_settings_path)
                .await
                .context("Failed to read claudecode config")?;
            files.push(ConfigProfileFile {
                path: ".claudecode/settings.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else if legacy_claudecode_path.exists() {
            let content = fs::read_to_string(&legacy_claudecode_path)
                .await
                .context("Failed to read claudecode config")?;
            files.push(ConfigProfileFile {
                path: ".claudecode/settings.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ClaudeCodeConfig::default()
        };

        // Load Amp Code config (new only)
        let ampcode_config = if ampcode_settings_path.exists() {
            let content = fs::read_to_string(&ampcode_settings_path)
                .await
                .context("Failed to read ampcode config")?;
            files.push(ConfigProfileFile {
                path: ".ampcode/settings.json".to_string(),
                content: content.clone(),
            });
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            AmpCodeConfig::default()
        };

        Ok(ConfigProfile {
            name: name.to_string(),
            is_default: name == DEFAULT_PROFILE,
            path: format!("{}/{}", CONFIGS_DIR, name),
            files,
            opencode_settings,
            sandboxed_config,
            claudecode_config,
            ampcode_config,
        })
    }

    /// Save a config profile.
    /// Uses new directory structure: .opencode/, .claudecode/, .ampcode/, .sandboxed-sh/
    pub async fn save_config_profile(&self, name: &str, profile: &ConfigProfile) -> Result<()> {
        Self::validate_name(name)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(name);

        // Create profile directories with dot-prefix (mirroring harness directories)
        let opencode_dir = profile_dir.join(".opencode");
        let sandboxed_dir = profile_dir.join(".sandboxed-sh");
        let claudecode_dir = profile_dir.join(".claudecode");
        let ampcode_dir = profile_dir.join(".ampcode");

        fs::create_dir_all(&opencode_dir).await?;
        fs::create_dir_all(&sandboxed_dir).await?;
        fs::create_dir_all(&claudecode_dir).await?;
        fs::create_dir_all(&ampcode_dir).await?;

        // Save OpenCode settings
        let opencode_content = serde_json::to_string_pretty(&profile.opencode_settings)?;
        fs::write(opencode_dir.join("settings.json"), opencode_content)
            .await
            .context("Failed to write opencode settings")?;

        // Save Sandboxed config
        let sandboxed_content = serde_json::to_string_pretty(&profile.sandboxed_config)?;
        fs::write(sandboxed_dir.join("config.json"), sandboxed_content)
            .await
            .context("Failed to write sandboxed config")?;

        // Save Claude Code config
        let claudecode_content = serde_json::to_string_pretty(&profile.claudecode_config)?;
        fs::write(claudecode_dir.join("settings.json"), claudecode_content)
            .await
            .context("Failed to write claudecode config")?;

        // Save Amp Code config
        let ampcode_content = serde_json::to_string_pretty(&profile.ampcode_config)?;
        fs::write(ampcode_dir.join("settings.json"), ampcode_content)
            .await
            .context("Failed to write ampcode config")?;

        Ok(())
    }

    /// Delete a config profile.
    pub async fn delete_config_profile(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;

        // Prevent deleting the default profile
        if name == DEFAULT_PROFILE {
            anyhow::bail!("Cannot delete the default profile");
        }

        let profile_dir = self.path.join(CONFIGS_DIR).join(name);

        if profile_dir.exists() {
            fs::remove_dir_all(&profile_dir)
                .await
                .context("Failed to delete config profile")?;
        }

        Ok(())
    }

    /// Create a new config profile.
    /// If base_profile is provided, copies settings from that profile.
    /// Otherwise, creates an empty profile that falls back to library defaults.
    pub async fn create_config_profile(
        &self,
        name: &str,
        base_profile: Option<&str>,
    ) -> Result<ConfigProfile> {
        Self::validate_name(name)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(name);
        if profile_dir.exists() {
            anyhow::bail!("Profile '{}' already exists", name);
        }

        // If a base profile is provided, copy its settings
        // Otherwise, just create an empty directory (falls back to library defaults)
        if let Some(base_name) = base_profile {
            let base = self.get_config_profile(base_name).await?;
            let new_profile = ConfigProfile {
                name: name.to_string(),
                is_default: false,
                path: format!("{}/{}", CONFIGS_DIR, name),
                files: Vec::new(),
                opencode_settings: base.opencode_settings,
                sandboxed_config: base.sandboxed_config,
                claudecode_config: base.claudecode_config,
                ampcode_config: base.ampcode_config,
            };
            self.save_config_profile(name, &new_profile).await?;
            Ok(new_profile)
        } else {
            // Create empty profile directory (no files = uses library defaults)
            fs::create_dir_all(&profile_dir).await?;
            Ok(ConfigProfile {
                name: name.to_string(),
                is_default: false,
                path: format!("{}/{}", CONFIGS_DIR, name),
                files: Vec::new(),
                opencode_settings: serde_json::json!({}),
                sandboxed_config: SandboxedConfig::default(),
                claudecode_config: ClaudeCodeConfig::default(),
                ampcode_config: AmpCodeConfig::default(),
            })
        }
    }

    /// Get OpenCode settings from a specific profile.
    pub async fn get_opencode_settings_for_profile(
        &self,
        profile: &str,
    ) -> Result<serde_json::Value> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        // Try new path first, then legacy
        let new_path = profile_dir.join(".opencode").join("oh-my-opencode.json");
        let legacy_path = profile_dir.join("opencode").join("oh-my-opencode.json");

        tracing::debug!(
            profile = %profile,
            new_path = %new_path.display(),
            new_path_exists = new_path.exists(),
            legacy_path = %legacy_path.display(),
            legacy_path_exists = legacy_path.exists(),
            "Checking OpenCode settings paths"
        );

        let path = if new_path.exists() {
            new_path
        } else if legacy_path.exists() {
            legacy_path
        } else {
            tracing::debug!(profile = %profile, "No OpenCode settings found for profile");
            return Ok(serde_json::json!({}));
        };

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read opencode settings")?;

        serde_json::from_str(&content).context("Failed to parse opencode settings")
    }

    /// Save OpenCode settings to a specific profile.
    pub async fn save_opencode_settings_for_profile(
        &self,
        profile: &str,
        settings: &serde_json::Value,
    ) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let opencode_dir = profile_dir.join(".opencode");

        fs::create_dir_all(&opencode_dir).await?;

        let content = serde_json::to_string_pretty(settings)?;
        fs::write(opencode_dir.join("settings.json"), content)
            .await
            .context("Failed to write opencode settings")?;

        Ok(())
    }

    /// Get Sandboxed config from a specific profile.
    pub async fn get_sandboxed_config_for_profile(&self, profile: &str) -> Result<SandboxedConfig> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        // Try new path first, then legacy
        let new_path = profile_dir.join(".sandboxed-sh").join("config.json");
        let legacy_path = profile_dir.join("sandboxed").join("config.json");

        let path = if new_path.exists() {
            new_path
        } else if legacy_path.exists() {
            legacy_path
        } else {
            return Ok(SandboxedConfig::default());
        };

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read sandboxed config")?;

        serde_json::from_str(&content).context("Failed to parse sandboxed config")
    }

    /// Save Sandboxed config to a specific profile.
    pub async fn save_sandboxed_config_for_profile(
        &self,
        profile: &str,
        config: &SandboxedConfig,
    ) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let sandboxed_dir = profile_dir.join(".sandboxed-sh");

        fs::create_dir_all(&sandboxed_dir).await?;

        let content = serde_json::to_string_pretty(config)?;
        fs::write(sandboxed_dir.join("config.json"), content)
            .await
            .context("Failed to write sandboxed config")?;

        Ok(())
    }

    /// Get Claude Code config from a specific profile.
    pub async fn get_claudecode_config_for_profile(
        &self,
        profile: &str,
    ) -> Result<ClaudeCodeConfig> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        // Try new path first, then legacy
        let new_path = profile_dir.join(".claudecode").join("settings.json");
        let legacy_path = profile_dir.join("claudecode").join("config.json");

        let path = if new_path.exists() {
            new_path
        } else if legacy_path.exists() {
            legacy_path
        } else {
            return Ok(ClaudeCodeConfig::default());
        };

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read claudecode config")?;

        serde_json::from_str(&content).context("Failed to parse claudecode config")
    }

    /// Save Claude Code config to a specific profile.
    pub async fn save_claudecode_config_for_profile(
        &self,
        profile: &str,
        config: &ClaudeCodeConfig,
    ) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let claudecode_dir = profile_dir.join(".claudecode");

        fs::create_dir_all(&claudecode_dir).await?;

        let content = serde_json::to_string_pretty(config)?;
        fs::write(claudecode_dir.join("settings.json"), content)
            .await
            .context("Failed to write claudecode config")?;

        Ok(())
    }

    /// Get Amp Code config from a specific profile.
    pub async fn get_ampcode_config_for_profile(&self, profile: &str) -> Result<AmpCodeConfig> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let path = profile_dir.join(".ampcode").join("settings.json");

        if !path.exists() {
            return Ok(AmpCodeConfig::default());
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read ampcode config")?;

        serde_json::from_str(&content).context("Failed to parse ampcode config")
    }

    /// Save Amp Code config to a specific profile.
    pub async fn save_ampcode_config_for_profile(
        &self,
        profile: &str,
        config: &AmpCodeConfig,
    ) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let ampcode_dir = profile_dir.join(".ampcode");

        fs::create_dir_all(&ampcode_dir).await?;

        let content = serde_json::to_string_pretty(config)?;
        fs::write(ampcode_dir.join("settings.json"), content)
            .await
            .context("Failed to write ampcode config")?;

        Ok(())
    }

    /// Get a specific file from a config profile.
    pub async fn get_config_profile_file(&self, profile: &str, file_path: &str) -> Result<String> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let path = profile_dir.join(file_path);

        if !path.exists() {
            anyhow::bail!("File not found: {}", file_path);
        }

        fs::read_to_string(&path)
            .await
            .context("Failed to read config file")
    }

    /// Save a specific file in a config profile.
    pub async fn save_config_profile_file(
        &self,
        profile: &str,
        file_path: &str,
        content: &str,
    ) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let path = profile_dir.join(file_path);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&path, content)
            .await
            .context("Failed to write config file")?;

        Ok(())
    }

    /// Delete a specific file from a config profile.
    pub async fn delete_config_profile_file(&self, profile: &str, file_path: &str) -> Result<()> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        let path = profile_dir.join(file_path);

        if !path.exists() {
            anyhow::bail!("File not found: {}", file_path);
        }

        fs::remove_file(&path)
            .await
            .context("Failed to delete config file")?;

        // Clean up empty parent directories
        if let Some(parent) = path.parent() {
            if parent != profile_dir {
                let _ = fs::remove_dir(parent).await; // Ignore error if not empty
            }
        }

        Ok(())
    }

    /// List all files in a config profile.
    pub async fn list_config_profile_files(&self, profile: &str) -> Result<Vec<String>> {
        Self::validate_name(profile)?;

        let profile_dir = self.path.join(CONFIGS_DIR).join(profile);
        if !profile_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        Self::collect_files_recursive(&profile_dir, &profile_dir, &mut files).await?;

        files.sort();
        Ok(files)
    }

    /// Recursively collect all files in a directory.
    #[async_recursion::async_recursion]
    async fn collect_files_recursive(
        base_dir: &Path,
        current_dir: &Path,
        files: &mut Vec<String>,
    ) -> Result<()> {
        let mut entries = fs::read_dir(current_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let metadata = fs::metadata(&path).await?;

            if metadata.is_dir() {
                Self::collect_files_recursive(base_dir, &path, files).await?;
            } else if metadata.is_file() {
                let relative_path = path
                    .strip_prefix(base_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                files.push(relative_path);
            }
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Harness Defaults
    // ─────────────────────────────────────────────────────────────────────────

    /// Get a harness default file from the library.
    /// Harness defaults are stored at the library root in directories like:
    /// - opencode/oh-my-opencode.json
    /// - opencode/settings.json
    /// - claudecode/config.json
    /// - ampcode/config.json
    /// - sandboxed/config.json
    pub async fn get_harness_default_file(&self, harness: &str, file_name: &str) -> Result<String> {
        // Validate harness name
        let valid_harnesses = ["opencode", "claudecode", "ampcode", "sandboxed"];
        if !valid_harnesses.contains(&harness) {
            anyhow::bail!("Invalid harness: {}", harness);
        }

        let path = self.path.join(harness).join(file_name);

        if !path.exists() {
            anyhow::bail!("Harness default file not found: {}/{}", harness, file_name);
        }

        fs::read_to_string(&path)
            .await
            .context("Failed to read harness default file")
    }

    /// List all default files for a harness.
    pub async fn list_harness_default_files(&self, harness: &str) -> Result<Vec<String>> {
        // Validate harness name
        let valid_harnesses = ["opencode", "claudecode", "ampcode", "sandboxed"];
        if !valid_harnesses.contains(&harness) {
            anyhow::bail!("Invalid harness: {}", harness);
        }

        let harness_dir = self.path.join(harness);
        if !harness_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        let mut entries = fs::read_dir(&harness_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    files.push(name.to_string_lossy().to_string());
                }
            }
        }

        files.sort();
        Ok(files)
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
        let _ = fs::create_dir_all(self.path.join(INIT_SCRIPT_DIR)).await;

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

    #[cfg(test)]
    async fn with_test_store(path: PathBuf) -> LibraryStore {
        LibraryStore {
            path,
            remote: "test-remote".to_string(),
        }
    }
}

#[cfg(test)]
mod opencode_settings_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "System config merging not implemented for config profiles yet"]
    async fn merges_missing_agents_from_system_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library_path = temp.path().join("library");
        let system_path = temp.path().join("system");

        // Use new config profile structure
        tokio::fs::create_dir_all(library_path.join("configs/default/.opencode"))
            .await
            .expect("create library dir");
        tokio::fs::create_dir_all(&system_path)
            .await
            .expect("create system dir");

        let library_settings = serde_json::json!({
            "agents": {
                "sisyphus": { "model": "openai/gpt-4o-mini" }
            }
        });
        tokio::fs::write(
            library_path.join("configs/default/.opencode/settings.json"),
            serde_json::to_string_pretty(&library_settings).unwrap(),
        )
        .await
        .expect("write library settings");

        let system_settings = serde_json::json!({
            "agents": {
                "sisyphus": { "model": "openai/gpt-4o-mini" },
                "prometheus": { "model": "openai/gpt-4o" }
            }
        });
        tokio::fs::write(
            system_path.join("oh-my-opencode.json"),
            serde_json::to_string_pretty(&system_settings).unwrap(),
        )
        .await
        .expect("write system settings");

        std::env::set_var("OPENCODE_CONFIG_DIR", &system_path);

        let store = LibraryStore::with_test_store(library_path).await;
        let merged = store.get_opencode_settings().await.expect("get settings");

        let agents = merged.get("agents").and_then(|v| v.as_object()).unwrap();
        assert!(agents.contains_key("sisyphus"));
        assert!(agents.contains_key("prometheus"));

        // Library file should be updated with prometheus.
        let updated_path = temp
            .path()
            .join("library/configs/default/.opencode/settings.json");
        let updated = tokio::fs::read_to_string(updated_path)
            .await
            .expect("read updated library");
        let updated_value: serde_json::Value = serde_json::from_str(&updated).unwrap();
        let updated_agents = updated_value
            .get("agents")
            .and_then(|v| v.as_object())
            .unwrap();
        assert!(updated_agents.contains_key("prometheus"));
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

#[cfg(test)]
mod skill_encryption_tests {
    use super::*;

    /// Helper to set up a test encryption key
    fn setup_test_key() {
        let test_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        std::env::set_var(env_crypto::PRIVATE_KEY_ENV, test_key);
    }

    #[tokio::test]
    async fn test_save_skill_encrypts_unversioned_tags() {
        setup_test_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LibraryStore::with_test_store(temp.path().to_path_buf()).await;

        // Create skills directory
        fs::create_dir_all(store.skills_dir()).await.unwrap();

        // Save a skill with unversioned encrypted tag
        let content = r#"---
description: Test skill with secret
---

# Test Skill

API Key: <encrypted>sk-secret-key-12345</encrypted>
"#;

        store.save_skill("test-skill", content).await.unwrap();

        // Read the raw file from disk
        let skill_md = store.skills_dir().join("test-skill").join("SKILL.md");
        let raw_content = fs::read_to_string(&skill_md).await.unwrap();

        // Verify the file has versioned (encrypted) tags, not plaintext
        assert!(
            raw_content.contains("<encrypted v=\"1\">"),
            "File should contain versioned encrypted tag"
        );
        assert!(
            !raw_content.contains("<encrypted>sk-secret-key-12345</encrypted>"),
            "File should NOT contain plaintext secret"
        );
        assert!(
            !raw_content.contains("sk-secret-key-12345"),
            "Plaintext secret should not appear anywhere in file"
        );
    }

    #[tokio::test]
    async fn test_get_skill_decrypts_for_display() {
        setup_test_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LibraryStore::with_test_store(temp.path().to_path_buf()).await;

        // Create skills directory
        fs::create_dir_all(store.skills_dir()).await.unwrap();

        // Save a skill with unversioned encrypted tag
        let content = r#"---
description: Test skill with secret
---

# Test Skill

API Key: <encrypted>sk-secret-key-12345</encrypted>
"#;

        store.save_skill("test-skill", content).await.unwrap();

        // Get the skill (should decrypt for display)
        let skill = store.get_skill("test-skill").await.unwrap();

        // The returned content should have unversioned tags with plaintext
        assert!(
            skill
                .content
                .contains("<encrypted>sk-secret-key-12345</encrypted>"),
            "Skill content should show decrypted value in unversioned tag format"
        );
    }

    #[tokio::test]
    async fn test_encrypt_skill_file_processes_unversioned_tags() {
        setup_test_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LibraryStore::with_test_store(temp.path().to_path_buf()).await;

        // Create skill directory and write file with unversioned tag (simulating git pull)
        let skill_dir = store.skills_dir().join("imported-skill");
        fs::create_dir_all(&skill_dir).await.unwrap();

        let content = r#"---
description: Imported skill
---

Secret: <encrypted>my-api-key</encrypted>
"#;
        fs::write(skill_dir.join("SKILL.md"), content)
            .await
            .unwrap();

        // Encrypt the skill file
        store.encrypt_skill_file("imported-skill").await.unwrap();

        // Verify the file is now encrypted
        let raw_content = fs::read_to_string(skill_dir.join("SKILL.md"))
            .await
            .unwrap();

        assert!(
            raw_content.contains("<encrypted v=\"1\">"),
            "File should be encrypted after encrypt_skill_file"
        );
        assert!(
            !raw_content.contains("<encrypted>my-api-key</encrypted>"),
            "Plaintext should be replaced with ciphertext"
        );
    }

    #[tokio::test]
    async fn test_encrypt_all_skill_files() {
        setup_test_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LibraryStore::with_test_store(temp.path().to_path_buf()).await;

        // Create multiple skills with unversioned tags
        let skills_dir = store.skills_dir();
        fs::create_dir_all(&skills_dir).await.unwrap();

        for name in ["skill-a", "skill-b"] {
            let skill_dir = skills_dir.join(name);
            fs::create_dir_all(&skill_dir).await.unwrap();
            let content = format!(
                "---\ndescription: {}\n---\n\nKey: <encrypted>secret-{}</encrypted>\n",
                name, name
            );
            fs::write(skill_dir.join("SKILL.md"), content)
                .await
                .unwrap();
        }

        // Encrypt all skills
        store.encrypt_all_skill_files().await.unwrap();

        // Verify both are encrypted
        for name in ["skill-a", "skill-b"] {
            let raw = fs::read_to_string(skills_dir.join(name).join("SKILL.md"))
                .await
                .unwrap();
            assert!(
                raw.contains("<encrypted v=\"1\">"),
                "Skill {} should be encrypted",
                name
            );
            assert!(
                !raw.contains(&format!("secret-{}", name)),
                "Skill {} should not have plaintext secret",
                name
            );
        }
    }

    #[tokio::test]
    async fn test_already_encrypted_not_double_encrypted() {
        setup_test_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LibraryStore::with_test_store(temp.path().to_path_buf()).await;

        fs::create_dir_all(store.skills_dir()).await.unwrap();

        // Save a skill (gets encrypted)
        let content = "---\ndescription: test\n---\n\nKey: <encrypted>secret</encrypted>\n";
        store.save_skill("test-skill", content).await.unwrap();

        // Read the encrypted content
        let skill_md = store.skills_dir().join("test-skill").join("SKILL.md");
        let first_save = fs::read_to_string(&skill_md).await.unwrap();

        // Save again (should not change)
        store.save_skill("test-skill", content).await.unwrap();
        let second_save = fs::read_to_string(&skill_md).await.unwrap();

        // Both saves should produce encrypted content (though ciphertext may differ due to random nonce)
        assert!(first_save.contains("<encrypted v=\"1\">"));
        assert!(second_save.contains("<encrypted v=\"1\">"));

        // The number of encrypted tags should be the same
        let count1 = first_save.matches("<encrypted v=\"1\">").count();
        let count2 = second_save.matches("<encrypted v=\"1\">").count();
        assert_eq!(
            count1, count2,
            "Should not create additional encrypted tags"
        );
    }
}
