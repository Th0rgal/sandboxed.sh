//! Minimal JWT auth for the dashboard (single-tenant).
//!
//! - Dashboard submits a password to `/api/auth/login`
//! - Server returns a JWT valid for ~30 days
//! - When `DEV_MODE=false`, all API endpoints require `Authorization: Bearer <jwt>`
//!
//! # Security notes
//! - This is intentionally minimal; it is NOT multi-tenant and does not implement RLS.
//! - Use a strong `JWT_SECRET` in production.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};

use super::routes::AppState;
use super::types::{LoginRequest, LoginResponse};
use crate::config::{AuthMode, Config, UserAccount};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Claims {
    /// Subject (we only need a stable sentinel)
    sub: String,
    /// Username (for display/auditing)
    #[serde(default)]
    usr: String,
    /// Issued-at unix seconds
    iat: i64,
    /// Expiration unix seconds
    exp: i64,
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
}

pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a_bytes.len() {
        diff |= a_bytes[i] ^ b_bytes[i];
    }
    diff == 0
}

/// Hash a password using PBKDF2-SHA256.
/// Returns a string in the format `pbkdf2:100000:<hex_salt>:<hex_hash>`.
pub fn hash_password(password: &str) -> String {
    use hmac::Hmac;
    use pbkdf2::pbkdf2;
    use rand::RngCore;
    use sha2::Sha256;

    let iterations = 100_000u32;
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);

    let mut hash = [0u8; 32];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), &salt, iterations, &mut hash)
        .expect("PBKDF2 should not fail");

    format!(
        "pbkdf2:{}:{}:{}",
        iterations,
        hex::encode(salt),
        hex::encode(hash)
    )
}

/// Verify a password against a stored PBKDF2 hash string.
pub fn verify_password_hash(password: &str, stored: &str) -> bool {
    use hmac::Hmac;
    use pbkdf2::pbkdf2;
    use sha2::Sha256;

    let parts: Vec<&str> = stored.split(':').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2" {
        return false;
    }

    let iterations: u32 = match parts[1].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let salt = match hex::decode(parts[2]) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let expected_hash = match hex::decode(parts[3]) {
        Ok(h) => h,
        Err(_) => return false,
    };

    let mut computed = vec![0u8; expected_hash.len()];
    if pbkdf2::<Hmac<Sha256>>(password.as_bytes(), &salt, iterations, &mut computed).is_err() {
        return false;
    }

    constant_time_eq(&hex::encode(&computed), &hex::encode(&expected_hash))
}

fn issue_jwt(secret: &str, ttl_days: i64, user: &AuthUser) -> anyhow::Result<(String, i64)> {
    let now = Utc::now();
    let exp = now + Duration::days(ttl_days.max(1));
    let claims = Claims {
        sub: user.id.clone(),
        usr: user.username.clone(),
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };
    let token = jsonwebtoken::encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, claims.exp))
}

fn verify_jwt(token: &str, secret: &str) -> anyhow::Result<Claims> {
    let validation = Validation::default();
    let token_data = jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(token_data.claims)
}

/// Verify a JWT against the server config.
/// Returns true iff:
/// - auth is not required (dev mode), OR
/// - auth is required and the token is valid.
pub fn verify_token_for_config(token: &str, config: &Config) -> bool {
    if !config.auth.auth_required(config.dev_mode) {
        return true;
    }
    let secret = match config.auth.jwt_secret.as_deref() {
        Some(s) => s,
        None => return false,
    };
    let Ok(claims) = verify_jwt(token, secret) else {
        return false;
    };
    match config.auth.auth_mode(config.dev_mode) {
        AuthMode::MultiUser => user_for_claims(&claims, &config.auth.users).is_some(),
        AuthMode::SingleTenant => true,
        AuthMode::Disabled => true,
    }
}

pub async fn login(
    State(state): State<std::sync::Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    let auth_mode = state.config.auth.auth_mode(state.config.dev_mode);
    let user = match auth_mode {
        AuthMode::MultiUser => {
            let username = req.username.as_deref().unwrap_or("").trim();
            if username.is_empty() {
                return Err((StatusCode::UNAUTHORIZED, "Username required".to_string()));
            }
            // Find user and verify password. Use a single generic error message
            // for both invalid username and invalid password to prevent username enumeration.
            let account = state
                .config
                .auth
                .users
                .iter()
                .find(|u| u.username.trim() == username);

            let valid = match account {
                Some(acc) => {
                    !acc.password.trim().is_empty()
                        && constant_time_eq(req.password.trim(), acc.password.trim())
                }
                None => {
                    // Perform a dummy comparison to prevent timing attacks
                    let _ = constant_time_eq(req.password.trim(), "dummy_password_for_timing");
                    false
                }
            };

            if !valid {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Invalid username or password".to_string(),
                ));
            }

            // account is guaranteed Some here: the None branch above sets valid=false,
            // and we returned early on !valid.
            let account = account.expect("account must be Some when valid is true");
            let effective_id = effective_user_id(account);

            AuthUser {
                id: effective_id,
                username: account.username.clone(),
            }
        }
        AuthMode::SingleTenant | AuthMode::Disabled => {
            // If dev_mode is enabled, we still allow login, but it won't be required.
            // Check dashboard-managed password hash first, then fall back to env var.
            let stored_auth = state.settings.get_auth_settings().await;
            let valid = if let Some(ref auth_settings) = stored_auth {
                if let Some(ref hash) = auth_settings.password_hash {
                    verify_password_hash(req.password.trim(), hash)
                } else {
                    // No stored hash — fall back to env var
                    let expected = state
                        .config
                        .auth
                        .dashboard_password
                        .as_deref()
                        .unwrap_or("");
                    !expected.is_empty() && constant_time_eq(req.password.trim(), expected)
                }
            } else {
                // No auth settings at all — fall back to env var
                let expected = state
                    .config
                    .auth
                    .dashboard_password
                    .as_deref()
                    .unwrap_or("");
                !expected.is_empty() && constant_time_eq(req.password.trim(), expected)
            };

            if !valid {
                return Err((StatusCode::UNAUTHORIZED, "Invalid password".to_string()));
            }

            AuthUser {
                id: "default".to_string(),
                username: "default".to_string(),
            }
        }
    };

    let secret = state.config.auth.jwt_secret.as_deref().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "JWT_SECRET not configured".to_string(),
        )
    })?;

    let (token, exp) = issue_jwt(secret, state.config.auth.jwt_ttl_days, &user)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(LoginResponse { token, exp }))
}

pub async fn require_auth(
    State(state): State<std::sync::Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    // Dev mode => no auth checks.
    if state.config.dev_mode {
        req.extensions_mut().insert(AuthUser {
            id: "dev".to_string(),
            username: "dev".to_string(),
        });
        return next.run(req).await;
    }

    // If auth isn't configured, fail closed in non-dev mode.
    let secret = match state.config.auth.jwt_secret.as_deref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "JWT_SECRET not configured",
            )
                .into_response();
        }
    };

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or("");

    if token.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response();
    }

    match verify_jwt(token, secret) {
        Ok(claims) => {
            let user = match state.config.auth.auth_mode(state.config.dev_mode) {
                AuthMode::MultiUser => match user_for_claims(&claims, &state.config.auth.users) {
                    Some(u) => u,
                    None => {
                        return (StatusCode::UNAUTHORIZED, "Invalid user").into_response();
                    }
                },
                AuthMode::SingleTenant => AuthUser {
                    id: claims.sub,
                    username: claims.usr,
                },
                AuthMode::Disabled => AuthUser {
                    id: "default".to_string(),
                    username: "default".to_string(),
                },
            };
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        Err(_) => (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response(),
    }
}

/// Returns the effective user ID (id if non-empty, otherwise username).
fn effective_user_id(user: &UserAccount) -> String {
    if user.id.is_empty() {
        user.username.clone()
    } else {
        user.id.clone()
    }
}

fn user_for_claims(claims: &Claims, users: &[UserAccount]) -> Option<AuthUser> {
    users
        .iter()
        .find(|u| effective_user_id(u) == claims.sub)
        .map(|u| AuthUser {
            id: effective_user_id(u),
            username: u.username.clone(),
        })
}

// ─── Auth status & password change endpoints ─────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct AuthStatusResponse {
    pub auth_mode: String,
    pub password_source: String, // "dashboard", "environment", "none"
    pub password_changed_at: Option<String>,
    pub dev_mode: bool,
}

pub async fn auth_status(
    State(state): State<std::sync::Arc<AppState>>,
) -> Json<AuthStatusResponse> {
    let auth_mode = match state.config.auth.auth_mode(state.config.dev_mode) {
        AuthMode::Disabled => "disabled",
        AuthMode::SingleTenant => "single_tenant",
        AuthMode::MultiUser => "multi_user",
    };

    let stored_auth = state.settings.get_auth_settings().await;
    let has_stored_hash = stored_auth
        .as_ref()
        .and_then(|a| a.password_hash.as_ref())
        .is_some();
    let has_env_password = state.config.auth.dashboard_password.is_some();

    let password_source = if has_stored_hash {
        "dashboard"
    } else if has_env_password {
        "environment"
    } else {
        "none"
    };

    let password_changed_at = stored_auth.and_then(|a| a.password_changed_at);

    Json(AuthStatusResponse {
        auth_mode: auth_mode.to_string(),
        password_source: password_source.to_string(),
        password_changed_at,
        dev_mode: state.config.dev_mode,
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: Option<String>,
    pub new_password: String,
}

pub async fn change_password(
    State(state): State<std::sync::Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Multi-user mode: passwords are managed via SANDBOXED_USERS env var
    if state.config.auth.auth_mode(state.config.dev_mode) == AuthMode::MultiUser {
        return Err((
            StatusCode::BAD_REQUEST,
            "Password change is not available in multi-user mode. Manage passwords via the SANDBOXED_USERS environment variable.".to_string(),
        ));
    }

    // Determine whether a current password exists (stored hash or env var)
    let stored_auth = state.settings.get_auth_settings().await;
    let has_stored_hash = stored_auth
        .as_ref()
        .and_then(|a| a.password_hash.as_ref())
        .is_some();
    let has_env_password = state
        .config
        .auth
        .dashboard_password
        .as_ref()
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let has_existing_password = has_stored_hash || has_env_password;

    // If a password exists and auth is not disabled (dev mode), require the current password
    if has_existing_password && !state.config.dev_mode {
        let current = req.current_password.as_deref().unwrap_or("").trim();
        if current.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Current password is required".to_string(),
            ));
        }

        let current_valid = if has_stored_hash {
            let hash = stored_auth
                .as_ref()
                .expect("stored_auth must be Some when has_stored_hash is true")
                .password_hash
                .as_ref()
                .expect("password_hash must be Some when has_stored_hash is true");
            verify_password_hash(current, hash)
        } else {
            let expected = state
                .config
                .auth
                .dashboard_password
                .as_deref()
                .unwrap_or("");
            constant_time_eq(current, expected)
        };

        if !current_valid {
            return Err((
                StatusCode::UNAUTHORIZED,
                "Current password is incorrect".to_string(),
            ));
        }
    }

    // Validate new password
    let new_password = req.new_password.trim();
    if new_password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "New password must be at least 8 characters".to_string(),
        ));
    }

    // Hash and persist
    let hashed = hash_password(new_password);
    let now = Utc::now().to_rfc3339();

    let auth_settings = crate::settings::AuthSettings {
        password_hash: Some(hashed),
        password_changed_at: Some(now.clone()),
    };

    state
        .settings
        .set_auth_settings(auth_settings)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save password: {}", e),
            )
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "password_changed_at": now
    })))
}
