//! Durable native-thread identity. The mission-store session id is a projection,
//! not permission to guess which native rollout belongs to a mission.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SESSION_PREFIX: &str = "codex-thread:";

#[derive(Debug, thiserror::Error)]
#[error("{evidence}")]
pub struct NativeGoalStop {
    /// Read from goal/get, never inferred from prose or a transport error.
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub mission_id: Uuid,
    pub workspace_id: Uuid,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub directory_ids: Vec<(u64, u64)>,
    /// Hash of the stable account id (or API key), never refresh/access tokens.
    pub account: String,
}

impl Identity {
    pub fn new(
        mission_id: Uuid,
        workspace_id: Uuid,
        cwd: &Path,
        home: &Path,
        account: String,
    ) -> anyhow::Result<Self> {
        let cwd = cwd.canonicalize().context("resolve Codex cwd")?;
        let home = home.canonicalize().context("resolve Codex HOME")?;
        let codex_home = home
            .join(".codex")
            .canonicalize()
            .context("resolve CODEX_HOME")?;
        let mut directory_ids = Vec::new();
        for path in [&cwd, &home, &codex_home] {
            let metadata = std::fs::metadata(path)?;
            if !metadata.is_dir() {
                bail!("codex_continuity_identity: expected a directory");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                directory_ids.push((metadata.dev(), metadata.ino()));
            }
        }
        Ok(Self {
            mission_id,
            workspace_id,
            cwd,
            home,
            codex_home,
            directory_ids,
            account,
        })
    }
}

pub fn account_fingerprint(kind: &str, identity: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{kind}:{identity}")))
}

pub fn binding_path(working_dir: &Path, mission_id: Uuid) -> PathBuf {
    working_dir
        .join(".sandboxed-sh/codex-sessions")
        .join(format!("{mission_id}.json"))
}

#[derive(Debug, Clone)]
pub struct Config {
    pub path: PathBuf,
    pub identity: Identity,
    pub projected_session: Option<String>,
    /// Only the current user input is sent when native history already exists.
    pub current_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub version: u32,
    pub identity: Identity,
    /// None is a durable creation fence, not permission to create another thread.
    pub thread_id: Option<String>,
    #[serde(default)]
    pub goal_seen: bool,
}

pub fn read(path: &Path) -> anyhow::Result<Option<Binding>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let binding: Binding = serde_json::from_slice(&bytes)
                .context("codex_continuity_invalid: malformed native binding")?;
            if binding.version != 1 {
                bail!("codex_continuity_invalid: unsupported native binding version");
            }
            Ok(Some(binding))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("codex_continuity_unavailable: read native binding"),
    }
}

/// Held until the app-server has stopped. Existing actor/track fences remain
/// authoritative; this also excludes two processes attaching one native thread.
pub struct Lease {
    _file: std::fs::File,
    path: PathBuf,
    pub binding: Binding,
    pub resumed: bool,
}

impl Lease {
    pub async fn acquire(config: &Config) -> anyhow::Result<Self> {
        let config = config.clone();
        tokio::task::spawn_blocking(move || {
            let parent = config.path.parent().context("native binding has no parent")?;
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).truncate(false).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(config.path.with_extension("lock"))?;
            fs2::FileExt::try_lock_exclusive(&file)
                .context("codex_continuity_busy: native thread already attached")?;
            let existing = read(&config.path)?;
            let resumed = existing.is_some();
            let binding = match existing {
                Some(binding) => {
                    if binding.identity != config.identity {
                        bail!("codex_continuity_identity: workspace, cwd, HOME or account changed; refusing a fresh thread");
                    }
                    let id = binding.thread_id.as_deref().context(
                        "codex_continuity_creation_unknown: native thread creation requires reconciliation",
                    )?;
                    if let Some(projected) = config.projected_session.as_deref().and_then(|s| s.strip_prefix(SESSION_PREFIX)) {
                        if projected != id {
                            bail!("codex_continuity_identity: mission projection names another native thread");
                        }
                    }
                    binding
                }
                None => {
                    if config.projected_session.as_deref().is_some_and(|s| s.starts_with(SESSION_PREFIX)) {
                        bail!("codex_continuity_missing: projected native thread has no durable binding; refusing a fresh thread");
                    }
                    Binding { version: 1, identity: config.identity, thread_id: None, goal_seen: false }
                }
            };
            Ok(Self { _file: file, path: config.path, binding, resumed })
        }).await?
    }

    /// Persist immediately before thread/start, after the handshake/auth checks.
    /// An ambiguous start must never create a second thread on retry.
    pub fn prepare_creation(&self) -> anyhow::Result<()> {
        self.persist()
    }

    pub fn bind(&mut self, thread_id: &str) -> anyhow::Result<()> {
        if thread_id.is_empty() || thread_id.len() > 256 {
            bail!("codex_continuity_invalid: invalid native thread id");
        }
        self.binding.thread_id = Some(thread_id.to_string());
        self.persist()
    }

    pub fn journal_path(&self) -> PathBuf {
        self.path.with_extension("tools.json")
    }

    pub fn note_goal(&mut self) -> anyhow::Result<()> {
        if !self.binding.goal_seen {
            self.binding.goal_seen = true;
            self.persist()?;
        }
        Ok(())
    }

    fn persist(&self) -> anyhow::Result<()> {
        use std::io::Write;
        let tmp = self.path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&self.binding)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        std::fs::File::open(self.path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self._file);
    }
}
