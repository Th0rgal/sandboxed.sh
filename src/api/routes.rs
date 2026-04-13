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
    routing::{get, post},
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
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, marking running missions as interrupted...");

    // Send graceful shutdown command to all control sessions
    let sessions = state.control.all_sessions().await;
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
) -> Json<Vec<TaskState>> {
    let tasks = state.tasks.read().await;
    let mut task_list: Vec<_> = tasks
        .get(&user.id)
        .map(|t| t.values().cloned().collect())
        .unwrap_or_default();
    // Sort by most recent first (by ID since UUIDs are time-ordered)
    task_list.sort_by_key(|task| Reverse(task.id));
    Json(task_list)
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

/// Create a new task.
async fn create_task(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<CreateTaskResponse>, (StatusCode, String)> {
    let id = Uuid::new_v4();
    let model = req
        .model
        .or(state.config.default_model.clone())
        .unwrap_or_default();

    let task_state = TaskState {
        id,
        status: TaskStatus::Pending,
        task: req.task.clone(),
        model: model.clone(),
        iterations: 0,
        result: None,
        log: Vec::new(),
    };

    // Store task
    {
        let mut tasks = state.tasks.write().await;
        tasks
            .entry(user.id.clone())
            .or_default()
            .insert(id, task_state);
    }

    // Spawn background task to run the agent
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
) -> Result<Json<TaskState>, (StatusCode, String)> {
    let tasks = state.tasks.read().await;
    tasks
        .get(&user.id)
        .and_then(|t| t.get(&id).cloned())
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task {} not found", id)))
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
