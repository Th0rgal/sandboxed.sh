//! Node-configured managed authentication for native harness launches.
//!
//! Raw node jobs run with a cleared environment and `HOME` set to the
//! per-mission job directory (`crate::remote_node::raw_command`), so a CLI
//! that authenticates through a cached login under `~/.<cli>` sees no
//! credential at all. Two things must never happen to fix that:
//!
//! - the core must not ship the credential inside the job payload (it would
//!   be persisted in the node's job database and echoed into logs), and
//! - the core must not be able to point the node at an arbitrary file on the
//!   node's filesystem.
//!
//! Instead the operator configures one trusted directory per profile on the
//! node, and a payload only *names* the profile it needs. The node resolves
//! the name to its own configured path and exports the CLI's documented home
//! override; the payload's own env can never redirect it.
//!
//! Profiles:
//! - [`PROFILE_GROK`]: `SANDBOXED_NODE_GROK_HOME` names a directory owned by
//!   the node service account that holds the Grok CLI state. The operator
//!   creates it with `GROK_HOME=<dir> grok login --device-auth` (as the
//!   service account) which writes `<dir>/auth.json`; an optional
//!   `config.toml` there may configure an external auth provider instead.
//!   Jobs receive `GROK_HOME=<dir>` (the CLI's documented override for
//!   `~/.grok`). Session state also lives there, keyed by the job cwd, which
//!   is what makes `--resume <session id>` work across jobs of one mission.

use std::path::{Path, PathBuf};

/// Profile name for the Grok Build CLI cached login.
pub const PROFILE_GROK: &str = "grok";
/// Node env var naming the trusted Grok home directory.
pub const GROK_HOME_ENV: &str = "SANDBOXED_NODE_GROK_HOME";
/// Env var the Grok CLI reads to relocate `~/.grok`.
pub const GROK_HOME_CHILD_ENV: &str = "GROK_HOME";
/// Credential file the Grok CLI keeps inside its home.
pub const GROK_AUTH_FILE: &str = "auth.json";

/// Operator-configured managed-auth profiles of this node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedAuth {
    grok_home: Option<PathBuf>,
}

impl ManagedAuth {
    /// Read the node configuration from the environment. A relative or empty
    /// `SANDBOXED_NODE_GROK_HOME` is ignored with a warning: a relative path
    /// would resolve against each job's cwd, i.e. against mission-controlled
    /// content.
    pub fn from_env() -> Self {
        Self::from_values(std::env::var(GROK_HOME_ENV).ok())
    }

    pub fn from_values(grok_home: Option<String>) -> Self {
        let grok_home = grok_home
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| {
                let path = PathBuf::from(raw);
                if path.is_absolute() {
                    Some(path)
                } else {
                    tracing::warn!(
                        path = %path.display(),
                        "{GROK_HOME_ENV} must be an absolute path; managed grok auth is disabled"
                    );
                    None
                }
            });
        Self { grok_home }
    }

    /// Test/explicit constructor.
    pub fn with_grok_home(path: impl Into<PathBuf>) -> Self {
        Self {
            grok_home: Some(path.into()),
        }
    }

    /// Profiles that are configured *and* usable right now (the credential
    /// file exists and is readable by this process). This is what the
    /// heartbeat advertises, so a node whose operator configured the path
    /// but never logged in is not selected for a launch that would hang on
    /// an interactive sign-in.
    pub fn advertised(&self) -> Vec<String> {
        let mut ready = Vec::new();
        if let Some(home) = &self.grok_home {
            if grok_auth_readable(home) {
                ready.push(PROFILE_GROK.to_string());
            }
        }
        ready
    }

    /// Reject a payload that names a profile this node cannot honour. Called
    /// at submission so the core gets a clear HTTP rejection instead of a job
    /// that fails (or hangs on an interactive login) minutes later.
    pub fn validate_request(&self, profiles: &[String]) -> Result<(), String> {
        for profile in profiles {
            match profile.as_str() {
                PROFILE_GROK => {
                    let Some(home) = &self.grok_home else {
                        return Err(format!(
                            "managed auth profile '{PROFILE_GROK}' is not configured on this node (set {GROK_HOME_ENV} to the service account's Grok home and run `grok login --device-auth` there)"
                        ));
                    };
                    if !grok_auth_readable(home) {
                        return Err(format!(
                            "managed auth profile '{PROFILE_GROK}' is configured but {}/{GROK_AUTH_FILE} is missing, unreadable, or not private to the service account; run `GROK_HOME={} grok login --device-auth` as the node service account",
                            home.display(),
                            home.display()
                        ));
                    }
                }
                other => {
                    return Err(format!(
                        "managed auth profile '{other}' is unknown to this node (supported: {PROFILE_GROK})"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Environment to export for the requested profiles. Applied *after* the
    /// payload env so a payload cannot redirect a managed profile elsewhere.
    pub fn env_for(&self, profiles: &[String]) -> Result<Vec<(String, String)>, String> {
        self.validate_request(profiles)?;
        let mut env = Vec::new();
        for profile in profiles {
            if profile == PROFILE_GROK {
                if let Some(home) = &self.grok_home {
                    env.push((
                        GROK_HOME_CHILD_ENV.to_string(),
                        home.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
        Ok(env)
    }
}

fn grok_auth_readable(home: &Path) -> bool {
    let Ok(file) = std::fs::File::open(home.join(GROK_AUTH_FILE)) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grok_home_with_auth() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(GROK_AUTH_FILE), "{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join(GROK_AUTH_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn unconfigured_node_advertises_nothing_and_rejects_grok() {
        let auth = ManagedAuth::from_values(None);
        assert!(auth.advertised().is_empty());
        let err = auth.validate_request(&["grok".to_string()]).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
        assert!(err.contains(GROK_HOME_ENV), "{err}");
        assert!(auth.validate_request(&[]).is_ok());
    }

    #[test]
    fn relative_grok_home_is_ignored() {
        let auth = ManagedAuth::from_values(Some("relative/grok".into()));
        assert_eq!(auth, ManagedAuth::default());
        assert_eq!(
            ManagedAuth::from_values(Some("   ".into())),
            ManagedAuth::default()
        );
    }

    #[test]
    fn configured_home_without_login_is_not_advertised() {
        let dir = tempfile::tempdir().unwrap();
        let auth = ManagedAuth::with_grok_home(dir.path());
        assert!(auth.advertised().is_empty());
        let err = auth.validate_request(&["grok".to_string()]).unwrap_err();
        assert!(err.contains("missing, unreadable"), "{err}");
    }

    #[test]
    fn logged_in_home_is_advertised_and_injected_as_grok_home_only() {
        let dir = grok_home_with_auth();
        let auth = ManagedAuth::with_grok_home(dir.path());
        assert_eq!(auth.advertised(), vec!["grok".to_string()]);
        let env = auth.env_for(&["grok".to_string()]).unwrap();
        assert_eq!(
            env,
            vec![(
                "GROK_HOME".to_string(),
                dir.path().to_string_lossy().into_owned()
            )]
        );
        // Only the home override is exported: the credential bytes never
        // travel through the job environment either.
        assert!(env.iter().all(|(_, value)| !value.contains('{')));
        assert!(auth.env_for(&[]).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn permissive_auth_file_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = grok_home_with_auth();
        std::fs::set_permissions(
            dir.path().join(GROK_AUTH_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let auth = ManagedAuth::with_grok_home(dir.path());
        assert!(auth.advertised().is_empty());
        assert!(auth.env_for(&["grok".into()]).is_err());
    }

    #[test]
    fn unknown_profile_is_rejected_by_name() {
        let dir = grok_home_with_auth();
        let auth = ManagedAuth::with_grok_home(dir.path());
        let err = auth
            .env_for(&["grok".to_string(), "openai".to_string()])
            .unwrap_err();
        assert!(err.contains("'openai' is unknown"), "{err}");
    }
}
