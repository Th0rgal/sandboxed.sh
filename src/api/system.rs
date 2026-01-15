//! System component management API.
//!
//! Provides endpoints to query and update system components like OpenCode
//! and oh-my-opencode.

use std::pin::Pin;
use std::sync::Arc;

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
use serde::Serialize;
use tokio::process::Command;

use super::routes::AppState;

/// Information about a system component.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentInfo {
    pub name: String,
    pub version: Option<String>,
    pub installed: bool,
    pub update_available: Option<String>,
    pub path: Option<String>,
    pub status: ComponentStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Ok,
    UpdateAvailable,
    NotInstalled,
    Error,
}

/// Response for the system components endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct SystemComponentsResponse {
    pub components: Vec<ComponentInfo>,
}

/// Response for update progress events.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateProgressEvent {
    pub event_type: String, // "log", "progress", "complete", "error"
    pub message: String,
    pub progress: Option<u8>, // 0-100
}

// Type alias for the boxed stream to avoid opaque type mismatch
type UpdateStream = Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>;

/// Create routes for system management.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/components", get(get_components))
        .route("/components/:name/update", post(update_component))
}

/// Get information about all system components.
async fn get_components(State(state): State<Arc<AppState>>) -> Json<SystemComponentsResponse> {
    let mut components = Vec::new();

    // Open Agent (self)
    components.push(ComponentInfo {
        name: "open_agent".to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        installed: true,
        update_available: None, // Would need to check GitHub releases
        path: Some("/usr/local/bin/open_agent".to_string()),
        status: ComponentStatus::Ok,
    });

    // OpenCode
    let opencode_info = get_opencode_info(&state.config).await;
    components.push(opencode_info);

    // oh-my-opencode
    let omo_info = get_oh_my_opencode_info().await;
    components.push(omo_info);

    Json(SystemComponentsResponse { components })
}

/// Get OpenCode version and status.
async fn get_opencode_info(config: &crate::config::Config) -> ComponentInfo {
    // Try to get version from the health endpoint
    let client = reqwest::Client::new();
    let health_url = format!("{}/global/health", config.opencode_base_url);

    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                let version = json
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Check for updates by querying the latest release
                let update_available = check_opencode_update(version.as_deref()).await;
                let status = if update_available.is_some() {
                    ComponentStatus::UpdateAvailable
                } else {
                    ComponentStatus::Ok
                };

                return ComponentInfo {
                    name: "opencode".to_string(),
                    version,
                    installed: true,
                    update_available,
                    path: Some("/usr/local/bin/opencode".to_string()),
                    status,
                };
            }
        }
        _ => {}
    }

    // Fallback: try to run opencode --version
    match Command::new("opencode").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout);
            let version = version_str
                .lines()
                .next()
                .map(|l| l.trim().replace("opencode version ", "").replace("opencode ", ""));

            let update_available = check_opencode_update(version.as_deref()).await;
            let status = if update_available.is_some() {
                ComponentStatus::UpdateAvailable
            } else {
                ComponentStatus::Ok
            };

            ComponentInfo {
                name: "opencode".to_string(),
                version,
                installed: true,
                update_available,
                path: Some("/usr/local/bin/opencode".to_string()),
                status,
            }
        }
        _ => ComponentInfo {
            name: "opencode".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

/// Check if there's a newer version of OpenCode available.
async fn check_opencode_update(current_version: Option<&str>) -> Option<String> {
    let current = current_version?;

    // Fetch latest release from opencode.ai or GitHub
    let client = reqwest::Client::new();

    // Check the anomalyco/opencode GitHub releases (the actual OpenCode source)
    // Note: anthropics/claude-code is a different project
    let resp = client
        .get("https://api.github.com/repos/anomalyco/opencode/releases/latest")
        .header("User-Agent", "open-agent")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let latest = json.get("tag_name")?.as_str()?;
    let latest_version = latest.trim_start_matches('v');

    // Simple version comparison (assumes semver-like format)
    if latest_version != current && version_is_newer(latest_version, current) {
        Some(latest_version.to_string())
    } else {
        None
    }
}

/// Simple semver comparison (newer returns true if a > b).
fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let va = parse(a);
    let vb = parse(b);

    for i in 0..va.len().max(vb.len()) {
        let a_part = va.get(i).copied().unwrap_or(0);
        let b_part = vb.get(i).copied().unwrap_or(0);
        if a_part > b_part {
            return true;
        }
        if a_part < b_part {
            return false;
        }
    }
    false
}

/// Get oh-my-opencode version and status.
async fn get_oh_my_opencode_info() -> ComponentInfo {
    // Check if oh-my-opencode is installed by looking for the config file
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let config_path = format!("{}/.config/opencode/oh-my-opencode.json", home);

    let installed = tokio::fs::metadata(&config_path).await.is_ok();

    if !installed {
        return ComponentInfo {
            name: "oh_my_opencode".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            status: ComponentStatus::NotInstalled,
        };
    }

    // Try to get version from the package
    // oh-my-opencode doesn't have a --version flag, so we check npm/bun
    let version = get_oh_my_opencode_version().await;
    let update_available = check_oh_my_opencode_update(version.as_deref()).await;
    let status = if update_available.is_some() {
        ComponentStatus::UpdateAvailable
    } else {
        ComponentStatus::Ok
    };

    ComponentInfo {
        name: "oh_my_opencode".to_string(),
        version,
        installed: true,
        update_available,
        path: Some(config_path),
        status,
    }
}

/// Get the installed version of oh-my-opencode.
async fn get_oh_my_opencode_version() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    // First, try to find the version from bun's cache (most reliable for the actual installed version)
    // Run: find ~/.bun -name 'package.json' -path '*oh-my-opencode*' and parse version
    let output = Command::new("bash")
        .args([
            "-c",
            &format!(
                "find {}/.bun -name 'package.json' -path '*oh-my-opencode*' 2>/dev/null | head -1 | xargs cat 2>/dev/null",
                home
            ),
        ])
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let content = String::from_utf8_lossy(&output.stdout);
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(version) = json.get("version").and_then(|v| v.as_str()) {
                return Some(version.to_string());
            }
        }
    }

    // Fallback: try running bunx to check the version (may be buggy in some versions)
    let output = Command::new("bunx")
        .args(["oh-my-opencode", "--version"])
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let version_str = String::from_utf8_lossy(&output.stdout);
        return version_str
            .lines()
            .next()
            .map(|l| l.trim().to_string());
    }

    None
}

/// Check if there's a newer version of oh-my-opencode available.
async fn check_oh_my_opencode_update(current_version: Option<&str>) -> Option<String> {
    // Query npm registry for latest version
    let client = reqwest::Client::new();
    let resp = client
        .get("https://registry.npmjs.org/oh-my-opencode/latest")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let latest = json.get("version")?.as_str()?;

    match current_version {
        Some(current) if latest != current && version_is_newer(latest, current) => {
            Some(latest.to_string())
        }
        None => Some(latest.to_string()), // If no current version, suggest the latest
        _ => None,
    }
}

/// Update a system component.
async fn update_component(
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    match name.as_str() {
        "opencode" => Ok(Sse::new(Box::pin(stream_opencode_update()))),
        "oh_my_opencode" => Ok(Sse::new(Box::pin(stream_oh_my_opencode_update()))),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", name),
        )),
    }
}

/// Stream the OpenCode update process.
fn stream_opencode_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        // Send initial progress
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Starting OpenCode update...".to_string(),
            progress: Some(0),
        }).unwrap()));

        // Download and install OpenCode
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Downloading latest OpenCode release...".to_string(),
            progress: Some(10),
        }).unwrap()));

        // Run the install script
        let install_result = Command::new("bash")
            .args(["-c", "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path"])
            .output()
            .await;

        match install_result {
            Ok(output) if output.status.success() => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "log".to_string(),
                    message: "Download complete, installing...".to_string(),
                    progress: Some(50),
                }).unwrap()));

                // Copy to /usr/local/bin
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                let install_result = Command::new("install")
                    .args(["-m", "0755", &format!("{}/.opencode/bin/opencode", home), "/usr/local/bin/opencode"])
                    .output()
                    .await;

                match install_result {
                    Ok(output) if output.status.success() => {
                        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                            event_type: "log".to_string(),
                            message: "Binary installed, restarting service...".to_string(),
                            progress: Some(80),
                        }).unwrap()));

                        // Restart the opencode service
                        let restart_result = Command::new("systemctl")
                            .args(["restart", "opencode.service"])
                            .output()
                            .await;

                        match restart_result {
                            Ok(output) if output.status.success() => {
                                // Wait a moment for the service to start
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

                                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                                    event_type: "complete".to_string(),
                                    message: "OpenCode updated successfully!".to_string(),
                                    progress: Some(100),
                                }).unwrap()));
                            }
                            Ok(output) => {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                                    event_type: "error".to_string(),
                                    message: format!("Failed to restart service: {}", stderr),
                                    progress: None,
                                }).unwrap()));
                            }
                            Err(e) => {
                                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                                    event_type: "error".to_string(),
                                    message: format!("Failed to restart service: {}", e),
                                    progress: None,
                                }).unwrap()));
                            }
                        }
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                            event_type: "error".to_string(),
                            message: format!("Failed to install binary: {}", stderr),
                            progress: None,
                        }).unwrap()));
                    }
                    Err(e) => {
                        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                            event_type: "error".to_string(),
                            message: format!("Failed to install binary: {}", e),
                            progress: None,
                        }).unwrap()));
                    }
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to download OpenCode: {}", stderr),
                    progress: None,
                }).unwrap()));
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to run install script: {}", e),
                    progress: None,
                }).unwrap()));
            }
        }
    }
}

/// Stream the oh-my-opencode update process.
fn stream_oh_my_opencode_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Starting oh-my-opencode update...".to_string(),
            progress: Some(0),
        }).unwrap()));

        // First, clear the bun cache for oh-my-opencode to force fetching the latest version
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Clearing package cache...".to_string(),
            progress: Some(10),
        }).unwrap()));

        // Clear bun cache to force re-download
        let _ = Command::new("bun")
            .args(["pm", "cache", "rm"])
            .output()
            .await;

        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Running bunx oh-my-opencode@latest install...".to_string(),
            progress: Some(20),
        }).unwrap()));

        // Run the install command with @latest to force the newest version
        // Enable all providers by default for updates
        let install_result = Command::new("bunx")
            .args([
                "oh-my-opencode@latest",
                "install",
                "--no-tui",
                "--claude=yes",
                "--chatgpt=yes",
                "--gemini=yes",
            ])
            .output()
            .await;

        match install_result {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "log".to_string(),
                    message: format!("Installation output: {}", stdout.lines().take(5).collect::<Vec<_>>().join("\n")),
                    progress: Some(80),
                }).unwrap()));

                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "complete".to_string(),
                    message: "oh-my-opencode updated successfully!".to_string(),
                    progress: Some(100),
                }).unwrap()));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to update oh-my-opencode: {} {}", stderr, stdout),
                    progress: None,
                }).unwrap()));
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to run update: {}", e),
                    progress: None,
                }).unwrap()));
            }
        }
    }
}
