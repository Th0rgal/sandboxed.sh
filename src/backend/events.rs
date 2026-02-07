use serde_json::Value;

/// Backend-agnostic execution events.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Agent is thinking/reasoning.
    Thinking { content: String },
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
    /// Message execution completed.
    MessageComplete { session_id: String },
    /// Error occurred.
    Error { message: String },
}
