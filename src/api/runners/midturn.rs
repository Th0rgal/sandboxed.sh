use std::time::Duration;

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::api::control::AgentEvent;

pub(crate) const MID_TURN_POLL: Duration = Duration::from_secs(5);

pub(crate) async fn drain_and_inject<F, Fut>(
    mission_id: Uuid,
    events_tx: &broadcast::Sender<AgentEvent>,
    inject: F,
) where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let Some(store) = crate::api::ask::ask_store_if_initialized() else {
        return;
    };
    drain_and_inject_from_store(store, mission_id, events_tx, inject).await;
}

// The injection closure is `async` and its `bool` MUST mean the note block was
// *actually accepted by the backend* — not merely queued for delivery. Backends
// whose delivery is deferred (e.g. Codex enqueues onto an inject channel that a
// driver task later turns into a `turn/start` RPC) must therefore await the real
// outcome before returning, so a failed RPC re-enqueues the notes here instead
// of silently dropping them after we already took them off the queue.
async fn drain_and_inject_from_store<F, Fut>(
    store: std::sync::Arc<crate::api::ask::AskStore>,
    mission_id: Uuid,
    events_tx: &broadcast::Sender<AgentEvent>,
    inject: F,
) where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let notes = match store.take_pending_operator_notes(mission_id).await {
        Ok(notes) if !notes.is_empty() => notes,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(
                mission_id = %mission_id,
                "Failed to drain operator notes for mid-turn injection: {error}"
            );
            return;
        }
    };

    let mut block = String::from("<operator-note>\n");
    for note in &notes {
        block.push_str(&note.body);
        block.push('\n');
    }
    block.push_str("</operator-note>");

    if inject(block.clone()).await {
        tracing::info!(
            mission_id = %mission_id,
            notes = notes.len(),
            "Injected operator notes mid-turn"
        );
        let _ = events_tx.send(AgentEvent::UserMessage {
            id: Uuid::new_v4(),
            content: block,
            queued: false,
            mission_id: Some(mission_id),
            source: None,
        });
        return;
    }

    for note in &notes {
        if let Err(error) = store
            .enqueue_operator_note(mission_id, &note.body, note.source_thread_id)
            .await
        {
            tracing::error!(
                mission_id = %mission_id,
                "Failed to re-enqueue operator note after injection failure: {error}"
            );
        }
    }
    tracing::warn!(
        mission_id = %mission_id,
        notes = notes.len(),
        "Mid-turn note injection failed; notes re-enqueued for next delivery"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::api::ask::AskStore;

    async fn temp_store() -> Arc<AskStore> {
        let path = std::env::temp_dir().join(format!("midturn-{}.db", Uuid::new_v4()));
        Arc::new(AskStore::open(path).await.unwrap())
    }

    #[tokio::test]
    async fn drain_and_inject_emits_event_on_success_and_requeues_on_failure() {
        let mission_id = Uuid::new_v4();
        let store = temp_store().await;
        let (events_tx, mut events_rx) = broadcast::channel(8);
        store
            .enqueue_operator_note(mission_id, "first note", None)
            .await
            .unwrap();
        store
            .enqueue_operator_note(mission_id, "second note", None)
            .await
            .unwrap();

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_for_inject = Arc::clone(&captured);
        drain_and_inject_from_store(Arc::clone(&store), mission_id, &events_tx, |block| {
            let captured_for_inject = Arc::clone(&captured_for_inject);
            async move {
                *captured_for_inject.lock().unwrap() = block.clone();
                true
            }
        })
        .await;

        let block = captured.lock().unwrap().clone();
        assert!(block.starts_with("<operator-note>"));
        assert!(block.contains("first note"));
        assert!(block.contains("second note"));
        assert!(matches!(
            events_rx.try_recv().unwrap(),
            AgentEvent::UserMessage { queued: false, .. }
        ));
        assert!(store
            .take_pending_operator_notes(mission_id)
            .await
            .unwrap()
            .is_empty());

        store
            .enqueue_operator_note(mission_id, "retry me", None)
            .await
            .unwrap();
        drain_and_inject_from_store(Arc::clone(&store), mission_id, &events_tx, |_| async {
            false
        })
        .await;

        let pending = store.take_pending_operator_notes(mission_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].body, "retry me");
    }
}
