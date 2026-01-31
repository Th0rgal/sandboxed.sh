//! Workspace execution layer.
//!
//! Spawns processes inside a workspace execution context so that:
//! - Host workspaces execute directly on the host
//! - Container workspaces execute via systemd-nspawn in the container filesystem
//!
//! This is used for per-workspace Claude Code and OpenCode execution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use tokio::process::{Child, Command};

use crate::nspawn;
use crate::workspace::{use_nspawn_for_workspace, TailscaleMode, Workspace, WorkspaceType};

#[derive(Debug, Clone)]
pub struct WorkspaceExec {
    pub workspace: Workspace,
}

impl WorkspaceExec {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    /// Translate a host path to a container-relative path.
    ///
    /// For container workspaces using nspawn/nsenter, paths must be relative to the container
    /// filesystem root, not the host. This translates paths like:
    ///   /root/.sandboxed-sh/containers/minecraft/workspaces/mission-xxx/.claude/settings.json
    /// to:
    ///   /workspaces/mission-xxx/.claude/settings.json
    ///
    /// For host workspaces or fallback mode, returns the original path unchanged.
    pub fn translate_path_for_container(&self, path: &Path) -> String {
        if self.workspace.workspace_type != WorkspaceType::Container {
            return path.to_string_lossy().to_string();
        }
        if !use_nspawn_for_workspace(&self.workspace) {
            return path.to_string_lossy().to_string();
        }
        // Translate to container-relative path
        self.rel_path_in_container(path)
    }

    fn rel_path_in_container(&self, cwd: &Path) -> String {
        let root = &self.workspace.path;
        let rel = cwd.strip_prefix(root).unwrap_or_else(|_| Path::new(""));
        if rel.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rel.to_string_lossy())
        }
    }

    fn build_env(&self, extra_env: HashMap<String, String>) -> HashMap<String, String> {
        let mut merged = self.workspace.env_vars.clone();
        merged.extend(extra_env);
        merged
            .entry("SANDBOXED_SH_WORKSPACE_TYPE".to_string())
            .or_insert_with(|| self.workspace.workspace_type.as_str().to_string());
        if self.workspace.workspace_type == WorkspaceType::Container {
            if let Some(name) = self.workspace.path.file_name().and_then(|n| n.to_str()) {
                if !name.trim().is_empty() {
                    merged
                        .entry("SANDBOXED_SH_WORKSPACE_NAME".to_string())
                        .or_insert_with(|| name.to_string());
                }
            }
            // Ensure container processes use container-local XDG paths instead of host defaults.
            merged
                .entry("HOME".to_string())
                .or_insert_with(|| "/root".to_string());
            merged
                .entry("XDG_CONFIG_HOME".to_string())
                .or_insert_with(|| "/root/.config".to_string());
            merged
                .entry("XDG_DATA_HOME".to_string())
                .or_insert_with(|| "/root/.local/share".to_string());
            merged
                .entry("XDG_STATE_HOME".to_string())
                .or_insert_with(|| "/root/.local/state".to_string());
            merged
                .entry("XDG_CACHE_HOME".to_string())
                .or_insert_with(|| "/root/.cache".to_string());
        }
        if self.workspace.workspace_type == WorkspaceType::Container
            && !use_nspawn_for_workspace(&self.workspace)
        {
            merged
                .entry("SANDBOXED_SH_CONTAINER_FALLBACK".to_string())
                .or_insert_with(|| "1".to_string());
        }
        merged
    }

    fn shell_escape(value: &str) -> String {
        if value.is_empty() {
            return "''".to_string();
        }
        let mut escaped = String::new();
        escaped.push('\'');
        for ch in value.chars() {
            if ch == '\'' {
                escaped.push_str("'\"'\"'");
            } else {
                escaped.push(ch);
            }
        }
        escaped.push('\'');
        escaped
    }

    /// Build a shell command with optional env var exports.
    /// When `env` is provided, all env vars are exported before running the program.
    /// This is needed for nsenter where env vars don't propagate into the container.
    fn build_shell_command_with_env(
        rel_cwd: &str,
        program: &str,
        args: &[String],
        env: Option<&HashMap<String, String>>,
    ) -> String {
        let mut cmd = String::new();

        // Export env vars inside the shell command so they're available in the container
        if let Some(env) = env {
            for (k, v) in env {
                if k.trim().is_empty() {
                    continue;
                }
                cmd.push_str("export ");
                cmd.push_str(k);
                cmd.push('=');
                cmd.push_str(&Self::shell_escape(v));
                cmd.push_str("; ");
            }
        }

        cmd.push_str("cd ");
        cmd.push_str(&Self::shell_escape(rel_cwd));
        cmd.push_str(" && exec ");
        cmd.push_str(&Self::shell_escape(program));
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&Self::shell_escape(arg));
        }
        cmd
    }

    /// Build a shell command that bootstraps Tailscale networking before running the program.
    ///
    /// This runs the sandboxed-tailscale-up script (which also calls sandboxed-network-up)
    /// to bring up the veth interface, get an IP via DHCP, start tailscaled, and authenticate.
    /// The scripts are installed by the workspace template's init_script.
    ///
    /// When `export_all_env` is true (nsenter path), all env vars are exported in the
    /// shell command since nsenter doesn't propagate env vars into the namespace.
    /// When false (nspawn path), only TS_* vars are exported (others use --setenv).
    ///
    /// `tailnet_only`: When true, set up default route via host gateway for internet
    /// while using Tailscale only for tailnet device access. When false (exit_node mode),
    /// all traffic goes through Tailscale's exit node.
    fn build_tailscale_bootstrap_command(
        rel_cwd: &str,
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
        export_all_env: bool,
        tailnet_only: bool,
    ) -> String {
        let mut cmd = String::new();

        // Export env vars so the bootstrap script and program can use them.
        for (k, v) in env {
            if k.trim().is_empty() {
                continue;
            }
            // When using nsenter, export ALL env vars (nsenter doesn't propagate them).
            // When using nspawn, only export TS_* vars (others are passed via --setenv).
            if export_all_env || (k.starts_with("TS_") && !v.trim().is_empty()) {
                cmd.push_str("export ");
                cmd.push_str(k);
                cmd.push('=');
                cmd.push_str(&Self::shell_escape(v));
                cmd.push_str("; ");
            }
        }

        // Run the Tailscale bootstrap script if it exists.
        // The script calls sandboxed-network-up (DHCP via udhcpc, which sets up
        // the IP, default route, and DNS), then starts tailscaled and authenticates.
        // Errors are suppressed to allow the main program to run even if networking fails.
        cmd.push_str(
            "if [ -x /usr/local/bin/sandboxed-tailscale-up ]; then \
             /usr/local/bin/sandboxed-tailscale-up >/dev/null 2>&1 || true; \
             fi; ",
        );

        if tailnet_only {
            // tailnet_only mode: Use Tailscale for tailnet access only, route
            // regular internet traffic through the host gateway (not exit node).
            // This ensures the container can reach both tailnet devices AND the internet.
            cmd.push_str(
                "_oa_ip=$(ip -4 addr show host0 2>/dev/null | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p' | head -1); \
                 _oa_gw=\"${_oa_ip%.*}.1\"; \
                 if [ -n \"$_oa_ip\" ]; then \
                   ip route del default 2>/dev/null || true; \
                   ip route add default via \"$_oa_gw\" 2>/dev/null || true; \
                 fi; \
                 if [ ! -s /etc/resolv.conf ]; then \
                 printf 'nameserver 8.8.8.8\\nnameserver 1.1.1.1\\n' > /etc/resolv.conf 2>/dev/null || true; \
                 fi; ",
            );
        } else {
            // exit_node mode: Fallback route only if DHCP/Tailscale didn't set one.
            // All traffic should go through Tailscale's exit node when properly configured.
            cmd.push_str(
                "if ! ip route show default 2>/dev/null | grep -q default; then \
                 _oa_ip=$(ip -4 addr show host0 2>/dev/null | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p' | head -1); \
                 _oa_gw=\"${_oa_ip%.*}.1\"; \
                 [ -n \"$_oa_ip\" ] && ip route add default via \"$_oa_gw\" 2>/dev/null || true; \
                 fi; \
                 if [ ! -s /etc/resolv.conf ]; then \
                 printf 'nameserver 8.8.8.8\\nnameserver 1.1.1.1\\n' > /etc/resolv.conf 2>/dev/null || true; \
                 fi; ",
            );
        }

        // Change to the working directory and exec the main program.
        cmd.push_str("cd ");
        cmd.push_str(&Self::shell_escape(rel_cwd));
        cmd.push_str(" && exec ");
        cmd.push_str(&Self::shell_escape(program));
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&Self::shell_escape(arg));
        }
        cmd
    }

    fn machine_name(&self) -> Option<String> {
        self.workspace
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
    }

    async fn running_container_leader(&self) -> Option<String> {
        let name = self.machine_name()?;
        let machinectl = if Path::new("/usr/bin/machinectl").exists() {
            "/usr/bin/machinectl"
        } else {
            "machinectl"
        };
        let output = Command::new(machinectl)
            .args(["show", &name, "-p", "Leader", "--value"])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let leader = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if leader.is_empty() {
            None
        } else {
            Some(leader)
        }
    }

    fn build_nsenter_command(
        &self,
        leader: &str,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
        tailscale_bootstrap: bool,
        tailnet_only: bool,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> anyhow::Result<Command> {
        let nsenter = if Path::new("/usr/bin/nsenter").exists() {
            "/usr/bin/nsenter"
        } else {
            "nsenter"
        };
        let rel_cwd = self.rel_path_in_container(cwd);
        // Build shell command with env exports - nsenter doesn't pass env vars
        // into the container namespace, so we need to export them in the shell.
        let shell_cmd = if tailscale_bootstrap {
            tracing::info!(
                workspace = %self.workspace.name,
                tailnet_only = %tailnet_only,
                "WorkspaceExec: nsenter with Tailscale bootstrap"
            );
            Self::build_tailscale_bootstrap_command(
                &rel_cwd,
                program,
                args,
                &env,
                true,
                tailnet_only,
            )
        } else {
            let env_ref = if env.is_empty() { None } else { Some(&env) };
            Self::build_shell_command_with_env(&rel_cwd, program, args, env_ref)
        };
        let mut cmd = Command::new(nsenter);
        cmd.args([
            "--target", leader, "--mount", "--uts", "--ipc", "--net", "--pid", "/bin/sh", "-lc",
        ]);
        cmd.arg(shell_cmd);
        // Note: env vars are now exported in the shell command, not here.
        // Setting them here with cmd.envs() doesn't propagate into the container.
        cmd.stdin(stdin).stdout(stdout).stderr(stderr);
        Ok(cmd)
    }

    async fn build_command(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> anyhow::Result<Command> {
        match self.workspace.workspace_type {
            WorkspaceType::Host => {
                // For Host workspaces, spawn the command directly with environment variables.
                // We pass env vars directly via Command::envs() rather than shell export
                // to avoid issues with shell profile sourcing that can cause timeouts.
                let mut cmd = Command::new(program);
                cmd.current_dir(cwd);
                if !args.is_empty() {
                    cmd.args(args);
                }
                if !env.is_empty() {
                    cmd.envs(env);
                }
                cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                Ok(cmd)
            }
            WorkspaceType::Container => {
                if !use_nspawn_for_workspace(&self.workspace) {
                    // Fallback: execute on host when systemd-nspawn isn't available.
                    let mut cmd = Command::new(program);
                    cmd.current_dir(cwd);
                    if !args.is_empty() {
                        cmd.args(args);
                    }
                    if !env.is_empty() {
                        cmd.envs(env);
                    }
                    cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                    return Ok(cmd);
                }

                let mut env = env;
                if !env.contains_key("HOME") {
                    env.insert("HOME".to_string(), "/root".to_string());
                }
                // Determine if Tailscale bootstrap is needed before the nsenter
                // check, so the nsenter path can also include the bootstrap.
                let needs_tailscale_bootstrap = nspawn::tailscale_enabled(&env)
                    && !nspawn::tailscale_nspawn_extra_args(&env).is_empty();
                // Calculate tailnet_only for nsenter path: TailnetOnly mode means
                // we connect to tailnet but use host gateway for internet.
                let nsenter_tailnet_only = needs_tailscale_bootstrap
                    && self
                        .workspace
                        .tailscale_mode
                        .unwrap_or(TailscaleMode::ExitNode)
                        == TailscaleMode::TailnetOnly;
                if let Some(leader) = self.running_container_leader().await {
                    return self.build_nsenter_command(
                        &leader,
                        cwd,
                        program,
                        args,
                        env,
                        needs_tailscale_bootstrap,
                        nsenter_tailnet_only,
                        stdin,
                        stdout,
                        stderr,
                    );
                }

                // For container workspaces we execute via systemd-nspawn.
                // Note: this requires systemd-nspawn on the host at runtime.
                let root = self.workspace.path.clone();
                let rel_cwd = self.rel_path_in_container(cwd);

                let mut cmd = Command::new("systemd-nspawn");
                cmd.arg("-D").arg(root);
                cmd.arg("--quiet");
                cmd.arg("--timezone=off");
                cmd.arg("--console=pipe");
                cmd.arg("--chdir").arg(&rel_cwd);

                // Ensure /root/context is available if Open Agent configured it.
                let context_dir_name = std::env::var("SANDBOXED_SH_CONTEXT_DIR_NAME")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "context".to_string());
                let global_context_root = std::env::var("SANDBOXED_SH_CONTEXT_ROOT")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/root").join(&context_dir_name));
                if global_context_root.exists() {
                    cmd.arg(format!(
                        "--bind={}:/root/context",
                        global_context_root.display()
                    ));
                    cmd.arg("--setenv=SANDBOXED_SH_CONTEXT_ROOT=/root/context");
                    cmd.arg(format!(
                        "--setenv=SANDBOXED_SH_CONTEXT_DIR_NAME={}",
                        context_dir_name
                    ));
                }

                // Bind X11 socket for GUI applications (e.g., Minecraft) when available.
                // The desktop MCP creates Xvfb displays on the host; containers need
                // access to /tmp/.X11-unix to connect to these displays.
                let x11_socket_path = Path::new("/tmp/.X11-unix");
                if x11_socket_path.exists() {
                    cmd.arg("--bind=/tmp/.X11-unix");
                }

                // Network configuration.
                // Respect the user's shared_network setting directly.
                // - shared_network=true: Use host network (and host's Tailscale if connected)
                // - shared_network=false: Isolated network with optional Tailscale
                //   - tailscale_mode=exit_node: All traffic via Tailscale exit node
                //   - tailscale_mode=tailnet_only: Tailscale for tailnet, host gateway for internet
                let use_shared_network = self.workspace.shared_network.unwrap_or(true);
                let tailscale_mode = self
                    .workspace
                    .tailscale_mode
                    .unwrap_or(TailscaleMode::ExitNode);
                let tailscale_requested = nspawn::tailscale_enabled(&env);

                tracing::debug!(
                    workspace = %self.workspace.name,
                    shared_network = ?self.workspace.shared_network,
                    tailscale_mode = ?tailscale_mode,
                    tailscale_requested = %tailscale_requested,
                    use_shared_network = %use_shared_network,
                    "WorkspaceExec: checking network configuration"
                );

                let tailscale_enabled = if use_shared_network {
                    // Shared network: use host network, bind DNS
                    tracing::debug!("WorkspaceExec: shared_network=true, binding resolv.conf");
                    cmd.arg("--bind-ro=/etc/resolv.conf");
                    false
                } else {
                    // Isolated network: check if Tailscale is configured
                    let tailscale_args = nspawn::tailscale_nspawn_extra_args(&env);
                    tracing::debug!(
                        tailscale_args = ?tailscale_args,
                        "WorkspaceExec: checking Tailscale args"
                    );
                    if tailscale_args.is_empty() {
                        tracing::debug!("WorkspaceExec: no Tailscale args, binding resolv.conf");
                        cmd.arg("--bind-ro=/etc/resolv.conf");
                        false
                    } else {
                        tracing::info!(
                            workspace = %self.workspace.name,
                            tailscale_mode = %tailscale_mode.as_str(),
                            "WorkspaceExec: Tailscale networking enabled"
                        );
                        for a in tailscale_args {
                            cmd.arg(a);
                        }
                        true
                    }
                };

                // For tailnet_only mode, we need to tell the bootstrap script
                // to set up a default route via host gateway for internet access
                let tailnet_only =
                    tailscale_enabled && tailscale_mode == TailscaleMode::TailnetOnly;

                // Set env vars inside the container.
                for (k, v) in &env {
                    if k.trim().is_empty() {
                        continue;
                    }
                    cmd.arg(format!("--setenv={}={}", k, v));
                }

                // When Tailscale is enabled, wrap the command in a shell that bootstraps
                // networking before running the actual program. The bootstrap scripts
                // are installed by the workspace template's init_script.
                if tailscale_enabled {
                    // Build a shell command that:
                    // 1. Runs sandboxed-tailscale-up (which also calls sandboxed-network-up)
                    // 2. Execs the actual program to hand off control
                    let shell_cmd = Self::build_tailscale_bootstrap_command(
                        &rel_cwd,
                        program,
                        args,
                        &env,
                        false,
                        tailnet_only,
                    );
                    tracing::info!(
                        workspace = %self.workspace.name,
                        program = %program,
                        tailnet_only = %tailnet_only,
                        "WorkspaceExec: running with Tailscale bootstrap"
                    );
                    tracing::debug!(
                        shell_cmd = %shell_cmd,
                        "WorkspaceExec: Tailscale bootstrap shell command"
                    );
                    cmd.arg("/bin/sh");
                    cmd.arg("-c");
                    cmd.arg(shell_cmd);
                } else {
                    tracing::debug!(
                        workspace = %self.workspace.name,
                        program = %program,
                        "WorkspaceExec: running without Tailscale bootstrap"
                    );
                    cmd.arg(program);
                    cmd.args(args);
                }

                cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                Ok(cmd)
            }
        }
    }

    pub async fn output(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
    ) -> anyhow::Result<std::process::Output> {
        let env = self.build_env(env);
        let mut cmd = self
            .build_command(
                cwd,
                program,
                args,
                env,
                Stdio::null(),
                Stdio::piped(),
                Stdio::piped(),
            )
            .await
            .context("Failed to build workspace command")?;
        let output = cmd
            .output()
            .await
            .context("Failed to run workspace command")?;
        Ok(output)
    }

    pub async fn spawn_streaming(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
    ) -> anyhow::Result<Child> {
        let env = self.build_env(env);
        let mut cmd = self
            .build_command(
                cwd,
                program,
                args,
                env,
                Stdio::piped(), // Pipe stdin for processes that read input (e.g., Claude Code --print)
                Stdio::piped(),
                Stdio::piped(),
            )
            .await
            .context("Failed to build workspace command")?;

        let child = cmd.spawn().context("Failed to spawn workspace command")?;
        Ok(child)
    }
}
