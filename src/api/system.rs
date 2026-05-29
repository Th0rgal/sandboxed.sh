//! System component management API.
//!
//! Provides endpoints to query and update system components like OpenCode
//! and related CLI components.

use std::pin::Pin;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        Json,
    },
    routing::{get, post},
    Extension, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use uuid::Uuid;

use super::auth::AuthUser;
use super::routes::AppState;
use crate::util::home_dir;
use crate::workspace::{Workspace, WorkspaceStatus, WorkspaceType};

/// Git remote used for sandboxed.sh self-updates
const SANDBOXED_REPO_REMOTE: &str = "https://github.com/Th0rgal/sandboxed.sh.git";
const MIN_SUPPORTED_OPENCODE_VERSION: &str = "1.1.59";

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

/// Per-workspace view of a single component's installed version.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceComponentInfo {
    pub workspace_id: String,
    pub workspace_name: String,
    pub workspace_type: &'static str,
    pub workspace_status: &'static str,
    /// Installed version of the component inside this workspace, if any.
    pub version: Option<String>,
    /// True iff this workspace's version equals the host's version.
    pub in_sync: bool,
    /// Optional reason this workspace couldn't be probed (e.g. "not ready",
    /// "nspawn unavailable", "timed out").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Aggregated by-workspace info for a single component.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentWorkspaceReport {
    pub name: String,
    pub host_version: Option<String>,
    pub host_update_available: Option<String>,
    pub host_status: ComponentStatus,
    /// True if this component supports per-workspace installs. Components like
    /// `sandboxed_sh` are host-only and have an empty `workspaces` list.
    pub per_workspace: bool,
    pub workspaces: Vec<WorkspaceComponentInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentsByWorkspaceResponse {
    pub components: Vec<ComponentWorkspaceReport>,
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
            Component::Normal(part) => {
                if part.to_string_lossy().starts_with('.') {
                    return false;
                }
                normal_count += 1;
            }
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
        .route("/components/by-workspace", get(get_components_by_workspace))
        .route("/hermes-assistant/adopt", post(adopt_hermes_assistant))
        .route("/hermes-assistant/status", get(get_hermes_assistant_status))
        .route("/hermes-assistant/stop", post(stop_hermes_assistant))
        .route("/components/:name/update", post(update_component))
        .route("/components/:name/uninstall", post(uninstall_component))
        .route("/deploy", post(deploy_sandboxed_sh))
}

/// Get information about all system components.
async fn get_components(State(state): State<Arc<AppState>>) -> Json<SystemComponentsResponse> {
    let mut components = Vec::new();
    let repo_path = resolve_sandboxed_repo_path(&state).await;

    // sandboxed.sh (self)
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

    // Hermes assistant MCP connector
    let assistant_mcp_info = get_assistant_mcp_info().await;
    components.push(assistant_mcp_info);

    // External Hermes assistant runtime/gateway service, if installed on this host.
    let hermes_assistant_info = get_hermes_assistant_info(&state.config).await;
    components.push(hermes_assistant_info);

    // OpenCode
    let opencode_info = get_opencode_info(&state.config).await;
    components.push(opencode_info);

    // Claude Code
    let claudecode_info = get_claude_code_info().await;
    components.push(claudecode_info);

    // Codex
    let codex_info = get_codex_info().await;
    components.push(codex_info);

    // Grok Build
    let grok_info = get_grok_info().await;
    components.push(grok_info);

    Json(SystemComponentsResponse { components })
}

/// Components that support per-workspace installations. Order is preserved in the response.
const PER_WORKSPACE_COMPONENTS: &[&str] = &["opencode", "claude_code", "codex", "grok"];

/// Get per-workspace version info for each component. Container workspaces are probed via nspawn
/// in parallel with a per-probe timeout to keep the page responsive.
async fn get_components_by_workspace(
    State(state): State<Arc<AppState>>,
) -> Json<ComponentsByWorkspaceResponse> {
    // Reuse the host-level report so the comparison target stays in lockstep with /components.
    let host = get_components(State(state.clone())).await.0.components;
    let host_by_name: std::collections::HashMap<String, ComponentInfo> =
        host.into_iter().map(|c| (c.name.clone(), c)).collect();

    let workspaces = state.workspaces.list().await;
    let nspawn_ok = crate::nspawn::nspawn_available();

    let mut reports = Vec::with_capacity(host_by_name.len());

    for name in PER_WORKSPACE_COMPONENTS {
        let Some(host_info) = host_by_name.get(*name).cloned() else {
            continue;
        };

        // Spawn a parallel probe per workspace.
        let host_version = host_info.version.clone();
        let mut probes = futures::stream::FuturesUnordered::new();
        for ws in &workspaces {
            let ws = ws.clone();
            let host_v = host_version.clone();
            let component = (*name).to_string();
            probes.push(tokio::spawn(async move {
                probe_workspace_component(&ws, &component, host_v.as_deref(), nspawn_ok).await
            }));
        }

        use futures::StreamExt;
        let mut ws_infos = Vec::with_capacity(workspaces.len());
        while let Some(joined) = probes.next().await {
            if let Ok(info) = joined {
                ws_infos.push(info);
            }
        }
        ws_infos.sort_by(|a, b| a.workspace_name.cmp(&b.workspace_name));

        reports.push(ComponentWorkspaceReport {
            name: host_info.name,
            host_version: host_info.version,
            host_update_available: host_info.update_available,
            host_status: host_info.status,
            per_workspace: true,
            workspaces: ws_infos,
        });
    }

    Json(ComponentsByWorkspaceResponse {
        components: reports,
    })
}

/// Probe a single workspace for the installed version of a component.
async fn probe_workspace_component(
    workspace: &Workspace,
    component: &str,
    host_version: Option<&str>,
    nspawn_ok: bool,
) -> WorkspaceComponentInfo {
    let workspace_type = match workspace.workspace_type {
        WorkspaceType::Host => "host",
        WorkspaceType::Container => "container",
    };
    let workspace_status = match workspace.status {
        WorkspaceStatus::Pending => "pending",
        WorkspaceStatus::Building => "building",
        WorkspaceStatus::Ready => "ready",
        WorkspaceStatus::Error => "error",
    };

    // Host workspaces share the host's binaries, so the version is whatever the host probe found.
    if workspace.workspace_type == WorkspaceType::Host {
        let version = host_version.map(|s| s.to_string());
        let in_sync = version.is_some() && version.as_deref() == host_version;
        return WorkspaceComponentInfo {
            workspace_id: workspace.id.to_string(),
            workspace_name: workspace.name.clone(),
            workspace_type,
            workspace_status,
            version,
            in_sync,
            note: None,
        };
    }

    if workspace.status != WorkspaceStatus::Ready {
        return WorkspaceComponentInfo {
            workspace_id: workspace.id.to_string(),
            workspace_name: workspace.name.clone(),
            workspace_type,
            workspace_status,
            version: None,
            in_sync: false,
            note: Some(format!("workspace is {}", workspace_status)),
        };
    }

    if !nspawn_ok {
        return WorkspaceComponentInfo {
            workspace_id: workspace.id.to_string(),
            workspace_name: workspace.name.clone(),
            workspace_type,
            workspace_status,
            version: None,
            in_sync: false,
            note: Some("nspawn unavailable on host".to_string()),
        };
    }

    let (version, note) = match probe_version_in_container(workspace, component).await {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    };
    let in_sync = match (&version, host_version) {
        (Some(v), Some(h)) => v == h,
        _ => false,
    };

    WorkspaceComponentInfo {
        workspace_id: workspace.id.to_string(),
        workspace_name: workspace.name.clone(),
        workspace_type,
        workspace_status,
        version,
        in_sync,
        note,
    }
}

/// Exec `<tool> --version` inside a container with a strict timeout. Returns the parsed
/// version (if any) or an error string describing why the probe failed.
async fn probe_version_in_container(
    workspace: &Workspace,
    component: &str,
) -> Result<Option<String>, String> {
    let bin = component_binary_name(component)
        .ok_or_else(|| format!("unsupported component: {component}"))?;
    let config = crate::nspawn::NspawnConfig {
        env: workspace.env_vars.clone(),
        ..Default::default()
    };
    // Use sh -lc so PATH is configured the same way an interactive shell would see it.
    let cmd = vec![
        "sh".to_string(),
        "-lc".to_string(),
        format!(
            "command -v {bin} >/dev/null 2>&1 && {bin} --version 2>&1 || echo __NOT_INSTALLED__"
        ),
    ];

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        crate::nspawn::execute_in_container(&workspace.path, &cmd, &config),
    )
    .await;

    let output = match result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("nspawn error: {e}")),
        Err(_) => return Err("timed out".to_string()),
    };

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.contains("__NOT_INSTALLED__") {
        return Ok(None);
    }
    Ok(extract_version_token(&combined))
}

/// CLI binary name used by each component inside a workspace.
fn component_binary_name(component: &str) -> Option<&'static str> {
    match component {
        "opencode" => Some("opencode"),
        "claude_code" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        _ => None,
    }
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

            let is_too_old = version
                .as_deref()
                .map(|v| version_is_newer(MIN_SUPPORTED_OPENCODE_VERSION, v))
                .unwrap_or(false);
            let mut update_available = check_opencode_update(version.as_deref()).await;
            if is_too_old && update_available.is_none() {
                update_available = Some(format!(">= {} required", MIN_SUPPORTED_OPENCODE_VERSION));
            }
            let status = if is_too_old {
                ComponentStatus::Error
            } else if update_available.is_some() {
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

/// Get Grok Build CLI version and status.
async fn get_grok_info() -> ComponentInfo {
    match Command::new("grok").arg("--version").output().await {
        Ok(output) if output.status.success() => {
            let mut version_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !version_str.is_empty() {
                    version_str.push(' ');
                }
                version_str.push_str(stderr.trim());
            }
            let version = extract_version_token(&version_str);

            ComponentInfo {
                name: "grok".to_string(),
                version,
                installed: true,
                update_available: None,
                path: which_grok().await,
                source_path: None,
                status: ComponentStatus::Ok,
            }
        }
        _ => ComponentInfo {
            name: "grok".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: None,
            source_path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

async fn get_assistant_mcp_info() -> ComponentInfo {
    match Command::new("assistant-mcp")
        .arg("--version")
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version = extract_version_token(&String::from_utf8_lossy(&output.stdout));

            ComponentInfo {
                name: "assistant_mcp".to_string(),
                version,
                installed: true,
                update_available: None,
                path: which_assistant_mcp().await,
                source_path: None,
                status: ComponentStatus::Ok,
            }
        }
        _ => ComponentInfo {
            name: "assistant_mcp".to_string(),
            version: None,
            installed: false,
            update_available: None,
            path: which_assistant_mcp().await,
            source_path: None,
            status: ComponentStatus::NotInstalled,
        },
    }
}

async fn get_hermes_assistant_info(config: &crate::config::Config) -> ComponentInfo {
    let expected_service = format!("{}.service", assistant_runtime_name(config));
    let fallback_service = if expected_service == "hermes-assistant-dev.service" {
        "hermes-assistant.service"
    } else {
        "hermes-assistant-dev.service"
    };

    for service_name in [expected_service.as_str(), fallback_service] {
        if let Some(info) = get_systemd_service_component("hermes_assistant", service_name).await {
            return info;
        }
    }

    ComponentInfo {
        name: "hermes_assistant".to_string(),
        version: None,
        installed: false,
        update_available: None,
        path: None,
        source_path: None,
        status: ComponentStatus::NotInstalled,
    }
}

#[derive(Debug, Deserialize)]
pub struct AdoptHermesAssistantRequest {
    pub gateway_id: Uuid,
    #[serde(default)]
    pub allow_all_users: bool,
    #[serde(default = "default_hermes_model")]
    pub model: String,
    #[serde(default)]
    pub install_hermes_if_missing: bool,
}

#[derive(Debug, Serialize)]
pub struct AdoptHermesAssistantResponse {
    pub ok: bool,
    pub gateway_id: Uuid,
    pub gateway_username: Option<String>,
    pub service_name: String,
    pub env_path: String,
    pub dotenv_path: String,
    pub config_path: String,
    pub soul_path: String,
    pub workspace_path: String,
    pub api_url: String,
    pub model: String,
    pub allowed_users_count: usize,
    pub allow_all_users: bool,
    pub legacy_gateway_active: bool,
    pub hermes_installed: bool,
    pub hermes_status: ComponentStatus,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HermesAssistantStatusResponse {
    pub service_name: String,
    pub service_active: bool,
    pub env_path: String,
    pub dotenv_path: String,
    pub config_path: String,
    pub soul_path: String,
    pub env_present: bool,
    pub dotenv_present: bool,
    pub config_present: bool,
    pub soul_present: bool,
    pub token_present: bool,
    pub telegram_ok: Option<bool>,
    pub telegram_bot_username: Option<String>,
    pub telegram_webhook_configured: Option<bool>,
    pub telegram_pending_update_count: Option<i64>,
    pub telegram_last_error: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct StopHermesAssistantResponse {
    pub ok: bool,
    pub service_name: String,
    pub service_active: bool,
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TelegramGetMeResult {
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TelegramWebhookInfoResult {
    #[serde(default)]
    url: String,
    #[serde(default)]
    pending_update_count: Option<i64>,
    #[serde(default)]
    last_error_message: Option<String>,
}

fn default_hermes_model() -> String {
    "builtin/assistant".to_string()
}

fn assistant_runtime_name(config: &crate::config::Config) -> &'static str {
    if config.port == 3002
        || std::env::var("SANDBOXED_ENV")
            .map(|v| v.eq_ignore_ascii_case("dev"))
            .unwrap_or(false)
        || std::env::var("OPEN_AGENT_ENV")
            .map(|v| v.eq_ignore_ascii_case("dev"))
            .unwrap_or(false)
    {
        "hermes-assistant-dev"
    } else {
        "hermes-assistant"
    }
}

fn local_api_url(config: &crate::config::Config) -> String {
    format!("http://127.0.0.1:{}", config.port)
}

fn env_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn env_line(key: &str, value: &str) -> String {
    format!("{key}={}\n", env_quote(value))
}

fn env_unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        return trimmed[1..trimmed.len() - 1].replace("'\\''", "'");
    }
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    trimmed.to_string()
}

fn parse_env_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (candidate, value) = line.split_once('=')?;
        if candidate.trim() == key {
            Some(env_unquote(value))
        } else {
            None
        }
    })
}

fn comma_join_i64(values: &[i64]) -> String {
    values
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn hermes_config_yaml(runtime_name: &str) -> String {
    format!(
        r#"model:
  provider: custom
  default: ${{HERMES_ASSISTANT_MODEL}}
  base_url: ${{OPENAI_BASE_URL}}
  api_key: ${{OPENAI_API_KEY}}
  api_mode: chat_completions

terminal:
  backend: local
  cwd: /var/lib/{runtime_name}/workspace

mcp_servers:
  sandboxed_assistant:
    command: ${{HERMES_ASSISTANT_MCP_COMMAND}}
    env:
      HERMES_SANDBOXED_API_URL: ${{HERMES_SANDBOXED_API_URL}}
      HERMES_SANDBOXED_API_TOKEN: ${{HERMES_SANDBOXED_API_TOKEN}}
      JWT_SECRET: ${{JWT_SECRET}}
      HERMES_ASSISTANT_USER_ID: ${{HERMES_ASSISTANT_USER_ID}}
      HERMES_DEFAULT_WORKSPACE_ID: ${{HERMES_DEFAULT_WORKSPACE_ID}}
    timeout: 120
    connect_timeout: 15
    tools:
      include:
        - list_active_missions
        - list_missions
        - get_mission
        - get_mission_events
        - start_mission
        - send_message_to_mission
        - cancel_mission
        - list_workspaces
      prompts: false
      resources: false

display:
  platforms:
    telegram:
      tool_progress: off
      cleanup_progress: true
"#
    )
}

fn hermes_soul_markdown(channel: &super::mission_store::TelegramChannel) -> String {
    let instructions = channel.instructions.as_deref().unwrap_or_default().trim();
    if instructions.is_empty() {
        return "You are the sandboxed.sh Assistant. Help the operator manage missions, workspaces, and related development work through the available tools.\n".to_string();
    }

    format!("{instructions}\n")
}

fn hermes_service_unit(runtime_name: &str, env_path: &str, service_after: &str) -> String {
    format!(
        r#"[Unit]
Description=Hermes Assistant gateway
After=network-online.target {service_after}
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile={env_path}
WorkingDirectory=/var/lib/{runtime_name}
ExecStart=/usr/local/bin/hermes gateway --accept-hooks run
Restart=always
RestartSec=5
TimeoutStopSec=240
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
"#
    )
}

async fn run_host_command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("failed to run {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        Ok(format!("{stdout}{stderr}"))
    } else {
        Err(format!(
            "{program} exited with {}: {}{}",
            output.status, stdout, stderr
        ))
    }
}

async fn ensure_hermes_installed(install_if_missing: bool) -> Result<bool, String> {
    if Command::new("hermes")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(true);
    }

    if !install_if_missing {
        return Ok(false);
    }

    run_host_command(
        "sh",
        &[
            "-lc",
            "set -e; curl -fsSL https://raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh | bash -s -- --skip-browser; for p in /root/.local/bin/hermes /root/.hermes/bin/hermes /usr/local/bin/hermes; do if [ -x \"$p\" ]; then install -m 0755 \"$p\" /usr/local/bin/hermes; exit 0; fi; done; command -v hermes >/dev/null",
        ],
    )
    .await?;

    Ok(true)
}

async fn write_private_file(path: &str, contents: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = format!("{path}.tmp");
    tokio::fs::write(&tmp, contents)
        .await
        .map_err(|e| format!("failed to write {tmp}: {e}"))?;
    tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| format!("failed to chmod {tmp}: {e}"))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("failed to install {path}: {e}"))?;
    Ok(())
}

async fn rollback_legacy_gateway(
    state: &Arc<AppState>,
    control: &super::control::ControlState,
    mut channel: super::mission_store::TelegramChannel,
) {
    channel.active = true;
    channel.updated_at = super::mission_store::now_string();
    if control
        .mission_store
        .update_telegram_channel(channel.clone())
        .await
        .is_ok()
    {
        let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
        let _ = state
            .telegram_bridge
            .start_channel(
                channel,
                control.cmd_tx.clone(),
                control.events_tx.clone(),
                control.mission_store.clone(),
                &public_url,
            )
            .await;
    }
}

/// Adopt an existing Assistant/Telegram gateway into the host Hermes runtime.
///
/// The token is read from the existing encrypted/local mission store and written
/// only to root-owned host files. The response intentionally returns no secret
/// values.
async fn adopt_hermes_assistant(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<AdoptHermesAssistantRequest>,
) -> Result<Json<AdoptHermesAssistantResponse>, (StatusCode, String)> {
    let control = state.control.get_or_spawn(&user).await;
    let mut channel = control
        .mission_store
        .get_telegram_channel(req.gateway_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Assistant gateway {} not found", req.gateway_id),
            )
        })?;

    let allowed_users = comma_join_i64(&channel.allowed_chat_ids);
    if allowed_users.is_empty() && !req.allow_all_users {
        return Err((
            StatusCode::BAD_REQUEST,
            "Gateway has no allowed chat/user IDs. Set allowed users first or explicitly allow all users for this adopt run.".to_string(),
        ));
    }

    let hermes_installed = ensure_hermes_installed(req.install_hermes_if_missing)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !hermes_installed {
        return Err((
            StatusCode::FAILED_DEPENDENCY,
            "Hermes is not installed. Retry with install_hermes_if_missing=true.".to_string(),
        ));
    }

    let runtime_name = assistant_runtime_name(&state.config);
    let service_name = format!("{runtime_name}.service");
    let env_path = format!("/etc/sandboxed-sh/{runtime_name}.env");
    let dotenv_path = format!("/var/lib/{runtime_name}/.env");
    let config_path = format!("/var/lib/{runtime_name}/config.yaml");
    let soul_path = format!("/var/lib/{runtime_name}/SOUL.md");
    let workspace_path = format!("/var/lib/{runtime_name}/workspace");
    let api_url = local_api_url(&state.config);
    let model = if req.model.trim().is_empty() {
        default_hermes_model()
    } else {
        req.model.trim().to_string()
    };
    let proxy_key = state
        .proxy_api_keys
        .create(format!(
            "Hermes Assistant {}",
            chrono::Utc::now().to_rfc3339()
        ))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .key;
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_default();
    let user_id = user.id.clone();
    let default_workspace_id = channel
        .default_workspace_id
        .map(|id| id.to_string())
        .unwrap_or_default();

    run_host_command("install", &["-d", "-m", "0755", "/etc/sandboxed-sh"])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    run_host_command(
        "install",
        &["-d", "-m", "0755", &format!("/var/lib/{runtime_name}")],
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    run_host_command("install", &["-d", "-m", "0755", &workspace_path])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut env = String::new();
    env.push_str(&env_line("HOME", &format!("/var/lib/{runtime_name}")));
    env.push_str(&env_line(
        "HERMES_HOME",
        &format!("/var/lib/{runtime_name}"),
    ));
    env.push_str("HERMES_ACCEPT_HOOKS=1\n");
    env.push_str("HERMES_INFERENCE_PROVIDER=custom\n");
    env.push_str(&env_line("HERMES_SANDBOXED_API_URL", &api_url));
    env.push_str("HERMES_SANDBOXED_API_TOKEN=\n");
    env.push_str(&env_line("JWT_SECRET", &jwt_secret));
    env.push_str(&env_line("HERMES_ASSISTANT_USER_ID", &user_id));
    env.push_str(&env_line(
        "HERMES_DEFAULT_WORKSPACE_ID",
        &default_workspace_id,
    ));
    env.push_str(&env_line("OPENAI_BASE_URL", &format!("{api_url}/v1")));
    env.push_str(&env_line("OPENAI_API_KEY", &proxy_key));
    env.push_str(&env_line("HERMES_ASSISTANT_MODEL", &model));
    env.push_str(&env_line("TELEGRAM_BOT_TOKEN", &channel.bot_token));
    env.push_str(&env_line("TELEGRAM_ALLOWED_USERS", &allowed_users));
    if channel.allowed_chat_ids.len() == 1 {
        let home_channel = channel.allowed_chat_ids[0].to_string();
        env.push_str(&env_line("TELEGRAM_HOME_CHANNEL", &home_channel));
        env.push_str(&env_line("TELEGRAM_HOME_CHANNEL_NAME", "Thomas"));
    }
    if req.allow_all_users {
        env.push_str("GATEWAY_ALLOW_ALL_USERS=true\n");
    }
    env.push_str("HERMES_ASSISTANT_MCP_COMMAND=/usr/local/bin/assistant-mcp\n");

    write_private_file(&env_path, &env)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    write_private_file(&dotenv_path, &env)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    write_private_file(&config_path, &hermes_config_yaml(runtime_name))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    write_private_file(&soul_path, &hermes_soul_markdown(&channel))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let service_path = format!("/etc/systemd/system/{service_name}");
    let service_after = if runtime_name.ends_with("-dev") {
        "sandboxed-sh-dev.service"
    } else {
        "sandboxed-sh-prod.service"
    };
    tokio::fs::write(
        &service_path,
        hermes_service_unit(runtime_name, &env_path, service_after),
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write service: {e}"),
        )
    })?;

    let was_active = channel.active;
    if channel.active {
        channel.active = false;
        channel.updated_at = super::mission_store::now_string();
        control
            .mission_store
            .update_telegram_channel(channel.clone())
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        state.telegram_bridge.stop_channel(channel.id).await;
    }

    let start_result = async {
        run_host_command("systemctl", &["daemon-reload"]).await?;
        run_host_command("systemctl", &["enable", "--now", &service_name]).await?;
        run_host_command("systemctl", &["restart", &service_name]).await?;
        run_host_command("systemctl", &["is-active", "--quiet", &service_name]).await
    }
    .await;

    if let Err(error) = start_result {
        if was_active {
            rollback_legacy_gateway(&state, &control, channel.clone()).await;
        }
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Hermes service failed to start; legacy gateway rollback attempted: {error}"),
        ));
    }

    let hermes_info = get_systemd_service_component("hermes_assistant", &service_name)
        .await
        .unwrap_or(ComponentInfo {
            name: "hermes_assistant".to_string(),
            version: None,
            installed: true,
            update_available: None,
            path: Some(service_path),
            source_path: None,
            status: ComponentStatus::Error,
        });

    let mut notes = vec![
        "Telegram bot token was copied from the existing gateway without being returned."
            .to_string(),
        "Legacy sandboxed webhook was deactivated for this gateway before starting Hermes."
            .to_string(),
    ];
    if allowed_users.is_empty() && req.allow_all_users {
        notes.push("Hermes was configured with GATEWAY_ALLOW_ALL_USERS=true.".to_string());
    }

    Ok(Json(AdoptHermesAssistantResponse {
        ok: matches!(hermes_info.status, ComponentStatus::Ok),
        gateway_id: channel.id,
        gateway_username: channel.bot_username.clone(),
        service_name,
        env_path,
        dotenv_path,
        config_path,
        soul_path,
        workspace_path,
        api_url,
        model,
        allowed_users_count: channel.allowed_chat_ids.len(),
        allow_all_users: req.allow_all_users,
        legacy_gateway_active: false,
        hermes_installed,
        hermes_status: hermes_info.status,
        notes,
    }))
}

async fn get_hermes_assistant_status(
    State(state): State<Arc<AppState>>,
) -> Json<HermesAssistantStatusResponse> {
    let runtime_name = assistant_runtime_name(&state.config);
    let service_name = format!("{runtime_name}.service");
    let env_path = format!("/etc/sandboxed-sh/{runtime_name}.env");
    let dotenv_path = format!("/var/lib/{runtime_name}/.env");
    let config_path = format!("/var/lib/{runtime_name}/config.yaml");
    let soul_path = format!("/var/lib/{runtime_name}/SOUL.md");
    let service_active = Command::new("systemctl")
        .args(["is-active", "--quiet", &service_name])
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false);
    let env_contents = tokio::fs::read_to_string(&env_path).await.ok();
    let env_present = env_contents.is_some();
    let dotenv_present = tokio::fs::metadata(&dotenv_path).await.is_ok();
    let config_present = tokio::fs::metadata(&config_path).await.is_ok();
    let soul_present = tokio::fs::metadata(&soul_path).await.is_ok();
    let token = env_contents
        .as_deref()
        .and_then(|contents| parse_env_value(contents, "TELEGRAM_BOT_TOKEN"))
        .filter(|value| !value.trim().is_empty());
    let token_present = token.is_some();

    let mut telegram_ok = None;
    let mut telegram_bot_username = None;
    let mut telegram_webhook_configured = None;
    let mut telegram_pending_update_count = None;
    let mut telegram_last_error = None;
    let mut notes = Vec::new();

    if let Some(token) = token {
        let client = reqwest::Client::new();
        let base = format!("https://api.telegram.org/bot{token}");

        match client
            .get(format!("{base}/getMe"))
            .send()
            .await
            .and_then(|resp| resp.error_for_status())
        {
            Ok(resp) => match resp
                .json::<TelegramApiResponse<TelegramGetMeResult>>()
                .await
            {
                Ok(body) => {
                    telegram_ok = Some(body.ok);
                    telegram_bot_username = body.result.and_then(|result| result.username);
                    if !body.ok {
                        telegram_last_error = body.description;
                    }
                }
                Err(error) => {
                    telegram_ok = Some(false);
                    telegram_last_error = Some(format!("Telegram getMe decode failed: {error}"));
                }
            },
            Err(error) => {
                telegram_ok = Some(false);
                telegram_last_error = Some(format!("Telegram getMe failed: {error}"));
            }
        }

        match client
            .get(format!("{base}/getWebhookInfo"))
            .send()
            .await
            .and_then(|resp| resp.error_for_status())
        {
            Ok(resp) => match resp
                .json::<TelegramApiResponse<TelegramWebhookInfoResult>>()
                .await
            {
                Ok(body) => {
                    if let Some(result) = body.result {
                        telegram_webhook_configured = Some(!result.url.trim().is_empty());
                        telegram_pending_update_count = result.pending_update_count;
                        if result.last_error_message.is_some() {
                            telegram_last_error = result.last_error_message;
                        }
                    }
                }
                Err(error) => {
                    telegram_last_error = Some(format!("Telegram webhook decode failed: {error}"));
                }
            },
            Err(error) => {
                telegram_last_error = Some(format!("Telegram getWebhookInfo failed: {error}"));
            }
        }
    } else {
        notes.push("TELEGRAM_BOT_TOKEN is not present in the Hermes env file.".to_string());
    }

    if telegram_webhook_configured == Some(false) {
        notes.push(
            "Telegram has no webhook configured; Hermes should receive updates by polling."
                .to_string(),
        );
    }
    if telegram_webhook_configured == Some(true) {
        notes.push(
            "Telegram still has a webhook configured, which can block polling mode.".to_string(),
        );
    }

    Json(HermesAssistantStatusResponse {
        service_name,
        service_active,
        env_path,
        dotenv_path,
        config_path,
        soul_path,
        env_present,
        dotenv_present,
        config_present,
        soul_present,
        token_present,
        telegram_ok,
        telegram_bot_username,
        telegram_webhook_configured,
        telegram_pending_update_count,
        telegram_last_error,
        notes,
    })
}

async fn stop_hermes_assistant(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StopHermesAssistantResponse>, (StatusCode, String)> {
    let runtime_name = assistant_runtime_name(&state.config);
    let service_name = format!("{runtime_name}.service");

    run_host_command("systemctl", &["disable", "--now", &service_name])
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    let service_active = Command::new("systemctl")
        .args(["is-active", "--quiet", &service_name])
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false);

    Ok(Json(StopHermesAssistantResponse {
        ok: !service_active,
        service_name,
        service_active,
    }))
}

async fn get_systemd_service_component(
    component_name: &str,
    service_name: &str,
) -> Option<ComponentInfo> {
    let output = Command::new("systemctl")
        .args([
            "show",
            service_name,
            "--property=LoadState",
            "--property=ActiveState",
            "--property=FragmentPath",
        ])
        .output()
        .await
        .ok()?;

    // `systemctl show` emits `Key=Value` lines, but NOT necessarily in the
    // order the `--property` flags were given (it follows internal property
    // ordering — see systemd#28205). Parse by key instead of by position so we
    // never swap LoadState/ActiveState/FragmentPath.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut load_state = "";
    let mut active_state = "";
    let mut fragment_path = "";
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "LoadState" => load_state = value.trim(),
                "ActiveState" => active_state = value.trim(),
                "FragmentPath" => fragment_path = value.trim(),
                _ => {}
            }
        }
    }

    systemd_service_component_from_states(
        component_name,
        service_name,
        load_state,
        active_state,
        fragment_path,
    )
}

fn systemd_service_component_from_states(
    component_name: &str,
    service_name: &str,
    load_state: &str,
    active_state: &str,
    fragment_path: &str,
) -> Option<ComponentInfo> {
    if load_state != "loaded" {
        return None;
    }

    Some(ComponentInfo {
        name: component_name.to_string(),
        version: None,
        installed: true,
        update_available: None,
        path: if fragment_path.is_empty() {
            Some(service_name.to_string())
        } else {
            Some(fragment_path.to_string())
        },
        source_path: None,
        status: if active_state == "active" {
            ComponentStatus::Ok
        } else {
            ComponentStatus::Error
        },
    })
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

/// Find the path to the Grok Build binary.
async fn which_grok() -> Option<String> {
    which_binary("grok", &["/usr/local/bin/grok"]).await
}

/// Find the path to the Hermes assistant MCP connector.
async fn which_assistant_mcp() -> Option<String> {
    which_binary("assistant-mcp", &["/usr/local/bin/assistant-mcp"]).await
}

/// Find the path to the OpenCode binary.
/// Checks PATH first, then user-local install, then system-wide.
async fn which_opencode() -> Option<String> {
    let home = home_dir();
    let user_local = format!("{}/.opencode/bin/opencode", home);
    which_binary("opencode", &[&user_local, "/usr/local/bin/opencode"]).await
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

/// Check if there's a newer version of Claude Code available.
async fn check_claude_code_update(current_version: Option<&str>) -> Option<String> {
    let current = extract_version_token(current_version?)?;
    let desired = desired_claude_code_version();
    if current != desired {
        return Some(desired);
    }

    let latest_raw = fetch_npm_latest_version("@anthropic-ai/claude-code").await?;
    let latest = extract_version_token(&latest_raw)
        .unwrap_or_else(|| latest_raw.trim_start_matches('v').to_string());
    (latest != current && version_is_newer(&latest, &current)).then_some(latest)
}

fn desired_claude_code_version() -> String {
    std::env::var("SANDBOXED_SH_CLAUDECODE_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "2.1.139".to_string())
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

/// Check if there's a newer version of sandboxed.sh available.
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
///
/// A token qualifies only if it has at least one `digit.digit` pair, so stray
/// dots from paths (e.g. `~/.config`, `node_modules/.bin`) don't get picked up
/// as a "version" of `.`.
fn extract_version_token(input: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();

    for ch in input.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            current.push(ch);
            continue;
        }
        if let Some(token) = qualify_version_token(&current) {
            best = Some(token);
        }
        current.clear();
    }

    if let Some(token) = qualify_version_token(&current) {
        best = Some(token);
    }

    best.map(|v| v.trim_start_matches('v').to_string())
}

fn qualify_version_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let has_digit_dot_digit = bytes
        .windows(3)
        .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit());
    if has_digit_dot_digit {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Optional query params accepted by /components/:name/update.
#[derive(Debug, Deserialize)]
pub struct UpdateComponentQuery {
    /// When set, the update runs inside the named workspace's container instead of on the host.
    pub workspace_id: Option<String>,
}

/// Update a system component, either on the host or inside a specific container workspace.
async fn update_component(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<UpdateComponentQuery>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    // If a workspace is targeted, dispatch to per-workspace update for the supported components.
    if let Some(ws_id) = query.workspace_id.as_deref() {
        if !PER_WORKSPACE_COMPONENTS.contains(&name.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Component '{name}' does not support per-workspace updates"),
            ));
        }
        let uuid = uuid::Uuid::parse_str(ws_id).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid workspace_id: {ws_id}"),
            )
        })?;
        let workspace = state.workspaces.get(uuid).await.ok_or((
            StatusCode::NOT_FOUND,
            format!("Workspace not found: {ws_id}"),
        ))?;

        // Host workspaces share host binaries, so update the host instead.
        if workspace.workspace_type == WorkspaceType::Host {
            return host_update_stream(state, &name);
        }

        return Ok(Sse::new(Box::pin(stream_container_component_update(
            workspace, name,
        ))));
    }

    host_update_stream(state, &name)
}

/// Dispatch to the appropriate host-level update stream by component name.
fn host_update_stream(
    state: Arc<AppState>,
    name: &str,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    match name {
        "sandboxed_sh" => Ok(Sse::new(Box::pin(stream_sandboxed_update(state)))),
        "opencode" => Ok(Sse::new(Box::pin(stream_opencode_update()))),
        "claude_code" => Ok(Sse::new(Box::pin(stream_claude_code_update()))),
        "codex" => Ok(Sse::new(Box::pin(stream_codex_update()))),
        "grok" => Ok(Sse::new(Box::pin(stream_grok_update()))),
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", other),
        )),
    }
}

/// Run the install command for `component` inside `workspace`'s container, streaming progress.
fn stream_container_component_update(
    workspace: Workspace,
    component: String,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", format!("Updating {} inside workspace '{}'...", component, workspace.name), Some(0));

        if !crate::nspawn::nspawn_available() {
            yield sse("error", "systemd-nspawn is not available on this host.", None);
            return;
        }
        if workspace.status != WorkspaceStatus::Ready {
            yield sse("error", format!("Workspace '{}' is not ready (status: {:?})", workspace.name, workspace.status), None);
            return;
        }

        let install_cmd = match container_install_command(&component) {
            Some(cmd) => cmd,
            None => {
                yield sse("error", format!("No container install command defined for {component}"), None);
                return;
            }
        };

        yield sse("log", format!("Running: {}", install_cmd), Some(10));

        let config = crate::nspawn::NspawnConfig {
            env: workspace.env_vars.clone(),
            ..Default::default()
        };
        let cmd = vec!["sh".to_string(), "-lc".to_string(), install_cmd];

        let result = crate::nspawn::execute_in_container(&workspace.path, &cmd, &config).await;
        match result {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let summary: String = stdout.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                if !summary.trim().is_empty() {
                    yield sse("log", format!("Output: {}", summary), Some(80));
                }
                yield sse("complete", format!("{} updated inside '{}'", component, workspace.name), Some(100));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield sse("error", format!("Install failed: {} {}", stderr.trim(), stdout.trim()), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run install inside container: {}", e), None);
            }
        }
    }
}

/// Shell command used to install/update a component inside a container, run via `sh -lc`.
///
/// We mirror the host-side installers so a "sync" produces the same version as on host.
fn container_install_command(component: &str) -> Option<String> {
    match component {
        "claude_code" => Some(format!(
            "command -v bun >/dev/null 2>&1 && PM=bun || PM=npm; $PM install -g @anthropic-ai/claude-code@{}",
            desired_claude_code_version()
        )),
        "codex" => Some(
            "command -v bun >/dev/null 2>&1 && PM=bun || PM=npm; $PM install -g @openai/codex@latest".to_string(),
        ),
        "opencode" => Some(
            "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path".to_string(),
        ),
        "grok" => Some(
            "curl -fsSL https://x.ai/cli/install.sh | GROK_BIN_DIR=/usr/local/bin bash".to_string(),
        ),
        _ => None,
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
        "grok" => Ok(Sse::new(Box::pin(stream_grok_uninstall()))),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown component: {}", name),
        )),
    }
}

/// Stream the sandboxed.sh update process.
/// Builds from source using git tags (no pre-built binaries needed).
fn stream_sandboxed_update(
    state: Arc<AppState>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting sandboxed.sh update...", Some(0));

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
        yield sse("log", "Building sandboxed.sh (this may take a few minutes)...", Some(20));

        match Command::new("bash")
            .args(["-c", "source /root/.cargo/env && cargo build --bin sandboxed-sh --bin workspace-mcp --bin desktop-mcp --bin assistant-mcp"])
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

        // Versioned-symlink install: when enabled, write the new binary into
        // `/usr/local/bin/versions/<sha>/<exe_name>` and atomically retarget
        // a symlink at `install_dest` to it. This gives us:
        //   - rollback in one `ln -sfn` (no rebuild needed)
        //   - the bin/ dir doesn't fill with `.bak`/`.backup` clutter
        //   - a clear "the active version is wherever the symlink points"
        //
        // Opt-in via `SANDBOXED_SH_VERSIONED_INSTALL=1` so a host that's
        // never had this layout doesn't get a surprise symlink swap.
        let versioned_install = std::env::var("SANDBOXED_SH_VERSIONED_INSTALL")
            .ok()
            .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        // Stop the service before replacing the binary to avoid "Text file busy"
        let _ = Command::new("systemctl")
            .args(["stop", &service_name])
            .output()
            .await;

        let src = format!("{}/target/debug/{}", repo_path.display(), exe_name);

        let install_result = if versioned_install {
            install_versioned_binary(repo_path, &exe_name, &latest_tag, &install_dest).await
        } else {
            // Legacy path: write straight to `install_dest`. Keep until the
            // operator opts into versioned installs.
            Command::new("install")
                .args(["-m", "0755", &src, &install_dest])
                .output()
                .await
                .map(|o| if o.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&o.stderr).to_string())
                })
                .unwrap_or_else(|e| Err(e.to_string()))
        };

        match install_result {
            Ok(()) => {}
            Err(msg) => {
                yield sse("error", format!("Failed to install binary: {}", msg), None);
                let _ = Command::new("systemctl").args(["start", &service_name]).output().await;
                return;
            }
        }

        // Also install MCP binaries if they were built
        for mcp_bin in ["workspace-mcp", "desktop-mcp", "assistant-mcp"] {
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

/// Default debounce window between automated deploys. Agents loop fast;
/// without this, three missions all firing `deploy_sandboxed_sh` produce
/// three restarts in 90 seconds and kill every other in-flight turn.
const DEPLOY_DEBOUNCE_SECS: u64 = 300;

/// Marker file recording the wall-clock time of the last `/api/system/deploy`
/// invocation. Stored under the API's state dir; mtime is the only field that
/// matters. Persisted across restarts so the debounce survives the very
/// restart it just scheduled.
fn deploy_marker_path() -> std::path::PathBuf {
    // Match the existing /var/lib/sandboxed-sh convention if present (prod),
    // otherwise fall back to $HOME/.sandboxed-sh (dev / containers).
    let varlib = std::path::Path::new("/var/lib/sandboxed-sh");
    if varlib.exists() {
        return varlib.join("last_deploy");
    }
    std::path::PathBuf::from(home_dir())
        .join(".sandboxed-sh")
        .join("last_deploy")
}

/// Result of evaluating the deploy debounce. Tested in isolation so we can
/// trust the wall-clock math without touching the filesystem.
#[derive(Debug, PartialEq, Eq)]
enum DebounceDecision {
    Allow,
    /// Last deploy was `since_secs` ago, < `min_interval_secs`.
    RefuseTooRecent {
        since_secs: u64,
    },
}

fn evaluate_debounce(
    last_deploy_secs_ago: Option<u64>,
    min_interval_secs: u64,
    force: bool,
) -> DebounceDecision {
    if force {
        return DebounceDecision::Allow;
    }
    match last_deploy_secs_ago {
        Some(since) if since < min_interval_secs => {
            DebounceDecision::RefuseTooRecent { since_secs: since }
        }
        _ => DebounceDecision::Allow,
    }
}

/// Read `mtime` of the deploy marker, return seconds since it was written.
/// `None` if the file doesn't exist or its mtime is in the future (clock skew).
fn deploy_marker_age_secs(path: &std::path::Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    std::time::SystemTime::now()
        .duration_since(mtime)
        .ok()
        .map(|d| d.as_secs())
}

fn touch_deploy_marker(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    // Open with truncate to bump mtime; ignore any prior content.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("touch {}: {}", path.display(), e))?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeployRequest {
    /// Mission ID of the caller. Used for self-protection: if this mission
    /// is running on the same service we're about to restart, refuse unless
    /// `force=true` (the agent explicitly accepts that its own turn dies).
    #[serde(default)]
    pub calling_mission_id: Option<Uuid>,
    /// Bypass debounce + self-protection. Default false. Agents should only
    /// set this when they've explicitly decided the restart is worth it
    /// (e.g. emergency revert, the mission is about to finish anyway).
    #[serde(default)]
    pub force: bool,
    /// Optional git ref to check out before building. Defaults to whatever
    /// the local repo already has checked out (treat as "deploy current
    /// source state").
    #[serde(default)]
    pub git_ref: Option<String>,
    /// Skip the cargo build and assume the binaries at
    /// `<repo_path>/target/debug/` are current. Useful for flows that
    /// build elsewhere (e.g. CI, or a separate build worktree).
    #[serde(default)]
    pub skip_build: bool,
    /// Explicit source repo path. Overrides the server's configured
    /// `resolve_sandboxed_repo_path` default. Required when the agent's
    /// build artifact lives somewhere other than the server's default
    /// "sandboxed.sh source" location (e.g. an ad-hoc worktree under
    /// `/opt/sandboxed-sh-<name>/`).
    #[serde(default)]
    pub repo_path: Option<String>,
    /// Optional guard supplied by deploy tooling. When set, the API refuses
    /// if the request reached a different systemd service than intended.
    #[serde(default)]
    pub expected_service: Option<String>,
}

/// Reasons we may refuse a deploy without doing any I/O. Surfaced to the MCP
/// tool so the agent can decide whether to retry with `force=true`.
#[derive(Debug, PartialEq, Eq)]
enum DeployRefusal {
    /// The caller expected to deploy a different service than the API instance
    /// that received the request.
    WrongService { expected: String, actual: String },
    /// Calling mission lives on this service; restarting it would kill the
    /// caller. Returned as a refusal so an LLM can't accidentally request
    /// self-destruction; the agent can retry with `force=true` if it knows
    /// what it's doing.
    SelfTarget,
    /// Last deploy was too recent (see [`DEPLOY_DEBOUNCE_SECS`]).
    Debounced { since_secs: u64 },
}

impl DeployRefusal {
    fn http_status(&self) -> StatusCode {
        match self {
            DeployRefusal::WrongService { .. } => StatusCode::CONFLICT,
            DeployRefusal::SelfTarget => StatusCode::CONFLICT,
            DeployRefusal::Debounced { .. } => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    fn message(&self) -> String {
        match self {
            DeployRefusal::WrongService { expected, actual } => format!(
                "Deploy target mismatch: request expected {}, but this API would restart {}. \
                 Send the request to the correct environment/API URL.",
                expected, actual
            ),
            DeployRefusal::SelfTarget => {
                "Calling mission runs on the service this deploy would restart. \
                 Pass force=true if killing your own turn is acceptable, or run the deploy from a \
                 different service (e.g. dev → prod)."
                    .to_string()
            }
            DeployRefusal::Debounced { since_secs } => format!(
                "Last deploy was {}s ago; this service is in debounce window ({}s). \
                 Pass force=true to override.",
                since_secs, DEPLOY_DEBOUNCE_SECS
            ),
        }
    }
}

/// Pure helper exercised by tests. Returns the refusal that should fire (if
/// any) given the inputs the handler computed from state + request.
fn evaluate_deploy_request(
    actual_service: Option<&str>,
    expected_service: Option<&str>,
    calling_mission_on_this_service: bool,
    last_deploy_secs_ago: Option<u64>,
    force: bool,
) -> Option<DeployRefusal> {
    if let (Some(actual), Some(expected)) = (actual_service, expected_service) {
        if actual != expected {
            return Some(DeployRefusal::WrongService {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    }
    if !force && calling_mission_on_this_service {
        return Some(DeployRefusal::SelfTarget);
    }
    match evaluate_debounce(last_deploy_secs_ago, DEPLOY_DEBOUNCE_SECS, force) {
        DebounceDecision::Allow => None,
        DebounceDecision::RefuseTooRecent { since_secs } => {
            Some(DeployRefusal::Debounced { since_secs })
        }
    }
}

/// Hot-swap-with-rails entry point invoked by the orchestrator MCP's
/// `deploy_sandboxed_sh` tool.
///
/// Differences from `/components/sandboxed_sh/update`:
///   - Self-protection: refuses to restart the very service hosting the
///     caller unless `force=true`.
///   - Debounce: refuses to restart twice within [`DEPLOY_DEBOUNCE_SECS`]
///     unless `force=true`.
///   - Detached restart: schedules the systemctl restart via a backgrounded
///     `setsid`/`nohup` so the SSE response can flush before the process
///     dies. (The existing update endpoint kills the SSE mid-stream.)
///
/// Self-protection is checked synchronously and returns 409 before any
/// disk work happens, so a misfiring agent can't accidentally chainsaw
/// the host by retrying in a loop.
pub async fn deploy_sandboxed_sh(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DeployRequest>,
) -> Result<Sse<UpdateStream>, (StatusCode, String)> {
    let actual_service =
        current_sandboxed_service_name().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Synchronous safety checks BEFORE we open SSE. A 4xx here is easier for
    // the MCP to surface than an early-error SSE event.
    let calling_on_self = match req.calling_mission_id {
        None => false,
        Some(mid) => {
            // The simplest "is this mission on my service?" check is "does
            // this API instance's mission_store know about it?". A
            // cross-service deployer (dev → prod) hits prod's API with a
            // mission that lives on dev — prod's store won't have it, so
            // self-protection won't fire. That's the correct outcome.
            let store = state.control.get_mission_store().await;
            store.get_mission(mid).await.ok().flatten().is_some()
        }
    };

    let marker = deploy_marker_path();
    let last_age = deploy_marker_age_secs(&marker);

    if let Some(refusal) = evaluate_deploy_request(
        Some(&actual_service),
        req.expected_service.as_deref(),
        calling_on_self,
        last_age,
        req.force,
    ) {
        return Err((refusal.http_status(), refusal.message()));
    }

    // Record the deploy intent BEFORE the actual work so a crash mid-build
    // still counts as "recently attempted" for debounce purposes. The mtime
    // is what matters; content is unused.
    if let Err(e) = touch_deploy_marker(&marker) {
        tracing::warn!(
            "deploy: failed to touch debounce marker {}: {}",
            marker.display(),
            e
        );
    }

    Ok(Sse::new(Box::pin(stream_deploy(
        state,
        req,
        actual_service,
    ))))
}

fn current_sandboxed_service_name() -> Result<String, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to detect current binary path: {}", e))?;
    let exe_name = current_exe
        .file_name()
        .ok_or_else(|| {
            format!(
                "Current binary path has no file name: {}",
                current_exe.display()
            )
        })?
        .to_string_lossy()
        .to_string();
    Ok(format!("{}.service", exe_name))
}

/// The actual deploy stream — git checkout (optional), build (optional),
/// versioned install, then a detached `systemctl restart`. Mirrors
/// `stream_sandboxed_update` but skips the "stop service first" step so the
/// SSE response can deliver the final "deployed" event before the new
/// binary takes over.
fn stream_deploy(
    state: Arc<AppState>,
    req: DeployRequest,
    service_name: String,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting deploy with safety rails", Some(0));

        let repo_path_str = match req.repo_path.as_deref() {
            Some(p) => p.to_string(),
            None => resolve_sandboxed_repo_path(&state).await,
        };
        let repo_path = std::path::Path::new(&repo_path_str);

        if let Err(err) = ensure_repo_present(repo_path).await {
            yield sse("error", format!("Failed to prepare source repo at {}: {}", repo_path.display(), err), None);
            return;
        }
        yield sse("log", format!("Source repo: {}", repo_path.display()), Some(5));

        if let Some(git_ref) = req.git_ref.as_deref() {
            yield sse("log", format!("Fetching + checking out {}", git_ref), Some(10));
            let fetch = Command::new("git")
                .args(["fetch", "--tags", "origin"])
                .current_dir(repo_path)
                .output()
                .await;
            if let Ok(o) = fetch {
                if !o.status.success() {
                    yield sse("error", format!("git fetch failed: {}", String::from_utf8_lossy(&o.stderr)), None);
                    return;
                }
            }
            let checkout = Command::new("git")
                .args(["checkout", git_ref])
                .current_dir(repo_path)
                .output()
                .await;
            match checkout {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    yield sse("error", format!("git checkout {} failed: {}", git_ref, String::from_utf8_lossy(&o.stderr)), None);
                    return;
                }
                Err(e) => {
                    yield sse("error", format!("git checkout {} error: {}", git_ref, e), None);
                    return;
                }
            }
        }

        let current_exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                yield sse("error", format!("Failed to detect current binary path: {}", e), None);
                return;
            }
        };
        // The cargo bin name is always `sandboxed-sh` regardless of how we
        // rename it on install (e.g. `sandboxed-sh-prod` or `sandboxed-sh-dev`).
        // Same for MCP binaries.
        const MAIN_CARGO_BIN: &str = "sandboxed-sh";
        const MCP_CARGO_BIN: &str = "orchestrator-mcp";
        const ASSISTANT_MCP_CARGO_BIN: &str = "assistant-mcp";
        let install_dest_main = current_exe.to_string_lossy().to_string();
        // Match the MCP install location: same dir as the main binary, fixed name.
        let install_dest_mcp = current_exe
            .parent()
            .map(|p| p.join(MCP_CARGO_BIN).to_string_lossy().to_string())
            .unwrap_or_else(|| format!("/usr/local/bin/{}", MCP_CARGO_BIN));
        let install_dest_assistant_mcp = current_exe
            .parent()
            .map(|p| p.join(ASSISTANT_MCP_CARGO_BIN).to_string_lossy().to_string())
            .unwrap_or_else(|| format!("/usr/local/bin/{}", ASSISTANT_MCP_CARGO_BIN));

        if !req.skip_build {
            yield sse("log", format!("Building {} + {} + {} (cargo build, debug)", MAIN_CARGO_BIN, MCP_CARGO_BIN, ASSISTANT_MCP_CARGO_BIN), Some(25));
            let build_cmd = format!(
                "source /root/.cargo/env 2>/dev/null; cargo build --bin {} --bin {} --bin {}",
                MAIN_CARGO_BIN, MCP_CARGO_BIN, ASSISTANT_MCP_CARGO_BIN
            );
            match Command::new("bash")
                .args(["-c", &build_cmd])
                .current_dir(repo_path)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    yield sse("log", "Build complete", Some(60));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let tail: Vec<&str> = stderr.lines().rev().take(15).collect();
                    let summary = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                    yield sse("error", format!("Build failed:\n{}", summary), None);
                    return;
                }
                Err(e) => {
                    yield sse("error", format!("cargo build error: {}", e), None);
                    return;
                }
            }
        } else {
            yield sse("log", "skip_build=true; using existing target/debug/ binaries", Some(60));
        }

        // Verify both source binaries exist *before* touching anything live.
        // Surfaces the "you pointed me at a path with no build" case as a
        // clean refusal instead of a half-applied deploy.
        let src_main = repo_path.join("target").join("debug").join(MAIN_CARGO_BIN);
        let src_mcp = repo_path.join("target").join("debug").join(MCP_CARGO_BIN);
        let src_assistant_mcp = repo_path
            .join("target")
            .join("debug")
            .join(ASSISTANT_MCP_CARGO_BIN);
        if !src_main.exists() {
            yield sse("error", format!("Build artifact missing: {}. Either set skip_build=false, or point repo_path at a checkout that has been built.", src_main.display()), None);
            return;
        }
        if !src_mcp.exists() {
            yield sse("error", format!("Build artifact missing: {}. Either set skip_build=false, or point repo_path at a checkout that has been built.", src_mcp.display()), None);
            return;
        }
        if !src_assistant_mcp.exists() {
            yield sse("error", format!("Build artifact missing: {}. Either set skip_build=false, or point repo_path at a checkout that has been built.", src_assistant_mcp.display()), None);
            return;
        }

        // Resolve commit sha for the "deployed" event so the agent has
        // something concrete to confirm.
        let sha = match Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .current_dir(repo_path)
            .output()
            .await
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => "unknown".to_string(),
        };

        // We do NOT use install_versioned_binary here because that helper
        // assumes the source filename equals the deployed filename (e.g.
        // src=target/debug/sandboxed-sh-prod, dest=/usr/local/bin/sandboxed-sh-prod).
        // The cargo bin is always `sandboxed-sh`; the deployed name is
        // service-specific. Doing a direct `install -m 0755 <src> <dest>`
        // gives us the rename for free.
        //
        // Backup the live binaries to `.pre-deploy-<sha>` so a one-line
        // rollback is `mv backup live && systemctl restart`. No versioned
        // dir scheme — the existing manual ops use the .backup-<ts>
        // convention, so we match it.
        yield sse("log", format!("Installing {} → {}", src_main.display(), install_dest_main), Some(75));
        let bkp_main = format!("{}.pre-deploy-{}", install_dest_main, sha);
        if std::path::Path::new(&install_dest_main).exists() {
            let _ = tokio::fs::copy(&install_dest_main, &bkp_main).await;
        }
        let install_main = Command::new("install")
            .args([
                "-m", "0755",
                src_main.to_string_lossy().as_ref(),
                &install_dest_main,
            ])
            .output()
            .await;
        match install_main {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                yield sse("error", format!("Install of main binary failed: {}", String::from_utf8_lossy(&o.stderr)), None);
                return;
            }
            Err(e) => {
                yield sse("error", format!("install command error: {}", e), None);
                return;
            }
        }

        yield sse("log", format!("Installing {} → {}", src_mcp.display(), install_dest_mcp), Some(82));
        let bkp_mcp = format!("{}.pre-deploy-{}", install_dest_mcp, sha);
        if std::path::Path::new(&install_dest_mcp).exists() {
            let _ = tokio::fs::copy(&install_dest_mcp, &bkp_mcp).await;
        }
        let install_mcp = Command::new("install")
            .args([
                "-m", "0755",
                src_mcp.to_string_lossy().as_ref(),
                &install_dest_mcp,
            ])
            .output()
            .await;
        match install_mcp {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                // Roll back the main binary swap before bailing — leaving
                // the main binary swapped without its matching MCP is the
                // worst possible half-applied state.
                let _ = tokio::fs::rename(&bkp_main, &install_dest_main).await;
                yield sse("error", format!("Install of orchestrator-mcp failed (main binary rolled back): {}", String::from_utf8_lossy(&o.stderr)), None);
                return;
            }
            Err(e) => {
                let _ = tokio::fs::rename(&bkp_main, &install_dest_main).await;
                yield sse("error", format!("install command error (main rolled back): {}", e), None);
                return;
            }
        }

        yield sse("log", format!("Installing {} → {}", src_assistant_mcp.display(), install_dest_assistant_mcp), Some(85));
        let bkp_assistant_mcp = format!("{}.pre-deploy-{}", install_dest_assistant_mcp, sha);
        if std::path::Path::new(&install_dest_assistant_mcp).exists() {
            let _ = tokio::fs::copy(&install_dest_assistant_mcp, &bkp_assistant_mcp).await;
        }
        let install_assistant_mcp = Command::new("install")
            .args([
                "-m", "0755",
                src_assistant_mcp.to_string_lossy().as_ref(),
                &install_dest_assistant_mcp,
            ])
            .output()
            .await;
        match install_assistant_mcp {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = tokio::fs::rename(&bkp_main, &install_dest_main).await;
                let _ = tokio::fs::rename(&bkp_mcp, &install_dest_mcp).await;
                yield sse("error", format!("Install of assistant-mcp failed (main/orchestrator binaries rolled back): {}", String::from_utf8_lossy(&o.stderr)), None);
                return;
            }
            Err(e) => {
                let _ = tokio::fs::rename(&bkp_main, &install_dest_main).await;
                let _ = tokio::fs::rename(&bkp_mcp, &install_dest_mcp).await;
                yield sse("error", format!("install command error for assistant-mcp (main/orchestrator rolled back): {}", e), None);
                return;
            }
        }
        yield sse("log", format!("Backups: {}, {}, {}", bkp_main, bkp_mcp, bkp_assistant_mcp), Some(88));

        // Schedule the restart in a fully detached process so this SSE
        // response can flush its final event before systemd SIGTERMs us.
        // `setsid` + `nohup` + `&` puts the restart in a new session that
        // outlives the API process, so the queued `systemctl restart` runs
        // even after our PID exits.
        let restart_cmd = format!(
            "sleep 2 && systemctl restart {} >/dev/null 2>&1",
            service_name
        );
        if let Err(e) = Command::new("setsid")
            .args(["nohup", "bash", "-c", &restart_cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            yield sse(
                "error",
                format!("Binary installed but failed to schedule restart: {}. Run `systemctl restart {}` manually.", e, service_name),
                None,
            );
            return;
        }

        yield sse(
            "deployed",
            format!(
                "Deployed commit {}; service {} will restart in ~2s",
                sha, service_name
            ),
            Some(100),
        );
        // Give the client a beat to receive the final event before the
        // restart tears down our TCP connection.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}

/// Install a new binary into a versioned dir and flip a symlink at
/// `install_dest` to point at it. The version dir lives under
/// `<install_dest_parent>/versions/<tag>/` so a single `ls` shows what's
/// deployable and rolling back is one symlink retarget.
///
/// Steps (each one tolerates partial-failure by leaving the previous
/// symlink target intact):
///   1. mkdir -p versions/<tag>
///   2. install --mode 0755 target/debug/<exe> -> versions/<tag>/<exe>
///   3. ln -sfn versions/<tag>/<exe>  install_dest
///   4. update `versions/current` text file (for ops visibility)
///
/// On first run against an existing real-file install, the live binary at
/// `install_dest` is moved aside into `versions/legacy/<exe>` and the
/// symlink is created. After that, every deploy is just step 3.
async fn install_versioned_binary(
    repo_path: &std::path::Path,
    exe_name: &str,
    tag: &str,
    install_dest: &str,
) -> Result<(), String> {
    use std::path::PathBuf;

    let dest_path = PathBuf::from(install_dest);
    let parent = dest_path
        .parent()
        .ok_or_else(|| format!("install_dest has no parent: {}", install_dest))?;
    let versions_root = parent.join("versions");
    // Sanitize tag: refuse `..`, `/`, or empty values so an attacker who
    // can influence the tag string can't write outside `versions/`.
    let safe_tag = tag.trim();
    if safe_tag.is_empty() || safe_tag.contains('/') || safe_tag.contains("..") {
        return Err(format!("refusing unsafe version tag: {:?}", safe_tag));
    }
    let version_dir = versions_root.join(safe_tag);

    tokio::fs::create_dir_all(&version_dir)
        .await
        .map_err(|e| format!("create_dir_all {}: {}", version_dir.display(), e))?;

    // If the live file is a real binary (not a symlink), preserve it under
    // versions/legacy/ so a rollback is possible even though we didn't
    // version it ourselves.
    let live_meta = tokio::fs::symlink_metadata(&dest_path).await.ok();
    if let Some(meta) = live_meta.as_ref() {
        if !meta.file_type().is_symlink() && meta.file_type().is_file() {
            let legacy_dir = versions_root.join("legacy");
            tokio::fs::create_dir_all(&legacy_dir)
                .await
                .map_err(|e| format!("create_dir_all {}: {}", legacy_dir.display(), e))?;
            let legacy_path = legacy_dir.join(exe_name);
            // Best-effort copy — we don't fail the deploy if the legacy
            // archive step fails; the new symlink swap below is what
            // actually has to work.
            let _ = tokio::fs::copy(&dest_path, &legacy_path).await;
        }
    }

    // Install the freshly-built binary into the version dir.
    let src = repo_path.join("target").join("debug").join(exe_name);
    let target = version_dir.join(exe_name);
    let install_status = tokio::process::Command::new("install")
        .args([
            "-m",
            "0755",
            src.to_string_lossy().as_ref(),
            target.to_string_lossy().as_ref(),
        ])
        .output()
        .await
        .map_err(|e| format!("install: {}", e))?;
    if !install_status.status.success() {
        return Err(String::from_utf8_lossy(&install_status.stderr).to_string());
    }

    // `ln -sfn` is the standard "atomic-ish" symlink retarget. `-n` makes
    // it treat an existing symlink-to-directory as a plain symlink (so we
    // overwrite it instead of creating a link *inside* it). The kernel
    // implements `symlink(2)` over a tmpfile + rename, so the swap is
    // visible to other processes as a single transition.
    let ln_status = tokio::process::Command::new("ln")
        .args([
            "-sfn",
            target.to_string_lossy().as_ref(),
            dest_path.to_string_lossy().as_ref(),
        ])
        .output()
        .await
        .map_err(|e| format!("ln: {}", e))?;
    if !ln_status.status.success() {
        return Err(String::from_utf8_lossy(&ln_status.stderr).to_string());
    }

    // Ops-visible "what's deployed right now" file. Best-effort.
    let _ = tokio::fs::write(versions_root.join("current"), format!("{}\n", safe_tag)).await;
    Ok(())
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
        let desired_version = desired_claude_code_version();

        let pm = crate::pkg_manager::preferred().await;
        let Some(pm) = pm else {
            yield sse("error", "No package manager (bun or npm) found. Please install Bun or Node.js first.", None);
            return;
        };

        yield sse("log", format!("Installing @anthropic-ai/claude-code@{} globally via {}...", desired_version, pm.bin()), Some(20));
        let package = format!("@anthropic-ai/claude-code@{}", desired_version);

        match Command::new(pm.bin())
            .args(pm.global_install_args(&package))
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

/// Stream the Grok Build install/update process.
fn stream_grok_update() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Grok Build installation/update...", Some(0));

        match Command::new("bash")
            .args(["-lc", "curl -fsSL https://x.ai/cli/install.sh | GROK_BIN_DIR=/usr/local/bin bash"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                yield sse("log", "Installation complete, verifying...", Some(80));

                let version = Command::new("grok").arg("--version").output().await
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
                    yield sse("complete", format!("Grok Build installed successfully! Version: {version}"), Some(100));
                } else {
                    yield sse("complete", "Grok Build installed, but version check failed. You may need to restart your shell.", Some(100));
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                yield sse("error", format!("Failed to install Grok Build: {} {}", stderr.trim(), stdout.trim()), None);
            }
            Err(e) => {
                yield sse("error", format!("Failed to run Grok Build installer: {}", e), None);
            }
        }
    }
}

/// Stream the Grok Build uninstall process.
fn stream_grok_uninstall() -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        yield sse("log", "Starting Grok Build uninstall...", Some(0));

        for path in ["/usr/local/bin/grok", "/usr/bin/grok"] {
            if std::path::Path::new(path).exists() {
                match Command::new("rm").args(["-f", path]).output().await {
                    Ok(output) if output.status.success() => {
                        yield sse("log", format!("Removed {path}"), Some(60));
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        yield sse("error", format!("Failed to remove {path}: {}", stderr.trim()), None);
                        return;
                    }
                    Err(e) => {
                        yield sse("error", format!("Failed to remove {path}: {}", e), None);
                        return;
                    }
                }
            }
        }

        yield sse("complete", "Grok Build uninstalled successfully.", Some(100));
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

#[cfg(test)]
mod tests {
    use super::{
        evaluate_debounce, evaluate_deploy_request, extract_version_token, is_safe_repo_path,
        normalize_repo_path, select_repo_path, systemd_service_component_from_states,
        ComponentStatus, DebounceDecision, DeployRefusal, DEPLOY_DEBOUNCE_SECS,
    };

    // ─── Deploy safety rails ────────────────────────────────────────────────

    #[test]
    fn systemd_service_component_reports_active_service_ok() {
        let component = systemd_service_component_from_states(
            "hermes_assistant",
            "hermes-assistant-dev.service",
            "loaded",
            "active",
            "/etc/systemd/system/hermes-assistant-dev.service",
        )
        .expect("loaded service should be reported");

        assert_eq!(component.name, "hermes_assistant");
        assert!(component.installed);
        assert_eq!(
            component.path.as_deref(),
            Some("/etc/systemd/system/hermes-assistant-dev.service")
        );
        assert!(matches!(component.status, ComponentStatus::Ok));
    }

    #[test]
    fn systemd_service_component_reports_loaded_inactive_service_error() {
        let component = systemd_service_component_from_states(
            "hermes_assistant",
            "hermes-assistant-dev.service",
            "loaded",
            "inactive",
            "",
        )
        .expect("loaded inactive service should be visible");

        assert!(component.installed);
        assert_eq!(
            component.path.as_deref(),
            Some("hermes-assistant-dev.service")
        );
        assert!(matches!(component.status, ComponentStatus::Error));
    }

    #[test]
    fn systemd_service_component_ignores_unloaded_services() {
        let component = systemd_service_component_from_states(
            "hermes_assistant",
            "hermes-assistant-dev.service",
            "not-found",
            "inactive",
            "",
        );

        assert!(component.is_none());
    }

    #[test]
    fn debounce_allows_when_no_prior_deploy() {
        assert_eq!(evaluate_debounce(None, 300, false), DebounceDecision::Allow);
    }

    #[test]
    fn debounce_allows_when_outside_window() {
        assert_eq!(
            evaluate_debounce(Some(301), 300, false),
            DebounceDecision::Allow
        );
        assert_eq!(
            evaluate_debounce(Some(3_600), 300, false),
            DebounceDecision::Allow
        );
    }

    #[test]
    fn debounce_refuses_when_inside_window() {
        assert_eq!(
            evaluate_debounce(Some(60), 300, false),
            DebounceDecision::RefuseTooRecent { since_secs: 60 }
        );
        assert_eq!(
            evaluate_debounce(Some(0), 300, false),
            DebounceDecision::RefuseTooRecent { since_secs: 0 }
        );
    }

    #[test]
    fn debounce_force_overrides_window() {
        assert_eq!(
            evaluate_debounce(Some(0), 300, true),
            DebounceDecision::Allow
        );
        assert_eq!(
            evaluate_debounce(Some(60), 300, true),
            DebounceDecision::Allow
        );
    }

    #[test]
    fn deploy_refuses_self_target_by_default() {
        let r = evaluate_deploy_request(None, None, true, None, false);
        assert_eq!(r, Some(DeployRefusal::SelfTarget));
    }

    #[test]
    fn deploy_self_target_force_allows() {
        // force=true bypasses self-protection (caller explicitly accepts
        // the in-flight turn dying). Still respects debounce unless the
        // debounce is also force-bypassed, which it is.
        assert_eq!(evaluate_deploy_request(None, None, true, None, true), None);
    }

    #[test]
    fn deploy_cross_service_no_self_protection() {
        // calling_on_self=false → no self-target refusal, no debounce
        // hit, no refusal at all.
        assert_eq!(
            evaluate_deploy_request(None, None, false, None, false),
            None
        );
        assert_eq!(
            evaluate_deploy_request(None, None, false, Some(10_000), false),
            None
        );
    }

    #[test]
    fn deploy_debounce_kicks_in_after_self_protection_passes() {
        // calling_on_self=false, but a deploy fired 30s ago — debounce
        // should refuse even though the self check passed.
        assert_eq!(
            evaluate_deploy_request(None, None, false, Some(30), false),
            Some(DeployRefusal::Debounced { since_secs: 30 })
        );
    }

    #[test]
    fn deploy_force_bypasses_both_self_and_debounce() {
        assert_eq!(
            evaluate_deploy_request(None, None, true, Some(0), true),
            None
        );
    }

    #[test]
    fn deploy_self_protection_checked_before_debounce() {
        // When both refusals would fire, return the more semantically
        // meaningful one (self-target) so the agent sees the actual reason
        // instead of being told "wait a bit and retry" only to discover
        // it'd kill itself.
        let r = evaluate_deploy_request(None, None, true, Some(30), false);
        assert_eq!(r, Some(DeployRefusal::SelfTarget));
    }

    #[test]
    fn deploy_refuses_wrong_expected_service() {
        let r = evaluate_deploy_request(
            Some("sandboxed-sh-prod.service"),
            Some("sandboxed-sh-dev.service"),
            false,
            None,
            false,
        );
        assert_eq!(
            r,
            Some(DeployRefusal::WrongService {
                expected: "sandboxed-sh-dev.service".to_string(),
                actual: "sandboxed-sh-prod.service".to_string(),
            })
        );
    }

    #[test]
    fn deploy_expected_service_match_allows_next_checks() {
        let r = evaluate_deploy_request(
            Some("sandboxed-sh-dev.service"),
            Some("sandboxed-sh-dev.service"),
            false,
            None,
            false,
        );
        assert_eq!(r, None);
    }

    #[test]
    fn deploy_refusal_self_target_returns_409() {
        assert_eq!(
            DeployRefusal::SelfTarget.http_status(),
            axum::http::StatusCode::CONFLICT
        );
    }

    #[test]
    fn deploy_refusal_debounced_returns_429() {
        assert_eq!(
            DeployRefusal::Debounced { since_secs: 10 }.http_status(),
            axum::http::StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn deploy_refusal_messages_mention_force_override() {
        // Both refusals should tell the caller how to override, otherwise
        // an LLM with no context will retry the same request forever.
        assert!(DeployRefusal::SelfTarget.message().contains("force=true"));
        assert!(DeployRefusal::Debounced { since_secs: 5 }
            .message()
            .contains("force=true"));
    }

    #[test]
    fn deploy_debounce_constant_at_least_one_minute() {
        // Sanity: a value below 60s would render the safety useless given
        // typical agent retry behavior. If you genuinely need to lower
        // this, change the test deliberately.
        let debounce_secs = DEPLOY_DEBOUNCE_SECS;
        assert!(debounce_secs >= 60);
    }

    // ─── Pre-existing helpers ───────────────────────────────────────────────

    #[test]
    fn extract_version_token_basic_semver() {
        assert_eq!(
            extract_version_token("opencode v1.4.0"),
            Some("1.4.0".to_string())
        );
        assert_eq!(
            extract_version_token("v0.128.0\n"),
            Some("0.128.0".to_string())
        );
    }

    #[test]
    fn extract_version_token_ignores_lone_dot_from_paths() {
        // Was returning Some(".") before — paths in CLI output should never
        // qualify as a version.
        assert_eq!(extract_version_token("/root/.config/opencode"), None);
        assert_eq!(extract_version_token("node_modules/.bin/foo"), None);
        assert_eq!(extract_version_token("Could not find ~/.opencode/"), None);
    }

    #[test]
    fn extract_version_token_prefers_last_semver_in_input() {
        assert_eq!(
            extract_version_token("warning at line 1.2 — installed v3.4.5"),
            Some("3.4.5".to_string())
        );
    }

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

    #[test]
    fn safe_repo_path_rejects_sensitive_hidden_subdirectories() {
        assert!(!is_safe_repo_path(std::path::Path::new("/root/.ssh")));
        assert!(!is_safe_repo_path(std::path::Path::new(
            "/opt/.cache/sandboxed-sh"
        )));
    }

    #[test]
    fn safe_repo_path_accepts_default_repo_location() {
        assert!(is_safe_repo_path(std::path::Path::new(
            crate::settings::DEFAULT_SANDBOXED_REPO_PATH
        )));
    }
}
