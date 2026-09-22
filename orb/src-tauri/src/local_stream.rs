//! Ordered, push-based output; a subscriber receives one snapshot, then deltas.
use serde::Serialize;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
pub struct Event {
    pub text: String,
    pub reset: bool,
    pub state: Option<crate::local_agents::PollState>,
}
#[derive(Default)]
struct State {
    text: String,
    next: u64,
    listeners: Vec<(u64, Channel<Event>)>,
}
#[derive(Default)]
pub struct Output(Mutex<State>, AtomicUsize);
pub struct ReaderGuard(Arc<Output>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0 .1.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Output {
    pub fn reader(self: &Arc<Self>) -> ReaderGuard {
        self.1.fetch_add(1, Ordering::SeqCst);
        ReaderGuard(self.clone())
    }
    pub fn drained(&self) -> bool {
        self.1.load(Ordering::SeqCst) == 0
    }
    pub fn snapshot(&self) -> String {
        self.0.lock().unwrap().text.clone()
    }
    pub fn append(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut state = self.0.lock().unwrap();
        state.text.push_str(text);
        let event = Event {
            text: text.into(),
            reset: false,
            state: None,
        };
        state
            .listeners
            .retain(|(_, channel)| channel.send(event.clone()).is_ok());
    }
    pub fn replace(&self, text: String) {
        let mut state = self.0.lock().unwrap();
        if state.text == text {
            return;
        }
        state.text = text.clone();
        let event = Event {
            text,
            reset: true,
            state: None,
        };
        state
            .listeners
            .retain(|(_, channel)| channel.send(event.clone()).is_ok());
    }
    pub fn subscribe(&self, channel: Channel<Event>) -> Result<u64, String> {
        let mut state = self.0.lock().unwrap();
        channel
            .send(Event {
                text: state.text.clone(),
                reset: true,
                state: None,
            })
            .map_err(|e| e.to_string())?;
        state.next += 1;
        let id = state.next;
        state.listeners.push((id, channel));
        Ok(id)
    }
    pub fn unsubscribe(&self, id: u64) {
        self.0
            .lock()
            .unwrap()
            .listeners
            .retain(|(key, _)| *key != id);
    }
}

/// Item identity separates commentary from final answers and final snapshots
/// replace streamed text rather than being appended for a second time.
#[derive(Default)]
pub struct CodexText {
    items: Vec<(String, String)>,
}
impl CodexText {
    pub fn apply(&mut self, value: &serde_json::Value, output: &Output) {
        let method = value["method"].as_str().unwrap_or("");
        let params = &value["params"];
        let (id, text, delta) = match method {
            "item/agentMessage/delta" => (
                params["itemId"].as_str().unwrap_or("assistant"),
                params["delta"].as_str().unwrap_or(""),
                true,
            ),
            "item/completed" if params["item"]["type"] == "agentMessage" => (
                params["item"]["id"].as_str().unwrap_or("assistant"),
                params["item"]["text"].as_str().unwrap_or(""),
                false,
            ),
            _ => return,
        };
        if let Some(index) = self.items.iter().position(|(key, _)| key == id) {
            if delta {
                self.items[index].1.push_str(text);
                if index + 1 == self.items.len() {
                    output.append(text);
                    return;
                }
            } else {
                self.items[index].1 = text.into();
            }
        } else {
            let separator = if self.items.is_empty() { "" } else { "\n\n" };
            self.items.push((id.into(), text.into()));
            output.append(&format!("{separator}{text}"));
            return;
        }
        output.replace(
            self.items
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn final_items_reconcile_without_duplication() {
        let output = Output::default();
        let mut state = CodexText::default();
        for event in [
            json!({"method":"item/started","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"prompt"}]}}}),
            json!({"method":"item/agentMessage/delta","params":{"itemId":"a","delta":"Checking"}}),
            json!({"method":"item/completed","params":{"item":{"id":"a","type":"agentMessage","text":"Checking."}}}),
            json!({"method":"item/agentMessage/delta","params":{"itemId":"b","delta":"Done"}}),
            json!({"method":"item/completed","params":{"item":{"id":"b","type":"agentMessage","text":"Done"}}}),
        ] {
            state.apply(&event, &output);
        }
        assert_eq!(output.snapshot(), "Checking.\n\nDone");
    }
    #[test]
    fn late_subscriber_gets_snapshot_then_only_new_text() {
        let output = Output::default();
        output.append("é");
        let events = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let channel = Channel::new(move |body| {
            captured.lock().unwrap().push(body);
            Ok(())
        });
        let token = output.subscribe(channel).unwrap();
        output.append("🙂");
        output.unsubscribe(token);
        output.append("hidden");
        assert_eq!(events.lock().unwrap().len(), 2);
    }
}
