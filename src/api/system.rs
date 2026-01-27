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

/// Build a single SSE event carrying an [`UpdateProgressEvent`] payload.
///
/// Used by all `stream_*_update()` functions to avoid repeating the
/// `Event::default().data(serde_json::to_string(...).unwrap())` boilerplate.
fn sse(
    event_type: &str,
    message: impl Into<String>,
    progress: Option<u8>,
) -> Result<Event, std::convert::Infallible> {
    Ok(Event::default().data(
        serde_json::to_string(&UpdateProgressEvent {
            event_type: event_type.to_string(),
            message: message.into(),
            progress,
        })
        .unwrap(),
    ))
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

    // Claude Code
    let claudecode_info = get_claude_code_info().await;
    components.push(claudecode_info);

    // Amp
    let amp_info = get_amp_info().await;
    components.push(amp_info);

    // oh-my-opencode
    let omo_info = get_oh_my_opencode_info().await;
    components.push(omo_info);

    Json(SystemComponentsResponse { components })
}

/// Get OpenCode version and status.
/// Note: No central server check - missions use per-workspace CLI execution.
async fn get_opencode_info(_config: &crate::config::Config) -> ComponentInfo {
    // Check CLI availability (per-workspace execution doesn't need a central server)
    match Command::new("opencode").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let mut version_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !version_str.is_empty() {
                    version_str.push(' ');
                }
                version_str.push_str(stderr.trim());
            }
            let version = version_str.lines().next().map(|l| {
                l.trim()
                    .replace("opencode version ", "")
                    .replace("opencode ", "")
            });

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
                path: which_opencode().await,
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

/// Get Claude Code version and status.
async fn get_claude_code_info() -> ComponentInfo {
    // Try to run claude --version to check if it's installed
    match Command::new("claude").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let mut version_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !version_str.is_empty() {
                    version_str.push(' ');
                }
                version_str.push_str(stderr.trim());
            }
            // Parse version from output like:
            // - "claude 2.1.12 (Code)"
            // - "Claude Code v2.1.12"
            let version = extract_version_token(&version_str);

            let update_available = check_claude_code_update(version.as_deref()).await;
            let status = if update_available.is_some() {
                ComponentStatus::UpdateAvailable
            } else {
                ComponentStatus::Ok
            };

            ComponentInfo {
                name: "claude_code".to_string(),
                version,
                installed: true,
                update_available,
                path: which_claude_code().await,
                status,
            }
        }
        _ => ComponentInfo {
            name: "claude_code".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

/// Find the path to a CLI binary.
/// Checks `which` first (respects the user's PATH), then explicit fallback paths.
async fn which_binary(name: &str, fallback_paths: &[&str]) -> Option<String> {
    if let Ok(output) = Command::new("which").arg(name).output().await {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    for path in fallback_paths {
        if std::path::Path::new(path).exists() {
            return Some((*path).to_string());
        }
    }
    None
}

/// Find the path to the Claude Code binary.
async fn which_claude_code() -> Option<String> {
    which_binary("claude", &[]).await
}

/// Find the path to the OpenCode binary.
/// Checks PATH first, then user-local install, then system-wide.
async fn which_opencode() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let user_local = format!("{}/.opencode/bin/opencode", home);
    which_binary("opencode", &[&user_local, "/usr/local/bin/opencode"]).await
}

/// Get Amp version and status.
async fn get_amp_info() -> ComponentInfo {
    // Try to run amp --version to check if it's installed
    match Command::new("amp").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let mut version_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !version_str.is_empty() {
                    version_str.push(' ');
                }
                version_str.push_str(stderr.trim());
            }
            // Parse version from output like "amp version 0.1.0" or "0.1.0"
            let version = extract_version_token(&version_str);

            let update_available = check_amp_update(version.as_deref()).await;
            let status = if update_available.is_some() {
                ComponentStatus::UpdateAvailable
            } else {
                ComponentStatus::Ok
            };

            ComponentInfo {
                name: "amp".to_string(),
                version,
                installed: true,
                update_available,
                path: which_amp().await,
                status,
            }
        }
        _ => ComponentInfo {
            name: "amp".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

/// Find the path to the Amp binary.
async fn which_amp() -> Option<String> {
    which_binary("amp", &[]).await
}

/// Check if there's a newer version of Amp available.
async fn check_amp_update(current_version: Option<&str>) -> Option<String> {
    let current_raw = current_version?;
    let current = extract_version_token(current_raw)?;

    // Check npm registry for @anthropic-ai/amp (note: actual package name may differ)
    let client = reqwest::Client::new();
    let resp = client
        .get("https://registry.npmjs.org/@anthropic-ai/amp/latest")
        .header("User-Agent", "open-agent")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let latest = json.get("version")?.as_str()?;

    if version_is_newer(latest, &current) {
        Some(latest.to_string())
    } else {
        None
    }
}

/// Check if there's a newer version of Claude Code available.
async fn check_claude_code_update(current_version: Option<&str>) -> Option<String> {
    let current_raw = current_version?;
    let current = extract_version_token(current_raw)?;

    // Check npm registry for @anthropic-ai/claude-code
    let client = reqwest::Client::new();
    let resp = client
        .get("https://registry.npmjs.org/@anthropic-ai/claude-code/latest")
        .header("User-Agent", "open-agent")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let latest_raw = json.get("version")?.as_str()?;
    let latest = extract_version_token(latest_raw).unwrap_or_else(|| {
        latest_raw.trim_start_matches('v').to_string()
    });

    if latest != current && version_is_newer(&latest, &current) {
        Some(latest.to_string())
    } else {
        None
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

    let latest_tag = String::from_utf8_lossy(&tag_result.stdout)
        .trim()
        .to_string();
    let latest_version = latest_tag.trim_start_matches('v');

    if latest_version != current && version_is_newer(latest_version, current) {
        Some(latest_version.to_string())
    } else {
        None
    }
}

/// Simple semver comparison (newer returns true if a > b).
fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse().ok()).collect() };

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

/// Extract the first semver-like token from a version string.
fn extract_version_token(input: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();

    for ch in input.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            current.push(ch);
            continue;
        }
        if current.contains('.') {
            best = Some(current.clone());
        }
        current.clear();
    }

    if current.contains('.') {
        best = Some(current);
    }

    best.map(|v| v.trim_start_matches('v').to_string())
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
/// Tries `bunx oh-my-opencode --version` first (most reliable), then falls back
/// to scanning the bun cache for platform-specific package directories.
async fn get_oh_my_opencode_version() -> Option<String> {
    // Primary: ask bunx directly (works regardless of cache layout)
    if let Ok(Ok(output)) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("bunx")
            .args(["oh-my-opencode", "--version"])
            .output(),
    )
    .await
    {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty()
                && version
                    .chars()
                    .next()
                    .map_or(false, |c| c.is_ascii_digit())
            {
                return Some(version);
            }
        }
    }

    // Fallback: scan bun cache for platform-specific packages
    // (e.g. oh-my-opencode-linux-x64@3.0.1@@@1)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let output = Command::new("bash")
        .args([
            "-c",
            &format!(
                r#"find {}/.bun/install/cache -maxdepth 1 -type d -name 'oh-my-opencode*@*' 2>/dev/null | \
                   grep -oP 'oh-my-opencode[^@]*@\K[0-9]+\.[0-9]+\.[0-9]+' | \
                   sort -V | tail -1"#,
                home
            ),
        ])
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !version.is_empty() {
            return Some(version);
        }
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
        "claude_code" => Ok(Sse::new(Box::pin(stream_claude_code_update()))),
        "amp" => Ok(Sse::new(Box::pin(stream_amp_update()))),
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
        yield sse("log", "Starting Open Agent update...", Some(0));

        // Check if source repo exists
        let repo_path = std::path::Path::new(OPEN_AGENT_REPO_PATH);
        if !repo_path.exists() {
            yield sse("error", format!("Source repo not found at {}. Clone the repo first (see INSTALL.md).", OPEN_AGENT_REPO_PATH), None);
            return;
        }

        // Fetch latest from git
        yield sse("log", "Fetching latest changes from git...", Some(5));

        let fetch_result = Command::new("git")
            .args(["fetch", "--tags", "origin"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await;

        match fetch_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield sse("error", format!("Failed to fetch: {}", stderr), None);
                return;
            }
            Err(e) => {
                yield sse("error", format!("Failed to run git fetch: {}", e), None);
                return;
            }
        }

        // Get the latest tag
        yield sse("log", "Finding latest release tag...", Some(10));

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
                yield sse("log", "No release tags found, using origin/master...", Some(12));
                "origin/master".to_string()
            }
        };

        yield sse("log", format!("Checking out {}...", latest_tag), Some(15));

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
        match Command::new("git")
            .args(["checkout", &latest_tag])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield sse("error", format!("Failed to checkout: {}", stderr), None);
                return;
            }
            Err(e) => {
                yield sse("error", format!("Failed to run git checkout: {}", e), None);
                return;
            }
        }

        // If using origin/master, pull latest
        if latest_tag == "origin/master" {
            if let Ok(output) = Command::new("git")
                .args(["pull", "origin", "master"])
                .current_dir(OPEN_AGENT_REPO_PATH)
                .output()
                .await
            {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    yield sse("log", format!("Warning: git pull failed: {}", stderr), Some(18));
                }
            }
        }

        // Build the project
        yield sse("log", "Building Open Agent (this may take a few minutes)...", Some(20));

        match Command::new("bash")
            .args(["-c", "source /root/.cargo/env && cargo build --bin open_agent"])
            .current_dir(OPEN_AGENT_REPO_PATH)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                yield sse("log", "Build complete", Some(70));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let last_lines: Vec<&str> = stderr.lines().rev().take(10).collect();
                let error_summary = last_lines.into_iter().rev().collect::<Vec<_>>().join("\n");
                yield sse("error", format!("Build failed:\n{}", error_summary), None);
                return;
            }
            Err(e) => {
                yield sse("error", format!("Failed to run cargo build: {}", e), None);
                return;
            }
        }

        // Install binaries
        yield sse("log", "Installing binaries...", Some(75));

        let binaries = [("open_agent", "/usr/local/bin/open_agent")];

        for (name, dest) in binaries {
            let src = format!("{}/target/debug/{}", OPEN_AGENT_REPO_PATH, name);
            match Command::new("install")
                .args(["-m", "0755", &src, dest])
                .output()
                .await
            {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    yield sse("error", format!("Failed to install {}: {}", name, stderr), None);
                    return;
                }
                Err(e) => {
                    yield sse("error", format!("Failed to install {}: {}", name, e), None);
                    return;
                }
            }
        }

        // Send restart event before restarting - the SSE connection will drop when the
        // service restarts since this process will be terminated by systemctl. The client
        // should detect the connection drop at progress 100% and treat it as success.
        yield sse("restarting", format!("Binaries installed, restarting service to complete update to {}...", latest_tag), Some(100));

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
///
/// Permission-aware: root installs to `/usr/local/bin` and restarts the
/// systemd service; non-root keeps the binary at `~/.opencode/bin` and
/// skips the service restart (non-root users typically lack systemd access).
fn stream_opencode_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting OpenCode update...", Some(0));
        yield sse("log", "Downloading latest OpenCode release...", Some(10));

        // Run the install script
        let download = Command::new("bash")
            .args(["-c", "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path"])
            .output()
            .await;

        let output = match download {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                yield sse("error", format!("Failed to download OpenCode: {}", stderr), None);
                return;
            }
            Err(e) => {
                yield sse("error", format!("Failed to run install script: {}", e), None);
                return;
            }
        };
        let _ = output; // consumed above; kept for clarity

        yield sse("log", "Download complete, installing...", Some(50));

        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let source_path = format!("{}/.opencode/bin/opencode", home);
        let is_root = unsafe { libc::geteuid() } == 0;

        if is_root {
            // Root: copy to system-wide location
            match Command::new("install")
                .args(["-m", "0755", &source_path, "/usr/local/bin/opencode"])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    yield sse("error", format!("Failed to install binary: {}", stderr), None);
                    return;
                }
                Err(e) => {
                    yield sse("error", format!("Failed to install binary: {}", e), None);
                    return;
                }
            }

            yield sse("log", "Binary installed, restarting service...", Some(80));

            // Restart the opencode service
            match Command::new("systemctl")
                .args(["restart", "opencode.service"])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    yield sse("complete", "OpenCode updated successfully!", Some(100));
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    yield sse("error", format!("Failed to restart service: {}", stderr), None);
                }
                Err(e) => {
                    yield sse("error", format!("Failed to restart service: {}", e), None);
                }
            }
        } else {
            // Non-root: keep binary at user-local path, skip systemd restart
            if std::path::Path::new(&source_path).exists() {
                yield sse("log", format!("Binary installed to {source_path}. Ensure this directory is in your PATH."), Some(80));
                yield sse("complete", format!("OpenCode updated successfully! Binary location: {source_path}"), Some(100));
            } else {
                yield sse(
                    "error",
                    format!(
                        "Update downloaded but binary not found at {source_path}. \
                         The installer may have placed it elsewhere. \
                         Try running 'which opencode' to find it."
                    ),
                    None,
                );
            }
        }
    }
}

/// Stream the Claude Code install/update process.
fn stream_claude_code_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Claude Code installation/update...", Some(0));

        // Check if npm is available
        let npm_check = Command::new("npm").arg("--version").output().await;
        if npm_check.is_err() || !npm_check.unwrap().status.success() {
            yield sse("error", "npm is required to install Claude Code. Please install Node.js first.", None);
            return;
        }

        yield sse("log", "Installing @anthropic-ai/claude-code globally...", Some(20));

        match Command::new("npm")
            .args(["install", "-g", "@anthropic-ai/claude-code@latest"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                yield sse("log", "Installation complete, verifying...", Some(80));

                let version = Command::new("claude").arg("--version").output().await
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .map(|l| l.trim().to_string())
                    })
                    .unwrap_or_else(|| "unknown".to_string());

                if version != "unknown" {
                    yield sse("complete", format!("Claude Code installed successfully! Version: {version}"), Some(100));
                } else {
                    yield sse("complete", "Claude Code installed, but version check failed. You may need to restart your shell.", Some(100));
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield sse("error", format!("Failed to install Claude Code: {}", stderr), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run npm install: {}", e), None);
            }
        }
    }
}

/// Stream the Amp update process.
fn stream_amp_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Amp update...", Some(0));
        yield sse("log", "Running npm install -g @anthropic-ai/amp@latest...", Some(20));

        match Command::new("npm")
            .args(["install", "-g", "@anthropic-ai/amp@latest"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let summary: String = stdout.lines().take(5).collect::<Vec<_>>().join("\n");
                yield sse("log", format!("Installation output: {summary}"), Some(80));
                yield sse("complete", "Amp updated successfully!", Some(100));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield sse("error", format!("Failed to update Amp: {} {}", stderr, stdout), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run update: {}", e), None);
            }
        }
    }
}

/// Stream the oh-my-opencode update process.
fn stream_oh_my_opencode_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting oh-my-opencode update...", Some(0));
        yield sse("log", "Clearing oh-my-opencode cache...", Some(10));

        // Remove only oh-my-opencode from bun cache (not the entire cache)
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let _ = Command::new("bash")
            .args([
                "-c",
                &format!("rm -rf {}/.bun/install/cache/oh-my-opencode@*", home),
            ])
            .output()
            .await;

        yield sse("log", "Running bunx oh-my-opencode@latest install...", Some(20));

        // Run the install command with @latest to force the newest version
        // Enable all providers by default for updates
        match Command::new("bunx")
            .args([
                "oh-my-opencode@latest",
                "install",
                "--no-tui",
                "--claude=yes",
                "--openai=yes",
                "--gemini=yes",
                "--copilot=no",
            ])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let summary: String = stdout.lines().take(5).collect::<Vec<_>>().join("\n");
                yield sse("log", format!("Installation output: {summary}"), Some(80));
                yield sse("complete", "oh-my-opencode updated successfully!", Some(100));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield sse("error", format!("Failed to update oh-my-opencode: {} {}", stderr, stdout), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run update: {}", e), None);
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
fn stream_plugin_update(
    package: String,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", format!("Starting {} update...", package), Some(0));
        yield sse("log", "Clearing package cache...", Some(10));

        let _ = Command::new("bun")
            .args(["pm", "cache", "rm"])
            .output()
            .await;

        yield sse("log", format!("Installing {}@latest...", package), Some(30));

        // Use bunx to trigger the plugin install (which will cache the latest version)
        match Command::new("bunx")
            .args([&format!("{}@latest", package), "--help"])
            .output()
            .await
        {
            Ok(_) => {
                yield sse("log", "Package downloaded, updating config...", Some(70));

                // Update the opencode config to use @latest
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                let config_path = format!("{}/.config/opencode/opencode.json", home);

                if let Ok(contents) = tokio::fs::read_to_string(&config_path).await {
                    if let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&contents) {
                        if let Some(plugins) = root.get_mut("plugin").and_then(|v| v.as_array_mut()) {
                            for p in plugins.iter_mut() {
                                if let Some(s) = p.as_str() {
                                    if s == package || s.starts_with(&format!("{}@", package)) {
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

                yield sse("complete", format!("{} updated successfully!", package), Some(100));
            }
            Err(e) => {
                yield sse("error", format!("Failed to update {}: {}", package, e), None);
            }
        }
    }
}
