//! Conservative cleanup of Chromium singleton state in a leased profile.
//!
//! Chromium serializes profile access through `SingletonLock` (a symlink whose
//! target is `hostname-pid`), `SingletonCookie`, and `SingletonSocket`. When a
//! browser dies without unwinding — a SIGKILL from the runner deadline, a
//! server crash, a container restart — those entries survive and the next
//! launch refuses the profile. Removal is only safe while the pool's exclusive
//! profile lock is held, and only when the evidence says no live browser owns
//! the entries:
//!
//! - same host, dead pid: stale, remove.
//! - same host, live pid: a browser outside the pool owns the profile; keep.
//! - foreign host: keep, unless a pool ownership marker proves the previous
//!   pool-managed run left them behind (containers change hostname between
//!   runs, which would otherwise strand the profile forever).
//! - anything unrecognized (regular files, unexpected types): keep.

use std::path::{Path, PathBuf};

const SINGLETON_NAMES: [&str; 3] = ["SingletonLock", "SingletonCookie", "SingletonSocket"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingletonCleanup {
    /// No singleton entries were present.
    Clean,
    /// Stale entries were removed; the count is how many.
    Removed(usize),
    /// The lock names another host and the pool does not own it; kept.
    ForeignHost,
    /// The lock names a live process on this host; kept.
    ActiveProcess,
    /// The lock is not the symlink Chromium writes; kept.
    Unrecognized,
}

impl SingletonCleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Removed(_) => "removed",
            Self::ForeignHost => "foreign_host",
            Self::ActiveProcess => "active_process",
            Self::Unrecognized => "unrecognized",
        }
    }

    /// Whether the profile is safe to hand to a fresh browser launch.
    pub fn profile_is_launchable(self) -> bool {
        matches!(self, Self::Clean | Self::Removed(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockVerdict {
    Stale,
    ForeignHost,
    ActiveProcess,
}

/// Judge a `SingletonLock` symlink target of the form `hostname-pid`.
fn judge_lock_target(
    target: &str,
    local_hostname: &str,
    pid_is_alive: impl Fn(i32) -> bool,
) -> LockVerdict {
    // Hostnames may themselves contain '-', so split on the last one.
    let Some((host, pid)) = target.rsplit_once('-') else {
        // Not the shape Chromium writes; with the profile lock held this is a
        // dangling leftover, not a live browser.
        return LockVerdict::Stale;
    };
    let Ok(pid) = pid.parse::<i32>() else {
        return LockVerdict::Stale;
    };
    if host != local_hostname {
        return LockVerdict::ForeignHost;
    }
    if pid > 0 && pid_is_alive(pid) {
        return LockVerdict::ActiveProcess;
    }
    LockVerdict::Stale
}

#[cfg(unix)]
fn pid_is_alive(pid: i32) -> bool {
    // Signal 0 probes existence; EPERM still means the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn local_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

/// Remove a singleton entry only when it is the kind of node Chromium
/// creates (symlink or socket) — never a regular file or directory.
fn remove_singleton_entry(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let file_type = metadata.file_type();
    #[cfg(unix)]
    let removable = {
        use std::os::unix::fs::FileTypeExt;
        file_type.is_symlink() || file_type.is_socket()
    };
    #[cfg(not(unix))]
    let removable = file_type.is_symlink();
    if !removable {
        return false;
    }
    std::fs::remove_file(path).is_ok()
}

fn cleanup_with(
    profile_dir: &Path,
    owned_by_pool: bool,
    local_hostname: &str,
    pid_alive: impl Fn(i32) -> bool,
) -> SingletonCleanup {
    let lock_path = profile_dir.join("SingletonLock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(&lock_path)
                .map(|target| target.to_string_lossy().into_owned())
                .unwrap_or_default();
            match judge_lock_target(&target, local_hostname, pid_alive) {
                LockVerdict::ActiveProcess => return SingletonCleanup::ActiveProcess,
                LockVerdict::ForeignHost if !owned_by_pool => return SingletonCleanup::ForeignHost,
                LockVerdict::ForeignHost | LockVerdict::Stale => {}
            }
        }
        Ok(_) => return SingletonCleanup::Unrecognized,
        Err(_) => {}
    }
    let mut removed = 0usize;
    for name in SINGLETON_NAMES {
        if remove_singleton_entry(&profile_dir.join(name)) {
            removed += 1;
        }
    }
    if removed == 0 {
        SingletonCleanup::Clean
    } else {
        SingletonCleanup::Removed(removed)
    }
}

/// Clean stale Chromium singleton entries from an exclusively leased profile.
#[cfg(unix)]
pub fn cleanup_profile_singletons(profile_dir: &Path, owned_by_pool: bool) -> SingletonCleanup {
    cleanup_with(profile_dir, owned_by_pool, &local_hostname(), pid_is_alive)
}

#[cfg(not(unix))]
pub fn cleanup_profile_singletons(_profile_dir: &Path, _owned_by_pool: bool) -> SingletonCleanup {
    SingletonCleanup::Clean
}

fn ownership_marker_path(profile_dir: &Path) -> PathBuf {
    let name = profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("profile");
    profile_dir
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(format!(".{name}.sandboxed-chatgpt-ui.owner"))
}

/// Whether the previous pool-managed run on this profile ended without a
/// graceful cleanup. The marker lives beside the pool lock file, never
/// inside the profile itself, and contains no data.
pub fn pool_owns_singletons(profile_dir: &Path) -> bool {
    ownership_marker_path(profile_dir).is_file()
}

pub fn claim_singleton_ownership(profile_dir: &Path) {
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(ownership_marker_path(profile_dir));
}

pub fn release_singleton_ownership(profile_dir: &Path) {
    let _ = std::fs::remove_file(ownership_marker_path(profile_dir));
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    // Beyond any real pid: kernel pid_max is at most 2^22 on 64-bit Linux.
    const DEAD_PID: i32 = 0x7fff_fffe;

    fn write_singletons(profile: &Path, lock_target: &str) {
        symlink(lock_target, profile.join("SingletonLock")).unwrap();
        symlink("cookie-value", profile.join("SingletonCookie")).unwrap();
        symlink(
            "/tmp/does-not-exist.socket",
            profile.join("SingletonSocket"),
        )
        .unwrap();
    }

    #[test]
    fn judges_lock_targets_conservatively() {
        let alive = |_pid: i32| true;
        let dead = |_pid: i32| false;
        assert_eq!(
            judge_lock_target("host-42", "host", dead),
            LockVerdict::Stale
        );
        assert_eq!(
            judge_lock_target("host-42", "host", alive),
            LockVerdict::ActiveProcess
        );
        assert_eq!(
            judge_lock_target("other-42", "host", alive),
            LockVerdict::ForeignHost
        );
        assert_eq!(
            judge_lock_target("my-host-42", "my-host", dead),
            LockVerdict::Stale
        );
        assert_eq!(
            judge_lock_target("garbage", "host", alive),
            LockVerdict::Stale
        );
        assert_eq!(
            judge_lock_target("host-0", "host", alive),
            LockVerdict::Stale
        );
    }

    #[test]
    fn removes_stale_same_host_singletons() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        write_singletons(&profile, &format!("{}-{DEAD_PID}", local_hostname()));

        let outcome = cleanup_profile_singletons(&profile, false);

        assert_eq!(outcome, SingletonCleanup::Removed(3));
        assert!(outcome.profile_is_launchable());
        for name in SINGLETON_NAMES {
            assert!(!profile.join(name).exists());
        }
    }

    #[test]
    fn keeps_singletons_owned_by_a_live_process() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let live_pid = std::process::id() as i32;
        write_singletons(&profile, &format!("{}-{live_pid}", local_hostname()));

        let outcome = cleanup_profile_singletons(&profile, true);

        assert_eq!(outcome, SingletonCleanup::ActiveProcess);
        assert!(!outcome.profile_is_launchable());
        assert!(profile.join("SingletonLock").symlink_metadata().is_ok());
    }

    #[test]
    fn keeps_foreign_host_singletons_unless_pool_owned() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        write_singletons(&profile, "some-other-container-4242");

        assert_eq!(
            cleanup_profile_singletons(&profile, false),
            SingletonCleanup::ForeignHost
        );
        assert!(profile.join("SingletonLock").symlink_metadata().is_ok());

        // A previous pool-managed run crashed in a container with another
        // hostname: the ownership marker makes removal safe.
        assert_eq!(
            cleanup_profile_singletons(&profile, true),
            SingletonCleanup::Removed(3)
        );
    }

    #[test]
    fn never_removes_regular_files_with_singleton_names() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::fs::write(profile.join("SingletonLock"), "not a symlink").unwrap();
        std::fs::write(profile.join("SingletonCookie"), "data").unwrap();

        let outcome = cleanup_profile_singletons(&profile, true);

        assert_eq!(outcome, SingletonCleanup::Unrecognized);
        assert!(profile.join("SingletonLock").is_file());
        assert!(profile.join("SingletonCookie").is_file());
    }

    #[test]
    fn missing_lock_still_sweeps_dangling_cookie_and_socket() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        symlink("cookie-value", profile.join("SingletonCookie")).unwrap();

        assert_eq!(
            cleanup_profile_singletons(&profile, false),
            SingletonCleanup::Removed(1)
        );
        assert_eq!(
            cleanup_profile_singletons(&profile, false),
            SingletonCleanup::Clean
        );
    }

    #[test]
    fn ownership_marker_lives_outside_the_profile_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();

        assert!(!pool_owns_singletons(&profile));
        claim_singleton_ownership(&profile);
        assert!(pool_owns_singletons(&profile));
        assert!(std::fs::read_dir(&profile).unwrap().next().is_none());
        release_singleton_ownership(&profile);
        assert!(!pool_owns_singletons(&profile));
    }
}
