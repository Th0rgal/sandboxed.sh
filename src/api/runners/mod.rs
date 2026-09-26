//! Per-backend turn-runner support modules.
//!
//! This is the landing zone for the mission_runner.rs decomposition: shared,
//! backend-agnostic pieces (error classification, and eventually the per
//! backend turn runners themselves) move here so `mission_runner.rs` can
//! shrink down to orchestration (dispatch, retry/fallback, TerminalReason).

pub(crate) mod chatgpt_ui;
pub(crate) mod chatgpt_ui_jobs;
pub(crate) mod claudecode;
pub(crate) mod codex;
pub(crate) mod errors;
pub(crate) mod gemini;
pub(crate) mod grok;
pub(crate) mod midturn;
pub(crate) mod opencode;
pub(crate) mod stream_guard;

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

tokio::task_local! {
    /// Set inside each spawned turn from its already-acquired lease. Backend
    /// event producers and direct native persistence share this immutable stamp.
    pub(crate) static SESSION_UPDATE_RUN: Option<super::mission_store::SessionUpdateRun>;
}

pub(crate) fn session_update_run() -> Option<super::mission_store::SessionUpdateRun> {
    SESSION_UPDATE_RUN.try_with(Clone::clone).ok().flatten()
}

/// Commit native identity before completion/rotation can admit another turn.
/// Broadcast is a notification only; delivery may occur after a successor starts.
pub(crate) async fn persist_and_publish_native_session(
    store: Option<&Arc<dyn super::mission_store::MissionStore>>,
    mission_id: Uuid,
    backend: &str,
    session_id: &str,
    events: &broadcast::Sender<AgentEvent>,
) -> Result<(), Box<AgentResult>> {
    let run = session_update_run();
    let accepted = match store {
        Some(store) => {
            store
                .update_mission_session_id(mission_id, session_id, backend, run.as_ref())
                .await
        }
        None => Err("mission session store is unavailable".to_string()),
    };
    match accepted {
        Ok(true) => {
            let _ = events.send(AgentEvent::SessionIdUpdate {
                mission_id,
                backend: backend.to_string(),
                session_id: session_id.to_string(),
                run,
            });
            Ok(())
        }
        result => {
            let reason = result
                .err()
                .unwrap_or_else(|| "stale or unattributed execution generation".to_string());
            Err(Box::new(
                AgentResult::failure(
                    format!("{backend} native session persistence failed: {reason}"),
                    0,
                )
                .with_terminal_reason(crate::agents::TerminalReason::NativeContinuityRequired),
            ))
        }
    }
}

/// Everything a harness needs to run one turn.
///
/// The common fields are identical across all harness backends; backend-specific
/// inputs travel in [`TurnExtras`]. Message framing (raw vs history-framed
/// `convo`, `/goal` passthrough) is the caller's responsibility — by the time
/// a `TurnContext` exists, `message` is exactly what the harness should see.
pub(crate) struct TurnContext<'a> {
    pub mission_store: Option<Arc<dyn super::mission_store::MissionStore>>,
    pub workspace: &'a Workspace,
    pub work_dir: &'a std::path::Path,
    pub message: &'a str,
    pub model: Option<&'a str>,
    pub model_effort: Option<&'a str>,
    pub fast_mode: bool,
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
    Codex {
        current_message: &'a str,
        tool_hub: Option<Arc<FrontendToolHub>>,
    },
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
pub(crate) struct ChatGptUiRunner;

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
            ctx.mission_store,
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
            ctx.mission_store,
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
            ctx.fast_mode,
            ctx.agent,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
            ctx.session_id,
            match &ctx.extras {
                TurnExtras::Codex {
                    current_message, ..
                } => current_message,
                _ => ctx.message,
            },
            ctx.is_continuation,
            match ctx.extras {
                TurnExtras::Codex { tool_hub, .. } => tool_hub,
                _ => None,
            },
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
        let Some(mission_store) = ctx.mission_store else {
            return Box::pin(async {
                AgentResult::failure("Grok session persistence store is unavailable", 0)
                    .with_terminal_reason(crate::agents::TerminalReason::NativeContinuityRequired)
            });
        };
        Box::pin(grok::run_grok_turn(
            mission_store,
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

impl HarnessRunner for ChatGptUiRunner {
    fn name(&self) -> &'static str {
        "chatgpt_ui"
    }

    fn run_turn<'a>(
        &'a self,
        ctx: TurnContext<'a>,
    ) -> Pin<Box<dyn Future<Output = AgentResult> + Send + 'a>> {
        Box::pin(chatgpt_ui::run_chatgpt_ui_turn(
            ctx.work_dir,
            ctx.message,
            ctx.model,
            ctx.mission_id,
            ctx.events_tx,
            ctx.cancel,
            ctx.app_working_dir,
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
        "chatgpt_ui" => Some(&ChatGptUiRunner),
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
    #[tokio::test]
    async fn queued_successor_keeps_acknowledged_session_with_delayed_actor_delivery() {
        use crate::api::mission_store::{
            FileMissionStore, InMemoryMissionStore, MissionStore, SessionUpdateRun,
            SqliteMissionStore,
        };
        for kind in ["memory", "file", "sqlite"] {
            for backend in ["opencode", "claudecode"] {
                let dir = tempfile::tempdir().unwrap();
                let mut store: Arc<dyn MissionStore> = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(dir.path().into(), "queued")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(dir.path().into(), "queued")
                            .await
                            .unwrap(),
                    ),
                    _ => Arc::new(InMemoryMissionStore::new()),
                };
                let mission = store
                    .create_mission(
                        Some("queued identity"),
                        None,
                        None,
                        None,
                        None,
                        Some(backend),
                        None,
                    )
                    .await
                    .unwrap();
                let old = store
                    .begin_mission_run(mission.id, "first", None)
                    .await
                    .unwrap();
                let old_stamp = SessionUpdateRun::from(&old);
                let (events, mut delayed) = broadcast::channel(8);
                super::SESSION_UPDATE_RUN
                    .scope(
                        Some(old_stamp.clone()),
                        super::persist_and_publish_native_session(
                            Some(&store),
                            mission.id,
                            backend,
                            "native-first",
                            &events,
                        ),
                    )
                    .await
                    .unwrap();
                // Acknowledgement includes durable storage, not only an
                // in-memory projection. Reopen persistent implementations.
                store = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(dir.path().into(), "queued")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(dir.path().into(), "queued")
                            .await
                            .unwrap(),
                    ),
                    _ => store,
                };
                // Model the actor selecting completion before consuming the
                // broadcast, then refreshing identity and acquiring a successor.
                assert!(store
                    .finish_mission_run(old.run_id, old.generation, Some("turn_complete"))
                    .await
                    .unwrap());
                let refreshed = store.get_mission(mission.id).await.unwrap().unwrap();
                assert_eq!(
                    refreshed.session_id.as_deref(),
                    Some("native-first"),
                    "{kind}/{backend}"
                );
                let successor = store
                    .begin_mission_run(mission.id, "queued", None)
                    .await
                    .unwrap();
                assert!(successor.generation > old.generation);
                let AgentEvent::SessionIdUpdate {
                    mission_id,
                    session_id,
                    backend: source,
                    run,
                } = delayed.try_recv().unwrap()
                else {
                    panic!("expected identity notification")
                };
                assert_eq!(run.as_ref(), Some(&old_stamp));
                // The old notification is stale, but the acknowledged binding
                // was already available to the successor before acquisition.
                assert!(!store
                    .update_mission_session_id(mission_id, &session_id, &source, run.as_ref())
                    .await
                    .unwrap());
                assert_eq!(
                    store
                        .get_mission(mission.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .session_id
                        .as_deref(),
                    Some("native-first")
                );
                let error = super::SESSION_UPDATE_RUN
                    .scope(
                        Some(old_stamp),
                        super::persist_and_publish_native_session(
                            Some(&store),
                            mission.id,
                            backend,
                            "late-wrong",
                            &events,
                        ),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.terminal_reason,
                    Some(crate::agents::TerminalReason::NativeContinuityRequired)
                );
                assert!(matches!(
                    delayed.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty)
                ));
                assert_eq!(
                    store
                        .get_mission(mission.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .session_id
                        .as_deref(),
                    Some("native-first")
                );
                // Missing durable authority must not emit a successful binding.
                let error = super::persist_and_publish_native_session(
                    None,
                    mission.id,
                    backend,
                    "uncommitted",
                    &events,
                )
                .await
                .unwrap_err();
                assert_eq!(
                    error.terminal_reason,
                    Some(crate::agents::TerminalReason::NativeContinuityRequired)
                );
                assert!(matches!(
                    delayed.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty)
                ));
            }
        }
    }

    #[tokio::test]
    async fn native_session_stamp_is_captured_per_turn_not_delivery() {
        use crate::api::mission_store::SessionUpdateRun;
        let first = SessionUpdateRun {
            run_id: uuid::Uuid::new_v4(),
            generation: 1,
        };
        let second = SessionUpdateRun {
            run_id: uuid::Uuid::new_v4(),
            generation: 2,
        };
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for stamp in [first.clone(), second.clone()] {
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(super::SESSION_UPDATE_RUN.scope(
                Some(stamp),
                async move {
                    barrier.wait().await;
                    tokio::task::yield_now().await;
                    super::session_update_run()
                },
            )));
        }
        assert_eq!(tasks.remove(0).await.unwrap(), Some(first));
        assert_eq!(tasks.remove(0).await.unwrap(), Some(second));
        assert_eq!(super::session_update_run(), None);
    }

    use super::*;

    #[test]
    fn runner_for_maps_every_backend() {
        for backend in [
            "claudecode",
            "opencode",
            "codex",
            "grok",
            "gemini",
            "chatgpt_ui",
        ] {
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
        for backend in ["opencode", "grok", "gemini", "chatgpt_ui"] {
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
