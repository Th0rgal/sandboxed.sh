//! In-memory mission store (non-persistent).

use super::{
    now_string, BoardOutboxItem, BoardProject, BoardTask, BoardTaskStatus, Mission,
    MissionExecutionState, MissionHistoryEntry, MissionRun, MissionStatus, MissionStatusCounts,
    MissionStore, MissionToolExecution, MissionToolExecutionState, NewBoardTask, TaskAttempt,
};
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
    board_tasks: Arc<RwLock<HashMap<Uuid, BoardTask>>>,
    /// FLEET-001 scheduling: deferred goals held outside the Mission struct
    /// (mirrors the sqlite `deferred_goal` column).
    deferred_goals: Arc<RwLock<HashMap<Uuid, String>>>,
    runs: Arc<RwLock<HashMap<Uuid, MissionRun>>>,
    tool_executions: Arc<RwLock<HashMap<(Uuid, String), MissionToolExecution>>>,
    board_projects: Arc<RwLock<HashMap<String, BoardProject>>>,
    task_attempts: Arc<RwLock<HashMap<(Uuid, u32), TaskAttempt>>>,
    board_outbox: Arc<RwLock<HashMap<String, BoardOutboxItem>>>,
}

impl InMemoryMissionStore {
    pub fn new() -> Self {
        Self {
            missions: Arc::new(RwLock::new(HashMap::new())),
            trees: Arc::new(RwLock::new(HashMap::new())),
            board_tasks: Arc::new(RwLock::new(HashMap::new())),
            deferred_goals: Arc::new(RwLock::new(HashMap::new())),
            runs: Arc::new(RwLock::new(HashMap::new())),
            tool_executions: Arc::new(RwLock::new(HashMap::new())),
            board_projects: Arc::new(RwLock::new(HashMap::new())),
            task_attempts: Arc::new(RwLock::new(HashMap::new())),
            board_outbox: Arc::new(RwLock::new(HashMap::new())),
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

    async fn count_missions_by_status(&self) -> Result<MissionStatusCounts, String> {
        let missions = self.missions.read().await;
        let mut counts = MissionStatusCounts {
            total: missions.len(),
            ..MissionStatusCounts::default()
        };
        for mission in missions.values() {
            match mission.status {
                MissionStatus::Active => counts.active += 1,
                MissionStatus::Completed => counts.completed += 1,
                MissionStatus::Failed => counts.failed += 1,
                _ => {}
            }
        }
        Ok(counts)
    }

    async fn get_mission(&self, id: Uuid) -> Result<Option<Mission>, String> {
        Ok(self.missions.read().await.get(&id).cloned())
    }

    async fn begin_mission_run(
        &self,
        mission_id: Uuid,
        owner_actor_id: &str,
        scope_unit: Option<&str>,
    ) -> Result<MissionRun, String> {
        let missions = self.missions.read().await;
        match missions.get(&mission_id) {
            None => return Err(format!("mission not found: {mission_id}")),
            Some(mission) if mission.status == MissionStatus::Acknowledged => {
                return Err(format!(
                    "acknowledged mission {mission_id} cannot acquire a non-terminal run"
                ));
            }
            Some(mission)
                if !matches!(
                    mission.status,
                    MissionStatus::Pending | MissionStatus::Active
                ) =>
            {
                return Err(format!(
                    "mission {mission_id} has status {}; activate it before acquiring a non-terminal run",
                    mission.status
                ));
            }
            Some(_) => {}
        }
        let mut runs = self.runs.write().await;
        if let Some(run) = runs
            .values()
            .find(|run| run.mission_id == mission_id && !run.execution_state.is_terminal())
        {
            return Err(format!(
                "mission {mission_id} already has non-terminal run {} generation {}",
                run.run_id, run.generation
            ));
        }
        let generation = runs
            .values()
            .filter(|run| run.mission_id == mission_id)
            .map(|run| run.generation)
            .max()
            .unwrap_or(0)
            + 1;
        let now = now_string();
        let run = MissionRun {
            run_id: Uuid::new_v4(),
            mission_id,
            generation,
            execution_state: MissionExecutionState::Starting,
            owner_actor_id: owner_actor_id.to_string(),
            scope_unit: scope_unit.map(str::to_string),
            started_at: now.clone(),
            heartbeat_at: now,
            stopping_at: None,
            ended_at: None,
            terminal_reason: None,
        };
        runs.insert(run.run_id, run.clone());
        Ok(run)
    }

    async fn get_active_mission_run(&self, mission_id: Uuid) -> Result<Option<MissionRun>, String> {
        Ok(self
            .runs
            .read()
            .await
            .values()
            .find(|run| run.mission_id == mission_id && !run.execution_state.is_terminal())
            .cloned())
    }

    async fn list_active_mission_runs(&self) -> Result<Vec<MissionRun>, String> {
        Ok(self
            .runs
            .read()
            .await
            .values()
            .filter(|run| !run.execution_state.is_terminal())
            .cloned()
            .collect())
    }

    async fn heartbeat_mission_run(
        &self,
        run_id: Uuid,
        generation: u64,
        state: MissionExecutionState,
        scope_unit: Option<&str>,
    ) -> Result<bool, String> {
        let mut runs = self.runs.write().await;
        let Some(run) = runs.get_mut(&run_id) else {
            return Ok(false);
        };
        if run.generation != generation || run.execution_state.is_terminal() {
            return Ok(false);
        }
        run.execution_state = state;
        run.heartbeat_at = now_string();
        if let Some(scope) = scope_unit {
            run.scope_unit = Some(scope.to_string());
        }
        if state == MissionExecutionState::Stopping && run.stopping_at.is_none() {
            run.stopping_at = Some(run.heartbeat_at.clone());
        }
        Ok(true)
    }

    async fn finish_mission_run(
        &self,
        run_id: Uuid,
        generation: u64,
        terminal_reason: Option<&str>,
    ) -> Result<bool, String> {
        let mut runs = self.runs.write().await;
        let Some(run) = runs.get_mut(&run_id) else {
            return Ok(false);
        };
        if run.generation != generation || run.execution_state.is_terminal() {
            return Ok(false);
        }
        let now = now_string();
        run.execution_state = MissionExecutionState::Terminal;
        run.heartbeat_at = now.clone();
        run.ended_at = Some(now);
        run.terminal_reason = terminal_reason.map(str::to_string);
        Ok(true)
    }

    async fn register_tool_execution(
        &self,
        execution: &MissionToolExecution,
    ) -> Result<(), String> {
        self.tool_executions.write().await.insert(
            (execution.run_id, execution.tool_call_id.clone()),
            execution.clone(),
        );
        Ok(())
    }

    async fn finish_tool_execution(
        &self,
        run_id: Uuid,
        tool_call_id: &str,
        state: MissionToolExecutionState,
        exit_status: Option<i32>,
        failure_class: Option<&str>,
    ) -> Result<bool, String> {
        let mut tools = self.tool_executions.write().await;
        let Some(tool) = tools.get_mut(&(run_id, tool_call_id.to_string())) else {
            return Ok(false);
        };
        let now = now_string();
        tool.state = state;
        tool.heartbeat_at = now.clone();
        tool.completed_at = Some(now);
        tool.exit_status = exit_status;
        tool.failure_class = failure_class.map(str::to_string);
        Ok(true)
    }

    async fn list_active_tool_executions(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<MissionToolExecution>, String> {
        Ok(self
            .tool_executions
            .read()
            .await
            .values()
            .filter(|tool| {
                tool.run_id == run_id
                    && matches!(
                        tool.state,
                        MissionToolExecutionState::Provisional | MissionToolExecutionState::Running
                    )
            })
            .cloned()
            .collect())
    }

    async fn create_mission_with_parent(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        model_effort: Option<&str>,
        fast_mode: bool,
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
            fast_mode,
            backend: backend.unwrap_or("claudecode").to_string(),
            config_profile: config_profile.map(|s| s.to_string()),
            history: vec![],
            created_at: now.clone(),
            updated_at: now.clone(),
            interrupted_at: None,
            paused_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: Some(Uuid::new_v4().to_string()),
            terminal_reason: None,
            parent_mission_id,
            working_directory: working_directory.map(|s| s.to_string()),
            mission_mode: super::MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
            first_viewed_at: None,
            scheduling: Default::default(),
            project: Default::default(),
            activity: super::MissionActivity {
                last_status_change_at: Some(now.clone()),
                ..Default::default()
            },
            awaiting_kind: None,
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
        if status == MissionStatus::Acknowledged && self.get_active_mission_run(id).await?.is_some()
        {
            return Err(format!(
                "mission {id} cannot be acknowledged while a run is non-terminal"
            ));
        }
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        let status_changed = mission.status != status;
        mission.status = status;
        let now = now_string();
        mission.updated_at = now.clone();
        if status_changed {
            mission.activity.last_status_change_at = Some(now.clone());
        }
        mission.terminal_reason = terminal_reason.map(|s| s.to_string());
        // AwaitingUser is resumable too (any user message wakes the agent).
        // Failed missions with LlmError are also resumable (transient API errors).
        mission.resumable = matches!(
            status,
            MissionStatus::Interrupted
                | MissionStatus::Blocked
                | MissionStatus::Failed
                | MissionStatus::AwaitingUser
                | MissionStatus::Acknowledged
                | MissionStatus::WaitingBackground
        );
        mission.interrupted_at =
            if matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked) {
                Some(now)
            } else {
                None
            };
        if matches!(status, MissionStatus::Active) {
            mission.first_viewed_at = None;
        }
        // awaiting_kind only describes an AwaitingUser mission; clear it on any
        // change away from that state (parity with the sqlite store).
        if !matches!(status, MissionStatus::AwaitingUser) {
            mission.awaiting_kind = None;
        }
        Ok(())
    }

    async fn set_mission_first_viewed_at_if_unset(
        &self,
        id: Uuid,
        timestamp: &str,
    ) -> Result<Option<String>, String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        if mission.first_viewed_at.is_some() {
            return Ok(None);
        }
        mission.first_viewed_at = Some(timestamp.to_string());
        Ok(Some(timestamp.to_string()))
    }

    async fn acknowledge_stale_awaiting_user_missions(
        &self,
        grace_seconds: u64,
    ) -> Result<Vec<Uuid>, String> {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(grace_seconds as i64);
        let active: std::collections::HashSet<Uuid> = self
            .list_active_mission_runs()
            .await?
            .into_iter()
            .map(|run| run.mission_id)
            .collect();
        let mut missions = self.missions.write().await;
        let mut promoted = Vec::new();
        for mission in missions.values_mut() {
            if mission.status != MissionStatus::AwaitingUser {
                continue;
            }
            if active.contains(&mission.id) {
                continue;
            }
            let Some(ref viewed_at) = mission.first_viewed_at else {
                continue;
            };
            let Ok(viewed_dt) = chrono::DateTime::parse_from_rfc3339(viewed_at) else {
                continue;
            };
            if viewed_dt <= cutoff {
                mission.status = MissionStatus::Acknowledged;
                mission.updated_at = now_string();
                promoted.push(mission.id);
            }
        }
        Ok(promoted)
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

    async fn update_mission_goal(
        &self,
        id: Uuid,
        goal_mode: bool,
        goal_objective: Option<&str>,
    ) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.goal_mode = goal_mode;
        mission.goal_objective = goal_objective.map(|s| s.to_string());
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
        fast_mode: Option<bool>,
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
        if let Some(fast_mode) = fast_mode {
            mission.fast_mode = fast_mode;
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

    async fn update_mission_project(
        &self,
        id: Uuid,
        patch: super::MissionProjectPatch,
    ) -> Result<(), String> {
        if patch.is_empty() {
            return Ok(());
        }
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        if let Some(project) = patch.project {
            mission.project.project = project;
        }
        if let Some(track) = patch.track {
            mission.project.track = track;
        }
        if let Some(intent) = patch.intent {
            mission.project.intent = intent;
        }
        if let Some(github_pr) = patch.github_pr {
            mission.project.github_pr = github_pr;
        }
        if let Some(tags) = patch.tags {
            mission.project.tags = tags;
        }
        if let Some(desired_state) = patch.desired_state {
            mission.project.desired_state = desired_state;
        }
        if let Some(next_check_at) = patch.next_check_at {
            mission.project.next_check_at = next_check_at;
        }
        mission.updated_at = now_string();
        Ok(())
    }

    async fn set_mission_awaiting_kind(
        &self,
        id: Uuid,
        kind: Option<super::AwaitingKind>,
    ) -> Result<(), String> {
        let mut missions = self.missions.write().await;
        let mission = missions
            .get_mut(&id)
            .ok_or_else(|| format!("Mission {} not found", id))?;
        mission.awaiting_kind = kind;
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

    async fn set_deferred_goal(
        &self,
        mission_id: Uuid,
        goal: Option<String>,
    ) -> Result<(), String> {
        let mut goals = self.deferred_goals.write().await;
        match goal {
            Some(g) => {
                goals.insert(mission_id, g);
            }
            None => {
                goals.remove(&mission_id);
            }
        }
        Ok(())
    }

    async fn get_deferred_goal(&self, mission_id: Uuid) -> Result<Option<String>, String> {
        Ok(self.deferred_goals.read().await.get(&mission_id).cloned())
    }

    async fn set_mission_paused_at(
        &self,
        mission_id: Uuid,
        paused_at: Option<String>,
    ) -> Result<(), String> {
        if let Some(m) = self.missions.write().await.get_mut(&mission_id) {
            m.paused_at = paused_at;
        }
        Ok(())
    }

    async fn get_scheduled_pending_missions(&self) -> Result<Vec<Mission>, String> {
        let goals = self.deferred_goals.read().await;
        let mut missions: Vec<Mission> = self
            .missions
            .read()
            .await
            .values()
            .filter(|m| m.status == MissionStatus::Pending && goals.contains_key(&m.id))
            .cloned()
            .collect();
        missions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
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

    // ---- Task board ------------------------------------------------------

    async fn upsert_board_tasks(
        &self,
        boss_mission_id: Uuid,
        tasks: Vec<NewBoardTask>,
    ) -> Result<Vec<BoardTask>, String> {
        let mut map = self.board_tasks.write().await;
        let now = now_string();
        let mut out = Vec::with_capacity(tasks.len());
        for t in tasks {
            let existing_id = map
                .values()
                .find(|bt| bt.boss_mission_id == boss_mission_id && bt.task_key == t.task_key)
                .map(|bt| bt.id);
            match existing_id {
                Some(id) => {
                    let bt = map.get_mut(&id).expect("just found");
                    if bt.status == BoardTaskStatus::Pending {
                        bt.title = t.title;
                        bt.prompt = t.prompt;
                        bt.backend = t.backend;
                        bt.model_override = t.model_override;
                        bt.model_effort = t.model_effort;
                        bt.working_directory = t.working_directory;
                        bt.repository = t.repository;
                        bt.branch = t.branch;
                        bt.role = t.role;
                        bt.acceptance_criteria = t.acceptance_criteria;
                        bt.verification_command = t.verification_command;
                        bt.design_domain = t.design_domain;
                        bt.declared_write_set = t.declared_write_set;
                        bt.risk_class = t.risk_class;
                        bt.token_budget = t.token_budget;
                        bt.cost_budget_cents = t.cost_budget_cents;
                        bt.depends_on = t.depends_on;
                        bt.updated_at = now.clone();
                    } else if bt.status == BoardTaskStatus::Running {
                        // Mirror the sqlite store: a running task's prompt is
                        // frozen but its outcome contract may still be
                        // corrected (spec_warnings arrive after registration,
                        // and the scheduler can spawn within one pass).
                        bt.acceptance_criteria = t.acceptance_criteria;
                        bt.verification_command = t.verification_command;
                        bt.risk_class = t.risk_class;
                        bt.updated_at = now.clone();
                    }
                    out.push(bt.clone());
                }
                None => {
                    let bt = BoardTask {
                        id: Uuid::new_v4(),
                        boss_mission_id,
                        task_key: t.task_key,
                        title: t.title,
                        prompt: t.prompt,
                        backend: t.backend,
                        model_override: t.model_override,
                        model_effort: t.model_effort,
                        working_directory: t.working_directory,
                        repository: t.repository,
                        branch: t.branch,
                        role: t.role,
                        acceptance_criteria: t.acceptance_criteria,
                        verification_command: t.verification_command,
                        design_domain: t.design_domain,
                        declared_write_set: t.declared_write_set,
                        risk_class: t.risk_class,
                        token_budget: t.token_budget,
                        cost_budget_cents: t.cost_budget_cents,
                        depends_on: t.depends_on,
                        status: BoardTaskStatus::Pending,
                        outcome: None,
                        worker_mission_id: None,
                        prior_worker_mission_id: None,
                        prior_outcome: None,
                        prior_result_digest: None,
                        attempts: 0,
                        result_digest: None,
                        notes: None,
                        created_at: now.clone(),
                        updated_at: now.clone(),
                    };
                    map.insert(bt.id, bt.clone());
                    out.push(bt);
                }
            }
        }
        Ok(out)
    }

    async fn list_board_tasks(&self, boss_mission_id: Uuid) -> Result<Vec<BoardTask>, String> {
        let map = self.board_tasks.read().await;
        let mut tasks: Vec<BoardTask> = map
            .values()
            .filter(|bt| bt.boss_mission_id == boss_mission_id)
            .cloned()
            .collect();
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.task_key.cmp(&b.task_key))
        });
        Ok(tasks)
    }

    async fn list_active_board_missions(&self) -> Result<Vec<Uuid>, String> {
        let map = self.board_tasks.read().await;
        let mut ids: Vec<Uuid> = map
            .values()
            .filter(|bt| !bt.status.is_terminal())
            .map(|bt| bt.boss_mission_id)
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn get_board_task(&self, task_id: Uuid) -> Result<Option<BoardTask>, String> {
        Ok(self.board_tasks.read().await.get(&task_id).cloned())
    }

    async fn get_board_task_by_worker(
        &self,
        worker_mission_id: Uuid,
    ) -> Result<Option<BoardTask>, String> {
        let map = self.board_tasks.read().await;
        Ok(map
            .values()
            .filter(|bt| bt.worker_mission_id == Some(worker_mission_id))
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
            .cloned())
    }

    async fn save_board_task(&self, task: &BoardTask) -> Result<(), String> {
        let mut map = self.board_tasks.write().await;
        if !map.contains_key(&task.id) {
            return Err(format!("Board task {} not found", task.id));
        }
        let mut saved = task.clone();
        saved.updated_at = now_string();
        map.insert(task.id, saved);
        Ok(())
    }

    async fn upsert_board_project(
        &self,
        mut project: BoardProject,
    ) -> Result<BoardProject, String> {
        let mut projects = self.board_projects.write().await;
        if let Some(existing) = projects.get(&project.slug) {
            project.created_at = existing.created_at.clone();
        }
        project.updated_at = now_string();
        projects.insert(project.slug.clone(), project.clone());
        Ok(project)
    }

    async fn get_board_project(&self, slug: &str) -> Result<Option<BoardProject>, String> {
        Ok(self.board_projects.read().await.get(slug).cloned())
    }

    async fn create_task_attempt(&self, attempt: TaskAttempt) -> Result<TaskAttempt, String> {
        let mut attempts = self.task_attempts.write().await;
        Ok(attempts
            .entry((attempt.task_id, attempt.attempt_number))
            .or_insert(attempt)
            .clone())
    }

    async fn finish_task_attempt(
        &self,
        task_id: Uuid,
        attempt_number: u32,
        terminal_class: &str,
        commit_sha: Option<&str>,
        changed_files: &[String],
        verification_evidence: &serde_json::Value,
        cost_cents: Option<i64>,
    ) -> Result<(), String> {
        let mut attempts = self.task_attempts.write().await;
        let attempt = attempts
            .get_mut(&(task_id, attempt_number))
            .ok_or_else(|| "Task attempt not found".to_string())?;
        attempt.terminal_class = Some(terminal_class.to_string());
        attempt.commit_sha = commit_sha.map(str::to_string);
        attempt.changed_files = changed_files.to_vec();
        attempt.verification_evidence = verification_evidence.clone();
        attempt.cost_cents = cost_cents;
        attempt.finished_at = Some(now_string());
        Ok(())
    }

    async fn list_task_attempts(&self, task_id: Uuid) -> Result<Vec<TaskAttempt>, String> {
        let mut attempts: Vec<_> = self
            .task_attempts
            .read()
            .await
            .values()
            .filter(|attempt| attempt.task_id == task_id)
            .cloned()
            .collect();
        attempts.sort_by_key(|attempt| attempt.attempt_number);
        Ok(attempts)
    }

    async fn enqueue_board_outbox(&self, item: BoardOutboxItem) -> Result<BoardOutboxItem, String> {
        let mut outbox = self.board_outbox.write().await;
        Ok(outbox
            .entry(item.idempotency_key.clone())
            .or_insert(item)
            .clone())
    }

    async fn acknowledge_board_outbox(&self, idempotency_key: &str) -> Result<(), String> {
        if let Some(item) = self.board_outbox.write().await.get_mut(idempotency_key) {
            item.state = "acknowledged".to_string();
            item.acknowledged_at = Some(now_string());
        }
        Ok(())
    }

    async fn list_pending_board_outbox(
        &self,
        limit: usize,
    ) -> Result<Vec<BoardOutboxItem>, String> {
        let mut items: Vec<_> = self
            .board_outbox
            .read()
            .await
            .values()
            .filter(|item| item.state != "acknowledged")
            .cloned()
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items.truncate(limit);
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_background_is_never_ack_promoted() {
        // A mission parked in WaitingBackground has live background work; the
        // stale-ack sweep must skip it even after its view-grace elapses,
        // otherwise it would be archived while work is still running.
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("bg mission"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_status(mission.id, MissionStatus::WaitingBackground)
            .await
            .expect("set waiting_background");
        // Mark it viewed well in the past so the grace window is definitely
        // exceeded — the only thing that should save it is the status guard.
        let long_ago = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
        store
            .set_mission_first_viewed_at_if_unset(mission.id, &long_ago)
            .await
            .expect("set first viewed");

        let promoted = store
            .acknowledge_stale_awaiting_user_missions(0)
            .await
            .expect("run ack sweep");
        assert!(
            !promoted.contains(&mission.id),
            "waiting_background must not be ack-promoted"
        );

        let after = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(after.status, MissionStatus::WaitingBackground);

        let waiting = store
            .get_waiting_background_mission_ids()
            .await
            .expect("list waiting_background");
        assert!(waiting.contains(&mission.id));
    }

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
    async fn inactive_mission_cannot_acquire_a_run_until_reactivated() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Queued work"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_status(mission.id, MissionStatus::Interrupted)
            .await
            .expect("interrupt mission");

        assert!(store
            .begin_mission_run(mission.id, "actor", None)
            .await
            .unwrap_err()
            .contains("activate it before acquiring"));

        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("reactivate mission");
        store
            .begin_mission_run(mission.id, "actor", None)
            .await
            .expect("active mission can acquire run");
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
