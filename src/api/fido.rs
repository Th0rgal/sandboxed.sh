//! FIDO signing relay — forwards signing requests from container workspaces
//! to the iOS app for approval (auto-approve or Face ID).

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use super::control::AgentEvent;

/// Hub that holds pending FIDO signing requests, each waiting for an iOS
/// app response via a oneshot channel.
pub struct FidoSigningHub {
    pending: Mutex<HashMap<Uuid, oneshot::Sender<FidoSignResponse>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FidoSignResponse {
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request body from the FIDO agent proxy.
#[derive(Debug, Deserialize)]
pub struct FidoRequestBody {
    /// SSH key type (e.g. "sk-ssh-ed25519@openssh.com")
    pub key_type: String,
    /// SHA256 fingerprint of the key
    pub key_fingerprint: String,
    /// What triggered the signing (e.g. "ssh git@github.com")
    pub origin: String,
    /// Remote hostname (if known)
    #[serde(default)]
    pub hostname: Option<String>,
    /// Workspace name for context
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Response returned to the FIDO agent proxy.
#[derive(Serialize)]
pub struct FidoRequestResponse {
    pub request_id: Uuid,
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request body from the iOS app to approve/deny.
#[derive(Debug, Deserialize)]
pub struct FidoRespondBody {
    pub request_id: Uuid,
    pub approved: bool,
}

impl Default for FidoSigningHub {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

impl FidoSigningHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new pending request, returning the request ID and a receiver
    /// that resolves when the iOS app responds.
    pub async fn register(&self) -> (Uuid, oneshot::Receiver<FidoSignResponse>) {
        let id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        (id, rx)
    }

    /// Resolve a pending request (called when the iOS app responds).
    /// Returns true if the request existed and was resolved.
    pub async fn resolve(&self, request_id: Uuid, response: FidoSignResponse) -> bool {
        if let Some(tx) = self.pending.lock().await.remove(&request_id) {
            let _ = tx.send(response);
            true
        } else {
            false
        }
    }

    /// Clean up a request that timed out (remove from pending map).
    pub async fn remove(&self, request_id: Uuid) {
        self.pending.lock().await.remove(&request_id);
    }
}

/// POST /api/fido/request — called by the FIDO agent proxy when an SSH
/// client needs a FIDO signature.  Blocks until the iOS app responds or
/// the 60-second timeout fires.
pub async fn post_fido_request(
    State(state): State<Arc<super::routes::AppState>>,
    Json(body): Json<FidoRequestBody>,
) -> Result<Json<FidoRequestResponse>, StatusCode> {
    let (request_id, rx) = state.fido_hub.register().await;

    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(60);

    tracing::info!(
        request_id = %request_id,
        key_type = %body.key_type,
        key_fingerprint = %body.key_fingerprint,
        origin = %body.origin,
        hostname = ?body.hostname,
        "FIDO signing request received"
    );

    // Broadcast to ALL connected SSE clients so the user sees the approval
    // request regardless of which browser tab / device they have open.
    let sessions = state.control.all_sessions().await;
    for session in &sessions {
        let _ = session.events_tx.send(AgentEvent::FidoSignRequest {
            request_id,
            key_type: body.key_type.clone(),
            key_fingerprint: body.key_fingerprint.clone(),
            origin: body.origin.clone(),
            hostname: body.hostname.clone(),
            workspace: body.workspace.clone(),
            expires_at: expires_at.to_rfc3339(),
        });
    }

    // Wait for response with timeout.
    let response = match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
        Ok(Ok(resp)) => {
            tracing::info!(
                request_id = %request_id,
                approved = resp.approved,
                "FIDO signing request resolved"
            );
            resp
        }
        _ => {
            // Timeout or channel dropped — auto-deny.
            state.fido_hub.remove(request_id).await;
            tracing::warn!(
                request_id = %request_id,
                "FIDO signing request timed out"
            );
            FidoSignResponse {
                approved: false,
                reason: Some("timeout".to_string()),
            }
        }
    };

    Ok(Json(FidoRequestResponse {
        request_id,
        approved: response.approved,
        reason: response.reason,
    }))
}

/// POST /api/fido/respond — called by the iOS app to approve or deny a
/// pending FIDO signing request.
pub async fn post_fido_respond(
    State(state): State<Arc<super::routes::AppState>>,
    Json(body): Json<FidoRespondBody>,
) -> Result<StatusCode, StatusCode> {
    let resolved = state
        .fido_hub
        .resolve(
            body.request_id,
            FidoSignResponse {
                approved: body.approved,
                reason: None,
            },
        )
        .await;

    if resolved {
        tracing::info!(
            request_id = %body.request_id,
            approved = body.approved,
            "FIDO signing request responded"
        );
        Ok(StatusCode::OK)
    } else {
        tracing::warn!(
            request_id = %body.request_id,
            "FIDO respond for unknown or expired request"
        );
        Err(StatusCode::NOT_FOUND)
    }
}
