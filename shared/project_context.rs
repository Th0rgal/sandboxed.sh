#[path = "context_fs.rs"]
mod secure_fs;
// Versioned project context. Metadata and immutable blobs live outside the agent-visible tree.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub const FILE_LIMIT: usize = 10 * 1024 * 1024;
pub const PROJECT_LIMIT: u64 = 100 * 1024 * 1024;
pub const ENTRY_LIMIT: usize = 5000;
pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub hash: Option<String>,
    pub directory: bool,
    pub revision: u64,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    #[serde(default)]
    pub before: Option<Entry>,
    #[serde(default)]
    pub timestamp: u64,
    pub revision: u64,
    pub path: String,
    pub entry: Option<Entry>,
    pub source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub path: String,
    pub base: Option<u64>,
    pub hash: Option<String>,
    #[serde(default)]
    pub directory: bool,
    #[serde(default)]
    pub delete: bool,
    pub source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub revision: u64,
    pub conflict: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub revision: u64,
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct State {
    #[serde(default)]
    root_identity: Option<String>,
    manifest: Manifest,
    history: Vec<Change>,
    conflicts: BTreeMap<String, Operation>,
    receipts: BTreeMap<String, (Operation, Receipt)>,
    // A committed update must be projected before inspecting external edits after a crash.
    pending: Vec<Change>,
}
#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
    pub metadata: PathBuf,
}
fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return Err("invalid context path".into());
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("context path must be canonical and relative".into());
    }
    for part in Path::new(path).components() {
        let Component::Normal(name) = part else {
            return Err("context path must be relative".into());
        };
        let name = name.to_str().ok_or("context path must be UTF-8")?;
        if name.starts_with('.')
            || matches!(name, "node_modules" | "target" | "vendor")
            || name.ends_with('~')
            || matches!(
                name,
                "id_rsa" | "id_ed25519" | "credentials.json" | "auth.json"
            )
            || name.ends_with(".pem")
            || name.ends_with(".key")
        {
            return Err(format!("excluded context path: {path}"));
        }
    }
    Ok(())
}
fn checked(root: &Path, path: &str) -> Result<PathBuf> {
    valid_path(path)?;
    let mut result = root.to_path_buf();
    if fs::symlink_metadata(root)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("context root cannot be a symlink".into());
    }
    for component in Path::new(path).components() {
        result.push(component);
        match fs::symlink_metadata(&result) {
            Ok(meta) if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) => {
                return Err(format!("unsupported context entry: {path}"))
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(result)
}
pub(crate) fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or("missing parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".context-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())?;
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}
impl Store {
    pub fn new(root: PathBuf, metadata: PathBuf) -> Self {
        Self { root, metadata }
    }
    fn lock(&self) -> Result<fs::File> {
        fs::create_dir_all(&self.metadata).map_err(|e| e.to_string())?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.metadata.join("lock"))
            .map_err(|e| e.to_string())?;
        lock.lock_exclusive().map_err(|e| e.to_string())?;
        Ok(lock)
    }
    fn load(&self) -> Result<State> {
        match fs::read(self.metadata.join("state.json")) {
            Ok(data) => serde_json::from_slice(&data).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn save(&self, state: &State) -> Result<()> {
        let mut state = state.clone();
        let cutoff = timestamp().saturating_sub(90 * 86400);
        let mut counts = BTreeMap::<String, usize>::new();
        state.history = state
            .history
            .into_iter()
            .rev()
            .filter(|change| {
                let count = counts.entry(change.path.clone()).or_default();
                *count += 1;
                *count <= 20 || change.timestamp >= cutoff || change.timestamp == 0
            })
            .collect();
        state.history.reverse();
        atomic(
            &self.metadata.join("state.json"),
            &serde_json::to_vec(&state).map_err(|e| e.to_string())?,
        )
    }
    pub fn put_blob(&self, bytes: &[u8]) -> Result<String> {
        if bytes.len() > FILE_LIMIT {
            return Err("context file exceeds 10 MiB".into());
        }
        let hash = digest(bytes);
        let path = self.metadata.join("blobs").join(&hash);
        if !path.exists() {
            atomic(&path, bytes)?;
        }
        Ok(hash)
    }
    pub fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid content hash".into());
        }
        let data = fs::read(self.metadata.join("blobs").join(hash)).map_err(|e| e.to_string())?;
        if digest(&data) != hash {
            return Err("context content checksum mismatch".into());
        }
        Ok(data)
    }
    fn recover(&self, state: &mut State) -> Result<()> {
        for change in state.pending.clone() {
            // Preserve a write made after a commit but before crash recovery.
            let target = checked(&self.root, &change.path)?;
            if target.is_file() {
                let bytes = secure_fs::read(&self.root, &change.path)?;
                let hash = digest(&bytes);
                if change.entry.as_ref().and_then(|e| e.hash.as_ref()) != Some(&hash)
                    && change.before.as_ref().and_then(|e| e.hash.as_ref()) != Some(&hash)
                {
                    self.put_blob(&bytes)?;
                    let id = uuid::Uuid::new_v4().to_string();
                    state.conflicts.insert(
                        id.clone(),
                        Operation {
                            id,
                            path: change.path.clone(),
                            base: change.before.as_ref().map(|e| e.revision),
                            hash: Some(hash),
                            directory: false,
                            delete: false,
                            source: "recovered filesystem edit".into(),
                        },
                    );
                    self.save(state)?;
                }
            }
            let path = checked(&self.root, &change.path)?;
            match &change.entry {
                Some(entry) if entry.directory => secure_fs::mkdir(&self.root, &change.path)?,
                Some(entry) => secure_fs::write(
                    &self.root,
                    &change.path,
                    &self.blob(entry.hash.as_deref().ok_or("missing hash")?)?,
                )?,
                None if path.is_dir() => secure_fs::remove(&self.root, &change.path, true)?,
                None if path.exists() => secure_fs::remove(&self.root, &change.path, false)?,
                None => (),
            }
        }
        if !state.pending.is_empty() {
            state.pending.clear();
            self.save(state)?;
        }
        Ok(())
    }
    fn scan_dir(
        &self,
        rel: &str,
        entries: &mut BTreeMap<String, Entry>,
        size: &mut u64,
    ) -> Result<()> {
        let dir = if rel.is_empty() {
            self.root.clone()
        } else {
            checked(&self.root, rel)?
        };
        for item in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let item = item.map_err(|e| e.to_string())?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| "non UTF-8 context filename")?;
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if valid_path(&path).is_err() {
                continue;
            }
            let full = checked(&self.root, &path)?;
            let meta = fs::metadata(&full).map_err(|e| e.to_string())?;
            if entries.len() >= ENTRY_LIMIT {
                return Err("context exceeds 5000 entries".into());
            }
            let entry = if meta.is_dir() {
                Entry {
                    hash: None,
                    directory: true,
                    revision: 0,
                    size: 0,
                }
            } else {
                if meta.len() > FILE_LIMIT as u64 {
                    return Err(format!("{path} exceeds 10 MiB"));
                }
                let bytes = secure_fs::read(&self.root, &path)?;
                let after = fs::metadata(&full).map_err(|e| e.to_string())?;
                if meta.len() != after.len() || meta.modified().ok() != after.modified().ok() {
                    return Err("context file changed during read; retry".into());
                }
                *size += bytes.len() as u64;
                if *size > PROJECT_LIMIT {
                    return Err("context exceeds 100 MiB".into());
                }
                Entry {
                    hash: Some(self.put_blob(&bytes)?),
                    directory: false,
                    revision: 0,
                    size: bytes.len() as u64,
                }
            };
            let directory = entry.directory;
            if entries
                .keys()
                .any(|existing| existing.to_lowercase() == path.to_lowercase() && existing != &path)
            {
                return Err(
                    "Context filenames differ only by case; rename one before syncing".into(),
                );
            }
            entries.insert(path.clone(), entry);
            if directory {
                self.scan_dir(&path, entries, size)?;
            }
        }
        Ok(())
    }
    fn reconcile(&self, state: &mut State) -> Result<()> {
        if !self.root.exists() {
            if state.manifest.revision > 0 {
                return Err("context root is missing; refusing mass deletion".into());
            }
            fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        }
        let marker = self.root.join(".sandboxed-context-root");
        if let Some(identity) = &state.root_identity {
            if fs::read_to_string(&marker).ok().as_ref() != Some(identity) {
                return Err("context root identity changed; refusing deletion or overwrite".into());
            }
        } else {
            let identity = uuid::Uuid::new_v4().to_string();
            atomic(&marker, identity.as_bytes())?;
            state.root_identity = Some(identity);
            self.save(state)?;
        }
        self.recover(state)?;
        let mut entries = BTreeMap::new();
        self.scan_dir("", &mut entries, &mut 0)?;
        let mut changes = Vec::new();
        for (path, entry) in &mut entries {
            if let Some(old) = state
                .manifest
                .entries
                .get(path)
                .filter(|old| old.hash == entry.hash && old.directory == entry.directory)
            {
                entry.revision = old.revision;
            } else {
                state.manifest.revision += 1;
                entry.revision = state.manifest.revision;
                changes.push(Change {
                    before: state.manifest.entries.get(path).cloned(),
                    timestamp: timestamp(),
                    revision: entry.revision,
                    path: path.clone(),
                    entry: Some(entry.clone()),
                    source: "filesystem".into(),
                });
            }
        }
        for path in state.manifest.entries.keys() {
            if !entries.contains_key(path) {
                state.manifest.revision += 1;
                changes.push(Change {
                    before: state.manifest.entries.get(path).cloned(),
                    timestamp: timestamp(),
                    revision: state.manifest.revision,
                    path: path.clone(),
                    entry: None,
                    source: "filesystem".into(),
                });
            }
        }
        if !changes.is_empty() {
            state.manifest.entries = entries;
            state.history.extend(changes);
            self.save(state)?;
        }
        Ok(())
    }
    pub fn manifest(&self) -> Result<Manifest> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        self.reconcile(&mut state)?;
        Ok(state.manifest)
    }
    pub fn history(&self) -> Result<Vec<Change>> {
        let _lock = self.lock()?;
        Ok(self.load()?.history)
    }
    pub fn resolve(&self, id: &str, operation: Operation) -> Result<Receipt> {
        let _lock = self.lock()?;
        let state = self.load()?;
        if !state.conflicts.contains_key(id) {
            if let Some((saved, receipt)) = state.receipts.get(&operation.id) {
                if serde_json::to_value(saved).unwrap() == serde_json::to_value(&operation).unwrap()
                {
                    return Ok(receipt.clone());
                }
            }
            return Err("conflict no longer exists".into());
        }
        self.apply_locked(operation, state, Some(id))
    }
    pub fn conflicts(&self) -> Result<BTreeMap<String, Operation>> {
        let _lock = self.lock()?;
        Ok(self.load()?.conflicts)
    }
    pub fn apply(&self, operation: Operation) -> Result<Receipt> {
        let _lock = self.lock()?;
        self.apply_locked(operation, self.load()?, None)
    }
    fn apply_locked(
        &self,
        operation: Operation,
        mut state: State,
        resolve: Option<&str>,
    ) -> Result<Receipt> {
        valid_path(&operation.path)?;
        if operation.id.is_empty() || operation.id.len() > 128 || operation.source.len() > 128 {
            return Err("invalid operation identity".into());
        }
        self.reconcile(&mut state)?;
        if let Some((saved, receipt)) = state.receipts.get(&operation.id) {
            if serde_json::to_value(saved).unwrap() != serde_json::to_value(&operation).unwrap() {
                return Err("operation id reused with different content".into());
            }
            return Ok(receipt.clone());
        }
        let current = state.manifest.entries.get(&operation.path);
        let identical = if operation.delete {
            current.is_none()
        } else {
            current.is_some_and(|e| e.hash == operation.hash && e.directory == operation.directory)
        };
        let conflict = !identical && current.map(|e| e.revision) != operation.base;
        if !operation.delete && current.is_none() {
            let parts: Vec<_> = operation.path.split('/').collect();
            for end in 1..=parts.len() {
                let prefix = parts[..end].join("/");
                if state
                    .manifest
                    .entries
                    .keys()
                    .any(|path| path.to_lowercase() == prefix.to_lowercase() && path != &prefix)
                {
                    return Err("context filename or folder collides by case".into());
                }
            }
        }
        if !operation.delete && !operation.directory {
            self.blob(operation.hash.as_deref().ok_or("missing content hash")?)?;
        }
        if conflict {
            state
                .conflicts
                .insert(operation.id.clone(), operation.clone());
        } else if !identical {
            let target = checked(&self.root, &operation.path)?;
            if operation.delete
                && target.is_dir()
                && fs::read_dir(&target)
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_some()
            {
                return Err("delete directory contents first".into());
            }
            if let Some(entry) = current {
                if !operation.delete && entry.directory != operation.directory {
                    return Err("delete existing entry before changing its type".into());
                }
            }
            let size = if operation.directory || operation.delete {
                0
            } else {
                self.blob(operation.hash.as_deref().unwrap())?.len() as u64
            };
            let total: u64 = state.manifest.entries.values().map(|e| e.size).sum();
            if total - current.map_or(0, |e| e.size) + size > PROJECT_LIMIT {
                return Err("context exceeds 100 MiB".into());
            }
            if !operation.delete && current.is_none() && state.manifest.entries.len() >= ENTRY_LIMIT
            {
                return Err("context exceeds 5000 entries".into());
            }
            state.manifest.revision += 1;
            let entry = if operation.delete {
                None
            } else {
                Some(Entry {
                    hash: operation.hash.clone(),
                    directory: operation.directory,
                    revision: state.manifest.revision,
                    size,
                })
            };
            let change = Change {
                before: current.cloned(),
                timestamp: timestamp(),
                revision: state.manifest.revision,
                path: operation.path.clone(),
                entry: entry.clone(),
                source: operation.source.clone(),
            };
            if let Some(entry) = entry {
                state.manifest.entries.insert(operation.path.clone(), entry);
            } else {
                state.manifest.entries.remove(&operation.path);
            }
            state.history.push(change.clone());
            state.pending.push(change);
        }
        let receipt = Receipt {
            revision: if identical {
                state
                    .manifest
                    .entries
                    .get(&operation.path)
                    .map_or(state.manifest.revision, |entry| entry.revision)
            } else {
                state.manifest.revision
            },
            conflict,
        };
        if !conflict {
            if let Some(id) = resolve {
                state.conflicts.remove(id);
            }
        }
        state
            .receipts
            .insert(operation.id.clone(), (operation, receipt.clone()));
        self.save(&state)?;
        self.recover(&mut state)?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("files"), dir.path().join("meta"));
        store.manifest().unwrap();
        (dir, store)
    }
    fn write(store: &Store, id: &str, base: Option<u64>, text: &str) -> Receipt {
        let hash = store.put_blob(text.as_bytes()).unwrap();
        store
            .apply(Operation {
                id: id.into(),
                path: "note.md".into(),
                base,
                hash: Some(hash),
                directory: false,
                delete: false,
                source: "test".into(),
            })
            .unwrap()
    }
    #[test]
    fn identical_write_keeps_file_revision_and_rejects_ambiguous_paths() {
        let (_dir, s) = setup();
        let first = write(&s, "first", None, "same");
        fs::write(s.root.join("other.md"), "other").unwrap();
        s.manifest().unwrap();
        assert_eq!(write(&s, "retry", None, "same").revision, first.revision);
        for path in [
            "notes//file.md",
            "notes/./file.md",
            "notes/../file.md",
            "/absolute",
            "trailing/",
        ] {
            assert!(valid_path(path).is_err());
        }
        fs::create_dir(s.root.join("Notes")).unwrap();
        s.manifest().unwrap();
        let hash = s.put_blob(b"content").unwrap();
        assert!(s
            .apply(Operation {
                id: "case".into(),
                path: "notes/file.md".into(),
                base: None,
                hash: Some(hash),
                directory: false,
                delete: false,
                source: "test".into()
            })
            .is_err());
    }
    #[test]
    fn concurrent_changes_preserve_both() {
        let (_dir, s) = setup();
        let first = write(&s, "1", None, "first");
        assert!(!first.conflict);
        let other = write(&s, "2", None, "other");
        assert!(other.conflict);
        assert_eq!(fs::read_to_string(s.root.join("note.md")).unwrap(), "first");
        assert_eq!(s.conflicts().unwrap().len(), 1);
        assert_eq!(write(&s, "2", None, "other").revision, other.revision);
    }
    #[test]
    fn external_edit_is_versioned() {
        let (_dir, s) = setup();
        let first = write(&s, "1", None, "first");
        fs::write(s.root.join("note.md"), "agent").unwrap();
        assert!(s.manifest().unwrap().revision > first.revision);
        assert!(write(&s, "2", Some(first.revision), "stale").conflict);
    }
    #[test]
    fn missing_root_does_not_delete_context() {
        let (_dir, s) = setup();
        write(&s, "1", None, "first");
        fs::remove_dir_all(&s.root).unwrap();
        assert!(s.manifest().unwrap_err().contains("mass deletion"));
    }
    #[test]
    fn rejects_escape() {
        let (_dir, s) = setup();
        for path in [
            "../secret",
            "/secret",
            "a/../../secret",
            ".env",
            "a/.git/config",
        ] {
            assert!(valid_path(path).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", s.root.join("escape")).unwrap();
            assert!(s.manifest().is_err());
        }
    }
}
