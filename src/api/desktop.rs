//! Desktop session management API.
//!
//! Provides endpoints for listing, closing, and managing desktop sessions.
//! Also includes background cleanup of orphaned sessions.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use uuid::Uuid;

use super::library::SharedLibrary;
use super::routes::AppState;

/// Status of a desktop session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSessionStatus {
    /// Session is running and owned by an active mission.
    Active,
    /// Session is running but the owning mission has completed.
    Orphaned,
    /// Session has been stopped.
    Stopped,
    /// Session status is unknown (process detection failed).
    Unknown,
}

/// Extended desktop session information for the API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionDetail {
    pub display: String,
    pub status: DesktopSessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_status: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_until: Option<String>,
    /// Seconds until auto-close (if orphaned and grace period applies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_in_secs: Option<i64>,
    /// Whether the Xvfb process is actually running.
    pub process_running: bool,
}

/// Response for listing desktop sessions.
#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    pub sessions: Vec<DesktopSessionDetail>,
}

/// Request to extend keep-alive.
#[derive(Debug, Deserialize)]
pub struct KeepAliveRequest {
    /// Additional seconds to extend the keep-alive (default: 7200 = 2 hours).
    #[serde(default = "default_keep_alive_extension")]
    pub extension_secs: u64,
}

fn default_keep_alive_extension() -> u64 {
    7200 // 2 hours
}

/// Response for close/keep-alive operations.
#[derive(Debug, Serialize)]
pub struct OperationResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Create desktop management routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/:display/close", post(close_session))
        .route("/sessions/:display/keep-alive", post(keep_alive_session))
        .route("/sessions/cleanup", post(cleanup_orphaned_sessions))
}

/// List all desktop sessions across all missions.
async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<ListSessionsResponse> {
    let sessions = collect_desktop_sessions(&state).await;
    Json(ListSessionsResponse { sessions })
}

/// Close a specific desktop session.
async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(display_id): Path<String>,
) -> Result<Json<OperationResponse>, (StatusCode, String)> {
    // Normalize display format
    let display_id = if display_id.starts_with(':') {
        display_id
    } else {
        format!(":{}", display_id)
    };

    // Try to close the desktop session
    match close_desktop_session(&display_id, &state.config.working_dir).await {
        Ok(()) => {
            tracing::info!(display_id = %display_id, "Desktop session closed via API");
            Ok(Json(OperationResponse {
                success: true,
                message: Some(format!("Desktop session {} closed", display_id)),
            }))
        }
        Err(e) => {
            tracing::warn!(display_id = %display_id, error = %e, "Failed to close desktop session");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to close desktop session: {}", e),
            ))
        }
    }
}

/// Extend the keep-alive for a desktop session.
async fn keep_alive_session(
    State(state): State<Arc<AppState>>,
    Path(display_id): Path<String>,
    Json(req): Json<KeepAliveRequest>,
) -> Result<Json<OperationResponse>, (StatusCode, String)> {
    // Normalize display format
    let display_id = if display_id.starts_with(':') {
        display_id
    } else {
        format!(":{}", display_id)
    };

    // Find and update the session
    let mission_store = state.control.get_mission_store().await;
    let missions = mission_store.list_missions(100, 0).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list missions: {}", e),
        )
    })?;

    // Find the mission that owns this display
    for mission in missions {
        for session in &mission.desktop_sessions {
            if session.display == display_id {
                // Calculate new keep-alive time
                let new_keep_alive = Utc::now() + chrono::Duration::seconds(req.extension_secs as i64);
                let new_keep_alive_str = new_keep_alive.to_rfc3339();

                // Update the session
                let mut updated_sessions = mission.desktop_sessions.clone();
                for s in &mut updated_sessions {
                    if s.display == display_id {
                        s.keep_alive_until = Some(new_keep_alive_str.clone());
                    }
                }

                if let Err(e) = mission_store
                    .update_mission_desktop_sessions(mission.id, &updated_sessions)
                    .await
                {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to update session: {}", e),
                    ));
                }

                tracing::info!(
                    display_id = %display_id,
                    mission_id = %mission.id,
                    keep_alive_until = %new_keep_alive_str,
                    "Desktop session keep-alive extended"
                );

                return Ok(Json(OperationResponse {
                    success: true,
                    message: Some(format!(
                        "Keep-alive extended to {}",
                        new_keep_alive_str
                    )),
                }));
            }
        }
    }

    Err((
        StatusCode::NOT_FOUND,
        format!("Desktop session {} not found", display_id),
    ))
}

/// Close all orphaned desktop sessions.
async fn cleanup_orphaned_sessions(
    State(state): State<Arc<AppState>>,
) -> Json<OperationResponse> {
    let sessions = collect_desktop_sessions(&state).await;
    let mut closed_count = 0;
    let mut failed_count = 0;

    for session in sessions {
        if session.status == DesktopSessionStatus::Orphaned && session.process_running {
            // Check if keep-alive is active
            if let Some(keep_alive_until) = &session.keep_alive_until {
                if let Ok(keep_until) = DateTime::parse_from_rfc3339(keep_alive_until) {
                    if keep_until > Utc::now() {
                        // Skip - keep-alive is active
                        continue;
                    }
                }
            }

            // Close this orphaned session
            if close_desktop_session(&session.display, &state.config.working_dir)
                .await
                .is_ok()
            {
                closed_count += 1;
            } else {
                failed_count += 1;
            }
        }
    }

    tracing::info!(
        closed = closed_count,
        failed = failed_count,
        "Orphaned desktop sessions cleanup complete"
    );

    Json(OperationResponse {
        success: failed_count == 0,
        message: Some(format!(
            "Closed {} orphaned sessions{}",
            closed_count,
            if failed_count > 0 {
                format!(", {} failed", failed_count)
            } else {
                String::new()
            }
        )),
    })
}

/// Collect all desktop sessions from all missions with status information.
async fn collect_desktop_sessions(state: &Arc<AppState>) -> Vec<DesktopSessionDetail> {
    let mut sessions = Vec::new();

    // Get desktop config for grace period
    let grace_period_secs = get_desktop_config(&state.library).await.auto_close_grace_period_secs;

    // Get all missions from the store
    let mission_store = state.control.get_mission_store().await;
    let missions = match mission_store.list_missions(1000, 0).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Failed to list missions for desktop sessions: {}", e);
            return sessions;
        }
    };

    // Collect sessions from missions
    for mission in missions {
        for session in &mission.desktop_sessions {
            let process_running = is_xvfb_running(&session.display).await;

            // Determine session status
            let status = if session.stopped_at.is_some() {
                DesktopSessionStatus::Stopped
            } else if !process_running {
                DesktopSessionStatus::Stopped
            } else {
                // Check if mission is still active
                let mission_active = matches!(
                    mission.status,
                    super::control::MissionStatus::Active
                );

                if mission_active {
                    DesktopSessionStatus::Active
                } else {
                    DesktopSessionStatus::Orphaned
                }
            };

            // Calculate auto-close countdown for orphaned sessions
            let auto_close_in_secs = if status == DesktopSessionStatus::Orphaned && grace_period_secs > 0 {
                // Check if keep-alive is active
                if let Some(keep_alive_until) = &session.keep_alive_until {
                    if let Ok(keep_until) = DateTime::parse_from_rfc3339(keep_alive_until) {
                        let secs_until = (keep_until.timestamp() - Utc::now().timestamp()).max(0);
                        if secs_until > 0 {
                            Some(secs_until)
                        } else {
                            // Keep-alive expired, use grace period from mission completion
                            calculate_auto_close_secs(&mission, grace_period_secs)
                        }
                    } else {
                        calculate_auto_close_secs(&mission, grace_period_secs)
                    }
                } else {
                    calculate_auto_close_secs(&mission, grace_period_secs)
                }
            } else {
                None
            };

            sessions.push(DesktopSessionDetail {
                display: session.display.clone(),
                status,
                mission_id: session.mission_id.or(Some(mission.id)),
                mission_title: mission.title.clone(),
                mission_status: Some(format!("{:?}", mission.status)),
                started_at: session.started_at.clone(),
                stopped_at: session.stopped_at.clone(),
                keep_alive_until: session.keep_alive_until.clone(),
                auto_close_in_secs,
                process_running,
            });
        }
    }

    // Also scan for any running Xvfb processes that might not be tracked in missions
    let running_displays = get_running_xvfb_displays().await;
    for display in running_displays {
        // Check if this display is already in our list
        if !sessions.iter().any(|s| s.display == display) {
            sessions.push(DesktopSessionDetail {
                display: display.clone(),
                status: DesktopSessionStatus::Unknown,
                mission_id: None,
                mission_title: None,
                mission_status: None,
                started_at: "unknown".to_string(),
                stopped_at: None,
                keep_alive_until: None,
                auto_close_in_secs: None,
                process_running: true,
            });
        }
    }

    sessions
}

/// Calculate seconds until auto-close based on mission completion time.
fn calculate_auto_close_secs(
    mission: &super::mission_store::Mission,
    grace_period_secs: u64,
) -> Option<i64> {
    // Try to get mission completion time from updated_at
    if let Ok(updated_at) = DateTime::parse_from_rfc3339(&mission.updated_at) {
        let grace_end = updated_at + chrono::Duration::seconds(grace_period_secs as i64);
        let secs_remaining = (grace_end.timestamp() - Utc::now().timestamp()).max(0);
        Some(secs_remaining)
    } else {
        None
    }
}

/// Get desktop config from library.
async fn get_desktop_config(library: &SharedLibrary) -> crate::library::types::DesktopConfig {
    let guard = library.read().await;
    if let Some(lib) = guard.as_ref() {
        match lib.get_openagent_config().await {
            Ok(config) => config.desktop,
            Err(_) => crate::library::types::DesktopConfig::default(),
        }
    } else {
        crate::library::types::DesktopConfig::default()
    }
}

/// Check if Xvfb is running on a specific display.
async fn is_xvfb_running(display: &str) -> bool {
    let output = Command::new("pgrep")
        .args(["-f", &format!("Xvfb {}", display)])
        .output()
        .await;

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Get list of running Xvfb displays.
async fn get_running_xvfb_displays() -> Vec<String> {
    let output = Command::new("pgrep")
        .args(["-a", "Xvfb"])
        .output()
        .await;

    let mut displays = Vec::new();

    if let Ok(o) = output {
        let stdout = String::from_utf8_lossy(&o.stdout);
        for line in stdout.lines() {
            // Parse lines like "12345 Xvfb :99 -screen 0 1280x720x24"
            if let Some(pos) = line.find(':') {
                let rest = &line[pos..];
                if let Some(space_pos) = rest.find(' ') {
                    displays.push(rest[..space_pos].to_string());
                } else {
                    displays.push(rest.to_string());
                }
            }
        }
    }

    displays
}

/// Close a desktop session by killing its processes.
async fn close_desktop_session(
    display: &str,
    working_dir: &std::path::Path,
) -> anyhow::Result<()> {
    // Extract display number
    let display_num: u32 = display
        .trim_start_matches(':')
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid display format: {}", display))?;

    // Try to read session file for PIDs
    let session_file = working_dir.join(format!(".desktop_session_{}", display_num));

    if session_file.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&session_file).await {
            if let Ok(session_info) = serde_json::from_str::<serde_json::Value>(&content) {
                // Kill processes by PID
                for pid_key in ["xvfb_pid", "i3_pid", "browser_pid"] {
                    if let Some(pid) = session_info[pid_key].as_u64() {
                        let pid = pid as i32;
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                    }
                }
            }
        }
        let _ = tokio::fs::remove_file(&session_file).await;
    }

    // Also kill by display pattern (fallback)
    let _ = Command::new("pkill")
        .args(["-f", &format!("Xvfb {}", display)])
        .output()
        .await;

    // Clean up lock files
    let lock_file = format!("/tmp/.X{}-lock", display_num);
    let socket_file = format!("/tmp/.X11-unix/X{}", display_num);
    let _ = tokio::fs::remove_file(&lock_file).await;
    let _ = tokio::fs::remove_file(&socket_file).await;

    Ok(())
}

/// Background task that periodically cleans up orphaned desktop sessions.
pub async fn start_cleanup_task(
    state: Arc<AppState>,
) {
    tracing::info!("Starting desktop session cleanup background task");

    loop {
        // Get config for intervals
        let config = get_desktop_config(&state.library).await;
        let interval_secs = config.cleanup_interval_secs;
        let grace_period_secs = config.auto_close_grace_period_secs;
        let warning_secs = config.warning_before_close_secs;

        // Skip if auto-close is disabled
        if grace_period_secs == 0 {
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            continue;
        }

        // Collect and process sessions
        let sessions = collect_desktop_sessions(&state).await;

        for session in sessions {
            if session.status != DesktopSessionStatus::Orphaned || !session.process_running {
                continue;
            }

            // Check if keep-alive is active
            if let Some(keep_alive_until) = &session.keep_alive_until {
                if let Ok(keep_until) = DateTime::parse_from_rfc3339(keep_alive_until) {
                    if keep_until > Utc::now() {
                        // Keep-alive is active, skip
                        continue;
                    }
                }
            }

            // Check auto-close countdown
            if let Some(secs_remaining) = session.auto_close_in_secs {
                if secs_remaining <= 0 {
                    // Time to close
                    tracing::info!(
                        display_id = %session.display,
                        mission_id = ?session.mission_id,
                        "Auto-closing orphaned desktop session"
                    );
                    let _ = close_desktop_session(&session.display, &state.config.working_dir).await;
                } else if warning_secs > 0 && secs_remaining <= warning_secs as i64 {
                    // Send warning notification via SSE
                    // (This would be implemented through the control hub's SSE broadcast)
                    tracing::debug!(
                        display_id = %session.display,
                        secs_remaining = secs_remaining,
                        "Desktop session will auto-close soon"
                    );
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}
