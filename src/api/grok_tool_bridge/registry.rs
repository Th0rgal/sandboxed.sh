//! In-memory registry of suspended bridge conversations.
//!
//! A conversation is parked here between the request that produced
//! `tool_calls` and the continuation that supplies the results. Each parked
//! session is addressable by any of the OpenAI `call_...` ids it is blocked on.
//!
//! Concurrency is deliberately *take-to-own*: a continuation removes the
//! session from the map for the duration of its (async) resume, so a second
//! concurrent continuation for the same session finds it absent and fails
//! closed instead of double-driving one Grok child.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::error::BridgeError;
use super::session::PendingCall;
use super::transport::BridgeConversation;

/// A parked conversation and everything needed to resume it.
pub struct ParkedSession {
    pub conversation: Box<dyn BridgeConversation>,
    pub pending: Vec<PendingCall>,
}

struct Slot {
    parked: ParkedSession,
    last_active: Instant,
}

pub struct SessionRegistry {
    slots: Mutex<HashMap<String, Slot>>,
    /// openai_call_id → conversation_id.
    index: Mutex<HashMap<String, String>>,
    ttl: Duration,
}

impl SessionRegistry {
    pub fn new(ttl: Duration) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            index: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Park a conversation and index it by its pending call ids. Returns the
    /// conversation id (opaque; not exposed to callers).
    ///
    /// Expired-session reaping is done by the caller (`reap_expired`) in an
    /// async context so evicted sessions can be shut down and awaited; `park`
    /// itself never silently drops a live conversation.
    pub fn park(&self, parked: ParkedSession) -> String {
        let conversation_id = uuid::Uuid::new_v4().to_string();
        {
            let mut index = self.index.lock().unwrap();
            for call in &parked.pending {
                index.insert(call.openai_call_id.clone(), conversation_id.clone());
            }
        }
        self.slots.lock().unwrap().insert(
            conversation_id.clone(),
            Slot {
                parked,
                last_active: Instant::now(),
            },
        );
        conversation_id
    }

    /// Remove and return the session addressed by any one of `call_ids`,
    /// validating that they all resolve to the *same* parked conversation.
    /// Fails closed on unknown id, cross-session mixing, or expiry.
    pub fn take_for_call_ids(&self, call_ids: &[String]) -> Result<ParkedSession, BridgeError> {
        if call_ids.is_empty() {
            return Err(BridgeError::invalid_request(
                "continuation request referenced no tool call ids",
            ));
        }
        let conversation_id = {
            let index = self.index.lock().unwrap();
            let mut resolved: Option<String> = None;
            for id in call_ids {
                let Some(cid) = index.get(id) else {
                    return Err(BridgeError::session_state(format!(
                        "no active Grok session for tool call id '{id}' (it may have expired, \
                         been cancelled, or already been resumed)"
                    )));
                };
                match &resolved {
                    Some(existing) if existing != cid => {
                        return Err(BridgeError::invalid_request(
                            "tool results reference more than one Grok session",
                        ));
                    }
                    _ => resolved = Some(cid.clone()),
                }
            }
            resolved.expect("non-empty call_ids yields a conversation id")
        };

        let slot = self
            .slots
            .lock()
            .unwrap()
            .remove(&conversation_id)
            .ok_or_else(|| {
                BridgeError::session_state(
                    "the referenced Grok session is already being resumed or has been closed",
                )
            })?;
        // Drop this session's index entries — it is now owned by the caller.
        self.drop_index_for(&slot.parked.pending);

        if slot.last_active.elapsed() > self.ttl {
            // Expired in the race window after the request-level reap: report a
            // clean session-state failure. `slot.parked` is dropped here, which
            // triggers the conversation's `Drop` teardown (cancel token + abort
            // background tasks + kill child).
            return Err(BridgeError::session_state(format!(
                "the Grok session expired after {}s of inactivity",
                self.ttl.as_secs()
            )));
        }
        Ok(slot.parked)
    }

    fn drop_index_for(&self, pending: &[PendingCall]) {
        let mut index = self.index.lock().unwrap();
        for call in pending {
            index.remove(&call.openai_call_id);
        }
    }

    /// Evict sessions idle longer than the TTL. Returns the parked sessions so
    /// the caller can shut down their child processes off-lock.
    pub fn sweep_expired(&self) -> Vec<ParkedSession> {
        let mut evicted = Vec::new();
        let expired_ids: Vec<String> = {
            let slots = self.slots.lock().unwrap();
            slots
                .iter()
                .filter(|(_, s)| s.last_active.elapsed() > self.ttl)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in expired_ids {
            if let Some(slot) = self.slots.lock().unwrap().remove(&id) {
                self.drop_index_for(&slot.parked.pending);
                evicted.push(slot.parked);
            }
        }
        evicted
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::grok_tool_bridge::transport::fake::{FakeBackend, ScriptedTurn};
    use crate::api::grok_tool_bridge::transport::{BridgeBackend, PromptInput};

    async fn parked_with_ids(ttl: Duration, ids: &[(&str, &str)]) -> (SessionRegistry, String) {
        let backend = FakeBackend::new(vec![
            ScriptedTurn::ToolCalls(vec![("t".into(), serde_json::json!({}))]),
            ScriptedTurn::Complete("done".into()),
        ]);
        let (conversation, _outcome) = backend
            .open(PromptInput {
                prompt: "p".into(),
                model: None,
                tools_mcp: vec![],
            })
            .await
            .unwrap();
        let pending = ids
            .iter()
            .map(|(oid, pid)| PendingCall {
                openai_call_id: oid.to_string(),
                provider_call_id: pid.to_string(),
                name: "t".into(),
                arguments: serde_json::json!({}),
            })
            .collect();
        let registry = SessionRegistry::new(ttl);
        let cid = registry.park(ParkedSession {
            conversation,
            pending,
        });
        (registry, cid)
    }

    #[tokio::test]
    async fn park_then_take_roundtrips() {
        let (registry, _cid) =
            parked_with_ids(Duration::from_secs(60), &[("call_1", "acp-1")]).await;
        assert_eq!(registry.len(), 1);
        let parked = registry.take_for_call_ids(&["call_1".to_string()]).unwrap();
        assert_eq!(parked.pending.len(), 1);
        assert_eq!(registry.len(), 0);
        // Second take fails closed — it's been removed.
        assert!(registry.take_for_call_ids(&["call_1".to_string()]).is_err());
    }

    #[tokio::test]
    async fn unknown_call_id_is_session_state_error() {
        let (registry, _cid) =
            parked_with_ids(Duration::from_secs(60), &[("call_1", "acp-1")]).await;
        let err = registry
            .take_for_call_ids(&["call_missing".to_string()])
            .err()
            .unwrap();
        assert!(matches!(
            err.class,
            crate::api::grok_tool_bridge::error::BridgeErrorClass::SessionState
        ));
    }

    #[tokio::test]
    async fn cross_session_mixing_rejected() {
        let (registry, _c) = parked_with_ids(Duration::from_secs(60), &[("call_1", "acp-1")]).await;
        // Park a second, separate session.
        let backend = FakeBackend::new(vec![ScriptedTurn::Complete("x".into())]);
        let (conversation, _o) = backend
            .open(PromptInput {
                prompt: "p".into(),
                model: None,
                tools_mcp: vec![],
            })
            .await
            .unwrap();
        registry.park(ParkedSession {
            conversation,
            pending: vec![PendingCall {
                openai_call_id: "call_2".into(),
                provider_call_id: "acp-x".into(),
                name: "t".into(),
                arguments: serde_json::json!({}),
            }],
        });
        let err = registry
            .take_for_call_ids(&["call_1".to_string(), "call_2".to_string()])
            .err()
            .unwrap();
        assert!(err.message.contains("more than one Grok session"));
    }

    #[tokio::test]
    async fn expired_session_fails_closed() {
        let (registry, _cid) =
            parked_with_ids(Duration::from_millis(1), &[("call_1", "acp-1")]).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let err = registry
            .take_for_call_ids(&["call_1".to_string()])
            .err()
            .unwrap();
        assert!(err.message.contains("expired"));
    }
}
