//! In-memory mission store (non-persistent).

use super::{now_string, Mission, MissionHistoryEntry, MissionStatus, MissionStore};
use crate::api::control::{AgentTreeNode, DesktopSessionInfo};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

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

    async fn create_mission(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
    ) -> Result<Mission, String> {
        let now = now_string();
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Pending,
            title: title.map(|s| s.to_string()),
            workspace_id: workspace_id.unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
            workspace_name: None,
            agent: agent.map(|s| s.to_string()),
            model_override: model_override.map(|s| s.to_string()),
            backend: backend.unwrap_or("opencode").to_string(),
            config_profile: config_profile.map(|s| s.to_string()),
            history: vec![],
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: Some(Uuid::new_v4().to_string()),
            terminal_reason: None,
        };
        self.missions
            .write()
            .await
            .insert(mission.id, mission.clone());
        Ok(mission)
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
        mission.updated_at = now_string();
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
