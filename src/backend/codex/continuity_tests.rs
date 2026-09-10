//! Real stdio/RPC regressions using an isolated synthetic app-server process.
use super::{client::CodexConfig, continuity, tool_call_journal::ToolCallJournal, CodexBackend};
use crate::backend::{events::ExecutionEvent, Backend, SessionConfig};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Fixture {
    dir: tempfile::TempDir,
    config: continuity::Config,
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let cli = dir.path().join("app-server");
        std::fs::write(
            &cli,
            include_str!("../../../tests/fixtures/codex_continuity_app_server.py"),
        )
        .unwrap();
        std::fs::set_permissions(cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mission = Uuid::new_v4();
        let config = continuity::Config {
            path: continuity::binding_path(dir.path(), mission),
            identity: continuity::Identity::new(
                mission,
                Uuid::new_v4(),
                dir.path(),
                dir.path(),
                continuity::account_fingerprint("oauth", "synthetic-account"),
            )
            .unwrap(),
            projected_session: None,
            current_message: "new input only".into(),
        };
        Self { dir, config }
    }

    fn backend_config(&self, mode: &str, cancel: CancellationToken) -> CodexConfig {
        CodexConfig {
            cli_path: self
                .dir
                .path()
                .join("app-server")
                .to_string_lossy()
                .into_owned(),
            continuity: Some(self.config.clone()),
            cancel_token: Some(cancel),
            extra_env: HashMap::from([
                (
                    "CONTINUITY_FIXTURE_DIR".into(),
                    self.dir.path().to_string_lossy().into_owned(),
                ),
                ("CONTINUITY_FIXTURE_MODE".into(), mode.into()),
            ]),
            ..Default::default()
        }
    }

    async fn start(
        &self,
        cfg: CodexConfig,
        message: &str,
    ) -> anyhow::Result<(
        tokio::sync::mpsc::Receiver<ExecutionEvent>,
        tokio::task::JoinHandle<()>,
    )> {
        let backend = CodexBackend::with_config(cfg);
        let session = backend
            .create_session(SessionConfig {
                directory: self.config.identity.cwd.to_string_lossy().into_owned(),
                title: None,
                model: Some("synthetic-model".into()),
                agent: None,
            })
            .await?;
        assert!(!session.id.starts_with("native-thread-"));
        tokio::time::timeout(
            Duration::from_secs(5),
            backend.send_message_streaming(&session, message),
        )
        .await?
    }

    async fn run(&self, mode: &str, message: &str) -> Vec<ExecutionEvent> {
        let (mut rx, handle) = self
            .start(self.backend_config(mode, CancellationToken::new()), message)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            handle.await.unwrap();
            events
        })
        .await
        .expect("synthetic native run terminates")
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.path().join("native.json")).unwrap())
            .unwrap()
    }

    fn write_state(&self, state: Value) {
        std::fs::write(
            self.dir.path().join("native.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|request| request["method"] == method)
            .map(|request| request["params"].clone())
            .collect()
    }

    fn assert_processes_stopped(&self) {
        for pid in std::fs::read_to_string(self.dir.path().join("pids"))
            .unwrap()
            .lines()
        {
            // shutdown waits for the child: no remaining app-server owns the binding.
            assert_eq!(unsafe { libc::kill(pid.parse().unwrap(), 0) }, -1);
        }
    }
}

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn codex_continuity_restarts_same_native_thread_without_transcript_replay() {
    let f = Fixture::new();
    let first = f.run("normal", "framed initial instructions").await;
    let second = f
        .run("normal", "MUST NOT REPLAY transcript and instructions")
        .await;
    assert_eq!(f.requests("thread/start").len(), 1);
    assert_eq!(f.requests("thread/resume").len(), 1);
    assert_eq!(
        f.state()["inputs"],
        json!(["framed initial instructions", "new input only"])
    );
    for events in [first, second] {
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::CodexSessionBound { thread_id, goal_mode: false } if thread_id == "native-thread-1")));
    }
    assert_eq!(
        continuity::read(&f.config.path)
            .unwrap()
            .unwrap()
            .thread_id
            .as_deref(),
        Some("native-thread-1")
    );
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_resume_applies_model_effort_and_clears_fast_tier() {
    let f = Fixture::new();
    f.run("normal", "first").await;
    let mut cfg = f.backend_config("normal", CancellationToken::new());
    cfg.model_effort = Some("high".into());
    let (mut rx, handle) = f.start(cfg, "outer transcript").await.unwrap();
    while rx.recv().await.is_some() {}
    handle.await.unwrap();
    let params = &f.requests("thread/resume")[0];
    assert_eq!(params["model"], "synthetic-model");
    assert_eq!(params["config"]["model_reasoning_effort"], "high");
    assert!(params.as_object().unwrap().contains_key("serviceTier"));
    assert!(params["serviceTier"].is_null());
    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandbox"], "danger-full-access");
    assert!(params.get("history").is_none());
}

#[tokio::test]
async fn codex_continuity_goal_resume_preserves_objective_budget_and_accumulated_usage() {
    let mut f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    let before = f.state()["goal"].clone();
    f.config.current_message = "/goal synthetic objective".into();
    f.run("normal", "/goal synthetic objective").await;
    let after = f.state()["goal"].clone();
    assert_eq!(after["objective"], before["objective"]);
    assert_eq!(after["tokenBudget"], before["tokenBudget"]);
    assert_eq!(after["tokensUsed"], 14);
    assert_eq!(after["timeUsedSeconds"], 4);
    let requests = f.requests("thread/goal/set");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1],
        json!({"threadId":"native-thread-1", "status":"active"})
    );
}

#[tokio::test]
async fn codex_continuity_existing_active_turn_gets_hint_without_started_replay() {
    let mut f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    f.config.current_message = "current hint".into();
    let events = f.run("active", "stale transcript").await;
    assert_eq!(f.state()["hints"], json!(["current hint"]));
    assert_eq!(f.requests("turn/steer").len(), 1);
    assert_eq!(f.requests("thread/goal/set").len(), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::CodexSessionBound {
            goal_mode: true,
            ..
        }
    )));
}

#[tokio::test]
async fn codex_continuity_idle_goal_delivers_current_hint_to_new_turn_once() {
    let mut f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    f.config.current_message = "new checkpoint hint".into();
    f.run("hint", "stale transcript").await;
    assert_eq!(f.state()["hints"], json!(["new checkpoint hint"]));
    assert_eq!(f.requests("turn/steer").len(), 1);
}

#[tokio::test]
async fn codex_continuity_goal_read_or_ambiguous_active_turn_fails_without_orphan() {
    let f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    for (mode, expected) in [
        ("goal-error", "goal_unavailable"),
        ("active-no-id", "steer_unconfirmed"),
        ("resume-error", "no fresh-thread fallback"),
        ("cwd-mismatch", "identity"),
    ] {
        let error = f
            .start(
                f.backend_config(mode, CancellationToken::new()),
                "outer input",
            )
            .await
            .err()
            .expect("must fail");
        assert!(error.to_string().contains(expected), "{error}");
        f.assert_processes_stopped();
        // Failure releases attachment only after shutdown, permitting an explicit retry.
        drop(continuity::Lease::acquire(&f.config).await.unwrap());
    }
    assert_eq!(f.requests("thread/start").len(), 1);
}

#[tokio::test]
async fn codex_continuity_missing_or_exhausted_goal_is_never_reset() {
    let mut f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    f.config.current_message = "/goal synthetic objective".into();
    let mut state = f.state();
    state["goal"]["tokensUsed"] = json!(100);
    f.write_state(state.clone());
    let error = f
        .start(
            f.backend_config("normal", CancellationToken::new()),
            "/goal synthetic objective",
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("goal_budget"));
    state["goal"] = Value::Null;
    f.write_state(state);
    let error = f
        .start(
            f.backend_config("normal", CancellationToken::new()),
            "/goal synthetic objective",
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("goal_missing"));
    assert_eq!(f.requests("thread/goal/set").len(), 1);
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_cancel_pauses_native_goal_and_preserves_next_resume() {
    let mut f = Fixture::new();
    let cancel = CancellationToken::new();
    let (mut rx, handle) = f
        .start(
            f.backend_config("hold", cancel.clone()),
            "/goal synthetic objective",
        )
        .await
        .unwrap();
    wait_file(&f.dir.path().join("started")).await;
    cancel.cancel();
    let mut cancelled = false;
    while let Some(event) = rx.recv().await {
        cancelled |= matches!(event, ExecutionEvent::Cancelled);
    }
    handle.await.unwrap();
    assert!(cancelled);
    assert_eq!(f.state()["goal"]["status"], "paused");
    assert_eq!(f.state()["goal"]["tokensUsed"], 0);
    assert!(f.requests("thread/goal/clear").is_empty());
    f.config.current_message = "/goal synthetic objective".into();
    f.run("normal", "/goal synthetic objective").await;
    assert_eq!(f.requests("thread/start").len(), 1);
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_cancel_pause_failure_never_claims_native_paused() {
    let f = Fixture::new();
    let cancel = CancellationToken::new();
    let (rx, handle) = f
        .start(
            f.backend_config("pause-error", cancel.clone()),
            "/goal synthetic objective",
        )
        .await
        .unwrap();
    wait_file(&f.dir.path().join("started")).await;
    cancel.cancel();
    let (events, mut receiver) = tokio::sync::broadcast::channel(100);
    // A cancel already pending before the binding event must still drain the driver.
    let result = crate::api::runners::codex::consume_codex_events(
        rx,
        events,
        cancel,
        f.config.identity.mission_id,
        "plain hint",
        None,
    )
    .await;
    handle.await.unwrap();
    assert_eq!(
        result.terminal_reason,
        Some(crate::agents::TerminalReason::CodexContinuityRequired)
    );
    assert!(result.output.contains("pause_unconfirmed"));
    assert_eq!(f.state()["goal"]["status"], "active");
    while let Ok(event) = receiver.try_recv() {
        assert!(
            !matches!(event, crate::api::control::AgentEvent::GoalStatus { status, .. } if status == "paused")
        );
    }
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_transport_reconnect_keeps_id_and_never_replays_input() {
    let f = Fixture::new();
    f.run("crash-once", "execute once").await;
    assert_eq!(f.requests("thread/start").len(), 1);
    assert_eq!(f.requests("thread/resume").len(), 1);
    assert_eq!(f.state()["inputs"], json!(["execute once"]));
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_reconnect_reads_stopped_goal_receipt_without_rearming() {
    let f = Fixture::new();
    let events = f.run("goal-snapshot", "/goal synthetic objective").await;
    assert!(events.iter().any(
        |event| matches!(event, ExecutionEvent::GoalStatus { status, .. } if status == "blocked")
    ));
    assert!(events.iter().any(|event| matches!(event, ExecutionEvent::TextDelta { content } if content == "Retained checkpoint receipt")));
    assert_eq!(f.requests("thread/start").len(), 1);
    assert_eq!(f.requests("thread/resume").len(), 1);
    assert_eq!(f.requests("thread/goal/set").len(), 1);
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_reconnect_missing_turn_receipt_requires_reconciliation() {
    let f = Fixture::new();
    let events = f
        .run("goal-snapshot-no-turn", "/goal synthetic objective")
        .await;
    assert!(events.iter().any(|event| matches!(event, ExecutionEvent::Error { message } if message.contains("codex_continuity_turn_unresolved"))));
    assert_eq!(f.requests("thread/goal/set").len(), 1);
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_unresolved_tool_stays_fenced_across_outer_restart() {
    let f = Fixture::new();
    f.run("crash-tool", "execute once").await;
    let journal = ToolCallJournal::at(f.config.path.with_extension("tools.json"));
    assert_eq!(journal.pending().await.unwrap().len(), 1);
    let count = f.requests("initialize").len();
    let error = f
        .start(
            f.backend_config("normal", CancellationToken::new()),
            "try again",
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("unresolved_tools"));
    assert_eq!(f.requests("initialize").len(), count);
    assert_eq!(f.requests("turn/start").len(), 1);
}

#[tokio::test]
async fn codex_continuity_creation_ambiguity_identity_loss_and_double_attachment_refuse_fresh_thread(
) {
    let mut f = Fixture::new();
    let lease = continuity::Lease::acquire(&f.config).await.unwrap();
    assert!(continuity::Lease::acquire(&f.config).await.is_err());
    lease.prepare_creation().unwrap();
    drop(lease);
    assert!(continuity::Lease::acquire(&f.config)
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("creation_unknown"));
    std::fs::remove_file(&f.config.path).unwrap();
    f.config.projected_session = Some("codex-thread:native-thread-1".into());
    assert!(continuity::Lease::acquire(&f.config)
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("continuity_missing"));
    f.config.projected_session = None;
    f.run("normal", "first").await;
    f.config.identity.account = continuity::account_fingerprint("oauth", "another-account");
    assert!(continuity::Lease::acquire(&f.config)
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("identity"));
    assert_eq!(f.requests("thread/start").len(), 1);
}

#[tokio::test]
async fn codex_continuity_pre_start_handshake_failure_does_not_poison_new_mission() {
    let f = Fixture::new();
    assert!(f
        .start(
            f.backend_config("init-error", CancellationToken::new()),
            "first"
        )
        .await
        .is_err());
    assert!(!f.config.path.exists());
    f.assert_processes_stopped();
    f.run("normal", "first").await;
    assert_eq!(f.requests("thread/start").len(), 1);
}

#[tokio::test]
async fn codex_continuity_projection_and_plain_hint_keep_native_goal_stopped_classification() {
    let f = Fixture::new();
    f.run("normal", "/goal synthetic objective").await;
    let cancel = CancellationToken::new();
    let (rx, handle) = f
        .start(
            f.backend_config("active", cancel.clone()),
            "outer framed hint",
        )
        .await
        .unwrap();
    let (events, mut receiver) = tokio::sync::broadcast::channel(100);
    let result = crate::api::runners::codex::consume_codex_events(
        rx,
        events,
        cancel,
        f.config.identity.mission_id,
        "plain hint",
        None,
    )
    .await;
    handle.await.unwrap();
    assert_eq!(
        result.terminal_reason,
        Some(crate::agents::TerminalReason::NativeGoalStopped)
    );
    assert!(!result.success);
    let mut projected = false;
    while let Ok(event) = receiver.try_recv() {
        if let crate::api::control::AgentEvent::SessionIdUpdate { session_id, .. } = event {
            projected = session_id == "codex-thread:native-thread-1";
        }
    }
    assert!(projected);
}

/// Regression for the production create_mission -> first dispatch -> native
/// projection -> persisted mission reload path. A hand-built None session id
/// missed this: the real SQLite store creates a UUID before Codex ever runs.
#[tokio::test]
async fn codex_continuity_fresh_sqlite_mission_enrolls_and_reloads_same_thread() {
    use crate::api::mission_store::{MissionStore, SqliteMissionStore};
    use crate::api::runners::codex::native_continuity_enabled;

    let mut f = Fixture::new();
    let store = SqliteMissionStore::new(f.dir.path().join("store"), "native-enrollment")
        .await
        .unwrap();
    let mission = store
        .create_mission(
            Some("fresh native goal"),
            None,
            None,
            Some("synthetic-model"),
            None,
            Some("codex"),
            None,
        )
        .await
        .unwrap();
    assert!(
        mission.session_id.is_some(),
        "exercise the real bookkeeping UUID"
    );
    assert!(mission.history.is_empty());
    assert!(native_continuity_enabled(
        false,
        mission.session_id.as_deref(),
        mission
            .history
            .iter()
            .any(|entry| entry.role == "assistant"),
        false
    ));
    f.config.identity.mission_id = mission.id;
    f.config.path = continuity::binding_path(f.dir.path(), mission.id);
    f.config.projected_session = mission.session_id;
    let events = f
        .run("normal", "/goal real store creation regression")
        .await;
    let native_id = events
        .iter()
        .find_map(|event| match event {
            ExecutionEvent::CodexSessionBound {
                thread_id,
                goal_mode: true,
            } => Some(thread_id.clone()),
            _ => None,
        })
        .expect("fresh stored mission emits a real native binding");
    let projected = format!("{}{}", continuity::SESSION_PREFIX, native_id);
    store
        .update_mission_session_id(mission.id, &projected)
        .await
        .unwrap();
    let persisted = store.get_mission(mission.id).await.unwrap().unwrap();
    f.config.projected_session = persisted.session_id;
    let before = f.state()["goal"].clone();
    f.config.current_message = "current checkpoint hint".into();
    assert!(native_continuity_enabled(
        true,
        f.config.projected_session.as_deref(),
        true,
        true
    ));
    f.run(
        "normal",
        "stale outer transcript must not become a new goal",
    )
    .await;
    assert_eq!(f.requests("thread/start").len(), 1);
    assert_eq!(f.requests("thread/resume").len(), 1);
    assert_eq!(
        continuity::read(&f.config.path)
            .unwrap()
            .unwrap()
            .thread_id
            .as_deref(),
        Some(native_id.as_str())
    );
    assert_eq!(f.state()["goal"]["objective"], before["objective"]);
    assert_eq!(f.state()["goal"]["tokenBudget"], before["tokenBudget"]);
    assert!(
        f.state()["goal"]["tokensUsed"].as_u64().unwrap() >= before["tokensUsed"].as_u64().unwrap()
    );
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_enrollment_keeps_existing_legacy_and_unknown_sessions_fenced() {
    use crate::api::mission_store::{MissionStore, SqliteMissionStore};
    use crate::api::runners::codex::native_continuity_enabled;
    let f = Fixture::new();
    let store = SqliteMissionStore::new(f.dir.path().join("store"), "legacy-enrollment")
        .await
        .unwrap();
    let mission = store
        .create_mission(
            Some("existing legacy"),
            None,
            None,
            None,
            None,
            Some("codex"),
            None,
        )
        .await
        .unwrap();
    store
        .log_event(
            mission.id,
            &crate::api::control::AgentEvent::AssistantMessage {
                id: Uuid::new_v4(),
                content: "prior executed work".into(),
                success: true,
                cost_cents: 0,
                cost_source: crate::agents::CostSource::Unknown,
                usage: None,
                model: None,
                model_normalized: None,
                mission_id: Some(mission.id),
                shared_files: None,
                resumable: false,
                completion_evidence: None,
            },
        )
        .await
        .unwrap();
    let persisted = store.get_mission(mission.id).await.unwrap().unwrap();
    assert_eq!(persisted.history.len(), 1);
    assert!(!native_continuity_enabled(
        false,
        persisted.session_id.as_deref(),
        persisted
            .history
            .iter()
            .any(|entry| entry.role == "assistant"),
        false
    ));
    // A legacy run can crash after a tool but before its first assistant final.
    // Its rollout storage remains a fence even with no assistant history.
    std::fs::create_dir(f.dir.path().join(".codex/sessions")).unwrap();
    assert!(!native_continuity_enabled(
        false,
        mission.session_id.as_deref(),
        false,
        f.dir.path().join(".codex/sessions").is_dir()
    ));
    assert!(!native_continuity_enabled(
        false,
        Some("unknown-harness-session"),
        false,
        false
    ));
    assert!(!native_continuity_enabled(
        false,
        Some("01900000-0000-7000-8000-000000000001"),
        false,
        false
    ));
    // A native projection with a lost binding must enter the native path and
    // fail closed there, never fall back to legacy or create a fresh thread.
    let projected = format!("{}missing-native-thread", continuity::SESSION_PREFIX);
    assert!(native_continuity_enabled(
        false,
        Some(&projected),
        true,
        false
    ));
    let mut cfg = f.config.clone();
    cfg.projected_session = Some(projected);
    let failure = continuity::Lease::acquire(&cfg)
        .await
        .err()
        .expect("missing binding is refused");
    assert!(failure.to_string().contains("codex_continuity_missing"));
}

#[tokio::test]
async fn codex_continuity_resume_restored_goal_snapshot_does_not_drop_current_hint() {
    let mut f = Fixture::new();
    f.run("normal", "/goal preserve this exact objective").await;
    let before = f.state()["goal"].clone();
    f.config.current_message = "CONTINUE-CANARY".into();
    let events = f.run("restored-goal-hint", "old outer transcript").await;
    assert_eq!(f.state()["hints"], json!(["CONTINUE-CANARY"]));
    assert_eq!(f.requests("thread/start").len(), 1);
    assert_eq!(f.requests("thread/resume").len(), 1);
    assert_eq!(f.requests("turn/steer").len(), 1);
    assert_eq!(
        f.requests("thread/goal/set")[1],
        json!({"threadId":"native-thread-1", "status":"active"})
    );
    assert_eq!(f.state()["goal"]["objective"], before["objective"]);
    assert_eq!(f.state()["goal"]["tokenBudget"], before["tokenBudget"]);
    assert!(
        f.state()["goal"]["tokensUsed"].as_i64().unwrap() >= before["tokensUsed"].as_i64().unwrap()
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::Error { .. })),
        "{events:?}"
    );
    assert!(events.iter().any(
        |event| matches!(event, ExecutionEvent::GoalStatus { status, .. } if status == "blocked")
    ));
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_resume_keeps_real_stop_after_reactivation() {
    let mut f = Fixture::new();
    f.run("normal", "/goal preserve this exact objective").await;
    f.config.current_message = "current hint".into();
    let events = f.run("restored-goal-stopped", "old transcript").await;
    assert!(f.requests("turn/steer").is_empty());
    assert!(events.iter().any(
        |event| matches!(event, ExecutionEvent::GoalStatus { status, .. } if status == "blocked")
    ));
    assert!(events.iter().any(|event| matches!(event, ExecutionEvent::Error { message } if message.contains("steer_unconfirmed"))));
    assert_eq!(f.state()["goal"]["status"], "blocked");
    f.assert_processes_stopped();
}

#[tokio::test]
async fn codex_continuity_snapshot_drain_retains_errors_requests_and_nonmatching_goals() {
    use super::app_server::{InboundMessage, ThreadGoal};
    let goal = ThreadGoal {
        thread_id: "native-thread".into(),
        objective: "goal".into(),
        status: "blocked".into(),
        token_budget: Some(100),
        tokens_used: 7,
        time_used_seconds: 2,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    tx.send(InboundMessage::Notification {
        method: "thread/goal/updated".into(),
        params: json!({"threadId":goal.thread_id,"goal":goal}),
    })
    .await
    .unwrap();
    let retained = vec![
        InboundMessage::Notification {
            method: "error".into(),
            params: json!({"error":{"message":"real error"}}),
        },
        InboundMessage::ServerRequest {
            id: json!(9),
            method: "item/tool/call".into(),
            params: json!({"callId":"real-tool"}),
        },
        InboundMessage::Notification {
            method: "thread/goal/updated".into(),
            params: json!({"threadId":goal.thread_id,"turnId":"real-turn","goal":goal}),
        },
        InboundMessage::Notification {
            method: "thread/goal/updated".into(),
            params: json!({"threadId":"another-thread","goal":goal}),
        },
    ];
    for message in &retained {
        tx.send(message.clone()).await.unwrap();
    }
    let actual = super::drain_restored_goal_snapshot(&mut rx, &goal);
    assert_eq!(
        format!("{actual:?}"),
        format!("{:?}", std::collections::VecDeque::from(retained))
    );
}
