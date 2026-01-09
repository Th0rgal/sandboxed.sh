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

    for (pattern, suggestion) in DANGEROUS_PATTERNS {
        // Check if command starts with the dangerous pattern
        if cmd_trimmed.starts_with(pattern) {
            return Err(format!(
                "Blocked dangerous command pattern '{}'. {}",
                pattern, suggestion
            ));
        }
        // Also check for the pattern after common prefixes (sudo, time, etc.)
        let prefixes = ["sudo ", "time ", "nice ", "nohup "];
        for prefix in prefixes {
            if cmd_trimmed.starts_with(prefix) {
                let after_prefix = &cmd_trimmed[prefix.len()..];
                if after_prefix.starts_with(pattern) {
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
    let workspace_type = env::var("OPEN_AGENT_WORKSPACE_TYPE").ok()?;
    if workspace_type != "chroot" && workspace_type != "nspawn" && workspace_type != "container" {
        return None;
    }
    let root = env::var("OPEN_AGENT_WORKSPACE_ROOT").ok()?;
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
    Duration::from_secs(60)
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
    let Ok(raw) = env::var("OPEN_AGENT_WORKSPACE_ENV_VARS") else {
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
        clear_env: args.get("clear_env").and_then(|v| v.as_bool()).unwrap_or(false),
        stdin: args.get("stdin").and_then(|v| v.as_str()).map(|s| s.to_string()),
        shell: args.get("shell").and_then(|v| v.as_str()).map(|s| s.to_string()),
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
        // Fall back to /bin/sh if requested shell isn't available.
        if shell_exists("/bin/sh", container_root) {
            return "/bin/sh".to_string();
        }
        return shell.to_string();
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

    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;

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
        (resolve_shell(options.shell.as_deref(), None), "-c".to_string())
    };
    let args = vec![shell_arg, command.to_string()];
    run_shell_command(&shell, &args, Some(cwd), options).await
}

fn runtime_display_path() -> Option<PathBuf> {
    if let Ok(path) = env::var("OPEN_AGENT_RUNTIME_DISPLAY_FILE") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let candidates = [
        env::var("WORKING_DIR").ok(),
        env::var("OPEN_AGENT_WORKSPACE_ROOT").ok(),
        env::var("HOME").ok(),
    ];

    for base in candidates.into_iter().flatten() {
        let path = PathBuf::from(base)
            .join(".openagent")
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

    if !cwd.starts_with(&root) {
        return Err(anyhow::anyhow!(
            "Working directory is outside container root: {}",
            cwd.display()
        ));
    }

    let rel = cwd.strip_prefix(&root).unwrap_or_else(|_| Path::new(""));
    let rel_str = if rel.as_os_str().is_empty() {
        "/".to_string()
    } else {
        format!("/{}", rel.to_string_lossy())
    };

    let mut args = vec![
        "-D".to_string(),
        root.to_string_lossy().to_string(),
        "--quiet".to_string(),
        "--chdir".to_string(),
        rel_str,
    ];

    if let Some(display) = read_runtime_display() {
        if Path::new("/tmp/.X11-unix").exists() {
            args.push("--bind=/tmp/.X11-unix".to_string());
            args.push(format!("--setenv=DISPLAY={}", display));
        }
    }

    let mut merged_env = workspace_env_vars();
    merged_env.extend(options.env.clone());

    for (key, value) in &merged_env {
        args.push(format!("--setenv={}={}", key, value));
    }

    let shell = resolve_shell(options.shell.as_deref(), Some(&root));
    args.push(shell);
    args.push("-c".to_string());
    args.push(command.to_string());

    run_shell_command("systemd-nspawn", &args, None, options).await
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

        // Validate command against dangerous patterns
        if let Err(msg) = validate_command(command) {
            tracing::warn!("Blocked dangerous command: {}", command);
            return Err(anyhow::anyhow!("{}", msg));
        }

        let cwd = args["cwd"]
            .as_str()
            .map(|p| resolve_path(p, working_dir))
            .unwrap_or_else(|| working_dir.to_path_buf());
        let options = parse_command_options(&args);

        tracing::info!("Executing command in {:?}: {}", cwd, command);

        let output = match container_root_from_env() {
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
