//! End-to-end bridge tests driven by the fake ACP/MCP backend.
//!
//! These prove the two-request tool-call contract without a live account:
//!   * a request carrying `tools[]` returns `assistant.tool_calls` and parks a
//!     session — the backend never executes the caller's tools,
//!   * the follow-up `role: "tool"` request resumes the *same* session and
//!     produces the next turn,
//!   * malformed continuations fail closed and tear the session down.
//!
//! The fake is honest about being a fake (`FakeBackend`); it reproduces the ACP
//! transcript shape, not a real Grok child.

use std::time::Duration;

use super::openai::BridgeChatRequest;
use super::registry::SessionRegistry;
use super::transport::fake::{FakeBackend, ScriptedTurn};
use super::{is_bridge_model, process_request};

fn req(json: serde_json::Value) -> BridgeChatRequest {
    serde_json::from_value(json).expect("valid request json")
}

fn registry() -> SessionRegistry {
    SessionRegistry::new(Duration::from_secs(60))
}

fn tool_call_id(resp: &serde_json::Value, idx: usize) -> String {
    resp["choices"][0]["message"]["tool_calls"][idx]["id"]
        .as_str()
        .expect("tool call id present")
        .to_string()
}

#[tokio::test]
async fn two_request_round_trip_keeps_tools_caller_owned() {
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![(
            "get_weather".into(),
            serde_json::json!({ "city": "Paris" }),
        )]),
        ScriptedTurn::Complete("It is sunny in Paris.".into()),
    ]);
    let transcript = backend.transcript();
    let reg = registry();

    // Request 1 — offers a tool, expects a suspended tool-call turn.
    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "What is the weather in Paris?" }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Look up weather",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
            }
        }]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    assert_eq!(resp1["choices"][0]["finish_reason"], "tool_calls");
    assert!(resp1["choices"][0]["message"]["content"].is_null());
    assert_eq!(
        resp1["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    let call_id = tool_call_id(&resp1, 0);
    assert!(call_id.starts_with("call_"));
    assert_eq!(reg.len(), 1, "session parked awaiting the tool result");

    // The backend was offered the tool but never executed it.
    {
        let t = transcript.lock().unwrap();
        assert_eq!(t.opened_with_tools, vec!["get_weather".to_string()]);
        assert!(
            t.executed_tools.is_empty(),
            "caller tools stay caller-owned"
        );
    }

    // Request 2 — returns the caller-produced result, resumes the same session.
    let second = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "What is the weather in Paris?" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
            }]},
            { "role": "tool", "tool_call_id": call_id, "content": "sunny, 24C" }
        ]
    }));
    let resp2 = process_request(&backend, &reg, second).await.unwrap();
    assert_eq!(resp2["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        resp2["choices"][0]["message"]["content"],
        "It is sunny in Paris."
    );
    assert_eq!(reg.len(), 0, "session torn down after completion");

    // The exact caller result was routed back to the suspended invocation, and
    // the session was shut down once.
    let t = transcript.lock().unwrap();
    assert_eq!(t.results_received.len(), 1);
    assert_eq!(t.results_received[0][0].content, "sunny, 24C");
    assert_eq!(t.shutdowns, 1);
}

#[tokio::test]
async fn multiple_tool_calls_require_every_result() {
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![
            ("get_weather".into(), serde_json::json!({ "city": "Paris" })),
            ("get_time".into(), serde_json::json!({ "tz": "CET" })),
        ]),
        ScriptedTurn::Complete("done".into()),
    ]);
    let reg = registry();

    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "weather and time" }],
        "tools": [
            { "type": "function", "function": { "name": "get_weather" } },
            { "type": "function", "function": { "name": "get_time" } }
        ]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    let calls = resp1["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    let id0 = tool_call_id(&resp1, 0);
    let id1 = tool_call_id(&resp1, 1);

    // The assistant echo binds the round; answering only one of the two fails
    // closed (missing result) and reclaims the session.
    let partial = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "weather and time" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": id0, "type": "function", "function": { "name": "get_weather", "arguments": "{}" } },
                { "id": id1, "type": "function", "function": { "name": "get_time", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": id0, "content": "sunny" }
        ]
    }));
    let err = process_request(&backend, &reg, partial).await.unwrap_err();
    assert!(err.message.contains("missing tool result"));
    assert_eq!(
        reg.len(),
        0,
        "inconsistent continuation tears the session down"
    );

    // The session is gone, so answering both now is a clean session-state error
    // rather than a double-drive.
    let both = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "weather and time" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": id0, "type": "function", "function": { "name": "get_weather", "arguments": "{}" } },
                { "id": id1, "type": "function", "function": { "name": "get_time", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": id0, "content": "sunny" },
            { "role": "tool", "tool_call_id": id1, "content": "12:00" }
        ]
    }));
    let err = process_request(&backend, &reg, both).await.unwrap_err();
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::SessionState
    ));
}

#[tokio::test]
async fn immediate_completion_without_tools() {
    let backend = FakeBackend::new(vec![ScriptedTurn::Complete("hello there".into())]);
    let reg = registry();
    let request = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "hi" }]
    }));
    let resp = process_request(&backend, &reg, request).await.unwrap();
    assert_eq!(resp["choices"][0]["finish_reason"], "stop");
    assert_eq!(resp["choices"][0]["message"]["content"], "hello there");
    assert_eq!(resp["usage"]["total_tokens"], 15);
    assert_eq!(reg.len(), 0);
}

#[tokio::test]
async fn streaming_is_rejected_not_downgraded() {
    let backend = FakeBackend::new(vec![ScriptedTurn::Complete("x".into())]);
    let reg = registry();
    let request = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true
    }));
    let err = process_request(&backend, &reg, request).await.unwrap_err();
    assert!(err.message.contains("streaming is not supported"));
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::InvalidRequest
    ));
}

#[tokio::test]
async fn orphan_tool_result_without_assistant_echo_is_rejected() {
    let backend = FakeBackend::new(vec![ScriptedTurn::Complete("x".into())]);
    let reg = registry();
    // A tool result referencing an id that no assistant turn ever issued, and
    // with no assistant tool_calls echoed at all, is malformed: reject before
    // touching any session.
    let orphan = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "hi" },
            { "role": "tool", "tool_call_id": "call_never_issued", "content": "ghost" }
        ]
    }));
    let err = process_request(&backend, &reg, orphan).await.unwrap_err();
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::InvalidRequest
    ));
    assert!(err.message.contains("was not issued"));
    assert_eq!(reg.len(), 0);
}

#[tokio::test]
async fn backend_open_failure_is_surfaced_as_infrastructure() {
    let backend = FakeBackend::failing(super::error::BridgeError::upstream(
        "grok agent stdio failed to start",
    ));
    let reg = registry();
    let request = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "hi" }]
    }));
    let err = process_request(&backend, &reg, request).await.unwrap_err();
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::UpstreamInfrastructure
    ));
    assert_eq!(
        reg.len(),
        0,
        "no session parked when the backend never opened"
    );
}

#[tokio::test]
async fn continuation_rejects_fabricated_tool_call_id() {
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![(
            "get_weather".into(),
            serde_json::json!({ "city": "Paris" }),
        )]),
        ScriptedTurn::Complete("done".into()),
    ]);
    let reg = registry();

    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "weather?" }],
        "tools": [{ "type": "function", "function": { "name": "get_weather" } }]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    let call_id = tool_call_id(&resp1, 0);

    // The assistant echoes the real id, but the tool result references a
    // different one — reject before touching the parked session.
    let forged = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "weather?" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": { "name": "get_weather", "arguments": "{}" }
            }]},
            { "role": "tool", "tool_call_id": "call_fabricated", "content": "sunny" }
        ]
    }));
    let err = process_request(&backend, &reg, forged).await.unwrap_err();
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::InvalidRequest
    ));
    assert!(err.message.contains("was not issued"));
    assert_eq!(reg.len(), 1, "the real session stays parked, untouched");
}

#[tokio::test]
async fn multi_round_binds_only_the_latest_tool_call_round() {
    // Two successive tool-call rounds, then completion. The final continuation
    // carries the *whole* history (round-1 and round-2 assistant turns and
    // results); binding must key off only the latest round's ids.
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![("tool_a".into(), serde_json::json!({}))]),
        ScriptedTurn::ToolCalls(vec![("tool_b".into(), serde_json::json!({}))]),
        ScriptedTurn::Complete("all done".into()),
    ]);
    let reg = registry();

    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "go" }],
        "tools": [
            { "type": "function", "function": { "name": "tool_a" } },
            { "type": "function", "function": { "name": "tool_b" } }
        ]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    let call_a = tool_call_id(&resp1, 0);

    let second = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "go" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": call_a, "type": "function", "function": { "name": "tool_a", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": call_a, "content": "a-result" }
        ]
    }));
    let resp2 = process_request(&backend, &reg, second).await.unwrap();
    assert_eq!(resp2["choices"][0]["finish_reason"], "tool_calls");
    let call_b = tool_call_id(&resp2, 0);
    assert_ne!(call_a, call_b);
    assert_eq!(reg.len(), 1, "session re-parked under the round-2 id");

    // Round 3 replays the full transcript. Round-1's `call_a` is no longer in
    // the registry, but binding uses only the latest round (`call_b`).
    let third = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "go" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": call_a, "type": "function", "function": { "name": "tool_a", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": call_a, "content": "a-result" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": call_b, "type": "function", "function": { "name": "tool_b", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": call_b, "content": "b-result" }
        ]
    }));
    let resp3 = process_request(&backend, &reg, third).await.unwrap();
    assert_eq!(resp3["choices"][0]["finish_reason"], "stop");
    assert_eq!(resp3["choices"][0]["message"]["content"], "all done");
    assert_eq!(reg.len(), 0);
}

#[tokio::test]
async fn replayed_continuation_after_resume_is_session_state_error() {
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![("get_weather".into(), serde_json::json!({}))]),
        ScriptedTurn::Complete("sunny".into()),
    ]);
    let reg = registry();

    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "weather?" }],
        "tools": [{ "type": "function", "function": { "name": "get_weather" } }]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    let call_id = tool_call_id(&resp1, 0);

    let continuation = serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [
            { "role": "user", "content": "weather?" },
            { "role": "assistant", "content": serde_json::Value::Null, "tool_calls": [
                { "id": call_id, "type": "function", "function": { "name": "get_weather", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": call_id, "content": "sunny" }
        ]
    });

    // First continuation resumes and completes the session.
    let ok = process_request(&backend, &reg, req(continuation.clone()))
        .await
        .unwrap();
    assert_eq!(ok["choices"][0]["finish_reason"], "stop");
    assert_eq!(reg.len(), 0);

    // Replaying the exact same continuation now fails closed — the session is
    // gone, so it can never be double-driven.
    let err = process_request(&backend, &reg, req(continuation))
        .await
        .unwrap_err();
    assert!(matches!(
        err.class,
        super::error::BridgeErrorClass::SessionState
    ));
}

#[tokio::test]
async fn expired_session_is_reaped_and_shut_down() {
    let backend = FakeBackend::new(vec![
        ScriptedTurn::ToolCalls(vec![("t".into(), serde_json::json!({}))]),
        ScriptedTurn::Complete("done".into()),
    ]);
    let transcript = backend.transcript();
    // Tiny TTL so the parked session is immediately eligible for reaping.
    let reg = SessionRegistry::new(Duration::from_millis(1));

    let first = req(serde_json::json!({
        "model": "grok-cli/grok-4.5",
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [{ "type": "function", "function": { "name": "t" } }]
    }));
    let resp1 = process_request(&backend, &reg, first).await.unwrap();
    assert_eq!(resp1["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(reg.len(), 1);

    // After the TTL elapses, reaping deterministically tears the abandoned
    // session down (awaited shutdown) rather than leaking it.
    tokio::time::sleep(Duration::from_millis(10)).await;
    super::reap_expired(&reg).await;
    assert_eq!(reg.len(), 0, "expired session reaped");
    assert_eq!(
        transcript.lock().unwrap().shutdowns,
        1,
        "reaped session was shut down, not leaked"
    );
}

#[test]
fn bridge_model_routing() {
    // Exact allowlist only — never arbitrary `grok-cli/*`.
    assert!(is_bridge_model("grok-cli/grok-4.5"));
    assert!(!is_bridge_model("grok-cli/anything"));
    assert!(!is_bridge_model("grok-cli/grok-4.5-mini"));
    assert!(!is_bridge_model("xai/grok-4.5"));
    assert!(!is_bridge_model("claude-opus-4-8"));
}
