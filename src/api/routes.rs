//! HTTP route handlers.

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
use crate::budget::ModelPricing;
use crate::config::{AuthMode, Config};
use crate::llm::OpenRouterClient;
use crate::mcp::McpRegistry;
use crate::memory::{self, MemorySystem};
use crate::tools::ToolRegistry;
use crate::workspace;

use super::auth::{self, AuthUser};
use super::console;
use super::control;
use super::desktop_stream;
use super::fs;
use super::library as library_api;
use super::mcp as mcp_api;
use super::types::*;
use super::workspaces as workspaces_api;

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub tasks: RwLock<HashMap<String, HashMap<Uuid, TaskState>>>,
    /// The agent used for task execution
    pub root_agent: AgentRef,
    /// Memory system (optional)
    pub memory: Option<MemorySystem>,
    /// Global interactive control session
    pub control: control::ControlHub,
    /// MCP server registry
    pub mcp: Arc<McpRegistry>,
    /// Benchmark registry for task-aware model selection
    pub benchmarks: crate::budget::SharedBenchmarkRegistry,
    /// Model resolver for auto-upgrading outdated model names
    pub resolver: crate::budget::SharedModelResolver,
    /// Configuration library (git-based)
    pub library: library_api::SharedLibrary,
    /// Workspace store
    pub workspaces: workspace::SharedWorkspaceStore,
    /// Agent configuration store
    pub agents: Arc<crate::agent_config::AgentStore>,
}

/// Start the HTTP server.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    // Always use OpenCode backend
    let root_agent: AgentRef = Arc::new(OpenCodeAgent::new(config.clone()));

    // Initialize memory system (optional - needs Supabase config).
    // Disable memory in multi-user mode to avoid cross-user leakage.
    let memory = if matches!(config.auth.auth_mode(config.dev_mode), AuthMode::MultiUser) {
        tracing::warn!("Multi-user auth enabled: disabling memory system");
        None
    } else {
        memory::init_memory(&config.memory, &config.api_key).await
    };

    // Initialize MCP registry
    let mcp = Arc::new(McpRegistry::new(&config.working_dir).await);
    // Refresh all MCPs in background
    {
        let mcp_clone = Arc::clone(&mcp);
        tokio::spawn(async move {
            mcp_clone.refresh_all().await;
        });
    }

    // Load benchmark registry for task-aware model selection
    let benchmarks = crate::budget::load_benchmarks(&config.working_dir.to_string_lossy());

    // Load model resolver for auto-upgrading outdated model names
    let resolver = crate::budget::load_resolver(&config.working_dir.to_string_lossy());

    // Initialize workspace store
    let workspaces = Arc::new(workspace::WorkspaceStore::new(config.working_dir.clone()));

    // Initialize agent configuration store
    let agents = Arc::new(crate::agent_config::AgentStore::new(
        config.working_dir.join(".openagent/agents.json"),
    ).await);

    // Spawn the single global control session actor.
    let control_state = control::ControlHub::new(
        config.clone(),
        Arc::clone(&root_agent),
        memory.clone(),
        Arc::clone(&benchmarks),
        Arc::clone(&resolver),
        Arc::clone(&mcp),
        Arc::clone(&workspaces),
    );

    // Initialize configuration library (optional - can also be configured at runtime)
    let library: library_api::SharedLibrary = Arc::new(RwLock::new(None));
    if let Some(library_remote) = config.library_remote.clone() {
        let library_clone = Arc::clone(&library);
        let library_path = config.library_path.clone();
        tokio::spawn(async move {
            match crate::library::LibraryStore::new(library_path, &library_remote).await {
                Ok(store) => {
                    tracing::info!("Configuration library initialized from {}", library_remote);
                    *library_clone.write().await = Some(Arc::new(store));
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize configuration library: {}", e);
                }
            }
        });
    } else {
        tracing::info!("Configuration library disabled (no remote configured)");
    }

    let state = Arc::new(AppState {
        config: config.clone(),
        tasks: RwLock::new(HashMap::new()),
        root_agent,
        memory,
        control: control_state,
        mcp,
        benchmarks,
        resolver,
        library,
        workspaces,
        agents,
    });

    let public_routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth::login))
        // WebSocket console uses subprotocol-based auth (browser can't set Authorization header)
        .route("/api/console/ws", get(console::console_ws))
        // WebSocket desktop stream uses subprotocol-based auth
        .route(
            "/api/desktop/stream",
            get(desktop_stream::desktop_stream_ws),
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
        // Global control session endpoints
        .route("/api/control/message", post(control::post_message))
        .route("/api/control/tool_result", post(control::post_tool_result))
        .route("/api/control/stream", get(control::stream))
        .route("/api/control/cancel", post(control::post_cancel))
        // State snapshots (for refresh resilience)
        .route("/api/control/tree", get(control::get_tree))
        .route("/api/control/progress", get(control::get_progress))
        // Diagnostic endpoints
        .route("/api/control/diagnostics/opencode", get(control::get_opencode_diagnostics))
        // Mission management endpoints
        .route("/api/control/missions", get(control::list_missions))
        .route("/api/control/missions", post(control::create_mission))
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
            "/api/control/missions/:id/load",
            post(control::load_mission),
        )
        .route(
            "/api/control/missions/:id/status",
            post(control::set_mission_status),
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
        .route("/api/mcp/:id/enable", post(mcp_api::enable_mcp))
        .route("/api/mcp/:id/disable", post(mcp_api::disable_mcp))
        .route("/api/mcp/:id/refresh", post(mcp_api::refresh_mcp))
        // Tools management endpoints
        .route("/api/tools", get(mcp_api::list_tools))
        .route("/api/tools/:name/toggle", post(mcp_api::toggle_tool))
        // Provider and model management endpoints
        .route("/api/providers", get(super::providers::list_providers))
        .route("/api/models", get(list_models))
        .route("/api/models/refresh", post(refresh_models))
        .route("/api/models/families", get(list_model_families))
        .route("/api/models/performance", get(get_model_performance))
        // Library management endpoints
        .nest("/api/library", library_api::routes())
        // Workspace management endpoints
        .nest("/api/workspaces", workspaces_api::routes())
        // Agent configuration endpoints
        .nest("/api/agents", super::agents::routes())
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
        tracing::info!(
            "Marked {} missions as interrupted: {:?}",
            all_interrupted.len(),
            all_interrupted
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
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        dev_mode: state.config.dev_mode,
        auth_required: state.config.auth.auth_required(state.config.dev_mode),
        auth_mode: auth_mode.to_string(),
        max_iterations: state.config.max_iterations,
    })
}

/// Get system statistics.
async fn get_stats(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<StatsResponse> {
    let tasks = state.tasks.read().await;
    let user_tasks = tasks.get(&user.id);

    let total_tasks = user_tasks.map(|t| t.len()).unwrap_or(0);
    let active_tasks = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Running)
                .count()
        })
        .unwrap_or(0);
    let completed_tasks = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Completed)
                .count()
        })
        .unwrap_or(0);
    let failed_tasks = user_tasks
        .map(|t| {
            t.values()
                .filter(|s| s.status == TaskStatus::Failed)
                .count()
        })
        .unwrap_or(0);

    // Calculate total cost from runs in database
    let total_cost_cents = if let Some(mem) = &state.memory {
        mem.supabase.get_total_cost_cents().await.unwrap_or(0)
    } else {
        0
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
    task_list.sort_by(|a, b| b.id.cmp(&a.id));
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
        .unwrap_or_else(|| state.config.default_model.clone());

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
    let working_dir = req.working_dir.map(std::path::PathBuf::from);

    tokio::spawn(async move {
        run_agent_task(
            state_clone,
            user.id,
            id,
            task_description,
            model,
            working_dir,
        )
        .await;
    });

    Ok(Json(CreateTaskResponse {
        id,
        status: TaskStatus::Pending,
    }))
}

/// Run the agent for a task (background).
async fn run_agent_task(
    state: Arc<AppState>,
    user_id: String,
    task_id: Uuid,
    task_description: String,
    requested_model: String,
    working_dir: Option<std::path::PathBuf>,
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

    // Create a Task object for the hierarchical agent
    let budget = crate::budget::Budget::new(1000); // $10 default budget
    let verification = crate::task::VerificationCriteria::None;

    let task_result = crate::task::Task::new(task_description.clone(), verification, budget);

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

    // Create context with the specified working directory and memory
    let llm = Arc::new(OpenRouterClient::new(state.config.api_key.clone()));
    let tools = ToolRegistry::empty();
    let pricing = Arc::new(ModelPricing::new());

    let mut ctx = AgentContext::with_memory(
        state.config.clone(),
        llm,
        tools,
        pricing,
        working_dir,
        state.memory.clone(),
    );
    ctx.benchmarks = Some(Arc::clone(&state.benchmarks));
    ctx.resolver = Some(Arc::clone(&state.resolver));
    ctx.mcp = Some(Arc::clone(&state.mcp));

    // Create a run in memory if available
    let memory_run_id = if let Some(ref mem) = state.memory {
        match mem.writer.create_run(&task_description).await {
            Ok(run_id) => {
                let _ = mem
                    .writer
                    .update_run_status(run_id, crate::memory::MemoryStatus::Running)
                    .await;
                Some(run_id)
            }
            Err(e) => {
                tracing::warn!("Failed to create memory run: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Run the hierarchical agent
    let result = state.root_agent.execute(&mut task, &ctx).await;

    // Complete the memory run and record events
    if let (Some(ref mem), Some(run_id)) = (&state.memory, memory_run_id) {
        // Record tool call events from result data
        if let Some(data) = &result.data {
            let recorder = crate::memory::EventRecorder::new(run_id);

            // RootAgent wraps executor data under "execution" field
            let exec_data = data.get("execution").unwrap_or(data);

            tracing::debug!(
                "Recording events for run {}, exec_data keys: {:?}",
                run_id,
                exec_data.as_object().map(|o| o.keys().collect::<Vec<_>>())
            );

            // Record each tool call as an event
            if let Some(tools_used) = exec_data.get("tools_used") {
                if let Some(arr) = tools_used.as_array() {
                    tracing::debug!("Recording {} tool call events", arr.len());
                    for tool_entry in arr {
                        let tool_str = tool_entry.as_str().unwrap_or("");
                        let event = crate::memory::RecordedEvent::new(
                            "TaskExecutor",
                            crate::memory::EventKind::ToolCall,
                        )
                        .with_preview(tool_str);
                        if let Err(e) = mem.writer.record_event(&recorder, event).await {
                            tracing::warn!("Failed to record tool call event: {}", e);
                        }
                    }
                }
            } else {
                tracing::debug!("No tools_used found in exec_data");
            }

            // Record final response as an event
            let prompt_tokens = exec_data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);
            let completion_tokens = exec_data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);

            let response_event = crate::memory::RecordedEvent::new(
                "TaskExecutor",
                crate::memory::EventKind::LlmResponse,
            )
            .with_preview(&if result.output.len() > 1000 {
                let safe_end = crate::memory::safe_truncate_index(&result.output, 1000);
                result.output[..safe_end].to_string()
            } else {
                result.output.clone()
            })
            .with_tokens(prompt_tokens, completion_tokens, result.cost_cents as i32);
            if let Err(e) = mem.writer.record_event(&recorder, response_event).await {
                tracing::warn!("Failed to record response event: {}", e);
            }
        } else {
            tracing::debug!("No result.data available for event recording");
        }

        let _ = mem
            .writer
            .complete_run(
                run_id,
                &result.output,
                result.cost_cents as i32,
                result.success,
            )
            .await;

        // Generate and store summary
        let summary = format!(
            "Task: {}\nResult: {}\nSuccess: {}",
            task_description,
            if result.output.len() > 500 {
                let safe_end = crate::memory::safe_truncate_index(&result.output, 500);
                &result.output[..safe_end]
            } else {
                &result.output
            },
            result.success
        );
        let _ = mem.writer.store_run_summary(run_id, &summary).await;

        // Archive the run
        let _ = mem.writer.archive_run(run_id).await;
    }

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
                let event = Event::default()
                    .event("log")
                    .json_data(entry)
                    .unwrap();
                yield Ok(event);
            }
            last_log_len = log_entries.len();

            // Check if task is done
            if status == TaskStatus::Completed || status == TaskStatus::Failed || status == TaskStatus::Cancelled {
                let event = Event::default()
                    .event("done")
                    .json_data(serde_json::json!({
                        "status": status,
                        "result": result
                    }))
                    .unwrap();
                yield Ok(event);
                break;
            }

            // Poll interval
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    };

    Ok(Sse::new(stream))
}

// ==================== Memory Endpoints ====================

/// Query parameters for listing runs.
#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

/// List archived runs.
async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

    if state.memory.is_none() {
        return Ok(Json(serde_json::json!({
            "runs": [],
            "limit": limit,
            "offset": offset
        })));
    }

    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let runs = mem
        .retriever
        .list_runs(limit, offset)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "runs": runs,
        "limit": limit,
        "offset": offset
    })))
}

/// Get a specific run.
async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.memory.is_none() {
        return Err((StatusCode::NOT_FOUND, "Run not found".to_string()));
    }
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let run = mem
        .retriever
        .get_run(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Run {} not found", id)))?;

    Ok(Json(serde_json::json!(run)))
}

/// Get events for a run.
async fn get_run_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<ListRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.memory.is_none() {
        return Ok(Json(serde_json::json!({
            "run_id": id,
            "events": []
        })));
    }
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let events = mem
        .retriever
        .get_run_events(id, params.limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "run_id": id,
        "events": events
    })))
}

/// Get tasks for a run.
async fn get_run_tasks(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.memory.is_none() {
        return Ok(Json(serde_json::json!({
            "run_id": id,
            "tasks": []
        })));
    }
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let tasks = mem
        .retriever
        .get_run_tasks(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "run_id": id,
        "tasks": tasks
    })))
}

/// Query parameters for memory search.
#[derive(Debug, Deserialize)]
pub struct SearchMemoryQuery {
    q: String,
    k: Option<usize>,
    run_id: Option<Uuid>,
}

/// Search memory.
async fn search_memory(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchMemoryQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.memory.is_none() {
        return Ok(Json(serde_json::json!({
            "query": params.q,
            "results": []
        })));
    }
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let results = mem
        .retriever
        .search(&params.q, params.k, None, params.run_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "query": params.q,
        "results": results
    })))
}

// ============================================================================
// Model Management Endpoints
// ============================================================================

/// List all model families with their latest versions.
async fn list_model_families(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let resolver = state.resolver.read().await;
    let families = resolver.families();

    let family_list: Vec<serde_json::Value> = families
        .iter()
        .map(|(name, family)| {
            serde_json::json!({
                "name": name,
                "latest": family.latest,
                "members": family.members,
                "tier": family.tier
            })
        })
        .collect();

    Json(serde_json::json!({
        "families": family_list,
        "count": family_list.len()
    }))
}

/// List available models with optional filtering.
#[derive(Debug, Deserialize)]
pub struct ListModelsQuery {
    /// Filter by tier: "flagship", "mid", "fast"
    tier: Option<String>,
    /// Only show latest version of each family
    latest_only: Option<bool>,
}

async fn list_models(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListModelsQuery>,
) -> Json<serde_json::Value> {
    let resolver = state.resolver.read().await;

    let models: Vec<&str> = if let Some(tier) = &params.tier {
        resolver.models_by_tier(tier)
    } else if params.latest_only.unwrap_or(false) {
        resolver.latest_models()
    } else {
        // Return all latest models by default
        resolver.latest_models()
    };

    Json(serde_json::json!({
        "models": models,
        "count": models.len()
    }))
}

/// Response for model refresh endpoint.
#[derive(serde::Serialize)]
struct RefreshModelsResponse {
    success: bool,
    message: String,
    families_count: usize,
    aliases_count: usize,
}

/// Refresh model data by reloading from disk.
///
/// This reloads the models_with_benchmarks.json file to pick up any updates.
/// To fully refresh from OpenRouter API and benchmarks, run the merge_benchmarks.py script.
async fn refresh_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RefreshModelsResponse>, (StatusCode, String)> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let path = format!("{}/models_with_benchmarks.json", working_dir);

    // Reload resolver from disk
    match crate::budget::ModelResolver::load_from_file(&path) {
        Ok(new_resolver) => {
            let families_count = new_resolver.families().len();

            // Update the shared resolver
            {
                let mut resolver = state.resolver.write().await;
                *resolver = new_resolver;
            }

            // Also reload benchmarks from disk
            match crate::budget::BenchmarkRegistry::load_from_file(&path) {
                Ok(new_benchmarks) => {
                    let benchmark_count = new_benchmarks.benchmark_count();
                    let mut benchmarks = state.benchmarks.write().await;
                    *benchmarks = new_benchmarks;
                    tracing::info!(
                        "Refreshed model resolver: {} families, {} benchmarks",
                        families_count,
                        benchmark_count
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to reload benchmarks: {}", e);
                }
            }

            Ok(Json(RefreshModelsResponse {
                success: true,
                message: format!("Model data refreshed from {}", path),
                families_count,
                aliases_count: 0,
            }))
        }
        Err(e) => {
            tracing::error!("Failed to reload model data: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to reload model data: {}", e),
            ))
        }
    }
}

/// Response for model performance endpoint.
#[derive(serde::Serialize)]
struct ModelPerformanceResponse {
    learned_stats: Vec<crate::budget::LearnedModelStats>,
    budget_estimates: Vec<crate::budget::LearnedBudgetEstimate>,
    best_models_by_task: std::collections::HashMap<String, String>,
}

/// Get learned model performance statistics.
///
/// Returns aggregated performance data from historical task outcomes,
/// used for self-improving model selection.
async fn get_model_performance(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ModelPerformanceResponse>, (StatusCode, String)> {
    let memory = match &state.memory {
        Some(m) => m,
        None => {
            return Ok(Json(ModelPerformanceResponse {
                learned_stats: vec![],
                budget_estimates: vec![],
                best_models_by_task: std::collections::HashMap::new(),
            }));
        }
    };

    // Fetch learned stats from database
    let learned_stats = memory
        .supabase
        .get_learned_model_stats()
        .await
        .map_err(|e| {
            tracing::warn!("Failed to get learned model stats: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get learned stats: {}", e),
            )
        })?;

    let budget_estimates = memory
        .supabase
        .get_learned_budget_estimates()
        .await
        .map_err(|e| {
            tracing::warn!("Failed to get learned budget estimates: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get budget estimates: {}", e),
            )
        })?;

    // Compute best models per task type
    let config = crate::budget::LearnedSelectionConfig::default();
    let best_models_by_task =
        crate::budget::learned::get_best_models_by_task_type(&learned_stats, &config);

    Ok(Json(ModelPerformanceResponse {
        learned_stats,
        budget_estimates,
        best_models_by_task,
    }))
}
