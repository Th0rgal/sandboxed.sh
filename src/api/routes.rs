//! HTTP route handlers.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use axum::middleware;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
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

use crate::agents::orchestrator::RootAgent;
use crate::agents::{AgentContext, AgentRef, TuningParams};
use crate::budget::ModelPricing;
use crate::config::Config;
use crate::llm::OpenRouterClient;
use crate::mcp::McpRegistry;
use crate::memory::{self, MemorySystem};
use crate::tools::ToolRegistry;

use super::auth;
use super::console;
use super::control;
use super::fs;
use super::mcp as mcp_api;
use super::types::*;

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub tasks: RwLock<HashMap<Uuid, TaskState>>,
    /// The hierarchical root agent
    pub root_agent: AgentRef,
    /// Memory system (optional)
    pub memory: Option<MemorySystem>,
    /// Global interactive control session
    pub control: control::ControlState,
    /// MCP server registry
    pub mcp: Arc<McpRegistry>,
}

/// Start the HTTP server.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    // Load empirically tuned parameters (if present in working directory)
    let tuning = TuningParams::load_from_working_dir(&config.working_dir).await;

    // Create the root agent (hierarchical)
    let root_agent: AgentRef = Arc::new(RootAgent::new_with_tuning(&tuning));

    // Initialize memory system (optional - needs Supabase config)
    let memory = memory::init_memory(&config.memory, &config.api_key).await;

    // Initialize MCP registry
    let mcp = Arc::new(McpRegistry::new(&config.working_dir).await);
    // Refresh all MCPs in background
    {
        let mcp_clone = Arc::clone(&mcp);
        tokio::spawn(async move {
            mcp_clone.refresh_all().await;
        });
    }

    // Spawn the single global control session actor.
    let control_state =
        control::spawn_control_session(config.clone(), Arc::clone(&root_agent), memory.clone());

    let state = Arc::new(AppState {
        config: config.clone(),
        tasks: RwLock::new(HashMap::new()),
        root_agent,
        memory,
        control: control_state,
        mcp,
    });

    let public_routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth::login))
        // WebSocket console uses subprotocol-based auth (browser can't set Authorization header)
        .route("/api/console/ws", get(console::console_ws));

    // File upload route with increased body limit (10GB)
    let upload_route = Router::new()
        .route("/api/fs/upload", post(fs::upload))
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
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_auth,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Server listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

/// Health check endpoint.
async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        dev_mode: state.config.dev_mode,
        auth_required: state.config.auth.auth_required(state.config.dev_mode),
    })
}

/// Get system statistics.
async fn get_stats(State(state): State<Arc<AppState>>) -> Json<StatsResponse> {
    let tasks = state.tasks.read().await;

    let total_tasks = tasks.len();
    let active_tasks = tasks
        .values()
        .filter(|t| t.status == TaskStatus::Running)
        .count();
    let completed_tasks = tasks
        .values()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let failed_tasks = tasks
        .values()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();

    // Calculate total cost (would need to track this properly in production)
    let total_cost_cents = 0u64; // TODO: Track actual costs

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
async fn list_tasks(State(state): State<Arc<AppState>>) -> Json<Vec<TaskState>> {
    let tasks = state.tasks.read().await;
    let mut task_list: Vec<_> = tasks.values().cloned().collect();
    // Sort by most recent first (by ID since UUIDs are time-ordered)
    task_list.sort_by(|a, b| b.id.cmp(&a.id));
    Json(task_list)
}

/// Stop a running task.
async fn stop_task(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut tasks = state.tasks.write().await;

    if let Some(task) = tasks.get_mut(&id) {
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
        tasks.insert(id, task_state);
    }

    // Spawn background task to run the agent
    let state_clone = Arc::clone(&state);
    let task_description = req.task.clone();
    let working_dir = req
        .working_dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.config.working_dir.clone());

    tokio::spawn(async move {
        run_agent_task(state_clone, id, task_description, model, working_dir).await;
    });

    Ok(Json(CreateTaskResponse {
        id,
        status: TaskStatus::Pending,
    }))
}

/// Run the agent for a task (background).
async fn run_agent_task(
    state: Arc<AppState>,
    task_id: Uuid,
    task_description: String,
    _model: String,
    working_dir: std::path::PathBuf,
) {
    // Update status to running
    {
        let mut tasks = state.tasks.write().await;
        if let Some(task_state) = tasks.get_mut(&task_id) {
            task_state.status = TaskStatus::Running;
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
            if let Some(task_state) = tasks.get_mut(&task_id) {
                task_state.status = TaskStatus::Failed;
                task_state.result = Some(format!("Failed to create task: {}", e));
            }
            return;
        }
    };

    // Create context with the specified working directory and memory
    let llm = Arc::new(OpenRouterClient::new(state.config.api_key.clone()));
    let tools = ToolRegistry::new();
    let pricing = Arc::new(ModelPricing::new());

    let ctx = AgentContext::with_memory(
        state.config.clone(),
        llm,
        tools,
        pricing,
        working_dir,
        state.memory.clone(),
    );

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

    // Complete the memory run
    if let (Some(ref mem), Some(run_id)) = (&state.memory, memory_run_id) {
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
                &result.output[..500]
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
        if let Some(task_state) = tasks.get_mut(&task_id) {
            // Add log entries from result data
            if let Some(data) = &result.data {
                if let Some(tools_used) = data.get("tools_used") {
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

/// Get task status and result.
async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<TaskState>, (StatusCode, String)> {
    let tasks = state.tasks.read().await;

    tasks
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task {} not found", id)))
}

/// Stream task progress via SSE.
async fn stream_task(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)>
{
    // Check task exists
    {
        let tasks = state.tasks.read().await;
        if !tasks.contains_key(&id) {
            return Err((StatusCode::NOT_FOUND, format!("Task {} not found", id)));
        }
    }

    // Create a stream that polls task state
    let stream = async_stream::stream! {
        let mut last_log_len = 0;

        loop {
            let (status, log_entries, result) = {
                let tasks = state.tasks.read().await;
                if let Some(task) = tasks.get(&id) {
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
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

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
