//! Durable job store for the `sandboxed-node` runner.
//!
//! Jobs live in a small SQLite database at `<workdir>/jobs.db` so job state
//! survives node restarts. All rusqlite calls run under
//! `tokio::task::spawn_blocking`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

/// Lifecycle state of a node job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// The node restarted while the job was queued or running; its process
    /// (if any) is gone and its true outcome is unknown.
    Lost,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "lost" => Self::Lost,
            _ => return None,
        })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost
        )
    }
}

/// One persisted job row.
#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: Uuid,
    pub mission_id: Uuid,
    pub payload_json: String,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub log_path: Option<String>,
    pub error: Option<String>,
    /// JSON-encoded `Vec<ArtifactEntry>` recorded after a successful build
    /// job; `None` for raw commands and unfinished/failed jobs.
    pub artifacts_json: Option<String>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// SQLite-backed job store; cheap to clone.
#[derive(Clone)]
pub struct JobStore {
    conn: Arc<Mutex<Connection>>,
}

impl JobStore {
    /// Open (creating if needed) `<workdir>/jobs.db` and ensure the schema.
    pub async fn open(work_dir: &Path) -> anyhow::Result<Self> {
        let work_dir = work_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&work_dir)?;
            let conn = Connection::open(work_dir.join("jobs.db"))?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    mission_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    state TEXT NOT NULL,
                    exit_code INTEGER,
                    created_at TEXT NOT NULL,
                    started_at TEXT,
                    finished_at TEXT,
                    log_path TEXT,
                    error TEXT,
                    artifacts_json TEXT
                );",
            )?;
            // Migration for jobs.db files created before artifacts shipped.
            // "duplicate column name" on already-migrated DBs is expected.
            let _ = conn.execute("ALTER TABLE jobs ADD COLUMN artifacts_json TEXT", []);
            Ok(Self {
                conn: Arc::new(Mutex::new(conn)),
            })
        })
        .await?
    }

    async fn with_conn<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        let result = tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&guard)
        })
        .await?;
        Ok(result?)
    }

    /// Flip any job left `queued` or `running` by a previous process lifetime
    /// to `lost`. Returns the number of recovered rows.
    pub async fn recover_on_start(&self) -> anyhow::Result<usize> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE jobs
                 SET state = 'lost', finished_at = ?1,
                     error = 'node restarted while the job was in flight'
                 WHERE state IN ('queued', 'running')",
                params![now_rfc3339()],
            )
        })
        .await
    }

    /// Insert a new job in `queued` state.
    pub async fn create(
        &self,
        id: Uuid,
        mission_id: Uuid,
        payload_json: String,
        log_path: String,
    ) -> anyhow::Result<()> {
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO jobs (id, mission_id, payload_json, state, created_at, log_path)
                 VALUES (?1, ?2, ?3, 'queued', ?4, ?5)",
                params![
                    id.to_string(),
                    mission_id.to_string(),
                    payload_json,
                    now_rfc3339(),
                    log_path
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn mark_running(&self, id: Uuid) -> anyhow::Result<()> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE jobs SET state = 'running', started_at = ?2 WHERE id = ?1",
                params![id.to_string(), now_rfc3339()],
            )
            .map(|_| ())
        })
        .await
    }

    /// Record a terminal state for the job.
    pub async fn finish(
        &self,
        id: Uuid,
        state: JobState,
        exit_code: Option<i32>,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        self.finish_with_artifacts(id, state, exit_code, error, None)
            .await
    }

    /// Record a terminal state plus (for successful build jobs) the resolved
    /// artifact manifest as JSON.
    pub async fn finish_with_artifacts(
        &self,
        id: Uuid,
        state: JobState,
        exit_code: Option<i32>,
        error: Option<String>,
        artifacts_json: Option<String>,
    ) -> anyhow::Result<()> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE jobs SET state = ?2, exit_code = ?3, error = ?4, finished_at = ?5,
                        artifacts_json = ?6
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    state.as_str(),
                    exit_code,
                    error,
                    now_rfc3339(),
                    artifacts_json,
                ],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<JobRecord>> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, mission_id, payload_json, state, exit_code, created_at,
                        started_at, finished_at, log_path, error, artifacts_json
                 FROM jobs WHERE id = ?1",
                params![id.to_string()],
                row_to_record,
            )
            .optional()
        })
        .await
    }

    /// Most recently created jobs, newest first.
    pub async fn recent(&self, limit: usize) -> anyhow::Result<Vec<JobRecord>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, mission_id, payload_json, state, exit_code, created_at,
                        started_at, finished_at, log_path, error, artifacts_json
                 FROM jobs ORDER BY created_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_record)?;
            rows.collect()
        })
        .await
    }
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<JobRecord> {
    let parse_uuid = |idx: usize| -> rusqlite::Result<Uuid> {
        let raw: String = row.get(idx)?;
        Uuid::parse_str(&raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    let state_raw: String = row.get(3)?;
    Ok(JobRecord {
        id: parse_uuid(0)?,
        mission_id: parse_uuid(1)?,
        payload_json: row.get(2)?,
        state: JobState::parse(&state_raw).unwrap_or(JobState::Lost),
        exit_code: row.get(4)?,
        created_at: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        log_path: row.get(8)?,
        error: row.get(9)?,
        artifacts_json: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn job_state_machine_create_run_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        let job_id = Uuid::new_v4();
        let mission_id = Uuid::new_v4();
        store
            .create(
                job_id,
                mission_id,
                "{\"kind\":\"raw_command\",\"command\":\"true\"}".to_string(),
                "logs/test.log".to_string(),
            )
            .await
            .unwrap();

        let record = store.get(job_id).await.unwrap().unwrap();
        assert_eq!(record.state, JobState::Queued);
        assert_eq!(record.mission_id, mission_id);
        assert!(record.started_at.is_none());
        assert!(!record.state.is_terminal());

        store.mark_running(job_id).await.unwrap();
        let record = store.get(job_id).await.unwrap().unwrap();
        assert_eq!(record.state, JobState::Running);
        assert!(record.started_at.is_some());
        assert!(record.finished_at.is_none());

        store
            .finish(job_id, JobState::Succeeded, Some(0), None)
            .await
            .unwrap();
        let record = store.get(job_id).await.unwrap().unwrap();
        assert_eq!(record.state, JobState::Succeeded);
        assert_eq!(record.exit_code, Some(0));
        assert!(record.finished_at.is_some());
        assert!(record.state.is_terminal());

        // Unknown job ids read as None.
        assert!(store.get(Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recover_on_start_flips_inflight_jobs_to_lost() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        let queued = Uuid::new_v4();
        let running = Uuid::new_v4();
        let done = Uuid::new_v4();
        for id in [queued, running, done] {
            store
                .create(
                    id,
                    Uuid::new_v4(),
                    "{}".to_string(),
                    "logs/x.log".to_string(),
                )
                .await
                .unwrap();
        }
        store.mark_running(running).await.unwrap();
        store
            .finish(done, JobState::Succeeded, Some(0), None)
            .await
            .unwrap();

        // Simulate a restart: reopen the same database and recover.
        drop(store);
        let store = JobStore::open(dir.path()).await.unwrap();
        let recovered = store.recover_on_start().await.unwrap();
        assert_eq!(recovered, 2);

        for id in [queued, running] {
            let record = store.get(id).await.unwrap().unwrap();
            assert_eq!(record.state, JobState::Lost);
            assert!(record.finished_at.is_some());
            assert!(record.error.as_deref().unwrap_or("").contains("restarted"));
        }
        // Terminal jobs are untouched.
        let record = store.get(done).await.unwrap().unwrap();
        assert_eq!(record.state, JobState::Succeeded);

        let recent = store.recent(10).await.unwrap();
        assert_eq!(recent.len(), 3);
    }
}
