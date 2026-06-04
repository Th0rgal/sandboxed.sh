//! WebSocket-based MJPEG streaming for Wayland app display.
//!
//! Provides real-time streaming of the Wayland compositor output to connected
//! clients over WebSocket using MJPEG frames.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::auth;
use super::routes::AppState;

/// Query parameters for the desktop stream endpoint
#[derive(Debug, Deserialize)]
pub struct StreamParams {
    /// Display identifier (e.g., ":99")
    pub display: String,
    /// Target frames per second (default: 10)
    pub fps: Option<u32>,
    /// JPEG quality 1-100 (default: 70)
    pub quality: Option<u32>,
}

/// Extract JWT from WebSocket subprotocol header
fn extract_jwt_from_protocols(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;
    // Client sends: ["sandboxed", "jwt.<token>"]
    for part in raw.split(',').map(|s| s.trim()) {
        if let Some(rest) = part.strip_prefix("jwt.") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// WebSocket endpoint for streaming desktop as MJPEG
pub async fn desktop_stream_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(params): Query<StreamParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Enforce auth in non-dev mode
    if state.config.auth.auth_required(state.config.dev_mode) {
        let token = match extract_jwt_from_protocols(&headers) {
            Some(t) => t,
            None => return (StatusCode::UNAUTHORIZED, "Missing websocket JWT").into_response(),
        };
        if !auth::verify_token_for_config(&token, &state.config) {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
        }
    }

    // Validate display/session format. The public API keeps the historical
    // ":99" shape, but the backend maps it to a Wayland socket.
    if !params.display.starts_with(':') {
        return (StatusCode::BAD_REQUEST, "Invalid display format").into_response();
    }

    let working_dir = state.config.working_dir.clone();
    ws.protocols(["sandboxed"])
        .on_upgrade(move |socket| handle_desktop_stream(socket, params, working_dir))
}

/// Client command for controlling the stream
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientCommand {
    /// Pause streaming
    #[serde(rename = "pause")]
    Pause,
    /// Resume streaming
    #[serde(rename = "resume")]
    Resume,
    /// Change FPS
    #[serde(rename = "fps")]
    SetFps { fps: u32 },
    /// Change quality
    #[serde(rename = "quality")]
    SetQuality { quality: u32 },
    /// Move mouse to position
    #[serde(rename = "move", alias = "mouse_move")]
    MouseMove { x: i32, y: i32 },
    /// Mouse down (for dragging)
    #[serde(rename = "mouse_down")]
    MouseDown {
        x: i32,
        y: i32,
        button: Option<ClickButton>,
    },
    /// Mouse up (for dragging)
    #[serde(rename = "mouse_up")]
    MouseUp {
        x: i32,
        y: i32,
        button: Option<ClickButton>,
    },
    /// Click mouse button at position
    #[serde(rename = "click")]
    Click {
        x: i32,
        y: i32,
        button: Option<ClickButton>,
        #[serde(default)]
        double: bool,
    },
    /// Scroll mouse wheel (delta in pixels)
    #[serde(rename = "scroll")]
    Scroll {
        amount: Option<i32>,
        delta_x: Option<i32>,
        delta_y: Option<i32>,
        #[serde(default)]
        x: Option<i32>,
        #[serde(default)]
        y: Option<i32>,
    },
    /// Type literal text
    #[serde(rename = "type")]
    Type { text: String, delay_ms: Option<u64> },
    /// Press a key (frontend normalized syntax, e.g. "Return" or "ctrl+shift+T")
    #[serde(rename = "key")]
    Key { key: String, delay_ms: Option<u64> },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ClickButton {
    Name(String),
    Number(u8),
}

/// Handle the WebSocket connection for desktop streaming
async fn handle_desktop_stream(socket: WebSocket, params: StreamParams, working_dir: PathBuf) {
    let display_id = params.display;
    let fps = params.fps.unwrap_or(10).clamp(1, 30);
    let quality = params.quality.unwrap_or(70).clamp(10, 100);

    tracing::info!(
        display_id = %display_id,
        fps = fps,
        quality = quality,
        "Starting Wayland app stream"
    );

    // Channels for client commands
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<ClientCommand>();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<ClientCommand>();
    let (input_err_tx, mut input_err_rx) = mpsc::unbounded_channel::<anyhow::Error>();

    // Split the socket
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Spawn task to handle incoming messages
    let control_tx_clone = control_tx.clone();
    let input_tx_clone = input_tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(cmd) = serde_json::from_str::<ClientCommand>(&t) {
                        match cmd {
                            ClientCommand::Pause
                            | ClientCommand::Resume
                            | ClientCommand::SetFps { .. }
                            | ClientCommand::SetQuality { .. } => {
                                let _ = control_tx_clone.send(cmd);
                            }
                            _ => {
                                let _ = input_tx_clone.send(cmd);
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Streaming state
    let mut paused = false;
    let mut current_quality = quality;
    let mut frame_interval = Duration::from_millis(1000 / fps as u64);

    let input_env = WaylandSessionEnv::from_display(&display_id, &working_dir);
    let mut input_task = tokio::spawn(async move {
        let mut scroll_acc_x: i32 = 0;
        let mut scroll_acc_y: i32 = 0;
        while let Some(cmd) = input_rx.recv().await {
            let result = match cmd {
                ClientCommand::MouseMove { x, y } => run_wlrctl_mouse_move(&input_env, x, y).await,
                ClientCommand::MouseDown { x, y, button } => {
                    let button = resolve_button(button);
                    run_wlrctl_mouse_button(&input_env, x, y, button, true).await
                }
                ClientCommand::MouseUp { x, y, button } => {
                    let button = resolve_button(button);
                    run_wlrctl_mouse_button(&input_env, x, y, button, false).await
                }
                ClientCommand::Click {
                    x,
                    y,
                    button,
                    double,
                } => {
                    let button = resolve_button(button);
                    run_wlrctl_click(&input_env, x, y, button, double).await
                }
                ClientCommand::Scroll {
                    amount,
                    delta_x,
                    delta_y,
                    x,
                    y,
                } => {
                    let (dx, dy) = match (delta_x, delta_y, amount) {
                        (Some(dx), Some(dy), _) => (dx, dy),
                        (Some(dx), None, _) => (dx, 0),
                        (None, Some(dy), _) => (0, dy),
                        (None, None, Some(a)) => (0, a),
                        _ => (0, 0),
                    };
                    scroll_acc_x = scroll_acc_x.saturating_add(dx);
                    scroll_acc_y = scroll_acc_y.saturating_add(dy);

                    let mut steps_x = scroll_acc_x / 120;
                    let mut steps_y = scroll_acc_y / 120;
                    let mut force_x = false;
                    let mut force_y = false;
                    if steps_x == 0 && dx.abs() >= 100 {
                        steps_x = dx.signum();
                        force_x = true;
                    }
                    if steps_y == 0 && dy.abs() >= 100 {
                        steps_y = dy.signum();
                        force_y = true;
                    }
                    scroll_acc_x -= steps_x * 120;
                    scroll_acc_y -= steps_y * 120;
                    if force_x {
                        scroll_acc_x = 0;
                    }
                    if force_y {
                        scroll_acc_y = 0;
                    }

                    run_wlrctl_scroll_steps(&input_env, steps_x, steps_y, x, y).await
                }
                ClientCommand::Type { text, delay_ms } => {
                    run_wtype_type(&input_env, &text, delay_ms).await
                }
                ClientCommand::Key { key, delay_ms } => {
                    run_wtype_key(&input_env, &key, delay_ms).await
                }
                _ => Ok(()),
            };

            if let Err(err) = result {
                let _ = input_err_tx.send(err);
            }
        }
    });

    // Main streaming loop
    let mut stream_task = tokio::spawn(async move {
        let mut frame_count: u64 = 0;

        loop {
            // Check for control commands (non-blocking)
            while let Ok(cmd) = control_rx.try_recv() {
                match cmd {
                    ClientCommand::Pause => {
                        paused = true;
                        tracing::debug!("Stream paused");
                    }
                    ClientCommand::Resume => {
                        paused = false;
                        tracing::debug!("Stream resumed");
                    }
                    ClientCommand::SetFps { fps: new_fps } => {
                        let clamped = new_fps.clamp(1, 30);
                        frame_interval = Duration::from_millis(1000 / clamped as u64);
                        tracing::debug!(fps = clamped, "FPS changed");
                    }
                    ClientCommand::SetQuality {
                        quality: new_quality,
                    } => {
                        current_quality = new_quality.clamp(10, 100);
                        tracing::debug!(quality = current_quality, "Quality changed");
                    }
                    _ => {}
                }
            }

            while let Ok(err) = input_err_rx.try_recv() {
                if send_stream_error(&mut ws_sender, err).await.is_err() {
                    return;
                }
            }

            if paused {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Capture frame
            match capture_frame(&display_id, &working_dir, current_quality).await {
                Ok(jpeg_data) => {
                    frame_count += 1;

                    // Send as binary WebSocket message
                    if ws_sender.send(Message::Binary(jpeg_data)).await.is_err() {
                        tracing::debug!("Client disconnected");
                        break;
                    }
                }
                Err(e) => {
                    // Send error as text message
                    let err_msg = serde_json::json!({
                        "error": "capture_failed",
                        "message": e.to_string()
                    });
                    if ws_sender
                        .send(Message::Text(err_msg.to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    // Wait a bit before retrying on error
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }

            // Wait for next frame
            tokio::time::sleep(frame_interval).await;
        }

        tracing::info!(frames = frame_count, "Wayland app stream ended");
    });

    // Wait for either task to complete, then abort the other to prevent resource waste
    tokio::select! {
        _ = &mut recv_task => {
            stream_task.abort();
            input_task.abort();
        }
        _ = &mut stream_task => {
            recv_task.abort();
            input_task.abort();
        }
        _ = &mut input_task => {
            recv_task.abort();
            stream_task.abort();
        }
    }
}

async fn send_stream_error(
    ws_sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    err: anyhow::Error,
) -> Result<(), ()> {
    let err_msg = serde_json::json!({
        "error": "input_failed",
        "message": err.to_string(),
    });
    ws_sender
        .send(Message::Text(err_msg.to_string()))
        .await
        .map_err(|_| ())
}

fn resolve_button(button: Option<ClickButton>) -> u8 {
    match button {
        Some(ClickButton::Number(num)) => match num {
            2..=7 => num,
            _ => 1,
        },
        Some(ClickButton::Name(name)) => {
            let lowered = name.trim().to_lowercase();
            match lowered.as_str() {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                _ => lowered.parse::<u8>().unwrap_or(1),
            }
        }
        None => 1,
    }
}

#[derive(Clone, Debug)]
struct WaylandSessionEnv {
    xdg_runtime_dir: PathBuf,
    wayland_display: String,
}

impl WaylandSessionEnv {
    fn from_display(display: &str, working_dir: &std::path::Path) -> Self {
        let display_num = display.trim_start_matches(':');
        Self {
            xdg_runtime_dir: working_dir
                .join(".sandboxed-sh")
                .join("wayland")
                .join(display_num),
            wayland_display: "wayland-1".to_string(),
        }
    }
}

fn command_with_wayland_env(program: &str, env: &WaylandSessionEnv) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("XDG_RUNTIME_DIR", &env.xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", &env.wayland_display);
    cmd
}

async fn run_wlrctl_mouse_move(env: &WaylandSessionEnv, x: i32, y: i32) -> anyhow::Result<()> {
    run_wlrctl(env, &["pointer", "move", &x.to_string(), &y.to_string()]).await
}

async fn run_wlrctl_mouse_button(
    env: &WaylandSessionEnv,
    x: i32,
    y: i32,
    button: u8,
    is_down: bool,
) -> anyhow::Result<()> {
    run_wlrctl_mouse_move(env, x, y).await?;
    let action = if is_down { "down" } else { "up" };
    run_wlrctl(env, &["pointer", "button", &button.to_string(), action]).await
}

async fn run_wlrctl_click(
    env: &WaylandSessionEnv,
    x: i32,
    y: i32,
    button: u8,
    double_click: bool,
) -> anyhow::Result<()> {
    let repeat = if double_click { 2 } else { 1 };
    run_wlrctl_mouse_move(env, x, y).await?;
    for idx in 0..repeat {
        run_wlrctl(env, &["pointer", "click", &button.to_string()]).await?;
        if idx + 1 < repeat {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
    Ok(())
}

async fn run_wlrctl_scroll_steps(
    env: &WaylandSessionEnv,
    steps_x: i32,
    steps_y: i32,
    x: Option<i32>,
    y: Option<i32>,
) -> anyhow::Result<()> {
    if steps_x == 0 && steps_y == 0 {
        return Ok(());
    }
    if let (Some(x), Some(y)) = (x, y) {
        run_wlrctl_mouse_move(env, x, y).await?;
    }

    if steps_y != 0 {
        run_wlrctl(
            env,
            &["pointer", "scroll", "vertical", &steps_y.to_string()],
        )
        .await?;
    }

    if steps_x != 0 {
        run_wlrctl(
            env,
            &["pointer", "scroll", "horizontal", &steps_x.to_string()],
        )
        .await?;
    }

    Ok(())
}

async fn run_wtype_type(
    env: &WaylandSessionEnv,
    text: &str,
    delay_ms: Option<u64>,
) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let delay = delay_ms.unwrap_or(1).to_string();
    run_wtype(env, &["-d", &delay, text]).await
}

async fn run_wtype_key(
    env: &WaylandSessionEnv,
    key: &str,
    delay_ms: Option<u64>,
) -> anyhow::Result<()> {
    if key.trim().is_empty() {
        return Ok(());
    }
    let key = key_for_wtype(key);
    let delay = delay_ms.unwrap_or(1).to_string();
    run_wtype(env, &["-d", &delay, "-P", &key, "-p", &key]).await
}

async fn run_wtype(env: &WaylandSessionEnv, args: &[&str]) -> anyhow::Result<()> {
    let output = command_with_wayland_env("wtype", env)
        .args(args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("wtype failed: {}", stderr.trim()));
    }

    Ok(())
}

async fn run_wlrctl(env: &WaylandSessionEnv, args: &[&str]) -> anyhow::Result<()> {
    let output = command_with_wayland_env("wlrctl", env)
        .args(args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run wlrctl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("wlrctl failed: {}", stderr.trim()));
    }

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

/// Capture a single frame from the Wayland display as JPEG.
async fn capture_frame(
    display: &str,
    working_dir: &std::path::Path,
    quality: u32,
) -> anyhow::Result<Vec<u8>> {
    let env = WaylandSessionEnv::from_display(display, working_dir);
    let output = command_with_wayland_env("grim", &env)
        .arg("-")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run grim: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("failed to connect") || stderr.contains("No such file") {
            return Err(anyhow::anyhow!(
                "Wayland display {} is no longer available. The app session may have been closed.",
                display
            ));
        }
        return Err(anyhow::anyhow!(
            "Wayland screenshot failed: {}",
            stderr.trim()
        ));
    }

    let mut convert = Command::new("convert")
        .args(["png:-", "-quality", &quality.to_string(), "jpeg:-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run convert: {}", e))?;

    if let Some(stdin) = convert.stdin.as_mut() {
        stdin.write_all(&output.stdout).await?;
    }
    let converted = convert.wait_with_output().await?;
    if !converted.status.success() {
        let stderr = String::from_utf8_lossy(&converted.stderr);
        return Err(anyhow::anyhow!("JPEG encode failed: {}", stderr.trim()));
    }

    Ok(converted.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_params_defaults() {
        let params = StreamParams {
            display: ":99".to_string(),
            fps: None,
            quality: None,
        };
        assert_eq!(params.fps.unwrap_or(10), 10);
        assert_eq!(params.quality.unwrap_or(70), 70);
    }

    #[test]
    fn test_fps_clamping() {
        assert_eq!(0_u32.clamp(1, 30), 1);
        assert_eq!(50_u32.clamp(1, 30), 30);
        assert_eq!(15_u32.clamp(1, 30), 15);
    }
}
