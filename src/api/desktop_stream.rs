//! WebSocket-based MJPEG streaming for virtual desktop display.
//!
//! Provides real-time streaming of the X11 virtual desktop (Xvfb)
//! to connected clients over WebSocket using MJPEG frames.

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
    // Client sends: ["openagent"|"sandboxed", "jwt.<token>"]
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

    // Validate display format
    if !params.display.starts_with(':') {
        return (StatusCode::BAD_REQUEST, "Invalid display format").into_response();
    }

    ws.protocols(["openagent", "sandboxed"])
        .on_upgrade(move |socket| handle_desktop_stream(socket, params))
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
    /// Press a key (xdotool syntax, e.g. "Return" or "ctrl+shift+T")
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
async fn handle_desktop_stream(socket: WebSocket, params: StreamParams) {
    let x11_display = params.display;
    let fps = params.fps.unwrap_or(10).clamp(1, 30);
    let quality = params.quality.unwrap_or(70).clamp(10, 100);

    tracing::info!(
        x11_display = %x11_display,
        fps = fps,
        quality = quality,
        "Starting desktop stream"
    );

    // Channel for control commands from client
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ClientCommand>();

    // Split the socket
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Spawn task to handle incoming messages
    let cmd_tx_clone = cmd_tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(cmd) = serde_json::from_str::<ClientCommand>(&t) {
                        let _ = cmd_tx_clone.send(cmd);
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

    // Main streaming loop
    let mut stream_task = tokio::spawn(async move {
        let mut frame_count: u64 = 0;

        loop {
            // Check for control commands (non-blocking)
            while let Ok(cmd) = cmd_rx.try_recv() {
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
                    ClientCommand::MouseMove { x, y } => {
                        if let Err(err) = run_xdotool_mouse_move(&x11_display, x, y).await {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                    ClientCommand::MouseDown { x, y, button } => {
                        let button = resolve_button(button);
                        if let Err(err) =
                            run_xdotool_mouse_button(&x11_display, x, y, button, true).await
                        {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                    ClientCommand::MouseUp { x, y, button } => {
                        let button = resolve_button(button);
                        if let Err(err) =
                            run_xdotool_mouse_button(&x11_display, x, y, button, false).await
                        {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                    ClientCommand::Click {
                        x,
                        y,
                        button,
                        double,
                    } => {
                        let button = resolve_button(button);
                        if let Err(err) =
                            run_xdotool_click(&x11_display, x, y, button, double).await
                        {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
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
                        if let Err(err) = run_xdotool_scroll(&x11_display, dx, dy, x, y).await {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                    ClientCommand::Type { text, delay_ms } => {
                        if let Err(err) = run_xdotool_type(&x11_display, &text, delay_ms).await {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                    ClientCommand::Key { key, delay_ms } => {
                        if let Err(err) = run_xdotool_key(&x11_display, &key, delay_ms).await {
                            if send_stream_error(&mut ws_sender, err).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            if paused {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Capture frame
            match capture_frame(&x11_display, current_quality).await {
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

        tracing::info!(frames = frame_count, "Desktop stream ended");
    });

    // Wait for either task to complete, then abort the other to prevent resource waste
    tokio::select! {
        _ = &mut recv_task => {
            stream_task.abort();
        }
        _ = &mut stream_task => {
            recv_task.abort();
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
            2 | 3 | 4 | 5 | 6 | 7 => num,
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

async fn run_xdotool_mouse_move(display: &str, x: i32, y: i32) -> anyhow::Result<()> {
    run_xdotool(
        display,
        &["mousemove", "--sync", &x.to_string(), &y.to_string()],
    )
    .await
}

async fn run_xdotool_mouse_button(
    display: &str,
    x: i32,
    y: i32,
    button: u8,
    is_down: bool,
) -> anyhow::Result<()> {
    run_xdotool(
        display,
        &["mousemove", "--sync", &x.to_string(), &y.to_string()],
    )
    .await?;
    let cmd = if is_down { "mousedown" } else { "mouseup" };
    run_xdotool(display, &[cmd, &button.to_string()]).await
}

async fn run_xdotool_click(
    display: &str,
    x: i32,
    y: i32,
    button: u8,
    double_click: bool,
) -> anyhow::Result<()> {
    run_xdotool(
        display,
        &["mousemove", "--sync", &x.to_string(), &y.to_string()],
    )
    .await?;
    if double_click {
        run_xdotool(
            display,
            &[
                "click",
                "--repeat",
                "2",
                "--delay",
                "40",
                &button.to_string(),
            ],
        )
        .await
    } else {
        run_xdotool(display, &["click", &button.to_string()]).await
    }
}

async fn run_xdotool_scroll(
    display: &str,
    delta_x: i32,
    delta_y: i32,
    x: Option<i32>,
    y: Option<i32>,
) -> anyhow::Result<()> {
    if let (Some(x), Some(y)) = (x, y) {
        run_xdotool(
            display,
            &["mousemove", "--sync", &x.to_string(), &y.to_string()],
        )
        .await?;
    }

    let steps_y = (delta_y.abs() / 120).max(1).min(10);
    let steps_x = (delta_x.abs() / 120).max(1).min(10);

    if delta_y != 0 {
        let button = if delta_y > 0 { "4" } else { "5" };
        run_xdotool(
            display,
            &["click", "--repeat", &steps_y.to_string(), button],
        )
        .await?;
    }

    if delta_x != 0 {
        let button = if delta_x > 0 { "6" } else { "7" };
        run_xdotool(
            display,
            &["click", "--repeat", &steps_x.to_string(), button],
        )
        .await?;
    }

    Ok(())
}

async fn run_xdotool_type(display: &str, text: &str, delay_ms: Option<u64>) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let delay = delay_ms.unwrap_or(1).to_string();
    run_xdotool(
        display,
        &["type", "--delay", &delay, "--clearmodifiers", text],
    )
    .await
}

async fn run_xdotool_key(display: &str, key: &str, delay_ms: Option<u64>) -> anyhow::Result<()> {
    if key.trim().is_empty() {
        return Ok(());
    }
    let delay = delay_ms.unwrap_or(1).to_string();
    run_xdotool(
        display,
        &["key", "--delay", &delay, "--clearmodifiers", key],
    )
    .await
}

async fn run_xdotool(display: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("xdotool")
        .args(args)
        .env("DISPLAY", display)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("xdotool failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Capture a single frame from the X11 display as JPEG
async fn capture_frame(display: &str, quality: u32) -> anyhow::Result<Vec<u8>> {
    // Use import from ImageMagick to capture and convert directly to JPEG
    // This avoids writing to disk and is more efficient
    let output = Command::new("import")
        .args([
            "-window",
            "root",
            "-quality",
            &quality.to_string(),
            "jpeg:-", // Output JPEG to stdout
        ])
        .env("DISPLAY", display)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run import: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Detect common error patterns and return user-friendly messages
        if stderr.contains("unable to open X server") {
            return Err(anyhow::anyhow!(
                "Display {} is no longer available. The desktop session may have been closed.",
                display
            ));
        }
        if stderr.contains("Can't open display") || stderr.contains("cannot open display") {
            return Err(anyhow::anyhow!(
                "Cannot connect to display {}. The session may have ended.",
                display
            ));
        }

        return Err(anyhow::anyhow!("Screenshot failed: {}", stderr.trim()));
    }

    Ok(output.stdout)
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
