//! OpenCode-backed agent - delegates task execution to an external OpenCode server.

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{Agent, AgentContext, AgentId, AgentResult, AgentType, TerminalReason};
use crate::api::control::AgentTreeNode;
use crate::config::Config;
use crate::opencode::{extract_text, OpenCodeClient};
use crate::task::Task;

pub struct OpenCodeAgent {
    id: AgentId,
    client: OpenCodeClient,
    default_agent: Option<String>,
}

impl OpenCodeAgent {
    pub fn new(config: Config) -> Self {
        let base_url = config
            .opencode_base_url
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:4096".to_string());
        let client = OpenCodeClient::new(
            base_url,
            config.opencode_agent.clone(),
            config.opencode_permissive,
        );
        Self {
            id: AgentId::new(),
            client,
            default_agent: config.opencode_agent,
        }
    }

    fn build_tree(&self, task_desc: &str, budget_cents: u64) -> AgentTreeNode {
        let mut root = AgentTreeNode::new("root", "OpenCode", "OpenCode Agent", task_desc)
            .with_budget(budget_cents, 0)
            .with_status("running");

        root.add_child(
            AgentTreeNode::new("opencode", "OpenCodeSession", "OpenCode Session", "Delegating to OpenCode")
                .with_budget(budget_cents, 0)
                .with_status("running"),
        );

        root
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Root
    }

    fn description(&self) -> &str {
        "OpenCode agent: delegates task execution to an OpenCode server"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        let task_desc = task.description().chars().take(60).collect::<String>();
        let budget_cents = task.budget().total_cents();

        let mut tree = self.build_tree(&task_desc, budget_cents);
        ctx.emit_tree(tree.clone());
        ctx.emit_phase(
            "executing",
            Some("Delegating to OpenCode server"),
            Some("OpenCodeAgent"),
        );

        if ctx.is_cancelled() {
            return AgentResult::failure("Task cancelled", 0).with_terminal_reason(TerminalReason::Cancelled);
        }

        // OpenCode requires an absolute path
        let directory = std::fs::canonicalize(&ctx.working_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ctx.working_dir_str());
        let title = Some(task_desc.as_str());

        let session = match self.client.create_session(&directory, title).await {
            Ok(s) => s,
            Err(e) => {
                tree.status = "failed".to_string();
                ctx.emit_tree(tree);
                return AgentResult::failure(format!("OpenCode session error: {}", e), 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        // Choose model: requested override > config default
        let model_override: Option<String> = task
            .analysis()
            .requested_model
            .clone()
            .or_else(|| Some(ctx.config.default_model.clone()));
        if let Some(ref model) = model_override {
            task.analysis_mut().selected_model = Some(model.clone());
        }

        let agent_name = self.default_agent.as_deref();
        let response = if let Some(cancel) = ctx.cancel_token.clone() {
            tokio::select! {
                res = self.client.send_message(&session.id, &directory, task.description(), model_override.as_deref(), agent_name) => res,
                _ = cancel.cancelled() => {
                    let _ = self.client.abort_session(&session.id, &directory).await;
                    return AgentResult::failure("Task cancelled", 0).with_terminal_reason(TerminalReason::Cancelled);
                }
            }
        } else {
            self.client
                .send_message(&session.id, &directory, task.description(), model_override.as_deref(), agent_name)
                .await
        };

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                tree.status = "failed".to_string();
                if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                    node.status = "failed".to_string();
                }
                ctx.emit_tree(tree);
                return AgentResult::failure(format!("OpenCode message error: {}", e), 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        if response.info.error.is_some() {
            tree.status = "failed".to_string();
            if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                node.status = "failed".to_string();
            }
            ctx.emit_tree(tree);
            return AgentResult::failure("OpenCode returned an error response", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }

        let output = extract_text(&response.parts);

        if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
            node.status = "completed".to_string();
        }
        tree.status = "completed".to_string();
        ctx.emit_tree(tree);

        let model_used = match (&response.info.provider_id, &response.info.model_id) {
            (Some(provider), Some(model)) => Some(format!("{}/{}", provider, model)),
            _ => None,
        };

        AgentResult {
            success: true,
            output,
            cost_cents: 0,
            model_used,
            data: Some(json!({
                "agent": "OpenCodeAgent",
                "session_id": session.id,
            })),
            terminal_reason: None,
        }
    }
}
