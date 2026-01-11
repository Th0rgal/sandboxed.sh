//! systemd-nspawn container workspace creation and management.
//!
//! This module provides functionality to create isolated container environments
//! for workspace execution using debootstrap/pacstrap and systemd-nspawn.

use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NspawnError {
    #[error("Failed to create container directory: {0}")]
    DirectoryCreation(#[from] std::io::Error),

    #[error("Failed to remove container directory: {0}")]
    DirectoryRemoval(std::io::Error),

    #[error("Debootstrap failed: {0}")]
    Debootstrap(String),

    #[error("Pacstrap failed: {0}")]
    Pacstrap(String),

    #[error("Unmount operation failed: {0}")]
    Unmount(String),

    #[error("systemd-nspawn command failed: {0}")]
    NspawnExecution(String),

    #[error("Unsupported distribution: {0}")]
    UnsupportedDistro(String),
}

pub type NspawnResult<T> = Result<T, NspawnError>;

/// Supported Linux distributions for container environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NspawnDistro {
    /// Ubuntu Noble (24.04 LTS)
    UbuntuNoble,
    /// Ubuntu Jammy (22.04 LTS)
    UbuntuJammy,
    /// Debian Bookworm (12)
    DebianBookworm,
    /// Arch Linux (base)
    ArchLinux,
}

impl NspawnDistro {
    /// Parse a distro string from API/user input.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ubuntu-noble" | "noble" => Some(Self::UbuntuNoble),
            "ubuntu-jammy" | "jammy" => Some(Self::UbuntuJammy),
            "debian-bookworm" | "bookworm" => Some(Self::DebianBookworm),
            "arch-linux" | "archlinux" | "arch" => Some(Self::ArchLinux),
            _ => None,
        }
    }

    /// Canonical API value for this distro.
    pub fn api_value(&self) -> &'static str {
        match self {
            Self::UbuntuNoble => "ubuntu-noble",
            Self::UbuntuJammy => "ubuntu-jammy",
            Self::DebianBookworm => "debian-bookworm",
            Self::ArchLinux => "arch-linux",
        }
    }

    pub fn supported_values() -> &'static [&'static str] {
        &[
            "ubuntu-noble",
            "ubuntu-jammy",
            "debian-bookworm",
            "arch-linux",
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UbuntuNoble => "noble",
            Self::UbuntuJammy => "jammy",
            Self::DebianBookworm => "bookworm",
            Self::ArchLinux => "arch-linux",
        }
    }

    pub fn mirror_url(&self) -> &'static str {
        match self {
            Self::UbuntuNoble | Self::UbuntuJammy => "http://archive.ubuntu.com/ubuntu",
            Self::DebianBookworm => "http://deb.debian.org/debian",
            Self::ArchLinux => "https://geo.mirror.pkgbuild.com/",
        }
    }
}

impl Default for NspawnDistro {
    fn default() -> Self {
        Self::UbuntuNoble
    }
}

#[derive(Debug, Clone)]
pub enum NetworkMode {
    /// Share the host network.
    Host,
    /// Use systemd-nspawn defaults (private network).
    Private,
    /// Disable veth networking.
    None,
}

#[derive(Debug, Clone)]
pub struct NspawnConfig {
    pub bind_x11: bool,
    pub display: Option<String>,
    pub network_mode: NetworkMode,
    pub ephemeral: bool,
    pub env: std::collections::HashMap<String, String>,
    pub binds: Vec<String>,
    pub capabilities: Vec<String>,
}

impl Default for NspawnConfig {
    fn default() -> Self {
        Self {
            bind_x11: false,
            display: None,
            network_mode: NetworkMode::Host,
            ephemeral: false,
            env: std::collections::HashMap::new(),
            binds: Vec::new(),
            capabilities: Vec::new(),
        }
    }
}

pub fn tailscale_enabled(env: &HashMap<String, String>) -> bool {
    env.iter().any(|(key, value)| {
        (key == "TS_AUTHKEY" || key == "TS_EXIT_NODE") && !value.trim().is_empty()
    })
}

pub fn apply_tailscale_to_config(config: &mut NspawnConfig, env: &HashMap<String, String>) {
    if !tailscale_enabled(env) {
        return;
    }

    config.network_mode = NetworkMode::Private;

    if !config.capabilities.iter().any(|cap| cap == "CAP_NET_ADMIN") {
        config.capabilities.push("CAP_NET_ADMIN".to_string());
    }

    if Path::new("/dev/net/tun").exists() && !config.binds.iter().any(|bind| bind == "/dev/net/tun")
    {
        config.binds.push("/dev/net/tun".to_string());
    }
}

pub fn tailscale_nspawn_extra_args(env: &HashMap<String, String>) -> Vec<String> {
    if !tailscale_enabled(env) {
        return Vec::new();
    }

    let mut args = Vec::new();
    args.push("--network-veth".to_string());
    args.push("--capability=CAP_NET_ADMIN".to_string());
    if Path::new("/dev/net/tun").exists() {
        args.push("--bind=/dev/net/tun".to_string());
    }
    args
}

/// Create a minimal container environment using debootstrap or pacstrap.
pub async fn create_container(path: &Path, distro: NspawnDistro) -> NspawnResult<()> {
    // Create the container directory
    tokio::fs::create_dir_all(path).await?;

    tracing::info!(
        "Creating container at {} with distro {}",
        path.display(),
        distro.as_str()
    );

    match distro {
        NspawnDistro::ArchLinux => create_arch_container(path).await?,
        _ => create_debootstrap_container(path, distro).await?,
    }

    tracing::info!("Container created successfully at {}", path.display());

    Ok(())
}

async fn create_debootstrap_container(path: &Path, distro: NspawnDistro) -> NspawnResult<()> {
    let output = tokio::process::Command::new("debootstrap")
        .arg("--variant=minbase")
        .arg(distro.as_str())
        .arg(path)
        .arg(distro.mirror_url())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                NspawnError::Debootstrap(
                    "debootstrap not found. Install debootstrap on the host.".to_string(),
                )
            } else {
                NspawnError::Debootstrap(e.to_string())
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NspawnError::Debootstrap(stderr.to_string()));
    }

    Ok(())
}

async fn create_arch_container(path: &Path) -> NspawnResult<()> {
    let pacman_conf = std::env::temp_dir().join("open_agent_pacman.conf");
    let pacman_conf_contents = r#"[options]
Architecture = auto
SigLevel = Never

[core]
Include = /etc/pacman.d/mirrorlist

[extra]
Include = /etc/pacman.d/mirrorlist
"#;
    tokio::fs::write(&pacman_conf, pacman_conf_contents).await?;

    let output = tokio::process::Command::new("pacstrap")
        .arg("-C")
        .arg(&pacman_conf)
        .arg("-c")
        .arg(path)
        .arg("base")
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                NspawnError::Pacstrap(
                    "pacstrap not found. Install arch-install-scripts (and pacman) on the host."
                        .to_string(),
                )
            } else {
                NspawnError::Pacstrap(e.to_string())
            }
        })?;

    let _ = tokio::fs::remove_file(&pacman_conf).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NspawnError::Pacstrap(stderr.to_string()));
    }

    Ok(())
}

async fn unmount_if_present(root: &Path, target: &str) -> NspawnResult<()> {
    let mount_point = root.join(target.trim_start_matches('/'));
    if !mount_point.exists() {
        return Ok(());
    }

    let output = tokio::process::Command::new("umount")
        .arg(&mount_point)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not mounted") {
            return Err(NspawnError::Unmount(stderr.to_string()));
        }
    }

    Ok(())
}

/// Execute a command inside a container using systemd-nspawn.
pub async fn execute_in_container(
    path: &Path,
    command: &[String],
    config: &NspawnConfig,
) -> NspawnResult<std::process::Output> {
    if command.is_empty() {
        return Err(NspawnError::NspawnExecution("Empty command".to_string()));
    }

    let mut cmd = tokio::process::Command::new("systemd-nspawn");
    cmd.arg("-D").arg(path);
    cmd.arg("--quiet");
    // Disable timezone bind-mount (minbase containers lack /usr/share/zoneinfo)
    cmd.arg("--timezone=off");

    match config.network_mode {
        NetworkMode::Host => {}
        NetworkMode::Private => {
            cmd.arg("--network-veth");
        }
        NetworkMode::None => {
            cmd.arg("--private-network");
        }
    }

    if config.ephemeral {
        cmd.arg("--ephemeral");
    }

    for capability in &config.capabilities {
        if capability.trim().is_empty() {
            continue;
        }
        cmd.arg(format!("--capability={}", capability));
    }

    for bind in &config.binds {
        if bind.trim().is_empty() {
            continue;
        }
        let has_dest = bind.contains(':');
        if has_dest || Path::new(bind).exists() {
            cmd.arg(format!("--bind={}", bind));
        }
    }

    if config.bind_x11 && Path::new("/tmp/.X11-unix").exists() {
        cmd.arg("--bind=/tmp/.X11-unix");
    }

    if let Some(display) = config.display.as_ref() {
        cmd.arg(format!("--setenv=DISPLAY={}", display));
    }

    if !config.env.is_empty() {
        for (key, value) in &config.env {
            if key.trim().is_empty() {
                continue;
            }
            cmd.arg(format!("--setenv={}={}", key, value));
        }
    }

    cmd.args(command);

    let output = cmd.output().await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            NspawnError::NspawnExecution(
                "systemd-nspawn not found. Install systemd-container on the host.".to_string(),
            )
        } else {
            NspawnError::NspawnExecution(e.to_string())
        }
    })?;

    Ok(output)
}

/// Check if a container environment is already created and functional.
pub fn is_container_ready(path: &Path) -> bool {
    let essential_paths = vec!["bin", "usr", "etc", "var"];
    for rel in essential_paths {
        if !path.join(rel).exists() {
            return false;
        }
    }
    true
}

fn parse_os_release_value(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    if !line.starts_with(&prefix) {
        return None;
    }
    let value = line[prefix.len()..].trim().trim_matches('"');
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Detect the distro of an existing container by inspecting /etc/os-release.
pub async fn detect_container_distro(path: &Path) -> Option<NspawnDistro> {
    let os_release_path = path.join("etc/os-release");
    let contents = tokio::fs::read_to_string(os_release_path).await.ok()?;
    let mut id: Option<String> = None;
    let mut codename: Option<String> = None;

    for line in contents.lines() {
        if id.is_none() {
            id = parse_os_release_value(line, "ID");
        }
        if codename.is_none() {
            codename = parse_os_release_value(line, "VERSION_CODENAME");
        }
    }

    match id.as_deref()? {
        "ubuntu" => match codename.as_deref()? {
            "noble" => Some(NspawnDistro::UbuntuNoble),
            "jammy" => Some(NspawnDistro::UbuntuJammy),
            _ => None,
        },
        "debian" => match codename.as_deref()? {
            "bookworm" => Some(NspawnDistro::DebianBookworm),
            _ => None,
        },
        "arch" | "archlinux" => Some(NspawnDistro::ArchLinux),
        _ => None,
    }
}

/// Clean up a container environment.
pub async fn destroy_container(path: &Path) -> NspawnResult<()> {
    tracing::info!("Destroying container at {}", path.display());

    if !path.exists() {
        tracing::info!(
            "Container path {} does not exist, nothing to destroy",
            path.display()
        );
        return Ok(());
    }

    // Clean up any legacy mounts if present (best effort).
    let _ = unmount_if_present(path, "/dev/shm").await;
    let _ = unmount_if_present(path, "/dev/pts").await;
    let _ = unmount_if_present(path, "/sys").await;
    let _ = unmount_if_present(path, "/proc").await;

    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(NspawnError::DirectoryRemoval(e)),
    }

    tracing::info!("Container destroyed successfully");

    Ok(())
}
