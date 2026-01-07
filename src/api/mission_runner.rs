//! Mission Runner - Isolated execution context for a single mission.
//!
//! This module provides a clean abstraction for running missions in parallel.
//! Each MissionRunner manages its own:
//! - Conversation history
//! - Message queue  
//! - Execution state
//! - Cancellation token
//! - Deliverable tracking
//! - Health monitoring
//! - Working directory (isolated per mission)

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentContext, AgentRef, AgentResult};
use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::task::{extract_deliverables, DeliverableSet};
use crate::workspace;

use super::control::{
    AgentEvent, AgentTreeNode, ControlStatus, ExecutionProgress, FrontendToolHub,
};

/// State of a running mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionRunState {
    /// Waiting in queue
    Queued,
    /// Currently executing
    Running,
    /// Waiting for frontend tool input
    WaitingForTool,
    /// Finished (check result)
    Finished,
}

/// Health status of a mission.
#[derive(Debug, Clone, serde::Serialize)]
pub enum MissionHealth {
    /// Mission is progressing normally
    Healthy,
    /// Mission may be stalled
    Stalled {
        seconds_since_activity: u64,
        last_state: String,
    },
    /// Mission completed without deliverables
    MissingDeliverables { missing: Vec<String> },
    /// Mission ended unexpectedly
    UnexpectedEnd { reason: String },
}

/// A message queued for this mission.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    pub model_override: Option<String>,
}

/// Isolated runner for a single mission.
pub struct MissionRunner {
    /// Mission ID
    pub mission_id: Uuid,

    /// Model override for this mission (if any)
    pub model_override: Option<String>,

    /// Workspace ID where this mission should run
    pub workspace_id: Uuid,

    /// Current state
    pub state: MissionRunState,

    /// Message queue for this mission
    pub queue: VecDeque<QueuedMessage>,

    /// Conversation history: (role, content)
    pub history: Vec<(String, String)>,

    /// Cancellation token for the current execution
    pub cancel_token: Option<CancellationToken>,

    /// Running task handle
    running_handle: Option<tokio::task::JoinHandle<(Uuid, String, AgentResult)>>,

    /// Tree snapshot for this mission
    pub tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,

    /// Progress snapshot for this mission
    pub progress_snapshot: Arc<RwLock<ExecutionProgress>>,

    /// Expected deliverables extracted from the initial message
    pub deliverables: DeliverableSet,

    /// Last activity timestamp for health monitoring
    pub last_activity: Instant,

    /// Whether complete_mission was explicitly called
    pub explicitly_completed: bool,
}

impl MissionRunner {
    /// Create a new mission runner.
    pub fn new(mission_id: Uuid, model_override: Option<String>, workspace_id: Uuid) -> Self {
        Self {
            mission_id,
            model_override,
            workspace_id,
            state: MissionRunState::Queued,
            queue: VecDeque::new(),
            history: Vec::new(),
            cancel_token: None,
            running_handle: None,
            tree_snapshot: Arc::new(RwLock::new(None)),
            progress_snapshot: Arc::new(RwLock::new(ExecutionProgress::default())),
            deliverables: DeliverableSet::default(),
            last_activity: Instant::now(),
            explicitly_completed: false,
        }
    }

    /// Check if this runner is currently executing.
    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            MissionRunState::Running | MissionRunState::WaitingForTool
        )
    }

    /// Check if this runner has finished.
    pub fn is_finished(&self) -> bool {
        matches!(self.state, MissionRunState::Finished)
    }

    /// Update the last activity timestamp.
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check the health of this mission.
    pub async fn check_health(&self) -> MissionHealth {
        let seconds_since = self.last_activity.elapsed().as_secs();

        // If running and no activity for 60+ seconds, consider stalled
        if self.is_running() && seconds_since > 60 {
            return MissionHealth::Stalled {
                seconds_since_activity: seconds_since,
                last_state: format!("{:?}", self.state),
            };
        }

        // If finished without explicit completion and has deliverables, check them
        if !self.is_running()
            && !self.explicitly_completed
            && !self.deliverables.deliverables.is_empty()
        {
            let missing = self.deliverables.missing_paths().await;
            if !missing.is_empty() {
                return MissionHealth::MissingDeliverables { missing };
            }
        }

        MissionHealth::Healthy
    }

    /// Extract deliverables from initial mission message.
    pub fn set_initial_message(&mut self, message: &str) {
        self.deliverables = extract_deliverables(message);
        if !self.deliverables.deliverables.is_empty() {
            tracing::info!(
                "Mission {} has {} expected deliverables: {:?}",
                self.mission_id,
                self.deliverables.deliverables.len(),
                self.deliverables
                    .deliverables
                    .iter()
                    .filter_map(|d| d.path())
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Queue a message for this mission.
    pub fn queue_message(&mut self, id: Uuid, content: String, model_override: Option<String>) {
        self.queue.push_back(QueuedMessage {
            id,
            content,
            model_override: model_override.or_else(|| self.model_override.clone()),
        });
    }

    /// Cancel the current execution.
    pub fn cancel(&mut self) {
        if let Some(token) = &self.cancel_token {
            token.cancel();
        }
    }

    /// Start executing the next queued message (if any and not already running).
    /// Returns true if execution was started.
    pub fn start_next(
        &mut self,
        config: Config,
        root_agent: AgentRef,
        mcp: Arc<McpRegistry>,
        workspaces: workspace::SharedWorkspaceStore,
        events_tx: broadcast::Sender<AgentEvent>,
        tool_hub: Arc<FrontendToolHub>,
        status: Arc<RwLock<ControlStatus>>,
        mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
        current_mission: Arc<RwLock<Option<Uuid>>>,
    ) -> bool {
        // Don't start if already running
        if self.is_running() {
            return false;
        }

        // Get next message from queue
        let msg = match self.queue.pop_front() {
            Some(m) => m,
            None => return false,
        };

        self.state = MissionRunState::Running;

        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());

        let hist_snapshot = self.history.clone();
        let tree_ref = Arc::clone(&self.tree_snapshot);
        let progress_ref = Arc::clone(&self.progress_snapshot);
        let mission_id = self.mission_id;
        let workspace_id = self.workspace_id;
        let model_override = msg.model_override;
        let user_message = msg.content.clone();
        let msg_id = msg.id;

        // Create mission control for complete_mission tool
        let mission_ctrl = crate::tools::mission::MissionControl {
            current_mission_id: current_mission,
            cmd_tx: mission_cmd_tx,
        };

        // Emit user message event with mission context
        let _ = events_tx.send(AgentEvent::UserMessage {
            id: msg_id,
            content: user_message.clone(),
            mission_id: Some(mission_id),
        });

        let handle = tokio::spawn(async move {
            let result = run_mission_turn(
                config,
                root_agent,
                mcp,
                workspaces,
                events_tx,
                tool_hub,
                status,
                cancel,
                hist_snapshot,
                user_message.clone(),
                model_override,
                Some(mission_ctrl),
                tree_ref,
                progress_ref,
                mission_id,
                Some(workspace_id),
            )
            .await;
            (msg_id, user_message, result)
        });

        self.running_handle = Some(handle);
        true
    }

    /// Poll for completion. Returns Some(result) if finished.
    pub async fn poll_completion(&mut self) -> Option<(Uuid, String, AgentResult)> {
        let handle = self.running_handle.take()?;

        // Check if handle is finished
        if handle.is_finished() {
            match handle.await {
                Ok(result) => {
                    self.touch(); // Update last activity
                    self.state = MissionRunState::Queued; // Ready for next message

                    // Check if complete_mission was called
                    if result.2.output.contains("Mission marked as")
                        || result.2.output.contains("complete_mission")
                    {
                        self.explicitly_completed = true;
                    }

                    // Add to history
                    self.history.push(("user".to_string(), result.1.clone()));
                    self.history
                        .push(("assistant".to_string(), result.2.output.clone()));

                    // Log warning if deliverables are missing and task ended
                    if !self.explicitly_completed && !self.deliverables.deliverables.is_empty() {
                        let missing = self.deliverables.missing_paths().await;
                        if !missing.is_empty() {
                            tracing::warn!(
                                "Mission {} ended but deliverables are missing: {:?}",
                                self.mission_id,
                                missing
                            );
                        }
                    }

                    Some(result)
                }
                Err(e) => {
                    tracing::error!("Mission runner task failed: {}", e);
                    self.state = MissionRunState::Finished;
                    None
                }
            }
        } else {
            // Not finished, put handle back
            self.running_handle = Some(handle);
            None
        }
    }

    /// Check if the running task is finished (non-blocking).
    pub fn check_finished(&self) -> bool {
        self.running_handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true)
    }
}

/// Build a history context string from conversation history.
fn build_history_context(history: &[(String, String)], max_chars: usize) -> String {
    let mut result = String::new();
    let mut total_chars = 0;
    for (role, content) in history.iter().rev() {
        let entry = format!("{}: {}\n\n", role.to_uppercase(), content);
        if total_chars + entry.len() > max_chars && !result.is_empty() {
            break;
        }
        result = format!("{}{}", entry, result);
        total_chars += entry.len();
    }
    result
}

/// Execute a single turn for a mission.
async fn run_mission_turn(
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    model_override: Option<String>,
    mission_control: Option<crate::tools::mission::MissionControl>,
    tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,
    progress_snapshot: Arc<RwLock<ExecutionProgress>>,
    mission_id: Uuid,
    workspace_id: Option<Uuid>,
) -> AgentResult {
    // Build context with history
    let max_history_chars = config.context.max_history_total_chars;
    let history_context = build_history_context(&history, max_history_chars);

    // Extract deliverables to include in instructions
    let deliverable_set = extract_deliverables(&user_message);
    let deliverable_reminder = if !deliverable_set.deliverables.is_empty() {
        let paths: Vec<String> = deliverable_set
            .deliverables
            .iter()
            .filter_map(|d| d.path())
            .map(|p| p.display().to_string())
            .collect();
        format!(
            "\n\n**REQUIRED DELIVERABLES** (do not stop until these exist):\n{}\n",
            paths
                .iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        String::new()
    };

    let is_multi_step = deliverable_set.is_research_task
        || deliverable_set.requires_report
        || user_message.contains("1.")
        || user_message.contains("- ")
        || user_message.to_lowercase().contains("then");

    let multi_step_instructions = if is_multi_step {
        r#"

**MULTI-STEP TASK RULES:**
- This task has multiple steps. Complete ALL steps before stopping.
- After each tool call, ask yourself: "Have I completed the FULL goal?"
- DO NOT stop after just one step - keep working until ALL deliverables exist.
- If you made progress but aren't done, continue in the same turn.
- Only call complete_mission when ALL requested outputs have been created."#
    } else {
        ""
    };

    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str(&deliverable_reminder);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- Use available tools to gather information or make changes.\n- For large data processing tasks (>10KB), prefer executing scripts rather than inline processing.\n- USE information already provided in the message - do not ask for URLs, paths, or details that were already given.\n- When you have fully completed the user's goal or determined it cannot be completed, state that clearly in your final response.");
    convo.push_str(multi_step_instructions);
    convo.push_str("\n");

    let mut task = match crate::task::Task::new(convo, Some(1000)) {
        Ok(t) => t,
        Err(e) => {
            return AgentResult::failure(format!("Failed to create task: {}", e), 0);
        }
    };

    // Apply model override if specified
    if let Some(model) = model_override {
        tracing::info!("Mission using model override: {}", model);
        task.analysis_mut().requested_model = Some(model);
    }

    // Ensure mission workspace exists and is configured for OpenCode.
    let workspace_root =
        workspace::resolve_workspace_root(&workspaces, &config, workspace_id).await;
    let mission_work_dir =
        match workspace::prepare_mission_workspace_in(&workspace_root, &mcp, mission_id).await {
            Ok(dir) => {
                tracing::info!(
                    "Mission {} workspace directory: {}",
                    mission_id,
                    dir.display()
                );
                dir
            }
            Err(e) => {
                tracing::warn!("Failed to prepare mission workspace, using default: {}", e);
                workspace_root
            }
        };

    let mut ctx = AgentContext::new(config.clone(), mission_work_dir);
    ctx.mission_control = mission_control;
    ctx.control_events = Some(events_tx);
    ctx.frontend_tool_hub = Some(tool_hub);
    ctx.control_status = Some(status);
    ctx.cancel_token = Some(cancel);
    ctx.tree_snapshot = Some(tree_snapshot);
    ctx.progress_snapshot = Some(progress_snapshot);
    ctx.mission_id = Some(mission_id);
    ctx.mcp = Some(mcp);

    root_agent.execute(&mut task, &ctx).await
}

/// Compact info about a running mission (for API responses).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningMissionInfo {
    pub mission_id: Uuid,
    pub model_override: Option<String>,
    pub state: String,
    pub queue_len: usize,
    pub history_len: usize,
    pub seconds_since_activity: u64,
    pub expected_deliverables: usize,
}

impl From<&MissionRunner> for RunningMissionInfo {
    fn from(runner: &MissionRunner) -> Self {
        Self {
            mission_id: runner.mission_id,
            model_override: runner.model_override.clone(),
            state: match runner.state {
                MissionRunState::Queued => "queued".to_string(),
                MissionRunState::Running => "running".to_string(),
                MissionRunState::WaitingForTool => "waiting_for_tool".to_string(),
                MissionRunState::Finished => "finished".to_string(),
            },
            queue_len: runner.queue.len(),
            history_len: runner.history.len(),
            seconds_since_activity: runner.last_activity.elapsed().as_secs(),
            expected_deliverables: runner.deliverables.deliverables.len(),
        }
    }
}
