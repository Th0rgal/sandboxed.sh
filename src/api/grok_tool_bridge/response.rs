//! OpenAI Chat Completions response envelopes.
//!
//! Provenance is truthful: `model` is the id the connected account actually
//! reported, usage is only populated from real token counts, and the tool-call
//! turn uses the canonical `finish_reason: "tool_calls"` with `content: null`.

use super::session::PendingCall;
use super::transport::{BridgeUsage, CompletedTurn};

fn envelope_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

fn usage_json(usage: &BridgeUsage) -> Option<serde_json::Value> {
    if usage.total() == 0 {
        return None;
    }
    Some(serde_json::json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total(),
    }))
}

/// Build a `finish_reason: "tool_calls"` response from the pending calls the
/// caller is expected to execute and return.
pub fn tool_calls_response(model: &str, pending: &[PendingCall]) -> serde_json::Value {
    let tool_calls: Vec<serde_json::Value> = pending
        .iter()
        .map(|call| {
            serde_json::json!({
                "id": call.openai_call_id,
                "type": "function",
                "function": {
                    "name": call.name,
                    // OpenAI encodes arguments as a JSON string.
                    "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
                },
            })
        })
        .collect();

    serde_json::json!({
        "id": envelope_id(),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "provider": "grok-cli",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": tool_calls,
            },
            "finish_reason": "tool_calls",
        }],
    })
}

/// Build a `finish_reason: "stop"` response from a completed assistant turn.
pub fn completed_response(turn: &CompletedTurn) -> serde_json::Value {
    let mut body = serde_json::json!({
        "id": envelope_id(),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": turn.model,
        "provider": "grok-cli",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": turn.text,
            },
            "finish_reason": stop_reason_to_openai(&turn.stop_reason),
        }],
    });
    if let Some(usage) = usage_json(&turn.usage) {
        body["usage"] = usage;
    }
    body
}

/// Map an ACP stop reason onto an OpenAI `finish_reason`. Unknown reasons map
/// to `"stop"` (the turn did complete); we never invent `"tool_calls"` here.
fn stop_reason_to_openai(stop_reason: &str) -> &'static str {
    match stop_reason {
        "max_tokens" | "length" => "length",
        "refusal" | "content_filter" => "content_filter",
        _ => "stop",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_response_shape_is_canonical() {
        let pending = vec![PendingCall {
            openai_call_id: "call_abc".into(),
            provider_call_id: "acp-1".into(),
            name: "get_weather".into(),
            arguments: serde_json::json!({ "city": "Paris" }),
        }];
        let resp = tool_calls_response("grok-4.5", &pending);
        assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            resp["choices"][0]["message"]["content"],
            serde_json::Value::Null
        );
        let tc = &resp["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["function"]["name"], "get_weather");
        // arguments is a JSON *string*.
        let args = tc["function"]["arguments"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(args).unwrap()["city"],
            "Paris"
        );
        assert_eq!(resp["model"], "grok-4.5");
    }

    #[test]
    fn completed_response_omits_zero_usage_and_is_truthful() {
        let turn = CompletedTurn {
            text: "hello".into(),
            model: "grok-4.5".into(),
            usage: BridgeUsage::default(),
            stop_reason: "stop".into(),
        };
        let resp = completed_response(&turn);
        assert_eq!(resp["choices"][0]["message"]["content"], "hello");
        assert_eq!(resp["choices"][0]["finish_reason"], "stop");
        assert!(resp.get("usage").is_none());

        let turn2 = CompletedTurn {
            usage: BridgeUsage {
                input_tokens: 3,
                output_tokens: 4,
            },
            ..turn
        };
        let resp2 = completed_response(&turn2);
        assert_eq!(resp2["usage"]["total_tokens"], 7);
    }
}
