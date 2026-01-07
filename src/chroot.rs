//! Chroot workspace creation and management.
//!
//! This module provides functionality to create isolated chroot environments
//! for workspace execution using debootstrap and Linux chroot syscall.

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChrootError {
    #[error("Failed to create chroot directory: {0}")]
    DirectoryCreation(#[from] std::io::Error),

    #[error("Debootstrap failed: {0}")]
    Debootstrap(String),

    #[error("Mount operation failed: {0}")]
    Mount(String),

    #[error("Chroot command failed: {0}")]
    ChrootExecution(String),

    #[error("Unsupported distribution: {0}")]
    UnsupportedDistro(String),
}

pub type ChrootResult<T> = Result<T, ChrootError>;

/// Supported Linux distributions for chroot environments
#[derive(Debug, Clone, Copy)]
pub enum ChrootDistro {
    /// Ubuntu Noble (24.04 LTS)
    UbuntuNoble,
    /// Ubuntu Jammy (22.04 LTS)
    UbuntuJammy,
    /// Debian Bookworm (12)
    DebianBookworm,
}

impl ChrootDistro {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UbuntuNoble => "noble",
            Self::UbuntuJammy => "jammy",
            Self::DebianBookworm => "bookworm",
        }
    }

    pub fn mirror_url(&self) -> &'static str {
        match self {
            Self::UbuntuNoble | Self::UbuntuJammy => "http://archive.ubuntu.com/ubuntu",
            Self::DebianBookworm => "http://deb.debian.org/debian",
        }
    }
}

impl Default for ChrootDistro {
    fn default() -> Self {
        Self::UbuntuNoble
    }
}

/// Create a minimal chroot environment using debootstrap
pub async fn create_chroot(
    chroot_path: &Path,
    distro: ChrootDistro,
) -> ChrootResult<()> {
    // Create the chroot directory
    tokio::fs::create_dir_all(chroot_path).await?;

    tracing::info!(
        "Creating chroot at {} with distro {}",
        chroot_path.display(),
        distro.as_str()
    );

    // Run debootstrap to create minimal root filesystem
    let output = tokio::process::Command::new("debootstrap")
        .arg("--variant=minbase")
        .arg(distro.as_str())
        .arg(chroot_path)
        .arg(distro.mirror_url())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ChrootError::Debootstrap(stderr.to_string()));
    }

    tracing::info!("Chroot created successfully at {}", chroot_path.display());

    // Mount necessary filesystems
    mount_chroot_filesystems(chroot_path).await?;

    Ok(())
}

/// Mount necessary filesystems for chroot environment
async fn mount_chroot_filesystems(chroot_path: &Path) -> ChrootResult<()> {
    let mounts = vec![
        ("proc", "proc", "/proc"),
        ("sysfs", "sysfs", "/sys"),
        ("devpts", "devpts", "/dev/pts"),
        ("tmpfs", "tmpfs", "/dev/shm"),
    ];

    for (fstype, source, target) in mounts {
        let mount_point = chroot_path.join(target.trim_start_matches('/'));
        tokio::fs::create_dir_all(&mount_point).await?;

        let output = tokio::process::Command::new("mount")
            .arg("-t")
            .arg(fstype)
            .arg(source)
            .arg(&mount_point)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't fail if mount is already mounted
            if !stderr.contains("already mounted") {
                return Err(ChrootError::Mount(stderr.to_string()));
            }
        }

        tracing::debug!("Mounted {} at {}", fstype, mount_point.display());
    }

    Ok(())
}

/// Unmount filesystems from chroot environment
pub async fn unmount_chroot_filesystems(chroot_path: &Path) -> ChrootResult<()> {
    let targets = vec!["/dev/shm", "/dev/pts", "/sys", "/proc"];

    for target in targets {
        let mount_point = chroot_path.join(target.trim_start_matches('/'));

        let output = tokio::process::Command::new("umount")
            .arg(&mount_point)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't fail if not mounted
            if !stderr.contains("not mounted") {
                tracing::warn!("Failed to unmount {}: {}", mount_point.display(), stderr);
            }
        }
    }

    Ok(())
}

/// Execute a command inside a chroot environment
pub async fn execute_in_chroot(
    chroot_path: &Path,
    command: &[String],
) -> ChrootResult<std::process::Output> {
    if command.is_empty() {
        return Err(ChrootError::ChrootExecution(
            "Empty command".to_string(),
        ));
    }

    // Build the chroot command
    let output = tokio::process::Command::new("chroot")
        .arg(chroot_path)
        .args(command)
        .output()
        .await?;

    Ok(output)
}

/// Check if a chroot environment is already created and fully functional.
/// This checks both essential directories and required mount points.
pub async fn is_chroot_created(chroot_path: &Path) -> bool {
    // Check for essential directories that indicate debootstrap completed
    let essential_paths = vec!["bin", "usr", "etc", "var"];

    for path in essential_paths {
        let full_path = chroot_path.join(path);
        if !full_path.exists() {
            return false;
        }
    }

    // Also check that mount points exist and are mounted
    // This ensures the chroot is fully initialized, not just partially created
    let mount_points = vec!["proc", "sys", "dev/pts", "dev/shm"];
    for mount in mount_points {
        let mount_path = chroot_path.join(mount);
        if !mount_path.exists() {
            return false;
        }
    }

    // Verify /proc is actually mounted by checking for /proc/1 (init process)
    let proc_check = chroot_path.join("proc/1");
    if !proc_check.exists() {
        return false;
    }

    true
}

/// Clean up a chroot environment
pub async fn destroy_chroot(chroot_path: &Path) -> ChrootResult<()> {
    tracing::info!("Destroying chroot at {}", chroot_path.display());

    // Unmount filesystems first
    unmount_chroot_filesystems(chroot_path).await?;

    // Remove the chroot directory
    tokio::fs::remove_dir_all(chroot_path).await?;

    tracing::info!("Chroot destroyed successfully");

    Ok(())
}
