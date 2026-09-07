use serde_json::Value;

/// Backend-agnostic execution events.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Agent is thinking/reasoning.
    ///
    /// `content` is a per-item cumulative snapshot (each emit for the same
    /// `item_id` contains the previous emit as a prefix). `item_id` identifies
    /// the reasoning item the snapshot belongs to so consumers can detect
    /// transitions between distinct thoughts within a single turn — codex
    /// emits multiple reasoning items per turn and they must not be merged
    /// into one buffer. `None` means the backend doesn't expose item IDs
    /// (Claude Code CLI handles its own block-index finalization upstream;
    /// Gemini emits a single thought stream per turn).
    Thinking {
        content: String,
        item_id: Option<String>,
    },
    /// Agent is calling a tool.
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// Tool execution completed.
    ToolResult {
        id: String,
        name: String,
        result: Value,
    },
    /// Text content being streamed.
    TextDelta { content: String },
    /// Optional turn summary (backend-specific).
    TurnSummary { content: String },
    /// Token usage report from the backend (e.g. Codex turn.completed).
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Goal-mode iteration marker. Emitted once per turn by the codex
    /// app-server driver when a goal is active so the UI can render
    /// "iter N" pills. `iteration` is 1-based and monotonically increasing
    /// within a single mission. Backends that don't run goal loops just
    /// don't emit this event.
    GoalIteration { iteration: u32, objective: String },
    /// Goal status transitioned (active/paused/blocked/usageLimited/budgetLimited/complete).
    /// Carries the canonical status string from codex's `thread/goal/updated`
    /// notification. UI renders this as a goal-state pill.
    GoalStatus { status: String, objective: String },
    /// Backend acknowledged caller-initiated cancellation. This stays separate
    /// from a generic error so mission_runner can preserve interrupted
    /// semantics while still flushing any prior output.
    Cancelled,
    /// Message execution completed.
    MessageComplete { session_id: String },
    /// Error occurred.
    Error { message: String },
}
