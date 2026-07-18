//! Transport boundary between the bridge state machine and a real Grok
//! `agent stdio` (ACP) child hosting an ephemeral caller-tool MCP server.
//!
//! Everything above this trait is pure and deterministic; everything below it
//! talks to a live CLI. Tests drive the state machine through [`FakeBackend`],
//! which reproduces the exact ACP/MCP transcript (Grok invokes a caller tool,
//! the invocation is *suspended* rather than executed, and the later caller
//! result unblocks the same session) without spawning a process.

use async_trait::async_trait;

use super::error::BridgeError;

/// Token accounting reported by the connected account. Kept truthful: absent
/// when the backend did not report usage rather than fabricated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl BridgeUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// A caller tool Grok invoked over the ephemeral MCP server. `provider_call_id`
/// is the ACP/MCP tool-call id the transport must correlate the later result
/// against; the state machine maps it to a stable OpenAI `call_...` id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedToolCall {
    pub provider_call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A caller-supplied result routed back to a suspended MCP invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolResult {
    pub provider_call_id: String,
    pub content: String,
}

/// The completed assistant turn (no further tool calls pending).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedTurn {
    pub text: String,
    pub model: String,
    pub usage: BridgeUsage,
    pub stop_reason: String,
}

/// Outcome of a prompt or resume: either Grok paused to request caller tools,
/// or it finished the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    ToolCalls {
        model: String,
        calls: Vec<RequestedToolCall>,
    },
    Completed(CompletedTurn),
}

/// Inputs for opening a fresh Grok session.
#[derive(Debug, Clone)]
pub struct PromptInput {
    pub prompt: String,
    pub model: Option<String>,
    /// Caller tools rendered as MCP descriptors (`name`/`description`/
    /// `inputSchema`). Empty means no caller tools were offered.
    pub tools_mcp: Vec<serde_json::Value>,
}

/// One live Grok conversation: owns the `agent stdio` child, the ephemeral MCP
/// server, and any suspended tool invocations. Held in the session registry
/// between HTTP requests.
#[async_trait]
pub trait BridgeConversation: Send + Sync {
    /// The underlying account session id (for provenance/logging).
    fn session_id(&self) -> &str;

    /// Deliver caller tool results to the suspended MCP invocations and resume
    /// the turn. Returns the next outcome (more tool calls, or completion).
    async fn resume(
        &mut self,
        results: Vec<ResolvedToolResult>,
    ) -> Result<TurnOutcome, BridgeError>;

    /// Tear down the child + MCP server. Best-effort; always fail-closed.
    async fn shutdown(&mut self);
}

/// Opens conversations against a backend (real CLI or a test fake).
#[async_trait]
pub trait BridgeBackend: Send + Sync {
    /// Spawn a session, send the initial prompt, and return the conversation
    /// handle together with the first turn outcome.
    async fn open(
        &self,
        input: PromptInput,
    ) -> Result<(Box<dyn BridgeConversation>, TurnOutcome), BridgeError>;
}

// ───────────────────────── Test fake ─────────────────────────
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A scripted turn the fake Grok will produce.
    #[derive(Debug, Clone)]
    pub enum ScriptedTurn {
        /// Grok invokes caller tools (name, arguments) and *suspends* — it does
        /// NOT execute them itself.
        ToolCalls(Vec<(String, serde_json::Value)>),
        /// Grok finishes with assistant text.
        Complete(String),
    }

    /// Records what actually happened so tests can assert the contract:
    /// the caller tools were never executed by the fake, and the results the
    /// caller submitted were the ones routed back.
    #[derive(Debug, Default)]
    pub struct Transcript {
        pub opened_with_tools: Vec<String>,
        pub prompt: String,
        pub model: Option<String>,
        pub results_received: Vec<Vec<ResolvedToolResult>>,
        pub executed_tools: Vec<String>, // must stay empty: fake never executes
        pub shutdowns: usize,
    }

    pub struct FakeBackend {
        script: Vec<ScriptedTurn>,
        pub transcript: Arc<Mutex<Transcript>>,
        /// If set, `open` fails with this error (capability/infra simulation).
        open_error: Option<BridgeError>,
    }

    impl FakeBackend {
        pub fn new(script: Vec<ScriptedTurn>) -> Self {
            Self {
                script,
                transcript: Arc::new(Mutex::new(Transcript::default())),
                open_error: None,
            }
        }

        pub fn failing(err: BridgeError) -> Self {
            Self {
                script: Vec::new(),
                transcript: Arc::new(Mutex::new(Transcript::default())),
                open_error: Some(err),
            }
        }

        pub fn transcript(&self) -> Arc<Mutex<Transcript>> {
            Arc::clone(&self.transcript)
        }
    }

    fn outcome_for(turn: &ScriptedTurn, idx: usize) -> TurnOutcome {
        match turn {
            ScriptedTurn::ToolCalls(calls) => TurnOutcome::ToolCalls {
                model: "grok-4.5".to_string(),
                calls: calls
                    .iter()
                    .enumerate()
                    .map(|(i, (name, args))| RequestedToolCall {
                        provider_call_id: format!("acp-{idx}-{i}"),
                        name: name.clone(),
                        arguments: args.clone(),
                    })
                    .collect(),
            },
            ScriptedTurn::Complete(text) => TurnOutcome::Completed(CompletedTurn {
                text: text.clone(),
                model: "grok-4.5".to_string(),
                usage: BridgeUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
                stop_reason: "stop".to_string(),
            }),
        }
    }

    struct FakeConversation {
        script: Vec<ScriptedTurn>,
        cursor: usize,
        transcript: Arc<Mutex<Transcript>>,
    }

    #[async_trait]
    impl BridgeConversation for FakeConversation {
        fn session_id(&self) -> &str {
            "fake-session"
        }

        async fn resume(
            &mut self,
            results: Vec<ResolvedToolResult>,
        ) -> Result<TurnOutcome, BridgeError> {
            self.transcript
                .lock()
                .unwrap()
                .results_received
                .push(results);
            self.cursor += 1;
            let turn = self
                .script
                .get(self.cursor)
                .cloned()
                .ok_or_else(|| BridgeError::upstream("fake script exhausted"))?;
            Ok(outcome_for(&turn, self.cursor))
        }

        async fn shutdown(&mut self) {
            self.transcript.lock().unwrap().shutdowns += 1;
        }
    }

    #[async_trait]
    impl BridgeBackend for FakeBackend {
        async fn open(
            &self,
            input: PromptInput,
        ) -> Result<(Box<dyn BridgeConversation>, TurnOutcome), BridgeError> {
            if let Some(err) = &self.open_error {
                return Err(err.clone());
            }
            {
                let mut t = self.transcript.lock().unwrap();
                t.prompt = input.prompt.clone();
                t.model = input.model.clone();
                t.opened_with_tools = input
                    .tools_mcp
                    .iter()
                    .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect();
            }
            let first = self
                .script
                .first()
                .cloned()
                .ok_or_else(|| BridgeError::upstream("fake script empty"))?;
            let outcome = outcome_for(&first, 0);
            let convo = FakeConversation {
                script: self.script.clone(),
                cursor: 0,
                transcript: Arc::clone(&self.transcript),
            };
            Ok((Box::new(convo), outcome))
        }
    }
}
