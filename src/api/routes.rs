//! HTTP route handlers.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        Json,
    },
    routing::{get, post},
    Router,
};
use futures::stream::Stream;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::agents::{AgentContext, AgentRef, TuningParams};
use crate::agents::orchestrator::RootAgent;
use crate::budget::ModelPricing;
use crate::config::Config;
use crate::llm::OpenRouterClient;
use crate::tools::ToolRegistry;

use super::types::*;

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub tasks: RwLock<HashMap<Uuid, TaskState>>,
    /// The hierarchical root agent
    pub root_agent: AgentRef,
}

/// Start the HTTP server.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    // Load empirically tuned parameters (if present in workspace)
    let tuning = TuningParams::load_from_workspace(&config.workspace_path).await;

    // Create the root agent (hierarchical)
    let root_agent: AgentRef = Arc::new(RootAgent::new_with_tuning(&tuning));
    
    let state = Arc::new(AppState {
        config: config.clone(),
        tasks: RwLock::new(HashMap::new()),
        root_agent,
    });

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/task", post(create_task))
        .route("/api/task/:id", get(get_task))
        .route("/api/task/:id/stream", get(stream_task))
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
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Create a new task.
async fn create_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<CreateTaskResponse>, (StatusCode, String)> {
    let id = Uuid::new_v4();
    let model = req.model.unwrap_or_else(|| state.config.default_model.clone());
    
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
    let workspace_path = req.workspace_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.config.workspace_path.clone());
    
    tokio::spawn(async move {
        run_agent_task(state_clone, id, task_description, model, workspace_path).await;
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
    workspace_path: std::path::PathBuf,
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
    
    let task_result = crate::task::Task::new(
        task_description.clone(),
        verification,
        budget,
    );

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

    // Create context with the specified workspace
    let llm = Arc::new(OpenRouterClient::new(state.config.api_key.clone()));
    let tools = ToolRegistry::new();
    let pricing = Arc::new(ModelPricing::new());
    
    let ctx = AgentContext::new(
        state.config.clone(),
        llm,
        tools,
        pricing,
        workspace_path,
    );

    // Run the hierarchical agent
    let result = state.root_agent.execute(&mut task, &ctx).await;
    
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
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, (StatusCode, String)> {
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
            if status == TaskStatus::Completed || status == TaskStatus::Failed {
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
