//! Durable, restartable context replica. The caller owns scheduling and credentials.
use crate::project_context::{Entry, Manifest, Operation, Receipt, Result, Store};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct ReplicaState {
    pub initialized: bool,
    #[serde(default)]
    pub ready: bool,
    pub baseline: Manifest,
    pub pending: Vec<Operation>,
    #[serde(default)]
    pub conflict_versions: BTreeMap<String, Option<String>>,
    pub conflicts: BTreeMap<String, String>,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct Replica {
    pub store: Store,
    pub endpoint: String,
    pub token: String,
    pub project: String,
    pub source: String,
}
fn same(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.hash == b.hash && a.directory == b.directory,
        _ => false,
    }
}
impl Replica {
    fn state_path(&self) -> PathBuf {
        self.store.metadata.join("replica.json")
    }
    pub fn status(&self) -> Result<ReplicaState> {
        match std::fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ReplicaState::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn save(&self, state: &ReplicaState) -> Result<()> {
        super::project_context::atomic(
            &self.state_path(),
            &serde_json::to_vec(state).map_err(|e| e.to_string())?,
        )
    }
    fn url(&self, suffix: &str) -> Result<String> {
        let mut url = reqwest::Url::parse(&self.endpoint).map_err(|e| e.to_string())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("context endpoint must use HTTP(S)".into());
        }
        url.path_segments_mut()
            .map_err(|_| "invalid context endpoint")?
            .pop_if_empty()
            .extend(["api", "projects", &self.project, "context"]);
        Ok(format!("{}/{}", url.as_str().trim_end_matches('/'), suffix))
    }
    async fn request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        suffix: &str,
        body: Option<Vec<u8>>,
        json: bool,
    ) -> Result<reqwest::Response> {
        let mut request = client
            .request(method, self.url(suffix)?)
            .bearer_auth(&self.token);
        if json {
            request = request.header("Content-Type", "application/json");
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Context server unavailable".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "Context sync returned HTTP {}",
                response.status().as_u16()
            ));
        }
        Ok(response)
    }
    pub async fn tick(&self) -> Result<ReplicaState> {
        std::fs::create_dir_all(&self.store.metadata).map_err(|e| e.to_string())?;
        let guard = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.store.metadata.join("sync.lock"))
            .map_err(|e| e.to_string())?;
        if guard.try_lock_exclusive().is_err() {
            return self.status();
        }
        let mut state = self.status()?;
        let result = self.synchronize(&mut state).await;
        state.error = result.err();
        self.save(&state)?;
        Ok(state)
    }
    async fn synchronize(&self, state: &mut ReplicaState) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let local = self.store.manifest()?;
        // Persist outgoing intent before any network operations, including while offline.
        if state.initialized {
            let paths: BTreeSet<_> = local
                .entries
                .keys()
                .chain(state.baseline.entries.keys())
                .cloned()
                .collect();
            for path in paths {
                if state.conflicts.contains_key(&path)
                    || state.pending.iter().any(|op| op.path == path)
                {
                    continue;
                }
                let entry = local.entries.get(&path);
                let base = state.baseline.entries.get(&path);
                if same(entry, base) {
                    continue;
                }
                let replacing = entry
                    .zip(base)
                    .is_some_and(|(entry, base)| entry.directory != base.directory);
                state.pending.push(Operation {
                    id: uuid::Uuid::new_v4().to_string(),
                    path,
                    base: base.map(|e| e.revision),
                    hash: if replacing {
                        None
                    } else {
                        entry.and_then(|e| e.hash.clone())
                    },
                    directory: !replacing && entry.is_some_and(|e| e.directory),
                    delete: replacing || entry.is_none(),
                    source: self.source.clone(),
                });
            }
            // Children before parents on deletion; parents before children on creation.
            state.pending.sort_by(|a, b| {
                b.delete.cmp(&a.delete).then_with(|| {
                    if a.delete {
                        b.path.len().cmp(&a.path.len())
                    } else {
                        a.path.len().cmp(&b.path.len())
                    }
                })
            });
            self.save(state)?;
        }
        let _: Manifest = self
            .request(&client, reqwest::Method::GET, "manifest", None, false)
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if !state.initialized && !local.entries.is_empty() {
            return Err("Uninitialized context cache contains files; refusing to overwrite".into());
        }
        if !state.initialized {
            state.initialized = true;
            self.save(state)?;
        }
        while let Some(op) = state.pending.first().cloned() {
            if let Some(hash) = &op.hash {
                self.request(
                    &client,
                    reqwest::Method::POST,
                    "blobs",
                    Some(self.store.blob(hash)?),
                    false,
                )
                .await?;
            }
            let receipt: Receipt = self
                .request(
                    &client,
                    reqwest::Method::POST,
                    "operations",
                    Some(serde_json::to_vec(&op).map_err(|e| e.to_string())?),
                    true,
                )
                .await?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            if receipt.conflict {
                state.conflicts.insert(op.path.clone(), op.id.clone());
                state
                    .conflict_versions
                    .insert(op.path.clone(), op.hash.clone());
            } else if op.delete {
                state.baseline.entries.remove(&op.path);
            } else {
                state.baseline.entries.insert(
                    op.path.clone(),
                    Entry {
                        hash: op.hash.clone(),
                        directory: op.directory,
                        revision: receipt.revision,
                        size: 0,
                    },
                );
            }
            state.pending.remove(0);
            self.save(state)?;
        }
        let remote: Manifest = self
            .request(&client, reqwest::Method::GET, "manifest", None, false)
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let current = self.store.manifest()?;
        if !state.conflicts.is_empty() {
            let open: BTreeMap<String, Operation> = self
                .request(&client, reqwest::Method::GET, "conflicts", None, false)
                .await?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            let resolved: Vec<_> = state
                .conflicts
                .iter()
                .filter(|(_, id)| !open.contains_key(*id))
                .map(|(path, _)| path.clone())
                .collect();
            for path in resolved {
                let existing = current.entries.get(&path);
                if existing.and_then(|e| e.hash.as_ref())
                    == state.conflict_versions.get(&path).and_then(|h| h.as_ref())
                {
                    if let Some(entry) = existing {
                        state.baseline.entries.insert(path.clone(), entry.clone());
                    } else {
                        state.baseline.entries.remove(&path);
                    }
                }
                state.conflicts.remove(&path);
                state.conflict_versions.remove(&path);
            }
            self.save(state)?;
        }
        let paths: BTreeSet<_> = remote
            .entries
            .keys()
            .chain(state.baseline.entries.keys())
            .cloned()
            .collect();
        let mut paths: Vec<_> = paths.into_iter().collect();
        paths.sort_by(|a, b| {
            let da = !remote.entries.contains_key(a);
            let db = !remote.entries.contains_key(b);
            db.cmp(&da).then_with(|| {
                if da {
                    b.len().cmp(&a.len())
                } else {
                    a.len().cmp(&b.len())
                }
            })
        });
        for path in paths {
            if state.conflicts.contains_key(&path) {
                continue;
            }
            let desired = remote.entries.get(&path);
            let baseline = state.baseline.entries.get(&path);
            let existing = current.entries.get(&path);
            // A local write during network I/O must be published on the next pass, never clobbered.
            if state.initialized && !same(existing, baseline) {
                continue;
            }
            let mut expected = existing.map(|e| e.revision);
            if existing
                .zip(desired)
                .is_some_and(|(a, b)| a.directory != b.directory)
            {
                let removed = self.store.apply(Operation {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: path.clone(),
                    base: expected,
                    hash: None,
                    directory: false,
                    delete: true,
                    source: "sync".into(),
                })?;
                if removed.conflict {
                    continue;
                }
                expected = None;
            }
            if !same(existing, desired) {
                if let Some(hash) = desired.and_then(|e| e.hash.as_ref()) {
                    let mut response = self
                        .request(
                            &client,
                            reqwest::Method::GET,
                            &format!("blobs/{hash}"),
                            None,
                            false,
                        )
                        .await?;
                    if response
                        .content_length()
                        .is_some_and(|size| size > super::project_context::FILE_LIMIT as u64)
                    {
                        return Err("Context file exceeds limit".into());
                    }
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                        if bytes.len() + chunk.len() > super::project_context::FILE_LIMIT {
                            return Err("Context file exceeds limit".into());
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    if self.store.put_blob(&bytes)? != *hash {
                        return Err("Context checksum mismatch".into());
                    }
                }
                let receipt = self.store.apply(Operation {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: path.clone(),
                    base: expected,
                    hash: desired.and_then(|e| e.hash.clone()),
                    directory: desired.is_some_and(|e| e.directory),
                    delete: desired.is_none(),
                    source: "sync".into(),
                })?;
                if receipt.conflict {
                    continue;
                }
            }
            if let Some(entry) = desired {
                state.baseline.entries.insert(path, entry.clone());
            } else {
                state.baseline.entries.remove(&path);
            }
            self.save(state)?;
        }
        state.baseline.revision = remote.revision;
        state.initialized = true;
        state.ready = true;
        state.error = None;
        self.save(state)
    }
}
