//! Per-backend turn-runner support modules.
//!
//! This is the landing zone for the mission_runner.rs decomposition: shared,
//! backend-agnostic pieces (error classification, and eventually the per
//! backend turn runners themselves) move here so `mission_runner.rs` can
//! shrink down to orchestration (dispatch, retry/fallback, TerminalReason).

pub(crate) mod claudecode;
pub(crate) mod codex;
pub(crate) mod errors;
pub(crate) mod gemini;
pub(crate) mod grok;
pub(crate) mod midturn;
pub(crate) mod opencode;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::AgentResult;
use crate::api::control::{AgentEvent, ControlStatus, FrontendToolHub};
use crate::secrets::SecretsStore;
use crate::workspace::Workspace;

/// Everything a harness needs to run one turn.
///
/// The common fields are identical across all five backends; backend-specific
/// inputs travel in [`TurnExtras`]. Message framing (raw vs history-framed
/// `convo`, `/goal` passthrough) is the caller's responsibility — by the time
/// a `TurnContext` exists, `message` is exactly what the harness should see.
pub(crate) struct TurnContext<'a> {
    pub workspace: &'a Workspace,
    pub work_dir: &'a std::path::Path,
    pub message: &'a str,
    pub model: Option<&'a str>,
    pub model_effort: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub mission_id: Uuid,
    pub events_tx: broadcast::Sender<AgentEvent>,
    pub cancel: CancellationToken,
    pub app_working_dir: &'a std::path::Path,
    pub session_id: Option<&'a str>,
    pub is_continuation: bool,
    pub extras: TurnExtras<'a>,
}

/// Backend-specific turn inputs. One variant per harness so the dispatch
/// site can't pair a runner with the wrong extras silently — runners check
/// their variant and fall back to defaults (with a debug log) on mismatch.
#[derive(Default)]
pub(crate) enum TurnExtras<'a> {
    #[default]
    None,
    ClaudeCode {
        secrets: Option<Arc<SecretsStore>>,
        tool_hub: Option<Arc<FrontendToolHub>>,
        status: Option<Arc<RwLock<ControlStatus>>>,
        /// Conversation history, used to rebuild a condensed context when
        /// transport recovery rotates to a fresh session.
        history: &'a [(String, String)],
        max_history_total_chars: usize,
    },
}

/// One harness backend's turn execution, behind a uniform interface.
///
/// `run_turn` returns a boxed future *by construction*: the per-turn futures
/// are huge in debug builds and embedding them in a caller's state machine
/// reintroduces the async stack overflow this codebase already fixed once.
pub(crate) trait HarnessRunner: Send + Sync {
    fn name(&self) -> &'static str;
    fn mid_turn_kind(&self) -> MidTurnKind {
        MidTurnKind::None
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MidTurnKind {
    None,
    StreamJsonStdin,
    CodexAppServer,
}

pub(crate) struct ClaudeCodeRunner;
pub(crate) struct OpenCodeRunner;
pub(crate) struct CodexRunner;
pub(crate) struct GrokRunner;
pub(crate) struct GeminiRunner;

impl HarnessRunner for ClaudeCodeRunner {
    fn name(&self) -> &'static str {
        "claudecode"
    }
    fn mid_turn_kind(&self) -> MidTurnKind {
        MidTurnKind::StreamJsonStdin
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        let (secrets, tool_hub, status, history, max_history_total_chars) = match ctx.extras {
            TurnExtras::ClaudeCode {
                secrets,
                tool_hub,
                status,
                history,
                max_history_total_chars,
            } => (secrets, tool_hub, status, history, max_history_total_chars),
            _ => {
                tracing::debug!("ClaudeCodeRunner invoked without ClaudeCode extras");
                (None, None, None, &[][..], 0)
            }
        };
        Box::pin(claudecode::run_claudecode_turn_with_recovery(
            ctx.workspace,
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.model_effort,
            ctx.agent,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            secrets,
            ctx.app_working_dir,
            ctx.session_id,
            ctx.is_continuation,
            tool_hub,
            status,
            history,
            max_history_total_chars,
        ))
    }
}

impl HarnessRunner for OpenCodeRunner {
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        Box::pin(opencode::run_opencode_turn(
            ctx.workspace,
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.model_effort,
            ctx.agent,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
            ctx.session_id,
            ctx.is_continuation,
        ))
    }
}

impl HarnessRunner for CodexRunner {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn mid_turn_kind(&self) -> MidTurnKind {
        // Raw backend capability: the app-server accepts a second `turn/start`
        // on the live thread. NOTE: this is gated OFF in
        // `effective_mid_turn_kind` — the non-goal driver marks the turn
        // terminal on the first `turn/completed` (see codex/mod.rs), so an
        // injected turn would be abandoned. Re-enable once the driver tracks
        // injected turns (or a `turn/steer`-style append RPC is wired).
        MidTurnKind::CodexAppServer
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        // Credential pool + rotation + cooldown handling live inside the
        // rotation wrapper so every dispatch path gets identical behavior.
        Box::pin(codex::run_codex_turn_with_rotation(
            ctx.workspace,
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.model_effort,
            ctx.agent,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
            ctx.session_id,
        ))
    }
}

impl HarnessRunner for GrokRunner {
    fn name(&self) -> &'static str {
        "grok"
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        Box::pin(grok::run_grok_turn(
            ctx.workspace,
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
            ctx.session_id,
            ctx.is_continuation,
        ))
    }
}

impl HarnessRunner for GeminiRunner {
    fn name(&self) -> &'static str {
        "gemini"
    }
    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        Box::pin(gemini::run_gemini_turn(
            ctx.workspace,
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.agent,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
            ctx.session_id,
        ))
    }
}

/// Resolve a backend id to its turn runner.
pub(crate) fn runner_for(backend_id: &str) -> Option<&'static dyn HarnessRunner> {
    match backend_id {
        "claudecode" => Some(&ClaudeCodeRunner),
        "opencode" => Some(&OpenCodeRunner),
        "codex" => Some(&CodexRunner),
        "grok" => Some(&GrokRunner),
        "gemini" => Some(&GeminiRunner),
        _ => None,
    }
}

pub(crate) fn effective_mid_turn_kind(
    backend_id: &str,
    stream_input_enabled: bool,
    is_goal: bool,
) -> MidTurnKind {
    // `is_goal` is retained for API stability / future re-enable; Codex is
    // currently gated off entirely (see below), so it is unused for now.
    let _ = is_goal;
    match backend_id {
        "claudecode" if !stream_input_enabled => MidTurnKind::None,
        // Codex mid-turn injection is disabled: the app-server can start a
        // second turn, but the non-goal driver ends the mission on the first
        // `turn/completed`, so the injected turn is abandoned and the steer
        // never reaches the model. Fall back to the authoritative next-turn
        // path until the driver can consume an injected turn.
        "codex" => MidTurnKind::None,
        _ => runner_for(backend_id)
            .map(|runner| runner.mid_turn_kind())
            .unwrap_or(MidTurnKind::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_for_maps_every_backend() {
        for backend in ["claudecode", "opencode", "codex", "grok", "gemini"] {
            let runner = runner_for(backend).expect("runner exists");
            assert_eq!(runner.name(), backend);
        }
        assert!(runner_for("unknown").is_none());
        assert!(runner_for("").is_none());

        assert_eq!(
            runner_for("claudecode").unwrap().mid_turn_kind(),
            MidTurnKind::StreamJsonStdin
        );
        assert_eq!(
            runner_for("codex").unwrap().mid_turn_kind(),
            MidTurnKind::CodexAppServer
        );
        for backend in ["opencode", "grok", "gemini"] {
            assert_eq!(
                runner_for(backend).unwrap().mid_turn_kind(),
                MidTurnKind::None
            );
        }
        assert_eq!(
            effective_mid_turn_kind("claudecode", true, false),
            MidTurnKind::StreamJsonStdin
        );
        assert_eq!(
            effective_mid_turn_kind("claudecode", false, false),
            MidTurnKind::None
        );
        // Codex is gated off in effective_mid_turn_kind (driver can't consume
        // an injected turn yet) even though its raw mid_turn_kind is
        // CodexAppServer — regardless of goal mode.
        assert_eq!(
            effective_mid_turn_kind("codex", true, false),
            MidTurnKind::None
        );
        assert_eq!(
            effective_mid_turn_kind("codex", true, true),
            MidTurnKind::None
        );
        assert_eq!(
            effective_mid_turn_kind("opencode", true, false),
            MidTurnKind::None
        );
    }
}
