//! Desktop automation tools for controlling graphical applications.
//!
//! This module provides tools for:
//! - Managing Xvfb virtual display sessions
//! - Taking screenshots
//! - Keyboard input (typing)
//! - Mouse operations (clicking)
//! - Extracting visible text (AT-SPI + OCR)
//!
//! Requires: Xvfb, i3, xdotool, scrot, tesseract, AT-SPI2
//! Only available when DESKTOP_ENABLED=true

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::Tool;

/// Global counter for display numbers to avoid conflicts
static DISPLAY_COUNTER: AtomicU32 = AtomicU32::new(99);

/// Check if desktop tools are enabled
pub(crate) fn desktop_enabled() -> bool {
    env_var_bool("DESKTOP_ENABLED") || env_var_bool("SANDBOXED_SH_ENABLE_DESKTOP_TOOLS")
}

fn env_var_bool(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "y" | "on"
            )
        })
        .unwrap_or(false)
}

/// Get the configured resolution
fn get_resolution() -> String {
    std::env::var("DESKTOP_RESOLUTION").unwrap_or_else(|_| "1280x720".to_string())
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

/// Start a new desktop session with Xvfb and i3.
///
/// Creates a virtual X11 display and starts the i3 window manager.
/// Returns the display identifier (e.g., ":99") for use with other desktop tools.
pub struct StartSession;

#[async_trait]
impl Tool for StartSession {
    fn name(&self) -> &str {
        "desktop_start_session"
    }

    fn description(&self) -> &str {
        "Start a virtual desktop session (Xvfb + i3 window manager). Returns the DISPLAY identifier (e.g., ':99') needed for other desktop_* tools. Call this before using any other desktop tools. Optionally launches Chromium browser."
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

        // Get next display number
        let display_num = DISPLAY_COUNTER.fetch_add(1, Ordering::SeqCst);
        let display_id = format!(":{}", display_num);
        let resolution = get_resolution();

        tracing::info!(display = %display_id, resolution = %resolution, "Starting desktop session");

        // Clean up any stale lock files
        let lock_file = format!("/tmp/.X{}-lock", display_num);
        let socket_file = format!("/tmp/.X11-unix/X{}", display_num);
        let _ = std::fs::remove_file(&lock_file);
        let _ = std::fs::remove_file(&socket_file);

        let x11_socket_dir = std::path::Path::new("/tmp/.X11-unix");
        if !x11_socket_dir.exists() {
            std::fs::create_dir_all(x11_socket_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    x11_socket_dir,
                    std::fs::Permissions::from_mode(0o1777),
                );
            }
        }

        // Start Xvfb
        let xvfb_args = format!("{} -screen 0 {}x24", display_id, resolution);
        let mut xvfb = Command::new("Xvfb")
            .args(xvfb_args.split_whitespace())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to start Xvfb: {}. Is Xvfb installed?", e))?;

        let xvfb_pid = xvfb.id().unwrap_or(0);

        // Wait for Xvfb to be ready
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify Xvfb is running
        if let Ok(Some(status)) = xvfb.try_wait() {
            return Err(anyhow::anyhow!(
                "Xvfb exited immediately with status: {:?}",
                status
            ));
        }

        // Start i3 window manager
        let i3 = Command::new("i3")
            .env("DISPLAY", &display_id)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to start i3: {}. Is i3 installed?", e))?;

        let i3_pid = i3.id().unwrap_or(0);

        // Wait for i3 to initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Create screenshots directory in working dir
        let screenshots_dir = working_dir.join("screenshots");
        std::fs::create_dir_all(&screenshots_dir)?;

        // Save session info to a file for cleanup
        let session_file = working_dir.join(format!(".desktop_session_{}", display_num));
        let session_info = json!({
            "display": display_id,
            "display_num": display_num,
            "xvfb_pid": xvfb_pid,
            "i3_pid": i3_pid,
            "resolution": resolution,
            "screenshots_dir": screenshots_dir.to_string_lossy()
        });
        std::fs::write(&session_file, serde_json::to_string_pretty(&session_info)?)?;

        // Optionally launch browser
        let launch_browser = args["launch_browser"].as_bool().unwrap_or(false);
        let browser_info = if launch_browser {
            let url = args["url"].as_str().unwrap_or("about:blank");

            let chromium = Command::new("chromium")
                .args([
                    "--no-sandbox",
                    "--disable-gpu",
                    "--disable-software-rasterizer",
                    "--disable-dev-shm-usage",
                    "--force-renderer-accessibility",
                    url,
                ])
                .env("DISPLAY", &display_id)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to start Chromium: {}", e))?;

            let chromium_pid = chromium.id().unwrap_or(0);

            // Wait for browser to load
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            format!(
                ", \"browser\": \"chromium\", \"browser_pid\": {}, \"url\": \"{}\"",
                chromium_pid, url
            )
        } else {
            String::new()
        };

        Ok(format!(
            "{{\"success\": true, \"display\": \"{}\", \"resolution\": \"{}\", \"xvfb_pid\": {}, \"i3_pid\": {}, \"screenshots_dir\": \"{}\"{}}}",
            display_id,
            resolution,
            xvfb_pid,
            i3_pid,
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
        "Stop a virtual desktop session. Kills Xvfb and all associated processes. Call this when done with desktop automation."
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
                    for pid_key in ["xvfb_pid", "i3_pid", "browser_pid"] {
                        if let Some(pid) = session_info[pid_key].as_u64() {
                            let pid = pid as i32;
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

        // Also kill by display pattern (fallback)
        let _ = Command::new("pkill")
            .args(["-f", &format!("Xvfb {}", display_id)])
            .output()
            .await;

        // Clean up lock files
        let lock_file = format!("/tmp/.X{}-lock", display_num);
        let socket_file = format!("/tmp/.X11-unix/X{}", display_num);
        let _ = std::fs::remove_file(&lock_file);
        let _ = std::fs::remove_file(&socket_file);

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

IMPORTANT: After launching applications with i3 exec commands, use wait_seconds (3-5s recommended) to let them render before capturing. Otherwise the screenshot may be black.

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

        // Build scrot command
        let mut scrot_args = vec!["-o".to_string(), filepath.to_string_lossy().to_string()];

        // Add region if specified
        if let Some(region) = args.get("region") {
            if region.is_object() {
                let x = region["x"].as_i64().unwrap_or(0);
                let y = region["y"].as_i64().unwrap_or(0);
                let w = region["width"].as_i64().unwrap_or(100);
                let h = region["height"].as_i64().unwrap_or(100);
                scrot_args.push("-a".to_string());
                scrot_args.push(format!("{},{},{},{}", x, y, w, h));
            }
        }

        let (_stdout, stderr, exit_code) = run_with_display(
            display_id,
            "scrot",
            &scrot_args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            30,
        )
        .await?;

        if exit_code != 0 {
            // Try import as fallback
            let import_result = run_with_display(
                display_id,
                "import",
                &["-window", "root", filepath.to_string_lossy().as_ref()],
                30,
            )
            .await;

            if let Err(e) = import_result {
                return Err(anyhow::anyhow!(
                    "Screenshot failed. scrot error: {}. import error: {}",
                    stderr,
                    e
                ));
            }
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

        let (_stdout, stderr, exit_code) = run_with_display(
            display_id,
            "xdotool",
            &[command, "--delay", &delay_ms.to_string(), &input],
            30,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("xdotool failed: {}", stderr));
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

        // Move to position first
        let (_, stderr, exit_code) = run_with_display(
            display_id,
            "xdotool",
            &["mousemove", &x.to_string(), &y.to_string()],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("xdotool mousemove failed: {}", stderr));
        }

        // Small delay to ensure move completes
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Click
        let (_, stderr, exit_code) = run_with_display(
            display_id,
            "xdotool",
            &["click", "--repeat", repeat, button],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("xdotool click failed: {}", stderr));
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
            if method_name.ends_with("_error") {
                output.push_str(&format!("--- {} ---\n{}\n\n", method_name, content));
            } else {
                output.push_str(&format!("--- {} ---\n{}\n\n", method_name, content));
            }
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

    // Take screenshot
    let (_, stderr, exit_code) = run_with_display(
        display,
        "scrot",
        &["-o", screenshot_path.to_string_lossy().as_ref()],
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

        let (_, stderr, exit_code) = run_with_display(
            display_id,
            "xdotool",
            &["mousemove", &x.to_string(), &y.to_string()],
            10,
        )
        .await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("xdotool mousemove failed: {}", stderr));
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

        // Move to position if specified
        if let (Some(x), Some(y)) = (args["x"].as_i64(), args["y"].as_i64()) {
            let (_, stderr, exit_code) = run_with_display(
                display_id,
                "xdotool",
                &["mousemove", &x.to_string(), &y.to_string()],
                10,
            )
            .await?;

            if exit_code != 0 {
                return Err(anyhow::anyhow!("xdotool mousemove failed: {}", stderr));
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        tracing::info!(display = %display_id, amount = amount, "Scrolling");

        // xdotool uses button 4 for scroll up, button 5 for scroll down
        let (button, clicks) = if amount >= 0 {
            ("5", amount.unsigned_abs() as usize)
        } else {
            ("4", amount.unsigned_abs() as usize)
        };

        for _ in 0..clicks {
            let (_, stderr, exit_code) =
                run_with_display(display_id, "xdotool", &["click", button], 10).await?;

            if exit_code != 0 {
                return Err(anyhow::anyhow!("xdotool scroll failed: {}", stderr));
            }

            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        Ok(format!(
            "{{\"success\": true, \"amount\": {}, \"direction\": \"{}\"}}",
            amount,
            if amount >= 0 { "down" } else { "up" }
        ))
    }
}

/// Execute i3 window manager commands using i3-msg.
pub struct I3Command;

#[async_trait]
impl Tool for I3Command {
    fn name(&self) -> &str {
        "desktop_i3_command"
    }

    fn description(&self) -> &str {
        "Execute i3 window manager commands using i3-msg. Use this to control window layout and launch applications.

IMPORTANT - Application launch requirements:
- Chromium: MUST use 'exec chromium --no-sandbox' (required when running as root)
- Terminal with command: Use 'exec xterm -hold -e <cmd>' (-hold keeps window open after command exits)
- Example: 'exec xterm -hold -e fastfetch' (shows fastfetch output and stays open)

Common commands:
- exec <app>: Launch an application
- split h/v: Split container horizontally/vertically for next window
- focus left/right/up/down: Focus adjacent window
- move left/right/up/down: Move focused window
- layout tabbed/stacking/splitv/splith: Change container layout
- fullscreen toggle: Toggle fullscreen
- kill: Close focused window

Layout example (chrome left, terminal right):
1. exec chromium --no-sandbox
2. split h
3. exec xterm -hold -e fastfetch"
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

        tracing::info!(display = %display_id, command = %command, "Executing i3 command");

        let (stdout, stderr, exit_code) =
            run_with_display(display_id, "i3-msg", &[command], 30).await?;

        if exit_code != 0 {
            return Err(anyhow::anyhow!("i3-msg failed: {} {}", stdout, stderr));
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
}
