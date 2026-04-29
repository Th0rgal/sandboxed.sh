//! In-memory mission store (non-persistent).

use super::{now_string, Mission, MissionHistoryEntry, MissionStatus, MissionStore};
use crate::api::control::{AgentTreeNode, DesktopSessionInfo};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

const METADATA_SOURCE_USER: &str = "user";

#[derive(Clone)]
pub struct InMemoryMissionStore {
    missions: Arc<RwLock<HashMap<Uuid, Mission>>>,
    trees: Arc<RwLock<HashMap<Uuid, AgentTreeNode>>>,
}

impl InMemoryMissionStore {
    pub fn new() -> Self {
        Self {
            missions: Arc::new(RwLock::new(HashMap::new())),
            trees: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryMissionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MissionStore for InMemoryMissionStore {
    fn is_persistent(&self) -> bool {
        false
    }

    async fn list_missions(&self, limit: usize, offset: usize) -> Result<Vec<Mission>, String> {
        let mut missions: Vec<Mission> = self.missions.read().await.values().cloned().collect();
        missions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let missions = missions.into_iter().skip(offset).take(limit).collect();
        Ok(missions)
    }

    async fn get_mission(&self, id: Uuid) -> Result<Option<Mission>, String> {
        Ok(self.missions.read().await.get(&id).cloned())
    }

    async fn create_mission_with_parent(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        model_effort: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
        parent_mission_id: Option<Uuid>,
        working_directory: Option<&str>,
    ) -> Result<Mission, String> {
        let now = now_string();
        let metadata_source = title.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(METADATA_SOURCE_USER.to_string())
            }
        });
        let metadata_updated_at = metadata_source.as_ref().map(|_| now.clone());
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Pending,
            title: title.map(|s| s.to_string()),
            short_description: None,
            metadata_updated_at,
            metadata_source,
            metadata_model: None,
            metadata_version: None,
            workspace_id: workspace_id.unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
            workspace_name: None,
            agent: agent.map(|s| s.to_string()),
            model_override: model_override.map(|s| s.to_string()),
            model_effort: model_effort.map(|s| s.to_string()),
            backend: backend.unwrap_or("claudecode").to_string(),
            config_profile: config_profile.map(|s| s.to_string()),
            history: vec![],
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: Some(Uuid::new_v4().to_string()),
            terminal_reason: None,
            parent_mission_id,
            working_directory: working_directory.map(|s| s.to_string()),
            mission_mode: super::MissionMode::default(),
        };
        self.missions
            .write()
            .await
            .insert(mission.id, mission.clone());
        Ok(mission)
    }

    async fn get_child_missions(&self, parent_id: Uuid) -> Result<Vec<Mission>, String> {
        let missions = self.missions.read().await;
        Ok(missions
            .values()
            .filter(|m| m.parent_mission_id == Some(parent_id))
            .cloned()
            .collect())
    }

    async fn update_mission_status(&self, id: Uuid, status: MissionStatus) -> Result<(), String> {
        self.update_mission_status_with_reason(id, status, None)
            .await
    }

    async fn update_mission_status_with_reason(
        &self,
        id: Uuid,
        status: MissionStatus,
        terminal_reason: Option<&str>,
    ) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.status = status;
        let now = now_string();
        mission.updated_at = now.clone();
        mission.terminal_reason = terminal_reason.map(|s| s.to_string());
        // Failed missions with LlmError are also resumable (transient API errors)
        if matches!(
            status,
            MissionStatus::Interrupted | MissionStatus::Blocked | MissionStatus::Failed
        ) {
            mission.interrupted_at = Some(now);
            mission.resumable = true;
        } else {
            mission.interrupted_at = None;
            mission.resumable = false;
        }
        Ok(())
    }

    async fn update_mission_history(
        &self,
        id: Uuid,
        history: &[MissionHistoryEntry],
    ) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.history = history.to_vec();
        mission.updated_at = now_string();
        Ok(())
    }

    async fn update_mission_desktop_sessions(
        &self,
        id: Uuid,
        sessions: &[DesktopSessionInfo],
    ) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.desktop_sessions = sessions.to_vec();
        mission.updated_at = now_string();
        Ok(())
    }

    async fn update_mission_title(&self, id: Uuid, title: &str) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.title = Some(title.to_string());
        mission.metadata_source = Some("user".to_string());
        mission.metadata_model = None;
        mission.metadata_version = None;
        let now = now_string();
        mission.metadata_updated_at = Some(now.clone());
        mission.updated_at = now;
        Ok(())
    }

    async fn update_mission_run_settings(
        &self,
        id: Uuid,
        backend: Option<&str>,
        agent: Option<Option<&str>>,
        model_override: Option<Option<&str>>,
        model_effort: Option<Option<&str>>,
        config_profile: Option<Option<&str>>,
        session_id: &str,
    ) -> Result<Mission, String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;

        if let Some(backend) = backend {
            mission.backend = backend.to_string();
        }
        if let Some(agent) = agent {
            mission.agent = agent.map(ToString::to_string);
        }
        if let Some(model_override) = model_override {
            mission.model_override = model_override.map(ToString::to_string);
        }
        if let Some(model_effort) = model_effort {
            mission.model_effort = model_effort.map(ToString::to_string);
        }
        if let Some(config_profile) = config_profile {
            mission.config_profile = config_profile.map(ToString::to_string);
        }
        mission.session_id = Some(session_id.to_string());
        mission.resumable = false;
        mission.interrupted_at = None;
        mission.terminal_reason = None;
        mission.updated_at = now_string();

        Ok(mission.clone())
    }

    async fn update_mission_metadata(
        &self,
        id: Uuid,
        title: Option<Option<&str>>,
        short_description: Option<Option<&str>>,
        metadata_source: Option<Option<&str>>,
        metadata_model: Option<Option<&str>>,
        metadata_version: Option<Option<&str>>,
    ) -> Result<(), String> {
        if title.is_none()
            && short_description.is_none()
            && metadata_source.is_none()
            && metadata_model.is_none()
            && metadata_version.is_none()
        {
            return Ok(());
        }

        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;

        if let Some(title) = title {
            mission.title = title.map(ToString::to_string);
        }
        if let Some(short_description) = short_description {
            mission.short_description = short_description.map(ToString::to_string);
        }
        if let Some(metadata_source) = metadata_source {
            mission.metadata_source = metadata_source.map(ToString::to_string);
        }
        if let Some(metadata_model) = metadata_model {
            mission.metadata_model = metadata_model.map(ToString::to_string);
        }
        if let Some(metadata_version) = metadata_version {
            mission.metadata_version = metadata_version.map(ToString::to_string);
        }
        let now = now_string();
        mission.metadata_updated_at = Some(now.clone());
        mission.updated_at = now;
        Ok(())
    }

    async fn update_mission_session_id(&self, id: Uuid, session_id: &str) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.session_id = Some(session_id.to_string());
        mission.updated_at = now_string();
        Ok(())
    }

    async fn update_mission_tree(&self, id: Uuid, tree: &AgentTreeNode) -> Result<(), String> {
        self.trees.write().await.insert(id, tree.clone());
        Ok(())
    }

    async fn get_mission_tree(&self, id: Uuid) -> Result<Option<AgentTreeNode>, String> {
        Ok(self.trees.read().await.get(&id).cloned())
    }

    async fn delete_mission(&self, id: Uuid) -> Result<bool, String> {
        let removed = self.missions.write().await.remove(&id).is_some();
        self.trees.write().await.remove(&id);
        Ok(removed)
    }

    async fn delete_empty_untitled_missions_excluding(
        &self,
        exclude: &[Uuid],
    ) -> Result<usize, String> {
        let mut missions = self.missions.write().await;

        let to_delete: Vec<Uuid> = missions
            .iter()
            .filter(|(id, mission)| {
                if exclude.contains(id) {
                    return false;
                }
                let title = mission.title.clone().unwrap_or_default();
                let title_empty = title.trim().is_empty() || title == "Untitled Mission";
                let history_empty = mission.history.is_empty();
                let active = mission.status == MissionStatus::Active;
                active && history_empty && title_empty
            })
            .map(|(id, _)| *id)
            .collect();

        for id in &to_delete {
            missions.remove(id);
        }
        drop(missions);

        let mut trees = self.trees.write().await;
        for id in &to_delete {
            trees.remove(id);
        }

        Ok(to_delete.len())
    }

    async fn get_stale_active_missions(&self, stale_hours: u64) -> Result<Vec<Mission>, String> {
        if stale_hours == 0 {
            return Ok(Vec::new());
        }
        let cutoff = Utc::now() - chrono::Duration::hours(stale_hours as i64);
        let missions: Vec<Mission> = self
            .missions
            .read()
            .await
            .values()
            .filter(|m| m.status == MissionStatus::Active)
            .filter(|m| {
                chrono::DateTime::parse_from_rfc3339(&m.updated_at)
                    .map(|t| t < cutoff)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        Ok(missions)
    }

    async fn get_all_active_missions(&self) -> Result<Vec<Mission>, String> {
        let missions: Vec<Mission> = self
            .missions
            .read()
            .await
            .values()
            .filter(|m| m.status == MissionStatus::Active)
            .cloned()
            .collect();
        Ok(missions)
    }

    async fn insert_mission_summary(
        &self,
        _mission_id: Uuid,
        _summary: &str,
        _key_files: &[String],
        _success: bool,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_mission_metadata_is_noop_when_fields_missing() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Renamed")),
                Some(Some("Short summary")),
                Some(Some("backend_heuristic")),
                None,
                Some(Some("v1")),
            )
            .await
            .expect("set metadata");

        let after_set = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let metadata_updated_at = after_set
            .metadata_updated_at
            .clone()
            .expect("metadata timestamp should be set");
        let updated_at = after_set.updated_at.clone();

        store
            .update_mission_metadata(mission.id, None, None, None, None, None)
            .await
            .expect("noop metadata update");

        let after_noop = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");

        assert_eq!(after_noop.title.as_deref(), Some("Renamed"));
        assert_eq!(
            after_noop.short_description.as_deref(),
            Some("Short summary")
        );
        assert_eq!(
            after_noop.metadata_source.as_deref(),
            Some("backend_heuristic")
        );
        assert_eq!(after_noop.metadata_model.as_deref(), None);
        assert_eq!(after_noop.metadata_version.as_deref(), Some("v1"));
        assert_eq!(
            after_noop.metadata_updated_at.as_deref(),
            Some(metadata_updated_at.as_str())
        );
        assert_eq!(after_noop.updated_at, updated_at);
    }

    #[tokio::test]
    async fn update_mission_metadata_can_clear_fields() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Renamed")),
                Some(Some("Short summary")),
                Some(Some("backend_heuristic")),
                None,
                Some(Some("v1")),
            )
            .await
            .expect("set metadata");

        store
            .update_mission_metadata(
                mission.id,
                Some(None),
                Some(None),
                Some(None),
                None,
                Some(None),
            )
            .await
            .expect("clear metadata fields");

        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(mission.title, None);
        assert_eq!(mission.short_description, None);
        assert_eq!(mission.metadata_source, None);
        assert_eq!(mission.metadata_version, None);
    }

    #[tokio::test]
    async fn update_mission_title_marks_user_metadata_source() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        store
            .update_mission_metadata(
                mission.id,
                None,
                None,
                Some(Some("backend_heuristic")),
                Some(Some("gpt-5")),
                Some(Some("v1")),
            )
            .await
            .expect("seed metadata source");
        let seeded = store
            .get_mission(mission.id)
            .await
            .expect("get seeded mission")
            .expect("mission exists");
        let seeded_metadata_updated_at = seeded
            .metadata_updated_at
            .expect("seed metadata timestamp should exist");

        store
            .update_mission_title(mission.id, "Manual title")
            .await
            .expect("rename mission");

        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(mission.title.as_deref(), Some("Manual title"));
        assert_eq!(mission.metadata_source.as_deref(), Some("user"));
        assert_eq!(mission.metadata_model, None);
        assert_eq!(mission.metadata_version, None);
        let metadata_updated_at = mission
            .metadata_updated_at
            .expect("manual title update should set metadata timestamp");
        assert!(
            metadata_updated_at >= seeded_metadata_updated_at,
            "manual title update should advance metadata timestamp"
        );
    }

    #[tokio::test]
    async fn update_mission_run_settings_clears_terminal_reason() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        store
            .update_mission_status_with_reason(
                mission.id,
                MissionStatus::Failed,
                Some("rate_limited"),
            )
            .await
            .expect("set terminal reason");

        let updated = store
            .update_mission_run_settings(
                mission.id,
                Some("codex"),
                None,
                None,
                None,
                None,
                "new-session",
            )
            .await
            .expect("update run settings");

        assert_eq!(updated.terminal_reason, None);
        assert_eq!(updated.session_id.as_deref(), Some("new-session"));
        assert!(!updated.resumable);
        assert_eq!(updated.interrupted_at, None);
    }

    #[tokio::test]
    async fn create_mission_marks_user_metadata_source_when_title_is_provided() {
        let store = InMemoryMissionStore::new();
        let titled = store
            .create_mission(
                Some("User titled mission"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create titled mission");
        assert_eq!(titled.metadata_source.as_deref(), Some("user"));
        assert!(
            titled.metadata_updated_at.is_some(),
            "titled mission should set metadata_updated_at"
        );

        let untitled = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create untitled mission");
        assert_eq!(untitled.metadata_source, None);
        assert_eq!(untitled.metadata_updated_at, None);

        let blank_titled = store
            .create_mission(Some("   "), None, None, None, None, None, None)
            .await
            .expect("create blank titled mission");
        assert_eq!(blank_titled.metadata_source, None);
        assert_eq!(blank_titled.metadata_updated_at, None);
    }
}
