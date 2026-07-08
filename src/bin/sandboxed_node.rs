use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use sandboxed_sh::remote_node::{
    bearer_token, run_lease_command, ExecuteResponse, LeaseRequest, NodeHeartbeat,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
struct NodeState {
    node_id: String,
    shared_token: String,
    work_root: PathBuf,
    capacity_total: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sandboxed_sh=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let node_id = std::env::var("SANDBOXED_NODE_ID").unwrap_or_else(|_| "local-node".to_string());
    let shared_token = std::env::var("SANDBOXED_NODE_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("SANDBOXED_NODE_TOKEN must be set"))?;
    let bind = std::env::var("SANDBOXED_NODE_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3088".to_string())
        .parse::<SocketAddr>()?;
    let work_root = std::env::var("SANDBOXED_NODE_WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/sandboxed-node/work"));
    let capacity_total = std::env::var("SANDBOXED_NODE_CAPACITY")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .unwrap_or(1);
    tokio::fs::create_dir_all(&work_root).await?;

    let state = Arc::new(NodeState {
        node_id,
        shared_token,
        work_root,
        capacity_total,
    });
    let app = Router::new()
        .route("/heartbeat", get(heartbeat))
        .route("/execute", post(execute))
        .with_state(state);

    info!("starting sandboxed-node on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn check_auth(headers: &HeaderMap, state: &NodeState) -> Result<(), (StatusCode, String)> {
    match bearer_token(headers) {
        Some(token) if token == state.shared_token => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "invalid node token".to_string())),
    }
}

async fn heartbeat(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
) -> Result<Json<NodeHeartbeat>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    Ok(Json(NodeHeartbeat {
        node_id: state.node_id.clone(),
        online: true,
        capacity_total: state.capacity_total,
        capacity_available: state.capacity_total,
        active_leases: 0,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }))
}

async fn execute(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    match run_lease_command(
        &state.node_id,
        &state.shared_token,
        state.work_root.clone(),
        request,
    )
    .await
    {
        Ok(response) => Ok(Json(response)),
        Err(err) => {
            warn!("lease execution rejected: {err}");
            Err((StatusCode::BAD_REQUEST, err.to_string()))
        }
    }
}
