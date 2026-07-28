//! Persistent project validation campaigns.
//!
//! A campaign pins one immutable candidate and evaluates a versioned DAG of
//! gates. Execution remains delegated to the existing workspace/remote job
//! APIs; typed execution references attach their durable outcomes here. This
//! module owns freshness, dependency advancement, structured receipts, and a
//! material-change outbox.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::routes::AppState;

pub type SharedValidationStore = Arc<ValidationStore>;
type ApiError = (StatusCode, String);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(create_campaign).get(list_campaigns))
        .route("/from-workspace", post(create_from_workspace))
        .route("/outbox", get(list_outbox))
        .route("/outbox/:event_id/ack", post(ack_outbox))
        .route("/:campaign_id", get(get_campaign))
        .route("/:campaign_id/ready", get(get_ready_gates))
        .route("/:campaign_id/gates/:gate_id/claim", post(claim_gate))
        .route(
            "/:campaign_id/gates/:gate_id/receipts",
            post(record_receipt),
        )
        .route("/:campaign_id/merged", post(mark_merged))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    Incremental,
    Clean,
}

fn default_mode() -> ValidationMode {
    ValidationMode::Incremental
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationCandidate {
    pub repo: String,
    pub commit: String,
    #[serde(default)]
    pub expected_head: Option<String>,
    #[serde(default)]
    pub source_bundle_digest: Option<String>,
}

impl ValidationCandidate {
    fn validate(&self) -> Result<(), String> {
        if self.repo.trim().is_empty() {
            return Err("candidate.repo is required".to_string());
        }
        validate_digest("candidate.commit", &self.commit, 40)?;
        if let Some(head) = self.expected_head.as_deref() {
            validate_digest("candidate.expected_head", head, 40)?;
        }
        if let Some(digest) = self.source_bundle_digest.as_deref() {
            validate_digest("candidate.source_bundle_digest", digest, 64)?;
        }
        Ok(())
    }

    pub fn id(&self) -> String {
        digest_json(self)
    }
}

fn validate_digest(label: &str, value: &str, len: usize) -> Result<(), String> {
    if value.len() == len
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} must be {len} lowercase hexadecimal characters"
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateSpec {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default, alias = "depends_on")]
    pub dependencies: Vec<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default = "default_mode")]
    pub mode: ValidationMode,
    #[serde(default = "default_true")]
    pub reuse: bool,
    #[serde(default)]
    pub toolchain: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationMatrix {
    #[serde(default = "matrix_version")]
    pub version: u32,
    pub project: String,
    #[serde(default)]
    pub profile: Option<String>,
    pub gates: Vec<GateSpec>,
}

fn matrix_version() -> u32 {
    1
}

impl ValidationMatrix {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported validation matrix version {}",
                self.version
            ));
        }
        if self.project.trim().is_empty() {
            return Err("matrix.project is required".to_string());
        }
        if self.gates.is_empty() {
            return Err("matrix.gates must not be empty".to_string());
        }
        let mut ids = HashSet::new();
        for gate in &self.gates {
            if !valid_gate_id(&gate.id) {
                return Err(format!("invalid gate id '{}'", gate.id));
            }
            if !ids.insert(gate.id.as_str()) {
                return Err(format!("duplicate gate id '{}'", gate.id));
            }
            if gate.command.is_empty() || gate.command.iter().any(|arg| arg.contains('\0')) {
                return Err(format!(
                    "gate '{}' requires a safe, non-empty argv",
                    gate.id
                ));
            }
            if gate.timeout_secs == Some(0) {
                return Err(format!("gate '{}' timeout_secs must be positive", gate.id));
            }
        }
        for gate in &self.gates {
            for dependency in &gate.dependencies {
                if dependency == &gate.id || !ids.contains(dependency.as_str()) {
                    return Err(format!(
                        "gate '{}' has invalid dependency '{}'",
                        gate.id, dependency
                    ));
                }
            }
        }
        detect_cycle(&self.gates)?;
        Ok(())
    }
}

fn valid_gate_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn detect_cycle(gates: &[GateSpec]) -> Result<(), String> {
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a GateSpec>,
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(format!("validation gate dependency cycle contains '{id}'"));
        }
        for dependency in &by_id[id].dependencies {
            visit(dependency, by_id, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    let by_id = gates
        .iter()
        .map(|gate| (gate.id.as_str(), gate))
        .collect::<HashMap<_, _>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for gate in gates {
        visit(&gate.id, &by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateCampaignRequest {
    pub candidate: ValidationCandidate,
    pub matrix: ValidationMatrix,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct CreateFromWorkspaceRequest {
    workspace_id: Uuid,
    candidate: ValidationCandidate,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionRef {
    MissionRun { run_id: Uuid, mission_id: Uuid },
    WorkspaceJob { job_id: Uuid, workspace_id: Uuid },
    RemoteJob { job_id: Uuid, node_id: String },
}

impl ExecutionRef {
    fn key(&self) -> String {
        match self {
            Self::MissionRun { run_id, .. } => format!("mission_run:{run_id}"),
            Self::WorkspaceJob { job_id, .. } => format!("workspace_job:{job_id}"),
            Self::RemoteJob { job_id, .. } => format!("remote_job:{job_id}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheReceipt {
    pub mode: Option<String>,
    pub key: Option<String>,
    pub hit: Option<bool>,
    #[serde(default)]
    pub clean_checkout: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptArtifact {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ClaimGateRequest {
    execution: ExecutionRef,
}

#[derive(Debug, Deserialize)]
struct RecordReceiptRequest {
    execution: ExecutionRef,
    exit_code: Option<i32>,
    #[serde(default)]
    blocked_reason: Option<String>,
    #[serde(default)]
    observed_head: Option<String>,
    #[serde(default)]
    toolchain: Option<String>,
    #[serde(default)]
    environment_digest: Option<String>,
    #[serde(default)]
    cache: CacheReceipt,
    #[serde(default)]
    artifacts: Vec<ReceiptArtifact>,
    #[serde(default)]
    diagnostics: Option<String>,
    #[serde(default)]
    started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCluster {
    pub fingerprint: String,
    pub representative: String,
    pub count: usize,
}

pub fn cluster_diagnostics(raw: &str) -> Vec<DiagnosticCluster> {
    let mut clusters = BTreeMap::<String, DiagnosticCluster>::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let normalized = normalize_diagnostic(line);
        let fingerprint = hex::encode(Sha256::digest(normalized.as_bytes()));
        clusters
            .entry(fingerprint.clone())
            .and_modify(|cluster| cluster.count += 1)
            .or_insert_with(|| DiagnosticCluster {
                fingerprint,
                representative: line.to_string(),
                count: 1,
            });
    }
    let mut clusters = clusters.into_values().collect::<Vec<_>>();
    clusters.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.representative.cmp(&b.representative))
    });
    clusters
}

fn normalize_diagnostic(line: &str) -> String {
    let mut normalized = String::with_capacity(line.len());
    let mut digits = false;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            if !digits {
                normalized.push('#');
            }
            digits = true;
        } else {
            digits = false;
            normalized.push(ch.to_ascii_lowercase());
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateView {
    pub id: String,
    pub spec: GateSpec,
    pub status: String,
    pub outcome: Option<String>,
    pub freshness: Option<String>,
    pub execution: Option<ExecutionRef>,
    pub validation_key: String,
    pub reused_receipt_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReceipt {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub gate_id: String,
    pub candidate_id: String,
    pub execution: ExecutionRef,
    pub outcome: String,
    pub freshness: String,
    pub exit_code: Option<i32>,
    pub blocked_reason: Option<String>,
    pub observed_head: Option<String>,
    pub toolchain: Option<String>,
    pub environment_digest: Option<String>,
    pub cache: CacheReceipt,
    pub artifacts: Vec<ReceiptArtifact>,
    pub diagnostic_clusters: Vec<DiagnosticCluster>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub reused_from: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignView {
    pub id: Uuid,
    pub project: String,
    pub profile: Option<String>,
    pub workspace_id: Option<Uuid>,
    pub candidate: ValidationCandidate,
    pub candidate_id: String,
    pub matrix_version: u32,
    pub status: String,
    pub certifying: bool,
    pub gates: Vec<GateView>,
    pub receipts: Vec<ValidationReceipt>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxEvent {
    id: Uuid,
    campaign_id: Uuid,
    event_type: String,
    fingerprint: String,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
    attempts: u32,
    next_attempt_at: Option<DateTime<Utc>>,
}

pub struct ValidationStore {
    connection: Mutex<Connection>,
}

impl ValidationStore {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.connection
            .lock()
            .map_err(|_| "validation database lock poisoned".to_string())
    }

    fn create(&self, request: CreateCampaignRequest) -> Result<CampaignView, String> {
        request.candidate.validate()?;
        request.matrix.validate()?;
        let id = Uuid::new_v4();
        let now = Utc::now();
        let candidate_id = request.candidate.id();
        let candidate_json = json_string(&request.candidate)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO validation_campaigns
                 (id, project, profile, workspace_id, candidate_json, candidate_id,
                  matrix_version, status, certifying, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 0, ?8, ?8)",
                params![
                    id.to_string(),
                    request.matrix.project,
                    request.matrix.profile,
                    request.workspace_id.map(|id| id.to_string()),
                    candidate_json,
                    candidate_id,
                    request.matrix.version,
                    now.to_rfc3339(),
                ],
            )
            .map_err(db_error)?;
        for gate in &request.matrix.gates {
            let status = if gate.dependencies.is_empty() {
                "ready"
            } else {
                "pending"
            };
            let validation_key = validation_key(&request.candidate, gate);
            transaction
                .execute(
                    "INSERT INTO validation_gates
                     (campaign_id, gate_id, spec_json, status, validation_key)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id.to_string(),
                        gate.id,
                        json_string(gate)?,
                        status,
                        validation_key,
                    ],
                )
                .map_err(db_error)?;
        }
        insert_outbox(
            &transaction,
            id,
            "candidate_changed",
            &candidate_id,
            serde_json::json!({
                "campaign_id": id,
                "project": request.matrix.project,
                "candidate": request.candidate,
                "candidate_id": candidate_id,
            }),
        )?;
        reuse_matching_receipts(&transaction, id, &request.candidate)?;
        recompute_campaign(&transaction, id)?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.get(id)
    }

    fn get(&self, id: Uuid) -> Result<CampaignView, String> {
        let connection = self.lock()?;
        load_campaign(&connection, id)?.ok_or_else(|| format!("campaign {id} not found"))
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS validation_campaigns (
    id TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    profile TEXT,
    workspace_id TEXT,
    candidate_json TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    matrix_version INTEGER NOT NULL,
    status TEXT NOT NULL,
    certifying INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS validation_campaign_project_idx
    ON validation_campaigns(project, created_at DESC);
CREATE TABLE IF NOT EXISTS validation_gates (
    campaign_id TEXT NOT NULL REFERENCES validation_campaigns(id) ON DELETE CASCADE,
    gate_id TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    status TEXT NOT NULL,
    outcome TEXT,
    freshness TEXT,
    execution_json TEXT,
    validation_key TEXT NOT NULL,
    reused_receipt_id TEXT,
    blocker_fingerprint TEXT,
    started_at TEXT,
    finished_at TEXT,
    PRIMARY KEY(campaign_id, gate_id)
);
CREATE INDEX IF NOT EXISTS validation_gate_key_idx
    ON validation_gates(validation_key, status);
CREATE TABLE IF NOT EXISTS validation_receipts (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL REFERENCES validation_campaigns(id) ON DELETE CASCADE,
    gate_id TEXT NOT NULL,
    execution_key TEXT NOT NULL,
    validation_key TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    outcome TEXT NOT NULL,
    freshness TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(campaign_id, gate_id, execution_key)
);
CREATE INDEX IF NOT EXISTS validation_receipt_key_idx
    ON validation_receipts(validation_key, outcome, freshness, created_at DESC);
CREATE TABLE IF NOT EXISTS validation_outbox (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL REFERENCES validation_campaigns(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    delivered_at TEXT,
    dead_lettered_at TEXT,
    UNIQUE(campaign_id, event_type, fingerprint)
);
"#;

async fn create_campaign(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateCampaignRequest>,
) -> Result<(StatusCode, Json<CampaignView>), ApiError> {
    state
        .validation
        .create(request)
        .map(|campaign| (StatusCode::CREATED, Json(campaign)))
        .map_err(bad_request)
}

async fn create_from_workspace(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateFromWorkspaceRequest>,
) -> Result<(StatusCode, Json<CampaignView>), ApiError> {
    let workspace = state
        .workspaces
        .get(request.workspace_id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, "workspace not found".to_string()))?;
    let relative = request
        .path
        .as_deref()
        .unwrap_or(".sandboxed/validation.toml");
    if relative.starts_with('/') || relative.split('/').any(|part| part == "..") {
        return Err(bad_request(
            "matrix path must be workspace-relative".to_string(),
        ));
    }
    let workspace_root = tokio::fs::canonicalize(&workspace.path)
        .await
        .map_err(|error| bad_request(format!("resolve workspace root: {error}")))?;
    let path = tokio::fs::canonicalize(workspace.path.join(relative))
        .await
        .map_err(|error| bad_request(format!("resolve validation matrix: {error}")))?;
    if !path.starts_with(&workspace_root) {
        return Err(bad_request(
            "matrix path must remain inside the workspace".to_string(),
        ));
    }
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|error| bad_request(format!("read {}: {error}", path.display())))?;
    let matrix = toml::from_str::<ValidationMatrix>(&raw)
        .map_err(|error| bad_request(format!("parse {}: {error}", path.display())))?;
    state
        .validation
        .create(CreateCampaignRequest {
            candidate: request.candidate,
            matrix,
            workspace_id: Some(request.workspace_id),
        })
        .map(|campaign| (StatusCode::CREATED, Json(campaign)))
        .map_err(bad_request)
}

async fn get_campaign(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<CampaignView>, ApiError> {
    state.validation.get(id).map(Json).map_err(not_found)
}

async fn list_campaigns(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<CampaignView>>, ApiError> {
    let connection = state.validation.lock().map_err(internal)?;
    let mut statement = connection
        .prepare("SELECT id FROM validation_campaigns ORDER BY created_at DESC LIMIT 100")
        .map_err(internal)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    let campaigns = ids
        .into_iter()
        .filter_map(|id| Uuid::parse_str(&id).ok())
        .filter_map(|id| load_campaign(&connection, id).transpose())
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    Ok(Json(campaigns))
}

async fn get_ready_gates(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<Vec<GateView>>, ApiError> {
    let campaign = state.validation.get(id).map_err(not_found)?;
    Ok(Json(
        campaign
            .gates
            .into_iter()
            .filter(|gate| gate.status == "ready")
            .collect(),
    ))
}

async fn claim_gate(
    State(state): State<Arc<AppState>>,
    AxumPath((campaign_id, gate_id)): AxumPath<(Uuid, String)>,
    Json(request): Json<ClaimGateRequest>,
) -> Result<Json<GateView>, ApiError> {
    let mut connection = state.validation.lock().map_err(internal)?;
    let transaction = connection.transaction().map_err(internal)?;
    let current = load_gate(&transaction, campaign_id, &gate_id)
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "validation gate not found".to_string(),
            )
        })?;
    if current.status == "running" && current.execution.as_ref() == Some(&request.execution) {
        return Ok(Json(current));
    }
    if current.status != "ready" {
        return Err((
            StatusCode::CONFLICT,
            format!("gate '{gate_id}' is {}, not ready", current.status),
        ));
    }
    transaction
        .execute(
            "UPDATE validation_gates SET status='running', execution_json=?3,
             started_at=?4 WHERE campaign_id=?1 AND gate_id=?2",
            params![
                campaign_id.to_string(),
                gate_id,
                json_string(&request.execution).map_err(internal)?,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(internal)?;
    transaction.commit().map_err(internal)?;
    load_gate(&connection, campaign_id, &gate_id)
        .map_err(internal)?
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "validation gate not found".to_string(),
            )
        })
}

async fn record_receipt(
    State(state): State<Arc<AppState>>,
    AxumPath((campaign_id, gate_id)): AxumPath<(Uuid, String)>,
    Json(request): Json<RecordReceiptRequest>,
) -> Result<Json<CampaignView>, ApiError> {
    let mut connection = state.validation.lock().map_err(internal)?;
    let transaction = connection.transaction().map_err(internal)?;
    let campaign = load_campaign_row(&transaction, campaign_id)
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "validation campaign not found".to_string(),
            )
        })?;
    let gate = load_gate(&transaction, campaign_id, &gate_id)
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "validation gate not found".to_string(),
            )
        })?;
    let execution_key = request.execution.key();
    if transaction
        .query_row(
            "SELECT 1 FROM validation_receipts
             WHERE campaign_id=?1 AND gate_id=?2 AND execution_key=?3",
            params![campaign_id.to_string(), gate_id, execution_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(internal)?
        .is_some()
    {
        transaction.commit().map_err(internal)?;
        drop(connection);
        return state
            .validation
            .get(campaign_id)
            .map(Json)
            .map_err(internal);
    }
    if gate.status != "running" || gate.execution.as_ref() != Some(&request.execution) {
        return Err((
            StatusCode::CONFLICT,
            "receipt execution does not own a running gate".to_string(),
        ));
    }

    if let Some(head) = request.observed_head.as_deref() {
        validate_digest("observed_head", head, 40).map_err(bad_request)?;
    }
    let outcome = if request
        .blocked_reason
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "blocked"
    } else if request.exit_code == Some(0) {
        "passed"
    } else {
        "failed"
    };
    if outcome == "passed"
        && gate.spec.mode == ValidationMode::Clean
        && !request.cache.clean_checkout
    {
        return Err(bad_request(
            "a passed clean gate receipt must attest cache.clean_checkout=true".to_string(),
        ));
    }
    let expected_head = campaign
        .candidate
        .expected_head
        .as_deref()
        .unwrap_or(&campaign.candidate.commit);
    let freshness = if request.observed_head.as_deref() == Some(expected_head) {
        "exact_head"
    } else {
        "stale"
    };
    let now = Utc::now();
    let receipt = ValidationReceipt {
        id: Uuid::new_v4(),
        campaign_id,
        gate_id: gate_id.clone(),
        candidate_id: campaign.candidate_id,
        execution: request.execution,
        outcome: outcome.to_string(),
        freshness: freshness.to_string(),
        exit_code: request.exit_code,
        blocked_reason: request.blocked_reason.clone(),
        observed_head: request.observed_head,
        toolchain: request.toolchain,
        environment_digest: request.environment_digest,
        cache: request.cache,
        artifacts: request.artifacts,
        diagnostic_clusters: request
            .diagnostics
            .as_deref()
            .map(cluster_diagnostics)
            .unwrap_or_default(),
        started_at: request.started_at.or(gate.started_at).unwrap_or(now),
        finished_at: request.finished_at.unwrap_or(now),
        reused_from: None,
    };
    transaction
        .execute(
            "INSERT INTO validation_receipts
             (id, campaign_id, gate_id, execution_key, validation_key, receipt_json,
              outcome, freshness, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                receipt.id.to_string(),
                campaign_id.to_string(),
                gate_id,
                execution_key,
                gate.validation_key,
                json_string(&receipt).map_err(internal)?,
                outcome,
                freshness,
                now.to_rfc3339(),
            ],
        )
        .map_err(internal)?;
    let gate_status = match (outcome, freshness) {
        ("passed", "exact_head") => "passed",
        ("passed", _) => "stale",
        ("blocked", _) => "blocked",
        _ => "failed",
    };
    let blocker_fingerprint = request.blocked_reason.as_deref().map(hash_text);
    transaction
        .execute(
            "UPDATE validation_gates SET status=?3, outcome=?4, freshness=?5,
             execution_json=?6, blocker_fingerprint=?7, finished_at=?8
             WHERE campaign_id=?1 AND gate_id=?2",
            params![
                campaign_id.to_string(),
                gate_id,
                gate_status,
                outcome,
                freshness,
                json_string(&receipt.execution).map_err(internal)?,
                blocker_fingerprint,
                receipt.finished_at.to_rfc3339(),
            ],
        )
        .map_err(internal)?;
    if let (Some(reason), Some(fingerprint)) = (
        request.blocked_reason.as_deref(),
        blocker_fingerprint.as_deref(),
    ) {
        insert_outbox(
            &transaction,
            campaign_id,
            "blocker_changed",
            fingerprint,
            serde_json::json!({
                "campaign_id": campaign_id,
                "gate_id": gate_id,
                "reason": reason,
                "fingerprint": fingerprint,
            }),
        )
        .map_err(internal)?;
    }
    recompute_campaign(&transaction, campaign_id).map_err(internal)?;
    transaction.commit().map_err(internal)?;
    drop(connection);
    state
        .validation
        .get(campaign_id)
        .map(Json)
        .map_err(internal)
}

async fn mark_merged(
    State(state): State<Arc<AppState>>,
    AxumPath(campaign_id): AxumPath<Uuid>,
) -> Result<Json<CampaignView>, ApiError> {
    let mut connection = state.validation.lock().map_err(internal)?;
    let transaction = connection.transaction().map_err(internal)?;
    let (status, certifying): (String, bool) = transaction
        .query_row(
            "SELECT status, certifying FROM validation_campaigns WHERE id=?1",
            params![campaign_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "validation campaign not found".to_string(),
            )
        })?;
    if status != "completed" || !certifying {
        return Err((
            StatusCode::CONFLICT,
            "only a completed exact-head campaign with a required clean gate may be merged"
                .to_string(),
        ));
    }
    transaction
        .execute(
            "UPDATE validation_campaigns SET status='merged', updated_at=?2 WHERE id=?1",
            params![campaign_id.to_string(), Utc::now().to_rfc3339()],
        )
        .map_err(internal)?;
    insert_outbox(
        &transaction,
        campaign_id,
        "merged",
        "merged",
        serde_json::json!({"campaign_id": campaign_id, "status": "merged"}),
    )
    .map_err(internal)?;
    transaction.commit().map_err(internal)?;
    drop(connection);
    state
        .validation
        .get(campaign_id)
        .map(Json)
        .map_err(internal)
}

async fn list_outbox(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<OutboxEvent>>, ApiError> {
    let connection = state.validation.lock().map_err(internal)?;
    let mut statement = connection
        .prepare(
            "SELECT id, campaign_id, event_type, fingerprint, payload_json,
                    created_at, attempts, next_attempt_at
             FROM validation_outbox WHERE delivered_at IS NULL
               AND dead_lettered_at IS NULL ORDER BY created_at LIMIT 200",
        )
        .map_err(internal)?;
    let events = statement
        .query_map([], |row| {
            Ok(OutboxEvent {
                id: parse_uuid_sql(row.get::<_, String>(0)?)?,
                campaign_id: parse_uuid_sql(row.get::<_, String>(1)?)?,
                event_type: row.get(2)?,
                fingerprint: row.get(3)?,
                payload: parse_json_sql(row.get::<_, String>(4)?)?,
                created_at: parse_time_sql(row.get::<_, String>(5)?)?,
                attempts: row.get(6)?,
                next_attempt_at: row
                    .get::<_, Option<String>>(7)?
                    .map(parse_time_sql)
                    .transpose()?,
            })
        })
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    Ok(Json(events))
}

/// Deliver semantic validation events from a durable outbox. The event id is
/// stable across retries, consumers can deduplicate at-least-once delivery,
/// and repeated process restarts resume from SQLite rather than losing a
/// broadcast-only callback.
pub fn spawn_outbox_forwarder(state: Arc<AppState>) {
    let Some(url) = state.config.paloma_webhook_forward_url.clone() else {
        return;
    };
    let secret = state.config.paloma_webhook_secret.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let due = {
                let Ok(connection) = state.validation.lock() else {
                    continue;
                };
                load_due_outbox(&connection, Utc::now()).unwrap_or_default()
            };
            for event in due {
                let body = serde_json::json!({
                    "event_id": event.id,
                    "type": event.event_type,
                    "campaign_id": event.campaign_id,
                    "fingerprint": event.fingerprint,
                    "created_at": event.created_at,
                    "payload": event.payload,
                });
                let Ok(bytes) = serde_json::to_vec(&body) else {
                    continue;
                };
                let mut request = state
                    .http_client
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(bytes.clone());
                if let Some(secret) = secret.as_deref() {
                    use hmac::{Hmac, Mac};
                    if let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) {
                        mac.update(&bytes);
                        request = request.header(
                            "X-Hub-Signature-256",
                            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                        );
                    }
                }
                let result = request.send().await;
                let (delivered, error) = match result {
                    Ok(response) if response.status().is_success() => (true, None),
                    Ok(response) => (false, Some(format!("HTTP {}", response.status()))),
                    Err(error) => (false, Some(error.to_string())),
                };
                let Ok(connection) = state.validation.lock() else {
                    continue;
                };
                if delivered {
                    let _ = connection.execute(
                        "UPDATE validation_outbox SET delivered_at=?2, attempts=attempts+1,
                         last_error=NULL WHERE id=?1 AND delivered_at IS NULL",
                        params![event.id.to_string(), Utc::now().to_rfc3339()],
                    );
                } else {
                    let attempts = event.attempts.saturating_add(1);
                    let delay = (5_i64.saturating_mul(2_i64.pow(attempts.min(10)))).min(3600);
                    let dead_lettered = (attempts >= 12).then(|| Utc::now().to_rfc3339());
                    let _ = connection.execute(
                        "UPDATE validation_outbox SET attempts=?2, next_attempt_at=?3,
                         last_error=?4, dead_lettered_at=?5 WHERE id=?1",
                        params![
                            event.id.to_string(),
                            attempts,
                            (Utc::now() + chrono::Duration::seconds(delay)).to_rfc3339(),
                            error,
                            dead_lettered,
                        ],
                    );
                }
            }
        }
    });
}

fn load_due_outbox(
    connection: &Connection,
    now: DateTime<Utc>,
) -> Result<Vec<OutboxEvent>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id, campaign_id, event_type, fingerprint, payload_json,
                created_at, attempts, next_attempt_at
         FROM validation_outbox
         WHERE delivered_at IS NULL AND dead_lettered_at IS NULL
           AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
         ORDER BY created_at LIMIT 32",
    )?;
    let events = statement
        .query_map(params![now.to_rfc3339()], |row| {
            Ok(OutboxEvent {
                id: parse_uuid_sql(row.get::<_, String>(0)?)?,
                campaign_id: parse_uuid_sql(row.get::<_, String>(1)?)?,
                event_type: row.get(2)?,
                fingerprint: row.get(3)?,
                payload: parse_json_sql(row.get::<_, String>(4)?)?,
                created_at: parse_time_sql(row.get::<_, String>(5)?)?,
                attempts: row.get(6)?,
                next_attempt_at: row
                    .get::<_, Option<String>>(7)?
                    .map(parse_time_sql)
                    .transpose()?,
            })
        })?
        .collect();
    events
}

async fn ack_outbox(
    State(state): State<Arc<AppState>>,
    AxumPath(event_id): AxumPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    let connection = state.validation.lock().map_err(internal)?;
    let changed = connection
        .execute(
            "UPDATE validation_outbox SET delivered_at=?2, attempts=attempts+1
             WHERE id=?1 AND delivered_at IS NULL",
            params![event_id.to_string(), Utc::now().to_rfc3339()],
        )
        .map_err(internal)?;
    if changed == 0 {
        Err((StatusCode::NOT_FOUND, "outbox event not found".to_string()))
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

#[derive(Debug)]
struct CampaignRow {
    project: String,
    profile: Option<String>,
    workspace_id: Option<Uuid>,
    candidate: ValidationCandidate,
    candidate_id: String,
    matrix_version: u32,
    status: String,
    certifying: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn load_campaign_row(
    connection: &Connection,
    id: Uuid,
) -> Result<Option<CampaignRow>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT project, profile, workspace_id, candidate_json, candidate_id,
                    matrix_version, status, certifying, created_at, updated_at
             FROM validation_campaigns WHERE id=?1",
            params![id.to_string()],
            |row| {
                Ok(CampaignRow {
                    project: row.get(0)?,
                    profile: row.get(1)?,
                    workspace_id: row
                        .get::<_, Option<String>>(2)?
                        .map(parse_uuid_sql)
                        .transpose()?,
                    candidate: parse_json_sql(row.get::<_, String>(3)?)?,
                    candidate_id: row.get(4)?,
                    matrix_version: row.get(5)?,
                    status: row.get(6)?,
                    certifying: row.get(7)?,
                    created_at: parse_time_sql(row.get::<_, String>(8)?)?,
                    updated_at: parse_time_sql(row.get::<_, String>(9)?)?,
                })
            },
        )
        .optional()
}

fn load_campaign(connection: &Connection, id: Uuid) -> Result<Option<CampaignView>, String> {
    let Some(row) = load_campaign_row(connection, id).map_err(db_error)? else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT gate_id, spec_json, status, outcome, freshness, execution_json,
                    validation_key, reused_receipt_id, started_at, finished_at
             FROM validation_gates WHERE campaign_id=?1 ORDER BY rowid",
        )
        .map_err(db_error)?;
    let gates = statement
        .query_map(params![id.to_string()], gate_from_row)
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut statement = connection
        .prepare(
            "SELECT receipt_json FROM validation_receipts
             WHERE campaign_id=?1 ORDER BY created_at",
        )
        .map_err(db_error)?;
    let receipts = statement
        .query_map(params![id.to_string()], |row| {
            parse_json_sql(row.get::<_, String>(0)?)
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(Some(CampaignView {
        id,
        project: row.project,
        profile: row.profile,
        workspace_id: row.workspace_id,
        candidate: row.candidate,
        candidate_id: row.candidate_id,
        matrix_version: row.matrix_version,
        status: row.status,
        certifying: row.certifying,
        gates,
        receipts,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

fn load_gate(
    connection: &Connection,
    campaign_id: Uuid,
    gate_id: &str,
) -> Result<Option<GateView>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT gate_id, spec_json, status, outcome, freshness, execution_json,
                    validation_key, reused_receipt_id, started_at, finished_at
             FROM validation_gates WHERE campaign_id=?1 AND gate_id=?2",
            params![campaign_id.to_string(), gate_id],
            gate_from_row,
        )
        .optional()
}

fn gate_from_row(row: &rusqlite::Row<'_>) -> Result<GateView, rusqlite::Error> {
    Ok(GateView {
        id: row.get(0)?,
        spec: parse_json_sql(row.get::<_, String>(1)?)?,
        status: row.get(2)?,
        outcome: row.get(3)?,
        freshness: row.get(4)?,
        execution: row
            .get::<_, Option<String>>(5)?
            .map(parse_json_sql)
            .transpose()?,
        validation_key: row.get(6)?,
        reused_receipt_id: row
            .get::<_, Option<String>>(7)?
            .map(parse_uuid_sql)
            .transpose()?,
        started_at: row
            .get::<_, Option<String>>(8)?
            .map(parse_time_sql)
            .transpose()?,
        finished_at: row
            .get::<_, Option<String>>(9)?
            .map(parse_time_sql)
            .transpose()?,
    })
}

fn reuse_matching_receipts(
    connection: &Connection,
    campaign_id: Uuid,
    candidate: &ValidationCandidate,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "SELECT gate_id, spec_json, validation_key FROM validation_gates WHERE campaign_id=?1",
        )
        .map_err(db_error)?;
    let gates = statement
        .query_map(params![campaign_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                parse_json_sql::<GateSpec>(row.get::<_, String>(1)?)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    for (gate_id, spec, key) in gates {
        if !spec.reuse {
            continue;
        }
        let prior = connection
            .query_row(
                "SELECT id, receipt_json FROM validation_receipts
                 WHERE validation_key=?1 AND outcome='passed' AND freshness='exact_head'
                 ORDER BY created_at DESC LIMIT 1",
                params![key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((prior_id, receipt_json)) = prior else {
            continue;
        };
        let mut receipt = serde_json::from_str::<ValidationReceipt>(&receipt_json)
            .map_err(|error| error.to_string())?;
        if receipt.candidate_id != candidate.id() {
            continue;
        }
        let prior_uuid = Uuid::parse_str(&prior_id).map_err(|error| error.to_string())?;
        receipt.id = Uuid::new_v4();
        receipt.campaign_id = campaign_id;
        receipt.gate_id = gate_id.clone();
        receipt.reused_from = Some(prior_uuid);
        let execution_key = format!("reused:{prior_uuid}");
        connection
            .execute(
                "INSERT INTO validation_receipts
                 (id, campaign_id, gate_id, execution_key, validation_key, receipt_json,
                  outcome, freshness, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'passed', 'exact_head', ?7)",
                params![
                    receipt.id.to_string(),
                    campaign_id.to_string(),
                    gate_id,
                    execution_key,
                    key,
                    json_string(&receipt)?,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(db_error)?;
        connection
            .execute(
                "UPDATE validation_gates SET status='passed', outcome='passed',
                 freshness='exact_head', execution_json=?3, reused_receipt_id=?4,
                 started_at=?5, finished_at=?5 WHERE campaign_id=?1 AND gate_id=?2",
                params![
                    campaign_id.to_string(),
                    gate_id,
                    json_string(&receipt.execution)?,
                    prior_id,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(db_error)?;
    }
    Ok(())
}

fn recompute_campaign(connection: &Connection, campaign_id: Uuid) -> Result<(), String> {
    let mut statement = connection
        .prepare("SELECT gate_id, spec_json, status FROM validation_gates WHERE campaign_id=?1")
        .map_err(db_error)?;
    let gates = statement
        .query_map(params![campaign_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                parse_json_sql::<GateSpec>(row.get::<_, String>(1)?)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    let statuses = gates
        .iter()
        .map(|(id, _, status)| (id.as_str(), status.as_str()))
        .collect::<HashMap<_, _>>();
    for (id, spec, status) in &gates {
        if status == "pending"
            && spec
                .dependencies
                .iter()
                .all(|dependency| statuses.get(dependency.as_str()) == Some(&"passed"))
        {
            connection
                .execute(
                    "UPDATE validation_gates SET status='ready' WHERE campaign_id=?1 AND gate_id=?2",
                    params![campaign_id.to_string(), id],
                )
                .map_err(db_error)?;
        }
    }
    let required = gates
        .iter()
        .filter(|(_, spec, _)| spec.required)
        .collect::<Vec<_>>();
    let failed = required.iter().any(|(_, _, status)| status == "failed");
    let blocked = required.iter().any(|(_, _, status)| status == "blocked");
    let complete = !required.is_empty() && required.iter().all(|(_, _, status)| status == "passed");
    let certifying = complete
        && required
            .iter()
            .any(|(_, spec, status)| spec.mode == ValidationMode::Clean && status == "passed");
    let next_status = if failed {
        "failed"
    } else if blocked {
        "blocked"
    } else if complete {
        "completed"
    } else {
        "active"
    };
    let prior_status: String = connection
        .query_row(
            "SELECT status FROM validation_campaigns WHERE id=?1",
            params![campaign_id.to_string()],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    connection
        .execute(
            "UPDATE validation_campaigns SET status=?2, certifying=?3, updated_at=?4 WHERE id=?1",
            params![
                campaign_id.to_string(),
                next_status,
                certifying,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(db_error)?;
    if prior_status != next_status && matches!(next_status, "failed" | "blocked" | "completed") {
        insert_outbox(
            connection,
            campaign_id,
            "campaign_terminal",
            next_status,
            serde_json::json!({
                "campaign_id": campaign_id,
                "status": next_status,
                "certifying": certifying,
            }),
        )?;
    }
    Ok(())
}

fn insert_outbox(
    connection: &Connection,
    campaign_id: Uuid,
    event_type: &str,
    fingerprint: &str,
    payload: serde_json::Value,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR IGNORE INTO validation_outbox
             (id, campaign_id, event_type, fingerprint, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::new_v4().to_string(),
                campaign_id.to_string(),
                event_type,
                fingerprint,
                payload.to_string(),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(db_error)?;
    Ok(())
}

fn validation_key(candidate: &ValidationCandidate, gate: &GateSpec) -> String {
    digest_json(&(candidate, gate))
}

fn digest_json(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

fn hash_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.trim().as_bytes()))
}

fn json_string(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn parse_json_sql<T: serde::de::DeserializeOwned>(raw: String) -> Result<T, rusqlite::Error> {
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_uuid_sql(raw: String) -> Result<Uuid, rusqlite::Error> {
    Uuid::parse_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_time_sql(raw: String) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                raw.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn db_error(error: rusqlite::Error) -> String {
    format!("validation database: {error}")
}

fn bad_request(message: String) -> ApiError {
    (StatusCode::BAD_REQUEST, message)
}

fn not_found(message: String) -> ApiError {
    (StatusCode::NOT_FOUND, message)
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(commit: char) -> ValidationCandidate {
        ValidationCandidate {
            repo: "https://github.com/example/verity.git".to_string(),
            commit: commit.to_string().repeat(40),
            expected_head: Some(commit.to_string().repeat(40)),
            source_bundle_digest: None,
        }
    }

    fn matrix() -> ValidationMatrix {
        ValidationMatrix {
            version: 1,
            project: "verity".to_string(),
            profile: Some("lean-4.31".to_string()),
            gates: vec![
                GateSpec {
                    id: "targeted".to_string(),
                    description: None,
                    command: vec![
                        "lake".to_string(),
                        "build".to_string(),
                        "CodeData".to_string(),
                    ],
                    cwd: None,
                    dependencies: vec![],
                    required: true,
                    mode: ValidationMode::Incremental,
                    reuse: true,
                    toolchain: Some("leanprover/lean4:v4.31.0".to_string()),
                    timeout_secs: Some(1800),
                    artifacts: vec![],
                },
                GateSpec {
                    id: "clean-final".to_string(),
                    description: None,
                    command: vec!["lake".to_string(), "build".to_string()],
                    cwd: None,
                    dependencies: vec!["targeted".to_string()],
                    required: true,
                    mode: ValidationMode::Clean,
                    reuse: true,
                    toolchain: Some("leanprover/lean4:v4.31.0".to_string()),
                    timeout_secs: Some(7200),
                    artifacts: vec![],
                },
            ],
        }
    }

    #[test]
    fn rejects_cycles_and_invalid_candidates() {
        let mut cyclic = matrix();
        cyclic.gates[0].dependencies = vec!["clean-final".to_string()];
        assert!(cyclic.validate().unwrap_err().contains("cycle"));
        let mut invalid = candidate('a');
        invalid.commit = "short".to_string();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn diagnostic_clusters_ignore_location_numbers() {
        let clusters =
            cluster_diagnostics("Foo.lean:10:2: type mismatch\nFoo.lean:99:8: type mismatch\nbar");
        assert_eq!(clusters[0].count, 2);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn exact_head_clean_gate_is_required_for_certification() {
        let dir = tempfile::tempdir().unwrap();
        let store = ValidationStore::open(dir.path().join("validation.db")).unwrap();
        let campaign = store
            .create(CreateCampaignRequest {
                candidate: candidate('a'),
                matrix: matrix(),
                workspace_id: None,
            })
            .unwrap();
        assert_eq!(campaign.gates[0].status, "ready");
        assert_eq!(campaign.gates[1].status, "pending");
        assert!(!campaign.certifying);
    }

    #[test]
    fn dependency_progression_requires_exact_pass_and_clean_gate_certifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = ValidationStore::open(dir.path().join("validation.db")).unwrap();
        let campaign = store
            .create(CreateCampaignRequest {
                candidate: candidate('a'),
                matrix: matrix(),
                workspace_id: None,
            })
            .unwrap();
        let connection = store.lock().unwrap();
        connection
            .execute(
                "UPDATE validation_gates SET status='stale', outcome='passed', freshness='stale'
                 WHERE campaign_id=?1 AND gate_id='targeted'",
                params![campaign.id.to_string()],
            )
            .unwrap();
        recompute_campaign(&connection, campaign.id).unwrap();
        assert_eq!(
            load_gate(&connection, campaign.id, "clean-final")
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
        connection
            .execute(
                "UPDATE validation_gates SET status='passed', freshness='exact_head'
                 WHERE campaign_id=?1 AND gate_id='targeted'",
                params![campaign.id.to_string()],
            )
            .unwrap();
        recompute_campaign(&connection, campaign.id).unwrap();
        assert_eq!(
            load_gate(&connection, campaign.id, "clean-final")
                .unwrap()
                .unwrap()
                .status,
            "ready"
        );
        connection
            .execute(
                "UPDATE validation_gates SET status='passed', outcome='passed', freshness='exact_head'
                 WHERE campaign_id=?1 AND gate_id='clean-final'",
                params![campaign.id.to_string()],
            )
            .unwrap();
        recompute_campaign(&connection, campaign.id).unwrap();
        drop(connection);
        let campaign = store.get(campaign.id).unwrap();
        assert_eq!(campaign.status, "completed");
        assert!(campaign.certifying);
    }

    #[test]
    fn material_outbox_coalesces_same_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let store = ValidationStore::open(dir.path().join("validation.db")).unwrap();
        let campaign = store
            .create(CreateCampaignRequest {
                candidate: candidate('a'),
                matrix: matrix(),
                workspace_id: None,
            })
            .unwrap();
        let connection = store.lock().unwrap();
        for _ in 0..3 {
            insert_outbox(
                &connection,
                campaign.id,
                "blocker_changed",
                "same-blocker",
                serde_json::json!({"reason": "same"}),
            )
            .unwrap();
        }
        let count: u32 = connection
            .query_row(
                "SELECT count(*) FROM validation_outbox
                 WHERE campaign_id=?1 AND event_type='blocker_changed'",
                params![campaign.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn matrix_toml_is_versioned_and_uses_argv() {
        let raw = r#"
version = 1
project = "verity"
profile = "lean-4.31"

[[gates]]
id = "compiler"
command = ["lake", "build", "Compiler"]
mode = "clean"
"#;
        let matrix: ValidationMatrix = toml::from_str(raw).unwrap();
        matrix.validate().unwrap();
        assert_eq!(matrix.gates[0].mode, ValidationMode::Clean);
    }
}
