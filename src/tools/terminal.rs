//! Terminal/shell command execution tool.
//!
//! ## Workspace-First Design
//!
//! Commands run in the workspace by default:
//! - `run_command("ls")` → lists workspace contents
//! - `run_command("cat output/report.md")` → reads workspace file

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{resolve_path_simple as resolve_path, Tool};
use crate::nspawn;

/// Context information read from the local context file.
/// This is re-read before each container command to handle timing issues
/// where the context file is written after the MCP process starts.
#[derive(Debug, Default)]
struct RuntimeContext {
    context_root: Option<String>,
    mission_id: Option<String>,
}

/// Read context information from the local context file or fall back to env vars.
/// This function is called before each container command to ensure we have the latest
/// context information, even if the context file was written after the MCP started.
fn read_runtime_context(container_root: &Path) -> RuntimeContext {
    // First check env vars (set during MCP initialization)
    let env_context_root = env::var("SANDBOXED_SH_CONTEXT_ROOT").ok();
    let env_mission_id = env::var("SANDBOXED_SH_MISSION_ID").ok();

    // If env vars are set, use them
    if env_context_root.is_some() {
        return RuntimeContext {
            context_root: env_context_root,
            mission_id: env_mission_id,
        };
    }

    // Otherwise, try to read from the local context file
    // This handles the case where the MCP started before the context file was written
    let context_file = container_root.join(".sandboxed-sh_context.json");
    if let Ok(contents) = std::fs::read_to_string(&context_file) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
            let context_root = json
                .get("context_root")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mission_id = json
                .get("mission_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // If we found context_root in the file, update the env var for future calls
            if let Some(ref root) = context_root {
                env::set_var("SANDBOXED_SH_CONTEXT_ROOT", root);
            }
            if let Some(ref id) = mission_id {
                env::set_var("SANDBOXED_SH_MISSION_ID", id);
            }

            return RuntimeContext {
                context_root,
                mission_id,
            };
        }
    }

    RuntimeContext::default()
}

/// Sanitize command output to be safe for LLM consumption.
/// Removes binary garbage while preserving valid text.
fn sanitize_output(bytes: &[u8]) -> String {
    // Check if output appears to be mostly binary
    let non_printable_count = bytes
        .iter()
        .filter(|&&b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
        .count();

    // If more than 10% is non-printable (excluding newlines/tabs), it's likely binary
    if bytes.len() > 100 && non_printable_count > bytes.len() / 10 {
        return format!(
            "[Binary output detected - {} bytes, {}% non-printable. \
            Use appropriate tools to process binary data.]",
            bytes.len(),
            non_printable_count * 100 / bytes.len()
        );
    }

    // Convert to string, replacing invalid UTF-8
    let text = String::from_utf8_lossy(bytes);

    // Remove null bytes and other problematic control characters
    // Keep: newlines, tabs, carriage returns
    text.chars()
        .filter(|&c| c == '\n' || c == '\r' || c == '\t' || (c >= ' ' && c != '\u{FFFD}'))
        .collect()
}

/// Dangerous command patterns that should be blocked.
/// These patterns cause infinite loops or could damage the system.
const DANGEROUS_PATTERNS: &[(&str, &str)] = &[
    (
        "find /",
        "Use 'find /root/work/' or a specific directory path",
    ),
    (
        "find / ",
        "Use 'find /root/work/' or a specific directory path",
    ),
    (
        "grep -r /",
        "Use 'grep -r /root/' or a specific directory path",
    ),
    (
        "grep -rn /",
        "Use 'grep -rn /root/' or a specific directory path",
    ),
    (
        "grep -R /",
        "Use 'grep -R /root/' or a specific directory path",
    ),
    ("ls -laR /", "Use a specific directory path instead of root"),
    ("du -sh /", "Use a specific directory path instead of root"),
    ("du -a /", "Use a specific directory path instead of root"),
    ("rm -rf /", "This would destroy the entire system"),
    ("rm -rf /*", "This would destroy the entire system"),
    ("> /dev/", "Writing to device files is blocked"),
    ("dd if=/dev/", "Direct disk operations are blocked"),
];

/// Validate a command against dangerous patterns.
/// Returns Ok(()) if safe, Err with suggestion if blocked.
fn validate_command(cmd: &str) -> Result<(), String> {
    let cmd_trimmed = cmd.trim();
    let prefixes = ["sudo ", "time ", "nice ", "nohup "];

    let is_safe_root_find = {
        let mut safe = false;
        if cmd_trimmed.starts_with("find /root") {
            safe = true;
        } else {
            for prefix in prefixes {
                if cmd_trimmed.starts_with(prefix) {
                    let after_prefix = cmd_trimmed[prefix.len()..].trim_start();
                    if after_prefix.starts_with("find /root") {
                        safe = true;
                        break;
                    }
                }
            }
        }
        safe
    };

    let is_safe_root_grep = {
        let mut safe = false;
        let grep_prefixes = ["grep -r /root", "grep -rn /root", "grep -R /root"];
        if grep_prefixes.iter().any(|p| cmd_trimmed.starts_with(p)) {
            safe = true;
        } else {
            for prefix in prefixes {
                if cmd_trimmed.starts_with(prefix) {
                    let after_prefix = cmd_trimmed[prefix.len()..].trim_start();
                    if grep_prefixes.iter().any(|p| after_prefix.starts_with(p)) {
                        safe = true;
                        break;
                    }
                }
            }
        }
        safe
    };

    for (pattern, suggestion) in DANGEROUS_PATTERNS {
        // Check if command starts with the dangerous pattern
        if cmd_trimmed.starts_with(pattern) {
            if (pattern == &"find /" || pattern == &"find / ") && is_safe_root_find {
                continue;
            }
            if (pattern == &"grep -r /" || pattern == &"grep -rn /" || pattern == &"grep -R /")
                && is_safe_root_grep
            {
                continue;
            }
            return Err(format!(
                "Blocked dangerous command pattern '{}'. {}",
                pattern, suggestion
            ));
        }
        // Also check for the pattern after common prefixes (sudo, time, etc.)
        for prefix in prefixes {
            if cmd_trimmed.starts_with(prefix) {
                let after_prefix = &cmd_trimmed[prefix.len()..];
                if after_prefix.starts_with(pattern) {
                    if (pattern == &"find /" || pattern == &"find / ") && is_safe_root_find {
                        continue;
                    }
                    if (pattern == &"grep -r /"
                        || pattern == &"grep -rn /"
                        || pattern == &"grep -R /")
                        && is_safe_root_grep
                    {
                        continue;
                    }
                    return Err(format!(
                        "Blocked dangerous command pattern '{}'. {}",
                        pattern, suggestion
                    ));
                }
            }
        }
    }

    Ok(())
}

fn container_root_from_env() -> Option<PathBuf> {
    let workspace_type = env::var("SANDBOXED_SH_WORKSPACE_TYPE").ok()?;
    if workspace_type != "container" {
        return None;
    }
    if let Ok(flag) = env::var("SANDBOXED_SH_CONTAINER_FALLBACK") {
        if matches!(
            flag.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        ) {
            return None;
        }
    }
    let root = env::var("SANDBOXED_SH_WORKSPACE_ROOT").ok()?;
    Some(PathBuf::from(root))
}

#[derive(Debug, Clone)]
struct CommandOptions {
    timeout: Duration,
    env: HashMap<String, String>,
    clear_env: bool,
    stdin: Option<String>,
    shell: Option<String>,
    max_output_chars: usize,
    raw_output: bool,
}

const DEFAULT_MAX_OUTPUT_CHARS: usize = 10_000;
const MAX_OUTPUT_CHARS_LIMIT: usize = 50_000;
const DEFAULT_COMMAND_TIMEOUT_SECS: f64 = 300.0;

fn default_timeout_from_env() -> Duration {
    if let Ok(raw) = env::var("SANDBOXED_SH_COMMAND_TIMEOUT_SECS") {
        if let Ok(value) = raw.parse::<f64>() {
            if value > 0.0 {
                return Duration::from_secs_f64(value);
            }
        }
    }
    Duration::from_secs_f64(DEFAULT_COMMAND_TIMEOUT_SECS)
}

fn parse_timeout(args: &Value) -> Duration {
    if let Some(ms) = args.get("timeout_ms").and_then(|v| v.as_u64()) {
        return Duration::from_millis(ms.max(1));
    }
    if let Some(secs) = args.get("timeout_secs").and_then(|v| v.as_u64()) {
        return Duration::from_secs(secs.max(1));
    }
    if let Some(secs) = args.get("timeout").and_then(|v| v.as_f64()) {
        if secs > 0.0 {
            return Duration::from_secs_f64(secs);
        }
    }
    default_timeout_from_env()
}

fn parse_env(args: &Value) -> HashMap<String, String> {
    let mut envs = HashMap::new();
    let Some(obj) = args.get("env").and_then(|v| v.as_object()) else {
        return envs;
    };
    for (key, value) in obj.iter() {
        if let Some(val) = value.as_str() {
            envs.insert(key.clone(), val.to_string());
        }
    }
    envs
}

fn workspace_env_vars() -> HashMap<String, String> {
    let mut envs = HashMap::new();
    if let Ok(raw_path) = env::var("SANDBOXED_SH_WORKSPACE_ENV_VARS_FILE") {
        let path = raw_path.trim();
        if !path.is_empty() {
            let mut candidates = Vec::new();
            let path_buf = PathBuf::from(path);
            if path_buf.is_absolute() {
                candidates.push(path_buf);
            } else {
                if let Ok(cwd) = env::current_dir() {
                    candidates.push(cwd.join(&path_buf));
                }
                if let Ok(workspace) = env::var("SANDBOXED_SH_WORKSPACE") {
                    if !workspace.trim().is_empty() {
                        candidates.push(PathBuf::from(workspace).join(&path_buf));
                    }
                }
                if let Ok(working_dir) = env::var("WORKING_DIR") {
                    if !working_dir.trim().is_empty() {
                        candidates.push(PathBuf::from(working_dir).join(&path_buf));
                    }
                }
                candidates.push(path_buf);
            }

            for candidate in candidates {
                if let Ok(raw) = std::fs::read_to_string(&candidate) {
                    if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
                        envs.extend(map);
                        return envs;
                    }
                }
            }
        }
    }
    let Ok(raw) = env::var("SANDBOXED_SH_WORKSPACE_ENV_VARS") else {
        return envs;
    };
    if raw.trim().is_empty() {
        return envs;
    }
    if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
        envs.extend(map);
    }
    envs
}

fn parse_max_output_chars(args: &Value) -> usize {
    let max = args
        .get("max_output_chars")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS);
    max.clamp(1, MAX_OUTPUT_CHARS_LIMIT)
}

fn parse_command_options(args: &Value) -> CommandOptions {
    CommandOptions {
        timeout: parse_timeout(args),
        env: parse_env(args),
        clear_env: args
            .get("clear_env")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        stdin: args
            .get("stdin")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        shell: args
            .get("shell")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        max_output_chars: parse_max_output_chars(args),
        raw_output: args.get("raw").and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

fn shell_exists(shell: &str, container_root: Option<&Path>) -> bool {
    if let Some(root) = container_root {
        let rel = shell.strip_prefix('/').unwrap_or(shell);
        return root.join(rel).exists();
    }
    Path::new(shell).exists()
}

fn resolve_shell(shell: Option<&str>, container_root: Option<&Path>) -> String {
    if let Some(shell) = shell {
        if shell_exists(shell, container_root) {
            return shell.to_string();
        }
        // Fall back to /bin/bash then /bin/sh if requested shell isn't available.
        if shell_exists("/bin/bash", container_root) {
            return "/bin/bash".to_string();
        }
        if shell_exists("/bin/sh", container_root) {
            return "/bin/sh".to_string();
        }
        return shell.to_string();
    }

    // Prefer bash for login shell support (profile.d scripts).
    if shell_exists("/bin/bash", container_root) {
        return "/bin/bash".to_string();
    }

    if shell_exists("/bin/sh", container_root) {
        return "/bin/sh".to_string();
    }

    "/bin/sh".to_string()
}

async fn run_shell_command(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    options: &CommandOptions,
) -> anyhow::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if options.clear_env {
        cmd.env_clear();
    }
    if !options.env.is_empty() {
        cmd.envs(&options.env);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;

    if let Some(input) = options.stdin.as_deref() {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to write to stdin: {}", e))?;
        }
    }

    let output = tokio::time::timeout(options.timeout, child.wait_with_output()).await;

    match output {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(anyhow::anyhow!("Failed to execute command: {}", e)),
        Err(_) => Err(anyhow::anyhow!(
            "Command timed out after {} seconds",
            options.timeout.as_secs_f64()
        )),
    }
}

async fn run_host_command(
    cwd: &Path,
    command: &str,
    options: &CommandOptions,
) -> anyhow::Result<Output> {
    let (shell, shell_arg) = if cfg!(target_os = "windows") {
        ("cmd".to_string(), "/C".to_string())
    } else {
        (
            resolve_shell(options.shell.as_deref(), None),
            "-c".to_string(),
        )
    };
    let args = vec![shell_arg, command.to_string()];
    run_shell_command(&shell, &args, Some(cwd), options).await
}

fn runtime_display_path() -> Option<PathBuf> {
    if let Ok(path) = env::var("SANDBOXED_SH_RUNTIME_DISPLAY_FILE") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let candidates = [
        env::var("WORKING_DIR").ok(),
        env::var("SANDBOXED_SH_WORKSPACE_ROOT").ok(),
        env::var("HOME").ok(),
    ];

    for base in candidates.into_iter().flatten() {
        let path = PathBuf::from(base)
            .join(".sandboxed-sh")
            .join("runtime")
            .join("current_display.json");
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn read_runtime_display() -> Option<String> {
    if let Ok(display) = env::var("DESKTOP_DISPLAY") {
        if !display.trim().is_empty() {
            return Some(display);
        }
    }

    let path = runtime_display_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
        return json
            .get("display")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn run_container_command(
    container_root: &Path,
    cwd: &Path,
    command: &str,
    options: &CommandOptions,
) -> anyhow::Result<Output> {
    let root = container_root
        .canonicalize()
        .unwrap_or_else(|_| container_root.to_path_buf());
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    let rel_str = if cwd.starts_with(&root) {
        let rel = cwd.strip_prefix(&root).unwrap_or_else(|_| Path::new(""));
        if rel.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rel.to_string_lossy())
        }
    } else if cwd.is_absolute() {
        // Allow absolute container paths even when the host path isn't under the container root.
        // The container runtime will resolve (or reject) the path inside the container.
        cwd.to_string_lossy().to_string()
    } else {
        "/".to_string()
    };

    // If a container is already running (e.g., MCP server), run commands via nsenter.
    if let Ok(machine_name) = env::var("SANDBOXED_SH_WORKSPACE_NAME") {
        let machine_name = machine_name.trim();
        if !machine_name.is_empty() {
            if let Some(leader) = running_container_leader(machine_name, options).await {
                if let Ok(output) = run_nsenter_command(&leader, &rel_str, command, options).await {
                    return Ok(output);
                }
            }
        }
    }

    let mut args = vec![
        "-D".to_string(),
        root.to_string_lossy().to_string(),
        "--quiet".to_string(),
        "--timezone=off".to_string(),
        "--chdir".to_string(),
        rel_str,
    ];

    // Explicitly set HOME=/root inside the container.
    // The host process may have a different HOME (e.g., /var/lib/opencode for isolated OpenCode),
    // and nspawn inherits environment variables by default. Tools like `shard` use $HOME/.shard
    // for their configuration, so we must ensure HOME points to the container's root user home.
    args.push("--setenv=HOME=/root".to_string());

    // Bind mission context into containers so uploaded files are accessible.
    // We read from the local context file to handle timing issues where the context file
    // is written after the MCP process starts.
    let runtime_ctx = read_runtime_context(&root);
    if let Some(context_root) = runtime_ctx.context_root.as_ref() {
        let context_root = context_root.trim();
        if !context_root.is_empty() && Path::new(context_root).exists() {
            args.push(format!("--bind={}:/root/context", context_root));
            args.push("--setenv=SANDBOXED_SH_CONTEXT_ROOT=/root/context".to_string());
            if let Some(mission_id) = runtime_ctx.mission_id.as_ref() {
                let mission_id = mission_id.trim();
                if !mission_id.is_empty() {
                    args.push(format!(
                        "--setenv=SANDBOXED_SH_MISSION_CONTEXT=/root/context/{}",
                        mission_id
                    ));
                }
            }
        }
    }

    if let Some(display) = read_runtime_display() {
        if Path::new("/tmp/.X11-unix").exists() {
            args.push("--bind=/tmp/.X11-unix".to_string());
            args.push(format!("--setenv=DISPLAY={}", display));
        }
    }

    let mut merged_env = workspace_env_vars();
    merged_env.extend(options.env.clone());

    for arg in nspawn::tailscale_nspawn_extra_args(&merged_env) {
        args.push(arg);
    }

    for (key, value) in &merged_env {
        args.push(format!("--setenv={}={}", key, value));
    }

    let shell = resolve_shell(options.shell.as_deref(), Some(&root));
    args.push(shell.clone());

    // Use login shell (-l) to source /etc/profile.d/ scripts.
    // This ensures networking and Tailscale are set up, consistent with interactive terminals.
    if shell.ends_with("bash") {
        args.push("--login".to_string());
    }
    args.push("-c".to_string());
    args.push(command.to_string());

    let mut output = run_shell_command("systemd-nspawn", &args, None, options).await?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let is_busy = stderr.contains("Directory tree") && stderr.contains("busy");
    if is_busy {
        // If the container is already running without an active desktop, terminate it and retry.
        if read_runtime_display().is_none() {
            if let Ok(machine_name) = env::var("SANDBOXED_SH_WORKSPACE_NAME") {
                let machine_name = machine_name.trim();
                if !machine_name.is_empty() {
                    let terminate_args = vec!["terminate".to_string(), machine_name.to_string()];
                    let machinectl = if Path::new("/usr/bin/machinectl").exists() {
                        "/usr/bin/machinectl"
                    } else {
                        "machinectl"
                    };
                    let _ = run_shell_command(machinectl, &terminate_args, None, options).await;
                }
            }
        }

        // Retry a few times in case another nspawn process is holding the root.
        for attempt in 1..=3 {
            tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
            output = run_shell_command("systemd-nspawn", &args, None, options).await?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !(stderr.contains("Directory tree") && stderr.contains("busy")) {
                break;
            }
        }
    }

    Ok(output)
}

async fn running_container_leader(machine_name: &str, options: &CommandOptions) -> Option<String> {
    let machinectl = if Path::new("/usr/bin/machinectl").exists() {
        "/usr/bin/machinectl"
    } else {
        "machinectl"
    };
    let args = vec![
        "show".to_string(),
        machine_name.to_string(),
        "-p".to_string(),
        "Leader".to_string(),
        "--value".to_string(),
    ];
    let output = run_shell_command(machinectl, &args, None, options)
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

fn nsenter_command(rel_str: &str, command: &str) -> String {
    let mut exports = Vec::new();
    exports.push("export SANDBOXED_SH_CONTEXT_ROOT=/root/context".to_string());
    if let Ok(context_dir) = env::var("SANDBOXED_SH_CONTEXT_DIR_NAME") {
        if !context_dir.trim().is_empty() {
            exports.push(format!(
                "export SANDBOXED_SH_CONTEXT_DIR_NAME={}",
                context_dir.trim()
            ));
        }
    }
    if let Ok(mission_id) = env::var("SANDBOXED_SH_MISSION_ID") {
        let mission_id = mission_id.trim();
        if !mission_id.is_empty() {
            exports.push(format!("export SANDBOXED_SH_MISSION_ID={}", mission_id));
            exports.push(format!(
                "export SANDBOXED_SH_MISSION_CONTEXT=/root/context/{}",
                mission_id
            ));
        }
    }
    let prelude = if exports.is_empty() {
        String::new()
    } else {
        format!("{}; ", exports.join("; "))
    };
    format!("{}cd {} && {}", prelude, rel_str, command)
}

async fn run_nsenter_command(
    leader_pid: &str,
    rel_str: &str,
    command: &str,
    options: &CommandOptions,
) -> anyhow::Result<Output> {
    let nsenter = if Path::new("/usr/bin/nsenter").exists() {
        "/usr/bin/nsenter"
    } else {
        "nsenter"
    };
    let args = vec![
        "--target".to_string(),
        leader_pid.to_string(),
        "--mount".to_string(),
        "--uts".to_string(),
        "--ipc".to_string(),
        "--net".to_string(),
        "--pid".to_string(),
        "/bin/sh".to_string(),
        "-lc".to_string(),
        nsenter_command(rel_str, command),
    ];
    run_shell_command(nsenter, &args, None, options).await
}

/// Run a shell command.
pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Runs in workspace by default. Use for tests, builds, package installs, etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute. Relative paths in commands resolve from workspace."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional: working directory. Defaults to workspace. Use relative paths (e.g., 'subdir/') or absolute for system access."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 60)."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (overrides timeout_secs)."
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds (float allowed)."
                },
                "env": {
                    "type": "object",
                    "description": "Environment variables to set for the command.",
                    "additionalProperties": { "type": "string" }
                },
                "clear_env": {
                    "type": "boolean",
                    "description": "If true, clear the environment before applying env vars."
                },
                "stdin": {
                    "type": "string",
                    "description": "Optional: string to pass to stdin."
                },
                "shell": {
                    "type": "string",
                    "description": "Optional: shell executable path (default: /bin/sh)."
                },
                "max_output_chars": {
                    "type": "integer",
                    "description": "Maximum output characters to return (default: 10000)."
                },
                "raw": {
                    "type": "boolean",
                    "description": "Return combined stdout/stderr only (no headers or exit code)."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;

        let container_root = container_root_from_env();
        if container_root.is_none() {
            // Validate command against dangerous patterns on host only.
            if let Err(msg) = validate_command(command) {
                tracing::warn!("Blocked dangerous command: {}", command);
                return Err(anyhow::anyhow!("{}", msg));
            }
        }

        let cwd = args["cwd"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let options = parse_command_options(&args);

        tracing::info!("Executing command in {:?}: {}", cwd, command);

        let output = match container_root {
            Some(container_root) => {
                run_container_command(&container_root, &cwd, command, &options).await?
            }
            None => run_host_command(&cwd, command, &options).await?,
        };

        let stdout = sanitize_output(&output.stdout);
        let stderr = sanitize_output(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        tracing::debug!(
            "Command completed: exit={}, stdout_len={}, stderr_len={}",
            exit_code,
            stdout.len(),
            stderr.len()
        );

        let result = if options.raw_output {
            let mut raw = String::new();
            if !stdout.is_empty() {
                raw.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !raw.is_empty() {
                    raw.push('\n');
                }
                raw.push_str(&stderr);
            }
            raw
        } else {
            let mut result = String::new();

            result.push_str(&format!("Exit code: {}\n", exit_code));

            // Add hint when non-zero exit but output exists (common with tools that warn but succeed)
            if exit_code != 0 && !stdout.is_empty() {
                result.push_str("Note: Non-zero exit code but output was produced. The command may have succeeded with warnings - verify output files exist.\n");
            }

            if !stdout.is_empty() {
                result.push_str("\n--- stdout ---\n");
                result.push_str(&stdout);
            }

            if !stderr.is_empty() {
                result.push_str("\n--- stderr ---\n");
                result.push_str(&stderr);
            }

            result
        };

        let mut result = result;
        if result.len() > options.max_output_chars {
            result.truncate(options.max_output_chars);
            result.push_str("\n... [output truncated]");
        }

        Ok(result)
    }
}
