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
use crate::util::home_dir;

/// Git remote used for sandboxed.sh self-updates
const SANDBOXED_REPO_REMOTE: &str = "https://github.com/Th0rgal/sandboxed.sh.git";

/// Information about a system component.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentInfo {
    pub name: String,
    pub version: Option<String>,
    pub installed: bool,
    pub update_available: Option<String>,
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
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

fn normalize_repo_path(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn select_repo_path(settings_value: Option<String>, env_override: Option<String>) -> String {
    normalize_repo_path(env_override)
        .or_else(|| normalize_repo_path(settings_value))
        .unwrap_or_else(|| crate::settings::DEFAULT_SANDBOXED_REPO_PATH.to_string())
}

fn repo_path_from_env() -> Option<String> {
    std::env::var("SANDBOXED_SH_REPO_PATH")
        .or_else(|_| std::env::var("SANDBOXED_REPO_PATH"))
        .ok()
}

async fn resolve_sandboxed_repo_path(state: &Arc<AppState>) -> String {
    let settings_value = state.settings.get_sandboxed_repo_path().await;
    select_repo_path(settings_value, repo_path_from_env())
}

fn is_safe_repo_path(path: &std::path::Path) -> bool {
    use std::path::Component;

    if !path.is_absolute() {
        return false;
    }

    let mut normal_count = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir => return false,
            Component::Normal(_) => normal_count += 1,
            _ => {}
        }
    }

    if normal_count < 2 {
        return false;
    }

    let banned = [
        "/", "/home", "/root", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/opt", "/var",
        "/tmp",
    ];
    if banned.iter().any(|p| path == std::path::Path::new(p)) {
        return false;
    }

    if let Ok(home) = std::env::var("HOME") {
        if path == std::path::Path::new(&home) {
            return false;
        }
    }

    true
}

async fn is_git_repo(repo_path: &std::path::Path) -> bool {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo_path)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .eq_ignore_ascii_case("true"),
        _ => false,
    }
}

async fn ensure_origin_remote(repo_path: &std::path::Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .await
        .map_err(|e| format!("Failed to check git remote: {}", e))?;

    if output.status.success() {
        let current = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if current == SANDBOXED_REPO_REMOTE {
            return Ok(());
        }
        let set_output = Command::new("git")
            .args(["remote", "set-url", "origin", SANDBOXED_REPO_REMOTE])
            .current_dir(repo_path)
            .output()
            .await
            .map_err(|e| format!("Failed to set git remote: {}", e))?;
        if set_output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&set_output.stderr);
        return Err(format!("Failed to set git remote: {}", stderr));
    }

    let add_output = Command::new("git")
        .args(["remote", "add", "origin", SANDBOXED_REPO_REMOTE])
        .current_dir(repo_path)
        .output()
        .await
        .map_err(|e| format!("Failed to add git remote: {}", e))?;

    if add_output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&add_output.stderr);
        Err(format!("Failed to add git remote: {}", stderr))
    }
}

async fn ensure_repo_present(repo_path: &std::path::Path) -> Result<(), String> {
    if !is_safe_repo_path(repo_path) {
        return Err(format!(
            "Refusing to operate on unsafe repo path {}",
            repo_path.display()
        ));
    }

    if repo_path.exists() && !is_git_repo(repo_path).await {
        if repo_path.is_file() {
            tokio::fs::remove_file(repo_path)
                .await
                .map_err(|e| format!("Failed to remove file at {}: {}", repo_path.display(), e))?;
        } else {
            tokio::fs::remove_dir_all(repo_path).await.map_err(|e| {
                format!(
                    "Failed to remove non-git directory at {}: {}",
                    repo_path.display(),
                    e
                )
            })?;
        }
    }

    if !repo_path.exists() {
        if let Some(parent) = repo_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                format!(
                    "Failed to create parent directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }

        let output = Command::new("git")
            .args([
                "clone",
                SANDBOXED_REPO_REMOTE,
                repo_path.to_string_lossy().as_ref(),
            ])
            .output()
            .await
            .map_err(|e| format!("Failed to run git clone: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Failed to clone repo: {}", stderr));
        }
    }

    ensure_origin_remote(repo_path).await
}

// Type alias for the boxed stream to avoid opaque type mismatch
type UpdateStream = Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>;

/// Create routes for system management.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/components", get(get_components))
        .route("/components/:name/update", post(update_component))
        .route("/components/:name/uninstall", post(uninstall_component))
}

/// Get information about all system components.
async fn get_components(State(state): State<Arc<AppState>>) -> Json<SystemComponentsResponse> {
    let mut components = Vec::new();
    let repo_path = resolve_sandboxed_repo_path(&state).await;

    // Open Agent (self)
    let current_version = env!("CARGO_PKG_VERSION");
    let update_available = check_sandboxed_update(Some(current_version), Some(&repo_path)).await;
    let status = if update_available.is_some() {
        ComponentStatus::UpdateAvailable
    } else {
        ComponentStatus::Ok
    };
    components.push(ComponentInfo {
        name: "sandboxed_sh".to_string(),
        version: Some(current_version.to_string()),
        installed: true,
        update_available,
        path: Some("/usr/local/bin/sandboxed-sh".to_string()),
        source_path: Some(repo_path),
        status,
    });

    // OpenCode
    let opencode_info = get_opencode_info(&state.config).await;
    components.push(opencode_info);

    // Claude Code
    let claudecode_info = get_claude_code_info().await;
    components.push(claudecode_info);

    // Codex
    let codex_info = get_codex_info().await;
    components.push(codex_info);

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
                source_path: None,
                status,
            }
        }
        _ => ComponentInfo {
            name: "opencode".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
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
                source_path: None,
                status,
            }
        }
        _ => ComponentInfo {
            name: "claude_code".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

/// Get Codex CLI version and status.
async fn get_codex_info() -> ComponentInfo {
    // Try to run codex --version to check if it's installed
    match Command::new("codex").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let mut version_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !version_str.is_empty() {
                    version_str.push(' ');
                }
                version_str.push_str(stderr.trim());
            }
            // Parse version from output like "codex-cli 0.94.0"
            let version = extract_version_token(&version_str);
            let update_available = check_codex_update(version.as_deref()).await;
            let status = if update_available.is_some() {
                ComponentStatus::UpdateAvailable
            } else {
                ComponentStatus::Ok
            };

            ComponentInfo {
                name: "codex".to_string(),
                version,
                installed: true,
                update_available,
                path: which_codex().await,
                source_path: None,
                status,
            }
        }
        _ => ComponentInfo {
            name: "codex".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
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

/// Find the path to the Codex binary.
async fn which_codex() -> Option<String> {
    which_binary("codex", &["/usr/local/bin/codex"]).await
}

/// Find the path to the OpenCode binary.
/// Checks PATH first, then user-local install, then system-wide.
async fn which_opencode() -> Option<String> {
    let home = home_dir();
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
                source_path: None,
                status,
            }
        }
        _ => ComponentInfo {
            name: "amp".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

/// Find the path to the Amp binary.
async fn which_amp() -> Option<String> {
    which_binary("amp", &[]).await
}

/// Fetch the latest version string for an npm package from the registry.
async fn fetch_npm_latest_version(package: &str) -> Option<String> {
    let url = format!("https://registry.npmjs.org/{package}/latest");
    let resp = reqwest::Client::new()
        .get(&url)
        .header("User-Agent", "open-agent")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
}

/// Check if there's a newer version of Amp available.
async fn check_amp_update(current_version: Option<&str>) -> Option<String> {
    let current = extract_version_token(current_version?)?;
    let latest = fetch_npm_latest_version("@sourcegraph/amp").await?;
    version_is_newer(&latest, &current).then_some(latest)
}

/// Check if there's a newer version of Claude Code available.
async fn check_claude_code_update(current_version: Option<&str>) -> Option<String> {
    let current = extract_version_token(current_version?)?;
    let latest_raw = fetch_npm_latest_version("@anthropic-ai/claude-code").await?;
    let latest = extract_version_token(&latest_raw)
        .unwrap_or_else(|| latest_raw.trim_start_matches('v').to_string());
    (latest != current && version_is_newer(&latest, &current)).then_some(latest)
}

/// Check if there's a newer version of Codex available.
async fn check_codex_update(current_version: Option<&str>) -> Option<String> {
    let current = extract_version_token(current_version?)?;
    let latest = fetch_npm_latest_version("@openai/codex").await?;
    version_is_newer(&latest, &current).then_some(latest)
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
async fn check_sandboxed_update(
    current_version: Option<&str>,
    repo_path_override: Option<&str>,
) -> Option<String> {
    let current = current_version?;

    // First, try GitHub releases API
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.github.com/repos/Th0rgal/sandboxed.sh/releases/latest")
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
    let repo_path = repo_path_override
        .map(std::path::Path::new)
        .unwrap_or_else(|| std::path::Path::new(crate::settings::DEFAULT_SANDBOXED_REPO_PATH));
    if !repo_path.exists() || !is_git_repo(repo_path).await {
        return None;
    }

    // Fetch tags first
    let _ = Command::new("git")
        .args(["fetch", "--tags", "origin"])
        .current_dir(repo_path)
        .output()
        .await;

    // Get the latest tag
    let tag_result = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0", "origin/master"])
        .current_dir(repo_path)
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
    let home = home_dir();
    let config_path = format!("{}/.config/opencode/oh-my-opencode.json", home);

    let installed = tokio::fs::metadata(&config_path).await.is_ok();

    if !installed {
        return ComponentInfo {
            name: "oh_my_opencode".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
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
        source_path: None,
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
            if !version.is_empty() && version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return Some(version);
            }
        }
    }

    // Fallback: scan bun cache for platform-specific packages
    // (e.g. oh-my-opencode-linux-x64@3.0.1@@@1)
    let home = home_dir();
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
    let latest = fetch_npm_latest_version("oh-my-opencode").await?;
    match current_version {
        Some(current) if latest != current && version_is_newer(&latest, current) => Some(latest),
        None => Some(latest), // If no current version, suggest the latest
        _ => None,
    }
}

/// Update a system component.
async fn update_component(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    match name.as_str() {
        "sandboxed_sh" => Ok(Sse::new(Box::pin(stream_sandboxed_update(state)))),
        "opencode" => Ok(Sse::new(Box::pin(stream_opencode_update()))),
        "claude_code" => Ok(Sse::new(Box::pin(stream_claude_code_update()))),
        "codex" => Ok(Sse::new(Box::pin(stream_codex_update()))),
        "amp" => Ok(Sse::new(Box::pin(stream_amp_update()))),
        "oh_my_opencode" => Ok(Sse::new(Box::pin(stream_oh_my_opencode_update()))),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", name),
        )),
    }
}

/// Uninstall a system component.
async fn uninstall_component(
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    match name.as_str() {
        "sandboxed_sh" => Err((
            StatusCode::BAD_REQUEST,
            "Cannot uninstall sandboxed.sh - it is the main application".to_string(),
        )),
        "opencode" => Ok(Sse::new(Box::pin(stream_opencode_uninstall()))),
        "claude_code" => Ok(Sse::new(Box::pin(stream_claude_code_uninstall()))),
        "codex" => Ok(Sse::new(Box::pin(stream_codex_uninstall()))),
        "amp" => Ok(Sse::new(Box::pin(stream_amp_uninstall()))),
        "oh_my_opencode" => Ok(Sse::new(Box::pin(stream_oh_my_opencode_uninstall()))),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", name),
        )),
    }
}

/// Stream the Open Agent update process.
/// Builds from source using git tags (no pre-built binaries needed).
fn stream_sandboxed_update(
    state: Arc<AppState>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Open Agent update...", Some(0));

        let repo_path_str = resolve_sandboxed_repo_path(&state).await;
        let repo_path = std::path::Path::new(&repo_path_str);

        yield sse("log", format!("Using source repo path: {}", repo_path.display()), Some(2));

        if let Err(err) = ensure_repo_present(repo_path).await {
            yield sse("error", format!("Failed to prepare source repo: {}", err), None);
            return;
        }

        // Fetch latest from git
        yield sse("log", "Fetching latest changes from git...", Some(5));

        let fetch_result = Command::new("git")
            .args(["fetch", "--tags", "origin"])
            .current_dir(repo_path)
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
            .current_dir(repo_path)
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
            .current_dir(repo_path)
            .output()
            .await;

        // Clean untracked files that might interfere
        let _ = Command::new("git")
            .args(["clean", "-fd"])
            .current_dir(repo_path)
            .output()
            .await;

        // Checkout the tag/branch
        match Command::new("git")
            .args(["checkout", &latest_tag])
            .current_dir(repo_path)
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
                .current_dir(repo_path)
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
            .args(["-c", "source /root/.cargo/env && cargo build --bin sandboxed-sh --bin workspace-mcp --bin desktop-mcp"])
            .current_dir(repo_path)
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

        // Detect the current binary path and derive the service name from it.
        // e.g. /usr/local/bin/sandboxed-sh-prod → service sandboxed-sh-prod.service
        let current_exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                yield sse("error", format!("Failed to detect current binary path: {}", e), None);
                return;
            }
        };
        let exe_name = current_exe.file_name().unwrap_or_default().to_string_lossy().to_string();
        let service_name = format!("{}.service", exe_name);
        let install_dest = current_exe.to_string_lossy().to_string();

        yield sse("log", format!("Installing binary to {} (service: {})...", install_dest, service_name), Some(75));

        // Stop the service before replacing the binary to avoid "Text file busy"
        let _ = Command::new("systemctl")
            .args(["stop", &service_name])
            .output()
            .await;

        let src = format!("{}/target/debug/sandboxed_sh", repo_path.display());
        match Command::new("install")
            .args(["-m", "0755", &src, &install_dest])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield sse("error", format!("Failed to install binary: {}", stderr), None);
                // Try to restart the service even if install failed
                let _ = Command::new("systemctl").args(["start", &service_name]).output().await;
                return;
            }
            Err(e) => {
                yield sse("error", format!("Failed to install binary: {}", e), None);
                let _ = Command::new("systemctl").args(["start", &service_name]).output().await;
                return;
            }
        }

        // Also install MCP binaries if they were built
        for mcp_bin in ["workspace-mcp", "desktop-mcp"] {
            let mcp_src = format!("{}/target/debug/{}", repo_path.display(), mcp_bin);
            let mcp_dest = format!("/usr/local/bin/{}", mcp_bin);
            if std::path::Path::new(&mcp_src).exists() {
                let _ = Command::new("install")
                    .args(["-m", "0755", &mcp_src, &mcp_dest])
                    .output()
                    .await;
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
            .args(["start", &service_name])
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

        let home = home_dir();
        let source_path = format!("{}/.opencode/bin/opencode", home);
        // SAFETY: geteuid() is a trivial syscall with no preconditions.
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

        let pm = crate::pkg_manager::preferred().await;
        let Some(pm) = pm else {
            yield sse("error", "No package manager (bun or npm) found. Please install Bun or Node.js first.", None);
            return;
        };

        yield sse("log", format!("Installing @anthropic-ai/claude-code globally via {}...", pm.bin()), Some(20));

        match Command::new(pm.bin())
            .args(pm.global_install_args("@anthropic-ai/claude-code@latest"))
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
                yield sse("error", format!("Failed to run {} install: {}", pm.bin(), e), None);
            }
        }
    }
}

/// Stream the Amp update process.
fn stream_amp_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Amp update...", Some(0));

        let pm = crate::pkg_manager::preferred().await;
        let Some(pm) = pm else {
            yield sse("error", "No package manager (bun or npm) found. Please install Bun or Node.js first.", None);
            return;
        };

        yield sse("log", format!("Running {} install -g @sourcegraph/amp@latest...", pm.bin()), Some(20));

        match Command::new(pm.bin())
            .args(pm.global_install_args("@sourcegraph/amp@latest"))
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

/// Stream the Codex install/update process.
fn stream_codex_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Codex installation/update...", Some(0));

        let pm = crate::pkg_manager::preferred().await;
        let Some(pm) = pm else {
            yield sse("error", "No package manager (bun or npm) found. Please install Bun or Node.js first.", None);
            return;
        };

        yield sse("log", format!("Installing @openai/codex@latest globally via {}...", pm.bin()), Some(20));

        match Command::new(pm.bin())
            .args(pm.global_install_args("@openai/codex@latest"))
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                yield sse("log", "Installation complete, verifying...", Some(80));

                let version = Command::new("codex").arg("--version").output().await
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        let combined = format!(
                            "{} {}",
                            String::from_utf8_lossy(&o.stdout),
                            String::from_utf8_lossy(&o.stderr)
                        );
                        extract_version_token(&combined)
                    })
                    .unwrap_or_else(|| "unknown".to_string());

                if version != "unknown" {
                    yield sse("complete", format!("Codex installed successfully! Version: {version}"), Some(100));
                } else {
                    yield sse("complete", "Codex installed, but version check failed. You may need to restart your shell.", Some(100));
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                yield sse("error", format!("Failed to install Codex: {}", stderr), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run {} install: {}", pm.bin(), e), None);
            }
        }
    }
}

/// Stream the Codex uninstall process.
fn stream_codex_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    stream_package_uninstall("@openai/codex", ".codex", "Codex")
}

/// Stream the oh-my-opencode update process.
fn stream_oh_my_opencode_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting oh-my-opencode update...", Some(0));

        let home = home_dir();

        // Remove conflicting npm/nvm global installs (we only use bunx)
        yield sse("log", "Removing npm/nvm global installs...", Some(5));
        let _ = Command::new("bash")
            .args([
                "-c",
                "npm uninstall -g oh-my-opencode 2>/dev/null || true",
            ])
            .output()
            .await;

        // Clear ALL oh-my-opencode caches (bun stores in multiple locations)
        yield sse("log", "Clearing oh-my-opencode caches...", Some(15));
        let cache_clear_script = format!(
            r#"
            rm -rf {home}/.bun/install/cache/oh-my-opencode* 2>/dev/null
            rm -rf {home}/.cache/.bun/install/cache/oh-my-opencode* 2>/dev/null
            rm -rf {home}/.npm/_npx/*/node_modules/oh-my-opencode* 2>/dev/null
            "#,
            home = home
        );
        let _ = Command::new("bash")
            .args(["-c", &cache_clear_script])
            .output()
            .await;

        yield sse("log", "Running bunx oh-my-opencode@latest install...", Some(25));

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

/// Stream the OpenCode uninstall process.
fn stream_opencode_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting OpenCode uninstall...", Some(0));

        let home = home_dir();
        // SAFETY: geteuid() is a trivial syscall with no preconditions.
        let is_root = unsafe { libc::geteuid() } == 0;

        // Stop the service first if running as root
        if is_root {
            yield sse("log", "Stopping opencode service...", Some(10));
            let _ = Command::new("systemctl")
                .args(["stop", "opencode.service"])
                .output()
                .await;
        }

        // Remove the binary from system location
        yield sse("log", "Removing OpenCode binary...", Some(30));

        let mut removed = false;

        // Remove from /usr/local/bin if exists
        if std::path::Path::new("/usr/local/bin/opencode").exists() {
            match Command::new("rm")
                .args(["-f", "/usr/local/bin/opencode"])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    yield sse("log", "Removed /usr/local/bin/opencode", Some(50));
                    removed = true;
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    yield sse("log", format!("Warning: Failed to remove /usr/local/bin/opencode: {}", stderr), Some(50));
                }
                Err(e) => {
                    yield sse("log", format!("Warning: Failed to remove /usr/local/bin/opencode: {}", e), Some(50));
                }
            }
        }

        // Remove from user-local location
        let user_bin = format!("{}/.opencode/bin/opencode", home);
        if std::path::Path::new(&user_bin).exists() {
            match Command::new("rm")
                .args(["-f", &user_bin])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    yield sse("log", format!("Removed {}", user_bin), Some(60));
                    removed = true;
                }
                _ => {}
            }
        }

        // Optionally remove the entire .opencode directory
        let opencode_dir = format!("{}/.opencode", home);
        if std::path::Path::new(&opencode_dir).exists() {
            yield sse("log", "Removing OpenCode configuration directory...", Some(70));
            match Command::new("rm")
                .args(["-rf", &opencode_dir])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    yield sse("log", format!("Removed {}", opencode_dir), Some(80));
                }
                _ => {}
            }
        }

        // Disable the systemd service if root
        if is_root {
            yield sse("log", "Disabling opencode service...", Some(90));
            let _ = Command::new("systemctl")
                .args(["disable", "opencode.service"])
                .output()
                .await;
        }

        if removed {
            yield sse("complete", "OpenCode uninstalled successfully!", Some(100));
        } else {
            yield sse("complete", "OpenCode was not installed or already removed.", Some(100));
        }
    }
}

/// Helper function to stream package uninstall process (bun-first, npm-fallback).
fn stream_package_uninstall(
    package_name: &'static str,
    config_dir: &'static str,
    display_name: &'static str,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", format!("Starting {} uninstall...", display_name), Some(0));
        let mut uninstall_failed = false;

        let pm = crate::pkg_manager::preferred().await;
        let Some(pm) = pm else {
            yield sse("error", format!("No package manager (bun or npm) found to uninstall {}.", display_name), None);
            return;
        };

        yield sse("log", format!("Uninstalling {} globally via {}...", package_name, pm.bin()), Some(20));

        match Command::new(pm.bin())
            .args(pm.global_uninstall_args(package_name))
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                yield sse("log", format!("Package removed via {}", pm.bin()), Some(50));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stderr.contains("not installed") && !stdout.contains("not installed") {
                    uninstall_failed = true;
                    yield sse("log", format!("Warning: {} uninstall had issues: {} {}", pm.bin(), stderr, stdout), None);
                }
            }
            Err(e) => {
                uninstall_failed = true;
                yield sse("log", format!("Warning: {} uninstall failed: {}", pm.bin(), e), None);
            }
        }

        // Also clean up from the other package manager if it was installed there
        let other = match pm {
            crate::pkg_manager::PkgManager::Bun => "npm",
            crate::pkg_manager::PkgManager::Npm => "bun",
        };
        let other_args = match pm {
            crate::pkg_manager::PkgManager::Bun => vec!["uninstall", "-g", package_name],
            crate::pkg_manager::PkgManager::Npm => vec!["remove", "-g", package_name],
        };
        yield sse("log", format!("Cleaning up {} global install if any...", other), Some(60));
        match Command::new(other).args(&other_args).output().await {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stderr.contains("not installed") && !stdout.contains("not installed") {
                    uninstall_failed = true;
                    yield sse("log", format!("Warning: {} uninstall had issues: {} {}", other, stderr, stdout), None);
                }
            }
            Err(e) => {
                uninstall_failed = true;
                yield sse("log", format!("Warning: {} uninstall failed: {}", other, e), None);
            }
        }

        // Remove configuration directory
        let home = home_dir();
        let config_path = format!("{}/{}", home, config_dir);
        if std::path::Path::new(&config_path).exists() {
            yield sse("log", format!("Removing {} configuration...", display_name), Some(80));
            let _ = Command::new("rm")
                .args(["-rf", &config_path])
                .output()
                .await;
        }

        if uninstall_failed {
            yield sse(
                "error",
                format!(
                    "{} uninstall encountered errors. Some files may remain installed.",
                    display_name
                ),
                None,
            );
        } else {
            yield sse("complete", format!("{} uninstalled successfully!", display_name), Some(100));
        }
    }
}

/// Stream the Claude Code uninstall process.
fn stream_claude_code_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    stream_package_uninstall("@anthropic-ai/claude-code", ".claude", "Claude Code")
}

/// Stream the Amp uninstall process.
fn stream_amp_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    stream_package_uninstall("@sourcegraph/amp", ".agents", "Amp")
}

/// Stream the oh-my-opencode uninstall process.
fn stream_oh_my_opencode_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>>
{
    async_stream::stream! {
        yield sse("log", "Starting oh-my-opencode uninstall...", Some(0));

        let home = home_dir();

        // Remove npm global install if exists
        yield sse("log", "Removing npm global install...", Some(10));
        let _ = Command::new("npm")
            .args(["uninstall", "-g", "oh-my-opencode"])
            .output()
            .await;

        // Clear bun cache for oh-my-opencode
        yield sse("log", "Clearing oh-my-opencode caches...", Some(30));
        let cache_clear_script = format!(
            r#"
            rm -rf {home}/.bun/install/cache/oh-my-opencode* 2>/dev/null
            rm -rf {home}/.cache/.bun/install/cache/oh-my-opencode* 2>/dev/null
            rm -rf {home}/.npm/_npx/*/node_modules/oh-my-opencode* 2>/dev/null
            "#,
            home = home
        );
        let _ = Command::new("bash")
            .args(["-c", &cache_clear_script])
            .output()
            .await;

        // Remove the oh-my-opencode config file
        yield sse("log", "Removing oh-my-opencode configuration...", Some(60));
        let config_path = format!("{}/.config/opencode/oh-my-opencode.json", home);
        if std::path::Path::new(&config_path).exists() {
            match Command::new("rm")
                .args(["-f", &config_path])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    yield sse("log", "Removed oh-my-opencode.json", Some(80));
                }
                _ => {}
            }
        }

        yield sse("complete", "oh-my-opencode uninstalled successfully!", Some(100));
    }
}

#[cfg(test)]
mod tests {
    use super::{is_safe_repo_path, normalize_repo_path, select_repo_path};

    #[test]
    fn select_repo_path_prefers_env() {
        let result = select_repo_path(
            Some("/opt/custom".to_string()),
            Some(" /env/override ".to_string()),
        );
        assert_eq!(result, "/env/override");
    }

    #[test]
    fn select_repo_path_falls_back_to_settings() {
        let result = select_repo_path(Some("/opt/custom".to_string()), None);
        assert_eq!(result, "/opt/custom");
    }

    #[test]
    fn select_repo_path_uses_default_when_empty() {
        let result = select_repo_path(Some("  ".to_string()), Some("".to_string()));
        assert_eq!(result, crate::settings::DEFAULT_SANDBOXED_REPO_PATH);
    }

    #[test]
    fn normalize_repo_path_trims_and_drops_empty() {
        assert_eq!(
            normalize_repo_path(Some("  /x  ".to_string())),
            Some("/x".to_string())
        );
        assert_eq!(normalize_repo_path(Some("   ".to_string())), None);
        assert_eq!(normalize_repo_path(None), None);
    }

    #[test]
    fn safe_repo_path_rejects_root() {
        assert!(!is_safe_repo_path(std::path::Path::new("/")));
    }
}
