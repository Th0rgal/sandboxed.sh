//! Mission storage module with pluggable backends.
//!
//! Supports:
//! - `memory`: In-memory storage (non-persistent, for testing)
//! - `file`: JSON file-based storage (legacy)
//! - `sqlite`: SQLite database with full event logging

mod file;
mod memory;
mod sqlite;

pub use file::FileMissionStore;
pub use memory::InMemoryMissionStore;
pub use sqlite::SqliteMissionStore;

use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo, MissionStatus};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// A mission (persistent goal-oriented session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: Uuid,
    pub status: MissionStatus,
    pub title: Option<String>,
    /// Workspace ID where this mission runs (defaults to host workspace)
    #[serde(default = "default_workspace_id")]
    pub workspace_id: Uuid,
    /// Workspace name (resolved from workspace_id for display)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// Agent name from library (e.g., "code-reviewer")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional model override (provider/model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    /// Backend to use for this mission ("opencode" or "claudecode")
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Config profile to use for this mission (from library configs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_profile: Option<String>,
    pub history: Vec<MissionHistoryEntry>,
    pub created_at: String,
    pub updated_at: String,
    /// When this mission was interrupted (if status is Interrupted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
    /// Whether this mission can be resumed
    #[serde(default)]
    pub resumable: bool,
    /// Desktop sessions started during this mission (used for reconnect/stream resume)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub desktop_sessions: Vec<DesktopSessionInfo>,
    /// Session ID for conversation persistence (used by Claude Code --session-id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Why the mission terminated (for failed/completed missions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

fn default_backend() -> String {
    "opencode".to_string()
}

fn default_workspace_id() -> Uuid {
    crate::workspace::DEFAULT_WORKSPACE_ID
}

/// A single entry in the mission history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionHistoryEntry {
    pub role: String,
    pub content: String,
}

/// A stored event with full metadata (for event replay/debugging).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: i64,
    pub mission_id: Uuid,
    pub sequence: i64,
    pub event_type: String,
    pub timestamp: String,
    pub event_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub content: String,
    pub metadata: serde_json::Value,
}

/// Get current timestamp as RFC3339 string.
pub fn now_string() -> String {
    Utc::now().to_rfc3339()
}

/// Sanitize a string for use as a filename.
pub fn sanitize_filename(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// Mission store trait - implemented by all storage backends.
#[async_trait]
pub trait MissionStore: Send + Sync {
    /// Whether this store persists data across restarts.
    fn is_persistent(&self) -> bool;

    /// List missions, ordered by updated_at descending.
    async fn list_missions(&self, limit: usize, offset: usize) -> Result<Vec<Mission>, String>;

    /// Get a single mission by ID.
    async fn get_mission(&self, id: Uuid) -> Result<Option<Mission>, String>;

    /// Create a new mission.
    async fn create_mission(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
    ) -> Result<Mission, String>;

    /// Update mission status.
    async fn update_mission_status(&self, id: Uuid, status: MissionStatus) -> Result<(), String>;

    /// Update mission status with terminal reason (for failed/completed missions).
    async fn update_mission_status_with_reason(
        &self,
        id: Uuid,
        status: MissionStatus,
        terminal_reason: Option<&str>,
    ) -> Result<(), String>;

    /// Update mission conversation history.
    async fn update_mission_history(
        &self,
        id: Uuid,
        history: &[MissionHistoryEntry],
    ) -> Result<(), String>;

    /// Update mission desktop sessions.
    async fn update_mission_desktop_sessions(
        &self,
        id: Uuid,
        sessions: &[DesktopSessionInfo],
    ) -> Result<(), String>;

    /// Update mission title.
    async fn update_mission_title(&self, id: Uuid, title: &str) -> Result<(), String>;

    /// Update mission session ID (for backends like Amp that generate their own IDs).
    async fn update_mission_session_id(&self, id: Uuid, session_id: &str) -> Result<(), String>;

    /// Update mission agent tree.
    async fn update_mission_tree(&self, id: Uuid, tree: &AgentTreeNode) -> Result<(), String>;

    /// Get mission agent tree.
    async fn get_mission_tree(&self, id: Uuid) -> Result<Option<AgentTreeNode>, String>;

    /// Delete a mission.
    async fn delete_mission(&self, id: Uuid) -> Result<bool, String>;

    /// Delete empty untitled missions, excluding the specified IDs.
    async fn delete_empty_untitled_missions_excluding(
        &self,
        exclude: &[Uuid],
    ) -> Result<usize, String>;

    /// Get missions that have been active but stale for the specified hours.
    async fn get_stale_active_missions(&self, stale_hours: u64) -> Result<Vec<Mission>, String>;

    /// Get all missions currently in active status (for startup recovery).
    async fn get_all_active_missions(&self) -> Result<Vec<Mission>, String>;

    /// Insert a mission summary (for historical lookup).
    async fn insert_mission_summary(
        &self,
        mission_id: Uuid,
        summary: &str,
        key_files: &[String],
        success: bool,
    ) -> Result<(), String>;

    // === Event logging methods (default no-op for backward compatibility) ===

    /// Log a streaming event. Called for every AgentEvent during execution.
    async fn log_event(&self, mission_id: Uuid, event: &AgentEvent) -> Result<(), String> {
        let _ = (mission_id, event);
        Ok(())
    }

    /// Get all events for a mission (for replay/debugging).
    async fn get_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, event_types, limit, offset);
        Ok(vec![])
    }

    /// Get total cost in cents across all missions.
    /// Aggregates cost_cents from all assistant_message events.
    async fn get_total_cost_cents(&self) -> Result<u64, String> {
        Ok(0)
    }
}

/// Mission store type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissionStoreType {
    Memory,
    File,
    #[default]
    Sqlite,
}

impl MissionStoreType {
    /// Parse from environment variable value.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "memory" => Self::Memory,
            "file" | "json" => Self::File,
            "sqlite" | "db" => Self::Sqlite,
            _ => Self::default(),
        }
    }
}

/// Create a mission store based on type and configuration.
pub async fn create_mission_store(
    store_type: MissionStoreType,
    base_dir: PathBuf,
    user_id: &str,
) -> Result<Box<dyn MissionStore>, String> {
    match store_type {
        MissionStoreType::Memory => Ok(Box::new(InMemoryMissionStore::new())),
        MissionStoreType::File => {
            let store = FileMissionStore::new(base_dir, user_id).await?;
            Ok(Box::new(store))
        }
        MissionStoreType::Sqlite => {
            let store = SqliteMissionStore::new(base_dir, user_id).await?;
            Ok(Box::new(store))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that missions are created with Pending status (not Active).
    /// This is critical to prevent the race condition where startup recovery
    /// marks newly created missions as interrupted.
    #[tokio::test]
    async fn test_mission_created_with_pending_status() {
        let store = InMemoryMissionStore::new();

        let mission = store
            .create_mission(Some("Test Mission"), None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(
            mission.status,
            MissionStatus::Pending,
            "New missions should have Pending status, not {:?}",
            mission.status
        );
    }

    /// Test that Pending missions are NOT returned by get_all_active_missions.
    /// This ensures the orphan detection won't mark Pending missions as interrupted.
    #[tokio::test]
    async fn test_pending_missions_not_in_active_list() {
        let store = InMemoryMissionStore::new();

        // Create a pending mission
        let mission = store
            .create_mission(Some("Pending Mission"), None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(mission.status, MissionStatus::Pending);

        // get_all_active_missions should NOT include pending missions
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        assert!(
            active_missions.is_empty(),
            "Pending missions should not appear in active missions list"
        );
    }

    /// Test that missions transition correctly from Pending to Active.
    #[tokio::test]
    async fn test_mission_status_transition_pending_to_active() {
        let store = InMemoryMissionStore::new();

        // Create a pending mission
        let mission = store
            .create_mission(Some("Test Mission"), None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(mission.status, MissionStatus::Pending);

        // Update status to Active
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("Failed to update status");

        // Verify status changed
        let updated = store
            .get_mission(mission.id)
            .await
            .expect("Failed to get mission")
            .expect("Mission not found");

        assert_eq!(
            updated.status,
            MissionStatus::Active,
            "Mission status should be Active after update"
        );

        // Now it should appear in active missions
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        assert_eq!(
            active_missions.len(),
            1,
            "Active mission should appear in active missions list"
        );
        assert_eq!(active_missions[0].id, mission.id);
    }

    /// Test the orphan detection scenario: Active missions should be detected,
    /// but Pending missions should not.
    #[tokio::test]
    async fn test_orphan_detection_ignores_pending() {
        let store = InMemoryMissionStore::new();

        // Create two missions
        let pending_mission = store
            .create_mission(Some("Pending"), None, None, None, None, None)
            .await
            .expect("Failed to create pending mission");

        let active_mission = store
            .create_mission(Some("Will be Active"), None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        // Activate only one mission
        store
            .update_mission_status(active_mission.id, MissionStatus::Active)
            .await
            .expect("Failed to activate mission");

        // Check active missions (simulating orphan detection)
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        // Only the active mission should be in the list
        assert_eq!(
            active_missions.len(),
            1,
            "Only Active missions should be returned, not Pending ones"
        );
        assert_eq!(active_missions[0].id, active_mission.id);

        // Pending mission should still exist but not be in active list
        let pending = store
            .get_mission(pending_mission.id)
            .await
            .expect("Failed to get pending mission")
            .expect("Pending mission not found");
        assert_eq!(pending.status, MissionStatus::Pending);
    }

    /// Test MissionStatus Display implementation includes Pending.
    #[test]
    fn test_mission_status_display() {
        assert_eq!(format!("{}", MissionStatus::Pending), "pending");
        assert_eq!(format!("{}", MissionStatus::Active), "active");
        assert_eq!(format!("{}", MissionStatus::Completed), "completed");
        assert_eq!(format!("{}", MissionStatus::Interrupted), "interrupted");
    }
}
