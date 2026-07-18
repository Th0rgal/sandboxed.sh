//! Bridge session state and the deterministic tool-result reconciliation.
//!
//! The reconciliation rules are the safety core of the bridge and are pure
//! functions so they can be exhaustively unit-tested:
//!   * every pending call must be answered exactly once (missing ⇒ reject),
//!   * every submitted id must match a pending call (unknown ⇒ reject),
//!   * no id may be answered twice in one request (duplicate ⇒ reject),
//!   * results are delivered to the transport in a stable order.
//!
//! All failure paths reject the whole continuation — we never resume a Grok
//! session with a partial or ambiguous tool-result set.

use super::error::BridgeError;
use super::openai::SubmittedResult;
use super::transport::{RequestedToolCall, ResolvedToolResult};

/// One caller tool call Grok is currently blocked on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    /// Stable OpenAI id we minted and returned to the caller.
    pub openai_call_id: String,
    /// The ACP/MCP id the transport correlates the result against.
    pub provider_call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Mint stable OpenAI `call_...` ids for a batch of requested tool calls.
pub fn mint_pending(calls: &[RequestedToolCall]) -> Vec<PendingCall> {
    calls
        .iter()
        .map(|c| PendingCall {
            openai_call_id: format!("call_{}", uuid::Uuid::new_v4().simple()),
            provider_call_id: c.provider_call_id.clone(),
            name: c.name.clone(),
            arguments: c.arguments.clone(),
        })
        .collect()
}

/// Reconcile caller-submitted tool results against the pending calls.
///
/// Returns results in `pending` order (deterministic regardless of the order
/// the caller listed them), each carrying the provider-side id the transport
/// needs to release the matching suspended MCP invocation.
pub fn reconcile(
    pending: &[PendingCall],
    submitted: &[SubmittedResult],
) -> Result<Vec<ResolvedToolResult>, BridgeError> {
    use std::collections::HashMap;

    // Reject duplicates within the submission up front.
    let mut by_id: HashMap<&str, &SubmittedResult> = HashMap::new();
    for result in submitted {
        if by_id.insert(result.tool_call_id.as_str(), result).is_some() {
            return Err(BridgeError::invalid_request(format!(
                "duplicate tool result for call id '{}'",
                result.tool_call_id
            )));
        }
    }

    // Reject unknown ids (submitted but never requested).
    let pending_ids: std::collections::HashSet<&str> =
        pending.iter().map(|p| p.openai_call_id.as_str()).collect();
    for result in submitted {
        if !pending_ids.contains(result.tool_call_id.as_str()) {
            return Err(BridgeError::invalid_request(format!(
                "tool result for unknown call id '{}'",
                result.tool_call_id
            )));
        }
    }

    // Require every pending call to be answered, and deliver in pending order.
    let mut resolved = Vec::with_capacity(pending.len());
    for call in pending {
        let Some(result) = by_id.get(call.openai_call_id.as_str()) else {
            return Err(BridgeError::invalid_request(format!(
                "missing tool result for call id '{}' (tool '{}')",
                call.openai_call_id, call.name
            )));
        };
        resolved.push(ResolvedToolResult {
            provider_call_id: call.provider_call_id.clone(),
            content: result.content.clone(),
        });
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(pairs: &[(&str, &str)]) -> Vec<PendingCall> {
        pairs
            .iter()
            .map(|(oid, pid)| PendingCall {
                openai_call_id: oid.to_string(),
                provider_call_id: pid.to_string(),
                name: "t".to_string(),
                arguments: serde_json::json!({}),
            })
            .collect()
    }

    fn submitted(pairs: &[(&str, &str)]) -> Vec<SubmittedResult> {
        pairs
            .iter()
            .map(|(id, c)| SubmittedResult {
                tool_call_id: id.to_string(),
                content: c.to_string(),
            })
            .collect()
    }

    #[test]
    fn mint_produces_unique_stable_ids() {
        let calls = vec![
            RequestedToolCall {
                provider_call_id: "a".into(),
                name: "x".into(),
                arguments: serde_json::json!({}),
            },
            RequestedToolCall {
                provider_call_id: "b".into(),
                name: "y".into(),
                arguments: serde_json::json!({}),
            },
        ];
        let p = mint_pending(&calls);
        assert_eq!(p.len(), 2);
        assert!(p[0].openai_call_id.starts_with("call_"));
        assert_ne!(p[0].openai_call_id, p[1].openai_call_id);
        assert_eq!(p[0].provider_call_id, "a");
    }

    #[test]
    fn reconcile_happy_path_orders_by_pending() {
        let p = pending(&[("call_1", "acp-1"), ("call_2", "acp-2")]);
        // Submitted out of order — reconciliation must still deliver in pending
        // order (acp-1 then acp-2).
        let s = submitted(&[("call_2", "second"), ("call_1", "first")]);
        let resolved = reconcile(&p, &s).unwrap();
        assert_eq!(resolved[0].provider_call_id, "acp-1");
        assert_eq!(resolved[0].content, "first");
        assert_eq!(resolved[1].provider_call_id, "acp-2");
        assert_eq!(resolved[1].content, "second");
    }

    #[test]
    fn reconcile_rejects_missing() {
        let p = pending(&[("call_1", "acp-1"), ("call_2", "acp-2")]);
        let s = submitted(&[("call_1", "first")]);
        let err = reconcile(&p, &s).unwrap_err();
        assert!(err.message.contains("missing tool result"));
    }

    #[test]
    fn reconcile_rejects_unknown() {
        let p = pending(&[("call_1", "acp-1")]);
        let s = submitted(&[("call_1", "first"), ("call_99", "ghost")]);
        let err = reconcile(&p, &s).unwrap_err();
        assert!(err.message.contains("unknown call id"));
    }

    #[test]
    fn reconcile_rejects_duplicate() {
        let p = pending(&[("call_1", "acp-1")]);
        let s = submitted(&[("call_1", "first"), ("call_1", "again")]);
        let err = reconcile(&p, &s).unwrap_err();
        assert!(err.message.contains("duplicate tool result"));
    }
}
