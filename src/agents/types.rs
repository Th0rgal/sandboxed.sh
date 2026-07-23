//! Core types for the agent system.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(Uuid);

impl AgentId {
    /// Create a new unique agent ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create an agent ID from a string (for testing).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self(Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v4()))
    }
}

impl std::str::FromStr for AgentId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Type of agent in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentType {
    /// Root orchestrator (top of tree)
    Root,
    /// Worker agent (delegated execution)
    Worker,
}

impl AgentType {
    /// Check if this is an orchestrator type (can have children).
    pub fn is_orchestrator(&self) -> bool {
        matches!(self, Self::Root)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    Actual,
    Estimated,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSignal {
    NativeTerminal,
    SessionIdle,
    ProcessExit,
    TextFallback,
    RecoveredSoftError,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    AgentError,
    AuthError,
    CapacityLimited,
    ProviderError,
    RateLimited,
    TransportError,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionEvidence {
    pub terminal_reason: Option<TerminalReason>,
    pub completion_signal: CompletionSignal,
    pub completion_confidence: CompletionConfidence,
    pub native_terminal_seen: bool,
    pub pending_tools: Option<usize>,
    pub transport_failure_stage: Option<String>,
    pub provider_error_source: Option<String>,
    pub failure_class: Option<FailureClass>,
    pub classification_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Complete {
        signal: CompletionSignal,
        confidence: CompletionConfidence,
        message: Option<String>,
    },
    Failed {
        reason: TerminalReason,
        source: Option<FailureClass>,
        message: Option<String>,
    },
    Interrupted {
        reason: TerminalReason,
        message: Option<String>,
    },
}

impl TurnOutcome {
    pub fn terminal_reason(&self) -> TerminalReason {
        match self {
            Self::Complete { .. } => TerminalReason::TurnComplete,
            Self::Failed { reason, .. } | Self::Interrupted { reason, .. } => *reason,
        }
    }

    pub fn completion_signal(&self) -> CompletionSignal {
        match self {
            Self::Complete { signal, .. } => *signal,
            Self::Failed { .. } | Self::Interrupted { .. } => CompletionSignal::ProcessExit,
        }
    }

    pub fn completion_confidence(&self) -> CompletionConfidence {
        match self {
            Self::Complete { confidence, .. } => *confidence,
            Self::Failed { .. } | Self::Interrupted { .. } => CompletionConfidence::High,
        }
    }

    pub fn failure_class(&self) -> Option<FailureClass> {
        match self {
            Self::Complete { .. } => None,
            Self::Failed { source, .. } => *source,
            Self::Interrupted { .. } => Some(FailureClass::AgentError),
        }
    }
}

/// Result of an agent executing a task.
///
/// # Invariants
/// - If `success == true`, the task was completed
/// - `cost_cents` reflects actual cost incurred (if known)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// Whether the task was successful
    pub success: bool,

    /// Output or response from the agent
    pub output: String,

    /// Cost incurred in cents
    pub cost_cents: u64,

    /// Cost source provenance
    pub cost_source: CostSource,

    /// Token usage when available
    pub usage: Option<crate::cost::TokenUsage>,

    /// Model used (if any)
    pub model_used: Option<String>,

    /// Detailed result data (type-specific)
    pub data: Option<serde_json::Value>,

    /// Reason why execution terminated (if not successful completion)
    pub terminal_reason: Option<TerminalReason>,
}

impl AgentResult {
    /// Create a successful result.
    pub fn success(output: impl Into<String>, cost_cents: u64) -> Self {
        Self {
            success: true,
            output: output.into(),
            cost_cents,
            cost_source: CostSource::Unknown,
            usage: None,
            model_used: None,
            data: None,
            terminal_reason: None,
        }
    }

    /// Create a failure result.
    pub fn failure(error: impl Into<String>, cost_cents: u64) -> Self {
        Self {
            success: false,
            output: error.into(),
            cost_cents,
            cost_source: CostSource::Unknown,
            usage: None,
            model_used: None,
            data: None,
            terminal_reason: None,
        }
    }

    /// Add model information to the result.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model_used = Some(model.into());
        self
    }

    /// Add additional data to the result.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        match (self.data.as_mut(), data) {
            (Some(serde_json::Value::Object(existing)), serde_json::Value::Object(additional)) => {
                existing.extend(additional);
            }
            (_, replacement) => self.data = Some(replacement),
        }
        self
    }

    /// Attach typed turn-outcome metadata while preserving existing backend
    /// diagnostics in `data`.
    pub fn with_turn_outcome(mut self, outcome: TurnOutcome) -> Self {
        self.terminal_reason = Some(outcome.terminal_reason());
        let data = self.data.get_or_insert_with(|| serde_json::json!({}));
        if !data.is_object() {
            *data = serde_json::json!({});
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("turn_outcome".to_string(), serde_json::json!(outcome));
            obj.insert(
                "completion_signal".to_string(),
                serde_json::json!(outcome.completion_signal()),
            );
            obj.insert(
                "completion_confidence".to_string(),
                serde_json::json!(outcome.completion_confidence()),
            );
            obj.insert(
                "native_terminal_seen".to_string(),
                serde_json::json!(matches!(
                    outcome.completion_signal(),
                    CompletionSignal::NativeTerminal
                )),
            );
            obj.insert(
                "failure_class".to_string(),
                serde_json::json!(outcome.failure_class()),
            );
            obj.insert(
                "classification_source".to_string(),
                serde_json::json!(match outcome.completion_signal() {
                    CompletionSignal::TextFallback | CompletionSignal::RecoveredSoftError =>
                        "text_fallback",
                    CompletionSignal::Unknown => "unknown",
                    _ => "structured",
                }),
            );
        }
        self
    }

    /// Add usage information.
    pub fn with_usage(mut self, usage: crate::cost::TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Add cost source metadata.
    pub fn with_cost_source(mut self, source: CostSource) -> Self {
        self.cost_source = source;
        self
    }

    /// Add terminal reason to the result.
    pub fn with_terminal_reason(mut self, reason: TerminalReason) -> Self {
        self.terminal_reason = Some(reason);
        self
    }
}

/// Reason why agent execution terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalReason {
    /// A single assistant turn ended successfully, but mission may continue
    TurnComplete,
    /// Task completed successfully
    Completed,
    /// Task was cancelled by user
    Cancelled,
    /// Mission was interrupted because the server is shutting down
    /// (SIGTERM, deploy, package upgrade). Distinct from `Cancelled`
    /// so the UI can show a "paused, click Resume" affordance instead
    /// of the user-initiated cancel wording.
    ServerShutdown,
    /// LLM/OpenCode API error
    LlmError,
    /// Agent stalled (no progress)
    Stalled,
    /// Detected infinite loop
    InfiniteLoop,
    /// Hit maximum iterations limit
    MaxIterations,
    /// Provider rate-limited all retry attempts
    RateLimited,
    /// Provider rejected turn due to concurrent mission capacity exhaustion
    CapacityLimited,
    /// Authentication credentials were rejected (expired/revoked token)
    AuthError,
}

/// Errors that can occur in agent operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("Task error: {0}")]
    TaskError(String),

    #[error("No capable agent found for task")]
    NoCapableAgent,

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("Tool error: {0}")]
    ToolError(String),

    #[error("Max iterations reached: {0}")]
    MaxIterations(usize),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<crate::task::TaskError> for AgentError {
    fn from(e: crate::task::TaskError) -> Self {
        Self::TaskError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_outcome_metadata_preserves_existing_backend_data() {
        let result = AgentResult::success("done", 0)
            .with_data(serde_json::json!({ "backend": "codex" }))
            .with_turn_outcome(TurnOutcome::Complete {
                signal: CompletionSignal::NativeTerminal,
                confidence: CompletionConfidence::High,
                message: None,
            });

        let data = result.data.expect("metadata");
        assert_eq!(result.terminal_reason, Some(TerminalReason::TurnComplete));
        assert_eq!(data["backend"], "codex");
        assert_eq!(data["completion_signal"], "native_terminal");
        assert_eq!(data["completion_confidence"], "high");
        assert_eq!(data["native_terminal_seen"], true);
        assert_eq!(data["classification_source"], "structured");
        assert_eq!(data["turn_outcome"]["outcome"], "complete");
    }

    #[test]
    fn additional_object_data_preserves_turn_outcome_metadata() {
        let result = AgentResult::success("done", 0)
            .with_turn_outcome(TurnOutcome::Complete {
                signal: CompletionSignal::NativeTerminal,
                confidence: CompletionConfidence::High,
                message: None,
            })
            .with_data(serde_json::json!({
                "transport_failure_stage": "pending_tools"
            }));

        let data = result.data.expect("metadata");
        assert_eq!(data["turn_outcome"]["outcome"], "complete");
        assert_eq!(data["completion_signal"], "native_terminal");
        assert_eq!(data["transport_failure_stage"], "pending_tools");
    }

    #[test]
    fn failed_turn_outcome_metadata_sets_failure_class() {
        let result =
            AgentResult::failure("rate limited", 0).with_turn_outcome(TurnOutcome::Failed {
                reason: TerminalReason::RateLimited,
                source: Some(FailureClass::RateLimited),
                message: None,
            });

        let data = result.data.expect("metadata");
        assert_eq!(result.terminal_reason, Some(TerminalReason::RateLimited));
        assert_eq!(data["completion_signal"], "process_exit");
        assert_eq!(data["completion_confidence"], "high");
        assert_eq!(data["failure_class"], "rate_limited");
        assert_eq!(data["turn_outcome"]["outcome"], "failed");
    }
}
