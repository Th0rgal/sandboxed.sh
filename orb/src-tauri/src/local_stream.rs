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
    pub activities: Option<Vec<Activity>>,
    pub state: Option<crate::local_agents::PollState>,
}
#[derive(Default)]
struct State {
    text: String,
    next: u64,
    listeners: Vec<(u64, Channel<Event>)>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub id: String,
    pub label: String,
    pub done: bool,
    pub failed: bool,
    pub kind: String,
    pub background: bool,
    pub tool_use_id: Option<String>,
    pub detail: Option<String>,
    pub status: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub finished_at: Option<u64>,
}
fn activity_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl Activity {
    fn new(id: String, label: String, kind: &str, background: bool) -> Self {
        let now = activity_now();
        Self {
            id,
            label,
            kind: kind.into(),
            background,
            done: false,
            failed: false,
            tool_use_id: None,
            detail: None,
            status: "running".into(),
            started_at: now,
            updated_at: now,
            finished_at: None,
        }
    }
    fn finish(&mut self, status: &str) {
        self.done = true;
        self.failed = matches!(status, "failed" | "stopped");
        self.status = status.into();
        self.updated_at = activity_now();
        self.finished_at.get_or_insert(self.updated_at);
    }
}
fn task_kind(value: &serde_json::Value) -> &'static str {
    if value["task_type"]
        .as_str()
        .is_some_and(|s| s.contains("agent"))
        || value["subagent_type"].is_string()
    {
        "agent"
    } else {
        "command"
    }
}
fn upsert_task<'a>(
    activities: &'a mut Vec<Activity>,
    value: &serde_json::Value,
) -> Option<&'a mut Activity> {
    let id = format!("task:{}", value["task_id"].as_str()?);
    let existing = activities.iter().position(|a| a.id == id);
    let index = existing.unwrap_or_else(|| {
        activities.push(Activity::new(
            id,
            "Background task".into(),
            task_kind(value),
            true,
        ));
        activities.len() - 1
    });
    let activity = &mut activities[index];
    // A recovered snapshot has no reliable start timestamp.
    if value["subtype"] != "task_started" && existing.is_none() {
        activity.started_at = 0;
    } else if value["subtype"] == "task_started" && activity.started_at == 0 {
        activity.started_at = activity_now();
    }
    if let Some(label) = value["description"].as_str().filter(|s| !s.is_empty()) {
        activity.label = label.chars().take(240).collect();
    }
    if value["task_type"].is_string() || value["subagent_type"].is_string() {
        activity.kind = task_kind(value).into();
    }
    if let Some(id) = value["tool_use_id"].as_str() {
        activity.tool_use_id = Some(id.into());
    }
    activity.updated_at = activity_now();
    Some(activity)
}
#[derive(Default)]
pub struct Output(Mutex<State>, AtomicUsize, Mutex<Vec<Activity>>);

pub struct ReaderGuard(Arc<Output>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0 .1.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Output {
    pub fn activities(&self) -> Vec<Activity> {
        self.2.lock().unwrap().clone()
    }
    pub fn codex_reconnecting(&self, message: Option<&str>) {
        let mut activities = self.2.lock().unwrap();
        let index = activities.iter().position(|a| a.id == "codex:connection");
        if let Some(message) = message {
            let item = Activity::new(
                "codex:connection".into(),
                message.chars().take(160).collect(),
                "connection",
                false,
            );
            if let Some(index) = index {
                activities[index] = item;
            } else {
                activities.push(item);
            }
        } else if let Some(index) = index {
            if activities[index].done {
                return;
            }
            activities[index].label = "Connection restored".into();
            activities[index].finish("completed");
        } else {
            return;
        }
        drop(activities);
        self.publish_activities();
    }
    pub fn native_activity(&self, value: &serde_json::Value) {
        let method = value["method"].as_str().unwrap_or("");
        let opencode = value["type"] == "tool_use";
        if !opencode && !matches!(method, "item/started" | "item/completed") {
            return;
        }
        let item = if opencode {
            &value["part"]
        } else {
            &value["params"]["item"]
        };
        let kind = item["type"].as_str().unwrap_or("");
        let (label, category) = if opencode {
            (item["tool"].as_str().unwrap_or("Tool"), "tool")
        } else {
            match kind {
                "reasoning" => ("Thinking", "thinking"),
                "commandExecution" => ("Run command", "tool"),
                "dynamicToolCall" | "mcpToolCall" => {
                    (item["tool"].as_str().unwrap_or("Tool"), "tool")
                }
                "webSearch" => ("Search the web", "tool"),
                "fileChange" => ("Edit files", "tool"),
                "collabAgentToolCall" => ("Agent", "agent"),
                _ => return,
            }
        };
        let Some(id) = item[if opencode { "callID" } else { "id" }].as_str() else {
            return;
        };
        let id = format!("native:{id}");
        let mut activities = self.2.lock().unwrap();
        let index = activities
            .iter()
            .position(|a| a.id == id)
            .unwrap_or_else(|| {
                activities.push(Activity::new(id, label.into(), category, false));
                activities.len() - 1
            });
        let activity = &mut activities[index];
        let status = if opencode {
            item["state"]["status"].as_str()
        } else {
            item["status"].as_str()
        }
        .unwrap_or("");
        // Only public tool data; encrypted/raw reasoning is never rendered.
        if category != "thinking" {
            let data = if opencode { &item["state"] } else { item };
            activity.detail = Some(
                serde_json::to_string_pretty(data)
                    .unwrap_or_default()
                    .chars()
                    .take(8000)
                    .collect(),
            );
        }
        activity.updated_at = activity_now();
        if method == "item/completed"
            || (opencode && matches!(status, "completed" | "error" | "failed" | "cancelled"))
        {
            activity.finish(
                if matches!(status, "error" | "failed") || item["success"] == false {
                    "failed"
                } else if status == "cancelled" {
                    "stopped"
                } else {
                    "completed"
                },
            );
        }
    }
    pub fn claude_activity(&self, value: &serde_json::Value) {
        let mut activities = self.2.lock().unwrap();
        if value["type"] == "system" {
            match value["subtype"].as_str() {
                Some("task_started" | "task_progress") => {
                    if let Some(activity) = upsert_task(&mut activities, value) {
                        if let Some(tool) = value["last_tool_name"].as_str() {
                            activity.detail = Some(format!("Latest tool: {tool}"));
                        }
                    }
                }
                Some("task_notification") => {
                    if let Some(activity) = upsert_task(&mut activities, value) {
                        activity.finish(value["status"].as_str().unwrap_or("finished"));
                        if let Some(summary) = value["summary"].as_str().filter(|s| !s.is_empty()) {
                            activity.detail = Some(summary.chars().take(8000).collect());
                        }
                    }
                }
                Some("background_tasks_changed") => {
                    if let Some(tasks) = value["tasks"].as_array() {
                        for task in tasks {
                            upsert_task(&mut activities, task);
                        }
                        for activity in activities.iter_mut().filter(|a| a.background && !a.done) {
                            if !tasks
                                .iter()
                                .any(|t| t["task_id"].as_str() == activity.id.strip_prefix("task:"))
                            {
                                // Removal says it finished, not whether it succeeded.
                                activity.finish("finished");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if value["type"] == "assistant" {
            if let Some(blocks) = value["message"]["content"].as_array() {
                for block in blocks.iter().filter(|b| b["type"] == "tool_use") {
                    if let Some(activity) = activities
                        .iter_mut()
                        .find(|a| Some(a.id.as_str()) == block["id"].as_str())
                    {
                        if let Some(label) = block["input"]["description"].as_str() {
                            activity.label = label.chars().take(240).collect();
                        }
                        if let Some(command) = block["input"]["command"].as_str() {
                            activity.detail = Some(command.chars().take(8000).collect());
                        }
                    }
                }
            }
        }
        let event = &value["event"];
        if value["type"] == "stream_event" && event["type"] == "content_block_start" {
            let block = &event["content_block"];
            let thinking = block["type"] == "thinking";
            if thinking || block["type"] == "tool_use" {
                let id = if thinking {
                    format!("thinking:{}", activities.len())
                } else {
                    block["id"].as_str().unwrap_or("").to_owned()
                };
                if !activities.iter().any(|a| a.id == id) {
                    let name = block["name"].as_str().unwrap_or("Tool");
                    activities.push(Activity::new(
                        id,
                        if thinking {
                            "Thinking".into()
                        } else {
                            name.into()
                        },
                        if thinking {
                            "thinking"
                        } else if name == "Agent" {
                            "agent"
                        } else if name == "Bash" {
                            "command"
                        } else {
                            "tool"
                        },
                        false,
                    ));
                }
            }
        }
        if value["type"] == "stream_event" && event["type"] == "content_block_stop" {
            if let Some(last) = activities
                .last_mut()
                .filter(|a| a.id.starts_with("thinking:"))
            {
                last.finish("completed");
            }
        }
        if value["type"] == "user" {
            if let Some(blocks) = value["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "tool_result" {
                        if let Some(activity) = activities
                            .iter_mut()
                            .find(|a| Some(a.id.as_str()) == block["tool_use_id"].as_str())
                        {
                            activity.finish(if block["is_error"] == true {
                                "failed"
                            } else {
                                "completed"
                            });
                        }
                    }
                }
            }
        }
    }
    pub fn publish_activities(&self) {
        let activities = self.activities();
        let event = Event {
            text: String::new(),
            reset: false,
            state: None,
            activities: Some(activities),
        };
        self.0
            .lock()
            .unwrap()
            .listeners
            .retain(|(_, channel)| channel.send(event.clone()).is_ok());
    }
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
            activities: None,
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
            activities: None,
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
                activities: None,
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
    #[test]
    fn native_tools_preserve_identity_and_terminal_errors() {
        let output = Output::default();
        output.native_activity(&serde_json::json!({"method":"item/started","params":{"item":{"id":"a","type":"dynamicToolCall","tool":"exec"}}}));
        assert!(!output.activities()[0].done);
        output.native_activity(&serde_json::json!({"method":"item/completed","params":{"item":{"id":"a","type":"dynamicToolCall","tool":"exec","success":false}}}));
        assert_eq!(output.activities().len(), 1);
        assert!(output.activities()[0].failed);
        output.native_activity(&serde_json::json!({"type":"tool_use","part":{"callID":"b","tool":"webfetch","state":{"status":"error","error":"403"}}}));
        assert!(output.activities()[1].done && output.activities()[1].failed);
        output.native_activity(&serde_json::json!({"method":"item/started","params":{"item":{"id":"c","type":"reasoning","encrypted_content":"private"}}}));
        assert_eq!(output.activities()[2].detail, None);
    }

    use serde_json::json;
    #[test]
    fn background_snapshot_recovers_task_and_notification_reports_failure() {
        let output = Output::default();
        output.claude_activity(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a","task_type":"local_agent","description":"Inspect importer"}]}));
        assert_eq!(output.activities()[0].kind, "agent");
        assert!(!output.activities()[0].done);
        output.claude_activity(
            &json!({"type":"system","subtype":"task_started","task_id":"a","tool_use_id":"spawn"}),
        );
        assert_eq!(output.activities().len(), 1);
        output.claude_activity(
            &json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        );
        assert_eq!(output.activities()[0].status, "finished");
        output.claude_activity(&json!({"type":"system","subtype":"task_notification","task_id":"a","status":"failed","summary":"Build exited with code 1"}));
        let task = output.activities().remove(0);
        assert!(task.failed);
        assert_eq!(task.tool_use_id.as_deref(), Some("spawn"));
        assert_eq!(task.detail.as_deref(), Some("Build exited with code 1"));
        assert!(task.finished_at.unwrap() >= task.started_at);
    }
    #[test]
    fn background_agent_remains_active_after_spawn_tool_returns() {
        let output = Output::default();
        output.claude_activity(&json!({"type":"system","subtype":"task_started","task_id":"a","tool_use_id":"t","description":"Explore importer"}));
        output.claude_activity(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t"}]}}));
        assert!(!output.activities()[0].done);
        assert_eq!(output.activities()[0].label, "Explore importer");
        output.claude_activity(
            &json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        );
        assert!(output.activities()[0].done);
    }
    #[test]
    fn claude_activity_tracks_thinking_and_tool_results() {
        let output = Output::default();
        output.claude_activity(&json!({"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"thinking"}}}));
        assert!(!output.activities()[0].done);
        output
            .claude_activity(&json!({"type":"stream_event","event":{"type":"content_block_stop"}}));
        output.claude_activity(&json!({"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"t1","name":"Bash"}}}));
        output
            .claude_activity(&json!({"type":"stream_event","event":{"type":"content_block_stop"}}));
        assert!(output.activities()[0].done);
        assert!(!output.activities()[1].done); // End of arguments is not tool completion.
        output.claude_activity(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"failed"}]}}));
        assert!(output.activities()[1].done);
        assert!(output.activities()[1].failed);
        assert_eq!(output.activities()[1].label, "Bash");
        assert_eq!(output.snapshot(), ""); // No arguments/results mixed into assistant prose.
    }

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
