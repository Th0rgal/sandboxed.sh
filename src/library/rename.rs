//! Library item rename functionality with cascade reference updates.
//!
//! This module handles renaming library items (skills, commands, agents, tools,
//! workspace templates) while automatically updating all cross-references.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

use super::types::SandboxedConfig;
use super::LibraryStore;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// The type of library item being renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Skill,
    Command,
    Agent,
    Tool,
    WorkspaceTemplate,
}

impl ItemType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::WorkspaceTemplate => "workspace-template",
        }
    }
}

/// A single change that will be or was applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RenameChange {
    /// Rename a file or directory.
    RenameFile { from: String, to: String },
    /// Update a reference in a file.
    UpdateReference {
        file: String,
        field: String,
        old_value: String,
        new_value: String,
    },
    /// Update workspace skills/tools list (in memory, via workspace store).
    UpdateWorkspace {
        workspace_id: String,
        workspace_name: String,
        field: String,
    },
}

/// Result of a rename operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameResult {
    /// Whether the operation was successful.
    pub success: bool,
    /// Changes that were applied (or would be applied in dry_run mode).
    pub changes: Vec<RenameChange>,
    /// Any warnings encountered.
    pub warnings: Vec<String>,
    /// Error message if success is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Reference Finding
// ─────────────────────────────────────────────────────────────────────────────

impl LibraryStore {
    /// Find all references to an item in the library.
    pub async fn find_references(
        &self,
        item_type: ItemType,
        name: &str,
    ) -> Result<Vec<RenameChange>> {
        let mut refs = Vec::new();

        match item_type {
            ItemType::Skill => {
                // Skills are referenced by:
                // 1. workspace-template/*.json -> skills array
                refs.extend(self.find_skill_refs_in_templates(name).await?);
            }
            ItemType::Agent => {
                // Agents are referenced by:
                // 1. sandboxed/config.json -> hidden_agents, default_agent
                refs.extend(self.find_agent_refs_in_config(name).await?);
            }
            ItemType::Command | ItemType::Tool | ItemType::WorkspaceTemplate => {
                // These don't have direct cross-references in library files.
                // Tools are referenced by workspaces (handled at API layer).
            }
        }

        Ok(refs)
    }

    /// Find references to a skill in workspace templates.
    async fn find_skill_refs_in_templates(&self, skill_name: &str) -> Result<Vec<RenameChange>> {
        let mut refs = Vec::new();
        let templates_dir = self.path.join("workspace-template");

        if !templates_dir.exists() {
            return Ok(refs);
        }

        let mut entries = fs::read_dir(&templates_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(template) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(skills) = template.get("skills").and_then(|s| s.as_array()) {
                            if skills.iter().any(|s| s.as_str() == Some(skill_name)) {
                                let rel_path = path
                                    .strip_prefix(&self.path)
                                    .unwrap_or(&path)
                                    .to_string_lossy()
                                    .to_string();
                                refs.push(RenameChange::UpdateReference {
                                    file: rel_path,
                                    field: "skills".to_string(),
                                    old_value: skill_name.to_string(),
                                    new_value: String::new(), // Will be filled in during rename
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(refs)
    }

    /// Find references to an agent in sandboxed config.
    async fn find_agent_refs_in_config(&self, agent_name: &str) -> Result<Vec<RenameChange>> {
        let mut refs = Vec::new();
        let config_path = self.path.join("sandboxed/config.json");

        if !config_path.exists() {
            return Ok(refs);
        }

        if let Ok(content) = fs::read_to_string(&config_path).await {
            if let Ok(config) = serde_json::from_str::<SandboxedConfig>(&content) {
                if config.hidden_agents.contains(&agent_name.to_string()) {
                    refs.push(RenameChange::UpdateReference {
                        file: "sandboxed/config.json".to_string(),
                        field: "hidden_agents".to_string(),
                        old_value: agent_name.to_string(),
                        new_value: String::new(),
                    });
                }
                if config.default_agent.as_deref() == Some(agent_name) {
                    refs.push(RenameChange::UpdateReference {
                        file: "sandboxed/config.json".to_string(),
                        field: "default_agent".to_string(),
                        old_value: agent_name.to_string(),
                        new_value: String::new(),
                    });
                }
            }
        }

        Ok(refs)
    }

    /// Rename an item and update all references.
    pub async fn rename_item(
        &self,
        item_type: ItemType,
        old_name: &str,
        new_name: &str,
        dry_run: bool,
    ) -> Result<RenameResult> {
        // Validate names
        Self::validate_name(old_name)?;
        Self::validate_name(new_name)?;

        if old_name == new_name {
            return Ok(RenameResult {
                success: true,
                changes: vec![],
                warnings: vec!["Old and new names are identical".to_string()],
                error: None,
            });
        }

        // Check source exists
        let (old_path, new_path) = self.get_item_paths(item_type, old_name, new_name);
        if !old_path.exists() {
            return Ok(RenameResult {
                success: false,
                changes: vec![],
                warnings: vec![],
                error: Some(format!("{} '{}' not found", item_type.as_str(), old_name)),
            });
        }

        // Check target doesn't exist
        if new_path.exists() {
            return Ok(RenameResult {
                success: false,
                changes: vec![],
                warnings: vec![],
                error: Some(format!(
                    "{} '{}' already exists",
                    item_type.as_str(),
                    new_name
                )),
            });
        }

        // Build change list
        let mut changes = Vec::new();
        let mut warnings = Vec::new();

        // Add the rename operation
        let old_rel = old_path
            .strip_prefix(&self.path)
            .unwrap_or(&old_path)
            .to_string_lossy()
            .to_string();
        let new_rel = new_path
            .strip_prefix(&self.path)
            .unwrap_or(&new_path)
            .to_string_lossy()
            .to_string();

        changes.push(RenameChange::RenameFile {
            from: old_rel,
            to: new_rel,
        });

        // Find and add reference updates
        let refs = self.find_references(item_type, old_name).await?;
        for mut ref_change in refs {
            // Fill in the new_value
            if let RenameChange::UpdateReference { new_value, .. } = &mut ref_change {
                *new_value = new_name.to_string();
            }
            changes.push(ref_change);
        }

        if dry_run {
            return Ok(RenameResult {
                success: true,
                changes,
                warnings,
                error: None,
            });
        }

        // Execute the rename
        if let Err(e) = self
            .execute_rename(item_type, old_name, new_name, &old_path, &new_path)
            .await
        {
            return Ok(RenameResult {
                success: false,
                changes: vec![],
                warnings,
                error: Some(format!("Failed to rename: {}", e)),
            });
        }

        // Execute reference updates
        for change in &changes {
            if let RenameChange::UpdateReference {
                file,
                field,
                old_value,
                new_value,
            } = change
            {
                if let Err(e) = self
                    .update_reference(file, field, old_value, new_value)
                    .await
                {
                    warnings.push(format!("Failed to update {}: {}", file, e));
                }
            }
        }

        Ok(RenameResult {
            success: true,
            changes,
            warnings,
            error: None,
        })
    }

    /// Get the old and new paths for an item type.
    fn get_item_paths(
        &self,
        item_type: ItemType,
        old_name: &str,
        new_name: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        match item_type {
            ItemType::Skill => (
                self.path.join("skill").join(old_name),
                self.path.join("skill").join(new_name),
            ),
            ItemType::Command => (
                self.path.join("command").join(format!("{}.md", old_name)),
                self.path.join("command").join(format!("{}.md", new_name)),
            ),
            ItemType::Agent => (
                self.path.join("agent").join(format!("{}.md", old_name)),
                self.path.join("agent").join(format!("{}.md", new_name)),
            ),
            ItemType::Tool => (
                self.path.join("tool").join(format!("{}.ts", old_name)),
                self.path.join("tool").join(format!("{}.ts", new_name)),
            ),
            ItemType::WorkspaceTemplate => (
                self.path
                    .join("workspace-template")
                    .join(format!("{}.json", old_name)),
                self.path
                    .join("workspace-template")
                    .join(format!("{}.json", new_name)),
            ),
        }
    }

    /// Execute the actual rename operation.
    async fn execute_rename(
        &self,
        item_type: ItemType,
        _old_name: &str,
        new_name: &str,
        old_path: &Path,
        new_path: &Path,
    ) -> Result<()> {
        // For workspace templates, also update the internal "name" field
        if item_type == ItemType::WorkspaceTemplate {
            let content = fs::read_to_string(old_path).await?;
            if let Ok(mut template) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(obj) = template.as_object_mut() {
                    obj.insert(
                        "name".to_string(),
                        serde_json::Value::String(new_name.to_string()),
                    );
                    let updated = serde_json::to_string_pretty(&template)?;
                    fs::write(old_path, updated).await?;
                }
            }
        }

        // Perform the rename
        fs::rename(old_path, new_path)
            .await
            .context("Failed to rename file/directory")?;

        Ok(())
    }

    /// Update a reference in a file.
    async fn update_reference(
        &self,
        file: &str,
        field: &str,
        old_value: &str,
        new_value: &str,
    ) -> Result<()> {
        let file_path = self.path.join(file);

        if file.ends_with(".json") {
            // JSON file (workspace template or sandboxed config)
            let content = fs::read_to_string(&file_path).await?;
            let mut data: serde_json::Value = serde_json::from_str(&content)?;

            if field == "skills" || field == "hidden_agents" {
                // Array field
                if let Some(arr) = data.get_mut(field).and_then(|a| a.as_array_mut()) {
                    for item in arr.iter_mut() {
                        if item.as_str() == Some(old_value) {
                            *item = serde_json::Value::String(new_value.to_string());
                        }
                    }
                }
            } else if field == "default_agent" {
                // String field
                if data.get(field).and_then(|v| v.as_str()) == Some(old_value) {
                    data[field] = serde_json::Value::String(new_value.to_string());
                }
            }

            let updated = serde_json::to_string_pretty(&data)?;
            fs::write(&file_path, updated).await?;
        } else if file.ends_with(".md") {
            // Markdown file with YAML frontmatter (agent)
            let content = fs::read_to_string(&file_path).await?;
            let updated = self.update_frontmatter_array(&content, field, old_value, new_value)?;
            fs::write(&file_path, updated).await?;
        }

        Ok(())
    }

    /// Update an array field in YAML frontmatter.
    fn update_frontmatter_array(
        &self,
        content: &str,
        field: &str,
        old_value: &str,
        new_value: &str,
    ) -> Result<String> {
        if !content.starts_with("---") {
            return Ok(content.to_string());
        }

        let rest = &content[3..];
        if let Some(end_pos) = rest.find("\n---") {
            let yaml_str = &rest[..end_pos];
            let body = &rest[end_pos..];

            // Parse and update YAML
            if let Ok(mut yaml) = serde_yaml::from_str::<serde_yaml::Value>(yaml_str) {
                if let Some(arr) = yaml.get_mut(field).and_then(|a| a.as_sequence_mut()) {
                    for item in arr.iter_mut() {
                        if item.as_str() == Some(old_value) {
                            *item = serde_yaml::Value::String(new_value.to_string());
                        }
                    }
                }

                let updated_yaml = serde_yaml::to_string(&yaml)?;
                return Ok(format!("---\n{}\n---{}", updated_yaml.trim(), body));
            }
        }

        Ok(content.to_string())
    }
}
