use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::http::header;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::routes::AppState;

pub const DEFER_ON_RATE_LIMIT_HEADER: &str = "x-sandboxed-defer-on-rate-limit";
const DEFAULT_TTL_HOURS: i64 = 24;
const WORKER_POLL_INTERVAL_SECS: u64 = 2;
const WORKER_FALLBACK_RETRY_SECS: i64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeferredRequestStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredResponsePayload {
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredRequestRecord {
    pub id: Uuid,
    pub chain_id: String,
    pub request_payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_organization: Option<String>,
    pub status: DeferredRequestStatus,
    pub attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub next_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_payload: Option<DeferredResponsePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DeferredRequestStoreFile {
    requests: Vec<DeferredRequestRecord>,
}

pub struct DeferredRequestStore {
    path: PathBuf,
    inner: RwLock<HashMap<Uuid, DeferredRequestRecord>>,
}

impl DeferredRequestStore {
    pub async fn new(path: PathBuf) -> Self {
        let requests = match tokio::fs::read_to_string(&path).await {
            Ok(content) => match serde_json::from_str::<DeferredRequestStoreFile>(&content) {
                Ok(file) => file,
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "Failed to parse deferred request store file");
                    DeferredRequestStoreFile::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                DeferredRequestStoreFile::default()
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "Failed to read deferred request store file");
                DeferredRequestStoreFile::default()
            }
        };

        let mut map = HashMap::new();
        for req in requests.requests {
            map.insert(req.id, req);
        }

        Self {
            path,
            inner: RwLock::new(map),
        }
    }

    pub async fn enqueue(
        &self,
        chain_id: String,
        request_payload: serde_json::Value,
        openai_organization: Option<String>,
        next_attempt_at: DateTime<Utc>,
    ) -> DeferredRequestRecord {
        let now = Utc::now();
        let rec = DeferredRequestRecord {
            id: Uuid::new_v4(),
            chain_id,
            request_payload,
            openai_organization,
            status: DeferredRequestStatus::Queued,
            attempt_count: 0,
            last_error: None,
            next_attempt_at,
            created_at: now,
            updated_at: now,
            expires_at: now + chrono::Duration::hours(DEFAULT_TTL_HOURS),
            response_payload: None,
        };
        self.upsert(rec.clone()).await;
        rec
    }

    pub async fn get(&self, id: Uuid) -> Option<DeferredRequestRecord> {
        self.expire_due().await;
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    pub async fn cancel(&self, id: Uuid) -> Option<DeferredRequestRecord> {
        let maybe = {
            let mut guard = self.inner.write().await;
            let rec = guard.get_mut(&id)?;
            if matches!(
                rec.status,
                DeferredRequestStatus::Succeeded
                    | DeferredRequestStatus::Failed
                    | DeferredRequestStatus::Canceled
                    | DeferredRequestStatus::Expired
            ) {
                return Some(rec.clone());
            }
            rec.status = DeferredRequestStatus::Canceled;
            rec.updated_at = Utc::now();
            Some(rec.clone())
        };
        self.persist().await;
        maybe
    }

    pub async fn claim_due_job(&self) -> Option<DeferredRequestRecord> {
        self.expire_due().await;
        let now = Utc::now();
        let claimed = {
            let mut guard = self.inner.write().await;
            let next_id = guard
                .values()
                .filter(|r| {
                    r.status == DeferredRequestStatus::Queued
                        && r.next_attempt_at <= now
                        && r.expires_at > now
                })
                .min_by_key(|r| r.created_at)
                .map(|r| r.id);
            let id = next_id?;
            let rec = guard.get_mut(&id)?;
            rec.status = DeferredRequestStatus::Running;
            rec.attempt_count = rec.attempt_count.saturating_add(1);
            rec.updated_at = now;
            Some(rec.clone())
        };
        self.persist().await;
        claimed
    }

    pub async fn mark_succeeded(
        &self,
        id: Uuid,
        response_payload: DeferredResponsePayload,
    ) -> Option<DeferredRequestRecord> {
        let updated = {
            let mut guard = self.inner.write().await;
            let rec = guard.get_mut(&id)?;
            rec.status = DeferredRequestStatus::Succeeded;
            rec.response_payload = Some(response_payload);
            rec.last_error = None;
            rec.updated_at = Utc::now();
            Some(rec.clone())
        };
        self.persist().await;
        updated
    }

    pub async fn mark_failed(&self, id: Uuid, error: String) -> Option<DeferredRequestRecord> {
        let updated = {
            let mut guard = self.inner.write().await;
            let rec = guard.get_mut(&id)?;
            rec.status = DeferredRequestStatus::Failed;
            rec.last_error = Some(error);
            rec.updated_at = Utc::now();
            Some(rec.clone())
        };
        self.persist().await;
        updated
    }

    pub async fn mark_requeued(
        &self,
        id: Uuid,
        next_attempt_at: DateTime<Utc>,
        error: String,
    ) -> Option<DeferredRequestRecord> {
        let updated = {
            let mut guard = self.inner.write().await;
            let rec = guard.get_mut(&id)?;
            rec.status = DeferredRequestStatus::Queued;
            rec.next_attempt_at = next_attempt_at;
            rec.last_error = Some(error);
            rec.updated_at = Utc::now();
            Some(rec.clone())
        };
        self.persist().await;
        updated
    }

    async fn upsert(&self, rec: DeferredRequestRecord) {
        {
            let mut guard = self.inner.write().await;
            guard.insert(rec.id, rec);
        }
        self.persist().await;
    }

    async fn expire_due(&self) {
        let now = Utc::now();
        let mut changed = false;
        {
            let mut guard = self.inner.write().await;
            for rec in guard.values_mut() {
                if rec.expires_at <= now
                    && matches!(
                        rec.status,
                        DeferredRequestStatus::Queued | DeferredRequestStatus::Running
                    )
                {
                    rec.status = DeferredRequestStatus::Expired;
                    rec.last_error = Some("deferred request expired".to_string());
                    rec.updated_at = now;
                    changed = true;
                }
            }
        }
        if changed {
            self.persist().await;
        }
    }

    async fn persist(&self) {
        let snapshot = {
            let guard = self.inner.read().await;
            let mut requests: Vec<DeferredRequestRecord> = guard.values().cloned().collect();
            requests.sort_by_key(|r| r.created_at);
            DeferredRequestStoreFile { requests }
        };

        if let Some(parent) = self.path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(path = %self.path.display(), error = %err, "Failed to create deferred request store directory");
                return;
            }
        }

        match serde_json::to_vec_pretty(&snapshot) {
            Ok(bytes) => {
                if let Err(err) = tokio::fs::write(&self.path, bytes).await {
                    tracing::warn!(path = %self.path.display(), error = %err, "Failed to persist deferred request store");
                }
            }
            Err(err) => {
                tracing::warn!(path = %self.path.display(), error = %err, "Failed to serialize deferred request store");
            }
        }
    }
}

pub fn start_worker(state: Arc<AppState>) {
    tokio::spawn(async move {
        worker_loop(state).await;
    });
}

async fn worker_loop(state: Arc<AppState>) {
    let worker_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(330))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    loop {
        let mut processed_any = false;

        while let Some(job) = state.deferred_requests.claim_due_job().await {
            processed_any = true;
            process_one_job(&worker_client, &state, job).await;
        }

        if !processed_any {
            tokio::time::sleep(std::time::Duration::from_secs(WORKER_POLL_INTERVAL_SECS)).await;
        }
    }
}

async fn process_one_job(
    client: &reqwest::Client,
    state: &Arc<AppState>,
    job: DeferredRequestRecord,
) {
    let local_host = match state.config.host.as_str() {
        "0.0.0.0" | "::" => "127.0.0.1",
        host => host,
    };
    let url = format!(
        "http://{}:{}/v1/chat/completions",
        local_host, state.config.port
    );

    let mut request = client
        .post(url)
        .header(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {}", state.proxy_secret),
        )
        .header(header::CONTENT_TYPE.as_str(), "application/json")
        .body(job.request_payload.to_string());

    if let Some(org) = &job.openai_organization {
        request = request.header("OpenAI-Organization", org);
    }

    let response = match request.send().await {
        Ok(resp) => resp,
        Err(err) => {
            let next_attempt = Utc::now() + chrono::Duration::seconds(WORKER_FALLBACK_RETRY_SECS);
            let _ = state
                .deferred_requests
                .mark_requeued(
                    job.id,
                    next_attempt,
                    format!("worker request error: {}", err),
                )
                .await;
            return;
        }
    };

    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let body = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = state
                .deferred_requests
                .mark_failed(
                    job.id,
                    format!("failed to read upstream response body: {}", err),
                )
                .await;
            return;
        }
    };

    if status.as_u16() == 429 {
        let next_attempt = estimate_next_attempt_at(state).await;
        let _ = state
            .deferred_requests
            .mark_requeued(
                job.id,
                next_attempt,
                "all providers still rate-limited".to_string(),
            )
            .await;
        return;
    }

    if status.is_success() {
        let parsed_body = serde_json::from_slice::<serde_json::Value>(&body)
            .unwrap_or_else(|_| serde_json::json!({ "raw_body": String::from_utf8_lossy(&body) }));
        let payload = DeferredResponsePayload {
            status_code: status.as_u16(),
            content_type,
            body: parsed_body,
        };
        let _ = state
            .deferred_requests
            .mark_succeeded(job.id, payload)
            .await;
        return;
    }

    let error_text = String::from_utf8_lossy(&body);
    let _ = state
        .deferred_requests
        .mark_failed(
            job.id,
            format!("upstream status {}: {}", status.as_u16(), error_text),
        )
        .await;
}

pub async fn estimate_next_attempt_at(state: &AppState) -> DateTime<Utc> {
    let now = Utc::now();
    let min_cooldown = state
        .health_tracker
        .get_all_health()
        .await
        .into_iter()
        .filter_map(|h| h.cooldown_remaining_secs)
        .filter(|secs| *secs > 0.0)
        .fold(None, |acc: Option<f64>, secs| {
            Some(acc.map_or(secs, |current| current.min(secs)))
        });

    let wait_secs = min_cooldown
        .map(|secs| secs.ceil() as i64)
        .unwrap_or(WORKER_FALLBACK_RETRY_SECS)
        .max(WORKER_FALLBACK_RETRY_SECS);

    now + chrono::Duration::seconds(wait_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deferred_store_state_transitions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("deferred_requests.json");
        let store = DeferredRequestStore::new(path).await;

        let next_attempt_at = Utc::now();
        let queued = store
            .enqueue(
                "builtin/smart".to_string(),
                serde_json::json!({ "model": "builtin/smart", "messages": [] }),
                Some("org-123".to_string()),
                next_attempt_at,
            )
            .await;
        assert_eq!(queued.status, DeferredRequestStatus::Queued);
        assert_eq!(queued.attempt_count, 0);

        let claimed = store.claim_due_job().await.expect("claim due job");
        assert_eq!(claimed.status, DeferredRequestStatus::Running);
        assert_eq!(claimed.attempt_count, 1);

        let requeued = store
            .mark_requeued(
                claimed.id,
                Utc::now() + chrono::Duration::seconds(30),
                "still rate-limited".to_string(),
            )
            .await
            .expect("requeue");
        assert_eq!(requeued.status, DeferredRequestStatus::Queued);
        assert_eq!(requeued.last_error.as_deref(), Some("still rate-limited"));

        let succeeded = store
            .mark_succeeded(
                requeued.id,
                DeferredResponsePayload {
                    status_code: 200,
                    content_type: Some("application/json".to_string()),
                    body: serde_json::json!({ "id": "ok" }),
                },
            )
            .await
            .expect("mark succeeded");
        assert_eq!(succeeded.status, DeferredRequestStatus::Succeeded);
        assert!(succeeded.response_payload.is_some());
    }

    #[tokio::test]
    async fn deferred_store_persists_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("deferred_requests.json");

        let store = DeferredRequestStore::new(path.clone()).await;
        let created = store
            .enqueue(
                "builtin/smart".to_string(),
                serde_json::json!({ "model": "builtin/smart", "messages": [] }),
                None,
                Utc::now(),
            )
            .await;

        let reloaded = DeferredRequestStore::new(path).await;
        let fetched = reloaded
            .get(created.id)
            .await
            .expect("record should exist after reload");
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.chain_id, "builtin/smart");
    }
}
