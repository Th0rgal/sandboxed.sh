//! Configuration library management.
//!
//! This module manages a git-based configuration library containing:
//! - MCP server definitions (`mcp/servers.json`)
//! - Skills (`skills/*/SKILL.md` with references)
//! - Commands/prompts (`commands/*.md`)

mod git;
pub mod types;

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs;

pub use types::*;

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

    /// Commit all changes with a message.
    pub async fn commit(&self, message: &str) -> Result<()> {
        git::commit(&self.path, message).await
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

        let servers: HashMap<String, McpServer> =
            serde_json::from_str(&content).context("Failed to parse mcp/servers.json")?;

        Ok(servers)
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
    // Skills (skills/*/SKILL.md)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all skills with their summaries.
    pub async fn list_skills(&self) -> Result<Vec<SkillSummary>> {
        let skills_dir = self.path.join("skills");

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

            let name = entry
                .file_name()
                .to_string_lossy()
                .to_string();

            // Read and parse frontmatter for description
            let content = fs::read_to_string(&skill_md).await.ok();
            let (frontmatter, _) = content
                .as_ref()
                .map(|c| parse_frontmatter(c))
                .unwrap_or((None, ""));

            let description = frontmatter
                .as_ref()
                .and_then(|fm| fm.get("description"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            skills.push(SkillSummary {
                name,
                description,
                path: format!("skills/{}", entry.file_name().to_string_lossy()),
            });
        }

        // Sort by name
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(skills)
    }

    /// Get a skill by name with full content.
    pub async fn get_skill(&self, name: &str) -> Result<Skill> {
        Self::validate_name(name)?;
        let skill_dir = self.path.join("skills").join(name);
        let skill_md = skill_dir.join("SKILL.md");

        if !skill_md.exists() {
            anyhow::bail!("Skill not found: {}", name);
        }

        let content = fs::read_to_string(&skill_md)
            .await
            .context("Failed to read SKILL.md")?;

        let (frontmatter, _body) = parse_frontmatter(&content);

        let description = frontmatter
            .as_ref()
            .and_then(|fm| fm.get("description"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // List reference files
        let references = self.list_references(&skill_dir).await?;

        Ok(Skill {
            name: name.to_string(),
            description,
            path: format!("skills/{}", name),
            content,
            references,
        })
    }

    /// Save a skill's SKILL.md content.
    pub async fn save_skill(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;
        let skill_dir = self.path.join("skills").join(name);
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
        let skill_dir = self.path.join("skills").join(name);

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
        let skill_dir = self.path.join("skills").join(skill_name);
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
        let skill_dir = self.path.join("skills").join(skill_name);
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

    // ─────────────────────────────────────────────────────────────────────────
    // Commands (commands/*.md)
    // ─────────────────────────────────────────────────────────────────────────

    /// List all commands with their summaries.
    pub async fn list_commands(&self) -> Result<Vec<CommandSummary>> {
        let commands_dir = self.path.join("commands");

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

            let description = frontmatter
                .as_ref()
                .and_then(|fm| fm.get("description"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            commands.push(CommandSummary {
                name,
                description,
                path: format!("commands/{}", file_name),
            });
        }

        // Sort by name
        commands.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(commands)
    }

    /// Get a command by name with full content.
    pub async fn get_command(&self, name: &str) -> Result<Command> {
        Self::validate_name(name)?;
        let command_path = self.path.join("commands").join(format!("{}.md", name));

        if !command_path.exists() {
            anyhow::bail!("Command not found: {}", name);
        }

        let content = fs::read_to_string(&command_path)
            .await
            .context("Failed to read command file")?;

        let (frontmatter, _body) = parse_frontmatter(&content);

        let description = frontmatter
            .as_ref()
            .and_then(|fm| fm.get("description"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(Command {
            name: name.to_string(),
            description,
            path: format!("commands/{}.md", name),
            content,
        })
    }

    /// Save a command's content.
    pub async fn save_command(&self, name: &str, content: &str) -> Result<()> {
        Self::validate_name(name)?;
        let commands_dir = self.path.join("commands");
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
        let command_path = self.path.join("commands").join(format!("{}.md", name));

        if command_path.exists() {
            fs::remove_file(&command_path)
                .await
                .context("Failed to delete command file")?;
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// List reference files in a skill directory (excluding SKILL.md).
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
