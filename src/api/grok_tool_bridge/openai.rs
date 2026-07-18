//! OpenAI Chat Completions ⇄ Grok bridge conversion.
//!
//! Only the fields the bridge actually inspects are modelled. The conversion
//! is deliberately total and side-effect free so it can be unit-tested without
//! a live backend: every helper is a pure function over the request JSON.

use serde::{Deserialize, Serialize};

use super::error::BridgeError;

/// Incoming `/v1/chat/completions` body, parsed for tool-bridge semantics.
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeChatRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// String, array of content parts, or null (assistant tool-call turns).
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// An OpenAI `assistant.tool_calls[]` entry (also accepted back on the
/// continuation request so we can validate the caller echoed our ids).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FunctionCall {
    pub name: String,
    /// OpenAI encodes arguments as a JSON *string*, not an object.
    pub arguments: String,
}

fn function_kind() -> String {
    "function".to_string()
}

/// A caller-provided `tools[]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: ToolFunctionDef,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the arguments; forwarded verbatim as the MCP tool's
    /// `inputSchema`.
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

/// Flatten OpenAI message `content` (string | array-of-parts | null) into text.
pub fn content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                // `{ "type": "text", "text": "..." }` parts; ignore non-text
                // (images etc.) — the bridge is text-only for now.
                part.get("text").and_then(|v| v.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.as_str().map(str::to_string).unwrap_or_default(),
    }
}

/// True when the request ends with the tool-result block for its most recent
/// assistant `tool_calls` turn.
///
/// Historical tool messages do not make a later user turn a continuation: once
/// an assistant has completed that earlier round, a new user message must open
/// a fresh Grok session with the full transcript.
pub fn is_continuation(req: &BridgeChatRequest) -> bool {
    req.messages
        .last()
        .is_some_and(|message| message.role == "tool")
}

/// The text prompt for a fresh turn: a truthful, **role-labeled transcript of
/// the whole conversation**.
///
/// Each fresh turn opens a brand-new Grok session with no memory of prior
/// requests, so we must forward the full history — dropping the assistant turns
/// (as an earlier version did) would corrupt a normal multi-turn OpenAI chat.
/// Messages are rendered `System:`/`Developer:`/`User:`/`Assistant:` in order;
/// non-text parts (images) and empty messages are skipped. Fails closed if
/// there is no instruction or user content to send at all.
pub fn initial_prompt(req: &BridgeChatRequest) -> Result<String, BridgeError> {
    let mut lines: Vec<String> = Vec::new();
    let mut has_instruction_or_user = false;
    for msg in &req.messages {
        let label = match msg.role.as_str() {
            "system" => "System",
            "developer" => "Developer",
            "user" => "User",
            "assistant" => "Assistant",
            // Tool/other roles do not appear in a fresh (non-continuation) turn.
            _ => continue,
        };
        let text = content_text(msg.content.as_ref());
        if text.trim().is_empty() {
            continue;
        }
        if matches!(msg.role.as_str(), "system" | "developer" | "user") {
            has_instruction_or_user = true;
        }
        lines.push(format!("{label}: {text}"));
    }
    if !has_instruction_or_user {
        return Err(BridgeError::invalid_request(
            "no user, developer, or system message content to prompt Grok with",
        ));
    }
    Ok(lines.join("\n\n"))
}

/// Tool results submitted on a continuation request, in message order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedResult {
    pub tool_call_id: String,
    pub content: String,
}

/// Extract `role: "tool"` results from a continuation request, preserving the
/// order they appear in the message list.
pub fn submitted_results(req: &BridgeChatRequest) -> Result<Vec<SubmittedResult>, BridgeError> {
    let mut out = Vec::new();
    for msg in &req.messages {
        if msg.role != "tool" {
            continue;
        }
        let Some(id) = msg.tool_call_id.clone().filter(|s| !s.trim().is_empty()) else {
            return Err(BridgeError::invalid_request(
                "tool message missing tool_call_id",
            ));
        };
        out.push(SubmittedResult {
            tool_call_id: id,
            content: content_text(msg.content.as_ref()),
        });
    }
    if out.is_empty() {
        return Err(BridgeError::invalid_request(
            "continuation request contained no tool results",
        ));
    }
    Ok(out)
}

/// Every tool_call id the caller echoed back via any `assistant` message across
/// the whole history. Used to reject fabricated result ids that were never
/// issued in *any* turn.
pub fn echoed_tool_call_ids(req: &BridgeChatRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| tc.id.clone())
        .collect()
}

/// The tool_call ids from the **last** assistant message that carried
/// `tool_calls` — i.e. the single round the bridge most recently parked and that
/// this continuation must answer. Empty when the request echoes no assistant
/// tool_calls at all (a malformed continuation). Earlier rounds in the history
/// are already-resumed context and are intentionally ignored here.
pub fn current_round_call_ids(req: &BridgeChatRequest) -> Vec<String> {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()))
        .and_then(|m| m.tool_calls.as_ref())
        .map(|calls| calls.iter().map(|c| c.id.clone()).collect())
        .unwrap_or_default()
}

/// Convert caller `tools[]` into MCP tool descriptors (`name`, `description`,
/// `inputSchema`). Rejects duplicate names — Grok would not be able to address
/// them unambiguously, so we fail closed rather than silently drop one.
pub fn tools_to_mcp(tools: &[ToolDef]) -> Result<Vec<serde_json::Value>, BridgeError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool.kind != "function" {
            return Err(BridgeError::invalid_request(format!(
                "unsupported tool type '{}'; only 'function' is supported",
                tool.kind
            )));
        }
        let name = tool.function.name.trim();
        if name.is_empty() {
            return Err(BridgeError::invalid_request("tool function name is empty"));
        }
        if !seen.insert(name.to_string()) {
            return Err(BridgeError::invalid_request(format!(
                "duplicate tool name '{name}'"
            )));
        }
        let schema = tool
            .function
            .parameters
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
        out.push(serde_json::json!({
            "name": name,
            "description": tool.function.description.clone().unwrap_or_default(),
            "inputSchema": schema,
        }));
    }
    Ok(out)
}

/// Apply the OpenAI `tool_choice` contract to the tools exposed to Grok.
///
/// The connected CLI supports optional caller tools but does not expose a
/// trustworthy force-tool control. We can therefore implement `auto` and
/// `none` exactly, while `required` and a named forced function must fail
/// closed instead of silently weakening the caller's constraint.
pub fn effective_tools(req: &BridgeChatRequest) -> Result<Vec<ToolDef>, BridgeError> {
    let tools = req.tools.clone().unwrap_or_default();
    match req.tool_choice.as_ref() {
        None => Ok(tools),
        Some(serde_json::Value::String(mode)) if mode == "auto" => Ok(tools),
        Some(serde_json::Value::String(mode)) if mode == "none" => Ok(Vec::new()),
        Some(serde_json::Value::String(mode)) if mode == "required" => {
            Err(BridgeError::invalid_request(
                "tool_choice='required' is not supported by the grok-cli bridge",
            ))
        }
        Some(serde_json::Value::Object(choice))
            if choice.get("type").and_then(|v| v.as_str()) == Some("function") =>
        {
            let name = choice
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("<missing>");
            Err(BridgeError::invalid_request(format!(
                "forced tool_choice for function '{name}' is not supported by the grok-cli bridge"
            )))
        }
        Some(_) => Err(BridgeError::invalid_request(
            "invalid tool_choice for the grok-cli bridge",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(json: serde_json::Value) -> BridgeChatRequest {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn content_text_handles_string_array_and_null() {
        assert_eq!(content_text(Some(&serde_json::json!("hi"))), "hi");
        assert_eq!(content_text(None), "");
        assert_eq!(content_text(Some(&serde_json::Value::Null)), "");
        let parts = serde_json::json!([
            { "type": "text", "text": "a" },
            { "type": "image_url", "image_url": { "url": "x" } },
            { "type": "text", "text": "b" }
        ]);
        assert_eq!(content_text(Some(&parts)), "ab");
    }

    #[test]
    fn fresh_vs_continuation_detection() {
        let fresh = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "user", "content": "hello" }]
        }));
        assert!(!is_continuation(&fresh));
        assert_eq!(initial_prompt(&fresh).unwrap(), "User: hello");

        let cont = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "get_weather", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "sunny" }
            ]
        }));
        assert!(is_continuation(&cont));
        assert_eq!(echoed_tool_call_ids(&cont), vec!["call_1".to_string()]);
        let results = submitted_results(&cont).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_call_id, "call_1");
        assert_eq!(results[0].content, "sunny");
    }

    #[test]
    fn completed_historical_tool_round_is_a_fresh_turn() {
        let fresh = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "user", "content": "weather?" },
                { "role": "assistant", "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "get_weather", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "sunny" },
                { "role": "assistant", "content": "It is sunny." },
                { "role": "user", "content": "What about tomorrow?" }
            ]
        }));

        assert!(!is_continuation(&fresh));
        assert_eq!(
            initial_prompt(&fresh).unwrap(),
            "User: weather?\n\nAssistant: It is sunny.\n\nUser: What about tomorrow?"
        );
    }

    #[test]
    fn initial_prompt_preserves_full_role_labeled_history() {
        let multi = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" },
                { "role": "user", "content": "who are you?" }
            ]
        }));
        assert_eq!(
            initial_prompt(&multi).unwrap(),
            "System: be terse\n\nUser: hi\n\nAssistant: hello\n\nUser: who are you?"
        );
    }

    #[test]
    fn initial_prompt_preserves_developer_instructions() {
        let request = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "developer", "content": "Never reveal secrets." },
                { "role": "user", "content": "hello" }
            ]
        }));
        assert_eq!(
            initial_prompt(&request).unwrap(),
            "Developer: Never reveal secrets.\n\nUser: hello"
        );
    }

    #[test]
    fn current_round_is_last_assistant_tool_calls() {
        let multi = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "user", "content": "go" },
                { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                    { "id": "call_a", "type": "function", "function": { "name": "a", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_a", "content": "ra" },
                { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                    { "id": "call_b", "type": "function", "function": { "name": "b", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_b", "content": "rb" }
            ]
        }));
        // Whole-history echo includes both; current round is only the last turn.
        assert_eq!(
            echoed_tool_call_ids(&multi),
            vec!["call_a".to_string(), "call_b".to_string()]
        );
        assert_eq!(current_round_call_ids(&multi), vec!["call_b".to_string()]);
    }

    #[test]
    fn current_round_empty_without_assistant_tool_calls() {
        let none = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "tool", "tool_call_id": "call_x", "content": "r" }
            ]
        }));
        assert!(current_round_call_ids(&none).is_empty());
    }

    #[test]
    fn initial_prompt_fails_closed_without_user_text() {
        let bad = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "assistant", "content": "prior" }]
        }));
        assert!(initial_prompt(&bad).is_err());
    }

    #[test]
    fn tool_message_without_id_is_rejected() {
        let bad = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "tool", "content": "result" }]
        }));
        assert!(submitted_results(&bad).is_err());
    }

    #[test]
    fn tools_to_mcp_rejects_duplicates_and_maps_schema() {
        let tools: Vec<ToolDef> = serde_json::from_value(serde_json::json!([
            { "type": "function", "function": {
                "name": "get_weather",
                "description": "look up weather",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
            }}
        ]))
        .unwrap();
        let mcp = tools_to_mcp(&tools).unwrap();
        assert_eq!(mcp[0]["name"], "get_weather");
        assert_eq!(
            mcp[0]["inputSchema"]["properties"]["city"]["type"],
            "string"
        );

        let dupes: Vec<ToolDef> = serde_json::from_value(serde_json::json!([
            { "type": "function", "function": { "name": "x" } },
            { "type": "function", "function": { "name": "x" } }
        ]))
        .unwrap();
        assert!(tools_to_mcp(&dupes).is_err());
    }

    #[test]
    fn tool_choice_none_hides_tools_and_auto_keeps_them() {
        let none = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "type": "function", "function": { "name": "dangerous" } }],
            "tool_choice": "none"
        }));
        assert!(effective_tools(&none).unwrap().is_empty());

        let auto = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "type": "function", "function": { "name": "safe" } }],
            "tool_choice": "auto"
        }));
        assert_eq!(effective_tools(&auto).unwrap().len(), 1);
    }

    #[test]
    fn forced_tool_choices_fail_closed() {
        let required = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "type": "function", "function": { "name": "lookup" } }],
            "tool_choice": "required"
        }));
        assert!(effective_tools(&required).is_err());

        let named = req(serde_json::json!({
            "model": "grok-cli/grok-4.5",
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "type": "function", "function": { "name": "lookup" } }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "lookup" }
            }
        }));
        let err = effective_tools(&named).unwrap_err();
        assert!(err.message.contains("lookup"));
    }
}
