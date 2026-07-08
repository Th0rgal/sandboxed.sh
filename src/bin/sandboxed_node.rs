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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct NodeState {
    node_id: String,
    shared_token: String,
    work_root: PathBuf,
    capacity_total: u32,
    active_leases: AtomicU32,
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
        active_leases: AtomicU32::new(0),
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
    let active_leases = state.active_leases.load(Ordering::Acquire);
    Ok(Json(NodeHeartbeat {
        node_id: state.node_id.clone(),
        online: true,
        capacity_total: state.capacity_total,
        capacity_available: state.capacity_total.saturating_sub(active_leases),
        active_leases,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }))
}

async fn execute(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    state.active_leases.fetch_add(1, Ordering::AcqRel);
    let result = run_lease_command(
        &state.node_id,
        &state.shared_token,
        state.work_root.clone(),
        request,
    )
    .await;
    state.active_leases.fetch_sub(1, Ordering::AcqRel);
    match result {
        Ok(response) => Ok(Json(response)),
        Err(err) => {
            warn!("lease execution rejected: {err}");
            Err((StatusCode::BAD_REQUEST, err.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;
    use sandboxed_sh::remote_node::{create_lease_token, LeaseClaims};
    use uuid::Uuid;

    fn auth_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .expect("valid auth header"),
        );
        headers
    }

    #[tokio::test]
    async fn heartbeat_reports_active_lease_capacity() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(NodeState {
            node_id: "test-node".to_string(),
            shared_token: "node-secret".to_string(),
            work_root: work_root.path().to_path_buf(),
            capacity_total: 2,
            active_leases: AtomicU32::new(0),
        });
        let mission_id = Uuid::new_v4();
        let claims = LeaseClaims {
            mission_id,
            node_id: state.node_id.clone(),
            scope: "mission:execute".to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).timestamp(),
        };
        let request = LeaseRequest {
            mission_id,
            node_id: state.node_id.clone(),
            lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
            command: "sleep 0.2".to_string(),
        };
        let headers = auth_headers(&state.shared_token);

        let running = tokio::spawn(execute(
            State(state.clone()),
            headers.clone(),
            Json(request),
        ));

        let mut observed_busy = None;
        for _ in 0..20 {
            let snapshot = heartbeat(State(state.clone()), headers.clone())
                .await
                .expect("heartbeat")
                .0;
            if snapshot.active_leases == 1 {
                observed_busy = Some(snapshot);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let busy = observed_busy.expect("heartbeat should observe active execute");
        assert_eq!(busy.capacity_total, 2);
        assert_eq!(busy.capacity_available, 1);
        let _response = running
            .await
            .expect("execute task")
            .expect("execute response");

        let idle = heartbeat(State(state), headers).await.expect("heartbeat").0;
        assert_eq!(idle.active_leases, 0);
        assert_eq!(idle.capacity_available, 2);
    }
}
