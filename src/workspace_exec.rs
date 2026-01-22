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
use crate::workspace::{use_nspawn_for_workspace, Workspace, WorkspaceType};

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
    ///   /root/.openagent/containers/minecraft/workspaces/mission-xxx/.claude/settings.json
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
            .entry("OPEN_AGENT_WORKSPACE_TYPE".to_string())
            .or_insert_with(|| self.workspace.workspace_type.as_str().to_string());
        if self.workspace.workspace_type == WorkspaceType::Container {
            if let Some(name) = self
                .workspace
                .path
                .file_name()
                .and_then(|n| n.to_str())
            {
                if !name.trim().is_empty() {
                    merged
                        .entry("OPEN_AGENT_WORKSPACE_NAME".to_string())
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
        if self.workspace.workspace_type == WorkspaceType::Container && !use_nspawn_for_workspace(&self.workspace) {
            merged
                .entry("OPEN_AGENT_CONTAINER_FALLBACK".to_string())
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

    fn build_shell_command(rel_cwd: &str, program: &str, args: &[String]) -> String {
        let mut cmd = String::new();
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
        let shell_cmd = Self::build_shell_command(&rel_cwd, program, args);
        let mut cmd = Command::new(nsenter);
        cmd.args([
            "--target",
            leader,
            "--mount",
            "--uts",
            "--ipc",
            "--net",
            "--pid",
            "/bin/sh",
            "-lc",
        ]);
        cmd.arg(shell_cmd);
        if !env.is_empty() {
            cmd.envs(env);
        }
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
                if let Some(leader) = self.running_container_leader().await {
                    return self.build_nsenter_command(
                        &leader, cwd, program, args, env, stdin, stdout, stderr,
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
                cmd.arg("--chdir").arg(rel_cwd);

                // Ensure /root/context is available if Open Agent configured it.
                let context_dir_name = std::env::var("OPEN_AGENT_CONTEXT_DIR_NAME")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "context".to_string());
                let global_context_root = std::env::var("OPEN_AGENT_CONTEXT_ROOT")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/root").join(&context_dir_name));
                if global_context_root.exists() {
                    cmd.arg(format!(
                        "--bind={}:/root/context",
                        global_context_root.display()
                    ));
                    cmd.arg("--setenv=OPEN_AGENT_CONTEXT_ROOT=/root/context");
                    cmd.arg(format!(
                        "--setenv=OPEN_AGENT_CONTEXT_DIR_NAME={}",
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
                let use_shared_network = self.workspace.shared_network.unwrap_or(true);
                if use_shared_network {
                    cmd.arg("--bind-ro=/etc/resolv.conf");
                } else {
                    // If Tailscale is configured, it will set up networking; otherwise bind DNS.
                    let tailscale_args = nspawn::tailscale_nspawn_extra_args(&env);
                    if tailscale_args.is_empty() {
                        cmd.arg("--bind-ro=/etc/resolv.conf");
                    } else {
                        for a in tailscale_args {
                            cmd.arg(a);
                        }
                    }
                }

                // Set env vars inside the container.
                for (k, v) in env {
                    if k.trim().is_empty() {
                        continue;
                    }
                    cmd.arg(format!("--setenv={}={}", k, v));
                }

                cmd.arg(program);
                cmd.args(args);

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
                Stdio::piped(),
                Stdio::piped(),
                Stdio::piped(),
            )
            .await
            .context("Failed to build workspace command")?;

        let child = cmd.spawn().context("Failed to spawn workspace command")?;
        Ok(child)
    }
}
