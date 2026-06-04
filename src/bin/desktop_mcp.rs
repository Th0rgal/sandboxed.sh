//! MCP Server for Desktop Tools
//!
//! This binary exposes the sandboxed.sh desktop tools (Sway, Wayland screenshots, etc.)
//! as an MCP server that can be used with OpenCode or other MCP-compatible clients.
//!
//! Communicates over stdio using JSON-RPC 2.0.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use sandboxed_sh::tools::desktop::find_browser_command;

/// Global counter for display numbers to avoid conflicts.
/// Seeds from $DISPLAY env var so each workspace gets its own display range,
/// preventing collisions on the shared /tmp/.X11-unix socket directory.
static DISPLAY_COUNTER: LazyLock<AtomicU32> = LazyLock::new(|| {
    let start = std::env::var("DISPLAY")
        .ok()
        .and_then(|d| d.trim_start_matches(':').parse::<u32>().ok())
        .unwrap_or(99);
    AtomicU32::new(start)
});

// =============================================================================
// JSON-RPC Types
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    /// JSON-RPC 2.0 notifications don't have an id field, so this must be optional
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// =============================================================================
// MCP Types
// =============================================================================

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ToolResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

// =============================================================================
// Desktop Tool Implementations
// =============================================================================

fn get_resolution() -> String {
    std::env::var("DESKTOP_RESOLUTION").unwrap_or_else(|_| "1280x720".to_string())
}

#[derive(Clone, Debug)]
struct WaylandEnv {
    xdg_runtime_dir: PathBuf,
    wayland_display: String,
    sway_socket: PathBuf,
}

fn display_num(display: &str) -> Result<u32, String> {
    display
        .trim_start_matches(':')
        .parse()
        .map_err(|_| format!("Invalid display format: {}", display))
}

fn wayland_env_for_display(display: &str) -> Result<WaylandEnv, String> {
    let display_num = display_num(display)?;
    let xdg_runtime_dir = get_working_dir()
        .join(".sandboxed-sh")
        .join("wayland")
        .join(display_num.to_string());
    Ok(WaylandEnv {
        sway_socket: xdg_runtime_dir.join("sway-ipc.sock"),
        xdg_runtime_dir,
        wayland_display: "wayland-1".to_string(),
    })
}

fn configure_wayland_command(cmd: &mut std::process::Command, env: &WaylandEnv) {
    cmd.env("XDG_RUNTIME_DIR", &env.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &env.wayland_display)
        .env("SWAYSOCK", &env.sway_socket)
        .env("GDK_BACKEND", "wayland")
        .env("QT_QPA_PLATFORM", "wayland")
        .env("MOZ_ENABLE_WAYLAND", "1");
}

fn run_with_wayland(
    env: &WaylandEnv,
    program: &str,
    args: &[&str],
) -> Result<(String, String, i32), String> {
    let mut cmd = std::process::Command::new(program);
    configure_wayland_command(&mut cmd, env);
    let output = cmd
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to execute {}: {}", program, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok((stdout, stderr, exit_code))
}

fn write_sway_config(path: &std::path::Path, resolution: &str) -> Result<(), String> {
    let config = format!(
        r#"output * resolution {resolution}
default_border none
default_floating_border none
gaps inner 0
gaps outer 0
focus_follows_mouse no
seat * hide_cursor 5000
exec_always true
"#
    );
    std::fs::write(path, config).map_err(|e| format!("Failed to write sway config: {}", e))
}

fn key_for_wtype(key: &str) -> String {
    key.split('+')
        .map(|part| match part {
            "ctrl" => "leftctrl",
            "alt" => "leftalt",
            "shift" => "leftshift",
            "super" => "leftmeta",
            "Return" => "enter",
            "BackSpace" => "backspace",
            "Escape" => "esc",
            "Page_Up" => "pageup",
            "Page_Down" => "pagedown",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn get_working_dir() -> PathBuf {
    std::env::var("WORKING_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn runtime_display_path() -> PathBuf {
    get_working_dir()
        .join(".sandboxed-sh")
        .join("runtime")
        .join("current_display.json")
}

fn write_display_info(display: &str) -> Result<(), String> {
    let path = runtime_display_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create runtime dir: {}", e))?;
    }
    let payload = json!({
        "display": display,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(path, serde_json::to_string_pretty(&payload).unwrap())
        .map_err(|e| format!("Failed to write display info: {}", e))?;
    Ok(())
}

fn clear_display_info_if_current(display: &str) {
    let path = runtime_display_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Ok(payload) = serde_json::from_str::<Value>(&contents) {
        if payload
            .get("display")
            .and_then(|v| v.as_str())
            .map(|current| current == display)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

// -----------------------------------------------------------------------------
// Tool: desktop_start_session
// -----------------------------------------------------------------------------

/// Helper to kill a process by PID (best effort)
fn kill_process(pid: u32) {
    use std::process::Command;
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
}

fn tool_start_session(args: &Value) -> Result<String, String> {
    let display_num = DISPLAY_COUNTER.fetch_add(1, Ordering::SeqCst);
    let display_id = format!(":{}", display_num);
    let resolution = get_resolution();
    let wayland_env = wayland_env_for_display(&display_id)?;

    let _ = std::fs::remove_dir_all(&wayland_env.xdg_runtime_dir);
    std::fs::create_dir_all(&wayland_env.xdg_runtime_dir)
        .map_err(|e| format!("Failed to create runtime dir: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &wayland_env.xdg_runtime_dir,
            std::fs::Permissions::from_mode(0o700),
        )
        .map_err(|e| format!("Failed to set runtime dir permissions: {}", e))?;
    }

    let sway_config = wayland_env.xdg_runtime_dir.join("sway.config");
    write_sway_config(&sway_config, &resolution)?;

    let mut sway_cmd = std::process::Command::new("sway");
    configure_wayland_command(&mut sway_cmd, &wayland_env);
    let mut sway = match sway_cmd
        .args([
            "--unsupported-gpu",
            "-c",
            sway_config.to_string_lossy().as_ref(),
        ])
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(sway) => sway,
        Err(e) => {
            return Err(format!("Failed to start sway: {}. Is sway installed?", e));
        }
    };

    let sway_pid = sway.id();

    std::thread::sleep(std::time::Duration::from_millis(900));
    if let Ok(Some(status)) = sway.try_wait() {
        return Err(format!("Sway exited immediately with status: {:?}", status));
    }
    let (_, stderr, exit_code) = run_with_wayland(&wayland_env, "swaymsg", &["-t", "get_outputs"])?;
    if exit_code != 0 {
        kill_process(sway_pid);
        return Err(format!("Sway did not become ready: {}", stderr));
    }

    // Create screenshots directory - cleanup on failure
    let working_dir = get_working_dir();
    let screenshots_dir = working_dir.join("screenshots");
    if let Err(e) = std::fs::create_dir_all(&screenshots_dir) {
        kill_process(sway_pid);
        return Err(format!("Failed to create screenshots dir: {}", e));
    }

    // Optionally launch browser
    let launch_browser = args
        .get("launch_browser")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (browser_pid, browser_info) = if launch_browser {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("about:blank");

        let browser_cmd = match find_browser_command() {
            Some(cmd) => cmd,
            None => {
                kill_process(sway_pid);
                return Err(
                    "Failed to start Chromium: no browser binary found (set CHROMIUM_BIN or BROWSER)"
                        .to_string(),
                );
            }
        };

        let mut browser_command = std::process::Command::new(&browser_cmd);
        configure_wayland_command(&mut browser_command, &wayland_env);
        let browser_profile_dir = wayland_env.xdg_runtime_dir.join("browser-profile");
        let mut chromium = match browser_command
            .args([
                // Security/sandbox (required for running as root)
                "--no-sandbox",
                "--disable-setuid-sandbox",
                "--disable-dev-shm-usage",
                // Accessibility for automation
                "--force-renderer-accessibility",
                // Suppress dialogs and prompts for LLM automation
                "--disable-infobars",               // "Restore pages?" bar
                "--disable-session-crashed-bubble", // Crash recovery dialog
                "--disable-restore-session-state",  // Don't restore previous session
                "--no-first-run",                   // Skip first-run wizard
                "--disable-translate",              // No translate prompts
                "--disable-default-apps",           // No app suggestions
                "--disable-popup-blocking",         // Allow popups for automation
                "--disable-prompt-on-repost",       // No repost warnings
                "--disable-hang-monitor",           // No unresponsive page dialogs
                "--disable-client-side-phishing-detection",
                // Clean profile behavior
                "--disable-background-networking", // No background requests
                "--disable-sync",                  // No sync prompts
                "--disable-extensions",            // No extension prompts
                "--ozone-platform=wayland",
                "--enable-features=UseOzonePlatform",
                // Window behavior
                "--start-fullscreen",
                "--new-window",
                &format!("--user-data-dir={}", browser_profile_dir.display()),
                url,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                kill_process(sway_pid);
                return Err(format!("Failed to start Chromium: {}", e));
            }
        };

        let chromium_pid = chromium.id();
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Ok(Some(status)) = chromium.try_wait() {
            kill_process(sway_pid);
            return Err(format!(
                "Browser exited immediately with status: {:?}",
                status
            ));
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        let _ = run_with_wayland(&wayland_env, "swaymsg", &["fullscreen", "enable"]);

        (
            Some(chromium_pid),
            format!(
                ", \"browser\": \"{}\", \"browser_pid\": {}, \"url\": \"{}\"",
                browser_cmd, chromium_pid, url
            ),
        )
    } else {
        (None, String::new())
    };

    // Save session info (including browser_pid if launched)
    let session_file = working_dir.join(format!(".desktop_session_{}", display_num));
    let mut session_info = json!({
        "display": display_id,
        "display_num": display_num,
        "sway_pid": sway_pid,
        "display_server": "wayland",
        "compositor": "sway-headless",
        "resolution": resolution,
        "xdg_runtime_dir": wayland_env.xdg_runtime_dir.to_string_lossy(),
        "wayland_display": wayland_env.wayland_display,
        "sway_socket": wayland_env.sway_socket.to_string_lossy(),
        "screenshots_dir": screenshots_dir.to_string_lossy()
    });
    if let Some(pid) = browser_pid {
        session_info["browser_pid"] = json!(pid);
    }
    if let Err(e) = std::fs::write(
        &session_file,
        serde_json::to_string_pretty(&session_info).unwrap(),
    ) {
        if let Some(pid) = browser_pid {
            kill_process(pid);
        }
        kill_process(sway_pid);
        return Err(format!("Failed to write session file: {}", e));
    }

    write_display_info(&display_id)?;

    Ok(format!(
        "{{\"success\": true, \"display\": \"{}\", \"display_server\": \"wayland\", \"compositor\": \"sway-headless\", \"resolution\": \"{}\", \"sway_pid\": {}, \"wayland_display\": \"{}\", \"xdg_runtime_dir\": \"{}\", \"screenshots_dir\": \"{}\"{}}}",
        display_id,
        resolution,
        sway_pid,
        wayland_env.wayland_display,
        wayland_env.xdg_runtime_dir.display(),
        screenshots_dir.display(),
        browser_info
    ))
}

// -----------------------------------------------------------------------------
// Tool: desktop_stop_session
// -----------------------------------------------------------------------------

fn tool_stop_session(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let display_num: u32 = display_id
        .trim_start_matches(':')
        .parse()
        .map_err(|_| format!("Invalid display format: {}", display_id))?;

    let working_dir = get_working_dir();
    let session_file = working_dir.join(format!(".desktop_session_{}", display_num));
    let mut killed_pids = Vec::new();

    if session_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&session_file) {
            if let Ok(session_info) = serde_json::from_str::<Value>(&content) {
                for pid_key in ["sway_pid", "browser_pid", "xvfb_pid", "i3_pid"] {
                    if let Some(pid) = session_info.get(pid_key).and_then(|v| v.as_u64()) {
                        let pid = pid as i32;
                        // SAFETY: PIDs are read from a session file we wrote;
                        // SIGTERM is a safe signal to send to any process.
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                        killed_pids.push(pid);
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&session_file);
    }

    if let Ok(env) = wayland_env_for_display(display_id) {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &format!("WAYLAND_DISPLAY={}", env.wayland_display)])
            .output();
        let _ = std::fs::remove_dir_all(&env.xdg_runtime_dir);
    }
    clear_display_info_if_current(display_id);

    Ok(format!(
        "{{\"success\": true, \"display\": \"{}\", \"killed_pids\": {:?}}}",
        display_id, killed_pids
    ))
}

// -----------------------------------------------------------------------------
// Tool: desktop_screenshot
// -----------------------------------------------------------------------------

fn tool_screenshot(args: &Value) -> Result<(String, Option<String>), String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    // Wait before taking screenshot if specified
    let wait_seconds = args
        .get("wait_seconds")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if wait_seconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(wait_seconds));
    }

    // Generate filename
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = args
        .get("filename")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("screenshot_{}.png", timestamp));

    let working_dir = get_working_dir();
    let screenshots_dir = working_dir.join("screenshots");
    std::fs::create_dir_all(&screenshots_dir)
        .map_err(|e| format!("Failed to create screenshots dir: {}", e))?;

    let filepath = screenshots_dir.join(&filename);

    let wayland_env = wayland_env_for_display(display_id)?;
    let mut grim_args: Vec<String> = Vec::new();

    // Add region if specified
    if let Some(region) = args.get("region") {
        if region.is_object() {
            let x = region.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = region.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let w = region.get("width").and_then(|v| v.as_i64()).unwrap_or(100);
            let h = region.get("height").and_then(|v| v.as_i64()).unwrap_or(100);
            grim_args.push("-g".to_string());
            grim_args.push(format!("{},{} {}x{}", x, y, w, h));
        }
    }
    grim_args.push(filepath.to_string_lossy().to_string());

    let grim_args_refs: Vec<&str> = grim_args.iter().map(|s| s.as_str()).collect();
    let (_stdout, stderr, exit_code) = run_with_wayland(&wayland_env, "grim", &grim_args_refs)?;

    if exit_code != 0 {
        return Err(format!("Screenshot failed. grim error: {}", stderr));
    }

    if !filepath.exists() {
        return Err("Screenshot file was not created".to_string());
    }

    let metadata =
        std::fs::metadata(&filepath).map_err(|e| format!("Failed to read file metadata: {}", e))?;

    // Check if we should return the image data
    let return_image = args
        .get("return_image")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let image_data = if return_image {
        let data =
            std::fs::read(&filepath).map_err(|e| format!("Failed to read screenshot: {}", e))?;
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &data,
        ))
    } else {
        None
    };

    let result = format!(
        "{{\"success\": true, \"path\": \"{}\", \"size_bytes\": {}}}",
        filepath.display(),
        metadata.len()
    );

    Ok((result, image_data))
}

// -----------------------------------------------------------------------------
// Tool: desktop_type
// -----------------------------------------------------------------------------

fn tool_type_text(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let delay_ms = args.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(12);

    let (command, input) = if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
        ("type", text.to_string())
    } else if let Some(key) = args.get("key").and_then(|v| v.as_str()) {
        ("key", key.to_string())
    } else {
        return Err("Either 'text' or 'key' must be provided".to_string());
    };

    let wayland_env = wayland_env_for_display(display_id)?;
    let delay_str = delay_ms.to_string();
    let wtype_args = if command == "type" {
        vec!["-d".to_string(), delay_str, input.clone()]
    } else {
        vec![
            "-d".to_string(),
            delay_str,
            "-P".to_string(),
            key_for_wtype(&input),
            "-p".to_string(),
            key_for_wtype(&input),
        ]
    };
    let refs: Vec<&str> = wtype_args.iter().map(|s| s.as_str()).collect();
    let (_stdout, stderr, exit_code) = run_with_wayland(&wayland_env, "wtype", &refs)?;

    if exit_code != 0 {
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(format!(
        "{{\"success\": true, \"command\": \"{}\", \"input\": \"{}\"}}",
        command,
        input.replace('\"', "\\\"").replace('\n', "\\n")
    ))
}

// -----------------------------------------------------------------------------
// Tool: desktop_click
// -----------------------------------------------------------------------------

fn tool_click(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let x = args
        .get("x")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'x' argument")?;
    let y = args
        .get("y")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'y' argument")?;

    let button = match args
        .get("button")
        .and_then(|v| v.as_str())
        .unwrap_or("left")
    {
        "left" => "1",
        "middle" => "2",
        "right" => "3",
        other => return Err(format!("Invalid button: {}", other)),
    };

    let double = args
        .get("double")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let repeat = if double { "2" } else { "1" };

    let wayland_env = wayland_env_for_display(display_id)?;

    // Move to position first
    let x_str = x.to_string();
    let y_str = y.to_string();
    let (_, stderr, exit_code) =
        run_with_wayland(&wayland_env, "wlrctl", &["pointer", "move", &x_str, &y_str])?;

    if exit_code != 0 {
        return Err(format!("wlrctl pointer move failed: {}", stderr));
    }

    std::thread::sleep(std::time::Duration::from_millis(50));

    for _ in 0..repeat.parse::<usize>().unwrap_or(1) {
        let (_, stderr, exit_code) =
            run_with_wayland(&wayland_env, "wlrctl", &["pointer", "click", button])?;
        if exit_code != 0 {
            return Err(format!("wlrctl pointer click failed: {}", stderr));
        }
    }

    Ok(format!(
        "{{\"success\": true, \"x\": {}, \"y\": {}, \"button\": \"{}\", \"double\": {}}}",
        x,
        y,
        args.get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left"),
        double
    ))
}

// -----------------------------------------------------------------------------
// Tool: desktop_mouse_move
// -----------------------------------------------------------------------------

fn tool_mouse_move(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let x = args
        .get("x")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'x' argument")?;
    let y = args
        .get("y")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'y' argument")?;

    let x_str = x.to_string();
    let y_str = y.to_string();
    let wayland_env = wayland_env_for_display(display_id)?;
    let (_, stderr, exit_code) =
        run_with_wayland(&wayland_env, "wlrctl", &["pointer", "move", &x_str, &y_str])?;

    if exit_code != 0 {
        return Err(format!("wlrctl pointer move failed: {}", stderr));
    }

    Ok(format!("{{\"success\": true, \"x\": {}, \"y\": {}}}", x, y))
}

// -----------------------------------------------------------------------------
// Tool: desktop_scroll
// -----------------------------------------------------------------------------

fn tool_scroll(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let amount = args
        .get("amount")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'amount' argument")?;

    let wayland_env = wayland_env_for_display(display_id)?;

    // Move to position if specified
    if let (Some(x), Some(y)) = (
        args.get("x").and_then(|v| v.as_i64()),
        args.get("y").and_then(|v| v.as_i64()),
    ) {
        let x_str = x.to_string();
        let y_str = y.to_string();
        let (_, stderr, exit_code) =
            run_with_wayland(&wayland_env, "wlrctl", &["pointer", "move", &x_str, &y_str])?;

        if exit_code != 0 {
            return Err(format!("wlrctl pointer move failed: {}", stderr));
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let (_, stderr, exit_code) = run_with_wayland(
        &wayland_env,
        "wlrctl",
        &["pointer", "scroll", "vertical", &amount.to_string()],
    )?;
    if exit_code != 0 {
        return Err(format!("wlrctl scroll failed: {}", stderr));
    }

    Ok(format!(
        "{{\"success\": true, \"amount\": {}, \"direction\": \"{}\"}}",
        amount,
        if amount >= 0 { "down" } else { "up" }
    ))
}

// -----------------------------------------------------------------------------
// Tool: desktop_i3_command
// -----------------------------------------------------------------------------

fn tool_i3_command(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'command' argument")?;

    let wayland_env = wayland_env_for_display(display_id)?;
    let (stdout, stderr, exit_code) = run_with_wayland(&wayland_env, "swaymsg", &[command])?;

    if exit_code != 0 {
        return Err(format!("swaymsg failed: {} {}", stdout, stderr));
    }

    // Parse i3-msg JSON output if present
    let result = if stdout.trim().starts_with('[') || stdout.trim().starts_with('{') {
        stdout.trim().to_string()
    } else {
        format!(
            "{{\"success\": true, \"output\": \"{}\"}}",
            stdout.trim().replace('"', "\\\"")
        )
    };

    Ok(result)
}

// -----------------------------------------------------------------------------
// Tool: desktop_get_text (OCR only for simplicity in MCP server)
// -----------------------------------------------------------------------------

fn tool_get_text(args: &Value) -> Result<String, String> {
    let display_id = args
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'display' argument")?;

    let working_dir = get_working_dir();
    let screenshots_dir = working_dir.join("screenshots");
    std::fs::create_dir_all(&screenshots_dir)
        .map_err(|e| format!("Failed to create screenshots dir: {}", e))?;

    let screenshot_path = screenshots_dir.join("_ocr_temp.png");

    let wayland_env = wayland_env_for_display(display_id)?;
    let (_, stderr, exit_code) = run_with_wayland(
        &wayland_env,
        "grim",
        &[screenshot_path.to_string_lossy().as_ref()],
    )?;

    if exit_code != 0 {
        return Err(format!("Failed to take screenshot for OCR: {}", stderr));
    }

    // Run tesseract
    let output = std::process::Command::new("tesseract")
        .args([
            screenshot_path.to_string_lossy().as_ref(),
            "stdout",
            "-l",
            "eng",
        ])
        .output()
        .map_err(|e| format!("Failed to run tesseract: {}", e))?;

    // Clean up temp screenshot
    let _ = std::fs::remove_file(&screenshot_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Tesseract failed: {}", stderr));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(format!("--- OCR Text ---\n{}", text.trim()))
}

// =============================================================================
// Tool Registry
// =============================================================================

fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "desktop_start_session".to_string(),
            description: "Start a virtual desktop session (Xvfb + i3 window manager). Returns the DISPLAY identifier (e.g., ':99') needed for other desktop_* tools. Call this before using any other desktop tools. Optionally launches Chromium browser.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "launch_browser": {
                        "type": "boolean",
                        "description": "If true, automatically launch Chromium browser after starting the session (default: false)"
                    },
                    "url": {
                        "type": "string",
                        "description": "Optional URL to open in Chromium (only used if launch_browser is true)"
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "desktop_stop_session".to_string(),
            description: "Stop a virtual desktop session. Kills Xvfb and all associated processes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99') returned by desktop_start_session"
                    }
                },
                "required": ["display"]
            }),
        },
        ToolDefinition {
            name: "desktop_screenshot".to_string(),
            description: "Take a screenshot of the virtual desktop. Use wait_seconds (3-5s recommended) after launching apps to let them render.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "wait_seconds": {
                        "type": "number",
                        "description": "Seconds to wait before taking screenshot (default: 0)"
                    },
                    "return_image": {
                        "type": "boolean",
                        "description": "If true, return the image data as base64 for vision analysis (default: false)"
                    },
                    "filename": {
                        "type": "string",
                        "description": "Optional filename for the screenshot"
                    },
                    "region": {
                        "type": "object",
                        "description": "Optional region to capture (x, y, width, height)",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "width": { "type": "integer" },
                            "height": { "type": "integer" }
                        }
                    }
                },
                "required": ["display"]
            }),
        },
        ToolDefinition {
            name: "desktop_type".to_string(),
            description: "Send keyboard input. Provide 'text' to type characters or 'key' for special keys (Return, Tab, Escape, ctrl+a, alt+F4, etc.).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (provide either 'text' OR 'key', not both)"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key combination (e.g., 'Return', 'ctrl+a', 'alt+F4')"
                    },
                    "delay_ms": {
                        "type": "integer",
                        "description": "Delay between keystrokes in milliseconds (default: 12)"
                    }
                },
                "required": ["display"]
            }),
        },
        ToolDefinition {
            name: "desktop_click".to_string(),
            description: "Click at a specific position. Coordinates are in pixels from top-left (0,0).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "x": {
                        "type": "integer",
                        "description": "X coordinate in pixels"
                    },
                    "y": {
                        "type": "integer",
                        "description": "Y coordinate in pixels"
                    },
                    "button": {
                        "type": "string",
                        "enum": ["left", "middle", "right"],
                        "description": "Mouse button (default: 'left')"
                    },
                    "double": {
                        "type": "boolean",
                        "description": "Double-click (default: false)"
                    }
                },
                "required": ["display", "x", "y"]
            }),
        },
        ToolDefinition {
            name: "desktop_mouse_move".to_string(),
            description: "Move the mouse cursor without clicking.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "x": {
                        "type": "integer",
                        "description": "X coordinate"
                    },
                    "y": {
                        "type": "integer",
                        "description": "Y coordinate"
                    }
                },
                "required": ["display", "x", "y"]
            }),
        },
        ToolDefinition {
            name: "desktop_scroll".to_string(),
            description: "Scroll the mouse wheel. Positive = down, negative = up.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Scroll amount (positive = down, negative = up)"
                    },
                    "x": {
                        "type": "integer",
                        "description": "Optional: X coordinate to scroll at"
                    },
                    "y": {
                        "type": "integer",
                        "description": "Optional: Y coordinate to scroll at"
                    }
                },
                "required": ["display", "amount"]
            }),
        },
        ToolDefinition {
            name: "desktop_i3_command".to_string(),
            description: "Execute i3 window manager commands. Use 'exec chromium --no-sandbox' to launch apps, 'split h/v' for layout, 'focus left/right/up/down' for navigation.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    },
                    "command": {
                        "type": "string",
                        "description": "The i3 command (e.g., 'exec chromium --no-sandbox', 'split h', 'focus right')"
                    }
                },
                "required": ["display", "command"]
            }),
        },
        ToolDefinition {
            name: "desktop_get_text".to_string(),
            description: "Extract visible text from the desktop using OCR (Tesseract).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "display": {
                        "type": "string",
                        "description": "The display identifier (e.g., ':99')"
                    }
                },
                "required": ["display"]
            }),
        },
    ]
}

fn execute_tool(name: &str, args: &Value) -> ToolResult {
    let result = match name {
        "desktop_start_session" => tool_start_session(args),
        "desktop_stop_session" => tool_stop_session(args),
        "desktop_screenshot" => match tool_screenshot(args) {
            Ok((text, Some(image_data))) => {
                return ToolResult {
                    content: vec![
                        ToolContent::Text { text },
                        ToolContent::Image {
                            data: image_data,
                            mime_type: "image/png".to_string(),
                        },
                    ],
                    is_error: false,
                };
            }
            Ok((text, None)) => Ok(text),
            Err(e) => Err(e),
        },
        "desktop_type" => tool_type_text(args),
        "desktop_click" => tool_click(args),
        "desktop_mouse_move" => tool_mouse_move(args),
        "desktop_scroll" => tool_scroll(args),
        "desktop_i3_command" => tool_i3_command(args),
        "desktop_get_text" => tool_get_text(args),
        _ => Err(format!("Unknown tool: {}", name)),
    };

    match result {
        Ok(text) => ToolResult {
            content: vec![ToolContent::Text { text }],
            is_error: false,
        },
        Err(e) => ToolResult {
            content: vec![ToolContent::Text { text: e }],
            is_error: true,
        },
    }
}

// =============================================================================
// MCP Server Main Loop
// =============================================================================

fn handle_request(request: JsonRpcRequest) -> Option<JsonRpcResponse> {
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            request.id,
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "desktop-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": {
                        "listChanged": false
                    }
                }
            }),
        )),

        "notifications/initialized" | "initialized" => {
            // JSON-RPC 2.0: "The Server MUST NOT reply to a Notification"
            None
        }

        "tools/list" => {
            let tools = get_tool_definitions();
            Some(JsonRpcResponse::success(
                request.id,
                json!({
                    "tools": tools
                }),
            ))
        }

        "tools/call" => {
            let name = request
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));

            let result = execute_tool(name, &args);
            Some(JsonRpcResponse::success(request.id, json!(result)))
        }

        _ => Some(JsonRpcResponse::error(
            request.id,
            -32601,
            format!("Method not found: {}", request.method),
        )),
    }
}

fn main() {
    // Log to stderr so it doesn't interfere with JSON-RPC on stdout
    eprintln!("[desktop-mcp] Starting MCP server for desktop tools...");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[desktop-mcp] Error reading stdin: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        eprintln!("[desktop-mcp] Received: {}", line);

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[desktop-mcp] Parse error: {}", e);
                let response = JsonRpcResponse::error(Value::Null, -32700, "Parse error");
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = writeln!(stdout, "{}", json);
                }
                let _ = stdout.flush();
                continue;
            }
        };

        // Only send response if it's not a notification (per JSON-RPC 2.0 spec)
        if let Some(response) = handle_request(request) {
            let json = match serde_json::to_string(&response) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("[desktop-mcp] Failed to serialize response: {}", e);
                    continue;
                }
            };
            eprintln!("[desktop-mcp] Sending: {}", json);

            if let Err(e) = writeln!(stdout, "{}", json) {
                eprintln!("[desktop-mcp] Error writing response: {}", e);
                break;
            }
            let _ = stdout.flush();
        } else {
            eprintln!("[desktop-mcp] Notification received, no response sent");
        }
    }

    eprintln!("[desktop-mcp] Server shutting down");
}
