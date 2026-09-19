//! UI-drivable OAuth login for CLIProxyAPI-owned providers.
//!
//! CLIProxyAPI only exposes OAuth login as interactive CLI flows
//! (`cli-proxy-api -claude-login` etc.): the process prints a provider auth
//! URL and listens on a localhost port for the OAuth callback. On a headless
//! server the user's browser cannot reach that listener, so this module
//! mediates:
//!
//! 1. `POST /api/ai/providers/cli-proxy-login` spawns the login process and
//!    returns the parsed auth URL (+ session id) to the UI.
//! 2. The user authorizes in their browser; the redirect to
//!    `http://localhost:<port>/callback?code=...&state=...` fails locally, so
//!    they paste the full callback URL into the UI.
//! 3. `POST .../cli-proxy-login/:id/callback` replays that URL against the
//!    process's loopback listener on this host, completing the flow. CLIProxyAPI
//!    writes the new auth file and exits.
//! 4. `GET .../cli-proxy-login/:id` reports pending/completed/failed.
//!
//! Sessions expire after `SESSION_TTL` and the child process is killed.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::Path as AxumPath;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const URL_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LoginStatus {
    /// Process spawned, auth URL parsed, waiting for the user.
    Pending,
    /// Callback replayed, waiting for the child to finish writing the file.
    Completing,
    Completed,
    Failed,
}

struct LoginSession {
    auth_url: String,
    /// Loopback listener port for redirect flows (claude/codex/grok). `None`
    /// for device-code flows (kimi), where the CLI polls the provider and
    /// writes the credential on its own once the user authorizes.
    callback_port: Option<u16>,
    oauth_state: Option<String>,
    status: LoginStatus,
    message: Option<String>,
    created_at: Instant,
    child: Option<Child>,
}

#[derive(Default)]
struct LoginSessions {
    inner: Mutex<HashMap<String, LoginSession>>,
}

fn sessions() -> &'static LoginSessions {
    static SESSIONS: OnceLock<LoginSessions> = OnceLock::new();
    SESSIONS.get_or_init(LoginSessions::default)
}

fn login_flag_for(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" | "claude" => Some("-claude-login"),
        "openai" | "codex" => Some("-codex-login"),
        "xai" | "grok" => Some("-grok-login"),
        "kimi" => Some("-kimi-login"),
        _ => None,
    }
}

fn cli_proxy_bin() -> String {
    std::env::var("CLI_PROXY_BIN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .filter(|v| std::path::Path::new(v).exists() || !v.contains('/'))
        .unwrap_or_else(|| {
            for cand in ["/usr/local/bin/cli-proxy-api", "/usr/bin/cli-proxy-api"] {
                if std::path::Path::new(cand).exists() {
                    return cand.to_string();
                }
            }
            "cli-proxy-api".to_string()
        })
}

fn cli_proxy_config() -> Option<String> {
    if let Ok(v) = std::env::var("CLI_PROXY_CONFIG") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    for cand in [
        "/etc/cli-proxy-api/config.yaml",
        "/etc/cli-proxy-api/config.yml",
    ] {
        if std::path::Path::new(cand).exists() {
            return Some(cand.to_string());
        }
    }
    None
}

#[derive(Debug, serde::Deserialize)]
struct StartRequest {
    provider: String,
}

#[derive(Debug, serde::Serialize)]
struct StartResponse {
    session_id: String,
    auth_url: String,
    /// "redirect": the provider redirects to a dead localhost URL the user
    /// pastes back. "device": the provider shows a code on its own page and
    /// the CLI completes on its own — no callback paste needed.
    flow: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct StatusResponse {
    status: LoginStatus,
    auth_url: String,
    message: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct CallbackRequest {
    url: String,
}

fn parse_auth_url(line: &str) -> Option<String> {
    // The CLI logs `Attempting to open URL in browser: https://...`.
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn callback_port_of(auth_url: &str) -> Option<u16> {
    // redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback (URL-encoded)
    let decoded = auth_url.replace("%3A", ":").replace("%2F", "/");
    let idx = decoded.find("localhost:")?;
    let rest = &decoded[idx + "localhost:".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn oauth_state_of(auth_url: &str) -> Option<String> {
    auth_url
        .split('&')
        .find_map(|kv| kv.strip_prefix("state=").map(str::to_string))
        .or_else(|| {
            auth_url
                .split('?')
                .nth(1)?
                .split('&')
                .find_map(|kv| kv.strip_prefix("state=").map(str::to_string))
        })
}

async fn start_login(
    Json(req): Json<StartRequest>,
) -> Result<Json<StartResponse>, (StatusCode, String)> {
    let provider = req.provider.trim().to_lowercase();
    let flag = login_flag_for(&provider).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("no CLI-proxy login flow for provider '{provider}'"),
        )
    })?;

    let bin = cli_proxy_bin();
    let mut cmd = Command::new(&bin);
    if let Some(cfg) = cli_proxy_config() {
        cmd.arg("-config").arg(cfg);
    }
    cmd.arg(flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn {bin}: {e}"),
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no stdout".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no stderr".to_string()))?;

    // The login CLI logs to stdout for some providers and stderr for others;
    // merge both into one line stream and scan for the first https:// URL.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);
    fn spawn_drain(
        stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        tx: tokio::sync::mpsc::Sender<String>,
    ) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        });
    }
    spawn_drain(stdout, line_tx.clone());
    spawn_drain(stderr, line_tx.clone());
    drop(line_tx);

    let deadline = Instant::now() + URL_WAIT;
    let mut auth_url = None;
    let mut early_lines = Vec::new();
    while Instant::now() < deadline {
        let line = match tokio::time::timeout(Duration::from_secs(2), line_rx.recv()).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => continue,
        };
        if let Some(url) = parse_auth_url(&line) {
            auth_url = Some(url);
            break;
        }
        early_lines.push(line);
        if early_lines.len() > 50 {
            break;
        }
    }

    let Some(auth_url) = auth_url else {
        let _ = child.kill().await;
        let detail = early_lines
            .iter()
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("login process printed no auth URL in time ({detail})"),
        ));
    };

    // Redirect flows encode a loopback redirect_uri; device-code flows (kimi)
    // have no listener — the CLI completes by itself after user authorization.
    let port = callback_port_of(&auth_url);
    let flow: &'static str = if port.is_some() { "redirect" } else { "device" };

    // Keep draining stdout so the child never blocks on a full pipe; when the
    // child exits, record the outcome on the session.
    let id = uuid::Uuid::new_v4().to_string();
    let session = LoginSession {
        auth_url: auth_url.clone(),
        callback_port: port,
        oauth_state: oauth_state_of(&auth_url),
        status: LoginStatus::Pending,
        message: None,
        created_at: Instant::now(),
        child: Some(child),
    };
    sessions().inner.lock().await.insert(id.clone(), session);

    // Reap task: keep draining the merged line stream (so the child never
    // blocks on a full pipe), keep a short tail for error reporting, wait for
    // exit, update status, drop the session after TTL.
    let reaper_id = id.clone();
    tokio::spawn(async move {
        let exit_status = {
            let mut child = {
                let mut guard = sessions().inner.lock().await;
                match guard.get_mut(&reaper_id).and_then(|s| s.child.take()) {
                    Some(c) => c,
                    None => return,
                }
            };
            let drain = tokio::spawn(async move {
                let mut tail: std::collections::VecDeque<String> =
                    std::collections::VecDeque::with_capacity(6);
                while let Some(line) = line_rx.recv().await {
                    if tail.len() >= 5 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                tail.into_iter().collect::<Vec<_>>().join(" | ")
            });
            let status = child.wait().await;
            let tail = drain.await.unwrap_or_default();
            (status, tail)
        };
        let mut guard = sessions().inner.lock().await;
        if let Some(s) = guard.get_mut(&reaper_id) {
            match exit_status.0 {
                Ok(st) if st.success() => {
                    s.status = LoginStatus::Completed;
                    s.message = Some("login completed — CLIProxyAPI wrote the credential".into());
                }
                Ok(st) => {
                    // A completed OAuth exchange exits 0; anything else with a
                    // completed session means the flow finished and the process
                    // was reaped after the fact.
                    if s.status != LoginStatus::Completed {
                        s.status = LoginStatus::Failed;
                        s.message =
                            Some(format!("login process exited with {st}: {}", exit_status.1));
                    }
                }
                Err(e) => {
                    s.status = LoginStatus::Failed;
                    s.message = Some(format!("waiting on login process failed: {e}"));
                }
            }
        }
        // Expire the session after TTL from creation regardless of outcome.
        let created = guard.get(&reaper_id).map(|s| s.created_at);
        drop(guard);
        if let Some(created) = created {
            let remaining = SESSION_TTL.saturating_sub(created.elapsed());
            tokio::time::sleep(remaining).await;
            sessions().inner.lock().await.remove(&reaper_id);
        }
    });

    Ok(Json(StartResponse {
        session_id: id,
        auth_url,
        flow,
    }))
}

async fn login_status(
    AxumPath(id): AxumPath<String>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let guard = sessions().inner.lock().await;
    let s = guard
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "login session not found".to_string()))?;
    Ok(Json(StatusResponse {
        status: s.status,
        auth_url: s.auth_url.clone(),
        message: s.message.clone(),
    }))
}

async fn login_callback(
    AxumPath(id): AxumPath<String>,
    Json(req): Json<CallbackRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let (port, expected_state) = {
        let guard = sessions().inner.lock().await;
        let s = guard
            .get(&id)
            .ok_or((StatusCode::NOT_FOUND, "login session not found".to_string()))?;
        let port = s.callback_port.ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "this provider uses a device-code flow — authorize in the browser and it completes automatically"
                    .to_string(),
            )
        })?;
        (port, s.oauth_state.clone())
    };

    let url = req.url.trim();
    let parsed = url::Url::parse(url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid callback URL: {e}"),
        )
    })?;
    let host = parsed.host_str().unwrap_or("");
    if host != "localhost" && host != "127.0.0.1" {
        return Err((
            StatusCode::BAD_REQUEST,
            "callback URL must point at localhost (paste the full redirected URL)".to_string(),
        ));
    }
    if let Some(expected) = expected_state.as_deref() {
        let got = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        if got.as_deref() != Some(expected) {
            return Err((
                StatusCode::BAD_REQUEST,
                "state mismatch — this callback belongs to a different login attempt".to_string(),
            ));
        }
    }

    let path = parsed.path();
    let query = parsed.query().unwrap_or("");
    let target = format!("http://127.0.0.1:{port}{path}?{query}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let upstream = client.get(&target).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("login listener unreachable on :{port} (process gone?): {e}"),
        )
    })?;
    let status_code = upstream.status();
    let body = upstream.text().await.unwrap_or_default();

    let mut guard = sessions().inner.lock().await;
    if let Some(s) = guard.get_mut(&id) {
        if status_code.is_success() {
            s.status = LoginStatus::Completing;
            s.message = Some("callback accepted — finishing the token exchange".into());
        } else {
            s.message = Some(format!(
                "login listener answered HTTP {status_code}: {}",
                &body[..body.len().min(200)]
            ));
        }
        let status = s.status;
        let auth_url = s.auth_url.clone();
        let message = s.message.clone();
        return Ok(Json(StatusResponse {
            status,
            auth_url,
            message,
        }));
    }
    Err((StatusCode::NOT_FOUND, "login session not found".to_string()))
}

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/cli-proxy-login", post(start_login))
        .route("/cli-proxy-login/:id", get(login_status))
        .route("/cli-proxy-login/:id/callback", post(login_callback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auth_url_from_login_output() {
        let line = "Attempting to open URL in browser: https://claude.ai/oauth/authorize?client_id=abc&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=xyz";
        let url = parse_auth_url(line).unwrap();
        assert!(url.starts_with("https://claude.ai/oauth/authorize"));
        assert_eq!(callback_port_of(&url), Some(54545));
        assert_eq!(oauth_state_of(&url), Some("xyz".to_string()));
    }

    #[test]
    fn parses_device_code_url_without_callback_port() {
        let line = "https://www.kimi.com/code/authorize_device?user_code=39WC-FOFX";
        let url = parse_auth_url(line).unwrap();
        assert!(url.starts_with("https://www.kimi.com/code/authorize_device"));
        assert_eq!(callback_port_of(&url), None);
        assert_eq!(oauth_state_of(&url), None);
    }

    #[test]
    fn login_flag_mapping() {
        assert_eq!(login_flag_for("anthropic"), Some("-claude-login"));
        assert_eq!(login_flag_for("openai"), Some("-codex-login"));
        assert_eq!(login_flag_for("xai"), Some("-grok-login"));
        assert_eq!(login_flag_for("unknown"), None);
    }

    /// The merged router mixes our `/cli-proxy-login/:id` wildcards with the
    /// existing `/:id` wildcard — a conflict panics at build time.
    #[test]
    fn merged_router_builds() {
        let _ = crate::api::ai_providers::routes();
    }
}
