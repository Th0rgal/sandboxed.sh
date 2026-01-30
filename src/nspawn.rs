//! systemd-nspawn container workspace creation and management.
//!
//! This module provides functionality to create isolated container environments
//! for workspace execution using debootstrap/pacstrap and systemd-nspawn.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;
use tokio::process::Command;

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

fn env_var_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        ),
        Err(_) => default,
    }
}

fn command_on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            if dir.trim().is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join(cmd);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

/// Returns true if systemd-nspawn is available on this host.
pub fn nspawn_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if Path::new("/usr/bin/systemd-nspawn").is_file() {
        return true;
    }
    command_on_path("systemd-nspawn")
}

/// Whether we should allow container workspaces to fall back to host execution.
/// Default: enabled on non-Linux hosts, disabled on Linux unless explicitly set.
pub fn allow_container_fallback() -> bool {
    let default = !cfg!(target_os = "linux");
    env_var_bool("SANDBOXED_SH_ALLOW_CONTAINER_FALLBACK", default)
}

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

/// Return the cache directory for rootfs tarballs.
/// Defaults to `{WORKING_DIR}/.sandboxed-sh/cache` (sibling of `containers/`).
fn rootfs_cache_dir() -> PathBuf {
    let working_dir = std::env::var("WORKING_DIR").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(working_dir)
        .join(".sandboxed-sh")
        .join("cache")
}

/// Return the path for a cached rootfs tarball for the given distro.
fn rootfs_cache_path(distro: NspawnDistro) -> PathBuf {
    rootfs_cache_dir().join(format!("rootfs-{}.tar", distro.as_str()))
}

/// Try to restore a container from a cached rootfs tarball.
/// Returns Ok(true) if cache hit, Ok(false) if no cache.
async fn restore_from_cache(path: &Path, distro: NspawnDistro) -> NspawnResult<bool> {
    let cache_path = rootfs_cache_path(distro);
    if !cache_path.exists() {
        return Ok(false);
    }

    tracing::info!(
        "Restoring container from cache: {} -> {}",
        cache_path.display(),
        path.display()
    );

    // Append to the build log so the dashboard can show progress.
    let build_log_path = build_log_path_for(path);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&build_log_path)
    {
        use std::io::Write;
        let _ = writeln!(f, "[sandboxed] Restoring from cached rootfs...");
    }

    let output = Command::new("tar")
        .arg("xf")
        .arg(&cache_path)
        .arg("-C")
        .arg(path)
        .output()
        .await
        .map_err(|e| NspawnError::Debootstrap(format!("tar extract failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "Cache restore failed (will fall back to debootstrap): {}",
            stderr
        );
        // Clean up partial extraction
        let _ = tokio::fs::remove_dir_all(path).await;
        let _ = tokio::fs::create_dir_all(path).await;
        return Ok(false);
    }

    tracing::info!("Container restored from cache successfully");
    Ok(true)
}

/// Save a freshly-created container rootfs to the cache for reuse.
async fn save_to_cache(path: &Path, distro: NspawnDistro) {
    let cache_path = rootfs_cache_path(distro);
    let cache_dir = rootfs_cache_dir();

    if let Err(e) = tokio::fs::create_dir_all(&cache_dir).await {
        tracing::warn!("Failed to create cache dir {}: {}", cache_dir.display(), e);
        return;
    }

    // Write to a temp file first, then rename for atomicity.
    let tmp_path = cache_path.with_extension("tar.tmp");

    tracing::info!(
        "Caching rootfs: {} -> {}",
        path.display(),
        cache_path.display()
    );

    let result = Command::new("tar")
        .arg("cf")
        .arg(&tmp_path)
        .arg("-C")
        .arg(path)
        .arg(".")
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            if let Err(e) = tokio::fs::rename(&tmp_path, &cache_path).await {
                tracing::warn!("Failed to finalize cache file: {}", e);
                let _ = tokio::fs::remove_file(&tmp_path).await;
            } else {
                tracing::info!("Rootfs cached at {}", cache_path.display());
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("Cache tar creation failed: {}", stderr);
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
        Err(e) => {
            tracing::warn!("Failed to run tar for caching: {}", e);
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
    }
}

/// Create a minimal container environment using debootstrap or pacstrap.
/// Uses a cached rootfs tarball when available to avoid repeating slow bootstraps.
pub async fn create_container(path: &Path, distro: NspawnDistro) -> NspawnResult<()> {
    // Create the container directory
    tokio::fs::create_dir_all(path).await?;

    tracing::info!(
        "Creating container at {} with distro {}",
        path.display(),
        distro.as_str()
    );

    // Try cache first
    if restore_from_cache(path, distro).await? {
        tracing::info!("Container created from cache at {}", path.display());
        return Ok(());
    }

    // No cache — bootstrap from scratch
    match distro {
        NspawnDistro::ArchLinux => create_arch_container(path).await?,
        _ => create_debootstrap_container(path, distro).await?,
    }

    tracing::info!("Container created successfully at {}", path.display());

    // Cache the fresh rootfs in the background for next time
    save_to_cache(path, distro).await;

    Ok(())
}

async fn create_debootstrap_container(path: &Path, distro: NspawnDistro) -> NspawnResult<()> {
    // Stream debootstrap output to a build log file so the dashboard can show progress.
    // The log is stored as a sibling file (e.g. /root/.sandboxed-sh/containers/alex.build.log)
    // because the container filesystem doesn't exist yet during debootstrap.
    let build_log_path = build_log_path_for(path);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&build_log_path)
        .ok();

    let mut child = tokio::process::Command::new("debootstrap")
        .arg("--variant=minbase")
        .arg(distro.as_str())
        .arg(path)
        .arg(distro.mirror_url())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                NspawnError::Debootstrap(
                    "debootstrap not found. Install debootstrap on the host.".to_string(),
                )
            } else {
                NspawnError::Debootstrap(e.to_string())
            }
        })?;

    // Stream stdout and stderr to the build log file in real-time
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let log_for_stdout = log_file.as_ref().and_then(|f| f.try_clone().ok());
    let log_for_stderr = log_file.as_ref().and_then(|f| f.try_clone().ok());

    let stdout_handle = tokio::spawn(async move {
        let mut collected = Vec::new();
        if let Some(stdout) = stdout {
            use tokio::io::AsyncReadExt;
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        collected.extend_from_slice(&buf[..n]);
                        if let Some(ref mut f) = log_for_stdout.as_ref() {
                            use std::io::Write;
                            let _ = f.write_all(&buf[..n]);
                            let _ = f.flush();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        collected
    });

    let stderr_handle = tokio::spawn(async move {
        let mut collected = Vec::new();
        if let Some(stderr) = stderr {
            use tokio::io::AsyncReadExt;
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        collected.extend_from_slice(&buf[..n]);
                        if let Some(ref mut f) = log_for_stderr.as_ref() {
                            use std::io::Write;
                            let _ = f.write_all(&buf[..n]);
                            let _ = f.flush();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        collected
    });

    let status = child
        .wait()
        .await
        .map_err(|e| NspawnError::Debootstrap(e.to_string()))?;
    let _ = stdout_handle.await;
    let stderr_bytes = stderr_handle.await.unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        return Err(NspawnError::Debootstrap(stderr.to_string()));
    }

    // Copy the build log into the container's var/log/ so the dashboard can read it
    // from the standard init-log path after debootstrap completes.
    let container_log_dir = path.join("var/log");
    if container_log_dir.exists() {
        let container_log = container_log_dir.join("sandboxed-init.log");
        let _ = std::fs::copy(&build_log_path, &container_log);
    }

    Ok(())
}

/// Returns the path to the build log file stored as a sibling to the container directory.
/// This is used during debootstrap when the container filesystem doesn't exist yet.
pub(crate) fn build_log_path_for(container_path: &Path) -> std::path::PathBuf {
    let mut log_path = container_path.to_path_buf().into_os_string();
    log_path.push(".build.log");
    std::path::PathBuf::from(log_path)
}

async fn create_arch_container(path: &Path) -> NspawnResult<()> {
    let pacman_conf = std::env::temp_dir().join("sandboxed_sh_pacman.conf");
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

    let tailscale_active = tailscale_enabled(&config.env);
    let should_bind_dns = matches!(config.network_mode, NetworkMode::Host)
        || (matches!(config.network_mode, NetworkMode::Private) && !tailscale_active);
    if should_bind_dns && Path::new("/etc/resolv.conf").exists() {
        cmd.arg("--bind-ro=/etc/resolv.conf");
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

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let machine_name = name.trim();
        if !machine_name.is_empty() {
            let machinectl = if Path::new("/usr/bin/machinectl").exists() {
                "/usr/bin/machinectl"
            } else {
                "machinectl"
            };
            match Command::new(machinectl)
                .arg("terminate")
                .arg(machine_name)
                .output()
                .await
            {
                Ok(output) => {
                    if output.status.success() {
                        tracing::info!(
                            machine = machine_name,
                            "Terminated running container before removal"
                        );
                    } else {
                        tracing::debug!(
                            machine = machine_name,
                            status = %output.status,
                            "machinectl terminate returned non-zero"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        machine = machine_name,
                        error = %e,
                        "Failed to terminate container before removal"
                    );
                }
            }
        }
    }

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
