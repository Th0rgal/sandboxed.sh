//! Mission control tool - allows the agent to complete or fail the current mission.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use super::Tool;

/// Command sent by the mission tool to the control session.
#[derive(Debug, Clone)]
pub enum MissionControlCommand {
    SetStatus {
        status: MissionStatusValue,
        summary: Option<String>,
    },
}

/// Mission status values (mirrors api::control::MissionStatus but simplified for tool use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionStatusValue {
    /// Task was fully completed with real deliverables
    Completed,
    /// Task failed due to errors during execution
    Failed,
    /// Task cannot be completed due to blockers (type mismatch, access issues, etc.)
    Blocked,
    /// Task is not feasible as specified (wrong assumptions in request)
    NotFeasible,
}

impl std::fmt::Display for MissionStatusValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Blocked => write!(f, "blocked"),
            Self::NotFeasible => write!(f, "not_feasible"),
        }
    }
}

/// Shared state for mission control, passed to the tool.
#[derive(Clone)]
pub struct MissionControl {
    pub current_mission_id: Arc<RwLock<Option<Uuid>>>,
    pub cmd_tx: mpsc::Sender<MissionControlCommand>,
}

/// Tool that allows the agent to mark the current mission as completed or failed.
pub struct CompleteMission {
    pub control: Option<MissionControl>,
}

impl CompleteMission {
    pub fn new() -> Self {
        Self { control: None }
    }

    pub fn with_control(control: MissionControl) -> Self {
        Self {
            control: Some(control),
        }
    }
}

impl Default for CompleteMission {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct CompleteMissionArgs {
    /// Status: "completed", "failed", "blocked", or "not_feasible"
    status: String,
    /// Summary explaining the outcome (required for blocked/not_feasible)
    summary: Option<String>,
    /// Type of blocker (for blocked status)
    blocker_type: Option<String>,
    /// List of approaches attempted before giving up
    attempted: Option<Vec<String>>,
}

#[async_trait]
impl Tool for CompleteMission {
    fn name(&self) -> &str {
        "complete_mission"
    }

    fn description(&self) -> &str {
        r#"Mark the current mission status. Use the appropriate status:
- 'completed': Task fully done with REAL deliverables (not examples/placeholders)
- 'failed': Errors occurred during execution  
- 'blocked': Cannot proceed due to blockers (wrong project type, access denied, etc.)
- 'not_feasible': Task cannot be done as specified (wrong assumptions in request)

IMPORTANT: Use 'blocked' or 'not_feasible' instead of producing fake/placeholder content!"#
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed", "blocked", "not_feasible"],
                    "description": "The mission status:\n- 'completed': Goal achieved with real deliverables\n- 'failed': Errors during execution\n- 'blocked': Cannot proceed (type mismatch, access issues)\n- 'not_feasible': Task impossible as specified"
                },
                "summary": {
                    "type": "string",
                    "description": "REQUIRED for blocked/not_feasible. Explain what blocked you or why it's not feasible. For completed, summarize what was done."
                },
                "blocker_type": {
                    "type": "string",
                    "enum": ["type_mismatch", "access_denied", "resource_unavailable", "tool_failure", "other"],
                    "description": "For blocked status: what kind of blocker was encountered"
                },
                "attempted": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "For blocked/not_feasible: list of approaches you tried before giving up"
                }
            },
            "required": ["status"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let args: CompleteMissionArgs = serde_json::from_value(args)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        let status = match args.status.to_lowercase().as_str() {
            "completed" => MissionStatusValue::Completed,
            "failed" => MissionStatusValue::Failed,
            "blocked" => MissionStatusValue::Blocked,
            "not_feasible" => MissionStatusValue::NotFeasible,
            other => {
                return Err(anyhow::anyhow!(
                "Invalid status '{}'. Must be 'completed', 'failed', 'blocked', or 'not_feasible'.",
                other
            ))
            }
        };

        let Some(control) = &self.control else {
            return Ok("Mission control not available in this context. The mission status was not changed.".to_string());
        };

        // Check if there's a current mission
        let mission_id = *control.current_mission_id.read().await;
        if mission_id.is_none() {
            return Ok("No active mission to complete. Start a mission first.".to_string());
        }

        // For blocked/not_feasible, require a summary explaining why
        if (matches!(
            status,
            MissionStatusValue::Blocked | MissionStatusValue::NotFeasible
        )) && args.summary.is_none()
        {
            return Ok(format!(
                "⚠️ A summary is required when marking a mission as '{}'. \n\
                Please call complete_mission again with a summary explaining:\n\
                - What blocked you or why the task isn't feasible\n\
                - What approaches you tried\n\
                - What would be needed to proceed",
                status
            ));
        }

        // Validate completion: check if output folder has any files
        if status == MissionStatusValue::Completed {
            let output_dir = working_dir.join("output");
            let output_empty = if output_dir.exists() {
                std::fs::read_dir(&output_dir)
                    .map(|mut entries| entries.next().is_none())
                    .unwrap_or(true)
            } else {
                true
            };

            if output_empty {
                // Return a soft warning - don't block, but encourage the agent to continue
                tracing::warn!("complete_mission called with empty output folder");
                return Ok(
                    "⚠️ WARNING: The output/ folder is empty. You haven't created any deliverables yet.\n\n\
                    Before completing the mission, please:\n\
                    1. Create the requested files in output/\n\
                    2. Verify the deliverables exist\n\
                    3. Then call complete_mission again\n\n\
                    If this task genuinely produces no files, call complete_mission with a summary explaining why.\n\n\
                    If you encountered a BLOCKER (wrong project type, access denied, etc.), use:\n\
                    complete_mission(status='blocked', summary='explanation of what blocked you')".to_string()
                );
            }
        }

        // Build enhanced summary for blocked/not_feasible
        let enhanced_summary = if matches!(
            status,
            MissionStatusValue::Blocked | MissionStatusValue::NotFeasible
        ) {
            let mut parts = vec![];
            if let Some(ref summary) = args.summary {
                parts.push(summary.clone());
            }
            if let Some(ref blocker_type) = args.blocker_type {
                parts.push(format!("Blocker type: {}", blocker_type));
            }
            if let Some(ref attempted) = args.attempted {
                if !attempted.is_empty() {
                    parts.push(format!("Attempted: {}", attempted.join(", ")));
                }
            }
            Some(parts.join("\n"))
        } else {
            args.summary.clone()
        };

        // Log blocked/not_feasible status clearly
        if matches!(
            status,
            MissionStatusValue::Blocked | MissionStatusValue::NotFeasible
        ) {
            tracing::warn!(
                "Mission marked as {} - {}",
                status,
                enhanced_summary.as_deref().unwrap_or("no summary")
            );
        }

        // Send the command
        control
            .cmd_tx
            .send(MissionControlCommand::SetStatus {
                status,
                summary: enhanced_summary.clone(),
            })
            .await
            .map_err(|_| anyhow::anyhow!("Failed to send mission control command"))?;

        let summary_msg = enhanced_summary
            .map(|s| format!(" Summary: {}", s))
            .unwrap_or_default();

        Ok(format!("Mission marked as {}.{}", status, summary_msg))
    }
}
