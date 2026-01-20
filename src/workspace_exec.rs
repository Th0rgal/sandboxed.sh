//! Workspace execution layer.
//!
//! Spawns processes inside a workspace execution context so that:
//! - Host workspaces execute directly on the host
//! - Chroot workspaces execute via systemd-nspawn in the container filesystem
//!
//! This is used for per-workspace Claude Code and OpenCode execution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use tokio::process::{Child, Command};

use crate::nspawn;
use crate::workspace::{Workspace, WorkspaceType};

#[derive(Debug, Clone)]
pub struct WorkspaceExec {
    pub workspace: Workspace,
}

impl WorkspaceExec {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
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
    }

    fn build_command(
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
                if !env.is_empty() {
                    cmd.envs(env);
                }
                cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                Ok(cmd)
            }
            WorkspaceType::Chroot => {
                // For chroot workspaces we execute via systemd-nspawn.
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
            .context("Failed to build workspace command")?;

        let child = cmd.spawn().context("Failed to spawn workspace command")?;
        Ok(child)
    }
}
