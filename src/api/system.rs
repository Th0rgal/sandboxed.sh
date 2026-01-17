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

/// Default repo path for Open Agent source
const OPEN_AGENT_REPO_PATH: &str = "/opt/open_agent/vaduz-v1";

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

/// Information about an installed OpenCode plugin.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledPluginInfo {
    /// Plugin package name (e.g., "opencode-gemini-auth")
    pub package: String,
    /// Full spec from config (e.g., "opencode-gemini-auth@latest")
    pub spec: String,
    /// Currently installed version (if detectable)
    pub installed_version: Option<String>,
    /// Latest version available on npm
    pub latest_version: Option<String>,
    /// Whether an update is available
    pub update_available: bool,
}

/// Response for installed plugins endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledPluginsResponse {
    pub plugins: Vec<InstalledPluginInfo>,
}

// Type alias for the boxed stream to avoid opaque type mismatch
type UpdateStream = Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>;

/// Create routes for system management.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/components", get(get_components))
        .route("/components/:name/update", post(update_component))
        .route("/plugins/installed", get(get_installed_plugins))
        .route("/plugins/:package/update", post(update_plugin))
}

/// Get information about all system components.
async fn get_components(State(state): State<Arc<AppState>>) -> Json<SystemComponentsResponse> {
    let mut components = Vec::new();

    // Open Agent (self)
    let current_version = env!("CARGO_PKG_VERSION");
    let update_available = check_open_agent_update(Some(current_version)).await;
    let status = if update_available.is_some() {
        ComponentStatus::UpdateAvailable
    } else {
        ComponentStatus::Ok
    };
    components.push(ComponentInfo {
        name: "open_agent".to_string(),
        version: Some(current_version.to_string()),
        installed: true,
        update_available,
        path: Some("/usr/local/bin/open_agent".to_string()),
        status,
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

/// Check if there's a newer version of Open Agent available.
/// First checks GitHub releases, then falls back to git tags if no releases exist.
async fn check_open_agent_update(current_version: Option<&str>) -> Option<String> {
    let current = current_version?;

    // First, try GitHub releases API
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.github.com/repos/Th0rgal/openagent/releases/latest")
        .header("User-Agent", "open-agent")
        .send()
        .await
        .ok();

    if let Some(resp) = resp {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(latest) = json.get("tag_name").and_then(|t| t.as_str()) {
                    let latest_version = latest.trim_start_matches('v');
                    if latest_version != current && version_is_newer(latest_version, current) {
                        return Some(latest_version.to_string());
                    }
                }
            }
        }
    }

    // Fallback: check git tags from the repo if it exists
    let repo_path = std::path::Path::new(OPEN_AGENT_REPO_PATH);
    if !repo_path.exists() {
        return None;
    }

    // Fetch tags first
    let _ = Command::new("git")
        .args(["fetch", "--tags", "origin"])
        .current_dir(OPEN_AGENT_REPO_PATH)
        .output()
        .await;

    // Get the latest tag
    let tag_result = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0", "origin/master"])
        .current_dir(OPEN_AGENT_REPO_PATH)
        .output()
        .await
        .ok()?;

    if !tag_result.status.success() {
        return None;
    }

    let latest_tag = String::from_utf8_lossy(&tag_result.stdout).trim().to_string();
    let latest_version = latest_tag.trim_start_matches('v');

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
        "open_agent" => Ok(Sse::new(Box::pin(stream_open_agent_update()))),
        "opencode" => Ok(Sse::new(Box::pin(stream_opencode_update()))),
        "oh_my_opencode" => Ok(Sse::new(Box::pin(stream_oh_my_opencode_update()))),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", name),
        )),
    }
}

/// Stream the Open Agent update process.
/// Builds from source using git tags (no pre-built binaries needed).
fn stream_open_agent_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Starting Open Agent update...".to_string(),
            progress: Some(0),
        }).unwrap()));

        // Check if source repo exists
        let repo_path = std::path::Path::new(OPEN_AGENT_REPO_PATH);
        if !repo_path.exists() {
            yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                event_type: "error".to_string(),
                message: format!("Source repo not found at {}. Clone the repo first (see INSTALL.md).", OPEN_AGENT_REPO_PATH),
                progress: None,
            }).unwrap()));
            return;
        }

        // Fetch latest from git
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Fetching latest changes from git...".to_string(),
            progress: Some(5),
        }).unwrap()));

        let fetch_result = Command::new("git")
            .args(["fetch", "--tags", "origin"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        match fetch_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to fetch: {}", stderr),
                    progress: None,
                }).unwrap()));
                return;
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to run git fetch: {}", e),
                    progress: None,
                }).unwrap()));
                return;
            }
        }

        // Get the latest tag
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Finding latest release tag...".to_string(),
            progress: Some(10),
        }).unwrap()));

        let tag_result = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0", "origin/master"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        let latest_tag = match tag_result {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                // No tags found, use origin/master
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "log".to_string(),
                    message: "No release tags found, using origin/master...".to_string(),
                    progress: Some(12),
                }).unwrap()));
                "origin/master".to_string()
            }
        };

        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: format!("Checking out {}...", latest_tag),
            progress: Some(15),
        }).unwrap()));

        // Reset any local changes before checkout to prevent conflicts
        let _ = Command::new("git")
            .args(["reset", "--hard", "HEAD"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        // Clean untracked files that might interfere
        let _ = Command::new("git")
            .args(["clean", "-fd"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        // Checkout the tag/branch
        let checkout_result = Command::new("git")
            .args(["checkout", &latest_tag])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        match checkout_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to checkout: {}", stderr),
                    progress: None,
                }).unwrap()));
                return;
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to run git checkout: {}", e),
                    progress: None,
                }).unwrap()));
                return;
            }
        }

        // If using origin/master, pull latest
        if latest_tag == "origin/master" {
            let pull_result = Command::new("git")
                .args(["pull", "origin", "master"])
                .current_dir(OPEN_AGENT_REPO_PATH)
                .output()
                .await;

            if let Ok(output) = pull_result {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                        event_type: "log".to_string(),
                        message: format!("Warning: git pull failed: {}", stderr),
                        progress: Some(18),
                    }).unwrap()));
                }
            }
        }

        // Build the project
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Building Open Agent (this may take a few minutes)...".to_string(),
            progress: Some(20),
        }).unwrap()));

        // Source cargo env and build
        let build_result = Command::new("bash")
            .args(["-c", "source /root/.cargo/env && cargo build --bin open_agent --bin workspace-mcp --bin desktop-mcp"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        match build_result {
            Ok(output) if output.status.success() => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "log".to_string(),
                    message: "Build complete".to_string(),
                    progress: Some(70),
                }).unwrap()));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Show last few lines of error
                let last_lines: Vec<&str> = stderr.lines().rev().take(10).collect();
                let error_summary = last_lines.into_iter().rev().collect::<Vec<_>>().join("\n");
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Build failed:\n{}", error_summary),
                    progress: None,
                }).unwrap()));
                return;
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to run cargo build: {}", e),
                    progress: None,
                }).unwrap()));
                return;
            }
        }

        // Install binaries
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Installing binaries...".to_string(),
            progress: Some(75),
        }).unwrap()));

        let binaries = [
            ("open_agent", "/usr/local/bin/open_agent"),
            ("workspace-mcp", "/usr/local/bin/workspace-mcp"),
            ("desktop-mcp", "/usr/local/bin/desktop-mcp"),
        ];

        for (name, dest) in binaries {
            let src = format!("{}/target/debug/{}", OPEN_AGENT_REPO_PATH, name);
            let install_result = Command::new("install")
                .args(["-m", "0755", &src, dest])
                .output()
                .await;

            match install_result {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                        event_type: "error".to_string(),
                        message: format!("Failed to install {}: {}", name, stderr),
                        progress: None,
                    }).unwrap()));
                    return;
                }
                Err(e) => {
                    yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                        event_type: "error".to_string(),
                        message: format!("Failed to install {}: {}", name, e),
                        progress: None,
                    }).unwrap()));
                    return;
                }
            }
        }

        // Send restart event before restarting - the SSE connection will drop when the
        // service restarts since this process will be terminated by systemctl. The client
        // should detect the connection drop at progress 100% and treat it as success.
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "restarting".to_string(),
            message: format!("Binaries installed, restarting service to complete update to {}...", latest_tag),
            progress: Some(100),
        }).unwrap()));

        // Small delay to ensure the SSE event is flushed before we restart
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Restart the service - this will terminate our process, so no code after this
        // will execute. The client should poll /api/health to confirm the new version.
        let _ = Command::new("systemctl")
            .args(["restart", "open_agent.service"])
            .output()
            .await;
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

/// Get all installed OpenCode plugins with version information.
async fn get_installed_plugins() -> Json<InstalledPluginsResponse> {
    let plugin_specs = crate::opencode_config::get_installed_plugins().await;

    let mut plugins = Vec::new();

    for spec in plugin_specs {
        let (package, _pinned_version) = split_package_spec(&spec);

        // Get installed version from bun cache
        let installed_version = get_plugin_installed_version(&package).await;

        // Get latest version from npm
        let latest_version = get_plugin_latest_version(&package).await;

        // Determine if update is available
        let update_available = match (&installed_version, &latest_version) {
            (Some(installed), Some(latest)) => {
                installed != latest && version_is_newer(latest, installed)
            }
            (None, Some(_)) => true, // Not installed locally but available
            _ => false,
        };

        plugins.push(InstalledPluginInfo {
            package: package.clone(),
            spec: spec.clone(),
            installed_version,
            latest_version,
            update_available,
        });
    }

    Json(InstalledPluginsResponse { plugins })
}

/// Split a package spec into (name, version).
/// E.g., "opencode-gemini-auth@1.2.3" -> ("opencode-gemini-auth", Some("1.2.3"))
fn split_package_spec(spec: &str) -> (String, Option<String>) {
    // Handle scoped packages like @scope/name@version
    if spec.starts_with('@') {
        if let Some(slash_idx) = spec.find('/') {
            let after_scope = &spec[slash_idx + 1..];
            if let Some(at_idx) = after_scope.find('@') {
                let name = format!("{}/{}", &spec[..slash_idx], &after_scope[..at_idx]);
                let version = &after_scope[at_idx + 1..];
                if !version.is_empty() && version != "latest" {
                    return (name, Some(version.to_string()));
                }
                return (name, None);
            }
        }
        return (spec.to_string(), None);
    }

    // Non-scoped package
    if let Some(at_idx) = spec.find('@') {
        let name = &spec[..at_idx];
        let version = &spec[at_idx + 1..];
        if !version.is_empty() && version != "latest" {
            return (name.to_string(), Some(version.to_string()));
        }
        return (name.to_string(), None);
    }

    (spec.to_string(), None)
}

/// Get the installed version of a plugin from bun cache.
async fn get_plugin_installed_version(package: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    // Search bun cache for the package
    let output = Command::new("bash")
        .args([
            "-c",
            &format!(
                "find {}/.bun/install/cache -maxdepth 1 -name '{}@*' 2>/dev/null | sort -V | tail -1",
                home,
                package.replace('/', "-") // Handle scoped packages in filesystem
            ),
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }

    // Extract version from directory name (e.g., "opencode-gemini-auth@1.3.7@@@1" -> "1.3.7")
    let dirname = std::path::Path::new(&path).file_name()?.to_str()?;

    // Find the version part between first @ and second @
    let after_name = dirname.strip_prefix(&format!("{}@", package.replace('/', "-")))?;
    let version = after_name.split('@').next()?;

    Some(version.to_string())
}

/// Get the latest version of a plugin from npm registry.
async fn get_plugin_latest_version(package: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let url = format!("https://registry.npmjs.org/{}/latest", package);

    let resp = client.get(&url).send().await.ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
}

/// Update a plugin to the latest version.
async fn update_plugin(
    Path(package): Path<String>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    Ok(Sse::new(Box::pin(stream_plugin_update(package))))
}

/// Stream the plugin update process.
fn stream_plugin_update(package: String) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: format!("Starting {} update...", package),
            progress: Some(0),
        }).unwrap()));

        // Clear bun cache for this package
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: "Clearing package cache...".to_string(),
            progress: Some(10),
        }).unwrap()));

        let _ = Command::new("bun")
            .args(["pm", "cache", "rm"])
            .output()
            .await;

        // Update the plugin by reinstalling with @latest
        yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
            event_type: "log".to_string(),
            message: format!("Installing {}@latest...", package),
            progress: Some(30),
        }).unwrap()));

        // Use bunx to trigger the plugin install (which will cache the latest version)
        let install_result = Command::new("bunx")
            .args([&format!("{}@latest", package), "--help"])
            .output()
            .await;

        match install_result {
            Ok(_) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "log".to_string(),
                    message: "Package downloaded, updating config...".to_string(),
                    progress: Some(70),
                }).unwrap()));

                // Update the opencode config to use @latest
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                let config_path = format!("{}/.config/opencode/opencode.json", home);

                if let Ok(contents) = tokio::fs::read_to_string(&config_path).await {
                    if let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&contents) {
                        if let Some(plugins) = root.get_mut("plugin").and_then(|v| v.as_array_mut()) {
                            // Update the plugin spec to @latest
                            for p in plugins.iter_mut() {
                                if let Some(s) = p.as_str() {
                                    if s.starts_with(&package) {
                                        *p = serde_json::json!(format!("{}@latest", package));
                                    }
                                }
                            }

                            if let Ok(payload) = serde_json::to_string_pretty(&root) {
                                let _ = tokio::fs::write(&config_path, payload).await;
                            }
                        }
                    }
                }

                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "complete".to_string(),
                    message: format!("{} updated successfully!", package),
                    progress: Some(100),
                }).unwrap()));
            }
            Err(e) => {
                yield Ok(Event::default().data(serde_json::to_string(&UpdateProgressEvent {
                    event_type: "error".to_string(),
                    message: format!("Failed to update {}: {}", package, e),
                    progress: None,
                }).unwrap()));
            }
        }
    }
}
