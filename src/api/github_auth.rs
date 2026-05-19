//! GitHub OAuth login flow.
//!
//! Two unprotected endpoints implement the standard OAuth Authorization Code
//! grant against `github.com`:
//!
//! ```text
//!  Android  --GET /api/auth/github/start?redirect=sandboxed://auth/callback-->  Server
//!  Android  <--302 https://github.com/login/oauth/authorize?...---  Server
//!  Android  --(user signs in)--> github.com --302 ...?code=&state=--> Server
//!  Server   --POST /login/oauth/access_token--> github.com
//!  Server   --GET /user--> github.com
//!  Server   --302 sandboxed://auth/callback?token=<jwt>&exp=<ts>--> Android
//! ```
//!
//! Required env vars (see `AuthConfig`):
//! - `GITHUB_OAUTH_CLIENT_ID`, `GITHUB_OAUTH_CLIENT_SECRET`: the GitHub OAuth App.
//! - `GITHUB_OAUTH_ALLOWLIST`: comma-separated GitHub usernames (or `*`) permitted to sign in.
//! - `GITHUB_OAUTH_REDIRECT_ALLOWLIST`: comma-separated redirect URIs (defaults to `sandboxed://auth/callback`).
//! - `SANDBOXED_PUBLIC_URL`: public base URL for the server (used to build the GitHub `redirect_uri`).
//! - `JWT_SECRET`: reused from the password flow to sign issued JWTs.
//!
//! State is held in-memory (`AppState::pending_github_oauth`) keyed by a
//! cryptographically random nonce; entries expire after 10 minutes.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use rand::RngCore;
use serde::Deserialize;

use super::auth::{issue_jwt, AuthUser};
use super::routes::AppState;

/// Pending OAuth nonce → original redirect URI requested by the client.
#[derive(Debug, Clone)]
pub struct PendingGithubOAuth {
    pub redirect_uri: String,
    pub expires_at: i64,
}

/// 10 minutes.
const STATE_TTL_SECONDS: i64 = 600;

#[derive(Debug, Deserialize)]
pub struct StartParams {
    /// Redirect target after the OAuth dance. Must be on the allowlist.
    pub redirect: String,
}

#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    login: String,
    id: u64,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn server_base_url(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(configured) = state.config.auth.public_base_url.as_deref() {
        return configured.trim_end_matches('/').to_string();
    }
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<StartParams>,
) -> Result<Redirect, (StatusCode, String)> {
    if !state.config.auth.github_enabled() {
        return Err((StatusCode::NOT_FOUND, "GitHub OAuth not configured".into()));
    }

    if !state.config.auth.github_redirect_allowed(&params.redirect) {
        return Err((
            StatusCode::BAD_REQUEST,
            "redirect URI not on allowlist".into(),
        ));
    }

    let client_id = state.config.auth.github_oauth_client_id.as_deref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "missing client id".into(),
    ))?;

    let nonce = random_state();
    {
        let mut pending = state.pending_github_oauth.write().await;
        // Garbage-collect expired entries.
        let cutoff = now_unix();
        pending.retain(|_, v| v.expires_at > cutoff);
        pending.insert(
            nonce.clone(),
            PendingGithubOAuth {
                redirect_uri: params.redirect.clone(),
                expires_at: cutoff + STATE_TTL_SECONDS,
            },
        );
    }

    let server_redirect = format!(
        "{}/api/auth/github/callback",
        server_base_url(&state, &headers)
    );
    let authorize = format!(
        "https://github.com/login/oauth/authorize?client_id={}&state={}&redirect_uri={}&scope={}",
        urlencoding::encode(client_id),
        urlencoding::encode(&nonce),
        urlencoding::encode(&server_redirect),
        urlencoding::encode("read:user"),
    );
    Ok(Redirect::to(&authorize))
}

pub async fn callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
) -> Response {
    if !state.config.auth.github_enabled() {
        return (StatusCode::NOT_FOUND, "GitHub OAuth not configured").into_response();
    }

    let Some(nonce) = params.state.clone() else {
        return (StatusCode::BAD_REQUEST, "missing state").into_response();
    };

    // Pop the pending entry and validate.
    let pending = {
        let mut store = state.pending_github_oauth.write().await;
        store.remove(&nonce)
    };

    let Some(pending) = pending else {
        return (StatusCode::BAD_REQUEST, "unknown or expired state").into_response();
    };

    if pending.expires_at <= now_unix() {
        return (StatusCode::BAD_REQUEST, "state expired").into_response();
    }

    if let Some(err) = params.error.as_deref() {
        let msg = params.error_description.as_deref().unwrap_or(err);
        return redirect_with_error(&pending.redirect_uri, msg);
    }

    let Some(code) = params.code.as_deref() else {
        return redirect_with_error(&pending.redirect_uri, "missing authorization code");
    };

    let token_url = "https://github.com/login/oauth/access_token";
    let client_id = state
        .config
        .auth
        .github_oauth_client_id
        .as_deref()
        .unwrap_or("");
    let client_secret = state
        .config
        .auth
        .github_oauth_client_secret
        .as_deref()
        .unwrap_or("");

    let token_resp: GithubTokenResponse = match state
        .http_client
        .post(token_url)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
        ])
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(parsed) => parsed,
            Err(e) => {
                return redirect_with_error(&pending.redirect_uri, &format!("token parse: {e}"))
            }
        },
        Err(e) => {
            return redirect_with_error(&pending.redirect_uri, &format!("token exchange: {e}"))
        }
    };

    let access_token = match token_resp.access_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            let msg = token_resp
                .error_description
                .or(token_resp.error)
                .unwrap_or_else(|| "no access token".to_string());
            return redirect_with_error(&pending.redirect_uri, &msg);
        }
    };

    let user: GithubUser = match state
        .http_client
        .get("https://api.github.com/user")
        .bearer_auth(&access_token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "sandboxed-dashboard")
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(parsed) => parsed,
            Err(e) => {
                return redirect_with_error(&pending.redirect_uri, &format!("user parse: {e}"))
            }
        },
        Err(e) => return redirect_with_error(&pending.redirect_uri, &format!("fetch user: {e}")),
    };

    if !state.config.auth.github_user_allowed(&user.login) {
        return redirect_with_error(&pending.redirect_uri, "GitHub login not in allowlist");
    }

    let auth_user = AuthUser {
        id: format!("github:{}", user.id),
        username: user.login.clone(),
    };

    let secret = match state.config.auth.jwt_secret.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return redirect_with_error(&pending.redirect_uri, "JWT_SECRET not configured"),
    };

    let (jwt, exp) = match issue_jwt(secret, state.config.auth.jwt_ttl_days, &auth_user) {
        Ok(v) => v,
        Err(e) => return redirect_with_error(&pending.redirect_uri, &format!("issue jwt: {e}")),
    };

    let target = format!(
        "{}{}token={}&exp={}",
        pending.redirect_uri,
        if pending.redirect_uri.contains('?') {
            "&"
        } else {
            "?"
        },
        urlencoding::encode(&jwt),
        exp,
    );
    Redirect::to(&target).into_response()
}

fn redirect_with_error(redirect_uri: &str, message: &str) -> Response {
    let target = format!(
        "{}{}error={}",
        redirect_uri,
        if redirect_uri.contains('?') { "&" } else { "?" },
        urlencoding::encode(message),
    );
    Redirect::to(&target).into_response()
}
