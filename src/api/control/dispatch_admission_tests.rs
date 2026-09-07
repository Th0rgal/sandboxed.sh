use super::*;
use crate::api::{mission_store::SqliteMissionStore, projects_store::ProjectsStore};
use serde_json::{json, Value};

type AdmissionHooks = std::sync::Mutex<HashMap<(Uuid, &'static str), oneshot::Sender<()>>>;
static HOOKS: std::sync::LazyLock<AdmissionHooks> = std::sync::LazyLock::new(Default::default);
pub(super) fn notify_wait(id: Uuid, kind: &'static str) {
    if let Some(tx) = HOOKS.lock().unwrap().remove(&(id, kind)) {
        let _ = tx.send(());
    }
}
fn wait_for(id: Uuid, kind: &'static str) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    HOOKS.lock().unwrap().insert((id, kind), tx);
    rx
}

struct Harness {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    control: ControlState,
    user: AuthUser,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let config = Config::new(path.to_path_buf());
        let root_agent: AgentRef = Arc::new(crate::agents::OpenCodeAgent::new(config.clone()));
        let mcp = Arc::new(McpRegistry::new(path).await);
        let workspaces = Arc::new(workspace::WorkspaceStore::new(path.to_path_buf()).await);
        let library = Arc::new(RwLock::new(None));
        let hub = ControlHub::new(
            config.clone(),
            root_agent.clone(),
            mcp.clone(),
            workspaces.clone(),
            library.clone(),
            None,
        );
        let user = AuthUser {
            id: "admission-test".into(),
            username: "admission-test".into(),
        };
        let store: Arc<dyn MissionStore> = Arc::new(
            SqliteMissionStore::new(path.join("missions"), &user.id)
                .await
                .unwrap(),
        );
        let control = spawn_control_session(
            hub.clone(),
            config.clone(),
            root_agent.clone(),
            mcp.clone(),
            workspaces.clone(),
            library.clone(),
            store,
            None,
            None,
            user.id.clone(),
        );
        hub.sessions
            .write()
            .await
            .insert(user.id.clone(), control.clone());
        let state = Arc::new(AppState {
            config,
            root_agent,
            mcp,
            workspaces,
            library,
            control: hub,
            tasks: Default::default(),
            opencode_connections: Arc::new(
                crate::opencode_config::OpenCodeStore::new(path.join("connections.json")).await,
            ),
            opencode_agents_cache: Default::default(),
            ai_providers: Arc::new(
                crate::ai_providers::AIProviderStore::new(path.join("providers.json")).await,
            ),
            pending_oauth: Default::default(),
            pending_github_oauth: Default::default(),
            pending_github_integration: Default::default(),
            github_connection: Arc::new(
                crate::github_connection::GithubConnectionStore::new(path.join("github.json"))
                    .await,
            ),
            secrets: None,
            console_pool: Arc::new(crate::api::console::SessionPool::new()),
            settings: Arc::new(crate::settings::SettingsStore::new(path).await),
            backend_registry: Arc::new(RwLock::new(
                crate::backend::registry::BackendRegistry::new("opencode"),
            )),
            backend_configs: Arc::new(
                crate::backend_config::BackendConfigStore::new(path.join("backends.json"), vec![])
                    .await,
            ),
            model_catalog: Default::default(),
            health_tracker: Arc::new(crate::provider_health::ProviderHealthTracker::new()),
            chain_store: Arc::new(
                crate::provider_health::ModelChainStore::new(path.join("chains.json")).await,
            ),
            http_client: reqwest::Client::new(),
            proxy_secret: "test-only".into(),
            proxy_api_keys: Arc::new(
                crate::api::proxy_keys::ProxyApiKeyStore::new(path.join("keys.json")).await,
            ),
            deferred_requests: Arc::new(
                crate::api::deferred_proxy::DeferredRequestStore::new(path.join("requests.json"))
                    .await,
            ),
            telegram_bridge: Arc::new(crate::api::telegram::TelegramBridge::new()),
            fido_hub: Arc::new(crate::api::fido::FidoSigningHub::new()),
            control_metrics: Arc::new(crate::api::control_metrics::ControlMetrics::new()),
            provider_usage_cache: crate::api::provider_usage_cache::ProviderUsageCache::new(),
            codex_usage: crate::api::codex_usage::CodexUsageStore::new(),
            fleet: Arc::new(crate::remote_node::FleetMonitor::new()),
            validation: Arc::new(
                crate::api::validation::ValidationStore::open(path.join("validation.db")).unwrap(),
            ),
            projects: Arc::new(ProjectsStore::open(path.join("projects.db")).unwrap()),
            attention_snapshot: Default::default(),
        });
        state.control.bind_admission_state(&state);
        state
            .projects
            .upsert_project("lido", None, None, None, None)
            .unwrap();
        let app = axum::Router::new()
            .route("/message", axum::routing::post(post_message))
            .route("/missions/:id/resume", axum::routing::post(resume_mission))
            .route(
                "/missions/:id/title",
                axum::routing::post(set_mission_title),
            )
            .route(
                "/missions/:id/project",
                axum::routing::patch(update_mission_project),
            )
            .layer(Extension(user.clone()))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _dir: dir,
            state,
            control,
            user,
            url,
            server,
        }
    }

    async fn writer(&self, status: MissionStatus, pr: Option<&str>) -> Mission {
        let m = self
            .control
            .mission_store
            .create_mission(
                Some("RESERVE-1"),
                None,
                None,
                None,
                None,
                Some("test-no-execution"),
                None,
            )
            .await
            .unwrap();
        self.control
            .mission_store
            .update_mission_project(
                m.id,
                crate::api::mission_store::MissionProjectPatch {
                    project: Some(Some("lido".into())),
                    track: Some(Some("trio-reserve1".into())),
                    github_pr: Some(pr.map(str::to_owned)),
                    tags: Some(vec!["pr-writer".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        self.control
            .mission_store
            .update_mission_status(m.id, status)
            .await
            .unwrap();
        self.state
            .projects
            .absorb_track("lido", "trio-reserve1", None, None)
            .unwrap();
        self.state
            .projects
            .acquire_track_lease(&crate::api::track_leases::lease_request(
                "lido",
                "trio-reserve1",
                &m.id.to_string(),
                "writer",
                None,
            ))
            .unwrap();
        self.control
            .mission_store
            .get_mission(m.id)
            .await
            .unwrap()
            .unwrap()
    }

    fn assertion(m: &Mission) -> Value {
        json!({"project": m.project.project, "track": m.project.track, "github_pr": m.project.github_pr})
    }
    async fn request(&self, resume: bool, id: Uuid, mut body: Value) -> reqwest::Response {
        let path = if resume {
            format!("/missions/{id}/resume")
        } else {
            body["mission_id"] = json!(id);
            "/message".into()
        };
        self.state
            .http_client
            .post(format!("{}{path}", self.url))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .unwrap()
    }
    async fn unchanged(&self, before: &Mission) {
        let after = self
            .control
            .mission_store
            .get_mission(before.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.title, before.title);
        assert_eq!(after.project, before.project);
        assert_eq!(after.status, before.status);
    }
}

#[tokio::test]
async fn http_actor_nonresumable_retag_keeps_title_identity_and_lease() {
    let h = Harness::new().await;
    for status in [
        MissionStatus::Active,
        MissionStatus::Completed,
        MissionStatus::Acknowledged,
        MissionStatus::Pending,
        MissionStatus::AwaitingUser,
    ] {
        let m = h.writer(status, Some("repo#244")).await;
        let response = h
            .request(
                true,
                m.id,
                json!({"github_pr":"", "track":"", "title":"Different work"}),
            )
            .await;
        assert!(!response.status().is_success());
        h.unchanged(&m).await;
        assert!(h
            .state
            .projects
            .live_leases(None)
            .unwrap()
            .iter()
            .any(|l| l.attempt_id == m.id.to_string()));
        h.state
            .projects
            .release_leases_for_attempt(&m.id.to_string())
            .unwrap();
    }
}

#[tokio::test]
async fn http_actor_replacement_owner_blocks_send_and_resume_with_or_without_pr() {
    let h = Harness::new().await;
    for pr in [None, Some("repo#244")] {
        let m = h.writer(MissionStatus::Failed, pr).await;
        h.state
            .projects
            .release_leases_for_attempt(&m.id.to_string())
            .unwrap();
        let replacement = h
            .state
            .projects
            .acquire_track_lease(&crate::api::track_leases::lease_request(
                "lido",
                "trio-reserve1",
                "replacement",
                "writer",
                None,
            ))
            .unwrap();
        for resume in [false, true] {
            let response = h.request(resume, m.id, json!({"content":"Continue RESERVE-1; exclude PRs 230/231 and consult collaborator PR 244.", "continue_identity": Harness::assertion(&m)})).await;
            assert!(!response.status().is_success());
            assert!(response.text().await.unwrap().contains("track_owned"));
            h.unchanged(&m).await;
        }
        h.state.projects.expire_lease(&replacement.id).unwrap();
    }
}

#[tokio::test]
async fn http_closed_command_channel_never_mutates_assignment() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    h.state
        .control
        .sessions
        .write()
        .await
        .get_mut(&h.user.id)
        .unwrap()
        .cmd_tx = tx;
    for resume in [false, true] {
        assert!(!h.request(resume, m.id, json!({"content":"Different work", "github_pr":"", "track":"", "title":"Different work"})).await.status().is_success());
        h.unchanged(&m).await;
    }
}

#[tokio::test]
async fn http_actor_stale_assertion_is_checked_after_concurrent_project_edit() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    for resume in [false, true] {
        let held = DISPATCH_ADMISSION.lock().await;
        // Both requests traverse actual HTTP handlers. Explicit arrival
        // notifications establish FIFO actor-command order, without sleeps.
        let edit_waiting = wait_for(m.id, "project");
        let actor_waiting = wait_for(m.id, "enqueued");
        let client = h.state.http_client.clone();
        let url = format!("{}/missions/{}/project", h.url, m.id);
        let edit = tokio::spawn(async move {
            client
                .patch(url)
                .json(&json!({"track":"replacement-track"}))
                .send()
                .await
                .unwrap()
        });
        edit_waiting.await.unwrap();
        let request = h.request(
            resume,
            m.id,
            json!({"content":"Continue RESERVE-1", "continue_identity": Harness::assertion(&m)}),
        );
        let (response, ()) = tokio::join!(request, async {
            actor_waiting.await.unwrap();
            drop(held);
            assert!(edit.await.unwrap().status().is_success());
        });
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("writer_identity_stale"));
        // Reset through the real HTTP project handler for the next endpoint.
        let response = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, m.id))
            .json(&json!({"track":"trio-reserve1"}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
    }
}

#[tokio::test]
async fn http_actor_explicit_rejection_rolls_back_retag_before_reply() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#244")).await;
    let (tx, mut rx) = mpsc::channel(2);
    h.state
        .control
        .sessions
        .write()
        .await
        .get_mut(&h.user.id)
        .unwrap()
        .cmd_tx = tx;
    let actor = tokio::spawn(async move {
        while let Some(ControlCommand::AdmitDispatch { admission, command }) = rx.recv().await {
            let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
                .await
                .unwrap();
            match command {
                ControlCommand::UserMessage { respond, .. } => {
                    let _ = respond.send(UserMessageAck::Rejected(
                        "injected delivery rejection".into(),
                    ));
                }
                ControlCommand::ResumeMission { respond, .. } => {
                    let _ = respond.send(Err("injected resume rejection".into()));
                }
                _ => panic!(),
            }
        }
    });
    for resume in [false, true] {
        let response = h.request(resume, m.id, json!({"content":"Different work", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
        assert!(!response.status().is_success());
        h.unchanged(&m).await;
        let leases = h.state.projects.live_leases(None).unwrap();
        assert_eq!(
            leases
                .iter()
                .filter(|l| l.attempt_id == m.id.to_string())
                .count(),
            1
        );
        assert_eq!(leases[0].track, "trio-reserve1");
    }
    actor.abort();
}

#[tokio::test]
async fn http_store_failure_keeps_old_identity_title_and_ownership_for_all_edits() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#244")).await;
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute_batch("CREATE TRIGGER refuse_assignment BEFORE UPDATE OF track, github_pr, title ON missions BEGIN SELECT RAISE(FAIL, 'injected assignment write failure'); END;").unwrap();
    for resume in [false, true] {
        let response = h.request(resume, m.id, json!({"content":"Different work", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("injected assignment write failure"));
        h.unchanged(&m).await;
        let leases = h.state.projects.live_leases(None).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].track, "trio-reserve1");
    }
    let response = h
        .state
        .http_client
        .patch(format!("{}/missions/{}/project", h.url, m.id))
        .json(&json!({"track":"new-track"}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    h.unchanged(&m).await;
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
}

#[tokio::test]
async fn http_pr_only_edit_and_writer_promotion_check_track_replacement() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    h.state
        .projects
        .release_leases_for_attempt(&m.id.to_string())
        .unwrap();
    h.state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "trio-reserve1",
            &m.id.to_string(),
            "reader",
            None,
        ))
        .unwrap();
    h.state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "trio-reserve1",
            "replacement",
            "writer",
            None,
        ))
        .unwrap();
    for body in [json!({"github_pr":"repo#245"}), json!({"writer":true})] {
        let response = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, m.id))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        h.unchanged(&m).await;
    }
}

#[tokio::test]
async fn http_actor_same_work_with_exclusions_and_missing_pr_can_queue() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Pending, None).await;
    h.control
        .mission_store
        .set_mission_scheduling(
            m.id,
            &crate::api::mission_store::MissionScheduling {
                not_before: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.control
        .mission_store
        .update_mission_goal(m.id, true, Some("Persist RESERVE-1 objective"))
        .await
        .unwrap();
    let response = h.request(false, m.id, json!({"content":"Continue RESERVE-1 PR 244. Exclude PRs #230/#231 and coordinate with the ALLOC-1 collaborator.", "continue_identity":Harness::assertion(&m)})).await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.goal_mode);
    assert_eq!(
        after.goal_objective.as_deref(),
        Some("Persist RESERVE-1 objective")
    );
    assert_eq!(after.project.github_pr, None);
    assert_eq!(after.project.track, m.project.track);
}

#[tokio::test]
async fn rejected_actor_dispatch_with_failed_rollback_is_durably_recoverable() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#244")).await;
    let (tx, mut rx) = mpsc::channel(1);
    h.state
        .control
        .sessions
        .write()
        .await
        .get_mut(&h.user.id)
        .unwrap()
        .cmd_tx = tx;
    let path = h._dir.path().join("missions/missions-admission-test.db");
    let actor = tokio::spawn(async move {
        let ControlCommand::AdmitDispatch { admission, command } = rx.recv().await.unwrap() else {
            panic!()
        };
        let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
            .await
            .unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("CREATE TRIGGER refuse_rollback BEFORE UPDATE OF track ON missions BEGIN SELECT RAISE(FAIL, 'injected rollback failure'); END;").unwrap();
        let ControlCommand::UserMessage { respond, .. } = command else {
            panic!()
        };
        let _ = respond.send(UserMessageAck::Rejected("injected rejection".into()));
    });
    let response = h.request(false, m.id, json!({"content":"Different work", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
    assert!(!response.status().is_success());
    assert!(response.text().await.unwrap().contains("recovery required"));
    actor.await.unwrap();
    let journal = h
        .state
        .projects
        .dispatch_admission(&m.id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(journal["phase"], "rejected");
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 2);
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 2);
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute_batch("DROP TRIGGER refuse_rollback").unwrap();
    let _guard = DISPATCH_ADMISSION.lock().await;
    dispatch_admission::recover_dispatch(&h.state, &h.control.mission_store, m.id)
        .await
        .unwrap();
    h.unchanged(&m).await;
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    assert!(h
        .state
        .projects
        .dispatch_admission(&m.id.to_string())
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn unknown_admission_outcome_survives_reopen_and_fences_both_tracks() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, None).await;
    h.state
        .projects
        .save_dispatch_admission(&m.id.to_string(), &json!({"phase":"pending"}))
        .unwrap();
    h.state
        .projects
        .absorb_track("lido", "new-track", None, None)
        .unwrap();
    h.state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "new-track",
            &m.id.to_string(),
            "writer",
            None,
        ))
        .unwrap();
    let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
    db.execute(
        "UPDATE track_leases SET lease_until = '2000-01-01T00:00:00Z'",
        [],
    )
    .unwrap();
    let reopened = ProjectsStore::open(h._dir.path().join("projects.db")).unwrap();
    assert_eq!(
        reopened
            .dispatch_admission(&m.id.to_string())
            .unwrap()
            .unwrap()["phase"],
        "pending"
    );
    for track in ["trio-reserve1", "new-track"] {
        assert!(matches!(
            reopened.acquire_track_lease(&crate::api::track_leases::lease_request(
                "lido",
                track,
                "replacement",
                "writer",
                None,
            )),
            Err(crate::api::projects_store::LeaseError::Owned { .. })
        ));
    }
    for resume in [false, true] {
        let response = h
            .request(
                resume,
                m.id,
                json!({"content":"Continue", "continue_identity":Harness::assertion(&m)}),
            )
            .await;
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("dispatch_recovery_required"));
        h.unchanged(&m).await;
    }
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 2);
}

#[tokio::test]
async fn http_actor_resume_reacquires_track_and_preserves_goal_without_prompt() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, None).await;
    h.state
        .projects
        .release_leases_for_attempt(&m.id.to_string())
        .unwrap();
    h.control
        .mission_store
        .update_mission_goal(m.id, true, Some("RESERVE-1 persistent objective"))
        .await
        .unwrap();
    let response = h
        .request(
            true,
            m.id,
            json!({"skip_message":true, "continue_identity":Harness::assertion(&m)}),
        )
        .await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, MissionStatus::Active);
    assert!(after.goal_mode);
    assert_eq!(
        after.goal_objective.as_deref(),
        Some("RESERVE-1 persistent objective")
    );
    assert_eq!(after.project.github_pr, None);
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
}

#[tokio::test]
async fn http_actor_retask_requires_edit_or_a_trusted_semantic_assertion() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Pending, Some("repo#244")).await;
    h.control
        .mission_store
        .set_mission_scheduling(
            m.id,
            &crate::api::mission_store::MissionScheduling {
                not_before: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let content = "Abandon RESERVE-1. Implement different work on PR #999 instead.";
    let response = h.request(false, m.id, json!({"content":content})).await;
    assert!(!response.status().is_success());
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("writer_identity_stale"));
    // Honest trust-model regression: the server cannot prove prompt semantics.
    let response = h
        .request(
            false,
            m.id,
            json!({"content":content, "continue_identity":Harness::assertion(&m)}),
        )
        .await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    assert_eq!(
        h.control
            .mission_store
            .get_mission(m.id)
            .await
            .unwrap()
            .unwrap()
            .project,
        m.project
    );
    let response = h.request(false, m.id, json!({"content":content, "github_pr":"repo#999", "track":"different-work", "title":"Different work"})).await;
    assert!(!response.status().is_success());
    assert!(response.text().await.unwrap().contains("assignment_busy"));
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.project, m.project);
    let deferred = h
        .control
        .mission_store
        .get_deferred_goal(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deferred.matches(content).count(), 1);
}

#[tokio::test]
async fn http_track_store_acquisition_failure_does_not_commit_retag() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#244")).await;
    let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_lease BEFORE INSERT ON track_leases BEGIN SELECT RAISE(FAIL, 'injected lease store failure'); END;").unwrap();
    for resume in [false, true] {
        let response = h.request(resume, m.id, json!({"content":"Different work", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("injected lease store failure"));
        h.unchanged(&m).await;
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    }
}

#[tokio::test]
async fn accepted_dispatch_cleanup_failure_is_not_reported_as_rejection() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#244")).await;
    let (tx, mut rx) = mpsc::channel(1);
    h.state
        .control
        .sessions
        .write()
        .await
        .get_mut(&h.user.id)
        .unwrap()
        .cmd_tx = tx;
    let path = h._dir.path().join("projects.db");
    let actor = tokio::spawn(async move {
        let ControlCommand::AdmitDispatch { admission, command } = rx.recv().await.unwrap() else {
            panic!()
        };
        let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
            .await
            .unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("CREATE TRIGGER refuse_cleanup BEFORE UPDATE OF state ON track_leases BEGIN SELECT RAISE(FAIL, 'injected cleanup failure'); END;").unwrap();
        let ControlCommand::UserMessage { respond, .. } = command else {
            panic!()
        };
        respond.send(UserMessageAck::Queued).unwrap();
    });
    let response = h.request(false, m.id, json!({"content":"Different work", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    actor.await.unwrap();
    assert_eq!(
        h.state
            .projects
            .dispatch_admission(&m.id.to_string())
            .unwrap()
            .unwrap()["phase"],
        "accepted"
    );
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 2);
    let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
    db.execute_batch("DROP TRIGGER refuse_cleanup").unwrap();
    let _guard = DISPATCH_ADMISSION.lock().await;
    dispatch_admission::recover_dispatch(&h.state, &h.control.mission_store, m.id)
        .await
        .unwrap();
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.title.as_deref(), Some("Different work"));
    assert_eq!(after.project.track.as_deref(), Some("new-track"));
    let leases = h.state.projects.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].track, "new-track");
}

#[tokio::test]
async fn http_actor_custom_resume_delivers_exactly_the_custom_prompt() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, None).await;
    let mut events = h.control.events_tx.subscribe();
    let content = "Continue RESERVE-1; exclude PRs #230 and #231; consult PR 244.";
    let response = h
        .request(
            true,
            m.id,
            json!({"content":content, "continue_identity":Harness::assertion(&m)}),
        )
        .await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let AgentEvent::UserMessage {
                content,
                mission_id: Some(id),
                ..
            } = events.recv().await.unwrap()
            {
                if id == m.id {
                    break content;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(delivered, content);
}

#[tokio::test]
async fn http_actor_queue_store_rejection_rolls_back_send_and_resume() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute_batch("CREATE TRIGGER refuse_queue BEFORE INSERT ON control_queue BEGIN SELECT RAISE(FAIL, 'injected queue failure'); END;").unwrap();
    for resume in [false, true] {
        let response = h.request(resume, m.id, json!({"content":"Different work on new assignment", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("injected queue failure"));
        h.unchanged(&m).await;
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    }
    db.execute_batch("DROP TRIGGER refuse_queue").unwrap();
    // Ask the real actor to complete another command after its rejection.
    let (tx, rx) = oneshot::channel();
    h.control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .unwrap();
    rx.await.unwrap();
    let durable_queue = h
        .control
        .mission_store
        .load_control_queue(&h.user.id)
        .await
        .unwrap();
    assert!(!durable_queue.contains("Different work on new assignment"));
}

#[tokio::test]
async fn http_actor_resume_dequeue_failure_keeps_accepted_prompt_and_assignment() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute_batch("CREATE TRIGGER refuse_dequeue BEFORE UPDATE ON control_queue WHEN NEW.payload NOT LIKE '%durable custom resume%' BEGIN SELECT RAISE(FAIL, 'injected dequeue failure'); END;").unwrap();
    let response = h.request(true, m.id, json!({"content":"durable custom resume", "github_pr":"", "track":"new-track", "title":"Different work"})).await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.title.as_deref(), Some("Different work"));
    assert_eq!(after.project.track.as_deref(), Some("new-track"));
    let durable_queue = h
        .control
        .mission_store
        .load_control_queue(&h.user.id)
        .await
        .unwrap();
    assert!(durable_queue.contains("durable custom resume"));
    let leases = h.state.projects.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].track, "new-track");
    // No runner started: the accepted assignment is still bound to the
    // durable queued prompt, even if presentation status is changed.
    h.control
        .mission_store
        .update_mission_status(m.id, MissionStatus::Paused)
        .await
        .unwrap();
    for track in ["second-assignment", "third-assignment"] {
        let response = h
            .request(
                false,
                m.id,
                json!({"content":"different queued work", "track":track, "github_pr":"repo#249"}),
            )
            .await;
        assert!(!response.status().is_success());
        assert!(response.text().await.unwrap().contains("assignment_busy"));
        let response = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, m.id))
            .json(&json!({"track":track}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(response.text().await.unwrap().contains("assignment_busy"));
        assert_eq!(
            h.control
                .mission_store
                .get_mission(m.id)
                .await
                .unwrap()
                .unwrap()
                .project,
            after.project
        );
        assert_eq!(
            h.control
                .mission_store
                .load_control_queue(&h.user.id)
                .await
                .unwrap(),
            durable_queue
        );
    }
}

#[tokio::test]
async fn http_actor_title_edit_does_not_deadlock_and_honors_recovery_fence() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let response = h
        .state
        .http_client
        .post(format!("{}/missions/{}/title", h.url, m.id))
        .json(&json!({"title":"Renamed"}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let before = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.title.as_deref(), Some("Renamed"));
    h.state
        .projects
        .save_dispatch_admission(&m.id.to_string(), &json!({"phase":"pending"}))
        .unwrap();
    let response = h
        .state
        .http_client
        .post(format!("{}/missions/{}/title", h.url, m.id))
        .json(&json!({"title":"Unsafe rename"}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("dispatch_recovery_required"));
    h.unchanged(&before).await;
}

#[tokio::test]
async fn unresolved_admission_fences_old_and_new_pr_even_without_track_or_live_status() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
    h.state
        .projects
        .save_dispatch_admission(
            &m.id.to_string(),
            &json!({
                "phase":"pending",
                "before":{"project":{"github_pr":"repo#244"}},
                "after":{"project":{"github_pr":"repo#245"}}
            }),
        )
        .unwrap();
    // The journal fence does not depend on a live track lease or mission status.
    h.state
        .projects
        .release_leases_for_attempt(&m.id.to_string())
        .unwrap();
    for pr in ["repo#244", "repo#245"] {
        assert_eq!(
            find_existing_pr_writer_global(&h.state.control, pr, None)
                .await
                .unwrap()
                .unwrap()
                .id,
            m.id
        );
        assert!(
            find_existing_pr_writer_global(&h.state.control, pr, Some(m.id))
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn http_actor_response_loss_quarantines_unknown_outcome_without_releasing_either_assignment()
{
    for resume in [false, true] {
        let h = Harness::new().await;
        let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
        let (tx, mut rx) = mpsc::channel(1);
        h.state
            .control
            .sessions
            .write()
            .await
            .get_mut(&h.user.id)
            .unwrap()
            .cmd_tx = tx;
        let actor = tokio::spawn(async move {
            let ControlCommand::AdmitDispatch { admission, command } = rx.recv().await.unwrap()
            else {
                panic!()
            };
            let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
                .await
                .unwrap();
            // Simulate a lost response after possible dispatch, not a known refusal.
            drop(command);
        });
        let response = h.request(resume, m.id, json!({"content":"Different work", "github_pr":"repo#245", "track":"new-track", "title":"Different work"})).await;
        assert!(!response.status().is_success());
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("acceptance is unknown"));
        actor.await.unwrap();
        assert_eq!(
            h.state
                .projects
                .dispatch_admission(&m.id.to_string())
                .unwrap()
                .unwrap()["phase"],
            "pending"
        );
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 2);
        let after = h
            .control
            .mission_store
            .get_mission(m.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.project.track.as_deref(), Some("new-track"));
        for pr in ["repo#244", "repo#245"] {
            assert_eq!(
                find_existing_pr_writer_global(&h.state.control, pr, None)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                m.id
            );
        }
    }
}

#[tokio::test]
async fn http_real_reader_promotion_without_pr_checks_owner_and_persists_writer_capability() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    h.control
        .mission_store
        .update_mission_project(
            m.id,
            crate::api::mission_store::MissionProjectPatch {
                tags: Some(vec!["pr-readonly".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.state
        .projects
        .release_leases_for_attempt(&m.id.to_string())
        .unwrap();
    h.state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "trio-reserve1",
            &m.id.to_string(),
            "reader",
            None,
        ))
        .unwrap();
    let replacement = h
        .state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "trio-reserve1",
            "replacement",
            "writer",
            None,
        ))
        .unwrap();
    let before = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    let response = h
        .state
        .http_client
        .patch(format!("{}/missions/{}/project", h.url, m.id))
        .json(&json!({"writer":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    h.unchanged(&before).await;
    h.state.projects.expire_lease(&replacement.id).unwrap();
    let response = h
        .state
        .http_client
        .patch(format!("{}/missions/{}/project", h.url, m.id))
        .json(&json!({"writer":true}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let promoted = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    assert!(promoted.project.tags.iter().any(|t| t == "pr-writer"));
    assert!(!promoted.project.tags.iter().any(|t| t == "pr-readonly"));
    assert_eq!(promoted.project.github_pr, None);
    let response = h
        .request(
            true,
            m.id,
            json!({"skip_message":true, "continue_identity":Harness::assertion(&promoted)}),
        )
        .await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let leases = h.state.projects.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].mode, "writer");
}

// A provider-launch-only seam: tests keep the real HTTP actor, durable queue,
// JSON-RPC child/driver, event consumer and mission finalization. No API keys.
type NativeFixture = (std::path::PathBuf, String);
static NATIVE_FIXTURES: std::sync::LazyLock<std::sync::Mutex<HashMap<Uuid, NativeFixture>>> =
    std::sync::LazyLock::new(Default::default);

pub(crate) async fn native_goal_fixture(
    id: Uuid,
    message: &str,
    events: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
) -> Option<crate::agents::AgentResult> {
    use crate::backend::{Backend, SessionConfig};
    let (dir, order) = NATIVE_FIXTURES.lock().unwrap().get(&id).cloned()?;
    let backend = crate::backend::codex::CodexBackend::with_config(
        crate::backend::codex::client::CodexConfig {
            cli_path: dir.join("app-server").to_string_lossy().into_owned(),
            cancel_token: Some(cancel.clone()),
            extra_env: HashMap::from([
                (
                    "GOAL_FIXTURE_DIR".into(),
                    dir.to_string_lossy().into_owned(),
                ),
                ("GOAL_FIXTURE_ORDER".into(), order),
            ]),
            ..Default::default()
        },
    );
    let session = backend
        .create_session(SessionConfig {
            directory: dir.to_string_lossy().into_owned(),
            title: None,
            model: None,
            agent: None,
        })
        .await
        .unwrap();
    let (rx, handle) = backend
        .send_message_streaming(&session, message)
        .await
        .unwrap();
    let result =
        crate::api::runners::codex::consume_codex_events(rx, events, cancel, id, message, None)
            .await;
    handle.await.unwrap();
    if result.terminal_reason == Some(TerminalReason::NativeGoalStopped) {
        assert!(!result.success);
        let evidence = completion_evidence_for_agent_result(&result);
        assert!(evidence.native_terminal_seen);
        assert_eq!(
            evidence.completion_signal,
            crate::agents::CompletionSignal::NativeTerminal
        );
        assert_eq!(
            result.data.as_ref().unwrap()["turn_outcome"]["outcome"],
            "interrupted"
        );
    }
    Some(result)
}

async fn install_native_fixture(h: &Harness, id: Uuid, order: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = h._dir.path().join(id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("app-server");
    std::fs::write(
        &cli,
        include_str!("../../../tests/fixtures/native_goal_app_server.py"),
    )
    .unwrap();
    std::fs::set_permissions(cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    NATIVE_FIXTURES
        .lock()
        .unwrap()
        .insert(id, (dir.clone(), order.into()));
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute(
        "UPDATE missions SET backend='codex' WHERE id=?1",
        [id.to_string()],
    )
    .unwrap();
    dir
}

async fn wait_native_file(path: &std::path::Path) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture reached expected wire boundary");
}

async fn wait_native_status(h: &Harness, id: Uuid, expected: MissionStatus) -> Mission {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let m = h
                .control
                .mission_store
                .get_mission(id)
                .await
                .unwrap()
                .unwrap();
            if m.status == expected
                && h.control
                    .mission_store
                    .get_active_mission_run(id)
                    .await
                    .unwrap()
                    .is_none()
            {
                return m;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("driver released its durable run and parked mission")
}

#[tokio::test]
async fn native_goal_http_actor_blocked_parks_and_default_resume_recovers_same_goal() {
    for order in ["before", "after", "before_started"] {
        let h = Harness::new().await;
        let m = h.writer(MissionStatus::Failed, None).await;
        let dir = install_native_fixture(&h, m.id, order).await;
        let mut events = h.control.events_tx.subscribe();
        let objective = "RESERVE-1 objective; preserve evidence and exclusions";
        let response = h.request(true, m.id, json!({"content":format!("/goal {objective}"), "continue_identity":Harness::assertion(&m)})).await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        wait_native_file(&dir.join("started")).await;
        // No tool is in flight. Release is an explicit native blocked update,
        // never an observation timeout or a controller cancel.
        std::fs::write(dir.join("release"), "").unwrap();
        let parked = wait_native_status(&h, m.id, MissionStatus::Blocked).await;
        assert_eq!(
            parked.terminal_reason.as_deref(),
            Some("native_goal_stopped")
        );
        assert!(parked
            .terminal_evidence
            .as_deref()
            .unwrap()
            .contains("status=blocked"));
        assert!(parked.goal_mode);
        assert_eq!(parked.goal_objective.as_deref(), Some(objective));
        assert_eq!(parked.project, m.project);
        assert!(!dir.join("unexpected-clear").exists());
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
        let finals: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|e| match e {
                AgentEvent::AssistantMessage {
                    content,
                    success,
                    resumable,
                    ..
                } => Some((content, success, resumable)),
                _ => None,
            })
            .collect();
        assert_eq!(
            finals,
            vec![(
                "Blocked: external node unavailable; evidence retained".into(),
                false,
                true
            )]
        );
        std::fs::write(dir.join("external-fixed"), "").unwrap();
        let response = h
            .request(
                true,
                m.id,
                json!({"continue_identity":Harness::assertion(&m)}),
            )
            .await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let recovered = wait_native_status(&h, m.id, MissionStatus::AwaitingUser).await;
        assert!(recovered.goal_mode);
        assert_eq!(recovered.goal_objective.as_deref(), Some(objective));
        assert_eq!(recovered.project, m.project);
        let requests: Vec<Value> = std::fs::read_to_string(dir.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r["method"] == "thread/goal/set"
            && r["params"]["objective"] == objective
            && r["params"]["status"] == "active"));
        NATIVE_FIXTURES.lock().unwrap().remove(&m.id);
    }
}

#[tokio::test]
async fn native_goal_http_actor_queued_steering_drains_once_with_blocked_evidence() {
    for parallel in [false, true] {
        let h = Harness::new().await;
        let blocker = if parallel {
            let b = h
                .control
                .mission_store
                .create_mission(Some("other"), None, None, None, None, Some("codex"), None)
                .await
                .unwrap();
            let dir = install_native_fixture(&h, b.id, "before").await;
            let response = h
                .request(false, b.id, json!({"content":"/goal unrelated live work"}))
                .await;
            assert!(response.status().is_success());
            wait_native_file(&dir.join("started")).await;
            Some((b.id, dir))
        } else {
            None
        };
        let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
        let dir = install_native_fixture(&h, m.id, "after").await;
        let response = h
            .request(
                true,
                m.id,
                json!({"content":"/goal RESERVE-1", "continue_identity":Harness::assertion(&m)}),
            )
            .await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        wait_native_file(&dir.join("started")).await;
        for content in ["first external steering", "second external steering"] {
            let response = h
                .request(
                    false,
                    m.id,
                    json!({"content":content, "continue_identity":Harness::assertion(&m)}),
                )
                .await;
            assert!(
                response.status().is_success(),
                "{}",
                response.text().await.unwrap()
            );
        }
        std::fs::write(dir.join("release"), "").unwrap();
        let parked = wait_native_status(&h, m.id, MissionStatus::AwaitingUser).await;
        let requests: Vec<Value> = std::fs::read_to_string(dir.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1]["params"]["input"][0]["text"],
            "first external steering"
        );
        assert_eq!(
            requests[2]["params"]["input"][0]["text"],
            "second external steering"
        );
        assert_eq!(parked.project, m.project);
        assert!(parked.goal_mode);
        assert_eq!(parked.goal_objective.as_deref(), Some("RESERVE-1"));
        assert_eq!(
            parked
                .history
                .iter()
                .filter(|e| e.role == "assistant" && e.content.contains("Blocked: external node"))
                .count(),
            1
        );
        for content in ["first external steering", "second external steering"] {
            assert_eq!(
                parked
                    .history
                    .iter()
                    .filter(|e| e.role == "user" && e.content == content)
                    .count(),
                1
            );
        }
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
        assert!(!dir.join("unexpected-clear").exists());
        NATIVE_FIXTURES.lock().unwrap().remove(&m.id);
        if let Some((id, dir)) = blocker {
            std::fs::write(dir.join("release"), "").unwrap();
            wait_native_status(&h, id, MissionStatus::Blocked).await;
            NATIVE_FIXTURES.lock().unwrap().remove(&id);
        }
    }
}

#[tokio::test]
async fn native_goal_parked_writer_survives_sweep_and_cross_store_pr_arbitration() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Active, Some("repo#244")).await;
    h.control
        .mission_store
        .update_mission_status_with_reason(
            m.id,
            MissionStatus::Blocked,
            Some("native_goal_stopped"),
        )
        .await
        .unwrap();
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    assert_eq!(
        find_existing_pr_writer(&h.control.mission_store, "repo#244", None)
            .await
            .unwrap()
            .unwrap()
            .id,
        m.id
    );
    assert_eq!(
        find_existing_pr_writer_in_sqlite(
            &h._dir.path().join("missions/missions-admission-test.db"),
            "repo#244",
            None
        )
        .unwrap()
        .unwrap()
        .id,
        m.id
    );
    let replacement = Uuid::new_v4();
    assert!(h
        .state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "trio-reserve1",
            &replacement.to_string(),
            "writer",
            None
        ))
        .is_err());
}

#[tokio::test]
async fn ownership_lifetime_actual_runner_refuses_retags_until_old_queue_drains() {
    for parallel in [false, true] {
        let h = Harness::new().await;
        let blocker = if parallel {
            let b = h
                .control
                .mission_store
                .create_mission(
                    Some("capacity blocker"),
                    None,
                    None,
                    None,
                    None,
                    Some("codex"),
                    None,
                )
                .await
                .unwrap();
            let dir = install_native_fixture(&h, b.id, "before").await;
            assert!(h
                .request(false, b.id, json!({"content":"/goal blocker"}))
                .await
                .status()
                .is_success());
            wait_native_file(&dir.join("started")).await;
            Some((b.id, dir))
        } else {
            None
        };
        let a = h.writer(MissionStatus::Failed, Some("repo#244")).await;
        let dir = install_native_fixture(&h, a.id, "after").await;
        assert!(h.request(true, a.id, json!({"content":"/goal old assignment", "continue_identity":Harness::assertion(&a)})).await.status().is_success());
        wait_native_file(&dir.join("started")).await;

        let b = h
            .control
            .mission_store
            .create_mission(
                Some("competing writer"),
                None,
                None,
                None,
                None,
                Some("codex"),
                None,
            )
            .await
            .unwrap();
        h.control
            .mission_store
            .update_mission_status(b.id, MissionStatus::Failed)
            .await
            .unwrap();
        h.control
            .mission_store
            .update_mission_project(
                b.id,
                crate::api::mission_store::MissionProjectPatch {
                    project: Some(Some("lido".into())),
                    tags: Some(vec!["pr-writer".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let bdir = install_native_fixture(&h, b.id, "after").await;

        for (index, followup) in ["first old continuation", "second old continuation"]
            .iter()
            .enumerate()
        {
            // Actual actor accepts and queues same-assignment work behind the
            // barrier-held provider. Neither a send retag nor a project patch
            // may change the binding of either queued message.
            let response = h
                .request(
                    false,
                    a.id,
                    json!({"content":followup, "continue_identity":Harness::assertion(&a)}),
                )
                .await;
            assert!(
                response.status().is_success(),
                "{}",
                response.text().await.unwrap()
            );
            let response = h.request(false, a.id, json!({"content":"new assignment", "track":format!("new-{index}"), "github_pr":format!("repo#{}", 245+index)})).await;
            assert!(!response.status().is_success());
            assert!(response.text().await.unwrap().contains("assignment_busy"));
            let response = h.state.http_client.patch(format!("{}/missions/{}/project", h.url, a.id))
                .json(&json!({"track":format!("new-{index}"), "github_pr":format!("repo#{}", 245+index)})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert!(response.text().await.unwrap().contains("assignment_busy"));
            assert_eq!(
                h.control
                    .mission_store
                    .get_mission(a.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .project,
                a.project
            );
            let response = h.request(true, b.id, json!({"content":"competing mutation", "track":"trio-reserve1", "github_pr":"repo#244"})).await;
            assert!(!response.status().is_success());
            assert!(!bdir.join("requests.jsonl").exists());
            assert_eq!(
                h.state
                    .projects
                    .live_leases(None)
                    .unwrap()
                    .iter()
                    .filter(|l| l.track == "trio-reserve1")
                    .count(),
                1
            );
        }
        std::fs::write(dir.join("release"), "").unwrap();
        wait_native_status(&h, a.id, MissionStatus::AwaitingUser).await;
        let requests: Vec<Value> = std::fs::read_to_string(dir.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1]["params"]["input"][0]["text"],
            "first old continuation"
        );
        assert_eq!(
            requests[2]["params"]["input"][0]["text"],
            "second old continuation"
        );
        // Transfer only after the last old queued turn has finished.
        let response = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, a.id))
            .json(&json!({"track":"new-final", "github_pr":"repo#246"}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let response = h.request(true, b.id, json!({"content":"competing mutation", "track":"trio-reserve1", "github_pr":"repo#244"})).await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        wait_native_status(&h, b.id, MissionStatus::AwaitingUser).await;
        assert!(std::fs::read_to_string(bdir.join("requests.jsonl"))
            .unwrap()
            .contains("competing mutation"));
        NATIVE_FIXTURES.lock().unwrap().remove(&a.id);
        NATIVE_FIXTURES.lock().unwrap().remove(&b.id);
        if let Some((id, dir)) = blocker {
            std::fs::write(dir.join("release"), "").unwrap();
            wait_native_status(&h, id, MissionStatus::Blocked).await;
            NATIVE_FIXTURES.lock().unwrap().remove(&id);
        }
    }
}

#[tokio::test]
async fn pending_deferred_assignment_refuses_multiple_retags_without_concatenating_work() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Pending, Some("repo#244")).await;
    h.control
        .mission_store
        .set_mission_scheduling(
            m.id,
            &crate::api::mission_store::MissionScheduling {
                not_before: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.control
        .mission_store
        .set_deferred_goal(m.id, Some("Implement old RESERVE-1 objective".into()))
        .await
        .unwrap();
    for pr in ["repo#245", "repo#246"] {
        let response = h.request(false, m.id, json!({"content":"Implement unrelated new objective", "track":"new-work", "github_pr":pr})).await;
        assert!(!response.status().is_success());
        assert!(response.text().await.unwrap().contains("assignment_busy"));
        let response = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, m.id))
            .json(&json!({"track":"new-work", "github_pr":pr}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(response.text().await.unwrap().contains("assignment_busy"));
        h.unchanged(&m).await;
        assert_eq!(
            h.control
                .mission_store
                .get_deferred_goal(m.id)
                .await
                .unwrap()
                .as_deref(),
            Some("Implement old RESERVE-1 objective")
        );
        assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    }
    let response = h.request(false, m.id, json!({"content":"Continue old RESERVE-1 with extra evidence", "continue_identity":Harness::assertion(&m)})).await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let deferred = h
        .control
        .mission_store
        .get_deferred_goal(m.id)
        .await
        .unwrap()
        .unwrap();
    assert!(deferred.contains("Implement old RESERVE-1 objective"));
    assert!(deferred.contains("Continue old RESERVE-1 with extra evidence"));
    assert!(!deferred.contains("unrelated new objective"));
}

#[tokio::test]
async fn dequeue_revalidates_track_before_starting_actual_queued_runner() {
    for parallel in [false, true] {
        let h = Harness::new().await;
        let blocker = if parallel {
            let b = h
                .control
                .mission_store
                .create_mission(Some("other"), None, None, None, None, Some("codex"), None)
                .await
                .unwrap();
            let dir = install_native_fixture(&h, b.id, "before").await;
            assert!(h
                .request(false, b.id, json!({"content":"/goal blocker"}))
                .await
                .status()
                .is_success());
            wait_native_file(&dir.join("started")).await;
            Some((b.id, dir))
        } else {
            None
        };
        let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
        let dir = install_native_fixture(&h, m.id, "after").await;
        assert!(h.request(true, m.id, json!({"content":"/goal old assignment", "continue_identity":Harness::assertion(&m)})).await.status().is_success());
        wait_native_file(&dir.join("started")).await;
        assert!(h.request(false, m.id, json!({"content":"retained old queued work", "continue_identity":Harness::assertion(&m)})).await.status().is_success());
        let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
        // Fail only the transactional claim revalidation after acceptance,
        // while the provider is still held at the old-runner barrier.
        db.execute_batch("CREATE TRIGGER refuse_revalidation BEFORE UPDATE OF lease_until ON track_leases BEGIN SELECT RAISE(FAIL, 'dequeue barrier reject'); END;").unwrap();
        let mut events = h.control.events_tx.subscribe();
        std::fs::write(dir.join("release"), "").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Ok(AgentEvent::Error {
                    message,
                    mission_id,
                    ..
                }) = events.recv().await
                {
                    if mission_id == Some(m.id) && message.contains("dequeue barrier reject") {
                        break;
                    }
                }
            }
        })
        .await
        .expect("queued activation revalidated the original track claim");
        assert_eq!(
            std::fs::read_to_string(dir.join("requests.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(h
            .control
            .mission_store
            .load_control_queue(&h.user.id)
            .await
            .unwrap()
            .contains("retained old queued work"));
        assert_eq!(
            h.control
                .mission_store
                .get_mission(m.id)
                .await
                .unwrap()
                .unwrap()
                .project,
            m.project
        );
        // The still-queued assignment must remain protected even after its
        // first runner became quiescent and dequeue was refused.
        let response = h
            .request(
                false,
                m.id,
                json!({"content":"unrelated work", "track":"unrelated", "github_pr":"repo#245"}),
            )
            .await;
        assert!(!response.status().is_success());
        assert!(response.text().await.unwrap().contains("assignment_busy"));
        db.execute_batch("DROP TRIGGER refuse_revalidation")
            .unwrap();
        let response = h
            .request(
                false,
                m.id,
                json!({
                    "content":"Continue old assignment after storage recovery",
                    "continue_identity":Harness::assertion(&m)
                }),
            )
            .await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        wait_native_status(&h, m.id, MissionStatus::AwaitingUser).await;
        let requests = std::fs::read_to_string(dir.join("requests.jsonl")).unwrap();
        assert_eq!(requests.lines().count(), 3);
        assert_eq!(requests.matches("retained old queued work").count(), 1);
        if let Some((id, dir)) = blocker {
            std::fs::write(dir.join("release"), "").unwrap();
            wait_native_status(&h, id, MissionStatus::Blocked).await;
            NATIVE_FIXTURES.lock().unwrap().remove(&id);
        }
        NATIVE_FIXTURES.lock().unwrap().remove(&m.id);
    }
}
