use axum::http::HeaderMap;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

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
        Self {
            enabled: settings.enabled,
            configured_nodes: settings.nodes.len(),
            status: if settings.enabled {
                RemoteNodeStatus::Unknown
            } else {
                RemoteNodeStatus::Disabled
            },
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeHeartbeat {
    pub node_id: String,
    pub online: bool,
    pub capacity_total: u32,
    pub capacity_available: u32,
    pub active_leases: u32,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeStatus {
    pub id: String,
    pub base_url: String,
    pub token_env: String,
    pub online: bool,
    pub capacity_total: Option<u32>,
    pub capacity_available: Option<u32>,
    pub active_leases: Option<u32>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseClaims {
    pub mission_id: Uuid,
    pub node_id: String,
    pub scope: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseRequest {
    pub mission_id: Uuid,
    pub node_id: String,
    pub lease_token: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecuteResponse {
    pub accepted: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Total request timeout for `/execute`, which blocks until the remote
/// command completes.
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct RemoteNodeClient {
    http: reqwest::Client,
}

impl Default for RemoteNodeClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl RemoteNodeClient {
    pub async fn heartbeat(
        &self,
        node: &RemoteNodeConfig,
        token: &str,
    ) -> Result<NodeHeartbeat, RemoteNodeError> {
        let url = format!("{}/heartbeat", node.base_url);
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        if !response.status().is_success() {
            return Err(RemoteNodeError::Request(format!(
                "heartbeat returned {}",
                response.status()
            )));
        }
        response
            .json::<NodeHeartbeat>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }

    pub async fn execute(
        &self,
        node: &RemoteNodeConfig,
        shared_token: &str,
        request: &LeaseRequest,
    ) -> Result<ExecuteResponse, RemoteNodeError> {
        let url = format!("{}/execute", node.base_url);
        let response = self
            .http
            .post(url)
            // Override the client-wide 30s timeout: remote commands
            // (builds, tests) routinely run for minutes and the response
            // only arrives once the command finishes.
            .timeout(EXECUTE_TIMEOUT)
            .bearer_auth(shared_token)
            .json(request)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteNodeError::Rejected(format!("{status}: {body}")));
        }
        response
            .json::<ExecuteResponse>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }
}

pub fn create_lease_token(claims: &LeaseClaims, secret: &str) -> Result<String, RemoteNodeError> {
    if secret.trim().is_empty() {
        return Err(RemoteNodeError::InvalidLease(
            "empty signing secret".to_string(),
        ));
    }
    let json =
        serde_json::to_vec(claims).map_err(|e| RemoteNodeError::InvalidLease(e.to_string()))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| RemoteNodeError::InvalidLease(e.to_string()))?;
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    Ok(format!("{payload}.{signature}"))
}

pub fn validate_lease_token(
    token: &str,
    secret: &str,
    expected_node_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<LeaseClaims, RemoteNodeError> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| RemoteNodeError::InvalidLease("missing signature".to_string()))?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| RemoteNodeError::InvalidLease(e.to_string()))?;
    mac.update(payload.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return Err(RemoteNodeError::InvalidLease("bad signature".to_string()));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| RemoteNodeError::InvalidLease(e.to_string()))?;
    let claims: LeaseClaims =
        serde_json::from_slice(&bytes).map_err(|e| RemoteNodeError::InvalidLease(e.to_string()))?;
    if claims.node_id != expected_node_id {
        return Err(RemoteNodeError::InvalidLease("wrong node".to_string()));
    }
    if claims.expires_at <= now.timestamp() {
        return Err(RemoteNodeError::InvalidLease("expired".to_string()));
    }
    if claims.scope != "mission:execute" {
        return Err(RemoteNodeError::InvalidLease("wrong scope".to_string()));
    }
    Ok(claims)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    use axum::{routing::get, Json, Router};
    use tokio::net::TcpListener;

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
            scope: "mission:execute".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 60,
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
    fn validates_scoped_lease_token() {
        let mission_id = Uuid::new_v4();
        let claims = LeaseClaims {
            mission_id,
            node_id: "babylon".to_string(),
            scope: "mission:execute".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let token = create_lease_token(&claims, "node-secret").unwrap();
        let parsed =
            validate_lease_token(&token, "node-secret", "babylon", chrono::Utc::now()).unwrap();
        assert_eq!(parsed.mission_id, mission_id);
        assert!(
            validate_lease_token(&token, "other-secret", "babylon", chrono::Utc::now()).is_err()
        );
        assert!(validate_lease_token(&token, "node-secret", "nippur", chrono::Utc::now()).is_err());
    }

    #[tokio::test]
    async fn heartbeat_client_reads_node_status() {
        async fn heartbeat() -> Json<NodeHeartbeat> {
            Json(NodeHeartbeat {
                node_id: "babylon".to_string(),
                online: true,
                capacity_total: 2,
                capacity_available: 1,
                active_leases: 1,
                version: "test".to_string(),
            })
        }
        let app = Router::new().route("/heartbeat", get(heartbeat));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = RemoteNodeClient::default();
        let node = RemoteNodeConfig {
            id: "babylon".to_string(),
            base_url: format!("http://{addr}"),
            token_env: "TOKEN".to_string(),
            labels: None,
        };
        let heartbeat = client.heartbeat(&node, "unused").await.unwrap();
        assert_eq!(heartbeat.capacity_available, 1);
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
