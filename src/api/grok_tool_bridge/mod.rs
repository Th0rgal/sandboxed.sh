//! Connected-account Grok 4.5 router bridge.
//!
//! Presents OpenAI `/v1/chat/completions` tool-call semantics on top of the
//! connected xAI/Grok account, while keeping **tool execution caller-owned**:
//! caller `tools[]` become an ephemeral MCP server offered to `grok agent
//! stdio`; when Grok invokes one, the bridge suspends that invocation and
//! surfaces it as `assistant.tool_calls`; the caller's later `role: "tool"`
//! result unblocks the same Grok session, which produces the next turn.
//!
//! The pure state machine (message/tool conversion, id minting, result
//! reconciliation, session registry) is fully unit- and transcript-tested via
//! a fake backend. The live [`acp::AcpMcpBackend`] is feature-gated and
//! default-off — see [`capability`].

pub mod acp;
pub mod capability;
pub mod error;
pub mod openai;
pub mod registry;
pub mod response;
pub mod session;
pub mod transport;

use std::sync::OnceLock;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub use capability::{BRIDGE_MODEL_ID, BRIDGE_MODEL_PREFIX};

use error::BridgeError;
use openai::BridgeChatRequest;
use registry::{ParkedSession, SessionRegistry};
use transport::{BridgeBackend, BridgeConversation, PromptInput, TurnOutcome};

/// Idle lifetime of a suspended tool-call session before it is reclaimed.
const SESSION_TTL: Duration = Duration::from_secs(300);

/// Process-wide registry of suspended bridge conversations.
fn registry() -> &'static SessionRegistry {
    static REGISTRY: OnceLock<SessionRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| SessionRegistry::new(SESSION_TTL))
}

/// True when the requested model id targets this bridge. Exact allowlist: we
/// advertise and answer exactly one connected-account model id, never arbitrary
/// `grok-cli/*`, so a caller can't route to an unsupported model through us.
pub fn is_bridge_model(model: &str) -> bool {
    model == BRIDGE_MODEL_ID
}

/// The concrete model to hand the connected account (strip the `grok-cli/`
/// routing prefix).
fn backend_model(requested: &str) -> String {
    requested
        .strip_prefix(BRIDGE_MODEL_PREFIX)
        .unwrap_or(requested)
        .to_string()
}

/// Catalog hook: the model id to advertise, or `None` when the route is not
/// provisioned/healthy.
pub fn advertised_model(working_dir: &std::path::Path) -> Option<&'static str> {
    if capability::probe(working_dir).advertise {
        Some(BRIDGE_MODEL_ID)
    } else {
        None
    }
}

/// Proxy entry point. Parses the body, enforces the capability gate, and drives
/// the live backend. Converts every outcome to an OpenAI-shaped response.
pub async fn handle_chat_completion(working_dir: &std::path::Path, body: &[u8]) -> Response {
    let req: BridgeChatRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return bridge_error_response(BridgeError::invalid_request(format!(
                "invalid request body: {e}"
            )));
        }
    };

    let cap = capability::probe(working_dir);
    if !cap.advertise {
        // Fail closed with a truthful configuration error — never a fake turn.
        return bridge_error_response(BridgeError::provider_configuration(format!(
            "the grok-cli connected-account bridge is not available: {}",
            cap.reason
        )));
    }

    let backend = acp::AcpMcpBackend::new(working_dir.to_path_buf());
    match process_request(&backend, registry(), req).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(err) => bridge_error_response(err),
    }
}

fn bridge_error_response(err: BridgeError) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": err.message,
            "type": err.openai_type(),
            "code": err.openai_code(),
        }
    });
    (err.status(), Json(body)).into_response()
}

/// Backend-agnostic request processing — the heart of the bridge. Tested
/// directly against the fake backend.
pub(crate) async fn process_request(
    backend: &dyn BridgeBackend,
    registry: &SessionRegistry,
    req: BridgeChatRequest,
) -> Result<serde_json::Value, BridgeError> {
    // Streaming is explicitly rejected — never silently downgraded to a
    // non-streaming body a streaming client would mis-parse.
    if req.stream.unwrap_or(false) {
        return Err(BridgeError::invalid_request(
            "streaming is not supported by the grok-cli bridge; retry with stream=false",
        ));
    }

    // Deterministically reap sessions idle past the TTL, awaiting their
    // teardown, before doing anything else — an abandoned tool-call session must
    // never leak its Grok child / ephemeral MCP server.
    reap_expired(registry).await;

    if openai::is_continuation(&req) {
        continue_turn(registry, req).await
    } else {
        fresh_turn(backend, registry, req).await
    }
}

async fn fresh_turn(
    backend: &dyn BridgeBackend,
    registry: &SessionRegistry,
    req: BridgeChatRequest,
) -> Result<serde_json::Value, BridgeError> {
    let prompt = openai::initial_prompt(&req)?;
    let tools = req.tools.clone().unwrap_or_default();
    let tools_mcp = openai::tools_to_mcp(&tools)?;
    let input = PromptInput {
        prompt,
        model: Some(backend_model(&req.model)),
        tools_mcp,
    };
    let (conversation, outcome) = backend.open(input).await?;
    finish_outcome(registry, conversation, outcome).await
}

async fn continue_turn(
    registry: &SessionRegistry,
    req: BridgeChatRequest,
) -> Result<serde_json::Value, BridgeError> {
    let submitted = openai::submitted_results(&req)?;

    // Every submitted result id must have been issued in *some* assistant
    // tool_calls turn — reject fabricated ids up front, before touching any
    // parked session, so a continuation can never bind to the wrong session.
    let all_echoed = openai::echoed_tool_call_ids(&req);
    for result in &submitted {
        if !all_echoed.contains(&result.tool_call_id) {
            return Err(BridgeError::invalid_request(format!(
                "tool result references id '{}' that was not issued in any assistant \
                 tool_calls of this request",
                result.tool_call_id
            )));
        }
    }

    // Bind to exactly the *last* tool-call round the bridge parked. A well-formed
    // continuation must echo that assistant turn; without it there is nothing to
    // resume.
    let round = openai::current_round_call_ids(&req);
    if round.is_empty() {
        return Err(BridgeError::invalid_request(
            "continuation provided tool results but echoed no assistant tool_calls to answer",
        ));
    }

    // The results answering the current round, in submission order. Results for
    // earlier (already-resumed) rounds are historical context and are ignored.
    let round_set: std::collections::HashSet<&str> = round.iter().map(String::as_str).collect();
    let round_results: Vec<openai::SubmittedResult> = submitted
        .iter()
        .filter(|r| round_set.contains(r.tool_call_id.as_str()))
        .cloned()
        .collect();

    let mut parked = registry.take_for_call_ids(&round)?;

    // Reconcile the current round's results against the session's pending calls
    // (rejects missing, duplicate, or extra results). On any inconsistency shut
    // the session down so it can never be half-resumed.
    let resolved = match session::reconcile(&parked.pending, &round_results) {
        Ok(r) => r,
        Err(e) => {
            parked.conversation.shutdown().await;
            return Err(e);
        }
    };
    let outcome = match parked.conversation.resume(resolved).await {
        Ok(o) => o,
        Err(e) => {
            parked.conversation.shutdown().await;
            return Err(e);
        }
    };
    finish_outcome(registry, parked.conversation, outcome).await
}

/// Reap sessions idle past the TTL, awaiting each teardown. Called at the top
/// of every request so an abandoned tool-call session cannot leak its Grok
/// child / MCP server indefinitely. (`AcpConversation` also tears down on
/// `Drop` as a safety net, but this path is deterministic and awaited.)
async fn reap_expired(registry: &SessionRegistry) {
    for mut parked in registry.sweep_expired() {
        parked.conversation.shutdown().await;
    }
}

/// Turn a backend outcome into an OpenAI response, parking the session when
/// more caller tool calls are pending and tearing it down when complete.
async fn finish_outcome(
    registry: &SessionRegistry,
    mut conversation: Box<dyn BridgeConversation>,
    outcome: TurnOutcome,
) -> Result<serde_json::Value, BridgeError> {
    match outcome {
        TurnOutcome::ToolCalls { model, calls } => {
            if calls.is_empty() {
                conversation.shutdown().await;
                return Err(BridgeError::upstream(
                    "Grok reported a tool-call turn with no tool calls",
                ));
            }
            let pending = session::mint_pending(&calls);
            let resp = response::tool_calls_response(&model, &pending);
            tracing::debug!(
                session_id = conversation.session_id(),
                model = %model,
                pending = pending.len(),
                "parked grok bridge session awaiting caller tool results"
            );
            registry.park(ParkedSession {
                conversation,
                pending,
            });
            Ok(resp)
        }
        TurnOutcome::Completed(turn) => {
            conversation.shutdown().await;
            Ok(response::completed_response(&turn))
        }
    }
}

#[cfg(test)]
mod tests;
