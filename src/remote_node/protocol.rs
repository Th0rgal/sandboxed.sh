//! Shared wire types for the core <-> `sandboxed-node` protocol.
//!
//! Both the core backend and the node binary (`src/bin/sandboxed_node.rs`)
//! import these from the library crate so request/response shapes cannot
//! drift apart.

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use super::RemoteNodeError;

type HmacSha256 = Hmac<Sha256>;

/// Protocol version reported by heartbeat v2 nodes.
pub const NODE_PROTOCOL_VERSION: u32 = 2;

/// Lease scope for the synchronous `/execute` path.
pub const SCOPE_MISSION_EXECUTE: &str = "mission:execute";

/// Lease scope for submitting async jobs (`POST /jobs`).
pub const SCOPE_JOB_SUBMIT: &str = "job:submit";

fn default_protocol_version() -> u32 {
    // Nodes predating heartbeat v2 don't send the field at all.
    1
}

/// Node heartbeat payload (v2).
///
/// All fields beyond the original v1 set are `#[serde(default)]`-tolerant so
/// core can parse heartbeats from nodes that were not yet upgraded, and old
/// cores simply ignore the extra fields of new nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeHeartbeat {
    pub node_id: String,
    pub online: bool,
    pub capacity_total: u32,
    pub capacity_available: u32,
    pub active_leases: u32,
    pub version: String,
    /// Heartbeat protocol version; `1` when absent (pre-v2 node).
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    /// Operator-assigned labels (`SANDBOXED_NODE_LABELS`).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Number of logical CPU cores on the node.
    #[serde(default)]
    pub cpu_total: u32,
    #[serde(default)]
    pub mem_total_bytes: u64,
    #[serde(default)]
    pub mem_available_bytes: u64,
    /// Disk figures for the filesystem backing the node work dir (or root).
    #[serde(default)]
    pub disk_total_bytes: u64,
    #[serde(default)]
    pub disk_available_bytes: u64,
    /// Async jobs currently executing (0 until the job API is in use).
    #[serde(default)]
    pub active_jobs: u32,
    /// Async jobs queued behind the capacity semaphore.
    #[serde(default)]
    pub queued_jobs: u32,
    /// Prewarmed toolchains cached on the node (empty until S3).
    #[serde(default)]
    pub cached_toolchains: Vec<String>,
    /// Whether the node can resolve an executable Lake proxy. `None` means an
    /// older node that predates readiness reporting.
    #[serde(default)]
    pub lean_runtime_ready: Option<bool>,
}

/// Legacy per-node status shape (kept for API compatibility).
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
    /// Job the lease is bound to (async job submissions only). Tolerated as
    /// absent so pre-job leases keep validating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
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

/// Git source of a declarative build job: the node fetches exactly this
/// commit itself, so no workspace sync between core and node is needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobSource {
    /// Clone/fetch URL (https or ssh).
    pub repo: String,
    /// Full 40-char lowercase hex commit SHA. Branch names are rejected so a
    /// job always builds a pinned, reproducible tree.
    pub commit: String,
    /// Optional bounded overlay applied after resetting the pinned checkout.
    /// This lets a local-only proof source run remotely without creating or
    /// pushing a Git commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<SourceBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceBundleFile {
    pub path: String,
    pub sha256: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceBundle {
    pub manifest_sha256: String,
    pub files: Vec<SourceBundleFile>,
}

/// One artifact produced by a build job, relative to the checkout root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEntry {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Async job payload. Tagged so future kinds can be added without breaking
/// the wire format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobPayload {
    /// Run one shell command with `bash -lc` under the mission work dir —
    /// same semantics as the synchronous `/execute` path.
    RawCommand {
        command: String,
        /// Client-requested timeout; clamped to the node's
        /// `SANDBOXED_NODE_MAX_JOB_SECS`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<std::collections::HashMap<String, String>>,
    },
    /// Declarative Lean build: the node checks out `source` into a
    /// content-addressed checkout, restores shared elan/lake caches, runs a
    /// constrained `lake`/`lean`/`elan` argv, and reports artifact digests.
    /// See `src/node/lean.rs` for validation and execution.
    LeanBuild {
        source: JobSource,
        /// Build cwd relative to the checkout root (validated: no traversal,
        /// no shell metacharacters). `None`/empty = checkout root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_rel: Option<String>,
        /// Argv executed directly (no shell). argv[0] must be one of
        /// `lake`/`lean`/`elan` on the node.
        command: Vec<String>,
        /// Client-requested timeout; clamped to the node's
        /// `SANDBOXED_NODE_MAX_JOB_SECS`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// Expected peak scratch consumption. Node and core admission reserve
        /// this capacity before accepting the job.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_disk_bytes: Option<u64>,
        /// Lake cache slot key. Defaults to a digest of the checkout's
        /// `lean-toolchain` + `lake-manifest.json` contents.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_key: Option<String>,
        /// Artifact patterns relative to the checkout root (exact paths or a
        /// single-segment `*` glob), resolved after a successful build.
        #[serde(default)]
        artifacts: Vec<String>,
        /// Extra env for the build command; keys must be allowlisted on the
        /// node (`SANDBOXED_NODE_ENV_ALLOWLIST`).
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
    },
}

/// Body of `POST /jobs` on the node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubmitJobRequest {
    pub job_id: Uuid,
    pub mission_id: Uuid,
    pub lease_token: String,
    pub payload: JobPayload,
}

/// `202 Accepted` body of `POST /jobs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubmitJobResponse {
    pub job_id: Uuid,
    pub state: String,
}

/// Job status as returned by `GET /jobs/:id` and `GET /jobs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeJobStatus {
    pub job_id: Uuid,
    pub mission_id: Uuid,
    /// queued | running | succeeded | failed | cancelled | lost
    pub state: String,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    /// Up to the last 64 KiB of combined stdout+stderr (single-job status
    /// only; omitted from list responses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_tail: Option<String>,
    /// Artifact digests recorded after a successful build job (empty for raw
    /// commands and for pre-artifact nodes).
    #[serde(default)]
    pub artifacts: Vec<ArtifactEntry>,
}

/// Body of `POST /jobs/:id/cancel`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelJobResponse {
    pub job_id: Uuid,
    pub state: String,
    /// Whether a cancellation was actually delivered to a live job.
    pub cancel_requested: bool,
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
    expected_scope: &str,
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
    if claims.scope != expected_scope {
        return Err(RemoteNodeError::InvalidLease("wrong scope".to_string()));
    }
    Ok(claims)
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Constant-time bearer-token check supporting one-step token rotation: the
/// presented token is accepted when it matches the current token OR the
/// (optional, non-empty) previous token still being phased out.
pub fn node_token_matches(presented: &str, current: &str, previous: Option<&str>) -> bool {
    let matches_current = constant_time_eq(presented.as_bytes(), current.as_bytes());
    let matches_previous = previous
        .filter(|prev| !prev.trim().is_empty())
        .is_some_and(|prev| constant_time_eq(presented.as_bytes(), prev.as_bytes()));
    matches_current || matches_previous
}

/// Parse a comma-separated label list (`SANDBOXED_NODE_LABELS`) into a
/// trimmed, non-empty vector.
pub fn parse_labels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_scoped_lease_token() {
        let mission_id = Uuid::new_v4();
        let claims = LeaseClaims {
            mission_id,
            node_id: "babylon".to_string(),
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            expires_at: chrono::Utc::now().timestamp() + 60,
            job_id: None,
        };
        let token = create_lease_token(&claims, "node-secret").unwrap();
        let parsed = validate_lease_token(
            &token,
            "node-secret",
            "babylon",
            SCOPE_MISSION_EXECUTE,
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(parsed.mission_id, mission_id);
        assert!(validate_lease_token(
            &token,
            "other-secret",
            "babylon",
            SCOPE_MISSION_EXECUTE,
            chrono::Utc::now()
        )
        .is_err());
        assert!(validate_lease_token(
            &token,
            "node-secret",
            "nippur",
            SCOPE_MISSION_EXECUTE,
            chrono::Utc::now()
        )
        .is_err());
    }

    #[test]
    fn lease_scopes_are_not_interchangeable() {
        let now = chrono::Utc::now();
        let claims_for = |scope: &str, job_id: Option<Uuid>| LeaseClaims {
            mission_id: Uuid::new_v4(),
            node_id: "babylon".to_string(),
            scope: scope.to_string(),
            expires_at: now.timestamp() + 60,
            job_id,
        };

        // A mission:execute lease must be rejected for job submission.
        let execute_token =
            create_lease_token(&claims_for(SCOPE_MISSION_EXECUTE, None), "node-secret").unwrap();
        assert!(validate_lease_token(
            &execute_token,
            "node-secret",
            "babylon",
            SCOPE_MISSION_EXECUTE,
            now
        )
        .is_ok());
        assert!(matches!(
            validate_lease_token(
                &execute_token,
                "node-secret",
                "babylon",
                SCOPE_JOB_SUBMIT,
                now
            ),
            Err(RemoteNodeError::InvalidLease(_))
        ));

        // ... and a job:submit lease must be rejected for /execute.
        let job_token = create_lease_token(
            &claims_for(SCOPE_JOB_SUBMIT, Some(Uuid::new_v4())),
            "node-secret",
        )
        .unwrap();
        let parsed =
            validate_lease_token(&job_token, "node-secret", "babylon", SCOPE_JOB_SUBMIT, now)
                .unwrap();
        assert!(parsed.job_id.is_some());
        assert!(matches!(
            validate_lease_token(
                &job_token,
                "node-secret",
                "babylon",
                SCOPE_MISSION_EXECUTE,
                now
            ),
            Err(RemoteNodeError::InvalidLease(_))
        ));
    }

    #[test]
    fn lease_claims_without_job_id_still_parse() {
        // Tokens minted before the job API existed have no job_id claim.
        let raw = serde_json::json!({
            "mission_id": Uuid::new_v4(),
            "node_id": "babylon",
            "scope": SCOPE_MISSION_EXECUTE,
            "expires_at": 1_900_000_000,
        });
        let claims: LeaseClaims = serde_json::from_value(raw).unwrap();
        assert_eq!(claims.job_id, None);
    }

    #[test]
    fn job_payload_round_trips_with_kind_tag() {
        let payload = JobPayload::RawCommand {
            command: "cargo test".to_string(),
            timeout_secs: Some(600),
            env: Some(
                [("RUST_LOG".to_string(), "info".to_string())]
                    .into_iter()
                    .collect(),
            ),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "raw_command");
        assert_eq!(json["command"], "cargo test");
        let parsed: JobPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, payload);

        // Minimal payload: optional fields default.
        let minimal: JobPayload = serde_json::from_value(serde_json::json!({
            "kind": "raw_command",
            "command": "true",
        }))
        .unwrap();
        assert_eq!(
            minimal,
            JobPayload::RawCommand {
                command: "true".to_string(),
                timeout_secs: None,
                env: None,
            }
        );

        // Unknown kinds are rejected, leaving room for future variants.
        assert!(serde_json::from_value::<JobPayload>(serde_json::json!({
            "kind": "gpu_render",
            "command": "true",
        }))
        .is_err());
    }

    #[test]
    fn lean_build_payload_round_trips_with_defaults() {
        let payload = JobPayload::LeanBuild {
            source: JobSource {
                repo: "https://github.com/example/verity.git".to_string(),
                commit: "a".repeat(40),
                bundle: None,
            },
            cwd_rel: Some("morpho-verity".to_string()),
            command: vec!["lake".to_string(), "build".to_string()],
            timeout_secs: Some(3600),
            estimated_disk_bytes: Some(12 << 30),
            cache_key: Some("abc123".to_string()),
            artifacts: vec![".lake/build/lib/*".to_string()],
            env: [("LEAN_NUM_THREADS".to_string(), "4".to_string())]
                .into_iter()
                .collect(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "lean_build");
        assert_eq!(json["source"]["commit"], "a".repeat(40));
        let parsed: JobPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, payload);

        // Minimal payload: only source + command; the rest defaults.
        let minimal: JobPayload = serde_json::from_value(serde_json::json!({
            "kind": "lean_build",
            "source": {"repo": "https://x.git", "commit": "b".repeat(40)},
            "command": ["lake", "build"],
        }))
        .unwrap();
        match minimal {
            JobPayload::LeanBuild {
                cwd_rel,
                timeout_secs,
                estimated_disk_bytes,
                cache_key,
                artifacts,
                env,
                ..
            } => {
                assert_eq!(cwd_rel, None);
                assert_eq!(timeout_secs, None);
                assert_eq!(estimated_disk_bytes, None);
                assert_eq!(cache_key, None);
                assert!(artifacts.is_empty());
                assert!(env.is_empty());
            }
            other => panic!("expected lean_build, got {other:?}"),
        }
    }

    #[test]
    fn job_status_artifacts_default_for_old_nodes() {
        // A pre-artifact node omits the field entirely.
        let raw = serde_json::json!({
            "job_id": Uuid::new_v4(),
            "mission_id": Uuid::new_v4(),
            "state": "succeeded",
            "exit_code": 0,
            "created_at": "2026-07-12T00:00:00Z",
            "started_at": null,
            "finished_at": null,
            "error": null,
        });
        let status: NodeJobStatus = serde_json::from_value(raw).unwrap();
        assert!(status.artifacts.is_empty());

        let entry = ArtifactEntry {
            path: ".lake/build/lib/Verity.olean".to_string(),
            sha256: "0".repeat(64),
            size_bytes: 42,
        };
        let json = serde_json::to_value(&entry).unwrap();
        let parsed: ArtifactEntry = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn token_rotation_accepts_current_or_previous() {
        // Current token always matches.
        assert!(node_token_matches("tok-new", "tok-new", None));
        assert!(node_token_matches("tok-new", "tok-new", Some("tok-old")));
        // Previous token matches only while configured and non-empty.
        assert!(node_token_matches("tok-old", "tok-new", Some("tok-old")));
        assert!(!node_token_matches("tok-old", "tok-new", None));
        assert!(!node_token_matches("tok-old", "tok-new", Some("")));
        assert!(!node_token_matches("tok-old", "tok-new", Some("   ")));
        // Anything else is rejected.
        assert!(!node_token_matches("wrong", "tok-new", Some("tok-old")));
        assert!(!node_token_matches("", "tok-new", Some("tok-old")));
    }

    #[test]
    fn heartbeat_v1_payload_parses_with_defaults() {
        // A pre-v2 node sends only the original field set.
        let raw = serde_json::json!({
            "node_id": "babylon",
            "online": true,
            "capacity_total": 2,
            "capacity_available": 1,
            "active_leases": 1,
            "version": "1.2.0",
        });
        let heartbeat: NodeHeartbeat = serde_json::from_value(raw).unwrap();
        assert_eq!(heartbeat.protocol_version, 1);
        assert!(heartbeat.labels.is_empty());
        assert_eq!(heartbeat.cpu_total, 0);
        assert_eq!(heartbeat.mem_total_bytes, 0);
        assert_eq!(heartbeat.disk_available_bytes, 0);
        assert_eq!(heartbeat.active_jobs, 0);
        assert_eq!(heartbeat.queued_jobs, 0);
        assert!(heartbeat.cached_toolchains.is_empty());
        // v1 fields still round-trip.
        assert_eq!(heartbeat.capacity_available, 1);
    }

    #[test]
    fn heartbeat_v2_round_trips() {
        let heartbeat = NodeHeartbeat {
            node_id: "babylon".to_string(),
            online: true,
            capacity_total: 4,
            capacity_available: 3,
            active_leases: 1,
            version: "1.3.0".to_string(),
            protocol_version: NODE_PROTOCOL_VERSION,
            labels: vec!["gpu".to_string(), "lean".to_string()],
            cpu_total: 16,
            mem_total_bytes: 64 << 30,
            mem_available_bytes: 32 << 30,
            disk_total_bytes: 512 << 30,
            disk_available_bytes: 100 << 30,
            active_jobs: 1,
            queued_jobs: 2,
            cached_toolchains: vec![],
            lean_runtime_ready: Some(true),
        };
        let json = serde_json::to_string(&heartbeat).unwrap();
        let parsed: NodeHeartbeat = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, heartbeat);
    }

    #[test]
    fn parses_label_lists() {
        assert_eq!(parse_labels("gpu, lean ,,x"), vec!["gpu", "lean", "x"]);
        assert!(parse_labels("  ").is_empty());
    }
}
