//! Remote runner nodes: core-side configuration, wire protocol, HTTP client
//! and fleet monitoring for the `sandboxed-node` runner binary.
//!
//! Module layout:
//! - [`protocol`]: shared wire types (heartbeat, leases, job payloads) used by
//!   both core and the node binary.
//! - [`client`]: core-side HTTP client for talking to nodes.
//! - [`monitor`]: background fleet monitor caching per-node statuses and
//!   recent dispatch outcomes.
//!
//! Everything public is re-exported here so call sites keep using
//! `crate::remote_node::*` unchanged.

pub mod client;
pub mod job_ledger;
pub mod monitor;
pub mod protocol;

pub use client::*;
pub use monitor::*;
pub use protocol::*;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteNodeStatus {
    Disabled,
    Unknown,
    Online,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNodeOverview {
    pub enabled: bool,
    pub configured_nodes: usize,
    pub status: RemoteNodeStatus,
    pub notes: Vec<String>,
}

impl RemoteNodeOverview {
    pub fn from_env() -> Self {
        match RemoteNodeSettings::from_env() {
            Ok(settings) => Self::from_settings(&settings),
            Err(err) => Self {
                enabled: false,
                configured_nodes: 0,
                status: RemoteNodeStatus::Degraded,
                notes: vec![format!("Remote node configuration is invalid: {err}")],
            },
        }
    }

    pub fn from_settings(settings: &RemoteNodeSettings) -> Self {
        let mut notes = Vec::new();
        if !settings.enabled {
            notes.push(
                "Remote mission nodes are disabled; local/container execution is unchanged."
                    .to_string(),
            );
        } else if settings.nodes.is_empty() {
            notes.push(
                "Remote nodes are enabled, but no node endpoints are configured.".to_string(),
            );
        } else {
            notes.push(
                "Remote node MVP is enabled for explicit selected-node missions.".to_string(),
            );
        }
        let status = if settings.enabled {
            // Derive the live status from the fleet monitor cache when
            // available instead of the old hardcoded `Unknown`.
            monitor::global_fleet()
                .and_then(|fleet| fleet.aggregate_status())
                .unwrap_or(RemoteNodeStatus::Unknown)
        } else {
            RemoteNodeStatus::Disabled
        };
        Self {
            enabled: settings.enabled,
            configured_nodes: settings.nodes.len(),
            status,
            notes,
        }
    }
}

#[derive(Debug, Error)]
pub enum RemoteNodeError {
    #[error("remote nodes are disabled; set SANDBOXED_REMOTE_NODES_ENABLED=true")]
    Disabled,
    #[error("remote node '{0}' is not configured")]
    UnknownNode(String),
    #[error("remote node '{0}' has no token in {1}")]
    MissingToken(String, String),
    #[error("invalid remote node config: {0}")]
    InvalidConfig(String),
    #[error("remote node request failed: {0}")]
    Request(String),
    #[error("remote node rejected lease: {0}")]
    Rejected(String),
    #[error("invalid lease token: {0}")]
    InvalidLease(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteNodeConfig {
    pub id: String,
    pub base_url: String,
    pub token_env: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteNodeSettings {
    pub enabled: bool,
    pub nodes: Vec<RemoteNodeConfig>,
}

impl RemoteNodeSettings {
    pub fn from_env() -> Result<Self, RemoteNodeError> {
        Self::from_raw(
            parse_bool_env("SANDBOXED_REMOTE_NODES_ENABLED").unwrap_or(false),
            std::env::var("SANDBOXED_REMOTE_NODES").ok(),
        )
    }

    fn from_raw(enabled: bool, raw: Option<String>) -> Result<Self, RemoteNodeError> {
        let raw = match raw {
            Some(raw) if !raw.trim().is_empty() => raw,
            _ => {
                return Ok(Self {
                    enabled,
                    nodes: vec![],
                })
            }
        };
        match parse_node_list(&raw) {
            Ok(nodes) => Ok(Self { enabled, nodes }),
            // A stale or legacy SANDBOXED_REMOTE_NODES value (e.g. the old
            // URL-only format) must not prevent core startup while the
            // feature is disabled.
            Err(_) if !enabled => Ok(Self {
                enabled,
                nodes: vec![],
            }),
            Err(err) => Err(err),
        }
    }

    pub fn node(&self, id: &str) -> Option<&RemoteNodeConfig> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

fn parse_bool_env(key: &str) -> Option<bool> {
    let raw = std::env::var(key).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub fn default_token_env(node_id: &str) -> String {
    let suffix = node_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("SANDBOXED_REMOTE_NODE_{}_TOKEN", suffix)
}

pub fn parse_node_list(raw: &str) -> Result<Vec<RemoteNodeConfig>, RemoteNodeError> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (id, rest) = entry.split_once('=').ok_or_else(|| {
                RemoteNodeError::InvalidConfig(format!(
                    "entry '{entry}' must be id=url or id=url|TOKEN_ENV"
                ))
            })?;
            let id = id.trim();
            if id.is_empty() {
                return Err(RemoteNodeError::InvalidConfig(
                    "node id cannot be empty".to_string(),
                ));
            }
            let mut parts = rest.split('|').map(str::trim);
            let base_url = parts
                .next()
                .filter(|url| !url.is_empty())
                .ok_or_else(|| RemoteNodeError::InvalidConfig(format!("{id} has empty url")))?;
            let token_env = parts
                .next()
                .filter(|token_env| !token_env.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| default_token_env(id));
            if parts.next().is_some() {
                return Err(RemoteNodeError::InvalidConfig(format!(
                    "{id} has too many pipe-separated fields"
                )));
            }
            Ok(RemoteNodeConfig {
                id: id.to_string(),
                base_url: base_url.trim_end_matches('/').to_string(),
                token_env,
                labels: None,
            })
        })
        .collect()
}

pub fn placement_for_selected_node<'a>(
    settings: &'a RemoteNodeSettings,
    selected_node_id: Option<&str>,
) -> Result<Option<&'a RemoteNodeConfig>, RemoteNodeError> {
    let Some(node_id) = selected_node_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    if !settings.enabled {
        return Err(RemoteNodeError::Disabled);
    }
    settings
        .node(node_id)
        .map(Some)
        .ok_or_else(|| RemoteNodeError::UnknownNode(node_id.to_string()))
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub async fn run_lease_command(
    node_id: &str,
    token_secret: &str,
    work_root: PathBuf,
    request: LeaseRequest,
) -> Result<ExecuteResponse, RemoteNodeError> {
    let claims = validate_lease_token(
        &request.lease_token,
        token_secret,
        node_id,
        SCOPE_MISSION_EXECUTE,
        chrono::Utc::now(),
    )?;
    if claims.mission_id != request.mission_id {
        return Err(RemoteNodeError::InvalidLease(
            "lease is scoped to a different mission".to_string(),
        ));
    }
    let mission_dir = work_root.join(request.mission_id.to_string());
    tokio::fs::create_dir_all(&mission_dir)
        .await
        .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
    let output = Command::new("bash")
        .arg("-lc")
        .arg(&request.command)
        .current_dir(&mission_dir)
        .output()
        .await
        .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
    Ok(ExecuteResponse {
        accepted: true,
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn parses_env_node_list_with_default_token_env() {
        let nodes = parse_node_list(
            "babylon=http://54.36.175.109:3088,nippur=http://37.187.92.183:3088|NIPPUR_TOKEN",
        )
        .unwrap();
        assert_eq!(nodes[0].id, "babylon");
        assert_eq!(nodes[0].token_env, "SANDBOXED_REMOTE_NODE_BABYLON_TOKEN");
        assert_eq!(nodes[1].token_env, "NIPPUR_TOKEN");
    }

    #[test]
    fn disabled_settings_tolerate_legacy_node_list_format() {
        let settings =
            RemoteNodeSettings::from_raw(false, Some("http://n1:9100,http://n2:9100".to_string()))
                .unwrap();
        assert!(!settings.enabled);
        assert!(settings.nodes.is_empty());

        assert!(RemoteNodeSettings::from_raw(
            true,
            Some("http://n1:9100,http://n2:9100".to_string())
        )
        .is_err());
    }

    #[tokio::test]
    async fn run_lease_command_rejects_mismatched_mission_id() {
        let claims = LeaseClaims {
            mission_id: Uuid::new_v4(),
            node_id: "babylon".to_string(),
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            expires_at: chrono::Utc::now().timestamp() + 60,
            job_id: None,
        };
        let token = create_lease_token(&claims, "node-secret").unwrap();
        let work_root = tempfile::tempdir().unwrap();
        let request = LeaseRequest {
            mission_id: Uuid::new_v4(),
            node_id: "babylon".to_string(),
            lease_token: token,
            command: "true".to_string(),
        };
        let err = run_lease_command(
            "babylon",
            "node-secret",
            work_root.path().to_path_buf(),
            request,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RemoteNodeError::InvalidLease(_)));
    }

    #[test]
    fn placement_requires_enabled_selected_configured_node() {
        let node = RemoteNodeConfig {
            id: "babylon".to_string(),
            base_url: "http://127.0.0.1:3088".to_string(),
            token_env: "TOKEN".to_string(),
            labels: None,
        };
        let disabled = RemoteNodeSettings {
            enabled: false,
            nodes: vec![node.clone()],
        };
        assert!(matches!(
            placement_for_selected_node(&disabled, Some("babylon")),
            Err(RemoteNodeError::Disabled)
        ));
        let enabled = RemoteNodeSettings {
            enabled: true,
            nodes: vec![node],
        };
        assert_eq!(
            placement_for_selected_node(&enabled, Some("babylon"))
                .unwrap()
                .unwrap()
                .id,
            "babylon"
        );
        assert!(matches!(
            placement_for_selected_node(&enabled, Some("ashur")),
            Err(RemoteNodeError::UnknownNode(_))
        ));
    }
}
