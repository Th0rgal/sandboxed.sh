//! Declarative `lean_build` job execution for the `sandboxed-node` runner.
//!
//! A lean-build job carries a git source (credential-free repo identity,
//! pinned commit, and normally a complete commit-bound object pack), a
//! constrained argv (`lake build`/`lean`), and artifact patterns. The node
//! materializes a content-addressed checkout without network access when the
//! archive is present, restores trusted shared runtime
//! caches, runs the build, and records artifact digests. Lake dependency-cache
//! plumbing fails closed until the evaluated package layout can be attested.
//! No workspace sync with core, no shell interpretation of the payload.
//!
//! Layout under `SANDBOXED_NODE_WORK_DIR`:
//! - `checkouts/<sha256(repo)[..16]>/<commit>/` — immutable-ish checkouts
//! - `caches/elan/` (`ELAN_HOME`), `caches/xdg/` (`XDG_CACHE_HOME`),
//!   `caches/home/` (`HOME`) — shared toolchain caches
//! - `caches/lake/<cache_key>/packages/` — reserved dependency-only Lake cache
//!   slots; restore/sync is disabled while package layout is unverified.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::job_store::JobState;
use super::runner::{clamp_timeout, run_logged_command, CommandEnvironment};
use crate::remote_node::{ArtifactEntry, JobPayload, JobSource, SourceArchive, SourceBundle};

/// Default allowlist for lean-build env keys
/// (`SANDBOXED_NODE_ENV_ALLOWLIST` overrides, comma-separated).
pub const DEFAULT_ENV_ALLOWLIST: &str = "LEAN_NUM_THREADS,LAKE_JOBS";

/// Binaries a lean-build argv may start with (basename of argv[0]).
pub const ALLOWED_COMMANDS: [&str; 2] = ["lake", "lean"];

/// Default node GC threshold: keep at least this many GiB free on the
/// filesystem backing the work dir (`SANDBOXED_NODE_MIN_FREE_GB` overrides).
const DEFAULT_MIN_FREE_GB: u64 = 10;
const DEFAULT_MAX_SOURCE_BUNDLE_BYTES: u64 = 1 << 20;
const DEFAULT_MAX_COMPLETE_SOURCE_BUNDLE_BYTES: u64 = 16 << 20;
const MAX_SOURCE_BUNDLE_FILES: usize = 256;
const DEFAULT_MAX_SOURCE_ARCHIVE_BYTES: u64 = 32 << 20;
const DEFAULT_MAX_SOURCE_ARCHIVE_EXPANDED_BYTES: u64 = 2 << 30;
const MAX_SOURCE_ARCHIVE_OBJECTS: usize = 100_000;
const MAX_COMPLETE_SOURCE_BUNDLE_FILES: usize = 4096;

/// Interval between node-side cache GC passes.
const GC_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Per-step ceiling for git operations (fetch/submodules) inside a build.
const GIT_STEP_TIMEOUT_SECS: u64 = 1800;

/// Outcome of one lean-build job.
pub struct LeanBuildResult {
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    /// Resolved artifact digests; non-empty only after a successful build.
    pub artifacts: Vec<ArtifactEntry>,
}

// ---------------------------------------------------------------------------
// Validation (pure; unit-tested without network or git)
// ---------------------------------------------------------------------------

/// Parse a comma-separated env-key allowlist.
pub fn parse_env_allowlist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

/// Env-key allowlist from `SANDBOXED_NODE_ENV_ALLOWLIST` (with default).
pub fn env_allowlist_from_env() -> Vec<String> {
    let raw = std::env::var("SANDBOXED_NODE_ENV_ALLOWLIST")
        .ok()
        .filter(|raw| !raw.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ENV_ALLOWLIST.to_string());
    parse_env_allowlist(&raw)
}

/// Full 40-char lowercase hex commit SHA (branch names/short SHAs rejected).
pub fn commit_is_valid(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

/// Whether a slash-trimmed relative path is safe to use below a build root.
///
/// Copy of the check in `src/api/spark.rs` (`rel_path_is_safe`) — duplicated
/// on purpose so the node runtime does not depend on the core API layer.
/// Only `[A-Za-z0-9._-]` components, no `..`, no `-`-leading component
/// (argv-flag smuggling). Empty = build root.
pub fn rel_path_is_safe(rel_clean: &str) -> bool {
    rel_clean.is_empty()
        || rel_clean.split('/').all(|c| {
            !c.is_empty()
                && c != ".."
                && !c.starts_with('-')
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
}

fn source_bundle_max_bytes(bundle: &SourceBundle) -> u64 {
    std::env::var("SANDBOXED_NODE_MAX_SOURCE_BUNDLE_BYTES")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
        .unwrap_or(if bundle.complete {
            DEFAULT_MAX_COMPLETE_SOURCE_BUNDLE_BYTES
        } else {
            DEFAULT_MAX_SOURCE_BUNDLE_BYTES
        })
}

fn source_archive_max_bytes() -> u64 {
    std::env::var("SANDBOXED_NODE_MAX_SOURCE_ARCHIVE_BYTES")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
        .unwrap_or(DEFAULT_MAX_SOURCE_ARCHIVE_BYTES)
}

fn source_archive_expanded_max_bytes() -> u64 {
    std::env::var("SANDBOXED_NODE_MAX_SOURCE_ARCHIVE_EXPANDED_BYTES")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
        .unwrap_or(DEFAULT_MAX_SOURCE_ARCHIVE_EXPANDED_BYTES)
}

pub(crate) fn source_archive_sha256(commit: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sandboxed-source-archive-v1\0");
    hasher.update(commit.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn decode_source_archive(source: &JobSource, archive: &SourceArchive) -> Result<Vec<u8>, String> {
    if archive.sha256.len() != 64
        || !archive
            .sha256
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
    {
        return Err("invalid source archive sha256".to_string());
    }
    if archive.size_bytes == 0 || archive.size_bytes > source_archive_max_bytes() {
        return Err(format!(
            "source archive size must be between 1 and {} bytes",
            source_archive_max_bytes()
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&archive.data_base64)
        .map_err(|_| "invalid source archive base64".to_string())?;
    if bytes.len() as u64 != archive.size_bytes {
        return Err("source archive decoded size mismatch".to_string());
    }
    let computed = source_archive_sha256(&source.commit, &bytes);
    if !crate::remote_node::protocol::constant_time_eq(
        computed.as_bytes(),
        archive.sha256.as_bytes(),
    ) {
        return Err("source archive commit-bound hash mismatch".to_string());
    }
    if bytes.len() < 12 || &bytes[..4] != b"PACK" {
        return Err("source archive is not a Git pack".to_string());
    }
    let object_count = u32::from_be_bytes(bytes[8..12].try_into().expect("fixed pack header"));
    if object_count as usize > MAX_SOURCE_ARCHIVE_OBJECTS {
        return Err(format!(
            "source archive contains more than {MAX_SOURCE_ARCHIVE_OBJECTS} objects"
        ));
    }
    Ok(bytes)
}

fn bundle_path_is_safe(path: &str) -> bool {
    rel_path_is_safe(path)
        && !path.is_empty()
        && !path
            .split('/')
            .any(|component| matches!(component, ".git" | ".lake"))
}

pub(crate) fn bundle_manifest_sha256_for_mode(
    files: &[(String, String)],
    complete: bool,
) -> String {
    let mut hasher = Sha256::new();
    if complete {
        hasher.update(b"sandboxed-source-bundle-v2-complete\n");
    } else {
        hasher.update(b"sandboxed-source-bundle-v1\n");
    }
    for (path, sha256) in files {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn bundle_operations_sha256(bundle: &SourceBundle) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sandboxed-source-bundle-ops-v1\n");
    for file in &bundle.files {
        hasher.update(b"file\0");
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(if file.executable == Some(true) {
            b"x"
        } else {
            b"-"
        });
        hasher.update(b"\n");
    }
    for path in &bundle.deleted_paths {
        hasher.update(b"delete\0");
        hasher.update(path.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn validate_source_bundle(bundle: &SourceBundle) -> Result<u64, String> {
    if bundle.files.is_empty() && bundle.deleted_paths.is_empty() {
        return Err("source bundle must contain files or deletions".to_string());
    }
    let max_files = if bundle.complete {
        MAX_COMPLETE_SOURCE_BUNDLE_FILES
    } else {
        MAX_SOURCE_BUNDLE_FILES
    };
    if bundle
        .files
        .len()
        .saturating_add(bundle.deleted_paths.len())
        > max_files
    {
        return Err(format!(
            "source bundle has {} operations; maximum is {max_files}",
            bundle.files.len() + bundle.deleted_paths.len()
        ));
    }
    let mut manifest_files = Vec::with_capacity(bundle.files.len());
    let mut previous: Option<&str> = None;
    let mut total = 0_u64;
    for file in &bundle.files {
        if !bundle_path_is_safe(&file.path) {
            return Err(format!("unsafe source bundle path '{}'", file.path));
        }
        if previous.is_some_and(|prior| prior >= file.path.as_str()) {
            return Err("source bundle paths must be unique and sorted".to_string());
        }
        previous = Some(&file.path);
        if file.sha256.len() != 64
            || !file
                .sha256
                .chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
        {
            return Err(format!("invalid source bundle sha256 for '{}'", file.path));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.data_base64)
            .map_err(|_| format!("invalid source bundle base64 for '{}'", file.path))?;
        total = total.saturating_add(bytes.len() as u64);
        if total > source_bundle_max_bytes(bundle) {
            return Err(format!(
                "source bundle is {total} bytes; maximum is {}",
                source_bundle_max_bytes(bundle)
            ));
        }
        if hex::encode(Sha256::digest(&bytes)) != file.sha256 {
            return Err(format!(
                "source bundle content hash mismatch for '{}'",
                file.path
            ));
        }
        manifest_files.push((file.path.clone(), file.sha256.clone()));
    }
    let expected = bundle_manifest_sha256_for_mode(&manifest_files, bundle.complete);
    if expected != bundle.manifest_sha256 {
        return Err("source bundle manifest hash mismatch".to_string());
    }
    let has_extended_operations = !bundle.deleted_paths.is_empty()
        || bundle.files.iter().any(|file| file.executable.is_some());
    if has_extended_operations {
        let Some(digest) = bundle.operations_sha256.as_deref() else {
            return Err("source bundle operations_sha256 is required".to_string());
        };
        if digest != bundle_operations_sha256(bundle) {
            return Err("source bundle operations hash mismatch".to_string());
        }
        let mut prior = None;
        let file_paths = bundle
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::HashSet<_>>();
        for path in &bundle.deleted_paths {
            if !bundle_path_is_safe(path) || file_paths.contains(path.as_str()) {
                return Err(format!(
                    "unsafe or conflicting source bundle deletion '{path}'"
                ));
            }
            if prior.is_some_and(|value| value >= path.as_str()) {
                return Err("source bundle deleted paths must be unique and sorted".to_string());
            }
            prior = Some(path.as_str());
        }
    } else if bundle.operations_sha256.is_some() {
        return Err("source bundle operations_sha256 has no operations".to_string());
    }
    Ok(total)
}

/// Only remote fetch sources: git happily fetches local paths and file://
/// URLs, which would let a remote-build token holder exfiltrate node-local
/// git data into a checkout.
fn repo_url_is_remote(repo: &str) -> bool {
    let repo = repo.trim();
    if repo.starts_with("https://") || repo.starts_with("ssh://") {
        return true;
    }
    // scp-like syntax: user@host:path (no scheme). Require the colon after
    // the host part and forbid path-like prefixes.
    if let Some((userhost, path)) = repo.split_once(':') {
        return userhost.contains('@')
            && !userhost.contains('/')
            && !path.is_empty()
            && !repo.starts_with('/')
            && !repo.starts_with('.');
    }
    false
}

fn repo_url_has_credentials(repo: &str) -> bool {
    let Ok(parsed) = url::Url::parse(repo) else {
        return false;
    };
    parsed.password().is_some()
        || (matches!(parsed.scheme(), "http" | "https") && !parsed.username().is_empty())
        || parsed.query().is_some()
        || parsed.fragment().is_some()
}

/// Validate a lean-build payload before touching the filesystem or network.
pub fn validate_lean_build(
    source: &JobSource,
    cwd_rel: Option<&str>,
    command: &[String],
    env: &HashMap<String, String>,
    allowlist: &[String],
) -> Result<(), String> {
    if source.repo.trim().is_empty() {
        return Err("source.repo is required".to_string());
    }
    if repo_url_has_credentials(&source.repo) {
        return Err(
            "source.repo URL must not contain credentials, query parameters, or a fragment"
                .to_string(),
        );
    }
    if !repo_url_is_remote(&source.repo) {
        return Err(format!(
            "source.repo must be an https://, ssh:// or git@host: URL (got '{}'); \
             local paths and file:// would expose node-local repositories",
            source.repo
        ));
    }
    if !commit_is_valid(&source.commit) {
        return Err(format!(
            "source.commit must be a full 40-char lowercase hex SHA (got '{}')",
            source.commit
        ));
    }
    if let Some(archive) = &source.archive {
        decode_source_archive(source, archive)?;
    }
    if let Some(bundle) = &source.bundle {
        validate_source_bundle(bundle)?;
    }
    let rel_clean = cwd_rel.unwrap_or("").trim_matches('/');
    if !rel_path_is_safe(rel_clean) {
        return Err(format!("invalid cwd_rel '{rel_clean}'"));
    }
    let Some(argv0) = command.first().filter(|argv0| !argv0.trim().is_empty()) else {
        return Err("command must be a non-empty argv".to_string());
    };
    // Bare tool names only: a path like `./lake` or `subdir/lake` would make
    // Command::new execute an attacker-controlled file from the checkout
    // instead of the PATH-resolved tool, bypassing the allowlist.
    if argv0.contains('/') || argv0.contains('\\') {
        return Err(format!(
            "argv[0] must be a bare tool name (one of {}), not a path: '{argv0}'",
            ALLOWED_COMMANDS.join("/")
        ));
    }
    if !ALLOWED_COMMANDS.contains(&argv0.as_str()) {
        return Err(format!(
            "command '{argv0}' is not allowed; argv[0] must be one of {}",
            ALLOWED_COMMANDS.join("/")
        ));
    }
    // Lake repository operations may execute untrusted project code and must
    // therefore run under the dedicated, non-root node service account. Keep
    // arbitrary-command escape hatches (`lake env <anything>`, `lake exe`) out
    // of the protocol while accepting the verification verbs routed by the
    // mission-local shim. `lake env lean` is safe because the nested executable
    // is fixed rather than caller-selected.
    if argv0 == "lake" {
        let allowed = matches!(
            command.get(1).map(String::as_str),
            Some("build" | "test" | "check")
        ) || (command.get(1).map(String::as_str) == Some("env")
            && command.get(2).map(String::as_str) == Some("lean"));
        if !allowed {
            return Err(
                "lake command must use build, test, check, or the fixed 'env lean' form"
                    .to_string(),
            );
        }
    }
    for key in env.keys() {
        if !allowlist.iter().any(|allowed| allowed == key) {
            return Err(format!(
                "env key '{key}' is not in the node allowlist ({})",
                allowlist.join(",")
            ));
        }
    }
    Ok(())
}

/// Reject a build if its estimated peak plus the emergency floor cannot fit
/// on the filesystem backing the node work root.
pub fn validate_disk_admission(work_root: &Path, estimated_disk_bytes: u64) -> Result<(), String> {
    let available = fs2::available_space(work_root)
        .map_err(|error| format!("cannot stat node work dir disk space: {error}"))?;
    let required = estimated_disk_bytes.saturating_add(min_free_bytes());
    if available < required {
        return Err(format!(
            "insufficient node disk: {} GiB available, {} GiB estimated plus {} GiB emergency floor",
            available / (1 << 30),
            estimated_disk_bytes / (1 << 30),
            min_free_bytes() / (1 << 30)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cache & checkout layout (pure path math, unit-tested)
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Content-addressed checkout directory for `(repo, commit)`.
pub fn checkout_dir(work_root: &Path, repo: &str, commit: &str) -> PathBuf {
    let repo_hash = sha256_hex(repo.as_bytes());
    work_root
        .join("checkouts")
        .join(&repo_hash[..16])
        .join(commit)
}

fn caches_dir(work_root: &Path) -> PathBuf {
    work_root.join("caches")
}

fn lake_cache_slot(work_root: &Path, cache_key: &str) -> PathBuf {
    caches_dir(work_root).join("lake").join(cache_key)
}

/// Shared-cache env for build steps: `ELAN_HOME`, `XDG_CACHE_HOME`, `HOME`
/// under `<workdir>/caches/`, plus `PATH` with the shared elan bin prepended.
fn cache_env(work_root: &Path) -> Vec<(String, String)> {
    let caches = caches_dir(work_root);
    let elan = caches.join("elan");
    let mut path = elan.join("bin").display().to_string();
    if let Ok(existing) = std::env::var("PATH") {
        path.push(':');
        path.push_str(&existing);
    }
    vec![
        ("ELAN_HOME".to_string(), elan.display().to_string()),
        (
            "XDG_CACHE_HOME".to_string(),
            caches.join("xdg").display().to_string(),
        ),
        (
            "HOME".to_string(),
            caches.join("home").display().to_string(),
        ),
        ("PATH".to_string(), path),
    ]
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a live, NUL-terminated C string. `access` has no
        // additional pointer or lifetime requirements and only probes access
        // for the current process identity (the sandboxed-node service user).
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn lean_runtime_ready_with_path(work_root: &Path, service_path: Option<&OsStr>) -> bool {
    let cached_lake = caches_dir(work_root).join("elan/bin/lake");
    executable_file(&cached_lake)
        || service_path
            .into_iter()
            .flat_map(std::env::split_paths)
            // Repository-controlled relative PATH entries must never make a
            // node advertise itself as a trustworthy Lean runner.
            .filter(|dir| dir.is_absolute())
            .any(|dir| executable_file(&dir.join("lake")))
}

/// Whether a `lean`-labelled node can actually start the Lake proxy used by
/// declarative builds. Toolchains may be downloaded lazily by Elan, but a
/// missing proxy would make every accepted Lean job fail with `ENOENT`.
pub fn lean_runtime_ready(work_root: &Path) -> bool {
    lean_runtime_ready_with_path(work_root, std::env::var_os("PATH").as_deref())
}

/// Add Lean/Lake concurrency defaults derived from the node process's usable
/// parallelism. Lake starts several Lean processes and each Lean process may
/// use several threads, so assigning the full CPU count to both knobs would
/// multiply rather than divide the node's CPU budget.
///
/// `available_parallelism` accounts for OS affinity and cgroup limits. A
/// payload may still override either or both values; when it supplies only one
/// knob, the missing one is derived from the remaining CPU budget.
fn lean_concurrency_env(
    payload_env: &HashMap<String, String>,
    available_parallelism: usize,
    uses_lake_fanout: bool,
) -> HashMap<String, String> {
    const MAX_DEFAULT_LAKE_JOBS: usize = 4;

    fn positive_value(env: &HashMap<String, String>, key: &str) -> Option<usize> {
        env.get(key)
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
    }

    let budget = available_parallelism.max(1);
    let explicit_threads = payload_env.contains_key("LEAN_NUM_THREADS");
    let explicit_lake_jobs = payload_env.contains_key("LAKE_JOBS");
    let mut effective = HashMap::new();

    if !explicit_lake_jobs {
        let jobs = if uses_lake_fanout {
            let thread_cost = positive_value(payload_env, "LEAN_NUM_THREADS").unwrap_or(1);
            (budget / thread_cost).clamp(1, MAX_DEFAULT_LAKE_JOBS)
        } else {
            1
        };
        effective.insert("LAKE_JOBS".to_string(), jobs.to_string());
    }
    if !explicit_threads {
        let lake_cost = if uses_lake_fanout {
            positive_value(payload_env, "LAKE_JOBS")
                .unwrap_or_else(|| budget.min(MAX_DEFAULT_LAKE_JOBS))
        } else {
            1
        };
        let threads = (budget / lake_cost).max(1);
        effective.insert("LEAN_NUM_THREADS".to_string(), threads.to_string());
    }
    effective.extend(payload_env.clone());
    effective
}

/// Divide the node's usable CPU affinity among the number of jobs that its
/// admission controller can run concurrently. The node may be idle when a
/// single job starts, but budgeting for the configured worst case prevents a
/// second job from multiplying Lean/Lake fan-out beyond the host capacity.
fn per_job_parallelism(available_parallelism: usize, capacity: u32) -> usize {
    available_parallelism
        .max(1)
        .checked_div(capacity.max(1) as usize)
        .unwrap_or(1)
        .max(1)
}

/// Derive the default lake cache key from the build cwd: sha256 over the
/// contents of `lean-toolchain` and `lake-manifest.json` (whichever exist).
/// `None` when neither file is present (no cache slot is used then).
pub fn derive_cache_key(build_cwd: &Path) -> Option<String> {
    let toolchain = std::fs::read(build_cwd.join("lean-toolchain")).ok();
    let manifest = std::fs::read(build_cwd.join("lake-manifest.json")).ok();
    if toolchain.is_none() && manifest.is_none() {
        return None;
    }
    let mut hasher = Sha256::new();
    if let Some(bytes) = &toolchain {
        hasher.update(b"lean-toolchain:");
        hasher.update(bytes);
    }
    if let Some(bytes) = &manifest {
        hasher.update(b"lake-manifest:");
        hasher.update(bytes);
    }
    Some(hex::encode(hasher.finalize()))
}

/// Return dependency-cache paths only after Lake's *evaluated* root package
/// layout has been attested by the node.
///
/// `lake-manifest.json` is repository content and does not authoritatively
/// describe `buildDir`: a checked-in or stale field can disagree with a
/// dynamic lakefile. Trusting it could copy root `.olean` files through a
/// directory presented as `packagesDir`, making a later commit falsely pass.
/// Until the node has a trusted Lake configuration query, fail closed and keep
/// these builds cold. Shared Elan/XDG/Home caches remain enabled.
fn configured_lake_cache_paths(_build_cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    None
}

/// Namespace a dependency cache key by the project and requested build. A
/// toolchain/manifest digest alone is shared by unrelated repositories and
/// can therefore restore stale `.lake` outputs that the current job never
/// produced.
fn project_cache_key(
    dependency_key: &str,
    source: &JobSource,
    cwd_rel: &str,
    command: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lake-cache-v2\0dependency\0");
    hasher.update(dependency_key.as_bytes());
    hasher.update(b"\0repo\0");
    hasher.update(source.repo.trim().as_bytes());
    hasher.update(b"\0cwd\0");
    hasher.update(cwd_rel.as_bytes());
    hasher.update(b"\0command\0");
    for arg in command {
        hasher.update((arg.len() as u64).to_be_bytes());
        hasher.update(arg.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Toolchain directory names under the shared elan cache, for heartbeat
/// `cached_toolchains` (empty when the cache does not exist yet).
pub fn cached_toolchains(work_root: &Path) -> Vec<String> {
    let dir = caches_dir(work_root).join("elan").join("toolchains");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Artifact resolution (pure over a directory tree, unit-tested)
// ---------------------------------------------------------------------------

/// Whether an artifact pattern is safe: relative, no `..`/`.` components.
pub fn artifact_pattern_is_safe(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.starts_with('/')
        && pattern
            .split('/')
            .all(|component| !component.is_empty() && component != ".." && component != ".")
}

/// Match one path segment against a pattern where `*` matches any run of
/// characters *within the segment* (never across `/`).
fn segment_matches(name: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return name == pattern;
    }
    let Some(mut rest) = name.strip_prefix(parts[0]) else {
        return false;
    };
    let last = parts.len() - 1;
    for part in &parts[1..last] {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    rest.len() >= parts[last].len() && rest.ends_with(parts[last])
}

/// Return metadata only when every component below `root` is present and is
/// not a symlink. Artifact paths come from an untrusted build, so following a
/// link at either an intermediate directory or the final file would escape the
/// checkout confinement promised by this API.
fn symlink_free_metadata(root: &Path, rel: &Path) -> Option<std::fs::Metadata> {
    let mut current = root.to_path_buf();
    let mut metadata = std::fs::symlink_metadata(&current).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    for component in rel.components() {
        current.push(component);
        metadata = std::fs::symlink_metadata(&current).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
    }
    Some(metadata)
}

/// Resolve artifact patterns to existing files below `root`.
///
/// Deliberately tiny glob: `*` is supported within a single path segment
/// only; everything else is an exact relative path. Patterns with `..` (or
/// absolute paths) are rejected, and expansion only ever joins validated
/// segments below `root`, so results cannot escape the checkout.
pub fn resolve_artifact_paths(root: &Path, patterns: &[String]) -> Result<Vec<String>, String> {
    let mut out = std::collections::BTreeSet::new();
    for pattern in patterns {
        if !artifact_pattern_is_safe(pattern) {
            return Err(format!("unsafe artifact pattern '{pattern}'"));
        }
        let segments: Vec<&str> = pattern.split('/').collect();
        let mut current: Vec<PathBuf> = vec![PathBuf::new()];
        for segment in &segments {
            let mut next = Vec::new();
            for rel in &current {
                if segment.contains('*') {
                    if !symlink_free_metadata(root, rel).is_some_and(|metadata| metadata.is_dir()) {
                        continue;
                    }
                    let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
                        continue;
                    };
                    for entry in entries.filter_map(|entry| entry.ok()) {
                        let Ok(name) = entry.file_name().into_string() else {
                            continue;
                        };
                        if segment_matches(&name, segment) {
                            next.push(rel.join(name));
                        }
                    }
                } else {
                    next.push(rel.join(segment));
                }
            }
            current = next;
        }
        for rel in current {
            if symlink_free_metadata(root, &rel).is_some_and(|metadata| metadata.is_file()) {
                out.insert(rel.to_string_lossy().into_owned());
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// Resolve artifact patterns and compute sha256 + size for each match.
pub async fn collect_artifacts(
    root: &Path,
    patterns: &[String],
) -> Result<Vec<ArtifactEntry>, String> {
    let root = root.to_path_buf();
    let patterns = patterns.to_vec();
    tokio::task::spawn_blocking(move || {
        let rels = resolve_artifact_paths(&root, &patterns)?;
        rels.into_iter()
            .map(|rel| {
                let full = root.join(&rel);
                if !symlink_free_metadata(&root, Path::new(&rel))
                    .is_some_and(|metadata| metadata.is_file())
                {
                    return Err(format!(
                        "artifact path is not a symlink-free regular file: {rel}"
                    ));
                }
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let mut file = options
                    .open(&full)
                    .map_err(|e| format!("open artifact {rel}: {e}"))?;
                let mut hasher = Sha256::new();
                let size_bytes = std::io::copy(&mut file, &mut hasher)
                    .map_err(|e| format!("hash artifact {rel}: {e}"))?;
                Ok(ArtifactEntry {
                    path: rel,
                    sha256: hex::encode(hasher.finalize()),
                    size_bytes,
                })
            })
            .collect()
    })
    .await
    .map_err(|e| format!("artifact hashing task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// File locking (fs2 flock)
// ---------------------------------------------------------------------------

/// Exclusive advisory file lock; released on drop.
struct FileLock {
    file: std::fs::File,
}

impl FileLock {
    async fn acquire(path: PathBuf) -> anyhow::Result<Self> {
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)?;
            file.lock_exclusive()?;
            Ok(Self { file })
        })
        .await?
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

// ---------------------------------------------------------------------------
// Checkout materialization (network; not exercised by unit tests)
// ---------------------------------------------------------------------------

/// `GIT_SSH_COMMAND` when `SANDBOXED_NODE_GIT_SSH_KEY` points at a key file.
fn git_ssh_env() -> Option<(String, String)> {
    let key = std::env::var("SANDBOXED_NODE_GIT_SSH_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())?;
    Some((
        "GIT_SSH_COMMAND".to_string(),
        format!("ssh -i {key} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"),
    ))
}

/// Run one git step, appending output to the job log. Returns an error with
/// the step name when the command is cancelled, times out, or exits non-zero.
async fn run_git_step(
    args: &[&str],
    cwd: &Path,
    log_path: &Path,
    token: &CancellationToken,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(cwd);
    if let Some((key, value)) = git_ssh_env() {
        cmd.env(key, value);
    }
    let outcome = run_logged_command(
        cmd,
        CommandEnvironment::Inherit,
        log_path,
        GIT_STEP_TIMEOUT_SECS,
        token,
    )
    .await?;
    if !outcome.success() {
        let (_, code, error) = outcome.into_job_result();
        anyhow::bail!(
            "git {} failed ({}, exit {code:?}); see job log",
            args.first().copied().unwrap_or("?"),
            error.unwrap_or_default(),
        );
    }
    Ok(())
}

fn validate_archive_tree_output(output: &[u8]) -> Result<(), String> {
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| "malformed source archive tree entry".to_string())?;
        let (metadata, path_with_separator) = record.split_at(separator);
        let path = &path_with_separator[1..];
        let mode = metadata
            .split(|byte| *byte == b' ')
            .next()
            .ok_or_else(|| "malformed source archive tree mode".to_string())?;
        if !matches!(mode, b"040000" | b"100644" | b"100755") {
            return Err(format!(
                "source archive contains unsupported tree mode {}",
                String::from_utf8_lossy(mode)
            ));
        }
        let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 3
            || !matches!(
                (fields[0], fields[1]),
                (b"040000", b"tree") | (b"100644" | b"100755", b"blob")
            )
            || fields[2].len() != 40
            || !fields[2].iter().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("malformed source archive tree metadata".to_string());
        }
        let path = std::str::from_utf8(path)
            .map_err(|_| "source archive paths must be UTF-8".to_string())?;
        if path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".." | ".git"))
        {
            return Err(format!("unsafe source archive path '{path}'"));
        }
    }
    Ok(())
}

async fn import_source_archive(
    tmp: &Path,
    source: &JobSource,
    archive: &SourceArchive,
    log_path: &Path,
    token: &CancellationToken,
) -> anyhow::Result<()> {
    let bytes = decode_source_archive(source, archive).map_err(|error| anyhow::anyhow!(error))?;
    run_git_step(&["init", "--quiet"], tmp, log_path, token).await?;

    let mut child = tokio::process::Command::new("git")
        // The transport deliberately contains the requested commit and its
        // complete snapshot, not historical parent commits. `index-pack`
        // still verifies pack/object integrity; `--strict` cannot be used
        // because it requires every referenced parent object.
        .args(["index-pack", "--stdin"])
        .current_dir(tmp)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("git index-pack stdin unavailable"))?
        .write_all(&bytes)
        .await?;
    if token.is_cancelled() {
        anyhow::bail!("source archive import cancelled");
    }
    let output = tokio::time::timeout(
        Duration::from_secs(GIT_STEP_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("source archive import timed out"))??;
    if !output.status.success() {
        anyhow::bail!(
            "source archive is not a valid Git object pack: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let pack_hash_output = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow::anyhow!("git index-pack returned a non-UTF-8 pack id"))?
        .trim();
    let pack_hash = pack_hash_output
        .strip_prefix("pack\t")
        .unwrap_or(pack_hash_output);
    if pack_hash.len() != 40 || !pack_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("git index-pack returned an invalid pack id");
    }
    let index_path = tmp.join(format!(".git/objects/pack/pack-{pack_hash}.idx"));
    let verified = tokio::process::Command::new("git")
        .args(["verify-pack", "-v"])
        .arg(&index_path)
        .current_dir(tmp)
        .output()
        .await?;
    if !verified.status.success() {
        anyhow::bail!("source archive pack verification failed");
    }
    let expanded_limit = source_archive_expanded_max_bytes();
    let mut expanded_bytes = 0_u64;
    let mut object_count = 0_usize;
    for line in verified.stdout.split(|byte| *byte == b'\n') {
        let mut fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty());
        let Some(object_id) = fields.next() else {
            continue;
        };
        if object_id.len() != 40 || !object_id.iter().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let _kind = fields.next();
        let size = fields
            .next()
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .and_then(|raw| raw.parse::<u64>().ok())
            .ok_or_else(|| anyhow::anyhow!("malformed source archive object size"))?;
        object_count = object_count.saturating_add(1);
        if object_count > MAX_SOURCE_ARCHIVE_OBJECTS {
            anyhow::bail!("source archive contains more than {MAX_SOURCE_ARCHIVE_OBJECTS} objects");
        }
        expanded_bytes = expanded_bytes.saturating_add(size);
        if expanded_bytes > expanded_limit {
            anyhow::bail!("source archive expands to more than {expanded_limit} bytes");
        }
    }
    tokio::fs::write(tmp.join(".git/shallow"), format!("{}\n", source.commit)).await?;

    let commit_object = format!("{}^{{commit}}", source.commit);
    run_git_step(
        &["cat-file", "-e", commit_object.as_str()],
        tmp,
        log_path,
        token,
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "source archive does not contain requested commit {}",
            source.commit
        )
    })?;
    let tree = tokio::process::Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "-t",
            "-z",
            "--full-tree",
            source.commit.as_str(),
        ])
        .current_dir(tmp)
        .output()
        .await?;
    if !tree.status.success() {
        anyhow::bail!(
            "source archive does not contain the complete tree for commit {}",
            source.commit
        );
    }
    validate_archive_tree_output(&tree.stdout).map_err(|error| anyhow::anyhow!(error))?;
    run_git_step(
        &["checkout", "--quiet", "--detach", source.commit.as_str()],
        tmp,
        log_path,
        token,
    )
    .await?;
    tokio::fs::write(
        tmp.join(".git/sandboxed-source-archive"),
        format!("{}\n", archive.sha256),
    )
    .await?;

    let mut log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await?;
    log.write_all(
        format!(
            "source archive verified sha256={} commit={} bytes={}\n",
            archive.sha256, source.commit, archive.size_bytes
        )
        .as_bytes(),
    )
    .await?;
    Ok(())
}

/// Ensure `<workdir>/checkouts/<repo-hash>/<commit>/` exists, fetching it if
/// needed. Builds into a temp sibling and atomically renames so a partially
/// fetched tree is never observed at the final path. Callers must hold the
/// per-checkout flock.
async fn ensure_checkout(
    work_root: &Path,
    source: &JobSource,
    log_path: &Path,
    token: &CancellationToken,
) -> anyhow::Result<PathBuf> {
    let base = checkout_dir(work_root, &source.repo, &source.commit);
    let complete_bundle = source.bundle.as_ref().filter(|bundle| bundle.complete);
    let dest = complete_bundle.map_or(base.clone(), |bundle| {
        base.with_file_name(format!(
            "{}-source-{}",
            source.commit, bundle.manifest_sha256
        ))
    });
    if complete_bundle.is_some() {
        match tokio::fs::symlink_metadata(&dest).await {
            Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
                "complete source checkout must not be a symlink: {}",
                dest.display()
            ),
            Ok(metadata) if metadata.is_dir() => {
                // Complete snapshots have no Git metadata to reset. Recreate
                // their source before each build so stale `.lake` output can
                // never turn into validation evidence.
                tokio::fs::remove_dir_all(&dest).await?;
            }
            Ok(_) => anyhow::bail!(
                "complete source checkout is not a directory: {}",
                dest.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    } else if dest.is_dir() {
        match &source.archive {
            None => return Ok(dest),
            Some(archive)
                if tokio::fs::read_to_string(dest.join(".git/sandboxed-source-archive"))
                    .await
                    .is_ok_and(|digest| digest.trim() == archive.sha256) =>
            {
                return Ok(dest);
            }
            // A legacy/network checkout or a different archive digest cannot
            // attest this request. Validate the supplied pack in a temporary
            // repository before reusing the already materialized commit.
            Some(_) => {}
        }
    }
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("checkout dir has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    let tmp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmp).await?;

    let materialize = async {
        if let Some(bundle) = complete_bundle {
            apply_source_bundle(&tmp, bundle, log_path).await?;
            return anyhow::Ok(());
        }
        if let Some(archive) = &source.archive {
            import_source_archive(&tmp, source, archive, log_path, token).await?;
            return anyhow::Ok(());
        }
        run_git_step(&["init", "--quiet"], &tmp, log_path, token).await?;
        run_git_step(
            &[
                "fetch",
                "--depth",
                "1",
                "--quiet",
                source.repo.as_str(),
                source.commit.as_str(),
            ],
            &tmp,
            log_path,
            token,
        )
        .await?;
        run_git_step(
            &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
            &tmp,
            log_path,
            token,
        )
        .await?;
        // Best-effort: many Lean repos have no submodules and lakefile deps
        // are fetched by lake itself.
        let _ = run_git_step(
            &["submodule", "update", "--init", "--depth", "1"],
            &tmp,
            log_path,
            token,
        )
        .await;
        anyhow::Ok(())
    }
    .await;

    if let Err(err) = materialize {
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        return Err(err);
    }
    let stale = if dest.is_dir() {
        let stale = parent.join(format!(".stale-{}", uuid::Uuid::new_v4()));
        tokio::fs::rename(&dest, &stale).await?;
        Some(stale)
    } else {
        None
    };
    match tokio::fs::rename(&tmp, &dest).await {
        Ok(()) => {
            if let Some(stale) = stale {
                let _ = tokio::fs::remove_dir_all(stale).await;
            }
            Ok(dest)
        }
        // A concurrent build (other lock domain) won the rename; reuse theirs.
        Err(_)
            if dest.is_dir()
                && source.archive.as_ref().is_none_or(|archive| {
                    std::fs::read_to_string(dest.join(".git/sandboxed-source-archive"))
                        .is_ok_and(|digest| digest.trim() == archive.sha256)
                }) =>
        {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            if let Some(stale) = stale {
                let _ = tokio::fs::remove_dir_all(stale).await;
            }
            Ok(dest)
        }
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            if let Some(stale) = stale {
                let _ = tokio::fs::rename(stale, &dest).await;
            }
            Err(err.into())
        }
    }
}

/// Copy `src` to `dest` without hardlinks. Builds mutate `.lake` in place, so
/// hardlinks would let one checkout corrupt the shared slot or another build.
/// `dest` must not exist.
async fn isolated_copy(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new("cp");
    command.arg("-a");
    // Linux runners use CoW reflinks when the backing filesystem supports
    // them, avoiding another physical Mathlib tree while preserving mutation
    // isolation. `auto` falls back to an ordinary copy on ext4 and similar.
    #[cfg(target_os = "linux")]
    command.arg("--reflink=auto");
    let output = command.arg(src).arg(dest).output().await?;
    if !output.status.success() {
        anyhow::bail!(
            "cp -a {} -> {} failed: {}",
            src.display(),
            dest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Require a cache root to be a real directory, not a symlink. Checking only
/// `Path::is_dir` follows symlinks; a repository-controlled `.lake` symlink
/// would then let later cleanup resolve `build` outside the checkout/cache.
async fn require_real_directory(path: &Path, label: &str) -> anyhow::Result<bool> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        anyhow::bail!("{label} must not be a symlink: {}", path.display());
    }
    if !metadata.is_dir() {
        anyhow::bail!("{label} is not a directory: {}", path.display());
    }
    Ok(true)
}

/// Validate every component of a directory path below a trusted lexical root.
/// Checking only the final path follows symlinked parents such as
/// `.lake/nested -> /outside`, which would let cache sync read outside Lake.
async fn require_real_directory_tree(
    root: &Path,
    path: &Path,
    label: &str,
) -> anyhow::Result<bool> {
    if path == root || !path.starts_with(root) {
        anyhow::bail!("{label} must be below {}", root.display());
    }
    if !require_real_directory(root, label).await? {
        return Ok(false);
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("{label} must be below {}", root.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            anyhow::bail!("{label} contains an invalid path component");
        };
        current.push(part);
        if !require_real_directory(&current, label).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Before creating or deleting a bundle destination, require every existing
/// ancestor below the checkout root to be a real directory. Once a component is
/// absent, all deeper components are necessarily absent too and may be created
/// safely (or, for deletions, the target cannot exist).
async fn require_safe_directory_creation_path(
    root: &Path,
    path: &Path,
    label: &str,
) -> anyhow::Result<()> {
    if path == root || !path.starts_with(root) {
        anyhow::bail!("{label} must be below {}", root.display());
    }
    if !require_real_directory(root, label).await? {
        anyhow::bail!("{label} root does not exist: {}", root.display());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("{label} must be below {}", root.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            anyhow::bail!("{label} contains an invalid path component");
        };
        current.push(part);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("{label} must not contain a symlink: {}", current.display());
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => anyhow::bail!("{label} is not a directory: {}", current.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn apply_source_bundle(
    checkout: &Path,
    bundle: &SourceBundle,
    log_path: &Path,
) -> anyhow::Result<()> {
    let total_bytes = validate_source_bundle(bundle).map_err(|error| anyhow::anyhow!("{error}"))?;
    for relative in &bundle.deleted_paths {
        let destination = checkout.join(relative);
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("bundle deletion path '{relative}' has no parent"))?;
        if parent != checkout {
            // A tracked symlink such as `escape -> /outside` would let
            // `remove_file` on `escape/victim` delete outside the checkout:
            // `symlink_metadata` refuses to follow only the final component.
            require_safe_directory_creation_path(checkout, parent, "source bundle deletion")
                .await?;
        }
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                tokio::fs::remove_file(&destination).await?;
            }
            Ok(_) => anyhow::bail!(
                "source bundle deletion must target a file: {}",
                destination.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for file in &bundle.files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.data_base64)
            .map_err(|_| anyhow::anyhow!("invalid bundle base64 for '{}'", file.path))?;
        let destination = checkout.join(&file.path);
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("bundle path '{}' has no parent", file.path))?;
        if parent != checkout {
            require_safe_directory_creation_path(checkout, parent, "source bundle destination")
                .await?;
        }
        tokio::fs::create_dir_all(parent).await?;
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "source bundle destination must not be a symlink: {}",
                    destination.display()
                );
            }
            Ok(metadata) if metadata.is_dir() => {
                anyhow::bail!(
                    "source bundle destination is a directory: {}",
                    destination.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let tmp = parent.join(format!(".source-bundle-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&tmp, &bytes).await?;
        if let Err(error) = tokio::fs::rename(&tmp, &destination).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(error.into());
        }
        #[cfg(unix)]
        if let Some(executable) = file.executable {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(&destination).await?.permissions();
            let mode = permissions.mode();
            permissions.set_mode(if executable {
                mode | 0o111
            } else {
                mode & !0o111
            });
            tokio::fs::set_permissions(&destination, permissions).await?;
        }
    }
    let mut log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await?;
    log.write_all(
        format!(
            "source bundle verified sha256={} operations_sha256={} files={} deletions={} bytes={}\n",
            bundle.manifest_sha256,
            bundle.operations_sha256.as_deref().unwrap_or("none"),
            bundle.files.len(),
            bundle.deleted_paths.len(),
            total_bytes
        )
        .as_bytes(),
    )
    .await?;
    Ok(())
}

/// Restore only shared Lake dependencies. Every other top-level `.lake` entry
/// belongs to the root project and is commit-specific (including custom Lake
/// `buildDir` values), so copying the whole directory can make Lake trust a
/// stale `.olean` and skip changed source.
async fn restore_lake_dependency_cache(
    slot: &Path,
    build_cwd: &Path,
    lake_dir: &Path,
    packages_dir: &Path,
) -> anyhow::Result<()> {
    if !require_real_directory(slot, "lake cache slot").await? {
        return Ok(());
    }
    let packages = slot.join("packages");
    if !require_real_directory(&packages, "lake cache packages").await? {
        return Ok(());
    }
    match tokio::fs::symlink_metadata(lake_dir).await {
        Ok(_) => anyhow::bail!(
            "lake cache restore destination already exists: {}",
            lake_dir.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let Some(packages_parent) = packages_dir.parent() else {
        anyhow::bail!("configured packages directory has no parent");
    };
    if packages_dir == lake_dir || !packages_dir.starts_with(lake_dir) {
        anyhow::bail!("configured packages directory must be below lake directory");
    }
    require_safe_directory_creation_path(build_cwd, packages_dir, "Lake cache destination").await?;
    tokio::fs::create_dir_all(packages_parent).await?;
    if let Err(error) = isolated_copy(&packages, packages_dir).await {
        let _ = tokio::fs::remove_dir_all(lake_dir).await;
        return Err(error);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Build execution
// ---------------------------------------------------------------------------

/// Execute a `lean_build` payload. Returns `Err` for validation/infra
/// failures (persisted as a failed job) and `Ok` with the run outcome
/// otherwise.
pub async fn execute_lean_build(
    work_root: &Path,
    log_path: &Path,
    payload: &JobPayload,
    node_capacity: u32,
    max_job_secs: u64,
    token: &CancellationToken,
) -> anyhow::Result<LeanBuildResult> {
    let JobPayload::LeanBuild {
        source,
        cwd_rel,
        command,
        timeout_secs,
        estimated_disk_bytes,
        cache_key,
        artifacts,
        env,
    } = payload
    else {
        anyhow::bail!("execute_lean_build called with a non-lean payload");
    };

    let allowlist = env_allowlist_from_env();
    validate_lean_build(source, cwd_rel.as_deref(), command, env, &allowlist)
        .map_err(|e| anyhow::anyhow!("invalid lean_build payload: {e}"))?;
    validate_disk_admission(work_root, estimated_disk_bytes.unwrap_or_default())
        .map_err(|e| anyhow::anyhow!("disk admission failed: {e}"))?;

    // Serialize builds of the same (repo, commit): one flock guards both the
    // checkout materialization and the build itself, so two jobs never run
    // `lake` concurrently in one checkout.
    let checkout = checkout_dir(work_root, &source.repo, &source.commit);
    let checkout_lock = FileLock::acquire(checkout.with_extension("lock")).await?;
    let checkout = ensure_checkout(work_root, source, log_path, token).await?;

    // The content-addressed checkout is reused, but build outputs are mutable.
    // Reset it before every invocation so a cancelled/different-target build
    // cannot leave `.lake` files or artifacts that a later job mistakes for
    // its own output.
    if source.bundle.as_ref().is_none_or(|bundle| !bundle.complete) {
        run_git_step(
            &["reset", "--hard", source.commit.as_str()],
            &checkout,
            log_path,
            token,
        )
        .await?;
        run_git_step(&["clean", "-ffdx"], &checkout, log_path, token).await?;
    }
    if let Some(bundle) = source.bundle.as_ref().filter(|bundle| !bundle.complete) {
        apply_source_bundle(&checkout, bundle, log_path).await?;
    }

    let rel_clean = cwd_rel.as_deref().unwrap_or("").trim_matches('/');
    let build_cwd = if rel_clean.is_empty() {
        checkout.clone()
    } else {
        checkout.join(rel_clean)
    };
    if !build_cwd.is_dir() {
        anyhow::bail!("cwd_rel '{rel_clean}' does not exist in the checkout");
    }

    // Lake dependency-cache restore: copy only the slot's packages into the
    // manifest's configured packagesDir when the checkout has no lakeDir yet.
    // Slot mutation is guarded by a per-key flock (shared across commits with
    // the same toolchain+manifest).
    let effective_cache_key = cache_key
        .clone()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| derive_cache_key(&build_cwd))
        .map(|key| project_cache_key(&key, source, rel_clean, command));
    if let Some(key) = effective_cache_key.as_deref() {
        let slot = lake_cache_slot(work_root, key);
        if let Some((lake_dir, packages_dir)) = configured_lake_cache_paths(&build_cwd) {
            let lake_dir_is_absent = tokio::fs::symlink_metadata(&lake_dir)
                .await
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            if lake_dir_is_absent && slot.is_dir() {
                let _cache_lock = FileLock::acquire(slot.with_extension("lock")).await?;
                if let Err(err) =
                    restore_lake_dependency_cache(&slot, &build_cwd, &lake_dir, &packages_dir).await
                {
                    tracing::warn!("lake cache restore failed (continuing cold): {err}");
                }
            }
        } else {
            tracing::warn!("lake cache restore skipped: invalid lakeDir/packagesDir");
        }
    }

    // Run the build argv directly (no shell) with shared-cache env plus the
    // allowlisted job env. Clear the node service environment first: bearer
    // tokens, signing secrets and operator credentials must never reach code
    // executed from a repository.
    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.env_clear().args(&command[1..]).current_dir(&build_cwd);
    // A node may also have the mission lake shim installed in /usr/local/bin.
    // This marker makes the shim execute the real Lake binary instead of
    // recursively dispatching another remote build.
    cmd.env("SANDBOXED_REMOTE_EXECUTION", "1");
    for (key, value) in cache_env(work_root) {
        cmd.env(key, value);
    }
    let available_parallelism = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1);
    for (key, value) in lean_concurrency_env(
        env,
        per_job_parallelism(available_parallelism, node_capacity),
        command.first().is_some_and(|c| c == "lake"),
    ) {
        cmd.env(key, value);
    }
    let limit_secs = clamp_timeout(*timeout_secs, max_job_secs);
    let outcome =
        run_logged_command(cmd, CommandEnvironment::Clear, log_path, limit_secs, token).await?;
    let success = outcome.success();

    let mut result_artifacts = Vec::new();
    if success {
        // Sync the lake cache back: copy into a tmp slot, then swap
        // it in under the per-key flock so readers never see a partial slot.
        if let Some(key) = effective_cache_key.as_deref() {
            if let Some((lake_dir, packages_dir)) = configured_lake_cache_paths(&build_cwd) {
                if let Err(err) =
                    sync_lake_cache_back(work_root, key, &build_cwd, &lake_dir, &packages_dir).await
                {
                    tracing::warn!("lake cache sync-back failed: {err}");
                }
            } else {
                tracing::warn!("lake cache sync-back skipped: invalid lakeDir/packagesDir");
            }
        }
        if !artifacts.is_empty() {
            result_artifacts = collect_artifacts(&checkout, artifacts)
                .await
                .map_err(|e| anyhow::anyhow!("artifact resolution failed: {e}"))?;
        }
    }
    drop(checkout_lock);

    let (state, exit_code, error) = outcome.into_job_result();
    Ok(LeanBuildResult {
        state,
        exit_code,
        error,
        artifacts: result_artifacts,
    })
}

/// Replace the Lake cache slot for `key` with dependency packages only.
async fn sync_lake_cache_back(
    work_root: &Path,
    key: &str,
    build_cwd: &Path,
    lake_dir: &Path,
    packages_dir: &Path,
) -> anyhow::Result<()> {
    if packages_dir == lake_dir || !packages_dir.starts_with(lake_dir) {
        anyhow::bail!("configured packages directory must be below lake directory");
    }
    if !require_real_directory_tree(build_cwd, packages_dir, "project Lake packages").await? {
        return Ok(());
    }
    let slot = lake_cache_slot(work_root, key);
    let parent = slot
        .parent()
        .ok_or_else(|| anyhow::anyhow!("lake cache slot has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    let _cache_lock = FileLock::acquire(slot.with_extension("lock")).await?;
    let tmp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmp).await?;
    if let Err(err) = isolated_copy(packages_dir, &tmp.join("packages")).await {
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        return Err(err);
    }
    let old = parent.join(format!(".old-{}", uuid::Uuid::new_v4()));
    let had_old = tokio::fs::rename(&slot, &old).await.is_ok();
    if let Err(err) = tokio::fs::rename(&tmp, &slot).await {
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        if had_old {
            let _ = tokio::fs::rename(&old, &slot).await;
        }
        return Err(err.into());
    }
    if had_old {
        let _ = tokio::fs::remove_dir_all(&old).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node-side cache GC
// ---------------------------------------------------------------------------

fn min_free_bytes() -> u64 {
    let gb = std::env::var("SANDBOXED_NODE_MIN_FREE_GB")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MIN_FREE_GB);
    gb.saturating_mul(1 << 30)
}

/// Spawn the periodic disk GC: every 30 minutes, when free space on the
/// work-dir filesystem drops below `SANDBOXED_NODE_MIN_FREE_GB` (default 10),
/// LRU-delete (by dir mtime) checkout dirs first, then lake cache slots,
/// until the threshold is met or nothing is left.
pub fn spawn_cache_gc(work_root: PathBuf) {
    tokio::spawn(async move {
        loop {
            let root = work_root.clone();
            let result = tokio::task::spawn_blocking(move || gc_once(&root)).await;
            if let Err(err) = result {
                tracing::warn!("node cache GC task panicked: {err}");
            }
            tokio::time::sleep(GC_INTERVAL).await;
        }
    });
}

/// Directories eligible for GC, LRU-ordered: checkouts before lake slots,
/// each group by mtime ascending.
fn gc_candidates(work_root: &Path) -> Vec<PathBuf> {
    fn dirs_by_mtime(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut with_mtime: Vec<(std::time::SystemTime, PathBuf)> = dirs
            .into_iter()
            .map(|dir| {
                let mtime = std::fs::metadata(&dir)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (mtime, dir)
            })
            .collect();
        with_mtime.sort();
        with_mtime.into_iter().map(|(_, dir)| dir).collect()
    }
    fn subdirs(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return vec![];
        };
        entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter(|path| {
                // Skip in-flight tmp dirs and lock files.
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| !name.starts_with('.'))
                    .unwrap_or(false)
            })
            .collect()
    }
    // checkouts/<repo-hash>/<commit>
    let mut checkouts = Vec::new();
    for repo_dir in subdirs(&work_root.join("checkouts")) {
        checkouts.extend(subdirs(&repo_dir));
    }
    let lake_slots = subdirs(&caches_dir(work_root).join("lake"));
    let mut candidates = dirs_by_mtime(checkouts);
    candidates.extend(dirs_by_mtime(lake_slots));
    candidates
}

fn gc_once(work_root: &Path) {
    let threshold = min_free_bytes();
    let free = match fs2::available_space(work_root) {
        Ok(free) => free,
        Err(err) => {
            tracing::warn!("node cache GC: cannot stat work dir: {err}");
            return;
        }
    };
    if free >= threshold {
        return;
    }
    tracing::info!(
        free_gb = free / (1 << 30),
        threshold_gb = threshold / (1 << 30),
        "node cache GC: below free-space threshold, evicting LRU caches"
    );
    for dir in gc_candidates(work_root) {
        // Take the same `<dir>.lock` the build path holds (checkout builds
        // and lake-slot syncs) — try-lock so an in-flight build makes the
        // sweep skip its directories instead of deleting them mid-build.
        let lock_path = dir.with_extension("lock");
        let lock_file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(
                    "node cache GC: cannot open lock for {}: {err}",
                    dir.display()
                );
                continue;
            }
        };
        if fs2::FileExt::try_lock_exclusive(&lock_file).is_err() {
            tracing::debug!(
                "node cache GC: skipped {} (locked by active build)",
                dir.display()
            );
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => tracing::info!("node cache GC: deleted {}", dir.display()),
            Err(err) => {
                tracing::warn!("node cache GC: failed to delete {}: {err}", dir.display())
            }
        }
        let _ = fs2::FileExt::unlock(&lock_file);
        drop(lock_file);
        // Keep the lock inode stable. Unlinking here can split the flock
        // domain when a waiter already has this inode open while a later job
        // recreates and locks a different inode at the same path.
        match fs2::available_space(work_root) {
            Ok(free) if free >= threshold => return,
            _ => {}
        }
    }
    tracing::warn!("node cache GC: still below threshold after evicting all caches");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn source(commit: &str) -> JobSource {
        JobSource {
            repo: "https://github.com/example/verity.git".to_string(),
            commit: commit.to_string(),
            archive: None,
            bundle: None,
        }
    }

    fn allowlist() -> Vec<String> {
        parse_env_allowlist(DEFAULT_ENV_ALLOWLIST)
    }

    fn source_bundle(path: &str, contents: &[u8]) -> SourceBundle {
        let sha256 = hex::encode(Sha256::digest(contents));
        SourceBundle {
            manifest_sha256: bundle_manifest_sha256_for_mode(
                &[(path.to_string(), sha256.clone())],
                false,
            ),
            files: vec![crate::remote_node::SourceBundleFile {
                path: path.to_string(),
                sha256,
                data_base64: base64::engine::general_purpose::STANDARD.encode(contents),
                executable: None,
            }],
            complete: false,
            deleted_paths: Vec::new(),
            operations_sha256: None,
        }
    }

    fn committed_archive() -> (tempfile::TempDir, String, SourceArchive) {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "Archive Test"]);
        git(&["config", "user.email", "archive@example.invalid"]);
        std::fs::create_dir(repo.path().join("Proof")).unwrap();
        std::fs::write(repo.path().join("Proof/Main.lean"), "baseline\n").unwrap();
        git(&["add", "Proof/Main.lean"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "parent",
        ]);
        std::fs::write(
            repo.path().join("Proof/Main.lean"),
            "theorem ok : True := by trivial\n",
        )
        .unwrap();
        git(&["add", "Proof/Main.lean"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "source",
        ]);
        let commit = String::from_utf8(git(&["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_string();
        let tree = git(&["ls-tree", "-r", "-t", "-z", "--full-tree", &commit]);
        let root_tree = String::from_utf8(git(&["rev-parse", &format!("{commit}^{{tree}}")]))
            .unwrap()
            .trim()
            .to_string();
        let mut objects = vec![commit.clone(), root_tree];
        for record in tree
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let metadata = record.split(|byte| *byte == b'\t').next().unwrap();
            objects.push(
                String::from_utf8(
                    metadata
                        .split(|byte| *byte == b' ')
                        .nth(2)
                        .unwrap()
                        .to_vec(),
                )
                .unwrap(),
            );
        }
        let mut child = Command::new("git")
            .args(["pack-objects", "--stdout", "--no-reuse-delta"])
            .current_dir(repo.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}\n", objects.join("\n")).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let archive = SourceArchive {
            sha256: source_archive_sha256(&commit, &output.stdout),
            size_bytes: output.stdout.len() as u64,
            data_base64: base64::engine::general_purpose::STANDARD.encode(output.stdout),
        };
        (repo, commit, archive)
    }

    #[tokio::test]
    async fn private_equivalent_archive_materializes_without_runner_git_credentials() {
        let (_source_repo, commit, archive) = committed_archive();
        let work_root = tempfile::tempdir().unwrap();
        let log = work_root.path().join("job.log");
        let mut source = JobSource {
            repo: "https://127.0.0.1:9/private/repository.git".to_string(),
            commit: commit.clone(),
            archive: Some(Box::new(archive)),
            bundle: None,
        };

        let checkout = ensure_checkout(work_root.path(), &source, &log, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(checkout.join("Proof/Main.lean")).unwrap(),
            "theorem ok : True := by trivial\n"
        );
        assert_eq!(
            String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(&checkout)
                    .output()
                    .unwrap()
                    .stdout
            )
            .unwrap()
            .trim(),
            commit
        );
        assert!(std::fs::read_to_string(log)
            .unwrap()
            .contains("source archive verified"));
        let log_history = Command::new("git")
            .args(["log", "--format=%H", "-1"])
            .current_dir(&checkout)
            .output()
            .unwrap();
        assert!(
            log_history.status.success(),
            "the archived commit must be a valid shallow history boundary"
        );

        // A legacy or differently attested cached checkout must be replaced by
        // the verified archive, never returned after the temporary validation.
        std::fs::write(checkout.join("Proof/Main.lean"), "stale checkout\n").unwrap();
        std::fs::write(
            checkout.join(".git/sandboxed-source-archive"),
            format!("{}\n", "0".repeat(64)),
        )
        .unwrap();
        let replaced = ensure_checkout(
            work_root.path(),
            &source,
            &work_root.path().join("replace.log"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(replaced.join("Proof/Main.lean")).unwrap(),
            "theorem ok : True := by trivial\n"
        );

        let invalid_pack = b"not a git pack";
        source.archive = Some(Box::new(SourceArchive {
            sha256: source_archive_sha256(&commit, invalid_pack),
            size_bytes: invalid_pack.len() as u64,
            data_base64: base64::engine::general_purpose::STANDARD.encode(invalid_pack),
        }));
        assert!(
            ensure_checkout(
                work_root.path(),
                &source,
                &work_root.path().join("retry.log"),
                &CancellationToken::new()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("not a Git pack"),
            "a cached checkout must not bypass validation of a different archive"
        );
    }

    #[tokio::test]
    async fn source_archive_is_commit_bound_and_missing_commit_fails_closed() {
        let (_source_repo, commit, archive) = committed_archive();
        let other_commit = "b".repeat(40);
        let mut source = JobSource {
            repo: "https://127.0.0.1:9/private/repository.git".to_string(),
            commit: other_commit.clone(),
            archive: Some(Box::new(archive.clone())),
            bundle: None,
        };
        assert!(validate_lean_build(
            &source,
            None,
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .unwrap_err()
        .contains("commit-bound hash mismatch"));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&archive.data_base64)
            .unwrap();
        source.archive.as_mut().unwrap().sha256 = source_archive_sha256(&other_commit, &bytes);
        let work_root = tempfile::tempdir().unwrap();
        let error = ensure_checkout(
            work_root.path(),
            &source,
            &work_root.path().join("job.log"),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("does not contain requested commit"),
            "{error}; original commit was {commit}"
        );
    }

    #[test]
    fn source_archive_tree_validation_rejects_unsafe_paths_and_special_entries() {
        assert!(validate_archive_tree_output(
            b"100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t../escape\0"
        )
        .unwrap_err()
        .contains("unsafe"));
        assert!(validate_archive_tree_output(
            b"120000 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tlink\0"
        )
        .unwrap_err()
        .contains("unsupported tree mode"));
    }

    #[test]
    fn legacy_public_source_without_archive_remains_valid() {
        assert!(validate_lean_build(
            &source(&"a".repeat(40)),
            None,
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .is_ok());
    }

    #[test]
    fn concurrency_env_uses_capability_defaults_and_preserves_explicit_values() {
        let defaults = lean_concurrency_env(&HashMap::new(), 12, true);
        assert_eq!(
            defaults.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("3")
        );
        assert_eq!(defaults.get("LAKE_JOBS").map(String::as_str), Some("4"));

        let explicit = HashMap::from([
            ("LEAN_NUM_THREADS".to_string(), "3".to_string()),
            ("LAKE_JOBS".to_string(), "5".to_string()),
        ]);
        let overridden = lean_concurrency_env(&explicit, 12, true);
        assert_eq!(
            overridden.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("3")
        );
        assert_eq!(overridden.get("LAKE_JOBS").map(String::as_str), Some("5"));

        let only_threads = lean_concurrency_env(
            &HashMap::from([("LEAN_NUM_THREADS".to_string(), "7".to_string())]),
            12,
            true,
        );
        assert_eq!(
            only_threads.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("7")
        );
        assert_eq!(only_threads.get("LAKE_JOBS").map(String::as_str), Some("1"));

        let only_lake = lean_concurrency_env(
            &HashMap::from([("LAKE_JOBS".to_string(), "9".to_string())]),
            12,
            true,
        );
        assert_eq!(
            only_lake.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("1")
        );
        assert_eq!(only_lake.get("LAKE_JOBS").map(String::as_str), Some("9"));

        let dgx_defaults = lean_concurrency_env(&HashMap::new(), 20, true);
        assert_eq!(
            dgx_defaults.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("5")
        );
        assert_eq!(dgx_defaults.get("LAKE_JOBS").map(String::as_str), Some("4"));

        let single_core = lean_concurrency_env(&HashMap::new(), 1, true);
        assert_eq!(
            single_core.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("1")
        );
        assert_eq!(single_core.get("LAKE_JOBS").map(String::as_str), Some("1"));

        let direct_lean = lean_concurrency_env(&HashMap::new(), 20, false);
        assert_eq!(
            direct_lean.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("20")
        );
        assert_eq!(direct_lean.get("LAKE_JOBS").map(String::as_str), Some("1"));
    }

    #[test]
    fn per_job_parallelism_respects_node_admission_capacity() {
        assert_eq!(per_job_parallelism(8, 1), 8);
        assert_eq!(per_job_parallelism(8, 2), 4);
        assert_eq!(per_job_parallelism(20, 3), 6);
        assert_eq!(per_job_parallelism(2, 8), 1);
        assert_eq!(per_job_parallelism(0, 0), 1);

        let two_job_defaults =
            lean_concurrency_env(&HashMap::new(), per_job_parallelism(8, 2), true);
        assert_eq!(
            two_job_defaults.get("LEAN_NUM_THREADS").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            two_job_defaults.get("LAKE_JOBS").map(String::as_str),
            Some("4")
        );
    }

    #[test]
    fn validation_accepts_a_well_formed_payload() {
        let env: HashMap<String, String> = [("LEAN_NUM_THREADS".to_string(), "4".to_string())]
            .into_iter()
            .collect();
        assert_eq!(
            validate_lean_build(
                &source(&"a".repeat(40)),
                Some("morpho-verity"),
                &["lake".to_string(), "build".to_string()],
                &env,
                &allowlist(),
            ),
            Ok(())
        );
        for command in [
            vec!["lake", "test"],
            vec!["lake", "check"],
            vec!["lake", "env", "lean", "Main.lean"],
        ] {
            let command = command.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert_eq!(
                validate_lean_build(
                    &source(&"a".repeat(40)),
                    Some("morpho-verity"),
                    &command,
                    &env,
                    &allowlist(),
                ),
                Ok(())
            );
        }
        // Path-form argv[0] is rejected even when the basename is allowed:
        // `./lake` or an absolute path would execute an arbitrary file from
        // the checkout / node fs instead of the PATH-resolved tool.
        for path_argv in ["/usr/local/bin/elan", "./lake", "subdir/lake"] {
            assert!(
                validate_lean_build(
                    &source(&"a".repeat(40)),
                    None,
                    &[path_argv.to_string(), "show".to_string()],
                    &HashMap::new(),
                    &allowlist(),
                )
                .is_err(),
                "{path_argv} must be rejected"
            );
        }
    }

    #[test]
    fn validation_accepts_a_verified_source_bundle_and_rejects_tampering() {
        let mut bundled_source = source(&"a".repeat(40));
        bundled_source.bundle = Some(source_bundle(
            "Beal/Proof.lean",
            b"theorem ok : True := by trivial\n",
        ));
        assert!(validate_lean_build(
            &bundled_source,
            None,
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .is_ok());

        let bundle = bundled_source.bundle.as_mut().unwrap();
        bundle.files[0].data_base64 = base64::engine::general_purpose::STANDARD.encode(b"tampered");
        assert!(validate_lean_build(
            &bundled_source,
            None,
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .unwrap_err()
        .contains("content hash mismatch"));

        let mut unsafe_source = source(&"a".repeat(40));
        unsafe_source.bundle = Some(source_bundle(".lake/build/poison", b"x"));
        assert!(validate_lean_build(
            &unsafe_source,
            None,
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .unwrap_err()
        .contains("unsafe source bundle path"));
    }

    #[tokio::test]
    async fn source_bundle_application_is_atomic_and_emits_a_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let log = temp.path().join("job.log");
        tokio::fs::create_dir_all(checkout.join("Beal"))
            .await
            .unwrap();
        tokio::fs::write(checkout.join("Beal/Proof.lean"), b"old")
            .await
            .unwrap();
        tokio::fs::write(checkout.join("Beal/Obsolete.lean"), b"remove")
            .await
            .unwrap();
        let mut bundle = source_bundle("Beal/Proof.lean", b"new proof\n");
        bundle.files[0].executable = Some(true);
        bundle.deleted_paths = vec!["Beal/Obsolete.lean".to_string()];
        bundle.operations_sha256 = Some(bundle_operations_sha256(&bundle));

        apply_source_bundle(&checkout, &bundle, &log).await.unwrap();

        assert_eq!(
            tokio::fs::read(checkout.join("Beal/Proof.lean"))
                .await
                .unwrap(),
            b"new proof\n"
        );
        assert!(!checkout.join("Beal/Obsolete.lean").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(checkout.join("Beal/Proof.lean"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        let receipt = tokio::fs::read_to_string(log).await.unwrap();
        assert!(receipt.contains(&bundle.manifest_sha256));
        assert!(receipt.contains("files=1 deletions=1 bytes=10"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_bundle_deletion_rejects_symlinked_parent_directories() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let log = temp.path().join("job.log");
        tokio::fs::create_dir_all(&checkout).await.unwrap();
        let outside = temp.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        let victim = outside.join("victim");
        tokio::fs::write(&victim, b"keep me").await.unwrap();
        symlink(&outside, checkout.join("escape")).unwrap();

        let mut bundle = source_bundle("Beal/Proof.lean", b"new proof\n");
        bundle.deleted_paths = vec!["escape/victim".to_string()];
        bundle.operations_sha256 = Some(bundle_operations_sha256(&bundle));

        let error = apply_source_bundle(&checkout, &bundle, &log)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must not contain a symlink"),
            "unexpected error: {error}"
        );
        assert_eq!(tokio::fs::read(&victim).await.unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn complete_source_bundle_materializes_without_git_and_rebuilds_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("job.log");
        let mut bundled_source = source(&"a".repeat(40));
        bundled_source.repo = "https://github.com/private/lido-proof.git".to_string();
        let mut bundle = source_bundle("lean-toolchain", b"leanprover/lean4:v4.31.0\n");
        bundle.complete = true;
        bundle.manifest_sha256 = bundle_manifest_sha256_for_mode(
            &bundle
                .files
                .iter()
                .map(|file| (file.path.clone(), file.sha256.clone()))
                .collect::<Vec<_>>(),
            true,
        );
        bundled_source.bundle = Some(bundle);
        let token = CancellationToken::new();

        let checkout = ensure_checkout(temp.path(), &bundled_source, &log, &token)
            .await
            .unwrap();
        assert_ne!(
            checkout,
            checkout_dir(temp.path(), &bundled_source.repo, &bundled_source.commit)
        );
        assert_eq!(
            tokio::fs::read_to_string(checkout.join("lean-toolchain"))
                .await
                .unwrap(),
            "leanprover/lean4:v4.31.0\n"
        );
        assert!(!checkout.join(".git").exists());
        tokio::fs::create_dir_all(checkout.join(".lake/build"))
            .await
            .unwrap();
        tokio::fs::write(checkout.join(".lake/build/stale.olean"), b"stale")
            .await
            .unwrap();

        let rebuilt = ensure_checkout(temp.path(), &bundled_source, &log, &token)
            .await
            .unwrap();
        assert_eq!(checkout, rebuilt);
        assert!(!rebuilt.join(".lake").exists());
    }

    #[test]
    fn validation_rejects_local_repo_sources() {
        for bad in [
            "/srv/git/private.git",
            "file:///var/lib/sandboxed-node/checkouts",
            "./repo",
            "../repo",
            "repo",
            "C:/repos/x",
            "https://user:placeholder@example.invalid/private.git",
            "https://example.invalid/private.git?token=placeholder",
        ] {
            assert!(
                validate_lean_build(
                    &JobSource {
                        repo: bad.to_string(),
                        commit: "a".repeat(40),
                        archive: None,
                        bundle: None,
                    },
                    None,
                    &["lake".to_string(), "build".to_string()],
                    &HashMap::new(),
                    &allowlist(),
                )
                .is_err(),
                "{bad} must be rejected"
            );
        }
        for good in [
            "https://github.com/lfglabs-dev/verity.git",
            "ssh://git@github.com/lfglabs-dev/erc4337-verity.git",
            "git@github.com:lfglabs-dev/erc4337-verity.git",
        ] {
            assert!(
                validate_lean_build(
                    &JobSource {
                        repo: good.to_string(),
                        commit: "a".repeat(40),
                        archive: None,
                        bundle: None,
                    },
                    None,
                    &["lake".to_string(), "build".to_string()],
                    &HashMap::new(),
                    &allowlist(),
                )
                .is_ok(),
                "{good} must be accepted"
            );
        }
    }

    #[test]
    fn validation_rejects_bad_commits() {
        for bad in [
            "main",
            "HEAD",
            &"a".repeat(39),
            &"a".repeat(41),
            &"A".repeat(40), // uppercase hex rejected
            &"g".repeat(40), // not hex
            "",
        ] {
            let err = validate_lean_build(
                &source(bad),
                None,
                &["lake".to_string(), "build".to_string()],
                &HashMap::new(),
                &allowlist(),
            )
            .unwrap_err();
            assert!(err.contains("commit"), "bad commit {bad:?}: {err}");
        }
    }

    #[test]
    fn validation_rejects_disallowed_argv() {
        for bad in [
            vec!["bash".to_string(), "-c".to_string(), "id".to_string()],
            vec!["sh".to_string()],
            vec!["/bin/bash".to_string()],
            vec!["lakex".to_string()],
            vec![],
            vec!["".to_string()],
        ] {
            let err = validate_lean_build(
                &source(&"a".repeat(40)),
                None,
                &bad,
                &HashMap::new(),
                &allowlist(),
            )
            .unwrap_err();
            assert!(
                err.contains("command") || err.contains("argv"),
                "bad argv {bad:?}: {err}"
            );
        }
    }

    #[test]
    fn validation_rejects_command_execution_subcommands() {
        for command in [
            vec!["lake", "env", "bash"],
            vec!["lake", "env", "lake", "build"],
            vec!["lake", "exe", "tool"],
            vec!["elan", "run", "stable", "bash"],
        ] {
            let command: Vec<String> = command.into_iter().map(str::to_string).collect();
            assert!(validate_lean_build(
                &source(&"a".repeat(40)),
                None,
                &command,
                &HashMap::new(),
                &allowlist(),
            )
            .is_err());
        }
    }

    #[test]
    fn validation_rejects_env_keys_outside_allowlist() {
        let env: HashMap<String, String> = [("LD_PRELOAD".to_string(), "/tmp/evil.so".to_string())]
            .into_iter()
            .collect();
        let err = validate_lean_build(
            &source(&"a".repeat(40)),
            None,
            &["lake".to_string(), "build".to_string()],
            &env,
            &allowlist(),
        )
        .unwrap_err();
        assert!(err.contains("LD_PRELOAD"), "{err}");

        // Custom allowlists are honored.
        let custom = parse_env_allowlist("LD_PRELOAD, FOO");
        assert_eq!(
            validate_lean_build(
                &source(&"a".repeat(40)),
                None,
                &["lake".to_string(), "build".to_string()],
                &env,
                &custom,
            ),
            Ok(())
        );
    }

    #[test]
    fn validation_rejects_cwd_rel_escapes() {
        for bad in ["../etc", "a/../../b", "a;b", "-flag", "a b", "$(id)"] {
            let err = validate_lean_build(
                &source(&"a".repeat(40)),
                Some(bad),
                &["lake".to_string(), "build".to_string()],
                &HashMap::new(),
                &allowlist(),
            )
            .unwrap_err();
            assert!(err.contains("cwd_rel"), "bad cwd_rel {bad:?}: {err}");
        }
        // Leading/trailing slashes are trimmed, inner path must be clean.
        assert!(validate_lean_build(
            &source(&"a".repeat(40)),
            Some("/verity/"),
            &["lake".to_string(), "build".to_string()],
            &HashMap::new(),
            &allowlist(),
        )
        .is_ok());
    }

    #[test]
    fn checkout_dir_is_content_addressed_and_stable() {
        let work_root = Path::new("/var/lib/node/work");
        let commit = "c".repeat(40);
        let a = checkout_dir(work_root, "https://x/repo-a.git", &commit);
        let b = checkout_dir(work_root, "https://x/repo-b.git", &commit);
        let a_again = checkout_dir(work_root, "https://x/repo-a.git", &commit);
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert!(a.starts_with(work_root.join("checkouts")));
        assert_eq!(a.file_name().unwrap().to_str().unwrap(), commit);
        // 16-hex-char repo hash segment.
        let repo_segment = a.parent().unwrap().file_name().unwrap().to_str().unwrap();
        assert_eq!(repo_segment.len(), 16);
        assert!(repo_segment.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn cache_key_derivation_tracks_toolchain_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        // Neither file present: no cache key, no slot used.
        assert_eq!(derive_cache_key(dir.path()), None);

        std::fs::write(
            dir.path().join("lean-toolchain"),
            "leanprover/lean4:v4.15.0",
        )
        .unwrap();
        let toolchain_only = derive_cache_key(dir.path()).unwrap();

        std::fs::write(dir.path().join("lake-manifest.json"), "{\"packages\":[]}").unwrap();
        let with_manifest = derive_cache_key(dir.path()).unwrap();
        assert_ne!(toolchain_only, with_manifest);

        // Same contents -> same key (stable across checkouts).
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir2.path().join("lean-toolchain"),
            "leanprover/lean4:v4.15.0",
        )
        .unwrap();
        std::fs::write(dir2.path().join("lake-manifest.json"), "{\"packages\":[]}").unwrap();
        assert_eq!(derive_cache_key(dir2.path()).unwrap(), with_manifest);

        // Toolchain bump changes the key.
        std::fs::write(
            dir2.path().join("lean-toolchain"),
            "leanprover/lean4:v4.16.0",
        )
        .unwrap();
        assert_ne!(derive_cache_key(dir2.path()).unwrap(), with_manifest);
    }

    #[test]
    fn lake_cache_key_is_partitioned_by_project_and_target() {
        let dependency_key = "same-toolchain-and-manifest";
        let source_a = JobSource {
            repo: "https://example.com/a.git".to_string(),
            commit: "a".repeat(40),
            archive: None,
            bundle: None,
        };
        let source_a_next_commit = JobSource {
            repo: source_a.repo.clone(),
            commit: "b".repeat(40),
            archive: None,
            bundle: None,
        };
        let source_b = JobSource {
            repo: "https://example.com/b.git".to_string(),
            commit: "a".repeat(40),
            archive: None,
            bundle: None,
        };
        let build = vec!["lake".to_string(), "build".to_string(), "A".to_string()];
        let build_b = vec!["lake".to_string(), "build".to_string(), "B".to_string()];

        let key = project_cache_key(dependency_key, &source_a, "pkg", &build);
        assert_eq!(
            key,
            project_cache_key(dependency_key, &source_a_next_commit, "pkg", &build),
            "commits in the same project intentionally reuse incremental dependencies"
        );
        assert_ne!(
            key,
            project_cache_key(dependency_key, &source_b, "pkg", &build)
        );
        assert_ne!(
            key,
            project_cache_key(dependency_key, &source_a, "other", &build)
        );
        assert_ne!(
            key,
            project_cache_key(dependency_key, &source_a, "pkg", &build_b)
        );
    }

    #[test]
    fn artifact_patterns_reject_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../secrets", "a/../b", "/etc/passwd", "", "a//b", "./a"] {
            let err = resolve_artifact_paths(dir.path(), &[bad.to_string()]).unwrap_err();
            assert!(err.contains("unsafe"), "bad pattern {bad:?}: {err}");
            assert!(!artifact_pattern_is_safe(bad), "{bad:?}");
        }
    }

    #[test]
    fn artifact_globs_match_within_a_single_segment_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".lake/build/lib")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join(".lake/build/lib/A.olean"), b"a").unwrap();
        std::fs::write(root.join(".lake/build/lib/B.olean"), b"bb").unwrap();
        std::fs::write(root.join(".lake/build/lib/notes.txt"), b"x").unwrap();
        std::fs::write(root.join("sub/C.olean"), b"c").unwrap();

        let matched = resolve_artifact_paths(
            root,
            &[
                ".lake/build/lib/*.olean".to_string(),
                "sub/C.olean".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            matched,
            vec![
                ".lake/build/lib/A.olean".to_string(),
                ".lake/build/lib/B.olean".to_string(),
                "sub/C.olean".to_string(),
            ]
        );

        // `*` never crosses a `/`: this matches nothing (files live one level
        // deeper), and missing exact paths resolve to nothing rather than
        // erroring.
        assert!(resolve_artifact_paths(root, &[".lake/*".to_string()])
            .unwrap()
            .is_empty());
        assert!(resolve_artifact_paths(root, &["missing.olean".to_string()])
            .unwrap()
            .is_empty());
        // Directories themselves are not artifacts.
        assert!(resolve_artifact_paths(root, &["sub".to_string()])
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn collect_artifacts_hashes_and_sizes_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("out.olean"), b"hello").unwrap();
        let artifacts = collect_artifacts(dir.path(), &["*.olean".to_string()])
            .await
            .unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "out.olean");
        assert_eq!(artifacts[0].size_bytes, 5);
        assert_eq!(
            artifacts[0].sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_collection_rejects_symlinked_files_and_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.olean"), b"secret").unwrap();
        symlink(
            outside.path().join("secret.olean"),
            dir.path().join("linked.olean"),
        )
        .unwrap();
        symlink(outside.path(), dir.path().join("linked-dir")).unwrap();

        let resolved = resolve_artifact_paths(
            dir.path(),
            &["*.olean".to_string(), "linked-dir/*.olean".to_string()],
        )
        .unwrap();
        assert!(resolved.is_empty());

        let artifacts = collect_artifacts(
            dir.path(),
            &[
                "linked.olean".to_string(),
                "linked-dir/secret.olean".to_string(),
            ],
        )
        .await
        .unwrap();
        assert!(artifacts.is_empty());
    }

    #[tokio::test]
    async fn cache_copy_is_isolated_from_build_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("slot");
        let restored = dir.path().join("restored");
        tokio::fs::create_dir_all(&slot).await.unwrap();
        tokio::fs::write(slot.join("cache.bin"), b"shared")
            .await
            .unwrap();

        isolated_copy(&slot, &restored).await.unwrap();
        tokio::fs::write(restored.join("cache.bin"), b"mutated")
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read(slot.join("cache.bin")).await.unwrap(),
            b"shared"
        );
    }

    #[test]
    fn lake_dependency_cache_rejects_unverified_manifest_layouts() {
        let dir = tempfile::tempdir().unwrap();
        for manifest in [
            br#"{"lakeDir":".lake","packagesDir":".lake/deps","buildDir":".lake/build"}"#
                .as_slice(),
            br#"{"lakeDir":".lake","packagesDir":".lake/deps"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":"../deps"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":"deps"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":".lake"}"#.as_slice(),
            br#"{"lakeDir":"/tmp/lake","packagesDir":"/tmp/lake/deps"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":".lake/build"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":".lake/build/deps"}"#.as_slice(),
            br#"{"lakeDir":".lake","packagesDir":".lake/deps","buildDir":".lake/deps/root"}"#
                .as_slice(),
        ] {
            std::fs::write(dir.path().join("lake-manifest.json"), manifest).unwrap();
            assert!(configured_lake_cache_paths(dir.path()).is_none());
        }
    }

    #[tokio::test]
    async fn lake_cache_round_trip_supports_custom_packages_dir() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let source_lake = dir.path().join("source/.lake");
        let source_packages = source_lake.join("deps");
        std::fs::create_dir_all(source_packages.join("mathlib")).unwrap();
        std::fs::write(source_packages.join("mathlib/cache.bin"), b"dependency").unwrap();

        sync_lake_cache_back(
            &work_root,
            "custom-key",
            &dir.path().join("source"),
            &source_lake,
            &source_packages,
        )
        .await
        .unwrap();

        let restored_root = dir.path().join("restored");
        std::fs::create_dir_all(&restored_root).unwrap();
        let restored_lake = restored_root.join(".lake");
        let restored_packages = restored_lake.join("deps");
        restore_lake_dependency_cache(
            &lake_cache_slot(&work_root, "custom-key"),
            &restored_root,
            &restored_lake,
            &restored_packages,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(restored_packages.join("mathlib/cache.bin")).unwrap(),
            b"dependency"
        );
        assert!(!restored_lake.join("packages").exists());
    }

    #[tokio::test]
    async fn lake_cache_restore_keeps_dependencies_but_drops_root_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("slot");
        let restored = dir.path().join("restored");
        std::fs::create_dir_all(slot.join("build/lib/lean")).unwrap();
        std::fs::create_dir_all(slot.join("packages/mathlib/.lake/build/lib/lean")).unwrap();
        std::fs::write(slot.join("build/lib/lean/Root.olean"), b"stale").unwrap();
        std::fs::write(
            slot.join("packages/mathlib/.lake/build/lib/lean/Mathlib.olean"),
            b"dependency",
        )
        .unwrap();

        restore_lake_dependency_cache(&slot, dir.path(), &restored, &restored.join("packages"))
            .await
            .unwrap();

        assert!(!restored.join("build").exists());
        assert_eq!(
            tokio::fs::read(restored.join("packages/mathlib/.lake/build/lib/lean/Mathlib.olean"))
                .await
                .unwrap(),
            b"dependency"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lake_cache_restore_ignores_root_build_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("slot");
        let restored = dir.path().join("restored");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"safe").unwrap();
        symlink(&outside, slot.join("build")).unwrap();

        restore_lake_dependency_cache(&slot, dir.path(), &restored, &restored.join("packages"))
            .await
            .unwrap();

        assert!(!restored.join("build").exists());
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"safe");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lake_cache_restore_rejects_symlinked_lake_ancestor() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        let slot = dir.path().join("slot");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(slot.join("packages/mathlib")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"safe").unwrap();
        symlink(&outside, checkout.join("link")).unwrap();
        let lake_dir = checkout.join("link/.lake");
        let packages_dir = lake_dir.join("deps");

        let error = restore_lake_dependency_cache(&slot, &checkout, &lake_dir, &packages_dir)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("must not contain a symlink"));
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"safe");
        assert!(!outside.join(".lake").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lake_cache_sync_rejects_symlinked_lake_without_touching_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let lake_dir = dir.path().join("lake-link");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("build")).unwrap();
        std::fs::write(outside.join("build/keep"), b"safe").unwrap();
        symlink(&outside, &lake_dir).unwrap();

        let error = sync_lake_cache_back(
            &work_root,
            "test-key",
            dir.path(),
            &lake_dir,
            &lake_dir.join("packages"),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("must not be a symlink"));
        assert_eq!(std::fs::read(outside.join("build/keep")).unwrap(), b"safe");
        assert!(!lake_cache_slot(&work_root, "test-key").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lake_cache_sync_rejects_symlinked_packages_parent() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let lake_dir = dir.path().join("lake");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&lake_dir).unwrap();
        std::fs::create_dir_all(outside.join("deps")).unwrap();
        std::fs::write(outside.join("deps/keep"), b"safe").unwrap();
        symlink(&outside, lake_dir.join("nested")).unwrap();
        let packages_dir = lake_dir.join("nested/deps");

        let error =
            sync_lake_cache_back(&work_root, "test-key", dir.path(), &lake_dir, &packages_dir)
                .await
                .unwrap_err();

        assert!(error.to_string().contains("must not be a symlink"));
        assert_eq!(std::fs::read(outside.join("deps/keep")).unwrap(), b"safe");
        assert!(!lake_cache_slot(&work_root, "test-key").exists());
    }

    #[tokio::test]
    async fn lake_cache_sync_does_not_persist_root_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let lake_dir = dir.path().join("lake");
        std::fs::create_dir_all(lake_dir.join("build/lib/lean")).unwrap();
        std::fs::create_dir_all(lake_dir.join("custom-root/lib/lean")).unwrap();
        std::fs::create_dir_all(lake_dir.join("packages/mathlib/.lake/build/lib/lean")).unwrap();
        std::fs::write(lake_dir.join("build/lib/lean/Root.olean"), b"root").unwrap();
        std::fs::write(
            lake_dir.join("custom-root/lib/lean/CustomRoot.olean"),
            b"custom-root",
        )
        .unwrap();
        std::fs::write(
            lake_dir.join("packages/mathlib/.lake/build/lib/lean/Mathlib.olean"),
            b"dependency",
        )
        .unwrap();

        sync_lake_cache_back(
            &work_root,
            "test-key",
            dir.path(),
            &lake_dir,
            &lake_dir.join("packages"),
        )
        .await
        .unwrap();

        let slot = lake_cache_slot(&work_root, "test-key");
        assert!(!slot.join("build").exists());
        assert!(!slot.join("custom-root").exists());
        assert_eq!(
            tokio::fs::read(slot.join("packages/mathlib/.lake/build/lib/lean/Mathlib.olean"))
                .await
                .unwrap(),
            b"dependency"
        );
    }

    #[test]
    fn cached_toolchains_lists_elan_toolchain_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cached_toolchains(dir.path()).is_empty());
        let toolchains = dir.path().join("caches/elan/toolchains");
        std::fs::create_dir_all(toolchains.join("leanprover--lean4---v4.15.0")).unwrap();
        std::fs::create_dir_all(toolchains.join("leanprover--lean4---v4.16.0")).unwrap();
        std::fs::write(toolchains.join("stray-file"), b"x").unwrap();
        assert_eq!(
            cached_toolchains(dir.path()),
            vec![
                "leanprover--lean4---v4.15.0".to_string(),
                "leanprover--lean4---v4.16.0".to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn lean_runtime_readiness_requires_an_executable_lake_proxy() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("caches/elan/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let lake = bin.join("lake");
        std::fs::write(&lake, b"#!/bin/sh\n").unwrap();

        std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!lean_runtime_ready_with_path(dir.path(), None));
        std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(lean_runtime_ready_with_path(dir.path(), None));
    }

    #[cfg(unix)]
    #[test]
    fn lean_runtime_readiness_accepts_an_absolute_service_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let service_bin = tempfile::tempdir().unwrap();
        let lake = service_bin.path().join("lake");
        std::fs::write(&lake, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(lean_runtime_ready_with_path(
            dir.path(),
            Some(service_bin.path().as_os_str())
        ));
        assert!(!lean_runtime_ready_with_path(
            dir.path(),
            Some(OsStr::new("relative/bin"))
        ));
    }

    #[test]
    fn timeout_is_clamped_like_raw_command() {
        assert_eq!(clamp_timeout(Some(600), 100), 100);
        assert_eq!(clamp_timeout(Some(50), 100), 50);
        assert_eq!(clamp_timeout(None, 100), 100);
        assert_eq!(clamp_timeout(Some(0), 100), 1);
    }
}
