//! Agent execution context - shared state across the agent runtime.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::tools::mission::MissionControl;

/// Shared context passed to all agents during execution.
///
/// # System Access
/// The agent has **full system access** - it can read/write any file, execute any command,
/// and search anywhere on the machine. The `working_dir` is just the default for relative paths.
pub struct AgentContext {
    /// Application configuration
    pub config: Config,

    /// Default working directory for relative paths.
    pub working_dir: PathBuf,

    /// Maximum iterations per agent
    pub max_iterations: usize,

    /// Optional event sink for streaming agent events (e.g. control session SSE).
    pub control_events: Option<broadcast::Sender<crate::api::control::AgentEvent>>,

    /// Optional hub for awaiting frontend (interactive) tool results.
    pub frontend_tool_hub: Option<Arc<crate::api::control::FrontendToolHub>>,

    /// Optional shared control-session status (so the executor can switch to WaitingForTool).
    pub control_status: Option<Arc<tokio::sync::RwLock<crate::api::control::ControlStatus>>>,

    /// Optional cancellation token for cooperative cancellation.
    pub cancel_token: Option<CancellationToken>,

    /// Mission control for allowing the agent to complete/fail missions.
    pub mission_control: Option<MissionControl>,

    /// Snapshot of current agent tree (for refresh resilience on frontend).
    pub tree_snapshot: Option<Arc<tokio::sync::RwLock<Option<crate::api::control::AgentTreeNode>>>>,

    /// Current execution progress (for progress indicator).
    pub progress_snapshot: Option<Arc<tokio::sync::RwLock<crate::api::control::ExecutionProgress>>>,

    /// Mission ID for tagging events (used in parallel mission execution).
    pub mission_id: Option<Uuid>,

    /// MCP registry for dynamic tool discovery and execution.
    pub mcp: Option<Arc<McpRegistry>>,
}

impl AgentContext {
    /// Create a new agent context.
    pub fn new(config: Config, working_dir: PathBuf) -> Self {
        Self {
            max_iterations: config.max_iterations,
            config,
            working_dir,
            control_events: None,
            frontend_tool_hub: None,
            control_status: None,
            cancel_token: None,
            mission_control: None,
            tree_snapshot: None,
            progress_snapshot: None,
            mission_id: None,
            mcp: None,
        }
    }

    /// Create a child context for delegated work.
    pub fn child_context(&self) -> Self {
        Self {
            config: self.config.clone(),
            working_dir: self.working_dir.clone(),
            max_iterations: self.max_iterations,
            control_events: self.control_events.clone(),
            frontend_tool_hub: self.frontend_tool_hub.clone(),
            control_status: self.control_status.clone(),
            cancel_token: self.cancel_token.clone(),
            mission_control: self.mission_control.clone(),
            tree_snapshot: self.tree_snapshot.clone(),
            progress_snapshot: self.progress_snapshot.clone(),
            mission_id: self.mission_id,
            mcp: self.mcp.clone(),
        }
    }

    /// Get the working directory path as a string.
    pub fn working_dir_str(&self) -> String {
        self.working_dir.to_string_lossy().to_string()
    }

    /// Check if cooperative cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token
            .as_ref()
            .map(|t| t.is_cancelled())
            .unwrap_or(false)
    }

    /// Emit an agent phase event (for UI feedback during preparation).
    pub fn emit_phase(&self, phase: &str, detail: Option<&str>, agent: Option<&str>) {
        if let Some(ref events) = self.control_events {
            let _ = events.send(crate::api::control::AgentEvent::AgentPhase {
                phase: phase.to_string(),
                detail: detail.map(|s| s.to_string()),
                agent: agent.map(|s| s.to_string()),
                mission_id: self.mission_id,
            });
        }
    }

    /// Emit an agent tree update event (for real-time tree visualization).
    /// Also saves the tree to the snapshot for refresh resilience.
    pub fn emit_tree(&self, tree: crate::api::control::AgentTreeNode) {
        if let Some(ref snapshot) = self.tree_snapshot {
            let tree_clone = tree.clone();
            let snapshot = Arc::clone(snapshot);
            tokio::spawn(async move {
                *snapshot.write().await = Some(tree_clone);
            });
        }

        if let Some(ref events) = self.control_events {
            let _ = events.send(crate::api::control::AgentEvent::AgentTree {
                tree,
                mission_id: self.mission_id,
            });
        }
    }

    /// Update execution progress and emit event.
    pub fn emit_progress(
        &self,
        total: usize,
        completed: usize,
        current: Option<String>,
        depth: u8,
    ) {
        let current_for_snapshot = current.clone();
        let current_for_event = current;

        if let Some(ref snapshot) = self.progress_snapshot {
            let snapshot = Arc::clone(snapshot);
            tokio::spawn(async move {
                let mut p = snapshot.write().await;
                p.total_subtasks = total;
                p.completed_subtasks = completed;
                p.current_subtask = current_for_snapshot;
                p.current_depth = depth;
            });
        }

        if let Some(ref events) = self.control_events {
            let _ = events.send(crate::api::control::AgentEvent::Progress {
                total_subtasks: total,
                completed_subtasks: completed,
                current_subtask: current_for_event,
                depth,
                mission_id: self.mission_id,
            });
        }
    }
}
