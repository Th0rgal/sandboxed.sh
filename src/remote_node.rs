//! Remote mission-node protocol skeleton.
//!
//! This module intentionally does not schedule work yet. It defines the stable
//! contract shape for a future `sandboxed-node` daemon while keeping core local
//! mission execution unchanged.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteNodeStatus {
    Disabled,
    Unknown,
    Online,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteNodeCapacity {
    pub cpu_cores: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub disk_free_bytes: Option<u64>,
    pub gpu_labels: Vec<String>,
    pub running_missions: u32,
    pub max_missions: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNodeHeartbeat {
    pub node_id: String,
    pub status: RemoteNodeStatus,
    pub labels: HashMap<String, String>,
    pub capacity: RemoteNodeCapacity,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteMissionLease {
    pub lease_id: String,
    pub mission_id: String,
    pub workspace_id: String,
    pub expires_at: String,
    pub callback_url: String,
    pub capability_token: String,
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
        Self::from_raw_env(
            std::env::var("SANDBOXED_REMOTE_NODES_ENABLED").ok(),
            std::env::var("SANDBOXED_REMOTE_NODES").ok(),
        )
    }

    fn from_raw_env(enabled_raw: Option<String>, nodes_raw: Option<String>) -> Self {
        let enabled = enabled_raw.is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
        });
        let configured_nodes = nodes_raw
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .count()
            })
            .unwrap_or(0);

        let mut notes = Vec::new();
        if !enabled {
            notes.push(
                "Remote mission nodes are disabled; local/container execution is unchanged."
                    .to_string(),
            );
        } else if configured_nodes == 0 {
            notes.push(
                "Remote nodes are enabled, but no node endpoints are configured.".to_string(),
            );
        } else {
            notes.push("Remote node scheduling is protocol-only in this build.".to_string());
        }

        Self {
            enabled,
            configured_nodes,
            status: if enabled {
                RemoteNodeStatus::Unknown
            } else {
                RemoteNodeStatus::Disabled
            },
            notes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteNodeOverview, RemoteNodeStatus};

    #[test]
    fn remote_nodes_default_to_disabled() {
        let overview = RemoteNodeOverview::from_raw_env(None, None);

        assert!(!overview.enabled);
        assert_eq!(overview.configured_nodes, 0);
        assert_eq!(overview.status, RemoteNodeStatus::Disabled);
        assert!(overview.notes[0].contains("disabled"));
    }

    #[test]
    fn remote_nodes_count_configured_endpoints_when_enabled() {
        let overview = RemoteNodeOverview::from_raw_env(
            Some("true".to_string()),
            Some("http://n1:9100, , http://n2:9100".to_string()),
        );

        assert!(overview.enabled);
        assert_eq!(overview.configured_nodes, 2);
        assert_eq!(overview.status, RemoteNodeStatus::Unknown);
        assert!(overview.notes[0].contains("protocol-only"));
    }
}
