//! HTTP route handlers.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use axum::middleware;
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        Json,
    },
    routing::{get, patch, post},
    Router,
};
use futures::stream::Stream;
use serde::Deserialize;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::agents::{AgentContext, AgentRef, OpenCodeAgent};
use crate::backend::registry::BackendRegistry;
use crate::backend_config::BackendConfigEntry;
use crate::config::{AuthMode, Config};
use crate::mcp::McpRegistry;
use crate::util::AI_PROVIDERS_PATH;
use crate::workspace;

/// Check whether a CLI binary is available on `$PATH`.
fn cli_available(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

use super::providers::ModelCatalog;

use super::ai_providers as ai_providers_api;
use super::ampcode as ampcode_api;
use super::auth::{self, AuthUser};
use super::backends as backends_api;
use super::claudecode as claudecode_api;
use super::console;
use super::control;
use super::deferred_proxy as deferred_proxy_api;
use super::desktop;
use super::desktop_stream;
use super::fs;
use super::library as library_api;
use super::mcp as mcp_api;
use super::model_routing as model_routing_api;
use super::monitoring;
use super::opencode as opencode_api;
use super::proxy as proxy_api;
use super::proxy_keys as proxy_keys_api;
use super::secrets as secrets_api;
use super::settings as settings_api;
use super::system as system_api;
use super::types::*;
use super::workspaces as workspaces_api;

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub tasks: RwLock<HashMap<String, HashMap<Uuid, TaskState>>>,
    /// The agent used for task execution
    pub root_agent: AgentRef,
    /// Global interactive control session
    pub control: control::ControlHub,
    /// MCP server registry
    pub mcp: Arc<McpRegistry>,
    /// Configuration library (git-based)
    pub library: library_api::SharedLibrary,
    /// Workspace store
    pub workspaces: workspace::SharedWorkspaceStore,
    /// OpenCode connection store
    pub opencode_connections: Arc<crate::opencode_config::OpenCodeStore>,
    /// Cached OpenCode agent list
    pub opencode_agents_cache: RwLock<opencode_api::OpenCodeAgentsCache>,
    /// AI Provider store
    pub ai_providers: Arc<crate::ai_providers::AIProviderStore>,
    /// Pending OAuth state for provider authorization
    pub pending_oauth:
        Arc<RwLock<HashMap<crate::ai_providers::ProviderType, crate::ai_providers::PendingOAuth>>>,
    /// Secrets store for encrypted credentials
    pub secrets: Option<Arc<crate::secrets::SecretsStore>>,
    /// Console session pool for WebSocket reconnection
    pub console_pool: Arc<console::SessionPool>,
    /// Global settings store
    pub settings: Arc<crate::settings::SettingsStore>,
    /// Backend registry for multi-backend support
    pub backend_registry: Arc<RwLock<BackendRegistry>>,
    /// Backend configuration store
    pub backend_configs: Arc<crate::backend_config::BackendConfigStore>,
    /// Cached model catalog fetched from provider APIs at startup
    pub model_catalog: ModelCatalog,
    /// Provider health tracker (per-account cooldown and stats)
    pub health_tracker: crate::provider_health::SharedProviderHealthTracker,
    /// Model chain store (fallback chain definitions)
    pub chain_store: crate::provider_health::SharedModelChainStore,
    /// Shared HTTP client for the proxy (connection pooling)
    pub http_client: reqwest::Client,
    /// Bearer token for the internal proxy endpoint
    pub proxy_secret: String,
    /// User-generated proxy API keys for external tools
    pub proxy_api_keys: super::proxy_keys::SharedProxyApiKeyStore,
    /// Deferred queue for proxy requests that opt into async-on-rate-limit mode
    pub deferred_requests: Arc<deferred_proxy_api::DeferredRequestStore>,
    /// Telegram bridge for assistant missions
    pub telegram_bridge: super::telegram::SharedTelegramBridge,
    /// FIDO signing relay hub (pending approval requests)
    pub fido_hub: Arc<super::fido::FidoSigningHub>,
}

/// Start the HTTP server.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let mut config = config;
    // Start monitoring background collector early so clients get history immediately
    monitoring::init_monitoring();

    // Initialize MCP registry
    let mcp = Arc::new(McpRegistry::new(&config.working_dir).await);
    if let Err(e) = crate::opencode_config::ensure_global_config(&mcp).await {
        tracing::warn!("Failed to ensure OpenCode global config: {}", e);
    }
    // Refresh all MCPs in background
    {
        let mcp_clone = Arc::clone(&mcp);
        tokio::spawn(async move {
            mcp_clone.refresh_all(true).await; // skip workspace MCPs at startup
        });
    }

    // Initialize workspace store (loads from disk and recovers orphaned containers)
    let workspaces = Arc::new(workspace::WorkspaceStore::new(config.working_dir.clone()).await);

    // Enable per-container metrics collection in the monitoring background task
    monitoring::init_monitoring_workspaces(Arc::clone(&workspaces)).await;

    // Initialize OpenCode connection store
    let opencode_connections = Arc::new(
        crate::opencode_config::OpenCodeStore::new(
            config
                .working_dir
                .join(".sandboxed-sh/opencode_connections.json"),
        )
        .await,
    );

    // Initialize AI provider store
    let ai_providers = Arc::new(
        crate::ai_providers::AIProviderStore::new(config.working_dir.join(AI_PROVIDERS_PATH)).await,
    );
    let pending_oauth = Arc::new(RwLock::new(HashMap::new()));

    // Initialize provider health tracker and model chain store
    let health_tracker = Arc::new(crate::provider_health::ProviderHealthTracker::new());
    let chain_store = Arc::new(
        crate::provider_health::ModelChainStore::new(
            config.working_dir.join(".sandboxed-sh/model_chains.json"),
        )
        .await,
    );

    // Initialize proxy API key store
    let proxy_api_keys = Arc::new(
        super::proxy_keys::ProxyApiKeyStore::new(
            config.working_dir.join(".sandboxed-sh/proxy_api_keys.json"),
        )
        .await,
    );
    let deferred_requests = Arc::new(
        deferred_proxy_api::DeferredRequestStore::new(
            config
                .working_dir
                .join(".sandboxed-sh/deferred_requests.json"),
        )
        .await,
    );

    // Initialize secrets store
    let secrets = match crate::secrets::SecretsStore::new(&config.working_dir).await {
        Ok(store) => {
            tracing::info!("Secrets store initialized");
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("Failed to initialize secrets store: {}", e);
            None
        }
    };

    // Initialize console session pool for WebSocket reconnection
    let console_pool = Arc::new(console::SessionPool::new());
    Arc::clone(&console_pool).start_cleanup_task();

    // Initialize global settings store
    let settings = Arc::new(crate::settings::SettingsStore::new(&config.working_dir).await);
    settings.init_cached_values();

    // Sweep orphaned command-task nspawn containers from a previous process lifetime.
    // Containers are named task-{uuid} via --machine=, so machinectl can find them.
    tokio::spawn(async {
        match tokio::process::Command::new("machinectl")
            .args(["list", "--no-legend", "--no-pager"])
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some(name) = line.split_whitespace().next() {
                        if name.starts_with("task-") {
                            tracing::warn!(machine = %name, "Terminating orphaned task container from previous process");
                            let _ = tokio::process::Command::new("machinectl")
                                .args(["terminate", name])
                                .output()
                                .await;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!("machinectl not available for orphan sweep: {}", e);
            }
        }
    });

    // Initialize backend config store (persisted settings).
    // Probe each CLI binary so backends whose CLI is missing default to disabled.
    // Persisted configs are preserved — this only affects fresh installs or new backends.
    let opencode_detected = cli_available("opencode");
    let claude_detected = cli_available("claude");
    let amp_detected = cli_available("amp");
    let codex_detected = cli_available("codex");
    let gemini_detected = cli_available("gemini");
    tracing::info!(
        opencode = opencode_detected,
        claude = claude_detected,
        amp = amp_detected,
        codex = codex_detected,
        gemini = gemini_detected,
        "CLI detection for backend defaults"
    );

    let backend_defaults = vec![
        {
            let mut entry = BackendConfigEntry::new(
                "opencode",
                "OpenCode",
                serde_json::json!({
                    "base_url": config.opencode_base_url,
                    "default_agent": config.opencode_agent,
                    "permissive": config.opencode_permissive,
                }),
            );
            entry.enabled = opencode_detected;
            entry
        },
        {
            let mut entry =
                BackendConfigEntry::new("claudecode", "Claude Code", serde_json::json!({}));
            entry.enabled = claude_detected;
            entry
        },
        {
            let mut entry = BackendConfigEntry::new("amp", "Amp", serde_json::json!({}));
            entry.enabled = amp_detected;
            entry
        },
        {
            let mut entry = BackendConfigEntry::new("codex", "Codex", serde_json::json!({}));
            entry.enabled = codex_detected;
            entry
        },
        {
            let mut entry = BackendConfigEntry::new("gemini", "Gemini CLI", serde_json::json!({}));
            entry.enabled = gemini_detected;
            entry
        },
    ];
    let backend_configs = Arc::new(
        crate::backend_config::BackendConfigStore::new(
            config.working_dir.join(".sandboxed-sh/backend_config.json"),
            backend_defaults,
        )
        .await,
    );

    // Apply persisted OpenCode settings (if present)
    if let Some(entry) = backend_configs.get("opencode").await {
        if let Some(settings) = entry.settings.as_object() {
            if let Some(base_url) = settings.get("base_url").and_then(|v| v.as_str()) {
                if !base_url.trim().is_empty() {
                    config.opencode_base_url = base_url.to_string();
                }
            }
            if let Some(agent) = settings.get("default_agent").and_then(|v| v.as_str()) {
                if !agent.trim().is_empty() {
                    config.opencode_agent = Some(agent.to_string());
                }
            }
            if let Some(permissive) = settings.get("permissive").and_then(|v| v.as_bool()) {
                config.opencode_permissive = permissive;
            }
        }
    }

    // Always use OpenCode backend
    let root_agent: AgentRef = Arc::new(OpenCodeAgent::new(config.clone()));

    // Initialize backend registry with OpenCode and Claude Code backends
    let opencode_base_url = config.opencode_base_url.clone();
    let opencode_default_agent = config.opencode_agent.clone();
    let opencode_permissive = config.opencode_permissive;

    // Determine default backend: env var, or first available with priority claudecode → opencode → amp → gemini → codex
    let default_backend = config.default_backend.clone().unwrap_or_else(|| {
        if claude_detected {
            "claudecode".to_string()
        } else if opencode_detected {
            "opencode".to_string()
        } else if amp_detected {
            "amp".to_string()
        } else if gemini_detected {
            "gemini".to_string()
        } else if codex_detected {
            "codex".to_string()
        } else {
            // Fallback to claudecode even if not detected (will show warning in UI)
            tracing::warn!(
                "No backend CLIs detected. Defaulting to claudecode. Please install at least one backend."
            );
            "claudecode".to_string()
        }
    });

    tracing::info!(
        "Default backend: {} (claudecode={}, opencode={}, amp={}, codex={}, gemini={})",
        default_backend,
        claude_detected,
        opencode_detected,
        amp_detected,
        codex_detected,
        gemini_detected
    );

    let mut backend_registry = BackendRegistry::new(default_backend);
    backend_registry.register(crate::backend::opencode::registry_entry(
        opencode_base_url.clone(),
        opencode_default_agent,
        opencode_permissive,
    ));
    backend_registry.register(crate::backend::claudecode::registry_entry());
    backend_registry.register(crate::backend::amp::registry_entry());
    backend_registry.register(crate::backend::codex::registry_entry());
    backend_registry.register(crate::backend::gemini::registry_entry());
    let backend_registry = Arc::new(RwLock::new(backend_registry));
    tracing::info!("Backend registry initialized with {} backends", 5);

    // Note: No central OpenCode server cleanup needed - missions use per-workspace CLI execution

    // Initialize configuration library (optional - can also be configured at runtime)
    // Must be created before ControlHub so it can be passed to control sessions
    let library: library_api::SharedLibrary = Arc::new(RwLock::new(None));
    // Read library_remote from settings (which falls back to env var if not configured)
    let library_remote = settings.get_library_remote().await;
    if let Some(library_remote) = library_remote {
        let library_clone = Arc::clone(&library);
        let library_path = config.library_path.clone();
        let workspaces_clone = Arc::clone(&workspaces);
        tokio::spawn(async move {
            match crate::library::LibraryStore::new(library_path, &library_remote).await {
                Ok(store) => {
                    if let Ok(plugins) = store.get_plugins().await {
                        if let Err(e) = crate::opencode_config::sync_global_plugins(&plugins).await
                        {
                            tracing::warn!("Failed to sync OpenCode plugins: {}", e);
                        }
                    }
                    tracing::info!("Configuration library initialized from {}", library_remote);
                    *library_clone.write().await = Some(Arc::new(store));

                    let workspaces = workspaces_clone.list().await;
                    if let Some(library) = library_clone.read().await.as_ref() {
                        for workspace in workspaces {
                            let is_default_host = workspace.id == workspace::DEFAULT_WORKSPACE_ID
                                && workspace.workspace_type == workspace::WorkspaceType::Host;
                            if is_default_host || !workspace.skills.is_empty() {
                                if let Err(e) =
                                    workspace::sync_workspace_skills(&workspace, library).await
                                {
                                    tracing::warn!(
                                        workspace = %workspace.name,
                                        error = %e,
                                        "Failed to sync skills after library init"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize configuration library: {}", e);
                }
            }
        });
    } else {
        tracing::info!("Configuration library disabled (no remote configured)");
    }

    // Create Telegram bridge (shared across all user sessions).
    let telegram_bridge = Arc::new(super::telegram::TelegramBridge::new());

    // Spawn the single global control session actor.
    let mut control_state = control::ControlHub::new(
        config.clone(),
        Arc::clone(&root_agent),
        Arc::clone(&mcp),
        Arc::clone(&workspaces),
        Arc::clone(&library),
        secrets.clone(),
    );
    control_state.set_telegram_bridge(Arc::clone(&telegram_bridge));

    let state = Arc::new(AppState {
        config: config.clone(),
        tasks: RwLock::new(HashMap::new()),
        root_agent,
        control: control_state,
        mcp,
        library,
        workspaces,
        opencode_connections,
        opencode_agents_cache: RwLock::new(opencode_api::OpenCodeAgentsCache::default()),
        ai_providers,
        pending_oauth,
        secrets,
        console_pool,
        settings,
        backend_registry,
        backend_configs,
        model_catalog: Arc::new(RwLock::new(HashMap::new())),
        health_tracker,
        chain_store,
        http_client: reqwest::Client::builder()
            // No global timeout — it applies to the full response body including
            // streaming chunks, which would kill long-running LLM generations.
            // Per-request timeouts are set in the proxy where needed.
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default(),
        proxy_secret: std::env::var("SANDBOXED_PROXY_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                let secret = uuid::Uuid::new_v4().to_string();
                tracing::info!("No SANDBOXED_PROXY_SECRET set; generated ephemeral proxy secret");
                // Also set in env so mission_runner can read it for OpenCode config.
                std::env::set_var("SANDBOXED_PROXY_SECRET", &secret);
                secret
            }),
        proxy_api_keys,
        deferred_requests,
        telegram_bridge,
        fido_hub: Arc::new(super::fido::FidoSigningHub::new()),
    });

    // Initialize the metadata LLM client for AI-powered mission titles/descriptions
    {
        super::metadata_llm::init_metadata_llm(state.http_client.clone());
        let ai_providers = Arc::clone(&state.ai_providers);
        tokio::spawn(async move {
            super::metadata_llm::refresh_metadata_llm_config(&ai_providers).await;
            // Store the AI providers reference for self-refresh (picks up new OAuth tokens)
            if let Some(client) = super::metadata_llm::metadata_llm() {
                client.set_ai_providers(ai_providers).await;
            }
        });
    }

    // Start background desktop session cleanup task
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            desktop::start_cleanup_task(state_clone).await;
        });
    }

    // Start background OAuth token refresher task
    {
        let ai_providers = Arc::clone(&state.ai_providers);
        let working_dir = config.working_dir.clone();
        tokio::spawn(async move {
            oauth_token_refresher_loop(ai_providers, working_dir).await;
        });
    }

    // Start deferred proxy queue worker.
    deferred_proxy_api::start_worker(Arc::clone(&state));

    // Eagerly boot the control session so Telegram webhooks are re-registered
    // immediately on server start (rather than waiting for the first
    // authenticated API call). Use the same implicit single-tenant identity
    // that the auth middleware would assign so Telegram channels and missions
    // are visible in the dashboard.
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let default_user = super::auth::implicit_single_tenant_user(&state_clone.config);
            let _ = state_clone.control.get_or_spawn(&default_user).await;
            tracing::info!("Eagerly booted default control session (Telegram webhooks registered)");
        });
    }

    // Fetch model catalog from provider APIs in background
    {
        let catalog = Arc::clone(&state.model_catalog);
        let ai_providers = Arc::clone(&state.ai_providers);
        let working_dir = config.working_dir.clone();
        tokio::spawn(async move {
            let fetched = super::providers::fetch_model_catalog(&ai_providers, &working_dir).await;
            let provider_count = fetched.len();
            let model_count: usize = fetched.values().map(|v| v.len()).sum();
            *catalog.write().await = fetched;
            tracing::info!(
                "Model catalog populated: {} models from {} providers",
                model_count,
                provider_count
            );
        });
    }

    let public_routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth::login))
        // Webhook receiver endpoint (no auth required - uses webhook secret validation)
        .route(
            "/api/webhooks/:mission_id/:webhook_id",
            post(control::webhook_receiver),
        )
        // Telegram webhook receiver (no auth - uses Telegram secret_token header validation)
        .route(
            "/api/telegram/webhook/:channel_id",
            post(control::telegram_webhook_receiver),
        )
        .route(
            "/api/control/telegram/actions/internal",
            post(control::execute_telegram_action_internal_api),
        )
        .route(
            "/api/control/telegram/workflows/request/internal",
            post(control::execute_telegram_workflow_request_internal_api),
        )
        // WebSocket console uses subprotocol-based auth (browser can't set Authorization header)
        .route("/api/console/ws", get(console::console_ws))
        // WebSocket workspace shell uses subprotocol-based auth
        .route(
            "/api/workspaces/:id/shell",
            get(console::workspace_shell_ws),
        )
        // WebSocket desktop stream uses subprotocol-based auth
        .route(
            "/api/desktop/stream",
            get(desktop_stream::desktop_stream_ws),
        )
        // WebSocket system monitoring uses subprotocol-based auth
        .route("/api/monitoring/ws", get(monitoring::monitoring_ws))
        // OpenAI-compatible proxy endpoint (bearer token auth via SANDBOXED_PROXY_SECRET).
        // LLM payloads with tool outputs and long contexts can exceed the default 2MB
        // body limit, so set a generous 50MB limit for proxy routes.
        .nest(
            "/v1",
            proxy_api::routes().layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        );

    // File upload routes with increased body limit (10GB)
    let upload_route = Router::new()
        .route("/api/fs/upload", post(fs::upload))
        .route("/api/fs/upload-chunk", post(fs::upload_chunk))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024 * 1024));

    let protected_routes = Router::new()
        .route("/api/stats", get(get_stats))
        .route("/api/task", post(create_task))
        .route("/api/task/:id", get(get_task))
        .route("/api/task/:id/stop", post(stop_task))
        .route("/api/task/:id/stream", get(stream_task))
        .route("/api/tasks", get(list_tasks))
        // FIDO signing relay endpoints
        .route("/api/fido/request", post(super::fido::post_fido_request))
        .route("/api/fido/respond", post(super::fido::post_fido_respond))
        // Global control session endpoints
        .route("/api/control/message", post(control::post_message))
        .route("/api/control/tool_result", post(control::post_tool_result))
        .route("/api/control/stream", get(control::stream))
        .route("/api/control/cancel", post(control::post_cancel))
        // Queue management endpoints
        .route("/api/control/queue", get(control::get_queue))
        .route(
            "/api/control/queue/:id",
            axum::routing::delete(control::remove_from_queue),
        )
        .route(
            "/api/control/queue",
            axum::routing::delete(control::clear_queue),
        )
        // State snapshots (for refresh resilience)
        .route("/api/control/tree", get(control::get_tree))
        .route("/api/control/progress", get(control::get_progress))
        // Diagnostic endpoints
        .route(
            "/api/control/diagnostics/opencode",
            get(control::get_opencode_diagnostics),
        )
        // Mission management endpoints
        .route("/api/control/missions", get(control::list_missions))
        .route("/api/control/missions", post(control::create_mission))
        .route(
            "/api/control/missions/search",
            get(control::search_missions),
        )
        .route(
            "/api/control/missions/search/moments",
            get(control::search_mission_moments),
        )
        .route(
            "/api/control/missions/current",
            get(control::get_current_mission),
        )
        .route("/api/control/missions/:id", get(control::get_mission))
        .route(
            "/api/control/missions/:id/tree",
            get(control::get_mission_tree),
        )
        .route(
            "/api/control/missions/:id/events",
            get(control::get_mission_events),
        )
        .route(
            "/api/control/missions/:id/load",
            post(control::load_mission),
        )
        .route(
            "/api/control/missions/:id/status",
            post(control::set_mission_status),
        )
        .route(
            "/api/control/missions/:id/title",
            post(control::set_mission_title),
        )
        .route(
            "/api/control/missions/:id/settings",
            patch(control::update_mission_settings),
        )
        .route(
            "/api/control/missions/:id/mode",
            post(control::set_mission_mode),
        )
        .route(
            "/api/control/missions/:id/cancel",
            post(control::cancel_mission),
        )
        .route(
            "/api/control/missions/:id/resume",
            post(control::resume_mission),
        )
        .route(
            "/api/control/missions/:id/parallel",
            post(control::start_mission_parallel),
        )
        .route(
            "/api/control/missions/:id",
            axum::routing::delete(control::delete_mission),
        )
        // Mission cleanup
        .route(
            "/api/control/missions/cleanup",
            post(control::cleanup_empty_missions),
        )
        // Automation endpoints
        .route(
            "/api/control/missions/:id/automations",
            get(control::list_mission_automations),
        )
        .route(
            "/api/control/missions/:id/automations",
            post(control::create_automation),
        )
        .route(
            "/api/control/automations",
            get(control::list_active_automations),
        )
        .route("/api/control/automations/:id", get(control::get_automation))
        .route(
            "/api/control/automations/:id",
            axum::routing::patch(control::update_automation),
        )
        .route(
            "/api/control/automations/:id",
            axum::routing::delete(control::delete_automation),
        )
        .route(
            "/api/control/automations/:id/executions",
            get(control::get_automation_executions),
        )
        .route(
            "/api/control/missions/:id/automation-executions",
            get(control::get_mission_automation_executions),
        )
        // Mission portability — export a mission for transfer to another
        // instance, and import one coming from elsewhere. The import route
        // gets its own body limit layer because mission bundles routinely
        // hit hundreds of MB (a long-running mission carries 50k+ tool
        // results), and the default axum limit (2 MB) would 413 them.
        .route(
            "/api/control/missions/:id/export",
            get(control::export_mission),
        )
        // Single-shot import buffers the entire body in memory (it
        // arrives as `axum::body::Bytes`). Keep the cap tight enough
        // that one request can't exhaust RAM — callers with larger
        // bundles should use the chunked `/import-chunks` flow, which
        // streams straight from disk.
        .route(
            "/api/control/missions/import",
            post(control::import_mission).layer(DefaultBodyLimit::max(128 * 1024 * 1024)),
        )
        // Chunked fallback for bundles that exceed Cloudflare's 100 MB
        // per-request cap even after gzip. Upload flow:
        //   1. POST /import-chunks → { upload_id }
        //   2. PUT  /import-chunks/:upload_id/:index (raw chunk body)
        //   3. POST /import-chunks/:upload_id/commit?total_chunks=N&gzip=...
        // Chunks stage under /tmp; commit assembles, decompresses, imports.
        .route(
            "/api/control/missions/import-chunks",
            post(control::init_mission_import),
        )
        .route(
            "/api/control/missions/import-chunks/:upload_id/:index",
            axum::routing::put(control::upload_mission_import_chunk)
                .layer(DefaultBodyLimit::max(128 * 1024 * 1024)),
        )
        .route(
            "/api/control/missions/import-chunks/:upload_id/commit",
            post(control::commit_mission_import),
        )
        .route(
            "/api/control/missions/import-chunks/:upload_id",
            axum::routing::delete(control::cancel_mission_import),
        )
        // Assistant missions
        .route(
            "/api/control/assistants",
            get(control::list_assistant_missions),
        )
        // Telegram channel endpoints
        .route(
            "/api/control/missions/:id/telegram-channels",
            get(control::list_telegram_channels),
        )
        .route(
            "/api/control/missions/:id/telegram-channels",
            post(control::create_telegram_channel),
        )
        .route(
            "/api/control/telegram-channels/:id",
            axum::routing::delete(control::delete_telegram_channel)
                .patch(control::update_telegram_channel),
        )
        .route(
            "/api/control/telegram-channels/:id/toggle",
            post(control::toggle_telegram_channel),
        )
        // Standalone Telegram bot endpoints (auto-create missions per chat)
        .route(
            "/api/control/telegram/bots",
            get(control::list_telegram_bots).post(control::create_telegram_bot),
        )
        .route(
            "/api/control/telegram/bots/:id/chats",
            get(control::list_bot_chats),
        )
        .route(
            "/api/control/telegram/bots/:id/scheduled",
            get(control::list_bot_scheduled_messages),
        )
        .route(
            "/api/control/telegram/bots/:id/actions",
            get(control::list_bot_action_executions),
        )
        .route(
            "/api/control/telegram/bots/:id/conversations",
            get(control::list_bot_conversations),
        )
        .route(
            "/api/control/telegram/bots/:id/workflows",
            get(control::list_bot_workflows),
        )
        .route(
            "/api/control/telegram/bots/:id/memory",
            get(control::list_bot_structured_memory),
        )
        .route(
            "/api/control/telegram/bots/:id/memory-search",
            get(control::search_bot_structured_memory),
        )
        .route(
            "/api/control/telegram/conversations/:id/messages",
            get(control::list_telegram_conversation_messages),
        )
        .route(
            "/api/control/telegram/workflows/:id/events",
            get(control::list_telegram_workflow_events),
        )
        .route(
            "/api/control/telegram/send",
            post(control::send_telegram_message_api),
        )
        .route(
            "/api/control/telegram/actions",
            post(control::execute_telegram_action_api),
        )
        .route(
            "/api/control/telegram/workflows/request",
            post(control::execute_telegram_workflow_request_api),
        )
        // Parallel execution endpoints
        .route("/api/control/running", get(control::list_running_missions))
        .route(
            "/api/control/parallel/config",
            get(control::get_parallel_config),
        )
        // Memory endpoints
        .route("/api/runs", get(list_runs))
        .route("/api/runs/:id", get(get_run))
        .route("/api/runs/:id/events", get(get_run_events))
        .route("/api/runs/:id/tasks", get(get_run_tasks))
        .route("/api/memory/search", get(search_memory))
        // Remote file explorer endpoints (use Authorization header)
        .route("/api/fs/list", get(fs::list))
        .route("/api/fs/download", get(fs::download))
        .route("/api/fs/validate", get(fs::validate))
        .merge(upload_route)
        .route("/api/fs/upload-finalize", post(fs::upload_finalize))
        .route("/api/fs/download-url", post(fs::download_from_url))
        .route("/api/fs/mkdir", post(fs::mkdir))
        .route("/api/fs/rm", post(fs::rm))
        // MCP management endpoints
        .route("/api/mcp", get(mcp_api::list_mcps))
        .route("/api/mcp", post(mcp_api::add_mcp))
        .route("/api/mcp/refresh", post(mcp_api::refresh_all_mcps))
        .route("/api/mcp/:id", get(mcp_api::get_mcp))
        .route("/api/mcp/:id", axum::routing::delete(mcp_api::remove_mcp))
        .route("/api/mcp/:id", axum::routing::patch(mcp_api::update_mcp))
        .route("/api/mcp/:id/enable", post(mcp_api::enable_mcp))
        .route("/api/mcp/:id/disable", post(mcp_api::disable_mcp))
        .route("/api/mcp/:id/refresh", post(mcp_api::refresh_mcp))
        // Tools management endpoints
        .route("/api/tools", get(mcp_api::list_tools))
        .route("/api/tools/:name/toggle", post(mcp_api::toggle_tool))
        // Provider management endpoints
        .route("/api/providers", get(super::providers::list_providers))
        .route(
            "/api/providers/backend-models",
            get(super::providers::list_backend_model_options),
        )
        // Library management endpoints
        .nest("/api/library", library_api::routes())
        // Workspace management endpoints
        .nest("/api/workspaces", workspaces_api::routes())
        // OpenCode connection endpoints
        .nest("/api/opencode/connections", opencode_api::routes())
        .route("/api/opencode/agents", get(opencode_api::list_agents))
        // OpenCode settings (oh-my-opencode.json)
        .route(
            "/api/opencode/settings",
            get(opencode_api::get_opencode_settings),
        )
        .route(
            "/api/opencode/settings",
            axum::routing::put(opencode_api::update_opencode_settings),
        )
        .route(
            "/api/opencode/config",
            get(opencode_api::get_opencode_config),
        )
        .route(
            "/api/opencode/config",
            axum::routing::put(opencode_api::update_opencode_config),
        )
        .route(
            "/api/claudecode/config",
            get(claudecode_api::get_claudecode_config),
        )
        .route(
            "/api/claudecode/config",
            axum::routing::put(claudecode_api::update_claudecode_config),
        )
        .route("/api/amp/config", get(ampcode_api::get_amp_config))
        .route(
            "/api/amp/config",
            axum::routing::put(ampcode_api::update_amp_config),
        )
        .route(
            "/api/opencode/restart",
            post(opencode_api::restart_opencode_service),
        )
        // AI Provider endpoints
        .nest("/api/ai/providers", ai_providers_api::routes())
        // Model routing (chains + health)
        .nest("/api/model-routing", model_routing_api::routes())
        // Proxy API key management
        .nest("/api/proxy-keys", proxy_keys_api::routes())
        // Secrets management endpoints
        .nest("/api/secrets", secrets_api::routes())
        // Global settings endpoints
        .nest("/api/settings", settings_api::routes())
        // Desktop session management endpoints
        .nest("/api/desktop", desktop::routes())
        // System component management endpoints
        .nest("/api/system", system_api::routes())
        // Auth management endpoints
        .route("/api/auth/status", get(auth::auth_status))
        .route("/api/auth/change-password", post(auth::change_password))
        // Backend management endpoints
        .route("/api/backends", get(backends_api::list_backends))
        .route("/api/backends/:id", get(backends_api::get_backend))
        .route(
            "/api/backends/:id/agents",
            get(backends_api::list_backend_agents),
        )
        .route(
            "/api/backends/:id/config",
            get(backends_api::get_backend_config),
        )
        .route(
            "/api/backends/:id/config",
            axum::routing::put(backends_api::update_backend_config),
        )
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_auth,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::clone(&state));

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Server listening on {}", addr);

    // Setup graceful shutdown on SIGTERM/SIGINT
    let shutdown_state = Arc::clone(&state);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal(shutdown_state).await;
        })
        .await?;

    Ok(())
}

/// Wait for shutdown signal and mark running missions as interrupted.
async fn shutdown_signal(state: Arc<AppState>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
        "SIGINT"
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
        "SIGTERM"
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<&'static str>();

    let signal = tokio::select! {
        signal = ctrl_c => signal,
        signal = terminate => signal,
    };

    let exe = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let invocation_id = std::env::var("INVOCATION_ID").ok();
    tracing::warn!(
        signal,
        pid = std::process::id(),
        ppid = shutdown_parent_pid(),
        exe = %exe,
        cmdline = ?shutdown_cmdline(),
        invocation_id = ?invocation_id,
        "Shutdown signal received; marking running missions as interrupted"
    );

    // Send graceful shutdown command to all control sessions
    let sessions = state.control.all_sessions().await;
    tracing::info!(
        signal,
        control_sessions = sessions.len(),
        "Dispatching graceful shutdown to control sessions"
    );
    if sessions.is_empty() {
        tracing::info!("No active control sessions to shut down");
        return;
    }

    // Grab a mission store reference before consuming sessions.
    let mission_store = sessions.first().map(|cs| cs.mission_store.clone());

    let mut all_interrupted: Vec<Uuid> = Vec::new();
    for control in sessions {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Err(e) = control
            .cmd_tx
            .send(control::ControlCommand::GracefulShutdown { respond: tx })
            .await
        {
            tracing::error!("Failed to send shutdown command: {}", e);
            continue;
        }

        match rx.await {
            Ok(mut interrupted_ids) => {
                all_interrupted.append(&mut interrupted_ids);
            }
            Err(e) => {
                tracing::error!("Failed to receive shutdown response: {}", e);
            }
        }
    }

    if all_interrupted.is_empty() {
        tracing::info!("No running missions to interrupt");
    } else {
        tracing::warn!(
            "SHUTDOWN: Interrupted {} active mission(s):",
            all_interrupted.len(),
        );
        // Log details for each interrupted mission so operators can resume them.
        if let Some(store) = mission_store.as_ref() {
            for mid in &all_interrupted {
                let title = store
                    .get_mission(*mid)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|m| m.title)
                    .unwrap_or_else(|| "<untitled>".to_string());
                tracing::warn!("  SHUTDOWN: mission {} - \"{}\"", mid, title,);
            }
        }
        // Log a single copy-pasteable line for easy resume.
        let ids: Vec<String> = all_interrupted.iter().map(|id| id.to_string()).collect();
        tracing::warn!(
            "SHUTDOWN: To resume, reset these mission IDs: {}",
            ids.join(" "),
        );
    }

    tracing::info!("Graceful shutdown complete");
}

#[cfg(target_os = "linux")]
fn shutdown_parent_pid() -> Option<u32> {
    // /proc/self/stat has the executable name in parentheses and the ppid as
    // the fourth field. Split after the closing parenthesis so names containing
    // spaces do not shift the field positions.
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn shutdown_parent_pid() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn shutdown_cmdline() -> Option<String> {
    let bytes = std::fs::read("/proc/self/cmdline").ok()?;
    let cmdline = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part))
        .collect::<Vec<_>>()
        .join(" ");
    if cmdline.is_empty() {
        None
    } else if cmdline.len() > 300 {
        Some(format!(
            "{}...",
            cmdline.chars().take(300).collect::<String>()
        ))
    } else {
        Some(cmdline)
    }
}

#[cfg(not(target_os = "linux"))]
fn shutdown_cmdline() -> Option<String> {
    None
}

/// Health check endpoint.
async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let auth_mode = match state.config.auth.auth_mode(state.config.dev_mode) {
        AuthMode::Disabled => "disabled",
        AuthMode::SingleTenant => "single_tenant",
        AuthMode::MultiUser => "multi_user",
    };
    // Read library_remote from settings store (persisted to disk)
    let library_remote = state.settings.get_library_remote().await;
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        dev_mode: state.config.dev_mode,
        auth_required: state.config.auth.auth_required(state.config.dev_mode),
        auth_mode: auth_mode.to_string(),
        max_iterations: state.config.max_iterations,
        library_remote,
    })
}

/// Optional query parameters for the stats endpoint.
#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    /// ISO-8601 lower bound for cost aggregation (e.g. "2026-02-15T00:00:00Z").
    /// When omitted the endpoint returns all-time totals.
    since: Option<String>,
}

/// Get system statistics.
async fn get_stats(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<StatsQuery>,
) -> Json<StatsResponse> {
    // Legacy tasks
    let tasks = state.tasks.read().await;
    let user_tasks = tasks.get(&user.id);

    let legacy_total = user_tasks.map(|t| t.len()).unwrap_or(0);
    let legacy_active = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Running)
                .count()
        })
        .unwrap_or(0);
    let legacy_completed = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Completed)
                .count()
        })
        .unwrap_or(0);
    let legacy_failed = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Failed)
                .count()
        })
        .unwrap_or(0);
    drop(tasks);

    // Get mission stats from mission store
    let control_state = state.control.get_or_spawn(&user).await;

    // Count missions by status
    let missions = control_state
        .mission_store
        .list_missions(1000, 0)
        .await
        .unwrap_or_default();
    let mission_total = missions.len();
    let mission_active = missions
        .iter()
        .filter(|m| m.status == super::control::MissionStatus::Active)
        .count();
    let mission_completed = missions
        .iter()
        .filter(|m| m.status == super::control::MissionStatus::Completed)
        .count();
    let mission_failed = missions
        .iter()
        .filter(|m| m.status == super::control::MissionStatus::Failed)
        .count();

    // Combine legacy tasks and missions
    let total_tasks = legacy_total + mission_total;
    let active_tasks = legacy_active + mission_active;
    let completed_tasks = legacy_completed + mission_completed;
    let failed_tasks = legacy_failed + mission_failed;

    // Get cost totals, optionally filtered by a time-range lower bound.
    let (total_cost_cents, actual_cost_cents, estimated_cost_cents, unknown_cost_cents) =
        if let Some(ref since) = params.since {
            let total = control_state
                .mission_store
                .get_total_cost_cents_since(since)
                .await
                .unwrap_or(0);
            let (a, e, u) = control_state
                .mission_store
                .get_cost_by_source_since(since)
                .await
                .unwrap_or((0, 0, 0));
            (total, a, e, u)
        } else {
            let total = control_state
                .mission_store
                .get_total_cost_cents()
                .await
                .unwrap_or(0);
            let (a, e, u) = control_state
                .mission_store
                .get_cost_by_source()
                .await
                .unwrap_or((0, 0, 0));
            (total, a, e, u)
        };

    let finished = completed_tasks + failed_tasks;
    let success_rate = if finished > 0 {
        completed_tasks as f64 / finished as f64
    } else {
        1.0
    };

    Json(StatsResponse {
        total_tasks,
        active_tasks,
        completed_tasks,
        failed_tasks,
        total_cost_cents,
        actual_cost_cents,
        estimated_cost_cents,
        unknown_cost_cents,
        success_rate,
    })
}

/// List all tasks.
async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let tasks = state.tasks.read().await;
    let mut task_list: Vec<(Uuid, serde_json::Value)> = tasks
        .get(&user.id)
        .map(|t| {
            t.iter()
                .filter_map(|(id, ts)| match serde_json::to_value(ts) {
                    Ok(v) => Some((*id, v)),
                    Err(e) => {
                        tracing::error!("Failed to serialize task {}: {}", id, e);
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // Sort by most recent first (by ID since UUIDs are time-ordered)
    task_list.sort_by_key(|(id, _)| Reverse(*id));
    let values: Vec<_> = task_list.into_iter().map(|(_, v)| v).collect();
    Json(serde_json::Value::Array(values))
}

/// Stop a running task.
async fn stop_task(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut tasks = state.tasks.write().await;
    let user_tasks = tasks.entry(user.id).or_default();

    if let Some(task) = user_tasks.get_mut(&id) {
        if task.status == TaskStatus::Running {
            // For command-mode tasks, fire the cancel channel so run_command_task
            // can abort the child process cleanly via tokio::select!.
            if let Some(tx) = task.cancel_tx.take() {
                let _ = tx.send(());
            }
            task.status = TaskStatus::Cancelled;
            task.result = Some("Task was cancelled by user".to_string());
            Ok(Json(serde_json::json!({
                "success": true,
                "message": "Task cancelled"
            })))
        } else {
            Err((
                StatusCode::BAD_REQUEST,
                format!("Task {} is not running (status: {:?})", id, task.status),
            ))
        }
    } else {
        Err((StatusCode::NOT_FOUND, format!("Task {} not found", id)))
    }
}

/// Maximum log entries per task. Prevents unbounded memory growth from verbose scripts.
const LOG_MAX_ENTRIES: usize = 10_000;

/// Maximum completed tasks to retain per user. Oldest are evicted first on completion.
const MAX_COMPLETED_TASKS: usize = 500;

/// Append a log entry to a task. Shared by agent-mode and command-mode paths.
/// Silently drops entries beyond LOG_MAX_ENTRIES (with a sentinel at the cap).
async fn append_log(
    state: &Arc<AppState>,
    user_id: &str,
    task_id: Uuid,
    entry_type: LogEntryType,
    content: &str,
) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mut tasks = state.tasks.write().await;
    if let Some(user_tasks) = tasks.get_mut(user_id) {
        if let Some(task_state) = user_tasks.get_mut(&task_id) {
            let len = task_state.log.len();
            if len < LOG_MAX_ENTRIES {
                task_state.log.push(TaskLogEntry {
                    timestamp,
                    entry_type,
                    content: content.to_string(),
                });
            } else if len == LOG_MAX_ENTRIES {
                task_state.log.push(TaskLogEntry {
                    timestamp,
                    entry_type: LogEntryType::Error,
                    content: "[log truncated — 10,000 line limit reached]".to_string(),
                });
            }
            // Beyond the cap: drop silently
        }
    }
}

/// Run a shell command as a background task inside a workspace container.
#[allow(clippy::too_many_arguments)]
async fn run_command_task(
    state: Arc<AppState>,
    user_id: String,
    task_id: Uuid,
    command: String,
    workspace: crate::workspace::Workspace,
    working_dir: Option<String>,
    timeout: Option<std::time::Duration>,
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Check container readiness
    if workspace.workspace_type == crate::workspace::WorkspaceType::Container
        && workspace.status != crate::workspace::WorkspaceStatus::Ready
    {
        let msg = format!(
            "Workspace '{}' is not ready (status: {:?}). Build it first.",
            workspace.name, workspace.status
        );
        append_log(&state, &user_id, task_id, LogEntryType::Error, &msg).await;
        let mut tasks = state.tasks.write().await;
        if let Some(ut) = tasks.get_mut(&user_id) {
            if let Some(ts) = ut.get_mut(&task_id) {
                ts.status = TaskStatus::Failed;
                ts.result = Some(msg);
            }
        }
        return;
    }

    // Transition to Running
    let task_start = chrono::Utc::now();
    {
        let mut tasks = state.tasks.write().await;
        if let Some(ut) = tasks.get_mut(&user_id) {
            if let Some(ts) = ut.get_mut(&task_id) {
                ts.status = TaskStatus::Running;
                ts.started_at = Some(task_start.to_rfc3339());
            }
        }
    }
    tracing::info!(
        task_id = %task_id,
        workspace = %workspace.name,
        "Command task started"
    );

    // Claude auth for command tasks:
    //
    // Missions run claude on the HOST — it reads ~/.claude/.credentials.json on every API call
    // and always sees the current token, even after the 15-min OAuth refresher rotates it.
    //
    // Command tasks run inside nspawn. Bind-mount the host credentials file read-only so
    // claude CLI reads it on every call. The refresher rewrites the file in-place (same
    // inode via std::fs::write), so the bind-ro always reflects the current token.
    let (program, mut args) =
        super::workspaces::build_nspawn_command(&workspace, &command, None, working_dir.as_deref());

    // Give the nspawn process a unique machine name so orphaned processes (e.g. after
    // a sandboxed restart mid-task) are identifiable by name and can be terminated:
    //   machinectl terminate task-<uuid>
    // Also insert the credentials bind-ro before the shell invocation.
    // We insert before the last 3 args ("/bin/bash", "-c", command) to stay robust
    // against future changes to the nspawn option list built by build_nspawn_command.
    let insert_pos = args.len().saturating_sub(3);
    args.insert(insert_pos, format!("--machine=task-{}", task_id));

    let host_creds = std::path::Path::new("/root/.claude/.credentials.json");
    if host_creds.exists() {
        let container_creds_dir = workspace.path.join("root/.claude");
        if !container_creds_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&container_creds_dir) {
                tracing::warn!(
                    "Failed to create container .claude dir: {} — claude -p may fail",
                    e
                );
            }
        }
        // Insert before the machine name (order doesn't matter to nspawn)
        args.insert(
            insert_pos,
            "--bind-ro=/root/.claude/.credentials.json:/root/.claude/.credentials.json".to_string(),
        );
    } else {
        tracing::warn!(
            "No Claude credentials file at /root/.claude/.credentials.json — claude -p will fail"
        );
    }

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to spawn command: {}", e);
            append_log(&state, &user_id, task_id, LogEntryType::Error, &msg).await;
            let task_end = chrono::Utc::now();
            let mut tasks = state.tasks.write().await;
            if let Some(ut) = tasks.get_mut(&user_id) {
                if let Some(ts) = ut.get_mut(&task_id) {
                    ts.status = TaskStatus::Failed;
                    ts.result = Some(msg);
                    ts.completed_at = Some(task_end.to_rfc3339());
                    ts.duration_secs =
                        Some((task_end - task_start).num_milliseconds() as f64 / 1000.0);
                }
            }
            return;
        }
    };

    // Stream stdout line-by-line; parse JSON step annotations.
    // Readers are kept as joinable handles so we can drain them after the child exits/is killed,
    // preventing post-cancel log entries from appearing after the terminal status is written.
    let stdout_handle: Option<tokio::task::JoinHandle<()>> = if let Some(stdout) =
        child.stdout.take()
    {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let state_clone = Arc::clone(&state);
        let user_id_clone = user_id.clone();
        Some(tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                // Try to parse JSON step annotations; emit readable summary and skip raw line.
                if line.trim_start().starts_with('{') {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                        if v.get("step").is_some() {
                            let now = chrono::Utc::now().to_rfc3339();
                            let status = v["status"].as_str().unwrap_or("unknown").to_string();
                            let step = TaskStep {
                                name: v["step"].as_str().unwrap_or("").to_string(),
                                iteration: v
                                    .get("iteration")
                                    .and_then(|x| x.as_u64())
                                    .map(|x| x as u32),
                                status: status.clone(),
                                started_at: if status == "started" {
                                    Some(now.clone())
                                } else {
                                    None
                                },
                                completed_at: if status != "started" { Some(now) } else { None },
                                duration_s: v.get("duration_s").and_then(|x| x.as_f64()),
                                metadata: v.get("metadata").cloned(),
                            };
                            {
                                let mut tasks = state_clone.tasks.write().await;
                                if let Some(ut) = tasks.get_mut(&user_id_clone) {
                                    if let Some(ts) = ut.get_mut(&task_id) {
                                        ts.steps.push(step);
                                    }
                                }
                            }
                            // Emit a readable one-liner instead of the raw JSON blob
                            let label = format!(
                                "[step] {} {}{}",
                                v["step"].as_str().unwrap_or("?"),
                                v["status"].as_str().unwrap_or("?"),
                                v.get("iteration")
                                    .and_then(|x| x.as_u64())
                                    .map(|i| format!(" (iter {})", i))
                                    .unwrap_or_default(),
                            );
                            append_log(
                                &state_clone,
                                &user_id_clone,
                                task_id,
                                LogEntryType::Response,
                                &label,
                            )
                            .await;
                            continue; // skip plain-line branch
                        }
                    }
                }
                // Plain stdout line — emit as-is
                append_log(
                    &state_clone,
                    &user_id_clone,
                    task_id,
                    LogEntryType::Response,
                    &line,
                )
                .await;
            }
        }))
    } else {
        None
    };

    // Stream stderr to log as errors
    let stderr_handle: Option<tokio::task::JoinHandle<()>> =
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            let state_clone = Arc::clone(&state);
            let user_id_clone = user_id.clone();
            Some(tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    append_log(
                        &state_clone,
                        &user_id_clone,
                        task_id,
                        LogEntryType::Error,
                        &line,
                    )
                    .await;
                }
            }))
        } else {
            None
        };

    // Wait for child exit, optional timeout, or cancel signal.
    // Duration::MAX (~584 years) serves as "no timeout" — avoids duplicate select! blocks.
    let effective_timeout = timeout.unwrap_or(std::time::Duration::MAX);
    let exit_result: Result<std::process::ExitStatus, String> = tokio::select! {
        r = tokio::time::timeout(effective_timeout, child.wait()) => {
            match r {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => {
                    let _ = child.kill().await;
                    Err(format!("Timed out after {}s", effective_timeout.as_secs()))
                }
            }
        }
        _ = cancel_rx => {
            let _ = child.kill().await;
            Err("Cancelled".to_string())
        }
    };

    // Drain reader tasks before writing terminal status.
    // After kill/exit the pipe closes and readers exit naturally; 2-second guard covers
    // the edge case of a grandchild process that inherited the pipe descriptor.
    let drain = std::time::Duration::from_secs(2);
    if let Some(h) = stdout_handle {
        let _ = tokio::time::timeout(drain, h).await;
    }
    if let Some(h) = stderr_handle {
        let _ = tokio::time::timeout(drain, h).await;
    }

    // Update final task status
    let task_end = chrono::Utc::now();
    let duration_secs = (task_end - task_start).num_milliseconds() as f64 / 1000.0;

    let final_status;
    let mut tasks = state.tasks.write().await;
    if let Some(ut) = tasks.get_mut(&user_id) {
        if let Some(ts) = ut.get_mut(&task_id) {
            // Don't overwrite if already marked Cancelled by stop_task.
            // The Err("Cancelled") case is handled by the outer guard — stop_task
            // sets status=Cancelled before firing the channel, so by the time we
            // reach here the condition is already false.
            if ts.status != TaskStatus::Cancelled {
                match exit_result {
                    Ok(status) if status.success() => {
                        ts.status = TaskStatus::Completed;
                        ts.result = Some("exit 0".to_string());
                    }
                    Ok(status) => {
                        ts.status = TaskStatus::Failed;
                        ts.result = Some(format!("exit {}", status.code().unwrap_or(-1)));
                    }
                    Err(msg) => {
                        ts.status = TaskStatus::Failed;
                        ts.result = Some(msg);
                    }
                }
            }
            ts.completed_at = Some(task_end.to_rfc3339());
            ts.duration_secs = Some(duration_secs);
            final_status = ts.status.clone();
        } else {
            final_status = TaskStatus::Failed;
        }

        // Evict oldest completed tasks if over the retention cap (single pass).
        let mut completed: Vec<(Uuid, String)> = ut
            .iter()
            .filter(|(_, t)| !matches!(t.status, TaskStatus::Running | TaskStatus::Pending))
            .map(|(id, t)| (*id, t.completed_at.clone().unwrap_or_default()))
            .collect();
        if completed.len() > MAX_COMPLETED_TASKS {
            // Sort oldest-first by completed_at (ISO 8601 sorts lexicographically).
            completed.sort_unstable_by(|a, b| a.1.cmp(&b.1));
            let to_remove = completed.len() - MAX_COMPLETED_TASKS;
            for (evict_id, _) in completed.into_iter().take(to_remove) {
                ut.remove(&evict_id);
            }
            tracing::debug!(
                user_id = %user_id,
                evicted = to_remove,
                cap = MAX_COMPLETED_TASKS,
                "Evicted oldest completed tasks"
            );
        }
    } else {
        final_status = TaskStatus::Failed;
    }

    tracing::info!(
        task_id = %task_id,
        status = ?final_status,
        duration_secs = duration_secs,
        "Command task finished"
    );
}

/// Create a new task.
async fn create_task(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<CreateTaskResponse>, (StatusCode, String)> {
    let id = Uuid::new_v4();

    // --- Command mode ---
    if let Some(command) = req.command.clone() {
        let workspace_id = req.workspace_id.ok_or((
            StatusCode::BAD_REQUEST,
            "workspace_id is required when command is set".to_string(),
        ))?;

        let workspace = state.workspaces.get(workspace_id).await.ok_or((
            StatusCode::NOT_FOUND,
            format!("Workspace {} not found", workspace_id),
        ))?;

        // Command mode runs inside nspawn containers. Host workspaces execute directly
        // on the host as root — too broad a security surface for arbitrary commands.
        if workspace.workspace_type == crate::workspace::WorkspaceType::Host {
            return Err((
                StatusCode::BAD_REQUEST,
                "Command-mode tasks are not supported for Host workspaces".to_string(),
            ));
        }

        let timeout = match req.timeout_secs {
            Some(s) if s > 0 => Some(std::time::Duration::from_secs(s)),
            _ => Some(std::time::Duration::from_secs(1800)), // 30 min default
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        let task_state = TaskState {
            id,
            status: TaskStatus::Pending,
            task: req.task.clone(),
            mode: crate::api::types::TaskMode::Command,
            model: String::new(),
            iterations: 0,
            workspace_id: Some(workspace_id),
            workspace_name: Some(workspace.name.clone()),
            result: None,
            log: Vec::new(),
            steps: Vec::new(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            started_at: None,
            completed_at: None,
            duration_secs: None,
            cancel_tx: Some(cancel_tx),
        };

        // Check concurrent limit and insert atomically under one write lock
        // to prevent TOCTOU races where multiple requests pass the check simultaneously.
        {
            let max_concurrent =
                crate::settings::max_concurrent_tasks_cached_or(state.config.max_concurrent_tasks);
            let mut tasks = state.tasks.write().await;
            let user_tasks = tasks.entry(user.id.clone()).or_default();
            let running = user_tasks
                .values()
                .filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::Pending))
                .count();
            if running >= max_concurrent {
                tracing::warn!(
                    user_id = %user.id,
                    running = running,
                    limit = max_concurrent,
                    "Command task rejected: concurrent limit reached"
                );
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Too many concurrent tasks ({}/{}). Stop a running task or wait for one to finish.",
                        running, max_concurrent
                    ),
                ));
            }
            user_tasks.insert(id, task_state);
        }

        let state_clone = Arc::clone(&state);
        let working_dir = req.working_dir.clone();
        tokio::spawn(async move {
            run_command_task(
                state_clone,
                user.id,
                id,
                command,
                workspace,
                working_dir,
                timeout,
                cancel_rx,
            )
            .await;
        });

        return Ok(Json(CreateTaskResponse {
            id,
            status: TaskStatus::Pending,
        }));
    }

    // --- Agent mode (existing behaviour) ---
    let model = req
        .model
        .or(state.config.default_model.clone())
        .unwrap_or_default();

    let task_state = TaskState {
        id,
        status: TaskStatus::Pending,
        task: req.task.clone(),
        mode: crate::api::types::TaskMode::Agent,
        model: model.clone(),
        iterations: 0,
        workspace_id: None,
        workspace_name: None,
        result: None,
        log: Vec::new(),
        steps: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        started_at: None,
        completed_at: None,
        duration_secs: None,
        cancel_tx: None,
    };

    {
        let mut tasks = state.tasks.write().await;
        tasks
            .entry(user.id.clone())
            .or_default()
            .insert(id, task_state);
    }

    let state_clone = Arc::clone(&state);
    let task_description = req.task.clone();
    let budget_cents = req.budget_cents;
    let working_dir = req.working_dir.map(std::path::PathBuf::from);

    tokio::spawn(async move {
        run_agent_task(
            state_clone,
            user.id,
            id,
            task_description,
            model,
            budget_cents,
            working_dir,
            None,
        )
        .await;
    });

    Ok(Json(CreateTaskResponse {
        id,
        status: TaskStatus::Pending,
    }))
}

/// Run the agent for a task (background).
#[allow(clippy::too_many_arguments)]
async fn run_agent_task(
    state: Arc<AppState>,
    user_id: String,
    task_id: Uuid,
    task_description: String,
    requested_model: String,
    budget_cents: Option<u64>,
    working_dir: Option<std::path::PathBuf>,
    agent_override: Option<String>,
) {
    // Update status to running
    {
        let mut tasks = state.tasks.write().await;
        if let Some(user_tasks) = tasks.get_mut(&user_id) {
            if let Some(task_state) = user_tasks.get_mut(&task_id) {
                task_state.status = TaskStatus::Running;
            }
        }
    }

    // Create a Task object for the OpenCode agent
    let task_result = crate::task::Task::new(task_description.clone(), budget_cents.or(Some(1000)));

    let mut task = match task_result {
        Ok(t) => t,
        Err(e) => {
            let mut tasks = state.tasks.write().await;
            if let Some(user_tasks) = tasks.get_mut(&user_id) {
                if let Some(task_state) = user_tasks.get_mut(&task_id) {
                    task_state.status = TaskStatus::Failed;
                    task_state.result = Some(format!("Failed to create task: {}", e));
                }
            }
            return;
        }
    };

    // Set the user-requested model as minimum capability floor
    if !requested_model.is_empty() {
        task.analysis_mut().requested_model = Some(requested_model);
    }

    // Prepare workspace for this task (or use a provided custom dir)
    let working_dir = if let Some(dir) = working_dir {
        match workspace::prepare_custom_workspace(&state.config, &state.mcp, dir).await {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("Failed to prepare custom workspace: {}", e);
                state.config.working_dir.clone()
            }
        }
    } else {
        match workspace::prepare_task_workspace(&state.config, &state.mcp, task_id).await {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("Failed to prepare task workspace: {}", e);
                state.config.working_dir.clone()
            }
        }
    };

    let mut config = state.config.clone();
    if let Some(agent) = agent_override {
        config.opencode_agent = Some(agent);
    }

    // Create context with the specified working directory
    let mut ctx = AgentContext::new(config, working_dir);
    ctx.mcp = Some(Arc::clone(&state.mcp));

    // Run the hierarchical agent
    let result = state.root_agent.execute(&mut task, &ctx).await;

    // Update task with result
    {
        let mut tasks = state.tasks.write().await;
        if let Some(user_tasks) = tasks.get_mut(&user_id) {
            if let Some(task_state) = user_tasks.get_mut(&task_id) {
                // Extract iterations and tools from result data
                // Note: RootAgent wraps executor data under "execution" field
                if let Some(data) = &result.data {
                    // Try to get execution data (may be nested under "execution" from RootAgent)
                    let exec_data = data.get("execution").unwrap_or(data);

                    // Update iterations count from execution signals
                    if let Some(signals) = exec_data.get("execution_signals") {
                        if let Some(iterations) = signals.get("iterations").and_then(|v| v.as_u64())
                        {
                            task_state.iterations = iterations as usize;
                        }
                    }

                    // Add log entries for tools used
                    if let Some(tools_used) = exec_data.get("tools_used") {
                        if let Some(arr) = tools_used.as_array() {
                            for tool in arr {
                                task_state.log.push(TaskLogEntry {
                                    timestamp: "0".to_string(),
                                    entry_type: LogEntryType::ToolCall,
                                    content: tool.as_str().unwrap_or("").to_string(),
                                });
                            }
                        }
                    }
                }

                // Add final response log
                task_state.log.push(TaskLogEntry {
                    timestamp: "0".to_string(),
                    entry_type: LogEntryType::Response,
                    content: result.output.clone(),
                });

                if result.success {
                    task_state.status = TaskStatus::Completed;
                    task_state.result = Some(result.output);
                } else {
                    task_state.status = TaskStatus::Failed;
                    task_state.result = Some(format!("Error: {}", result.output));
                }
            }
        }
    }
}

/// Get task status and result.
async fn get_task(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let tasks = state.tasks.read().await;
    tasks
        .get(&user.id)
        .and_then(|t| t.get(&id))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task {} not found", id)))
        .and_then(|ts| {
            serde_json::to_value(ts).map(Json).map_err(|e| {
                tracing::error!("Failed to serialize task {}: {}", id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to serialize task".to_string(),
                )
            })
        })
}

/// Stream task progress via SSE.
async fn stream_task(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)>
{
    // Check task exists
    {
        let tasks = state.tasks.read().await;
        if !tasks
            .get(&user.id)
            .map(|t| t.contains_key(&id))
            .unwrap_or(false)
        {
            return Err((StatusCode::NOT_FOUND, format!("Task {} not found", id)));
        }
    }

    // Create a stream that polls task state
    let stream = async_stream::stream! {
        let mut last_log_len = 0;

        loop {
            let (status, log_entries, result) = {
                let tasks = state.tasks.read().await;
                let user_tasks = tasks.get(&user.id);
                if let Some(task) = user_tasks.and_then(|t| t.get(&id)) {
                    (task.status.clone(), task.log.clone(), task.result.clone())
                } else {
                    break;
                }
            };

            // Send new log entries
            for entry in log_entries.iter().skip(last_log_len) {
                match Event::default().event("log").json_data(entry) {
                    Ok(event) => yield Ok(event),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to serialize task log SSE event");
                    }
                }
            }
            last_log_len = log_entries.len();

            // Check if task is done
            if matches!(
                status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                match Event::default()
                    .event("done")
                    .json_data(serde_json::json!({
                        "status": status,
                        "result": result
                    })) {
                    Ok(event) => yield Ok(event),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to serialize task done SSE event");
                    }
                }
                break;
            }

            // Poll interval
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    };

    Ok(Sse::new(stream))
}

// ==================== Memory Endpoints (Stub - Memory Removed) ====================

/// Query parameters for listing runs.
#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

/// List archived runs (stub - memory system removed).
async fn list_runs(Query(params): Query<ListRunsQuery>) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);
    Json(serde_json::json!({
        "runs": [],
        "limit": limit,
        "offset": offset
    }))
}

/// Get a specific run (stub - memory system removed).
async fn get_run(Path(id): Path<Uuid>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_FOUND,
        format!("Run {} not found (memory system disabled)", id),
    ))
}

/// Get events for a run (stub - memory system removed).
async fn get_run_events(Path(id): Path<Uuid>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "run_id": id,
        "events": []
    }))
}

/// Get tasks for a run (stub - memory system removed).
async fn get_run_tasks(Path(id): Path<Uuid>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "run_id": id,
        "tasks": []
    }))
}

/// Query parameters for memory search (stub - memory system removed).
#[derive(Debug, Deserialize)]
pub struct SearchMemoryQuery {
    q: String,
    #[serde(rename = "k")]
    _k: Option<usize>,
    #[serde(rename = "run_id")]
    _run_id: Option<Uuid>,
}

/// Search memory (stub - memory system removed).
async fn search_memory(Query(params): Query<SearchMemoryQuery>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "query": params.q,
        "results": []
    }))
}

// Note: opencode_session_cleanup_task removed - per-workspace CLI execution doesn't need central session cleanup

/// Background task that proactively refreshes OAuth tokens before they expire.
///
/// This prevents the 24-hour reconnection issue by:
/// 1. Checking credential files (credentials.json) for OAuth tokens every 15 minutes
/// 2. Refreshing tokens that will expire within 1 hour
/// 3. Syncing refreshed tokens to all storage tiers (sandboxed-sh, OpenCode, Claude CLI)
/// 4. Handling refresh token rotation (updating stored refresh token if changed)
///
/// The refresher checks credential files directly rather than relying on the
/// AIProviderStore, because OAuth tokens from the callback are stored in
/// credentials.json but may not have a corresponding AIProvider entry.
async fn oauth_token_refresher_loop(
    _ai_providers: Arc<crate::ai_providers::AIProviderStore>,
    working_dir: std::path::PathBuf,
) {
    use crate::ai_providers::ProviderType;

    // Check every 15 minutes
    let check_interval = std::time::Duration::from_secs(15 * 60);
    // Refresh tokens that will expire within 1 hour
    let refresh_threshold_ms: i64 = 60 * 60 * 1000; // 1 hour in milliseconds

    // Provider types that support OAuth
    let oauth_capable_types = [
        ProviderType::Anthropic,
        ProviderType::OpenAI,
        ProviderType::Google,
    ];

    tracing::info!(
        "OAuth token refresher task started (check every 15 min, refresh if < 1 hour until expiry)"
    );

    // Run an initial check after a short delay (let the server finish booting).
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // Populate missing account emails on startup (e.g. Anthropic tokens loaded
    // from credential files don't include email — fetch via userinfo endpoint).
    {
        let accounts = ai_providers_api::read_provider_accounts_state(&working_dir);
        for &provider_type in &oauth_capable_types {
            let provider_id = provider_type.id();
            if accounts.contains_key(provider_id) {
                continue; // already have email
            }
            let entry = match ai_providers_api::read_oauth_token_entry(provider_type) {
                Some(e) => e,
                None => continue,
            };
            if entry.access_token.is_empty() {
                continue;
            }
            // Anthropic needs a dedicated userinfo call; others use JWT id_token
            // which only arrives during the OAuth callback (not from credential files).
            if matches!(provider_type, ProviderType::Anthropic) {
                if let Some(email) =
                    ai_providers_api::fetch_anthropic_account_email(&entry.access_token).await
                {
                    tracing::info!(
                        provider_type = ?provider_type,
                        email = %email,
                        "Fetched Anthropic account email via userinfo endpoint"
                    );
                    let _ =
                        ai_providers_api::update_provider_account(&working_dir, provider_id, email);
                }
            }
        }
    }

    loop {
        // Check credential files directly for each OAuth-capable provider type.
        // This ensures we find tokens even if they aren't in the AIProviderStore.
        let mut found_count = 0u32;
        let mut refreshed_count = 0u32;

        for &provider_type in &oauth_capable_types {
            let entry = match ai_providers_api::read_oauth_token_entry(provider_type) {
                Some(e) => e,
                None => continue,
            };

            // Skip entries without a refresh token
            if entry.refresh_token.trim().is_empty() {
                continue;
            }

            found_count += 1;

            let now_ms = chrono::Utc::now().timestamp_millis();
            let time_until_expiry = entry.expires_at - now_ms;
            let is_expired = time_until_expiry <= 0;

            tracing::debug!(
                provider_type = ?provider_type,
                expires_at = entry.expires_at,
                expires_in_minutes = time_until_expiry / 1000 / 60,
                is_expired = is_expired,
                needs_refresh = time_until_expiry <= refresh_threshold_ms,
                "Checking OAuth token from credentials file"
            );

            if time_until_expiry > refresh_threshold_ms {
                continue;
            }

            if is_expired {
                tracing::warn!(
                    provider_type = ?provider_type,
                    expired_since_minutes = (-time_until_expiry) / 1000 / 60,
                    "OAuth token is ALREADY EXPIRED, attempting refresh..."
                );
            } else {
                tracing::info!(
                    provider_type = ?provider_type,
                    expires_in_minutes = time_until_expiry / 1000 / 60,
                    "OAuth token will expire soon, refreshing proactively"
                );
            }

            match ai_providers_api::refresh_oauth_token_with_lock(provider_type, entry.expires_at)
                .await
            {
                Ok((_new_access, _new_refresh, new_expires_at)) => {
                    let new_time_until = new_expires_at - now_ms;
                    tracing::info!(
                        provider_type = ?provider_type,
                        new_expires_in_minutes = new_time_until / 1000 / 60,
                        "Successfully refreshed OAuth token proactively"
                    );
                    refreshed_count += 1;
                }
                Err(e) => match e {
                    ai_providers_api::OAuthRefreshError::InvalidGrant(reason) => {
                        tracing::warn!(
                            provider_type = ?provider_type,
                            reason = %reason,
                            "OAuth refresh token expired or revoked - user needs to re-authenticate"
                        );
                    }
                    ai_providers_api::OAuthRefreshError::Other(msg) => {
                        tracing::error!(
                            provider_type = ?provider_type,
                            error = %msg,
                            "Failed to refresh OAuth token"
                        );
                    }
                },
            }
        }

        tracing::debug!(
            oauth_tokens_found = found_count,
            oauth_tokens_refreshed = refreshed_count,
            "OAuth refresh check cycle complete"
        );

        tokio::time::sleep(check_interval).await;
    }
}
