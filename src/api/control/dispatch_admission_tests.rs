use super::*;
use crate::api::{mission_store::SqliteMissionStore, projects_store::ProjectsStore};
use serde_json::{json, Value};

type AdmissionHooks = std::sync::Mutex<HashMap<(Uuid, &'static str), oneshot::Sender<()>>>;
static HOOKS: std::sync::LazyLock<AdmissionHooks> = std::sync::LazyLock::new(Default::default);
pub(crate) fn notify_wait(id: Uuid, kind: &'static str) {
    if let Some(tx) = HOOKS.lock().unwrap().remove(&(id, kind)) {
        let _ = tx.send(());
    }
}
fn wait_for(id: Uuid, kind: &'static str) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    HOOKS.lock().unwrap().insert((id, kind), tx);
    rx
}

struct FixtureDir {
    path: std::path::PathBuf,
    _cleanup: Option<tempfile::TempDir>,
}
impl FixtureDir {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

struct Harness {
    _dir: FixtureDir,
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
        Self::with_nodes(Vec::new()).await
    }

    async fn with_nodes(nodes: Vec<crate::remote_node::RemoteNodeConfig>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dir = FixtureDir {
            path: dir.path().to_path_buf(),
            _cleanup: Some(dir),
        };
        Self::with_directory(dir, nodes).await
    }

    async fn with_directory(
        dir: FixtureDir,
        nodes: Vec<crate::remote_node::RemoteNodeConfig>,
    ) -> Self {
        let path = dir.path();
        let mut config = Config::new(path.to_path_buf());
        config.remote_nodes.enabled = !nodes.is_empty();
        config.remote_nodes.nodes = nodes;
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
            .nest("/remote-build", crate::api::remote_build::routes())
            .route("/message", axum::routing::post(post_message))
            .route("/missions", axum::routing::post(create_mission))
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
        assert_status_metadata_eq(before, &after);
    }
}

fn assert_status_metadata_eq(before: &Mission, after: &Mission) {
    let before = serde_json::to_value(before).unwrap();
    let after = serde_json::to_value(after).unwrap();
    for field in [
        "status",
        "interrupted_at",
        "paused_at",
        "resumable",
        "terminal_reason",
        "terminal_evidence",
        "first_viewed_at",
        "awaiting_kind",
        "last_status_change_at",
    ] {
        assert_eq!(after[field], before[field], "status metadata {field}");
    }
}

async fn track_dispatch_candidate(h: &Harness) -> Mission {
    h.control
        .mission_store
        .create_mission(
            Some("replacement"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap()
}

async fn bind_track_dispatch(
    h: &Harness,
    candidate: &Mission,
) -> Result<String, (StatusCode, String)> {
    // Same boundary as create_mission, including the admission/file/PR order.
    let _admission = DISPATCH_ADMISSION.lock().await;
    let _file_guard = dispatch_admission::durable_lock(&h.state.config)
        .await
        .unwrap();
    let _pr_guard = acquire_durable_pr_writer_lock(&h.state.control)
        .await
        .unwrap();
    bind_mission_to_track(
        &h.state,
        &h.control,
        candidate,
        "lido",
        Some("trio-reserve1"),
        candidate.title.as_deref(),
        None,
        Some("implementation"),
        Some(true),
        &[],
        true,
        None,
        &[],
    )
    .await
}

#[tokio::test]
async fn track_dispatch_replaces_cancelled_and_acknowledged_owner_without_timer() {
    for acknowledge in [false, true] {
        let h = Harness::new().await;
        let old = h.writer(MissionStatus::Active, None).await;
        h.control
            .mission_store
            .update_mission_status(old.id, MissionStatus::Interrupted)
            .await
            .unwrap();
        if acknowledge {
            h.control
                .mission_store
                .update_mission_status(old.id, MissionStatus::Acknowledged)
                .await
                .unwrap();
        }
        let candidate = track_dispatch_candidate(&h).await;
        let old_lease = h.state.projects.live_leases(None).unwrap().remove(0);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&old_lease.lease_until).unwrap()
                > chrono::Utc::now()
        );
        assert!(
            h.state
                .projects
                .acquire_track_lease(&crate::api::track_leases::lease_request(
                    "lido",
                    "trio-reserve1",
                    &candidate.id.to_string(),
                    "writer",
                    None,
                ))
                .is_err(),
            "the unswept lease reproduces the original conflict"
        );

        assert_eq!(
            bind_track_dispatch(&h, &candidate).await.unwrap(),
            "trio-reserve1"
        );
        let leases = h.state.projects.live_leases(None).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt_id, candidate.id.to_string());
        assert_ne!(leases[0].id, old_lease.id);
        // A late cleanup for the cancelled attempt cannot revoke its successor.
        h.state
            .projects
            .release_leases_for_attempt(&old.id.to_string())
            .unwrap();
        assert!(bind_track_dispatch(&h, &candidate).await.is_ok());
        assert_eq!(
            h.state.projects.live_leases(None).unwrap()[0].id,
            leases[0].id
        );
    }
}

#[tokio::test]
async fn track_dispatch_retains_nonterminal_and_native_goal_owners() {
    for status in [
        MissionStatus::Active,
        MissionStatus::Pending,
        MissionStatus::AwaitingUser,
        MissionStatus::WaitingBackground,
        MissionStatus::Paused,
        MissionStatus::Blocked,
    ] {
        let h = Harness::new().await;
        let old = h.writer(status, None).await;
        if status == MissionStatus::Blocked {
            h.control
                .mission_store
                .update_mission_status_with_reason(old.id, status, Some("native_goal_stopped"))
                .await
                .unwrap();
        }
        let candidate = track_dispatch_candidate(&h).await;
        let (status, body) = bind_track_dispatch(&h, &candidate).await.unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["holder_mission_id"], old.id.to_string());
        assert_eq!(
            h.state.projects.live_leases(None).unwrap()[0].attempt_id,
            old.id.to_string()
        );
    }
}

#[tokio::test]
async fn track_dispatch_terminal_presentation_does_not_override_unresolved_execution() {
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    for evidence in ["run", "actor", "remote", "admission"] {
        let h = Harness::new().await;
        let old = h
            .writer(
                if evidence == "run" {
                    MissionStatus::Active
                } else {
                    MissionStatus::Acknowledged
                },
                None,
            )
            .await;
        match evidence {
            "run" => {
                h.control
                    .mission_store
                    .begin_mission_run(old.id, "old-actor", None)
                    .await
                    .unwrap();
                h.control
                    .mission_store
                    .update_mission_status(old.id, MissionStatus::Interrupted)
                    .await
                    .unwrap();
            }
            "actor" => {
                h.control.assignment_owners.write().await.insert(old.id);
            }
            "remote" => {
                job_ledger::record(
                    &h.state.config.working_dir,
                    JobHandle {
                        mission_id: old.id,
                        node_id: "unreachable-node".into(),
                        job_id: Uuid::new_v4(),
                        started_at: chrono::Utc::now(),
                        submission_sequence: 0,
                        accepted_at: Some(chrono::Utc::now()),
                        heartbeat_at: None,
                        disk_reservation_bytes: 0,
                        kind: JobHandleKind::Mission,
                        identity: None,
                        wait_for_completion: None,
                        wake_on_terminal: false,
                    },
                )
                .await
                .unwrap();
            }
            "admission" => {
                h.state
                    .projects
                    .save_dispatch_admission(&old.id.to_string(), &json!({"phase":"pending"}))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let candidate = track_dispatch_candidate(&h).await;
        assert_eq!(
            bind_track_dispatch(&h, &candidate).await.unwrap_err().0,
            StatusCode::CONFLICT,
            "{evidence}"
        );
        assert_eq!(
            h.state.projects.live_leases(None).unwrap()[0].attempt_id,
            old.id.to_string()
        );
    }
}

#[tokio::test]
async fn track_dispatch_unreadable_store_cannot_release_terminal_owner() {
    let h = Harness::new().await;
    let old = h.writer(MissionStatus::Acknowledged, None).await;
    let base = h.state.config.working_dir.join(".sandboxed-sh/missions");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        base.join("missions-unreadable.json"),
        b"incomplete snapshot",
    )
    .unwrap();
    let candidate = track_dispatch_candidate(&h).await;
    assert_eq!(
        bind_track_dispatch(&h, &candidate).await.unwrap_err().0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        h.state.projects.live_leases(None).unwrap()[0].attempt_id,
        old.id.to_string()
    );
    assert_eq!(
        h.control
            .mission_store
            .get_mission(candidate.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        MissionStatus::Interrupted
    );
}

fn isolated_track_http_test(test_name: &str) -> bool {
    // Keep this HTTP/lock regression independent of the production 150 GiB
    // disk floor without changing environment shared by other parallel tests.
    const CHILD: &str = "TERMINAL_TRACK_LEASE_HTTP_TEST_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(test_name) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("api::control::dispatch_admission_tests::{test_name}"),
                "--nocapture",
            ])
            .env(CHILD, test_name)
            .env("MISSION_DISK_EMERGENCY_RESERVE_GB", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated HTTP regression failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return true;
    }
    false
}

#[tokio::test]
async fn explicit_reader_creation_without_pr_survives_queued_activation() {
    if isolated_track_http_test("explicit_reader_creation_without_pr_survives_queued_activation") {
        return;
    }
    for pr in [None, Some("repo#244")] {
        let h = Harness::new().await;
        h.state.backend_registry.write().await.register(Arc::new(
            crate::backend::opencode::OpenCodeBackend::new(
                "http://127.0.0.1:9".into(),
                None,
                false,
            ),
        ));
        let prompt = "Read-only review in verity-integration-b. Write output/review.md; no repository edits.";
        // This is the production trigger: the prose heuristic sees a writer,
        // while the explicit capability admits only a reader.
        assert!(inferred_pr_writer(None, Some("review"), Some(prompt)));
        let response = h
            .state
            .http_client
            .post(format!("{}/missions", h.url))
            .json(&json!({
                "title":"bounded read-only review", "backend":"opencode",
                "project":"lido", "track":"independent-review", "intent":"review",
                "github_pr":pr, "writer":false, "tags":["pr-writer", "ssz"],
                "prompt":prompt, "estimated_disk_gib":1,
                "not_before":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()
            }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert!(status.is_success(), "{status}: {body}");
        let created: Mission = serde_json::from_str(&body).unwrap();
        let stored = h
            .control
            .mission_store
            .get_mission(created.id)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.project.tags.iter().any(|tag| tag == "pr-readonly"));
        assert!(!stored.project.tags.iter().any(|tag| tag == "pr-writer"));
        assert!(!mission_is_pr_writer_with_prompt(&stored, Some(prompt)));
        h.control
            .mission_store
            .log_event(
                created.id,
                &AgentEvent::UserMessage {
                    id: Uuid::new_v4(),
                    content: prompt.into(),
                    queued: true,
                    mission_id: Some(created.id),
                    source: None,
                },
            )
            .await
            .unwrap();
        // Exercise the real dequeue revalidation against the lease acquired
        // by HTTP creation, after the initial prompt is persisted.
        activate_mission_for_message(
            &h.state.control,
            &h.control.mission_store,
            &h.control.events_tx,
            &stored,
            prompt,
        )
        .await
        .unwrap();
        let leases = h.state.projects.live_leases(None).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt_id, created.id.to_string());
        assert_eq!(leases[0].mode, "reader");
        assert!(h
            .control
            .mission_store
            .get_active_mission_run(created.id)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn track_dispatch_http_creation_reconciles_without_deadlocking_actor_or_pr_lock() {
    if isolated_track_http_test(
        "track_dispatch_http_creation_reconciles_without_deadlocking_actor_or_pr_lock",
    ) {
        return;
    }
    for pr in [None, Some("repo#244")] {
        let h = Harness::new().await;
        h.state.backend_registry.write().await.register(Arc::new(
            crate::backend::opencode::OpenCodeBackend::new(
                "http://127.0.0.1:9".into(),
                None,
                false,
            ),
        ));
        let old = h.writer(MissionStatus::Acknowledged, pr).await;
        let response = h.state.http_client.post(format!("{}/missions", h.url))
            .json(&json!({"title":"new replacement", "backend":"opencode", "project":"lido", "track":"trio-reserve1", "github_pr":pr, "writer":true, "estimated_disk_gib":1}))
            .timeout(std::time::Duration::from_secs(10)).send().await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert!(status.is_success(), "{status}: {body}");
        let mission: Mission = serde_json::from_str(&body).unwrap();
        assert_ne!(mission.id, old.id);
        assert_eq!(mission.project.track.as_deref(), Some("trio-reserve1"));
        let leases = h.state.projects.live_leases(None).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt_id, mission.id.to_string());
    }
}

#[tokio::test]
async fn track_dispatch_lock_failure_interrupts_candidate_and_releases_disk() {
    if isolated_track_http_test(
        "track_dispatch_lock_failure_interrupts_candidate_and_releases_disk",
    ) {
        return;
    }
    let h = Harness::new().await;
    h.state.backend_registry.write().await.register(Arc::new(
        crate::backend::opencode::OpenCodeBackend::new("http://127.0.0.1:9".into(), None, false),
    ));
    let old = h.writer(MissionStatus::Acknowledged, None).await;
    // A directory at the lock-file path makes the real open fail, while
    // leaving the actor's separate disk ledger and mission store writable.
    let admission_guard = DISPATCH_ADMISSION.lock().await;
    let file_guard = dispatch_admission::durable_lock(&h.state.config)
        .await
        .unwrap();
    let lock_path = h
        .state
        .config
        .working_dir
        .join(".sandboxed-sh/missions/.dispatch-admission.lock");
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::create_dir(&lock_path).unwrap();
    drop(file_guard);
    drop(admission_guard);
    let response = h.state.http_client.post(format!("{}/missions", h.url))
        .json(&json!({"title":"rejected replacement", "backend":"opencode", "project":"lido", "track":"trio-reserve1", "writer":true, "estimated_disk_gib":1}))
        .timeout(std::time::Duration::from_secs(10)).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.text().await.unwrap();
    let candidates = h
        .control
        .mission_store
        .list_missions_filtered(&Default::default(), 10, 0)
        .await
        .unwrap();
    let candidate = candidates.iter().find(|m| m.id != old.id).unwrap();
    assert_eq!(candidate.status, MissionStatus::Interrupted);
    assert_eq!(
        candidate.terminal_reason.as_deref(),
        Some("dispatch_admission_unavailable")
    );
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["mission_id"], candidate.id.to_string());
    assert!(body["cleanup_error"].is_null());
    assert!(!read_disk_reservation_ledger(&h.state.config)
        .unwrap()
        .reservations
        .contains_key(&candidate.id));
    assert_eq!(
        h.state.projects.live_leases(None).unwrap()[0].attempt_id,
        old.id.to_string()
    );
    assert!(candidate.project.track.is_none());
    assert!(candidate.history.is_empty());
}

#[tokio::test]
async fn track_dispatch_concurrent_replacements_admit_exactly_one_writer() {
    let h = Harness::new().await;
    let old = h.writer(MissionStatus::Acknowledged, None).await;
    let mut candidates = Vec::new();
    for _ in 0..8 {
        candidates.push(track_dispatch_candidate(&h).await);
    }
    let results = futures::future::join_all(
        candidates
            .iter()
            .map(|candidate| bind_track_dispatch(&h, candidate)),
    )
    .await;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|error| error.0 == StatusCode::CONFLICT));
    let winner = candidates
        .iter()
        .zip(&results)
        .find(|(_, result)| result.is_ok())
        .unwrap()
        .0;
    h.state
        .projects
        .release_leases_for_attempt(&old.id.to_string())
        .unwrap();
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    let leases = h.state.projects.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].attempt_id, winner.id.to_string());
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
            assert_eq!(response.status(), StatusCode::CONFLICT);
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
        assert_eq!(response.status(), StatusCode::CONFLICT);
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
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("dispatch_recovery_required"));
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
async fn codex_continuity_http_actor_parks_preserves_ownership_and_suppresses_automation() {
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
                .request(
                    false,
                    b.id,
                    json!({"content":"/goal unrelated synthetic work"})
                )
                .await
                .status()
                .is_success());
            wait_native_file(&dir.join("started")).await;
            Some((b.id, dir))
        } else {
            None
        };
        let m = h.writer(MissionStatus::Failed, Some("repo#244")).await;
        let dir = install_native_fixture(&h, m.id, "continuity_required").await;
        let automation: mission_store::Automation = serde_json::from_value(json!({
            "id":Uuid::new_v4(), "mission_id":m.id,
            "command_source":{"type":"inline", "content":"must not automatically repeat"},
            "trigger":{"type":"agent_finished"}, "active":true,
            "created_at":chrono::Utc::now().to_rfc3339(), "last_triggered_at":null,
            "stop_policy":{"type":"never"}
        }))
        .unwrap();
        h.control
            .mission_store
            .create_automation(automation)
            .await
            .unwrap();
        let response = h.request(true, m.id, json!({"content":"/goal preserve native state", "continue_identity":Harness::assertion(&m)})).await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let parked = wait_native_status(&h, m.id, MissionStatus::Blocked).await;
        assert_eq!(
            parked.terminal_reason.as_deref(),
            Some("codex_continuity_required")
        );
        assert!(parked.goal_mode);
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert_eq!(
            h.state.projects.live_leases(None).unwrap()[0].attempt_id,
            m.id.to_string()
        );
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
        // The finished-turn automation runs after 500 ms if not suppressed.
        tokio::time::sleep(std::time::Duration::from_millis(650)).await;
        assert_eq!(
            std::fs::read_to_string(dir.join("requests.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            h.control
                .mission_store
                .get_mission(m.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            MissionStatus::Blocked
        );
        if let Some((id, dir)) = blocker {
            std::fs::write(dir.join("release"), "").unwrap();
            wait_native_status(&h, id, MissionStatus::Blocked).await;
            NATIVE_FIXTURES.lock().unwrap().remove(&id);
        }
        NATIVE_FIXTURES.lock().unwrap().remove(&m.id);
    }
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
        // A supported terminal presentation edit must not retire the actual
        // main/parallel runner or make its assignment available to sweep.
        let (tx, rx) = oneshot::channel();
        h.control
            .cmd_tx
            .send(ControlCommand::SetMissionStatus {
                id: a.id,
                status: MissionStatus::Failed,
                respond: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert_eq!(
            find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                .await
                .unwrap()
                .unwrap()
                .id,
            a.id
        );
        assert!(h
            .state
            .projects
            .live_leases(None)
            .unwrap()
            .iter()
            .any(|lease| lease.attempt_id == a.id.to_string()));

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

#[tokio::test]
async fn remote_ownership_survives_terminal_presentation_sweep_and_recovery() {
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    for status in [
        MissionStatus::Active,
        MissionStatus::Pending,
        MissionStatus::Failed,
        MissionStatus::Interrupted,
        MissionStatus::Completed,
        MissionStatus::Acknowledged,
        MissionStatus::Blocked,
    ] {
        for pr in [Some("repo#244"), None] {
            let h = Harness::new().await;
            let m = h.writer(status, pr).await;
            let job_id = Uuid::new_v4();
            let stale = chrono::Utc::now() - chrono::Duration::days(7);
            job_ledger::record(
                &h.state.config.working_dir,
                JobHandle {
                    mission_id: m.id,
                    node_id: "unconfigured-recovery-node".into(),
                    job_id,
                    started_at: stale,
                    submission_sequence: 0,
                    accepted_at: Some(stale),
                    heartbeat_at: Some(stale),
                    disk_reservation_bytes: 0,
                    kind: JobHandleKind::Mission,
                    identity: None,
                    wait_for_completion: None,
                    wake_on_terminal: false,
                },
            )
            .await
            .unwrap();
            let mut attached = std::collections::HashSet::new();
            reconcile_pending_handles(
                &h.state,
                &h.state.config.working_dir,
                job_ledger::load(&h.state.config.working_dir).await.unwrap(),
                &mut attached,
            )
            .await;
            assert!(
                attached.is_empty(),
                "configuration loss must remain retryable"
            );
            assert_eq!(
                job_ledger::load(&h.state.config.working_dir)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            crate::api::track_leases::sweep(&h.state).await.unwrap();
            assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
            assert_eq!(
                find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                m.id
            );
            assert!(
                find_existing_pr_writer_global(&h.state.control, "repo#244", Some(m.id))
                    .await
                    .unwrap()
                    .is_none(),
                "same-owner exclusion survives"
            );
            let request = crate::api::track_leases::lease_request(
                "lido",
                "trio-reserve1",
                &Uuid::new_v4().to_string(),
                "writer",
                None,
            );
            assert!(h.state.projects.acquire_track_lease(&request).is_err());
            h.unchanged(&m).await;
            // This test models the terminal wrapper's ledger cleanup. The
            // fake-node lifecycle test must separately establish that only an
            // authoritative node terminal response reaches this cleanup.
            h.control
                .mission_store
                .update_mission_status(m.id, MissionStatus::Completed)
                .await
                .unwrap();
            job_ledger::remove(&h.state.config.working_dir, job_id).await;
            crate::api::track_leases::sweep(&h.state).await.unwrap();
            assert!(
                find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(h.state.projects.acquire_track_lease(&request).is_ok());
        }
    }
}

#[tokio::test]
async fn remote_poll_loss_and_cancel_ack_retain_ownership_until_terminal_cleanup() {
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let h = Harness::new().await;
    let owner = h.writer(MissionStatus::Active, Some("repo#244")).await;
    let job_id = Uuid::new_v4();
    let phase = Arc::new(AtomicUsize::new(0));
    let failed_cancels = Arc::new(AtomicUsize::new(0));
    let acknowledged_cancels = Arc::new(AtomicUsize::new(0));
    let observed_phase = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .route(
            "/jobs/:id",
            axum::routing::get({
                let phase = phase.clone();
                let observed_phase = observed_phase.clone();
                move || {
                    let phase = phase.clone();
                    let observed_phase = observed_phase.clone();
                    async move {
                        let current = phase.load(Ordering::SeqCst);
                        if current == 0 {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({"error":"observation unavailable"})),
                            );
                        }
                        observed_phase.store(current, Ordering::SeqCst);
                        if current == 3 {
                            return (StatusCode::NOT_FOUND, Json(json!({"error":"job record missing"})));
                        }
                        (
                            StatusCode::OK,
                            Json(json!({
                                "job_id":job_id, "mission_id":owner.id,
                                "state":match current {1 => "running", 2 => "lost", _ => "cancelled"},
                                "created_at":chrono::Utc::now().to_rfc3339(),
                            })),
                        )
                    }
                }
            }),
        )
        .route(
            "/jobs/:id/cancel",
            axum::routing::post({
                let phase = phase.clone();
                let failed_cancels = failed_cancels.clone();
                let acknowledged_cancels = acknowledged_cancels.clone();
                move || {
                    let phase = phase.clone();
                    let failed_cancels = failed_cancels.clone();
                    let acknowledged_cancels = acknowledged_cancels.clone();
                    async move {
                        if phase.load(Ordering::SeqCst) == 0 {
                            failed_cancels.fetch_add(1, Ordering::SeqCst);
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({"error":"cancel unavailable"})),
                            );
                        }
                        if phase.load(Ordering::SeqCst) == 3 {
                            return (StatusCode::NOT_FOUND, Json(json!({"error":"job record missing"})));
                        }
                        acknowledged_cancels.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::OK,
                            Json(
                                json!({"job_id":job_id,"state":"running","cancel_requested":true}),
                            ),
                        )
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node = crate::remote_node::RemoteNodeConfig {
        id: "lifetime-test-node".into(),
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        token_env: "UNUSED_LIFETIME_TEST_TOKEN".into(),
        labels: None,
    };
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // Model an already accepted remote job. From here the real HTTP client,
    // poll loop, mission finalizer and durable terminal cleanup own lifecycle.
    job_ledger::record(
        &h.state.config.working_dir,
        JobHandle {
            mission_id: owner.id,
            node_id: node.id.clone(),
            job_id,
            started_at: chrono::Utc::now(),
            submission_sequence: 0,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Mission,
            identity: None,
            wait_for_completion: None,
            wake_on_terminal: false,
        },
    )
    .await
    .unwrap();
    let poller = tokio::spawn({
        let ledger_dir = h.state.config.working_dir.clone();
        let fleet = h.state.fleet.clone();
        let poll_owner = RemoteMissionOwner::live(&h.control);
        async move {
            poll_remote_job(
                &ledger_dir,
                poll_owner,
                fleet,
                crate::remote_node::RemoteNodeClient::default(),
                node,
                "fixture-token".into(),
                owner.id,
                job_id,
                chrono::Utc::now(),
            )
            .await;
        }
    });
    let competitor = h
        .control
        .mission_store
        .create_mission(
            Some("Competitor"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap();
    h.control
        .mission_store
        .update_mission_project(
            competitor.id,
            crate::api::mission_store::MissionProjectPatch {
                project: Some(Some("lido".into())),
                track: Some(Some("competitor-track".into())),
                github_pr: Some(Some("repo#245".into())),
                tags: Some(vec!["pr-writer".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.control
        .mission_store
        .update_mission_status(competitor.id, MissionStatus::Failed)
        .await
        .unwrap();
    let competitor = h
        .control
        .mission_store
        .get_mission(competitor.id)
        .await
        .unwrap()
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(40), async {
        while failed_cancels.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("five failed observations must lead to cancellation retry");
    let failed = h
        .control
        .mission_store
        .get_mission(owner.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MissionStatus::Failed);
    assert_eq!(failed.terminal_reason.as_deref(), Some("remote_node_lost"));

    for next_phase in 0..=3 {
        if next_phase > 0 {
            phase.store(next_phase, Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while acknowledged_cancels.load(Ordering::SeqCst) == 0
                    || observed_phase.load(Ordering::SeqCst) != next_phase
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
        assert!(!poller.is_finished());
        assert_eq!(
            job_ledger::load(&h.state.config.working_dir)
                .await
                .unwrap()
                .len(),
            1
        );
        // An outage may exceed lease TTL. Check admission BEFORE sweep has
        // had any chance to renew the old writer's claim.
        rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap()
            .execute("UPDATE track_leases SET lease_until='2000-01-01T00:00:00+00:00' WHERE attempt_id=?1", [owner.id.to_string()]).unwrap();
        for pr in ["repo#244", ""] {
            let response = h
                .request(
                    true,
                    competitor.id,
                    json!({
                        "content":"Implement replacement work", "title":"Replacement",
                        "github_pr":pr, "track":"trio-reserve1",
                    }),
                )
                .await;
            assert!(!response.status().is_success());
            let error = response.text().await.unwrap();
            assert!(
                error.contains("held") || error.contains("track_owned"),
                "{error}"
            );
            h.unchanged(&competitor).await;
        }
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert!(h
            .state
            .projects
            .live_leases(None)
            .unwrap()
            .iter()
            .any(|lease| lease.attempt_id == owner.id.to_string()));
        assert_eq!(
            find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                .await
                .unwrap()
                .unwrap()
                .id,
            owner.id
        );
    }
    // Terminal proof must survive a transient ledger write/read failure and
    // a subsequent loss of the node. No second terminal response is required.
    let receipt_path = h
        .state
        .config
        .working_dir
        .join(".sandboxed-sh/remote-job-receipts.json");
    std::fs::create_dir(&receipt_path).unwrap();
    let cleanup_failed = wait_for(job_id, "remote_cleanup_failed");
    phase.store(4, Ordering::SeqCst);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while observed_phase.load(Ordering::SeqCst) != 4 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), cleanup_failed)
        .await
        .unwrap()
        .unwrap();
    assert!(!poller.is_finished());
    assert_eq!(
        job_ledger::load(&h.state.config.working_dir)
            .await
            .unwrap()
            .len(),
        1
    );
    phase.store(3, Ordering::SeqCst);
    std::fs::remove_dir(receipt_path).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), poller)
        .await
        .unwrap()
        .unwrap();
    assert!(job_ledger::load(&h.state.config.working_dir)
        .await
        .unwrap()
        .is_empty());
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert!(
        find_existing_pr_writer_global(&h.state.control, "repo#244", None)
            .await
            .unwrap()
            .is_none()
    );
    let response = h
        .request(
            true,
            competitor.id,
            json!({
                "content":"Implement replacement work", "title":"Replacement",
                "github_pr":"repo#244", "track":"trio-reserve1",
            }),
        )
        .await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn execution_ownership_retains_live_and_offline_runs_until_reconciled() {
    for kind in ["live", "sqlite", "file"] {
        let h = Harness::new().await;
        let base = h.state.config.working_dir.join(".sandboxed-sh/missions");
        let store: Arc<dyn MissionStore> = match kind {
            "live" => h.control.mission_store.clone(),
            "sqlite" => Arc::new(
                SqliteMissionStore::new(base.clone(), "offline")
                    .await
                    .unwrap(),
            ),
            _ => Arc::new(
                mission_store::FileMissionStore::new(base.clone(), "offline")
                    .await
                    .unwrap(),
            ),
        };
        let mission = store
            .create_mission(
                Some("offline owner"),
                None,
                None,
                None,
                None,
                Some("test-no-execution"),
                None,
            )
            .await
            .unwrap();
        store
            .update_mission_project(
                mission.id,
                crate::api::mission_store::MissionProjectPatch {
                    project: Some(Some("lido".into())),
                    track: Some(Some("offline-track".into())),
                    github_pr: Some(Some("repo#991".into())),
                    tags: Some(vec!["pr-writer".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let run = store
            .begin_mission_run(mission.id, "test-owner", None)
            .await
            .unwrap();
        h.state
            .projects
            .absorb_track("lido", "offline-track", None, None)
            .unwrap();
        let request = crate::api::track_leases::lease_request(
            "lido",
            "offline-track",
            &mission.id.to_string(),
            "writer",
            None,
        );
        h.state.projects.acquire_track_lease(&request).unwrap();
        let competitor = crate::api::track_leases::lease_request(
            "lido",
            "offline-track",
            &Uuid::new_v4().to_string(),
            "writer",
            None,
        );
        for status in [
            MissionStatus::Failed,
            MissionStatus::Interrupted,
            MissionStatus::Acknowledged,
        ] {
            let update = store.update_mission_status(mission.id, status).await;
            if status == MissionStatus::Acknowledged {
                assert!(update.unwrap_err().contains("cannot be acknowledged"));
            } else {
                update.unwrap();
            }
            crate::api::track_leases::sweep(&h.state).await.unwrap();
            assert!(
                h.state.projects.acquire_track_lease(&competitor).is_err(),
                "{kind} {status}"
            );
            assert_eq!(
                find_existing_pr_writer_global(&h.state.control, "repo#991", None)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                mission.id
            );
            assert!(
                find_existing_pr_writer_global(&h.state.control, "repo#991", Some(mission.id))
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        store
            .finish_mission_run(run.run_id, run.generation, Some("confirmed-exit"))
            .await
            .unwrap();
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert!(
            find_existing_pr_writer_global(&h.state.control, "repo#991", None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(h.state.projects.acquire_track_lease(&competitor).is_ok());
    }
}

#[tokio::test]
async fn execution_ownership_corrupt_offline_store_cannot_release_overdue_claims() {
    let h = Harness::new().await;
    let mission = h.writer(MissionStatus::Completed, Some("repo#244")).await;
    let base = h.state.config.working_dir.join(".sandboxed-sh/missions");
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("missions-unreadable.json");
    std::fs::write(&path, b"incomplete snapshot").unwrap();
    rusqlite::Connection::open(h._dir.path().join("projects.db"))
        .unwrap()
        .execute(
            "UPDATE track_leases SET lease_until='2000-01-01T00:00:00+00:00'",
            [],
        )
        .unwrap();
    assert!(crate::api::track_leases::sweep(&h.state).await.is_err());
    assert!(
        find_existing_pr_writer_global(&h.state.control, "repo#244", None)
            .await
            .is_err()
    );
    assert_eq!(
        h.state.projects.live_leases(None).unwrap()[0].attempt_id,
        mission.id.to_string()
    );
    std::fs::write(path, br#"{"missions":{},"runs":{}}"#).unwrap();
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert!(h.state.projects.live_leases(None).unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_recovery_retains_accepted_and_tentative_jobs_on_404_and_lost() {
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    for kind in [JobHandleKind::Mission, JobHandleKind::Tentative] {
        let h = Harness::new().await;
        let owner = h.writer(MissionStatus::Interrupted, Some("repo#244")).await;
        let job_id = Uuid::new_v4();
        let phase = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(AtomicUsize::new(usize::MAX));
        let app = axum::Router::new()
            .route(
                "/jobs/:id",
                axum::routing::get({
                    let phase = phase.clone();
                    let observed = observed.clone();
                    move || {
                        let phase = phase.clone();
                        let observed = observed.clone();
                        async move {
                            let value = phase.load(Ordering::SeqCst);
                            observed.store(value, Ordering::SeqCst);
                            if value == 0 {
                                return (StatusCode::NOT_FOUND, Json(json!({"error":"missing"})));
                            }
                            (
                                StatusCode::OK,
                                Json(json!({"job_id":job_id, "mission_id":owner.id,
                            "state": if value == 1 {"lost"} else {"cancelled"},
                            "created_at":chrono::Utc::now().to_rfc3339()})),
                            )
                        }
                    }
                }),
            )
            .route(
                "/jobs/:id/cancel",
                axum::routing::post(|| async { StatusCode::NOT_FOUND }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node = crate::remote_node::RemoteNodeConfig {
            id: "recovery-fixture".into(),
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            token_env: "UNUSED_RECOVERY_FIXTURE_TOKEN".into(),
            labels: None,
        };
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        job_ledger::record(
            &h.state.config.working_dir,
            JobHandle {
                mission_id: owner.id,
                node_id: node.id.clone(),
                job_id,
                started_at: chrono::Utc::now(),
                submission_sequence: 0,
                accepted_at: (kind == JobHandleKind::Mission).then(chrono::Utc::now),
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind,
                identity: None,
                wait_for_completion: None,
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();
        let observer = tokio::spawn(observe_untracked_remote_job_cancellation(
            h.state.fleet.clone(),
            node,
            "fixture".into(),
            owner.id,
            job_id,
            chrono::Utc::now(),
            h.state.config.working_dir.clone(),
        ));
        for value in [0, 1] {
            phase.store(value, Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while observed.load(Ordering::SeqCst) != value {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(!observer.is_finished());
            assert_eq!(
                job_ledger::load(&h.state.config.working_dir)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            crate::api::track_leases::sweep(&h.state).await.unwrap();
            assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
            assert_eq!(
                find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                owner.id
            );
        }
        phase.store(2, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(10), observer)
            .await
            .unwrap()
            .unwrap();
        assert!(job_ledger::load(&h.state.config.working_dir)
            .await
            .unwrap()
            .is_empty());
        crate::api::track_leases::sweep(&h.state).await.unwrap();
        assert!(h.state.projects.live_leases(None).unwrap().is_empty());
        assert!(
            find_existing_pr_writer_global(&h.state.control, "repo#244", None)
                .await
                .unwrap()
                .is_none()
        );
        server.abort();
    }
}

#[tokio::test]
async fn http_terminal_build_response_retains_cleanup_after_request_and_node_loss() {
    // Isolate the capability signing secret from concurrently running tests.
    const CHILD: &str = "PR889_HTTP_TERMINAL_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "api::control::dispatch_admission_tests::http_terminal_build_response_retains_cleanup_after_request_and_node_loss", "--nocapture"])
            .env(CHILD, "1").env("SANDBOXED_INTERNAL_ACTION_SECRET", "isolated-test-only-secret")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node = crate::remote_node::RemoteNodeConfig {
        id: "http-terminal-node".into(),
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        // A nonsecret existing variable avoids process-global env mutation.
        token_env: "PATH".into(),
        labels: None,
    };
    let h = Harness::with_nodes(vec![node.clone()]).await;
    let owner = h.writer(MissionStatus::Failed, Some("repo#244")).await;
    let job_id = Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route("/jobs/:id", axum::routing::get({
        let calls = calls.clone();
        move || {
            let calls = calls.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) > 0 {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"node observation lost"})));
                }
                (StatusCode::OK, Json(json!({"job_id":job_id,"mission_id":owner.id,"state":"succeeded","exit_code":0,"created_at":chrono::Utc::now().to_rfc3339()})))
            }
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    job_ledger::record(
        &h.state.config.working_dir,
        JobHandle {
            mission_id: owner.id,
            node_id: node.id.clone(),
            job_id,
            started_at: chrono::Utc::now(),
            submission_sequence: 0,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: None,
            wait_for_completion: Some(false),
            wake_on_terminal: false,
        },
    )
    .await
    .unwrap();
    let receipts = h
        .state
        .config
        .working_dir
        .join(".sandboxed-sh/remote-job-receipts.json");
    std::fs::create_dir(&receipts).unwrap();
    let token = crate::api::remote_build::build_remote_build_token(owner.id).unwrap();
    let response = h
        .state
        .http_client
        .get(format!("{}/remote-build/{}", h.url, job_id))
        .query(&[("mission_id", owner.id.to_string()), ("node_id", node.id)])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let response: Value = response.json().await.unwrap();
    assert_eq!(response["state"], "succeeded");
    assert_eq!(
        job_ledger::load(&h.state.config.working_dir)
            .await
            .unwrap()
            .len(),
        1
    );
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert_eq!(
        find_existing_pr_writer_global(&h.state.control, "repo#244", None)
            .await
            .unwrap()
            .unwrap()
            .id,
        owner.id
    );
    // The HTTP request is over and the node will never repeat terminal proof.
    std::fs::remove_dir(receipts).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !job_ledger::load(&h.state.config.working_dir)
            .await
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "cleanup must use the retained proof"
    );
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert!(
        find_existing_pr_writer_global(&h.state.control, "repo#244", None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(h.state.projects.live_leases(None).unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn configured_recovery_polls_offline_owner_until_confirmed_cancellation() {
    use crate::remote_node::job_ledger::{self, JobHandle, JobHandleKind};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node = crate::remote_node::RemoteNodeConfig {
        id: "offline-recovery-node".into(),
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        token_env: "PATH".into(),
        labels: None,
    };
    let h = Harness::with_nodes(vec![node.clone()]).await;
    let store: Arc<dyn MissionStore> = Arc::new(
        SqliteMissionStore::new(
            h.state.config.working_dir.join(".sandboxed-sh/missions"),
            "offline-recovered-owner",
        )
        .await
        .unwrap(),
    );
    let owner = store
        .create_mission(
            Some("offline writer"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap();
    store
        .update_mission_project(
            owner.id,
            crate::api::mission_store::MissionProjectPatch {
                project: Some(Some("lido".into())),
                track: Some(Some("recovery-track".into())),
                github_pr: Some(Some("repo#882".into())),
                tags: Some(vec!["pr-writer".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .update_mission_status(owner.id, MissionStatus::Interrupted)
        .await
        .unwrap();
    h.state
        .projects
        .absorb_track("lido", "recovery-track", None, None)
        .unwrap();
    h.state
        .projects
        .acquire_track_lease(&crate::api::track_leases::lease_request(
            "lido",
            "recovery-track",
            &owner.id.to_string(),
            "writer",
            None,
        ))
        .unwrap();
    let job_id = Uuid::new_v4();
    let terminal = Arc::new(AtomicBool::new(false));
    let cancels = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .route("/jobs/:id", axum::routing::get({
            let terminal = terminal.clone();
            move || { let terminal = terminal.clone(); async move {
                Json(json!({"job_id":job_id,"mission_id":owner.id,"state":if terminal.load(Ordering::SeqCst) {"cancelled"} else {"running"},"created_at":chrono::Utc::now().to_rfc3339()}))
            }}
        }))
        .route("/jobs/:id/cancel", axum::routing::post({
            let cancels = cancels.clone();
            move || { let cancels = cancels.clone(); async move {
                cancels.fetch_add(1, Ordering::SeqCst);
                Json(json!({"job_id":job_id,"state":"running","cancel_requested":true}))
            }}
        }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    job_ledger::record(
        &h.state.config.working_dir,
        JobHandle {
            mission_id: owner.id,
            node_id: node.id,
            job_id,
            started_at: chrono::Utc::now(),
            submission_sequence: 0,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Mission,
            identity: None,
            wait_for_completion: None,
            wake_on_terminal: false,
        },
    )
    .await
    .unwrap();
    let mut attached = HashSet::new();
    reconcile_pending_handles(
        &h.state,
        &h.state.config.working_dir,
        job_ledger::load(&h.state.config.working_dir).await.unwrap(),
        &mut attached,
    )
    .await;
    assert_eq!(attached, HashSet::from([job_id]));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while cancels.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert_eq!(
        find_existing_pr_writer_global(&h.state.control, "repo#882", None)
            .await
            .unwrap()
            .unwrap()
            .id,
        owner.id
    );
    assert_eq!(h.state.projects.live_leases(None).unwrap().len(), 1);
    terminal.store(true, Ordering::SeqCst);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !job_ledger::load(&h.state.config.working_dir)
            .await
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store.get_mission(owner.id).await.unwrap().unwrap().status,
        MissionStatus::Interrupted
    );
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert!(h.state.projects.live_leases(None).unwrap().is_empty());
    assert!(
        find_existing_pr_writer_global(&h.state.control, "repo#882", None)
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
}

async fn assert_resume_rejection(check: &str) {
    let h = Harness::new().await;
    let prior = h
        .control
        .mission_store
        .create_mission(
            Some("prior context"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap();
    let prior_history = vec![MissionHistoryEntry {
        role: "user".into(),
        content: "prior history must survive".into(),
    }];
    h.control
        .mission_store
        .log_event(
            prior.id,
            &AgentEvent::UserMessage {
                id: Uuid::new_v4(),
                content: prior_history[0].content.clone(),
                queued: false,
                mission_id: Some(prior.id),
                source: None,
            },
        )
        .await
        .unwrap();
    let (tx, rx) = oneshot::channel();
    h.control
        .cmd_tx
        .send(ControlCommand::LoadMission {
            id: prior.id,
            respond: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap().unwrap();
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    for status in [
        MissionStatus::Failed,
        MissionStatus::Interrupted,
        MissionStatus::Blocked,
        MissionStatus::Paused,
    ] {
        let m = h.writer(status, None).await;
        db.execute("UPDATE missions SET terminal_reason='original diagnosis',
            interrupted_at='2026-01-01T01:00:00Z', paused_at='2026-01-01T02:00:00Z', resumable=1,
            first_viewed_at='2026-01-01T03:00:00Z', awaiting_kind='decision', last_status_change_at='2026-01-01T04:00:00Z'
            WHERE id=?1", [m.id.to_string()]).unwrap();
        h.control
            .mission_store
            .set_terminal_evidence(m.id, "observed failure")
            .await
            .unwrap();
        let before = h
            .control
            .mission_store
            .get_mission(m.id)
            .await
            .unwrap()
            .unwrap();
        h.control
            .mission_store
            .log_event(
                m.id,
                &AgentEvent::UserMessage {
                    id: Uuid::new_v4(),
                    content: "rejected history must not replace prior context".into(),
                    queued: false,
                    mission_id: Some(m.id),
                    source: None,
                },
            )
            .await
            .unwrap();
        let failure = if check == "run" {
            db.execute_batch("CREATE TRIGGER refuse_queue BEFORE INSERT ON mission_runs BEGIN SELECT RAISE(FAIL, 'adversarial run failure'); END;").unwrap();
            "adversarial run failure"
        } else {
            db.execute_batch("CREATE TRIGGER refuse_queue BEFORE INSERT ON control_queue BEGIN SELECT RAISE(FAIL, 'adversarial queue failure'); END;").unwrap();
            "adversarial queue failure"
        };
        let mut events = h.control.events_tx.subscribe();
        let response = h.request(true, m.id, json!({"content":"different work", "track":"new-track", "github_pr":"", "title":"different work"})).await;
        assert!(!response.status().is_success());
        assert!(response.text().await.unwrap().contains(failure));
        if check == "metadata" || check == "run" {
            h.unchanged(&before).await;
        }
        if check == "actor" || check == "run" {
            assert_eq!(*h.control.current_mission.read().await, Some(prior.id));
        }
        let mut statuses = Vec::new();
        let mut control_statuses = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let AgentEvent::Status {
                state,
                queue_len,
                mission_id,
            } = &event
            {
                control_statuses.push((*state, *queue_len, *mission_id));
            }
            if let AgentEvent::MissionStatusChanged {
                mission_id, status, ..
            } = event
            {
                if mission_id == m.id {
                    statuses.push(status);
                }
            }
        }
        if check == "events" || check == "run" {
            assert!(statuses.contains(&MissionStatus::Active));
            assert_eq!(
                statuses.last(),
                Some(&status),
                "rejection must compensate Active event"
            );
        }
        if check == "run" {
            let shared = h.control.status.read().await;
            assert_eq!(
                shared.state,
                ControlRunState::Idle,
                "rejected resume must restore shared control status"
            );
            assert_eq!(shared.queue_len, 0);
            assert_eq!(shared.mission_id, None);
            assert!(control_statuses
                .iter()
                .any(|s| s.0 == ControlRunState::Running));
            assert_eq!(
                control_statuses.last(),
                Some(&(ControlRunState::Idle, 0, None)),
                "rejection must compensate the Running event"
            );
        }
        db.execute_batch("DROP TRIGGER refuse_queue").unwrap();
        let (tx, rx) = oneshot::channel();
        h.control
            .cmd_tx
            .send(ControlCommand::InspectActorContext { respond: tx })
            .await
            .unwrap();
        let (current, history) = rx.await.unwrap();
        if check == "actor" || check == "run" {
            assert_eq!(current, Some(prior.id));
            assert_eq!(
                history,
                vec![("user".to_string(), prior_history[0].content.clone())]
            );
        }
        h.state
            .projects
            .release_leases_for_attempt(&m.id.to_string())
            .unwrap();
    }
}

#[tokio::test]
async fn adversarial_resume_rejection_restores_diagnostics() {
    assert_resume_rejection("metadata").await;
}
#[tokio::test]
async fn adversarial_resume_rejection_restores_actor_context_and_history() {
    assert_resume_rejection("actor").await;
}
#[tokio::test]
async fn adversarial_resume_rejection_compensates_active_event() {
    assert_resume_rejection("events").await;
}

#[tokio::test]
async fn adversarial_resume_run_acquisition_rejection_restores_actor_and_metadata() {
    assert_resume_rejection("run").await;
}

#[tokio::test]
async fn adversarial_project_recovery_conflict_is_409() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    h.state
        .projects
        .save_dispatch_admission(&m.id.to_string(), &json!({"phase":"pending"}))
        .unwrap();
    let response = h
        .state
        .http_client
        .patch(format!("{}/missions/{}/project", h.url, m.id))
        .json(&json!({"track":"different-track"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    h.unchanged(&m).await;
}

#[tokio::test]
async fn adversarial_startup_recovery_does_not_skip_its_own_identity_restore() {
    assert_startup_identity_recovery(false).await;
}

#[tokio::test]
async fn adversarial_startup_recovery_survives_preceding_global_sweep() {
    assert_startup_identity_recovery(true).await;
}

async fn assert_startup_identity_recovery(preceding_sweep: bool) {
    let admission = DISPATCH_ADMISSION.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteMissionStore::new(dir.path().join("missions"), "admission-test")
        .await
        .unwrap();
    let m = store
        .create_mission(
            Some("old active work"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap();
    store
        .update_mission_status(m.id, MissionStatus::Active)
        .await
        .unwrap();
    let db =
        rusqlite::Connection::open(dir.path().join("missions/missions-admission-test.db")).unwrap();
    db.execute(
        "UPDATE missions SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
        [m.id.to_string()],
    )
    .unwrap();
    let before = store.get_mission(m.id).await.unwrap().unwrap();
    let projects = ProjectsStore::open(dir.path().join("projects.db")).unwrap();
    projects
        .save_dispatch_admission(
            &m.id.to_string(),
            &json!({
                "phase":"preparing", "identity_changed":true, "actor_may_have_started":false,
                "before":{"title":before.title,"project":before.project}, "acquired":[]
            }),
        )
        .unwrap();
    let h = Harness::with_directory(
        FixtureDir {
            path: dir.path().to_path_buf(),
            _cleanup: Some(dir),
        },
        Vec::new(),
    )
    .await;
    let mut events = h.control.events_tx.subscribe();
    if preceding_sweep {
        // A periodic sweep or another session's startup can own this lock
        // before this session captures its candidates. Reconcile globally
        // while this session is waiting, preserving that exact ordering.
        let _file = dispatch_admission::durable_lock(&h.state.config)
            .await
            .unwrap();
        dispatch_admission::recover_sweep(&h.state).await.unwrap();
    }
    drop(admission);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let Ok(AgentEvent::MissionStatusChanged {
                completion: None,
                execution: None,
                mission_id,
                status: MissionStatus::Interrupted,
                summary: Some(summary),
            }) = events.recv().await
            {
                if mission_id == m.id && summary.contains("server restarted") {
                    break;
                }
            }
        }
    })
    .await
    .expect("receipt recovery must not suppress restart of an old Active mission");
    assert!(h
        .state
        .projects
        .dispatch_admission(&m.id.to_string())
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn adversarial_journal_insert_failure_cannot_leave_provisional_lease_even_when_cleanup_fails()
{
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_journal BEFORE INSERT ON mission_dispatch_admissions BEGIN SELECT RAISE(FAIL, 'journal insert failure'); END;
        CREATE TRIGGER refuse_cleanup BEFORE UPDATE OF state ON track_leases BEGIN SELECT RAISE(FAIL, 'cleanup failure'); END;").unwrap();
    for resume in [false, true] {
        let response = h.request(resume, m.id, json!({"content":"different work", "track":"new-track", "github_pr":"", "title":"different work"})).await;
        assert!(!response.status().is_success());
        h.unchanged(&m).await;
        let reopened = ProjectsStore::open(h._dir.path().join("projects.db")).unwrap();
        assert_eq!(
            reopened.live_leases(None).unwrap().len(),
            1,
            "failed journal must roll back INSERT without compensation"
        );
        assert!(reopened
            .dispatch_admission(&m.id.to_string())
            .unwrap()
            .is_none());
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
    assert_eq!(
        ProjectsStore::open(h._dir.path().join("projects.db"))
            .unwrap()
            .live_leases(None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn adversarial_project_edit_cleanup_retries_after_database_reopen() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let db = rusqlite::Connection::open(h._dir.path().join("projects.db")).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_cleanup BEFORE UPDATE OF state ON track_leases BEGIN SELECT RAISE(FAIL, 'cleanup failure'); END;").unwrap();
    let response = h
        .state
        .http_client
        .patch(format!("{}/missions/{}/project", h.url, m.id))
        .json(&json!({"track":"new-track", "desired_state":"awaiting-ci"}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let reopened = ProjectsStore::open(h._dir.path().join("projects.db")).unwrap();
    assert_eq!(reopened.live_leases(None).unwrap().len(), 2);
    assert!(reopened
        .dispatch_admission(&m.id.to_string())
        .unwrap()
        .is_some());
    db.execute_batch("DROP TRIGGER refuse_cleanup").unwrap();
    // A concurrent internal annotation after commit must not strand cleanup.
    let mut tags = m.project.tags.clone();
    tags.push("orphaned".into());
    h.control
        .mission_store
        .update_mission_project(
            m.id,
            crate::api::mission_store::MissionProjectPatch {
                tags: Some(tags),
                desired_state: Some(Some("ready".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    let leases = reopened.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].track, "new-track");
    assert!(reopened
        .dispatch_admission(&m.id.to_string())
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn adversarial_concurrent_tags_survive_rejected_retag_and_rollback_retry() {
    assert_concurrent_tags_rejection(false).await;
}

#[tokio::test]
async fn adversarial_resume_cleanup_recovery_conflict_is_409() {
    assert_concurrent_tags_rejection(true).await;
}

async fn assert_concurrent_tags_rejection(resume: bool) {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
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
        let store = admission.store.clone();
        let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
            .await
            .unwrap();
        // This mutation lands after the admission snapshot and before rollback.
        let mut mission = store.get_mission(m.id).await.unwrap().unwrap();
        mission.project.tags.extend([
            "superseded".into(),
            "superseded_by:replacement".into(),
            "orphaned".into(),
        ]);
        store
            .update_mission_project(
                m.id,
                crate::api::mission_store::MissionProjectPatch {
                    tags: Some(mission.project.tags),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("CREATE TRIGGER refuse_rollback BEFORE UPDATE OF track ON missions BEGIN SELECT RAISE(FAIL, 'rollback failure'); END;").unwrap();
        match command {
            ControlCommand::UserMessage { respond, .. } => respond
                .send(UserMessageAck::Rejected("injected refusal".into()))
                .unwrap(),
            ControlCommand::ResumeMission { respond, .. } => {
                respond.send(Err("injected refusal".into())).unwrap()
            }
            _ => panic!(),
        }
    });
    let response = h.request(resume, m.id, json!({"content":"different work", "track":"new-track", "github_pr":"", "title":"different work"})).await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    actor.await.unwrap();
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute_batch("DROP TRIGGER refuse_rollback").unwrap();
    let _guard = DISPATCH_ADMISSION.lock().await;
    let _file = dispatch_admission::durable_lock(&h.state.config)
        .await
        .unwrap();
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
    for tag in [
        "pr-writer",
        "superseded",
        "superseded_by:replacement",
        "orphaned",
    ] {
        assert!(
            after.project.tags.iter().any(|value| value == tag),
            "lost {tag}"
        );
    }
    assert_eq!(after.project.track, m.project.track);
}

/// Only active in the explicitly spawned crash-test child. Exits without
/// unwinding, dropping receipts, compensating leases, or running destructors.
pub(crate) fn crash_checkpoint(phase: &str) {
    if std::env::var("PR889_CRASH_PHASE")
        .ok()
        .is_some_and(|requested| requested.trim_end_matches("_offline") == phase)
    {
        std::process::exit(86);
    }
}

#[tokio::test]
async fn adversarial_crash_child() {
    let Ok(path) = std::env::var("PR889_CRASH_DIR") else {
        return;
    };
    let h = Harness::with_directory(
        FixtureDir {
            path: path.into(),
            _cleanup: None,
        },
        Vec::new(),
    )
    .await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let db = rusqlite::Connection::open(h._dir.path().join("missions/missions-admission-test.db"))
        .unwrap();
    db.execute("UPDATE missions SET terminal_reason='prior diagnosis', interrupted_at='2026-01-01T01:00:00Z', paused_at='2026-01-01T02:00:00Z', resumable=1, first_viewed_at='2026-01-01T03:00:00Z' WHERE id=?1", [m.id.to_string()]).unwrap();
    let m = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    std::fs::write(
        h._dir.path().join("before.json"),
        serde_json::to_vec(&m).unwrap(),
    )
    .unwrap();
    let phase = std::env::var("PR889_CRASH_PHASE").unwrap();
    if phase.starts_with("dispatch_rejected") {
        db.execute_batch("CREATE TRIGGER refuse_queue BEFORE INSERT ON control_queue BEGIN SELECT RAISE(FAIL, 'queue failure'); END;
            CREATE TRIGGER refuse_status BEFORE UPDATE OF status ON missions WHEN NEW.status='paused' BEGIN SELECT RAISE(FAIL, 'status rollback failure'); END;").unwrap();
    }
    if phase.starts_with("project_edit") {
        let _ = h
            .state
            .http_client
            .patch(format!("{}/missions/{}/project", h.url, m.id))
            .json(&json!({"track":"new-track"}))
            .send()
            .await
            .unwrap();
    } else {
        let _ = h.request(true, m.id, json!({"content":"different work", "track":"new-track", "github_pr":"", "title":"different work"})).await;
    }
    panic!("crash checkpoint was not reached: {phase}");
}

async fn assert_crash_recovery(phase: &'static str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let test_exe = std::env::current_exe().unwrap();
    let status = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            std::process::Command::new(test_exe)
                .args([
                    "--exact",
                    "api::control::dispatch_admission_tests::adversarial_crash_child",
                    "--nocapture",
                ])
                .env("PR889_CRASH_DIR", path)
                .env("PR889_CRASH_PHASE", phase)
                .status()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(status.code(), Some(86), "child must terminate at {phase}");
    let before: Mission =
        serde_json::from_slice(&std::fs::read(path.join("before.json")).unwrap()).unwrap();
    let projects = ProjectsStore::open(path.join("projects.db")).unwrap();
    if phase.ends_with("before_commit") {
        assert_eq!(projects.live_leases(None).unwrap().len(), 1);
        assert!(projects
            .dispatch_admission(&before.id.to_string())
            .unwrap()
            .is_none());
    } else {
        let journal = projects
            .dispatch_admission(&before.id.to_string())
            .unwrap()
            .expect("committed provisional lease must have journal");
        assert_eq!(journal["acquired"].as_array().unwrap().len(), 1);
        assert_eq!(projects.live_leases(None).unwrap().len(), 2);
    }
    drop(projects);
    if phase.starts_with("dispatch_rejected") {
        let db =
            rusqlite::Connection::open(path.join("missions/missions-admission-test.db")).unwrap();
        db.execute_batch("DROP TRIGGER refuse_queue; DROP TRIGGER refuse_status;")
            .unwrap();
    }
    if phase.ends_with("_offline") {
        let offline = path.join(".sandboxed-sh/missions");
        std::fs::create_dir_all(&offline).unwrap();
        // The child exited without checkpointing SQLite. Move the WAL and
        // shared-memory sidecars with the database, preserving its durable state.
        for suffix in ["", "-wal", "-shm"] {
            let name = format!("missions-admission-test.db{suffix}");
            let source = path.join("missions").join(&name);
            if source.exists() {
                std::fs::rename(source, offline.join(name)).unwrap();
            }
        }
    }
    // New stores and actor, on the same durable databases, model restart.
    let restarted = Harness::with_directory(
        FixtureDir {
            path,
            _cleanup: Some(dir),
        },
        Vec::new(),
    )
    .await;
    crate::api::track_leases::sweep(&restarted.state)
        .await
        .unwrap();
    let store: Arc<dyn MissionStore> = if phase.ends_with("_offline") {
        Arc::new(
            SqliteMissionStore::new(
                restarted._dir.path().join(".sandboxed-sh/missions"),
                &restarted.user.id,
            )
            .await
            .unwrap(),
        )
    } else {
        restarted.control.mission_store.clone()
    };
    let after = store.get_mission(before.id).await.unwrap().unwrap();
    let leases = restarted.state.projects.live_leases(None).unwrap();
    assert_eq!(leases.len(), 1, "cleanup after {phase}");
    if phase.starts_with("project_edit_after_metadata") {
        assert_eq!(after.project.track.as_deref(), Some("new-track"));
        assert_eq!(leases[0].track, "new-track");
    } else {
        assert_eq!(after.project, before.project);
        assert_eq!(after.title, before.title);
        assert_eq!(leases[0].track, "trio-reserve1");
    }
    assert_status_metadata_eq(&before, &after);
    assert!(restarted
        .state
        .projects
        .dispatch_admission(&before.id.to_string())
        .unwrap()
        .is_none());
}

macro_rules! crash_recovery_test {
    ($name:ident, $phase:literal) => {
        #[tokio::test]
        async fn $name() {
            assert_crash_recovery($phase).await;
        }
    };
}
crash_recovery_test!(
    adversarial_crash_dispatch_before_commit,
    "dispatch_before_commit"
);
crash_recovery_test!(
    adversarial_crash_dispatch_after_commit,
    "dispatch_after_commit"
);
crash_recovery_test!(
    adversarial_crash_project_edit_before_commit,
    "project_edit_before_commit"
);
crash_recovery_test!(
    adversarial_crash_project_edit_after_commit,
    "project_edit_after_commit"
);
crash_recovery_test!(
    adversarial_crash_project_edit_after_metadata,
    "project_edit_after_metadata"
);
crash_recovery_test!(
    adversarial_crash_rejected_resume_restores_status_metadata,
    "dispatch_rejected"
);

crash_recovery_test!(
    adversarial_crash_project_edit_cleanup_without_live_session,
    "project_edit_after_metadata_offline"
);
crash_recovery_test!(
    adversarial_crash_rejected_resume_without_live_session,
    "dispatch_rejected_offline"
);

type OwnershipPageHooks = std::sync::Mutex<HashMap<Uuid, Box<dyn FnOnce() + Send>>>;
static OWNERSHIP_PAGE_HOOKS: std::sync::LazyLock<OwnershipPageHooks> =
    std::sync::LazyLock::new(Default::default);
pub(crate) fn after_ownership_page(missions: &[Mission]) {
    let callbacks: Vec<_> = {
        let mut hooks = OWNERSHIP_PAGE_HOOKS.lock().unwrap();
        missions
            .iter()
            .filter_map(|mission| hooks.remove(&mission.id))
            .collect()
    };
    for callback in callbacks {
        callback();
    }
}

#[tokio::test]
async fn adversarial_ownership_snapshot_cannot_omit_parked_writer_moving_across_pages() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let db_path = h._dir.path().join("missions/missions-admission-test.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "UPDATE missions SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
        [m.id.to_string()],
    )
    .unwrap();
    let mut marker = None;
    for _ in 0..205 {
        let mission = h
            .control
            .mission_store
            .create_mission(
                None,
                None,
                None,
                None,
                None,
                Some("test-no-execution"),
                None,
            )
            .await
            .unwrap();
        marker = Some(mission.id);
    }
    let moved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = moved.clone();
    OWNERSHIP_PAGE_HOOKS.lock().unwrap().insert(
        marker.unwrap(),
        Box::new(move || {
            // A concurrent status/history/metadata writer can move the old writer
            // to the front after the first read, without using admission locks.
            let db = rusqlite::Connection::open(db_path).unwrap();
            db.execute(
                "UPDATE missions SET updated_at='2099-01-01T00:00:00Z' WHERE id=?1",
                [m.id.to_string()],
            )
            .unwrap();
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    crate::api::track_leases::sweep(&h.state).await.unwrap();
    assert!(moved.load(std::sync::atomic::Ordering::SeqCst));
    let leases = h.state.projects.live_leases(None).unwrap();
    assert_eq!(
        leases.len(),
        1,
        "mutable pagination must never release the omitted writer"
    );
    assert_eq!(leases[0].attempt_id, m.id.to_string());
    let replacement =
        h.state
            .projects
            .acquire_track_lease(&crate::api::track_leases::lease_request(
                "lido",
                "trio-reserve1",
                "replacement",
                "writer",
                None,
            ));
    assert!(matches!(
        replacement,
        Err(crate::api::projects_store::LeaseError::Owned { .. })
    ));
}

type IdentityWriteHooks = std::sync::Mutex<HashMap<Uuid, Box<dyn FnOnce() + Send>>>;
static IDENTITY_WRITE_HOOKS: std::sync::LazyLock<IdentityWriteHooks> =
    std::sync::LazyLock::new(Default::default);
pub(crate) fn before_admission_identity_write(id: Uuid) {
    let callback = IDENTITY_WRITE_HOOKS.lock().unwrap().remove(&id);
    if let Some(callback) = callback {
        callback();
    }
}

#[tokio::test]
async fn adversarial_internal_tag_between_snapshot_and_identity_write_survives_acceptance() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, None).await;
    let path = h._dir.path().join("missions/missions-admission-test.db");
    IDENTITY_WRITE_HOOKS.lock().unwrap().insert(
        m.id,
        Box::new(move || {
            let db = rusqlite::Connection::open(path).unwrap();
            db.execute(
                "UPDATE missions SET tags=?2 WHERE id=?1",
                rusqlite::params![
                    m.id.to_string(),
                    r#"["pr-writer","orphaned","superseded","superseded_by:replacement"]"#
                ],
            )
            .unwrap();
        }),
    );
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
        let ControlCommand::AdmitDispatch { admission, command } = rx.recv().await.unwrap() else {
            panic!()
        };
        let command = admit_dispatch(*admission, *command, DISPATCH_ADMISSION.lock().await)
            .await
            .unwrap();
        let ControlCommand::UserMessage { respond, .. } = command else {
            panic!()
        };
        respond.send(UserMessageAck::Queued).unwrap();
    });
    let response = h.request(false, m.id, json!({"content":"different work", "track":"new-track", "github_pr":"", "title":"different work"})).await;
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    actor.await.unwrap();
    let after = h
        .control
        .mission_store
        .get_mission(m.id)
        .await
        .unwrap()
        .unwrap();
    for tag in [
        "pr-writer",
        "orphaned",
        "superseded",
        "superseded_by:replacement",
    ] {
        assert!(
            after.project.tags.iter().any(|value| value == tag),
            "lost {tag} during initial identity write"
        );
    }
}

#[tokio::test]
async fn startup_recovery_does_not_activate_new_work_while_waiting_for_admission() {
    let admission = DISPATCH_ADMISSION.lock().await;
    let h = Harness::new().await;
    let fresh = h
        .control
        .mission_store
        .create_mission(
            Some("fresh work"),
            None,
            None,
            None,
            None,
            Some("test-no-execution"),
            None,
        )
        .await
        .unwrap();
    h.control
        .mission_store
        .update_mission_status(fresh.id, MissionStatus::Active)
        .await
        .unwrap();
    let scanned = wait_for(fresh.id, "startup_scanned");
    drop(admission);
    tokio::time::timeout(std::time::Duration::from_secs(60), scanned)
        .await
        .unwrap()
        .unwrap();
    let after = h
        .control
        .mission_store
        .get_mission(fresh.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, MissionStatus::Active);
    assert!(after.terminal_reason.is_none());
    assert!(h
        .control
        .mission_store
        .get_active_mission_run(fresh.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn adversarial_pr_lookup_cannot_omit_parked_writer_moving_across_pages() {
    let h = Harness::new().await;
    let m = h.writer(MissionStatus::Paused, Some("repo#999")).await;
    let db_path = h._dir.path().join("missions/missions-admission-test.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "UPDATE missions SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
        [m.id.to_string()],
    )
    .unwrap();
    let mut marker = None;
    for _ in 0..205 {
        let mission = h
            .control
            .mission_store
            .create_mission(
                None,
                None,
                None,
                None,
                None,
                Some("test-no-execution"),
                None,
            )
            .await
            .unwrap();
        marker = Some(mission.id);
    }
    let moved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = moved.clone();
    OWNERSHIP_PAGE_HOOKS.lock().unwrap().insert(
        marker.unwrap(),
        Box::new(move || {
            // A concurrent status/history/metadata writer can move the old writer
            // to the front after the first read, without using admission locks.
            let db = rusqlite::Connection::open(db_path).unwrap();
            db.execute(
                "UPDATE missions SET updated_at='2099-01-01T00:00:00Z' WHERE id=?1",
                [m.id.to_string()],
            )
            .unwrap();
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    let found = find_existing_pr_writer(&h.control.mission_store, "repo#999", None)
        .await
        .unwrap();
    assert!(moved.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(found.map(|writer| writer.id), Some(m.id));
}

#[tokio::test]
async fn http_idle_message_continuation_captures_predecessor_before_delivery() {
    for (busy, ended, actor_ack) in [
        (false, true, UserMessageAck::Delivered),
        (false, true, UserMessageAck::Queued),
        (true, true, UserMessageAck::Queued),
        (false, false, UserMessageAck::Delivered),
        (false, true, UserMessageAck::Dropped),
        (
            false,
            true,
            UserMessageAck::Rejected("injected refusal".into()),
        ),
    ] {
        let h = Harness::new().await;
        let m = h.writer(MissionStatus::Active, Some("repo#244")).await;
        let store = h.control.mission_store.clone();
        let prior = store
            .begin_mission_run(m.id, "prior-actor", None)
            .await
            .unwrap();
        if ended {
            store
                .finish_mission_run(prior.run_id, prior.generation, Some("turn_complete"))
                .await
                .unwrap();
            store
                .update_mission_status(m.id, MissionStatus::AwaitingUser)
                .await
                .unwrap();
        }
        let (tx, mut rx) = mpsc::channel(1);
        h.state
            .control
            .sessions
            .write()
            .await
            .get_mut(&h.user.id)
            .unwrap()
            .cmd_tx = tx;
        let should_accept = matches!(
            actor_ack,
            UserMessageAck::Delivered | UserMessageAck::Queued
        );
        let should_continue = !busy && ended && should_accept;
        let expected_queued = actor_ack == UserMessageAck::Queued;
        let mission_id = m.id;
        let actor = tokio::spawn(async move {
            let ControlCommand::AdmitDispatch { admission, command } = rx.recv().await.unwrap()
            else {
                panic!()
            };
            let command = dispatch_admission::admit_dispatch_with_lifetime(
                *admission,
                *command,
                DISPATCH_ADMISSION.lock().await,
                busy,
                None,
            )
            .await
            .unwrap();
            if should_continue {
                // The next native run can already finish before HTTP receives
                // its acknowledgement. The reply must retain generation 1.
                store
                    .update_mission_status(mission_id, MissionStatus::Active)
                    .await
                    .unwrap();
                let successor = store
                    .begin_mission_run(mission_id, "next-actor", None)
                    .await
                    .unwrap();
                assert_eq!(successor.generation, 2);
                store
                    .finish_mission_run(
                        successor.run_id,
                        successor.generation,
                        Some("turn_complete"),
                    )
                    .await
                    .unwrap();
                store
                    .update_mission_status(mission_id, MissionStatus::AwaitingUser)
                    .await
                    .unwrap();
            }
            let ControlCommand::UserMessage { respond, .. } = command else {
                panic!()
            };
            respond.send(actor_ack).unwrap();
        });
        let response = h.request(false, m.id, json!({"content":"Continue the same work", "continue_identity":Harness::assertion(&m)})).await;
        actor.await.unwrap();
        if !should_accept {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            continue;
        }
        assert!(response.status().is_success());
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["mission_id"], m.id.to_string());
        assert_ne!(body["id"], m.id.to_string());
        assert_eq!(body["message_accepted"], true);
        assert_eq!(body["queued"], expected_queued);
        if should_continue {
            assert_eq!(
                body["previous_execution"],
                json!({"run_id":prior.run_id,"generation":prior.generation})
            );
        } else {
            assert!(body.get("previous_execution").is_none());
        }
    }
}
