//! Durable app-server tool-call lifecycle journal.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub start_payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Entry {
    Started { descriptor: PendingToolCall },
    Completed { descriptor: PendingToolCall },
}

pub struct ToolCallJournal {
    path: PathBuf,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

impl ToolCallJournal {
    pub fn new(working_dir: &Path, session_id: &str, thread_id: &str) -> Self {
        let safe = |value: &str| {
            value
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        Self {
            path: working_dir
                .join(".sandboxed-sh")
                .join("codex-tool-calls")
                .join(format!("{}-{}.json", safe(session_id), safe(thread_id))),
        }
    }

    async fn load_entries(&self) -> anyhow::Result<Vec<Entry>> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| anyhow::anyhow!("parse {}: {err}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(anyhow::anyhow!("read {}: {err}", self.path.display())),
        }
    }

    async fn store(&self, entries: &[Entry]) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(entries)?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(tmp, &self.path).await?;
        Ok(())
    }

    pub async fn started(&self, descriptor: PendingToolCall) -> anyhow::Result<()> {
        let _guard = lock().lock().await;
        let mut entries = self.load_entries().await?;
        entries.retain(|entry| match entry {
            Entry::Started { descriptor: old } | Entry::Completed { descriptor: old } => {
                old.id != descriptor.id
            }
        });
        entries.push(Entry::Started { descriptor });
        self.store(&entries).await
    }

    pub async fn completed(&self, id: &str) -> anyhow::Result<()> {
        let _guard = lock().lock().await;
        let mut entries = self.load_entries().await?;
        for entry in &mut entries {
            if let Entry::Started { descriptor } = entry {
                if descriptor.id == id {
                    *entry = Entry::Completed {
                        descriptor: descriptor.clone(),
                    };
                    self.store(&entries).await?;
                    break;
                }
            }
        }
        Ok(())
    }

    pub async fn pending(&self) -> anyhow::Result<Vec<PendingToolCall>> {
        Ok(self
            .load_entries()
            .await?
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Started { descriptor } => Some(descriptor),
                Entry::Completed { .. } => None,
            })
            .collect())
    }

    pub async fn clear(&self) -> anyhow::Result<()> {
        let _guard = lock().lock().await;
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_is_atomic_idempotent_and_clearable() {
        let dir = tempfile::tempdir().unwrap();
        let journal = ToolCallJournal::new(dir.path(), "session", "thread");
        let descriptor = PendingToolCall {
            id: "call-1".into(),
            name: "bash".into(),
            start_payload: serde_json::json!({"command": "do-once"}),
        };
        journal.started(descriptor.clone()).await.unwrap();
        journal.started(descriptor.clone()).await.unwrap();
        assert_eq!(journal.pending().await.unwrap(), vec![descriptor]);
        journal.completed("call-1").await.unwrap();
        journal.completed("call-1").await.unwrap();
        assert!(journal.pending().await.unwrap().is_empty());
        journal.clear().await.unwrap();
        assert!(!journal.path.exists());
    }
}
