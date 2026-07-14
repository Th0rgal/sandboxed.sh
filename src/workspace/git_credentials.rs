//! GitHub git-credential injection for workspaces.
//!
//! Lets agents running inside a workspace `git commit` / `git push` to GitHub
//! repos without any per-mission setup. When a GitHub token is configured in
//! the backend process environment, standard git credential files are
//! materialized into the workspace's home directory at mission-prep time:
//!
//! - `~/.git-credentials` — the token, in git's `store` helper format.
//! - `~/.gitconfig` — a managed block wiring the `store` helper for github.com
//!   plus the commit identity (`user.name` / `user.email`).
//! - `~/.config/gh/hosts.yml` — authenticates the GitHub CLI (`gh`), which
//!   ignores `~/.git-credentials` and reads its own config. Only written on the
//!   dashboard-connected path (where we know the account login); the env-var
//!   fallback leaves `gh` untouched.
//!
//! This mirrors how Codex/Claude credentials are written per workspace
//! (`write_codex_credentials_for_workspace`) and is deliberately
//! backend-agnostic so it works for every harness.
//!
//! The credential is resolved from two sources, in priority order:
//! 1. The GitHub account connected via the dashboard ("Connect GitHub"),
//!    persisted in `.sandboxed-sh/github_connection.json`. The commit identity
//!    comes from that account, so no extra configuration is needed. This is the
//!    normal path — see [`crate::api::github_integration`].
//! 2. Backend environment variables (operator fallback, same source as
//!    `JWT_SECRET`/`PORT`):
//!    - `GITHUB_TOKEN` / `GH_TOKEN` — the credential (a PAT or App installation
//!      token).
//!    - `GIT_AUTHOR_NAME` / `GIT_USER_NAME` — commit author name.
//!    - `GIT_AUTHOR_EMAIL` / `GIT_USER_EMAIL` — commit author email.
//!
//! When neither yields a token, nothing is written and missions are unchanged
//! (the feature is opt-in).
//!
//! Security note: the token lands inside the workspace, which runs agent code,
//! so scope it narrowly — a fine-grained PAT limited to the target repos, or a
//! short-lived GitHub App installation token. The credentials file is written
//! `0600`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::github_connection::{GithubConnection, GithubConnectionStore};
use crate::util::{env_var_nonempty, home_dir, write_file_0600, GITHUB_CONNECTION_PATH};
use crate::workspace::{container_fallback_from_env, Workspace, WorkspaceType};

const MANAGED_BEGIN: &str = "# >>> sandboxed.sh git credentials >>>";
const MANAGED_END: &str = "# <<< sandboxed.sh git credentials <<<";
/// Host the `store` credential helper is scoped to, so we never hijack auth
/// for other git hosts the workspace may talk to.
const GITHUB_CREDENTIAL_HOST: &str = "https://github.com";

/// Subdirectory under a **host** workspace's root used as the git/gh credential
/// home. Containers already get an isolated home (`<root>/root`), but host
/// workspaces previously shared the operator's real `$HOME` — so a connected
/// account's `~/.gitconfig` / `~/.git-credentials` / `~/.config/gh/hosts.yml`
/// would land on (and overwrite) the operator's own files. Writing them under
/// this sandboxed subdir keeps the operator's dotfiles untouched; git/gh still
/// find the creds because [`GitCredentialConfig::apply_to_env`] points them here
/// explicitly (`GIT_CONFIG_*`, `GH_CONFIG_DIR`, `SANDBOXED_SH_GIT_CREDENTIALS_FILE`).
const HOST_GIT_HOME_SUBDIR: &str = ".sandboxed-sh/git-home";

/// Git/gh credential home for a workspace. Mirrors the harness's `$HOME` for
/// containers (so creds are visible to the agent), but isolates **host**
/// workspaces into [`HOST_GIT_HOME_SUBDIR`] instead of the operator's real
/// `$HOME`. Container fallback (Docker/macOS) keeps using the host `$HOME`,
/// matching where that harness actually runs.
fn resolve_git_home_root(
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
) -> PathBuf {
    if workspace_type == WorkspaceType::Host {
        workspace_root.join(HOST_GIT_HOME_SUBDIR)
    } else if workspace_type == WorkspaceType::Container
        && !container_fallback_from_env(workspace_env)
    {
        workspace_root.join("root")
    } else {
        PathBuf::from(home_dir())
    }
}

/// Resolved git credential configuration sourced from the backend environment.
#[derive(Debug, Clone)]
pub struct GitCredentialConfig {
    token: String,
    user_name: Option<String>,
    user_email: Option<String>,
    /// GitHub login (username), known on the dashboard-connected path. Used to
    /// author `gh`'s `hosts.yml`; `None` on the env-var fallback path.
    login: Option<String>,
}

impl GitCredentialConfig {
    /// Build from the backend process environment. Returns `None` when no
    /// GitHub token is configured (the feature is opt-in; missions unchanged).
    pub fn from_env() -> Option<Self> {
        let token = env_var_nonempty("GITHUB_TOKEN").or_else(|| env_var_nonempty("GH_TOKEN"))?;
        let user_name =
            env_var_nonempty("GIT_AUTHOR_NAME").or_else(|| env_var_nonempty("GIT_USER_NAME"));
        let user_email =
            env_var_nonempty("GIT_AUTHOR_EMAIL").or_else(|| env_var_nonempty("GIT_USER_EMAIL"));
        Some(Self {
            token,
            user_name,
            user_email,
            login: None,
        })
    }

    /// Build from workspace-scoped environment values. This is the fallback
    /// used when no dashboard connection or service-level token exists. It is
    /// intentionally resolved at process-spawn time so a secret refresher can
    /// rotate `GH_TOKEN` without requiring a backend restart.
    pub fn from_workspace_env(workspace_env: &HashMap<String, String>) -> Option<Self> {
        let value = |key: &str| {
            workspace_env
                .get(key)
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && !value.starts_with("[DECRYPTION_FAILED]"))
                .map(str::to_string)
        };
        let token = value("GITHUB_TOKEN").or_else(|| value("GH_TOKEN"))?;
        let user_name = value("GIT_AUTHOR_NAME").or_else(|| value("GIT_USER_NAME"));
        let user_email = value("GIT_AUTHOR_EMAIL").or_else(|| value("GIT_USER_EMAIL"));
        Some(Self {
            token,
            user_name,
            user_email,
            login: None,
        })
    }

    /// Build from a dashboard-connected GitHub account. The commit identity is
    /// always derived from the account (profile name/email, falling back to the
    /// login and GitHub `noreply` form), so [`Self::has_identity`] is always
    /// true on this path.
    pub fn from_connection(conn: &GithubConnection) -> Self {
        Self {
            token: conn.access_token.clone(),
            user_name: Some(conn.commit_name()),
            user_email: Some(conn.commit_email()),
            login: Some(conn.login.clone()),
        }
    }

    /// Resolve the credential for a workspace: prefer the dashboard-connected
    /// GitHub account, then fall back to the operator environment variables.
    ///
    /// `app_working_dir` is the backend `Config::working_dir`, where the
    /// dashboard stores the connected account. Extra fallbacks cover legacy/dev
    /// layouts and direct `WorkspaceExec` calls that only have a workspace.
    pub fn resolve(workspace_root: &Path, app_working_dir: Option<&Path>) -> Option<Self> {
        Self::resolve_with_workspace_env(workspace_root, app_working_dir, &HashMap::new())
    }

    /// Resolve a fresh credential for a workspace. Dashboard credentials have
    /// precedence, followed by the service environment and finally the
    /// workspace-scoped environment. Callers that spawn subprocesses should
    /// use this path on every spawn rather than retaining a possibly stale
    /// token in memory.
    pub fn resolve_with_workspace_env(
        workspace_root: &Path,
        app_working_dir: Option<&Path>,
        workspace_env: &HashMap<String, String>,
    ) -> Option<Self> {
        let candidates = github_connection_candidates(workspace_root, app_working_dir);
        for path in &candidates {
            if let Some(conn) = GithubConnectionStore::read_from_path(path) {
                return Some(Self::from_connection(&conn));
            }
        }
        Self::from_env().or_else(|| Self::from_workspace_env(workspace_env))
    }

    /// Whether a commit identity is configured. Without it `git commit` fails
    /// with "Author identity unknown", so callers should surface a warning.
    pub fn has_identity(&self) -> bool {
        self.user_name.is_some() && self.user_email.is_some()
    }

    /// The resolved access token, used only for writing credential files.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Commit author/committer name, when known (for the `GIT_AUTHOR_NAME` /
    /// `GIT_COMMITTER_NAME` env vars, which override config home-independently).
    pub fn user_name(&self) -> Option<&str> {
        self.user_name.as_deref()
    }

    /// Commit author/committer email, when known.
    pub fn user_email(&self) -> Option<&str> {
        self.user_email.as_deref()
    }

    /// Resolve credentials, write git/gh config files into the workspace home,
    /// and cache the result on `workspace` for [`Self::apply_to_env`]. Non-fatal:
    /// failures are logged and must not break mission prep.
    pub fn inject_for_mission(
        workspace: &mut Workspace,
        mission_id: Uuid,
        app_working_dir: Option<&Path>,
    ) {
        let Some(creds) =
            Self::resolve_with_workspace_env(&workspace.path, app_working_dir, &workspace.env_vars)
        else {
            workspace.resolved_git_credentials = None;
            return;
        };

        match creds.write_for_workspace(
            &workspace.path,
            workspace.workspace_type,
            &workspace.env_vars,
        ) {
            Ok(home) => {
                tracing::info!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    workspace_type = ?workspace.workspace_type,
                    home = %home.display(),
                    identity = creds.has_identity(),
                    "Wrote GitHub git credentials for workspace"
                );
                if !creds.has_identity() {
                    tracing::warn!(
                        mission = %mission_id,
                        workspace = %workspace.name,
                        "GITHUB_TOKEN is set but GIT_AUTHOR_NAME/GIT_AUTHOR_EMAIL are not; \
                         `git commit` will fail with 'Author identity unknown' until they are set"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to write GitHub git credentials for workspace"
                );
            }
        }

        workspace.resolved_git_credentials = Some(creds);
    }

    /// Export non-secret pointers into `merged` so `git`/`gh` work when a
    /// backend repoints `HOME`/`XDG_CONFIG_HOME` away from the files written at
    /// prep. Never overrides env vars already set by the caller.
    pub fn apply_to_env(
        &self,
        merged: &mut HashMap<String, String>,
        workspace_root: &Path,
        workspace_type: WorkspaceType,
        workspace_env: &HashMap<String, String>,
    ) {
        let process_home =
            resolve_workspace_home_for_process(workspace_root, workspace_type, workspace_env);
        let git_credentials_file = process_home.join(".git-credentials");
        merged
            .entry("SANDBOXED_SH_GIT_CREDENTIALS_FILE".to_string())
            .or_insert_with(|| git_credentials_file.to_string_lossy().to_string());
        if self.login.is_some() {
            merged
                .entry("GH_CONFIG_DIR".to_string())
                .or_insert_with(|| {
                    process_home
                        .join(".config/gh")
                        .to_string_lossy()
                        .to_string()
                });
        }
        if let Some(name) = self.user_name() {
            merged
                .entry("GIT_AUTHOR_NAME".to_string())
                .or_insert_with(|| name.to_string());
            merged
                .entry("GIT_COMMITTER_NAME".to_string())
                .or_insert_with(|| name.to_string());
        }
        if let Some(email) = self.user_email() {
            merged
                .entry("GIT_AUTHOR_EMAIL".to_string())
                .or_insert_with(|| email.to_string());
            merged
                .entry("GIT_COMMITTER_EMAIL".to_string())
                .or_insert_with(|| email.to_string());
        }
        // HOME-independent push credentials: point git at the credential-store
        // file written during mission prep. The env carries only a file path,
        // never the OAuth token, so nsenter shell argv cannot expose it.
        append_git_config_env(
            merged,
            "credential.https://github.com.helper",
            r#"!f() { git credential-store --file "$SANDBOXED_SH_GIT_CREDENTIALS_FILE" "$@"; }; f"#,
        );
    }

    /// Materialize git credential files into the workspace's git-credential home
    /// ([`resolve_git_home_root`]):
    ///
    /// - Container under systemd-nspawn → `<workspace_root>/root` (the
    ///   container's `/root`).
    /// - Container in fallback mode (no nspawn, e.g. Docker/macOS — flagged via
    ///   `SANDBOXED_SH_CONTAINER_FALLBACK` in the workspace env) → the host
    ///   `$HOME`, because the harness then runs directly on the host.
    /// - Host → a sandboxed subdir ([`HOST_GIT_HOME_SUBDIR`]), NOT the
    ///   operator's real `$HOME`, so we never clobber their dotfiles. git/gh are
    ///   pointed here via env in [`Self::apply_to_env`].
    ///
    /// Returns the home directory written into (for logging).
    pub fn write_for_workspace(
        &self,
        workspace_root: &Path,
        workspace_type: WorkspaceType,
        workspace_env: &HashMap<String, String>,
    ) -> std::io::Result<PathBuf> {
        let home = resolve_git_home_root(workspace_root, workspace_type, workspace_env);
        std::fs::create_dir_all(&home)?;

        self.write_git_credentials(&home)?;
        self.write_gitconfig(&home)?;
        self.write_gh_hosts(&home)?;
        Ok(home)
    }

    /// Remove this connected account's materialized credentials from a
    /// workspace home. Used on disconnect so stale files do not keep
    /// authenticating future missions.
    pub fn scrub_for_workspace(
        &self,
        workspace_root: &Path,
        workspace_type: WorkspaceType,
        workspace_env: &HashMap<String, String>,
    ) -> std::io::Result<PathBuf> {
        let home = resolve_git_home_root(workspace_root, workspace_type, workspace_env);
        scrub_home(&home, &self.token)?;
        Ok(home)
    }

    /// Write/merge the token into `~/.git-credentials` (`0600`), preserving any
    /// non-github.com entries already present.
    fn write_git_credentials(&self, home: &Path) -> std::io::Result<()> {
        let path = home.join(".git-credentials");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let merged = merge_git_credentials(&existing, &self.token);
        write_file_0600(&path, merged.as_bytes())
    }

    /// Write/merge a managed block into `~/.gitconfig` carrying the credential
    /// helper and commit identity, leaving config outside the block untouched.
    fn write_gitconfig(&self, home: &Path) -> std::io::Result<()> {
        let path = home.join(".gitconfig");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let merged = replace_managed_block(&existing, &self.managed_gitconfig_block());
        std::fs::write(&path, merged)
    }

    /// Write `~/.config/gh/hosts.yml` so the GitHub CLI (`gh`) is authenticated
    /// in the workspace — `gh` ignores `~/.git-credentials` and reads its own
    /// config. Written full (not merged); the workspace home is per-mission.
    ///
    /// Requires the account login, so it only runs on the dashboard-connected
    /// path. On the env-var fallback the login is unknown, so `gh` is left
    /// untouched (git still works via `.git-credentials`).
    ///
    /// Merges the `github.com` entry into any existing `hosts.yml` rather than
    /// overwriting the file, so a pre-existing entry for another host (e.g. a
    /// GitHub Enterprise instance, or — on a host workspace — the operator's own
    /// `gh auth`) is preserved.
    fn write_gh_hosts(&self, home: &Path) -> std::io::Result<()> {
        let Some(login) = self
            .login
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let gh_dir = home.join(".config").join("gh");
        std::fs::create_dir_all(&gh_dir)?;
        let path = gh_dir.join("hosts.yml");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let merged = merge_gh_hosts(&existing, login, &self.token);
        write_file_0600(&path, merged.as_bytes())
    }

    /// The managed `.gitconfig` block (between the sentinel markers).
    fn managed_gitconfig_block(&self) -> String {
        let mut s = String::new();
        s.push_str(MANAGED_BEGIN);
        s.push('\n');
        s.push_str(&format!("[credential \"{}\"]\n", GITHUB_CREDENTIAL_HOST));
        s.push_str("\thelper = store\n");
        if let (Some(name), Some(email)) = (&self.user_name, &self.user_email) {
            s.push_str("[user]\n");
            s.push_str(&format!("\tname = {}\n", name));
            s.push_str(&format!("\temail = {}\n", email));
        }
        s.push_str(MANAGED_END);
        s
    }
}

/// Merge the `github.com` entry into existing `gh` `hosts.yml` content, dropping
/// any prior `github.com` entry and preserving every other host. Both the
/// host-level `oauth_token`/`user` (older gh) and the `users` map (newer gh) are
/// written so `gh auth status` works across versions. Falls back to a
/// github.com-only document if the existing file can't be parsed as a mapping.
fn merge_gh_hosts(existing: &str, login: &str, token: &str) -> String {
    use serde_yaml::{Mapping, Value};

    let github_entry = {
        let mut entry = Mapping::new();
        entry.insert(
            Value::String("git_protocol".into()),
            Value::String("https".into()),
        );
        entry.insert(Value::String("user".into()), Value::String(login.into()));
        entry.insert(
            Value::String("oauth_token".into()),
            Value::String(token.into()),
        );
        let mut user_entry = Mapping::new();
        user_entry.insert(
            Value::String("oauth_token".into()),
            Value::String(token.into()),
        );
        let mut users = Mapping::new();
        users.insert(Value::String(login.into()), Value::Mapping(user_entry));
        entry.insert(Value::String("users".into()), Value::Mapping(users));
        Value::Mapping(entry)
    };

    let mut doc: Mapping = serde_yaml::from_str::<Value>(existing)
        .ok()
        .and_then(|v| match v {
            Value::Mapping(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    doc.insert(Value::String("github.com".into()), github_entry);

    serde_yaml::to_string(&Value::Mapping(doc)).unwrap_or_else(|_| {
        format!(
            "github.com:\n    git_protocol: https\n    user: {login}\n    oauth_token: {token}\n    users:\n        {login}:\n            oauth_token: {token}\n"
        )
    })
}

/// Merge a github.com token line into existing `.git-credentials` content,
/// dropping any prior github.com entry and keeping everything else.
fn merge_git_credentials(existing: &str, token: &str) -> String {
    let line = format!("https://x-access-token:{}@github.com", token);
    let mut kept: Vec<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("@github.com"))
        .collect();
    kept.push(&line);
    let mut out = kept.join("\n");
    out.push('\n');
    out
}

/// Replace the managed block in `existing`, or append it if absent. Content
/// before/after the markers is preserved.
fn replace_managed_block(existing: &str, managed: &str) -> String {
    let managed = managed.trim_end_matches('\n');
    if let (Some(start), Some(end)) = (existing.find(MANAGED_BEGIN), existing.find(MANAGED_END)) {
        if start < end {
            let end = end + MANAGED_END.len();
            let before = existing[..start].trim_end_matches('\n');
            let after = existing[end..].trim_start_matches('\n');
            let mut parts: Vec<&str> = Vec::new();
            if !before.is_empty() {
                parts.push(before);
            }
            parts.push(managed);
            if !after.is_empty() {
                parts.push(after);
            }
            return format!("{}\n", parts.join("\n"));
        }
    }
    let base = existing.trim_end_matches('\n');
    if base.is_empty() {
        format!("{}\n", managed)
    } else {
        format!("{}\n{}\n", base, managed)
    }
}

fn github_connection_candidates(
    workspace_root: &Path,
    app_working_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    push_unique_candidate(&mut candidates, workspace_root.join(GITHUB_CONNECTION_PATH));

    if let Some(root) = app_working_dir {
        push_unique_candidate(&mut candidates, root.join(GITHUB_CONNECTION_PATH));
    }

    // Container workspaces normally live at
    // <working_dir>/.sandboxed-sh/containers/<name>. Infer <working_dir> so
    // direct WorkspaceExec paths still find the central connection file.
    for ancestor in workspace_root.ancestors() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some(".sandboxed-sh") {
            if let Some(root) = ancestor.parent() {
                push_unique_candidate(&mut candidates, root.join(GITHUB_CONNECTION_PATH));
            }
        }
    }

    if let Ok(root) = std::env::var("WORKING_DIR") {
        let root = root.trim();
        if !root.is_empty() {
            push_unique_candidate(
                &mut candidates,
                PathBuf::from(root).join(GITHUB_CONNECTION_PATH),
            );
        }
    }
    if let Ok(root) = std::env::current_dir() {
        push_unique_candidate(&mut candidates, root.join(GITHUB_CONNECTION_PATH));
    }
    push_unique_candidate(
        &mut candidates,
        PathBuf::from(home_dir()).join(GITHUB_CONNECTION_PATH),
    );
    candidates
}

fn push_unique_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.iter().any(|p| p == &path) {
        candidates.push(path);
    }
}

/// The git-credential home as the *agent process* sees it. Matches
/// [`resolve_git_home_root`] except for nspawn containers, where the host path
/// `<workspace_root>/root` is the container-internal `/root`.
fn resolve_workspace_home_for_process(
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
) -> PathBuf {
    if workspace_type == WorkspaceType::Host {
        workspace_root.join(HOST_GIT_HOME_SUBDIR)
    } else if workspace_type == WorkspaceType::Container
        && !container_fallback_from_env(workspace_env)
    {
        PathBuf::from("/root")
    } else {
        PathBuf::from(home_dir())
    }
}

fn append_git_config_env(merged: &mut HashMap<String, String>, key: &str, value: &str) {
    let index = merged
        .get("GIT_CONFIG_COUNT")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    merged.insert("GIT_CONFIG_COUNT".to_string(), (index + 1).to_string());
    merged.insert(format!("GIT_CONFIG_KEY_{index}"), key.to_string());
    merged.insert(format!("GIT_CONFIG_VALUE_{index}"), value.to_string());
}

fn scrub_home(home: &Path, token: &str) -> std::io::Result<()> {
    scrub_git_credentials(&home.join(".git-credentials"), token)?;
    scrub_gitconfig(&home.join(".gitconfig"))?;
    scrub_gh_hosts(&home.join(".config").join("gh").join("hosts.yml"), token)?;
    Ok(())
}

fn scrub_git_credentials(path: &Path, token: &str) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| !(line.contains("@github.com") && line.contains(token)))
        .collect();
    if kept.is_empty() {
        remove_file_if_exists(path)
    } else {
        let mut out = kept.join("\n");
        out.push('\n');
        write_file_0600(path, out.as_bytes())
    }
}

fn scrub_gitconfig(path: &Path) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let cleaned = remove_managed_block(&existing);
    if cleaned.trim().is_empty() {
        remove_file_if_exists(path)
    } else {
        std::fs::write(path, cleaned)
    }
}

fn remove_managed_block(existing: &str) -> String {
    if let (Some(start), Some(end)) = (existing.find(MANAGED_BEGIN), existing.find(MANAGED_END)) {
        if start < end {
            let end = end + MANAGED_END.len();
            let before = existing[..start].trim_end_matches('\n');
            let after = existing[end..].trim_start_matches('\n');
            let mut parts: Vec<&str> = Vec::new();
            if !before.is_empty() {
                parts.push(before);
            }
            if !after.is_empty() {
                parts.push(after);
            }
            if parts.is_empty() {
                String::new()
            } else {
                format!("{}\n", parts.join("\n"))
            }
        } else {
            existing.to_string()
        }
    } else {
        existing.to_string()
    }
}

fn scrub_gh_hosts(path: &Path, token: &str) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !existing.contains(token) {
        return Ok(());
    }

    let parsed = serde_yaml::from_str::<serde_yaml::Value>(&existing);
    let Ok(mut value) = parsed else {
        return remove_file_if_exists(path);
    };
    let Some(mapping) = value.as_mapping_mut() else {
        return remove_file_if_exists(path);
    };
    let github_key = serde_yaml::Value::String("github.com".to_string());
    if mapping
        .get(&github_key)
        .is_some_and(|github| yaml_value_contains(github, token))
    {
        mapping.remove(&github_key);
    }
    if mapping.is_empty() {
        remove_file_if_exists(path)
    } else {
        let out = serde_yaml::to_string(&value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_file_0600(path, out.as_bytes())
    }
}

fn yaml_value_contains(value: &serde_yaml::Value, needle: &str) -> bool {
    match value {
        serde_yaml::Value::String(s) => s == needle,
        serde_yaml::Value::Sequence(seq) => seq.iter().any(|v| yaml_value_contains(v, needle)),
        serde_yaml::Value::Mapping(map) => map
            .iter()
            .any(|(k, v)| yaml_value_contains(k, needle) || yaml_value_contains(v, needle)),
        _ => false,
    }
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::resolve_workspace_home_root;

    #[test]
    fn merge_credentials_into_empty() {
        let out = merge_git_credentials("", "tok123");
        assert_eq!(out, "https://x-access-token:tok123@github.com\n");
    }

    #[test]
    fn merge_credentials_replaces_prior_github_entry() {
        let existing = "https://x-access-token:OLD@github.com\nhttps://user:pw@gitlab.com\n";
        let out = merge_git_credentials(existing, "NEW");
        // gitlab entry preserved, github entry replaced, single trailing newline.
        assert_eq!(
            out,
            "https://user:pw@gitlab.com\nhttps://x-access-token:NEW@github.com\n"
        );
    }

    #[test]
    fn managed_block_appended_then_replaced() {
        let cfg = GitCredentialConfig {
            token: "t".into(),
            user_name: Some("Ada".into()),
            user_email: Some("ada@example.com".into()),
            login: Some("ada".into()),
        };
        let user_cfg = "[core]\n\teditor = vim\n";
        let first = replace_managed_block(user_cfg, &cfg.managed_gitconfig_block());
        assert!(first.contains("[core]"));
        assert!(first.contains("ada@example.com"));
        assert!(first.matches(MANAGED_BEGIN).count() == 1);

        // Re-running with a new identity must not duplicate the block.
        let cfg2 = GitCredentialConfig {
            token: "t".into(),
            user_name: Some("Grace".into()),
            user_email: Some("grace@example.com".into()),
            login: Some("grace".into()),
        };
        let second = replace_managed_block(&first, &cfg2.managed_gitconfig_block());
        assert!(second.contains("[core]"));
        assert!(second.contains("grace@example.com"));
        assert!(!second.contains("ada@example.com"));
        assert_eq!(second.matches(MANAGED_BEGIN).count(), 1);
    }

    #[test]
    fn connection_candidates_include_working_dir_and_container_parent() {
        let workspace_root = Path::new("/srv/app/.sandboxed-sh/containers/ws1");
        let explicit = Path::new("/custom/root");
        let candidates = github_connection_candidates(workspace_root, Some(explicit));
        assert!(candidates.contains(&workspace_root.join(GITHUB_CONNECTION_PATH)));
        assert!(candidates.contains(&explicit.join(GITHUB_CONNECTION_PATH)));
        assert!(candidates.contains(&PathBuf::from("/srv/app").join(GITHUB_CONNECTION_PATH)));
    }

    #[test]
    fn apply_to_env_uses_file_paths_not_token() {
        let cfg = GitCredentialConfig {
            token: "gho_secret".into(),
            user_name: Some("Ada".into()),
            user_email: Some("ada@example.com".into()),
            login: Some("ada".into()),
        };
        let mut merged = HashMap::new();
        cfg.apply_to_env(
            &mut merged,
            Path::new("/srv/app/.sandboxed-sh/containers/ws1"),
            WorkspaceType::Container,
            &HashMap::new(),
        );
        assert_eq!(merged.get("GH_CONFIG_DIR").unwrap(), "/root/.config/gh");
        assert_eq!(
            merged.get("SANDBOXED_SH_GIT_CREDENTIALS_FILE").unwrap(),
            "/root/.git-credentials"
        );
        assert!(!merged.values().any(|value| value.contains("gho_secret")));
    }

    #[test]
    fn workspace_token_is_a_supported_last_resort() {
        let mut env = HashMap::new();
        env.insert("GH_TOKEN".into(), "workspace-token".into());
        env.insert("GIT_AUTHOR_NAME".into(), "Ada".into());
        env.insert("GIT_AUTHOR_EMAIL".into(), "ada@example.com".into());
        let cfg = GitCredentialConfig::from_workspace_env(&env).unwrap();
        assert_eq!(cfg.token(), "workspace-token");
        assert_eq!(cfg.user_name(), Some("Ada"));
        assert_eq!(cfg.user_email(), Some("ada@example.com"));
    }

    #[test]
    fn decryption_failure_marker_is_never_used_as_a_token() {
        let mut env = HashMap::new();
        env.insert(
            "GH_TOKEN".into(),
            "[DECRYPTION_FAILED]encrypted-payload".into(),
        );
        assert!(GitCredentialConfig::from_workspace_env(&env).is_none());
    }

    #[test]
    fn home_resolves_to_container_root_under_nspawn() {
        let root = Path::new("/srv/ws/mission-1");
        let env = HashMap::new();
        let home = resolve_workspace_home_root(root, WorkspaceType::Container, &env);
        assert_eq!(home, root.join("root"));
    }

    #[test]
    fn home_resolves_to_host_home_in_container_fallback() {
        // Docker/macOS: container can't use nspawn, so the harness runs on the
        // host and git must read creds from the real $HOME — not <root>/root.
        let root = Path::new("/srv/ws/mission-1");
        let mut env = HashMap::new();
        env.insert("SANDBOXED_SH_CONTAINER_FALLBACK".into(), "1".into());
        let home = resolve_workspace_home_root(root, WorkspaceType::Container, &env);
        assert_eq!(home, PathBuf::from(crate::util::home_dir()));
    }

    #[test]
    fn home_resolves_to_host_home_for_host_workspace() {
        let root = Path::new("/srv/ws/mission-1");
        let env = HashMap::new();
        let home = resolve_workspace_home_root(root, WorkspaceType::Host, &env);
        assert_eq!(home, PathBuf::from(crate::util::home_dir()));
    }

    #[test]
    fn git_home_isolates_host_workspace_from_operator_home() {
        // Host workspace: git creds go to a sandboxed subdir, NOT the operator's
        // real $HOME, so we never clobber their ~/.gitconfig / ~/.config/gh.
        let root = Path::new("/srv/ws/mission-1");
        let env = HashMap::new();
        let write = resolve_git_home_root(root, WorkspaceType::Host, &env);
        let process = resolve_workspace_home_for_process(root, WorkspaceType::Host, &env);
        assert_eq!(write, root.join(HOST_GIT_HOME_SUBDIR));
        // Write and process homes must agree for a host workspace (no namespace).
        assert_eq!(write, process);
        assert_ne!(write, PathBuf::from(crate::util::home_dir()));
        // Containers are unchanged (already isolated at <root>/root).
        assert_eq!(
            resolve_git_home_root(root, WorkspaceType::Container, &env),
            root.join("root")
        );
    }

    #[test]
    fn gh_hosts_merge_preserves_other_hosts_and_replaces_github() {
        let existing = "\
git.example.com:
    git_protocol: ssh
    user: someone
    oauth_token: keep-me
github.com:
    git_protocol: https
    user: old-user
    oauth_token: stale-token
";
        let merged = merge_gh_hosts(existing, "new-user", "fresh-token");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let map = parsed.as_mapping().unwrap();
        // Other host untouched.
        let other = map.get("git.example.com").unwrap();
        assert_eq!(other.get("oauth_token").unwrap().as_str(), Some("keep-me"));
        // github.com replaced with the new identity/token; no stale data left.
        let gh = map.get("github.com").unwrap();
        assert_eq!(gh.get("user").unwrap().as_str(), Some("new-user"));
        assert_eq!(gh.get("oauth_token").unwrap().as_str(), Some("fresh-token"));
        assert!(!merged.contains("stale-token"));
        assert!(!merged.contains("old-user"));
    }

    #[test]
    fn gh_hosts_merge_writes_github_into_empty_file() {
        let merged = merge_gh_hosts("", "ada", "tok");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let gh = parsed.as_mapping().unwrap().get("github.com").unwrap();
        assert_eq!(gh.get("user").unwrap().as_str(), Some("ada"));
        assert_eq!(gh.get("oauth_token").unwrap().as_str(), Some("tok"));
    }

    #[test]
    fn gh_hosts_written_with_login_and_skipped_without() {
        let base = std::env::temp_dir().join(format!("sbx-ghhosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // Dashboard-connected path (login known): hosts.yml is written.
        let cfg = GitCredentialConfig {
            token: "gho_abc".into(),
            user_name: Some("Ada".into()),
            user_email: Some("ada@example.com".into()),
            login: Some("ada-login".into()),
        };
        let home = base.join("with");
        std::fs::create_dir_all(&home).unwrap();
        cfg.write_gh_hosts(&home).unwrap();
        let hosts = std::fs::read_to_string(home.join(".config/gh/hosts.yml")).unwrap();
        assert!(hosts.contains("github.com:"));
        assert!(hosts.contains("user: ada-login"));
        assert!(hosts.contains("oauth_token: gho_abc"));

        // Env-var fallback (login unknown): nothing written, gh left untouched.
        let cfg_no = GitCredentialConfig {
            token: "gho_abc".into(),
            user_name: None,
            user_email: None,
            login: None,
        };
        let home2 = base.join("without");
        std::fs::create_dir_all(&home2).unwrap();
        cfg_no.write_gh_hosts(&home2).unwrap();
        assert!(!home2.join(".config/gh/hosts.yml").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scrub_home_removes_connected_token_and_managed_config() {
        let base = std::env::temp_dir().join(format!("sbx-ghscrub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".config/gh")).unwrap();

        let cfg = GitCredentialConfig {
            token: "gho_scrub".into(),
            user_name: Some("Ada".into()),
            user_email: Some("ada@example.com".into()),
            login: Some("ada-login".into()),
        };
        cfg.write_git_credentials(&base).unwrap();
        cfg.write_gitconfig(&base).unwrap();
        cfg.write_gh_hosts(&base).unwrap();
        std::fs::write(
            base.join(".git-credentials"),
            "https://user:pw@gitlab.com\nhttps://x-access-token:gho_scrub@github.com\n",
        )
        .unwrap();

        scrub_home(&base, "gho_scrub").unwrap();

        let git_credentials = std::fs::read_to_string(base.join(".git-credentials")).unwrap();
        assert_eq!(git_credentials, "https://user:pw@gitlab.com\n");
        let gitconfig = std::fs::read_to_string(base.join(".gitconfig")).unwrap_or_default();
        assert!(!gitconfig.contains(MANAGED_BEGIN));
        assert!(!base.join(".config/gh/hosts.yml").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn managed_block_omits_user_when_identity_missing() {
        let cfg = GitCredentialConfig {
            token: "t".into(),
            user_name: None,
            user_email: None,
            login: None,
        };
        let block = cfg.managed_gitconfig_block();
        assert!(block.contains("helper = store"));
        assert!(!block.contains("[user]"));
        assert!(!cfg.has_identity());
    }
}
