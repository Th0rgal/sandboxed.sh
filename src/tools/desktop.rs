//! Desktop automation tools for controlling graphical applications.
//!
//! This module provides tools for:
//! - Managing headless Wayland app sessions
//! - Taking screenshots
//! - Keyboard input (typing)
//! - Mouse operations (clicking)
//! - Extracting visible text (AT-SPI + OCR)
//!
//! Requires: sway, grim, wtype, wlrctl, convert, tesseract, AT-SPI2
//! Only available when DESKTOP_ENABLED=true

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::Tool;
use crate::util::env_var_bool;

/// Global counter for display numbers to avoid conflicts
static DISPLAY_COUNTER: AtomicU32 = AtomicU32::new(99);

/// Check if desktop tools are enabled
pub(crate) fn desktop_enabled() -> bool {
    env_var_bool("DESKTOP_ENABLED", false)
        || env_var_bool("SANDBOXED_SH_ENABLE_DESKTOP_TOOLS", false)
}

fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: Sending SIGTERM to a valid PID. The pid == 0 guard above
    // prevents accidentally signalling the caller's process group.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Get the configured resolution
fn get_resolution() -> String {
    std::env::var("DESKTOP_RESOLUTION").unwrap_or_else(|_| "1280x720".to_string())
}

#[derive(Clone, Debug)]
struct WaylandEnv {
    xdg_runtime_dir: PathBuf,
    wayland_display: String,
    sway_socket: PathBuf,
}

fn display_num(display: &str) -> anyhow::Result<u32> {
    display
        .trim_start_matches(':')
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid display format: {}", display))
}

fn wayland_env_for_display(display: &str, working_dir: &Path) -> anyhow::Result<WaylandEnv> {
    let display_num = display_num(display)?;
    let xdg_runtime_dir = working_dir
        .join(".sandboxed-sh")
        .join("wayland")
        .join(display_num.to_string());
    Ok(WaylandEnv {
        sway_socket: xdg_runtime_dir.join("sway-ipc.sock"),
        xdg_runtime_dir,
        wayland_display: "wayland-1".to_string(),
    })
}

fn configure_wayland_command(cmd: &mut Command, env: &WaylandEnv) {
    cmd.env("XDG_RUNTIME_DIR", &env.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &env.wayland_display)
        .env("SWAYSOCK", &env.sway_socket)
        .env("GDK_BACKEND", "wayland")
        .env("QT_QPA_PLATFORM", "wayland")
        .env("MOZ_ENABLE_WAYLAND", "1");
}

async fn run_with_wayland(
    env: &WaylandEnv,
    program: &str,
    args: &[&str],
    timeout_secs: u64,
) -> anyhow::Result<(String, String, i32)> {
    let mut cmd = Command::new(program);
    configure_wayland_command(&mut cmd, env);
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to execute {}: {}", program, e)),
        Err(_) => return Err(anyhow::anyhow!("Command {} timed out", program)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok((stdout, stderr, exit_code))
}

fn write_sway_config(path: &Path, resolution: &str) -> anyhow::Result<()> {
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
    std::fs::write(path, config)?;
    Ok(())
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

pub fn find_browser_command() -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();

    if let Ok(chromium_bin) = std::env::var("CHROMIUM_BIN") {
        if !chromium_bin.trim().is_empty() {
            candidates.push(chromium_bin);
        }
    }

    if let Ok(browser) = std::env::var("BROWSER") {
        if !browser.trim().is_empty() {
            candidates.extend(browser.split(':').map(|s| s.trim().to_string()));
        }
    }

    candidates.extend(
        [
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
            "brave-browser",
            "microsoft-edge",
            "msedge",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        let candidate_path = resolve_browser_candidate(&candidate);
        if let Some(path) = candidate_path {
            if is_snap_stub(&path) {
                continue;
            }
            return Some(path.to_string_lossy().to_string());
        }
    }

    None
}

fn resolve_browser_candidate(candidate: &str) -> Option<PathBuf> {
    let candidate_path = std::path::Path::new(candidate);
    if candidate_path.is_absolute() || candidate.contains('/') {
        if candidate_path.exists() {
            return Some(candidate_path.to_path_buf());
        }
        return None;
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(candidate);
            if full.exists() {
                return Some(full);
            }
        }
    }
    None
}

fn is_snap_stub(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    // Snap stubs are tiny scripts; skip large binaries.
    if meta.len() > 256 * 1024 {
        return false;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return false,
    };
    let lower = content.to_lowercase();
    lower.contains("snap install") || (lower.contains("snap") && lower.contains("chromium"))
}
/// Run a command with DISPLAY environment variable set
async fn run_with_display(
    display: &str,
    program: &str,
    args: &[&str],
    timeout_secs: u64,
) -> anyhow::Result<(String, String, i32)> {
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new(program)
            .args(args)
            .env("DISPLAY", display)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to execute {}: {}", program, e)),
        Err(_) => return Err(anyhow::anyhow!("Command {} timed out", program)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok((stdout, stderr, exit_code))
}

/// Start a new desktop session with headless Sway.
///
/// Creates a headless Wayland compositor output.
/// Returns the display identifier (e.g., ":99") for use with other desktop tools.
pub struct StartSession;

#[async_trait]
impl Tool for StartSession {
    fn name(&self) -> &str {
        "desktop_start_session"
    }

    fn description(&self) -> &str {
        "Start a headless Wayland app session (Sway compositor). Returns the display identifier (e.g., ':99') needed for other desktop_* tools. Call this before using any other desktop tools. Optionally launches Chromium browser."
    }

    fn parameters_schema(&self) -> Value {
        json!({
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
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        if !desktop_enabled() {
            return Err(anyhow::anyhow!(
                "Desktop tools are disabled. Set DESKTOP_ENABLED=true to enable."
            ));
        }

        let display_num = DISPLAY_COUNTER.fetch_add(1, Ordering::SeqCst);
        let display_id = format!(":{}", display_num);
        let resolution = get_resolution();
        let wayland_env = wayland_env_for_display(&display_id, working_dir)?;

        tracing::info!(display = %display_id, resolution = %resolution, "Starting Wayland desktop session");

        let _ = std::fs::remove_dir_all(&wayland_env.xdg_runtime_dir);
        std::fs::create_dir_all(&wayland_env.xdg_runtime_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &wayland_env.xdg_runtime_dir,
                std::fs::Permissions::from_mode(0o700),
            )?;
        }

        let sway_config = wayland_env.xdg_runtime_dir.join("sway.config");
        write_sway_config(&sway_config, &resolution)?;

        let mut sway_cmd = Command::new("sway");
        configure_wayland_command(&mut sway_cmd, &wayland_env);
        let mut sway = sway_cmd
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
            .map_err(|e| anyhow::anyhow!("Failed to start sway: {}. Is sway installed?", e))?;

        let sway_pid = sway.id().unwrap_or(0);

        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        if let Ok(Some(status)) = sway.try_wait() {
            return Err(anyhow::anyhow!(
                "Sway exited immediately with status: {:?}",
                status
            ));
        }
        let (_, stderr, exit_code) =
            run_with_wayland(&wayland_env, "swaymsg", &["-t", "get_outputs"], 5).await?;
        if exit_code != 0 {
            kill_pid(sway_pid);
            return Err(anyhow::anyhow!("Sway did not become ready: {}", stderr));
        }

        // Create screenshots directory in working dir
        let screenshots_dir = working_dir.join("screenshots");
        std::fs::create_dir_all(&screenshots_dir)?;

        // Optionally launch browser
        let launch_browser = args["launch_browser"].as_bool().unwrap_or(false);
        let (browser_pid, browser_info) = if launch_browser {
            let url = args["url"].as_str().unwrap_or("about:blank");
            let browser_cmd = match find_browser_command() {
                Some(cmd) => cmd,
                None => {
                    kill_pid(sway_pid);
                    return Err(anyhow::anyhow!(
                        "Failed to find a Chromium-compatible browser in PATH. \
                        Set CHROMIUM_BIN or BROWSER, or install chromium/chromium-browser."
                    ));
                }
            };
            let mut browser_command = Command::new(&browser_cmd);
            configure_wayland_command(&mut browser_command, &wayland_env);
            let browser_profile_dir = wayland_env.xdg_runtime_dir.join("browser-profile");
            let mut chromium = browser_command
                .args([
                    "--no-sandbox",
                    "--disable-dev-shm-usage",
                    "--force-renderer-accessibility",
                    "--ozone-platform=wayland",
                    "--enable-features=UseOzonePlatform",
                    "--start-fullscreen",
                    "--new-window",
                    &format!("--user-data-dir={}", browser_profile_dir.display()),
                    url,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| {
                    kill_pid(sway_pid);
                    anyhow::anyhow!("Failed to start Chromium: {}", e)
                })?;

            let chromium_pid = chromium.id().unwrap_or(0);
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if let Ok(Some(status)) = chromium.try_wait() {
                kill_pid(sway_pid);
                return Err(anyhow::anyhow!(
                    "Browser exited immediately with status: {:?}",
                    status
                ));
            }

            // Wait for browser to load
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let _ = run_with_wayland(&wayland_env, "swaymsg", &["fullscreen", "enable"], 5).await;

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
        };
        std::fs::write(&session_file, serde_json::to_string_pretty(&session_info)?)?;

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
}

/// Stop a desktop session and clean up resources.
pub struct StopSession;

#[async_trait]
impl Tool for StopSession {
    fn name(&self) -> &str {
        "desktop_stop_session"
    }

    fn description(&self) -> &str {
        "Stop a Wayland app session. Kills Sway and all associated processes. Call this when done with desktop automation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99') returned by desktop_start_session"
                }
            },
            "required": ["display"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        // Extract display number
        let display_num: u32 = display_id
            .trim_start_matches(':')
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid display format: {}", display_id))?;

        tracing::info!(display = %display_id, "Stopping desktop session");

        // Read session file if it exists
        let session_file = working_dir.join(format!(".desktop_session_{}", display_num));
        let mut killed_pids = Vec::new();

        if session_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&session_file) {
                if let Ok(session_info) = serde_json::from_str::<Value>(&content) {
                    // Kill processes by PID
                    for pid_key in ["sway_pid", "browser_pid", "xvfb_pid", "i3_pid"] {
                        if let Some(pid) = session_info[pid_key].as_u64() {
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

        if let Ok(env) = wayland_env_for_display(display_id, working_dir) {
            let _ = Command::new("pkill")
                .args(["-f", &format!("WAYLAND_DISPLAY={}", env.wayland_display)])
                .output()
                .await;
            let _ = std::fs::remove_dir_all(&env.xdg_runtime_dir);
        }

        Ok(format!(
            "{{\"success\": true, \"display\": \"{}\", \"killed_pids\": {:?}}}",
            display_id, killed_pids
        ))
    }
}

/// Take a screenshot of the desktop.
pub struct Screenshot;

#[async_trait]
impl Tool for Screenshot {
    fn name(&self) -> &str {
        "desktop_screenshot"
    }

    fn description(&self) -> &str {
        "Take a screenshot of the virtual desktop and save it locally.

IMPORTANT: After launching applications, use wait_seconds (3-5s recommended) to let them render before capturing. Otherwise the screenshot may be black.

Set return_image=true to SEE the screenshot yourself (vision). This lets you verify the layout is correct before responding."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99') from desktop_start_session"
                },
                "wait_seconds": {
                    "type": "number",
                    "description": "Seconds to wait before taking screenshot. Use 3-5 seconds after launching apps (chromium, xterm, etc.) to let them render. Default: 0"
                },
                "return_image": {
                    "type": "boolean",
                    "description": "If true, the screenshot image will be included in your context so you can SEE it (requires vision model). Use this to verify the desktop layout is correct. Default: false"
                },
                "description": {
                    "type": "string",
                    "description": "Description for the image (default: 'screenshot')"
                },
                "filename": {
                    "type": "string",
                    "description": "Optional filename for the screenshot (default: auto-generated with timestamp)"
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
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        // Wait before taking screenshot if specified (for apps to render)
        let wait_seconds = args["wait_seconds"].as_f64().unwrap_or(0.0);
        if wait_seconds > 0.0 {
            tracing::info!(display = %display_id, wait_seconds = wait_seconds, "Waiting before screenshot");
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait_seconds)).await;
        }

        // Generate filename
        let filename = args["filename"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                format!("screenshot_{}.png", timestamp)
            });

        // Ensure screenshots directory exists
        let screenshots_dir = working_dir.join("screenshots");
        std::fs::create_dir_all(&screenshots_dir)?;

        let filepath = screenshots_dir.join(&filename);

        tracing::info!(display = %display_id, path = %filepath.display(), "Taking screenshot");

        let wayland_env = wayland_env_for_display(display_id, working_dir)?;
        let mut grim_args: Vec<String> = Vec::new();

        // Add region if specified
        if let Some(region) = args.get("region") {
            if region.is_object() {
                let x = region["x"].as_i64().unwrap_or(0);
                let y = region["y"].as_i64().unwrap_or(0);
                let w = region["width"].as_i64().unwrap_or(100);
                let h = region["height"].as_i64().unwrap_or(100);
                grim_args.push("-g".to_string());
                grim_args.push(format!("{},{} {}x{}", x, y, w, h));
            }
        }
        grim_args.push(filepath.to_string_lossy().to_string());

        let (_stdout, stderr, exit_code) = run_with_wayland(
            &wayland_env,
            "grim",
            &grim_args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            30,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("Screenshot failed. grim error: {}", stderr));
        }

        // Verify file exists
        if !filepath.exists() {
            return Err(anyhow::anyhow!("Screenshot file was not created"));
        }

        let metadata = std::fs::metadata(&filepath)?;
        let return_image = args["return_image"].as_bool().unwrap_or(false);

        // Include vision marker if return_image is true
        let vision_marker = if return_image {
            format!("\n\n[VISION_IMAGE:file://{}]", filepath.display())
        } else {
            String::new()
        };

        Ok(format!(
            "{{\"success\": true, \"path\": \"{}\", \"size_bytes\": {}}}{}",
            filepath.display(),
            metadata.len(),
            vision_marker
        ))
    }
}

/// Send keyboard input to the desktop.
pub struct TypeText;

#[async_trait]
impl Tool for TypeText {
    fn name(&self) -> &str {
        "desktop_type"
    }

    fn description(&self) -> &str {
        "Send keyboard input to the virtual desktop. Can type text or send special keys (Return, Tab, Escape, ctrl+a, alt+F4, etc.). Text is typed into the currently focused window."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (provide either 'text' OR 'key', not both). For special keys, use key names: 'Return', 'Tab', 'Escape', 'BackSpace', 'Delete', 'Up', 'Down', 'Left', 'Right', 'Home', 'End', 'Page_Up', 'Page_Down', 'F1'-'F12'"
                },
                "key": {
                    "type": "string",
                    "description": "Send a key combination instead of typing text (provide either 'text' OR 'key', not both). Examples: 'Return', 'ctrl+a', 'alt+F4', 'ctrl+shift+t', 'super+Return'"
                },
                "delay_ms": {
                    "type": "integer",
                    "description": "Delay between keystrokes in milliseconds (default: 12, increase for slow applications)"
                }
            },
            "required": ["display"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let delay_ms = args["delay_ms"].as_u64().unwrap_or(12);

        let (command, input) = if let Some(text) = args["text"].as_str() {
            // Type text character by character
            ("type", text.to_string())
        } else if let Some(key) = args["key"].as_str() {
            // Send key combination
            ("key", key.to_string())
        } else {
            return Err(anyhow::anyhow!("Either 'text' or 'key' must be provided"));
        };

        tracing::info!(display = %display_id, command = %command, "Sending keyboard input");

        let wayland_env = wayland_env_for_display(display_id, _working_dir)?;
        let args = if command == "type" {
            vec!["-d".to_string(), delay_ms.to_string(), input.clone()]
        } else {
            vec![
                "-d".to_string(),
                delay_ms.to_string(),
                "-P".to_string(),
                key_for_wtype(&input),
                "-p".to_string(),
                key_for_wtype(&input),
            ]
        };
        let (_stdout, stderr, exit_code) = run_with_wayland(
            &wayland_env,
            "wtype",
            &args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            30,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("wtype failed: {}", stderr));
        }

        Ok(format!(
            "{{\"success\": true, \"command\": \"{}\", \"input\": \"{}\"}}",
            command,
            input.replace('\"', "\\\"").replace('\n', "\\n")
        ))
    }
}

/// Click at a position on the desktop.
pub struct Click;

#[async_trait]
impl Tool for Click {
    fn name(&self) -> &str {
        "desktop_click"
    }

    fn description(&self) -> &str {
        "Click at a specific position on the virtual desktop. Supports left, middle, right click and double-click. Coordinates are in pixels from top-left (0,0)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate in pixels from left edge"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate in pixels from top edge"
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "middle", "right"],
                    "description": "Mouse button to click (default: 'left')"
                },
                "double": {
                    "type": "boolean",
                    "description": "If true, perform a double-click (default: false)"
                },
                "hold_ms": {
                    "type": "integer",
                    "description": "Hold the click for this many milliseconds (for drag operations, use with move)"
                }
            },
            "required": ["display", "x", "y"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let x = args["x"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'x' argument"))?;
        let y = args["y"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'y' argument"))?;

        let button = match args["button"].as_str().unwrap_or("left") {
            "left" => "1",
            "middle" => "2",
            "right" => "3",
            other => return Err(anyhow::anyhow!("Invalid button: {}", other)),
        };

        let double = args["double"].as_bool().unwrap_or(false);
        let repeat = if double { "2" } else { "1" };

        tracing::info!(display = %display_id, x = x, y = y, button = button, "Clicking");

        let wayland_env = wayland_env_for_display(display_id, _working_dir)?;
        let (_, stderr, exit_code) = run_with_wayland(
            &wayland_env,
            "wlrctl",
            &["pointer", "move", &x.to_string(), &y.to_string()],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("wlrctl pointer move failed: {}", stderr));
        }
        for _ in 0..repeat.parse::<usize>().unwrap_or(1) {
            let (_, stderr, exit_code) =
                run_with_wayland(&wayland_env, "wlrctl", &["pointer", "click", button], 10).await?;
            if exit_code != 0 {
                return Err(anyhow::anyhow!("wlrctl pointer click failed: {}", stderr));
            }
        }

        Ok(format!(
            "{{\"success\": true, \"x\": {}, \"y\": {}, \"button\": \"{}\", \"double\": {}}}",
            x,
            y,
            args["button"].as_str().unwrap_or("left"),
            double
        ))
    }
}

/// Extract visible text from the desktop using AT-SPI or OCR.
pub struct GetText;

#[async_trait]
impl Tool for GetText {
    fn name(&self) -> &str {
        "desktop_get_text"
    }

    fn description(&self) -> &str {
        "Extract visible text from the virtual desktop. Uses the accessibility tree (AT-SPI) for structured output with element types, or falls back to OCR (Tesseract) for raw text. The accessibility tree provides better structure for web pages and applications."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "method": {
                    "type": "string",
                    "enum": ["accessibility", "ocr", "both"],
                    "description": "Method to extract text. 'accessibility' uses AT-SPI (best for browsers/apps), 'ocr' uses Tesseract (works on any content), 'both' tries accessibility first then OCR (default: 'accessibility')"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth to traverse in accessibility tree (default: 10)"
                }
            },
            "required": ["display"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let method = args["method"].as_str().unwrap_or("accessibility");
        let max_depth = args["max_depth"].as_u64().unwrap_or(10);

        tracing::info!(display = %display_id, method = %method, "Extracting text");

        let mut results = Vec::new();

        // Try accessibility tree
        if method == "accessibility" || method == "both" {
            match get_accessibility_text(display_id, max_depth).await {
                Ok(text) if !text.trim().is_empty() => {
                    results.push(("accessibility", text));
                }
                Ok(_) => {
                    tracing::debug!("Accessibility tree returned empty");
                }
                Err(e) => {
                    tracing::warn!("Accessibility tree extraction failed: {}", e);
                    if method == "accessibility" {
                        // Only fail if accessibility was the only method
                        results.push(("accessibility_error", e.to_string()));
                    }
                }
            }
        }

        // Try OCR
        if method == "ocr" || (method == "both" && results.is_empty()) {
            match get_ocr_text(display_id, working_dir).await {
                Ok(text) => {
                    results.push(("ocr", text));
                }
                Err(e) => {
                    tracing::warn!("OCR extraction failed: {}", e);
                    results.push(("ocr_error", e.to_string()));
                }
            }
        }

        // Format output
        if results.is_empty() {
            return Err(anyhow::anyhow!("No text extraction method succeeded"));
        }

        let mut output = String::new();
        for (method_name, content) in results {
            output.push_str(&format!("--- {} ---\n{}\n\n", method_name, content));
        }

        Ok(output.trim().to_string())
    }
}

/// Extract text using AT-SPI accessibility tree
async fn get_accessibility_text(display: &str, max_depth: u64) -> anyhow::Result<String> {
    // Python script to extract accessibility tree
    let python_script = format!(
        r#"
import gi
import sys
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi

def get_text(obj, depth=0, max_depth={}):
    if depth > max_depth:
        return ""
    
    result = []
    try:
        name = obj.get_name() or ""
        role = obj.get_role_name()
        
        # Get text content if available
        text = ""
        try:
            text_iface = obj.get_text()
            if text_iface:
                text = text_iface.get_text(0, text_iface.get_character_count())
        except:
            pass
        
        # Include meaningful content
        if name or text:
            indent = "  " * depth
            content = text or name
            if content.strip():
                result.append(f"{{indent}}[{{role}}] {{content[:500]}}")
        
        # Recurse into children
        for i in range(obj.get_child_count()):
            child = obj.get_child_at_index(i)
            if child:
                child_text = get_text(child, depth + 1, max_depth)
                if child_text:
                    result.append(child_text)
    except Exception as e:
        pass
    
    return "\n".join(result)

try:
    desktop = Atspi.get_desktop(0)
    output = []
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app:
            app_text = get_text(app, 0, {})
            if app_text.strip():
                output.append(app_text)
    print("\n".join(output))
except Exception as e:
    print(f"Error: {{e}}", file=sys.stderr)
    sys.exit(1)
"#,
        max_depth, max_depth
    );

    let (stdout, stderr, exit_code) =
        run_with_display(display, "python3", &["-c", &python_script], 30).await?;

    if exit_code != 0 {
        return Err(anyhow::anyhow!("AT-SPI extraction failed: {}", stderr));
    }

    Ok(stdout)
}

/// Extract text using OCR (Tesseract)
async fn get_ocr_text(display: &str, working_dir: &Path) -> anyhow::Result<String> {
    // Take a screenshot first
    let screenshots_dir = working_dir.join("screenshots");
    std::fs::create_dir_all(&screenshots_dir)?;

    let screenshot_path = screenshots_dir.join("_ocr_temp.png");

    let wayland_env = wayland_env_for_display(display, working_dir)?;
    let (_, stderr, exit_code) = run_with_wayland(
        &wayland_env,
        "grim",
        &[screenshot_path.to_string_lossy().as_ref()],
        30,
    )
    .await?;

    if exit_code != 0 {
        return Err(anyhow::anyhow!(
            "Failed to take screenshot for OCR: {}",
            stderr
        ));
    }

    // Run tesseract
    let output = Command::new("tesseract")
        .args([
            screenshot_path.to_string_lossy().as_ref(),
            "stdout",
            "-l",
            "eng",
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run tesseract: {}", e))?;

    // Clean up temp screenshot
    let _ = std::fs::remove_file(&screenshot_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Tesseract failed: {}", stderr));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(text)
}

/// Move the mouse to a position (without clicking).
pub struct MouseMove;

#[async_trait]
impl Tool for MouseMove {
    fn name(&self) -> &str {
        "desktop_mouse_move"
    }

    fn description(&self) -> &str {
        "Move the mouse cursor to a specific position without clicking. Useful for hover effects or preparing for drag operations."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate in pixels from left edge"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate in pixels from top edge"
                }
            },
            "required": ["display", "x", "y"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let x = args["x"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'x' argument"))?;
        let y = args["y"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'y' argument"))?;

        tracing::info!(display = %display_id, x = x, y = y, "Moving mouse");

        let wayland_env = wayland_env_for_display(display_id, _working_dir)?;
        let (_, stderr, exit_code) = run_with_wayland(
            &wayland_env,
            "wlrctl",
            &["pointer", "move", &x.to_string(), &y.to_string()],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("wlrctl pointer move failed: {}", stderr));
        }

        Ok(format!("{{\"success\": true, \"x\": {}, \"y\": {}}}", x, y))
    }
}

/// Scroll the mouse wheel.
pub struct Scroll;

#[async_trait]
impl Tool for Scroll {
    fn name(&self) -> &str {
        "desktop_scroll"
    }

    fn description(&self) -> &str {
        "Scroll the mouse wheel at the current position or at specified coordinates. Positive amount scrolls down, negative scrolls up."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "amount": {
                    "type": "integer",
                    "description": "Scroll amount. Positive = down, negative = up. Each unit is typically one 'click' of the scroll wheel."
                },
                "x": {
                    "type": "integer",
                    "description": "Optional: X coordinate to scroll at (moves mouse first)"
                },
                "y": {
                    "type": "integer",
                    "description": "Optional: Y coordinate to scroll at (moves mouse first)"
                }
            },
            "required": ["display", "amount"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let amount = args["amount"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'amount' argument"))?;

        let wayland_env = wayland_env_for_display(display_id, _working_dir)?;

        // Move to position if specified
        if let (Some(x), Some(y)) = (args["x"].as_i64(), args["y"].as_i64()) {
            let (_, stderr, exit_code) = run_with_wayland(
                &wayland_env,
                "wlrctl",
                &["pointer", "move", &x.to_string(), &y.to_string()],
                10,
            )
            .await?;

            if exit_code != 0 {
                return Err(anyhow::anyhow!("wlrctl pointer move failed: {}", stderr));
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        tracing::info!(display = %display_id, amount = amount, "Scrolling");

        let (_, stderr, exit_code) = run_with_wayland(
            &wayland_env,
            "wlrctl",
            &["pointer", "scroll", "vertical", &amount.to_string()],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("wlrctl scroll failed: {}", stderr));
        }

        Ok(format!(
            "{{\"success\": true, \"amount\": {}, \"direction\": \"{}\"}}",
            amount,
            if amount >= 0 { "down" } else { "up" }
        ))
    }
}

/// Execute Sway compositor commands using swaymsg.
pub struct I3Command;

#[async_trait]
impl Tool for I3Command {
    fn name(&self) -> &str {
        "desktop_i3_command"
    }

    fn description(&self) -> &str {
        "Execute Sway compositor commands using swaymsg. Use this to control the focused Wayland app or launch apps.

IMPORTANT - Application launch requirements:
- Chromium: use 'exec chromium --no-sandbox --ozone-platform=wayland'
- Prefer one focused app per session; fullscreen the selected app.

Common commands:
- exec <app>: Launch an application
- fullscreen toggle: Toggle fullscreen
- kill: Close focused window
"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "string",
                    "description": "The display identifier (e.g., ':99')"
                },
                "command": {
                    "type": "string",
                    "description": "The i3 command to execute (e.g., 'exec chromium', 'split h', 'focus right')"
                }
            },
            "required": ["display", "command"]
        })
    }

    async fn execute(&self, args: Value, _working_dir: &Path) -> anyhow::Result<String> {
        let display_id = args["display"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'display' argument"))?;

        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;

        tracing::info!(display = %display_id, command = %command, "Executing sway command");

        let wayland_env = wayland_env_for_display(display_id, _working_dir)?;
        let (stdout, stderr, exit_code) =
            run_with_wayland(&wayland_env, "swaymsg", &[command], 30).await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("swaymsg failed: {} {}", stdout, stderr));
        }

        // Parse swaymsg JSON output if present
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
}
