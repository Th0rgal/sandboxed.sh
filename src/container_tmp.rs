//! Disk-backed `/tmp` for container workspaces.
//!
//! systemd-nspawn mounts a tmpfs on `/tmp` inside every container, sized at 10%
//! of host RAM (6.3G on a 64G box). Three consequences, all of which we hit in
//! production on 2026-08-05:
//!
//! 1. **It is RAM.** Mission scratch — multi-GB audit trees, cert runs, Ask
//!    sandbox copies — competes with the host and with `missions.slice`'s own
//!    memory caps. Sizing it generously is not free the way disk is: eleven
//!    containers at the default already oversubscribe a 62G box.
//! 2. **Filling it fails quietly.** Writes return ENOSPC, but scripts that
//!    don't check leave 0-byte logs and empty directories behind. Two
//!    containers sat wedged at zero bytes free for ~2 days before anyone
//!    noticed, because nothing crashed.
//! 3. **It is invisible from the host.** The host-side `containers/<ws>/tmp` is
//!    an empty shadow, so `df`, the disk watcher, and the workspace GC all
//!    report reassuring numbers while the container has nothing left.
//!
//! When [`ROOT_ENV`] is set, each container's `/tmp` is bind-mounted from a
//! per-workspace directory under that root instead. The margin becomes whatever
//! the backing filesystem has, the RAM cost drops to zero, and — because it is
//! now an ordinary host path — both the disk watcher and [`sweep_root`] below
//! can finally see it.
//!
//! Unset (default) = unchanged tmpfs behaviour, so shipping this is a no-op
//! until the env is enabled (Docker installs without systemd PID 1 keep the
//! stock nspawn path).

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Host directory backing every container `/tmp`. Unset = feature disabled.
pub const ROOT_ENV: &str = "SANDBOXED_SH_CONTAINER_TMP_ROOT";

/// How long an untouched scratch entry survives a sweep.
pub const RETENTION_ENV: &str = "SANDBOXED_SH_CONTAINER_TMP_RETENTION_HOURS";

/// Conservative by default: a build can legitimately leave a tree untouched for
/// a long stretch while it works elsewhere, and the disk-backed margin means
/// there is no pressure to reclaim aggressively.
pub const DEFAULT_RETENTION_HOURS: u64 = 72;

/// Entries nspawn mounts *into* `/tmp` at container start. They are mount
/// points, not scratch: removing them fails with EBUSY at best, and takes the
/// container's X11 socket with it at worst.
const RUNTIME_MOUNTPOINTS: &[&str] = &[".X11-unix"];

/// Depth cap for the staleness walk. A pathological tree (symlink loops are
/// excluded, but deeply nested `node_modules` are not) should cost bounded work
/// and, when the cap is hit, be treated as *in use* rather than deleted.
const MAX_WALK_DEPTH: usize = 64;

/// Resolve the configured root, or `None` when the feature is disabled.
pub fn root() -> Option<PathBuf> {
    std::env::var(ROOT_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from)
}

/// Reduce a workspace name to a single safe path component.
///
/// Workspace names reach us from user-facing configuration, so they can contain
/// separators, `..`, or unicode. The result is only ever joined onto the
/// configured root, and must not be able to escape it.
pub fn sanitize_component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(['-', '.'].as_slice()).to_string();
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed
    }
}

/// Per-workspace `/tmp` backing directory under `root`.
pub fn dir_in(root: &Path, workspace_name: &str) -> PathBuf {
    root.join(sanitize_component(workspace_name))
}

/// Per-workspace `/tmp` backing directory, or `None` when disabled.
pub fn dir_for(workspace_name: &str) -> Option<PathBuf> {
    root().map(|root| dir_in(&root, workspace_name))
}

/// Reset a workspace's `/tmp` backing directory ahead of a container start.
///
/// A container leader start *is* that container's boot, and `/tmp` is empty on
/// boot — matching tmpfs semantics here keeps the disk-backed variant a drop-in
/// and stops a workspace that cycles often from accumulating forever. A failed
/// clear is not fatal: a stale tree is still a better `/tmp` than none.
pub fn prepare(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path = %dir.display(),
                %error,
                "Could not clear the container /tmp directory; reusing it as-is"
            );
        }
    }
    fs::create_dir_all(dir)?;
    // Sticky + world-writable, exactly as `/tmp` is expected to be: the
    // container runs its own uid space and unprivileged tooling writes here.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o1777))
}

/// Configured retention for stale scratch.
pub fn retention() -> Duration {
    let hours = std::env::var(RETENTION_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|hours| (1..=24 * 365).contains(hours))
        .unwrap_or(DEFAULT_RETENTION_HOURS);
    Duration::from_secs(hours * 3600)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    pub scanned: usize,
    pub removed: usize,
    pub bytes_freed: u64,
    pub errors: usize,
}

/// Prune scratch entries that nothing has touched since `cutoff`.
///
/// Only the *contents* of each per-workspace directory are candidates — the
/// workspace directory itself stays, because a running container has it
/// bind-mounted onto `/tmp`.
pub fn sweep_root(root: &Path, cutoff: SystemTime) -> SweepStats {
    let mut stats = SweepStats::default();
    let Ok(workspaces) = fs::read_dir(root) else {
        return stats;
    };
    for workspace in workspaces.flatten() {
        if workspace
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            sweep_workspace_dir(&workspace.path(), cutoff, &mut stats);
        }
    }
    stats
}

fn sweep_workspace_dir(dir: &Path, cutoff: SystemTime, stats: &mut SweepStats) {
    let Ok(entries) = fs::read_dir(dir) else {
        stats.errors += 1;
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if RUNTIME_MOUNTPOINTS
            .iter()
            .any(|reserved| name == std::ffi::OsStr::new(reserved))
        {
            continue;
        }
        let path = entry.path();
        stats.scanned += 1;
        if !tree_is_idle_since(&path, cutoff, 0) {
            continue;
        }
        let bytes = tree_size(&path, 0);
        // `symlink_metadata`, not `is_dir()`: a symlink pointing at a directory
        // answers `true` to the latter, and `remove_dir_all` refuses to unlink
        // a symlink — the entry would fail every sweep forever.
        let is_real_dir = fs::symlink_metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        let removed = if is_real_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match removed {
            Ok(()) => {
                stats.removed += 1;
                stats.bytes_freed += bytes;
            }
            Err(error) => {
                stats.errors += 1;
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "Could not remove stale container /tmp entry"
                );
            }
        }
    }
}

/// `true` when nothing in the tree has been modified since `cutoff`.
///
/// Deliberately biased toward keeping: anything we cannot stat, cannot read, or
/// that nests deeper than [`MAX_WALK_DEPTH`] counts as active. A top-level
/// directory's own mtime is not enough — a long build writes deep inside a tree
/// whose root was last touched when it was created.
fn tree_is_idle_since(path: &Path, cutoff: SystemTime, depth: usize) -> bool {
    if depth > MAX_WALK_DEPTH {
        return false;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    if modified >= cutoff {
        return false;
    }
    // Never traverse a symlink: it can point outside the sweep root, and its
    // own mtime is the only thing we are entitled to judge it by.
    if !meta.is_dir() {
        return true;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        if !tree_is_idle_since(&entry.path(), cutoff, depth + 1) {
            return false;
        }
    }
    true
}

fn tree_size(path: &Path, depth: usize) -> u64 {
    if depth > MAX_WALK_DEPTH {
        return 0;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| tree_size(&entry.path(), depth + 1))
        .sum()
}

/// Sweep the configured root, if the feature is enabled. Called from the
/// mission-workspace GC loop so all periodic disk hygiene shares one cadence.
pub async fn sweep_if_enabled() {
    let Some(root) = root() else {
        return;
    };
    let retention = retention();
    let started = std::time::Instant::now();
    let stats = tokio::task::spawn_blocking(move || {
        let cutoff = SystemTime::now()
            .checked_sub(retention)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        sweep_root(&root, cutoff)
    })
    .await
    .unwrap_or_default();

    if stats.removed > 0 || stats.errors > 0 {
        tracing::info!(
            scanned = stats.scanned,
            removed = stats.removed,
            bytes_freed = stats.bytes_freed,
            errors = stats.errors,
            retention_hours = retention.as_secs() / 3600,
            duration_ms = started.elapsed().as_millis() as u64,
            "container /tmp sweep finished",
        );
    } else {
        tracing::debug!(
            scanned = stats.scanned,
            duration_ms = started.elapsed().as_millis() as u64,
            "container /tmp sweep found nothing to reclaim",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn touch(path: &Path, age: Duration) {
        fs::write(path, b"x").unwrap();
        set_age(path, age);
    }

    /// Age a symlink itself. `File::set_times` follows links, so the only way
    /// to stamp the link (rather than its target) is `lutimes`.
    fn set_symlink_age(path: &Path, age: Duration) {
        use std::os::unix::ffi::OsStrExt;

        let when = SystemTime::now() - age;
        let secs = when
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as libc::time_t;
        let times = [
            libc::timeval {
                tv_sec: secs,
                tv_usec: 0,
            },
            libc::timeval {
                tv_sec: secs,
                tv_usec: 0,
            },
        ];
        let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::lutimes(raw.as_ptr(), times.as_ptr()) };
        assert_eq!(rc, 0, "lutimes failed for {}", path.display());
    }

    fn set_age(path: &Path, age: Duration) {
        let when = SystemTime::now() - age;
        let times = fs::FileTimes::new().set_modified(when).set_accessed(when);
        // Directories need an explicit open; `File::options` handles both.
        let file = fs::File::options()
            .write(!path.is_dir())
            .read(true)
            .open(path)
            .unwrap();
        file.set_times(times).unwrap();
    }

    #[test]
    fn sanitize_component_cannot_escape_the_root() {
        assert_eq!(sanitize_component("../../etc"), "etc");
        assert_eq!(sanitize_component("a/b"), "a-b");
        assert_eq!(sanitize_component(".."), "workspace");
        assert_eq!(sanitize_component(""), "workspace");
        assert_eq!(
            sanitize_component("verity-integration-a"),
            "verity-integration-a"
        );

        let root = Path::new("/srv/tmp");
        assert_eq!(dir_in(root, "../../etc"), root.join("etc"));
    }

    #[test]
    fn prepare_creates_a_sticky_world_writable_dir_and_clears_stale_content() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("assistant");
        fs::create_dir_all(dir.join("leftover")).unwrap();

        prepare(&dir).unwrap();

        assert!(dir.is_dir());
        assert!(
            !dir.join("leftover").exists(),
            "container /tmp starts empty"
        );
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o1777);
    }

    #[test]
    fn sweep_removes_idle_trees_and_keeps_active_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("dumbcontracts");
        fs::create_dir_all(&workspace).unwrap();

        // Stale: the whole tree is old.
        let stale = workspace.join("pr25-cert.8TywLP");
        fs::create_dir_all(&stale).unwrap();
        touch(&stale.join("out.log"), Duration::from_secs(96 * 3600));
        set_age(&stale, Duration::from_secs(96 * 3600));

        // Active: the directory itself looks old, but a build is writing deep
        // inside it. This is the case a top-level mtime check gets wrong.
        let active = workspace.join("ep1039-audit.j8LCqV");
        fs::create_dir_all(active.join("build")).unwrap();
        touch(&active.join("build/fresh.o"), Duration::from_secs(60));
        set_age(&active.join("build"), Duration::from_secs(96 * 3600));
        set_age(&active, Duration::from_secs(96 * 3600));

        let cutoff = SystemTime::now() - Duration::from_secs(72 * 3600);
        let stats = sweep_root(tmp.path(), cutoff);

        assert!(!stale.exists(), "idle scratch is reclaimed");
        assert!(active.exists(), "a tree with fresh contents is left alone");
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.scanned, 2);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn sweep_never_touches_runtime_mountpoints() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("assistant");
        let x11 = workspace.join(".X11-unix");
        fs::create_dir_all(&x11).unwrap();
        set_age(&x11, Duration::from_secs(30 * 24 * 3600));

        let stats = sweep_root(tmp.path(), SystemTime::now());

        assert!(x11.exists(), ".X11-unix is a mount point, not scratch");
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.scanned, 0);
    }

    #[test]
    fn sweep_reclaims_a_stale_symlink_without_following_it() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("misc");
        fs::create_dir_all(&workspace).unwrap();

        // Freshly-written data outside the sweep root, reachable only through a
        // stale link inside it. Missions really do this: we saw
        // `/tmp/b3v2 -> /workspaces/mission-a163ed85/b3v2-storage` in
        // production. Following the link would judge the entry by the target's
        // activity and, worse, delete someone else's files.
        let outside = tmp.path().join("outside-target");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"important").unwrap();

        let link = workspace.join("b3v2");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        set_symlink_age(&link, Duration::from_secs(96 * 3600));

        let cutoff = SystemTime::now() - Duration::from_secs(72 * 3600);
        let stats = sweep_root(tmp.path(), cutoff);

        assert!(
            fs::symlink_metadata(&link).is_err(),
            "the stale link itself is reclaimed"
        );
        assert!(
            outside.join("keep.txt").exists(),
            "the sweep must not follow a link out of the root"
        );
        assert_eq!(stats.removed, 1);
    }

    #[test]
    fn a_fresh_symlink_keeps_its_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("misc");
        let stale = workspace.join("mission-scratch");
        fs::create_dir_all(&stale).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), stale.join("live")).unwrap();
        set_age(&stale, Duration::from_secs(96 * 3600));

        let cutoff = SystemTime::now() - Duration::from_secs(72 * 3600);
        let stats = sweep_root(tmp.path(), cutoff);

        assert!(
            stale.exists(),
            "a just-created link means the tree is in use"
        );
        assert_eq!(stats.removed, 0);
    }
}
