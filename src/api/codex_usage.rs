//! Codex (OpenAI ChatGPT subscription) usage & limits.
//!
//! ChatGPT-plan Codex accounts expose their rate-limit state on the Codex
//! backend (`https://chatgpt.com/backend-api/codex/responses`) via `x-codex-*`
//! response headers — the same data the Codex CLI's `/status` shows
//! ("Rate Limits Remaining: 5h X%, Weekly Y%"). This mirrors Anthropic's
//! `anthropic-ratelimit-unified-*` headers but for OpenAI subscriptions.
//!
//! The headers are NOT visible on the normal inference path: missions route
//! Codex through the local CLIProxyAPI (`127.0.0.1:8317`), which strips them
//! and only surfaces a derived `model_cooldown` error on 429. So we can't fully
//! capture this passively. The design:
//!
//! - **Active probe (A)** — talk directly to the Codex backend with the
//!   account's (auto-refreshed) OAuth token and read the `x-codex-*` headers.
//!   This is the only source of the rich used-percent / window data. It costs
//!   one Codex "message" when budget remains (free — a 429 — when exhausted),
//!   so it is *throttled* ([`ACTIVE_TTL`]) and never run by the background
//!   usage-refresh loop; only on explicit dashboard demand.
//! - **Passive capture (B)** — when a real mission Codex call comes back as a
//!   `model_cooldown` 429 through our proxy, we stamp the "exhausted + reset"
//!   signal into the same store for free. It can't carry used-percent (the
//!   cli-proxy hid it), but it keeps the exhaustion state live between probes
//!   and lets the probe path short-circuit when we already know it's capped.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;
use tokio::sync::RwLock;

/// Minimum wall-clock gap between two *active* probes for one account. Codex
/// limits are message-based, so probing is not free when there is budget — keep
/// it well below the dashboard poll rate. A snapshot younger than this is served
/// as-is instead of re-probing.
pub const ACTIVE_TTL: Duration = Duration::from_secs(900); // 15 min

/// The Codex backend endpoint that returns `x-codex-*` rate-limit headers.
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// A point-in-time view of one Codex account's subscription limits.
///
/// Field names mirror the `x-codex-*` headers. Serialized under `codex_*` keys
/// so it slots into the dashboard's open-ended `ProviderUsage` shape next to
/// the Anthropic `unified_*` fields.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CodexUsageSnapshot {
    #[serde(rename = "codex_plan_type", skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(rename = "codex_active_limit", skip_serializing_if = "Option::is_none")]
    pub active_limit: Option<String>,

    /// Primary window = the 5-hour bucket.
    #[serde(
        rename = "codex_primary_used_percent",
        skip_serializing_if = "Option::is_none"
    )]
    pub primary_used_percent: Option<f64>,
    #[serde(
        rename = "codex_primary_window_minutes",
        skip_serializing_if = "Option::is_none"
    )]
    pub primary_window_minutes: Option<u64>,
    #[serde(
        rename = "codex_primary_reset_at",
        skip_serializing_if = "Option::is_none"
    )]
    pub primary_reset_at: Option<i64>,

    /// Secondary window = the weekly (7-day) bucket.
    #[serde(
        rename = "codex_secondary_used_percent",
        skip_serializing_if = "Option::is_none"
    )]
    pub secondary_used_percent: Option<f64>,
    #[serde(
        rename = "codex_secondary_window_minutes",
        skip_serializing_if = "Option::is_none"
    )]
    pub secondary_window_minutes: Option<u64>,
    #[serde(
        rename = "codex_secondary_reset_at",
        skip_serializing_if = "Option::is_none"
    )]
    pub secondary_reset_at: Option<i64>,

    #[serde(
        rename = "codex_credits_balance",
        skip_serializing_if = "Option::is_none"
    )]
    pub credits_balance: Option<f64>,
    #[serde(
        rename = "codex_credits_unlimited",
        skip_serializing_if = "Option::is_none"
    )]
    pub credits_unlimited: Option<bool>,

    /// `"probe"` (full, from an active probe) or `"passive"` (partial, from a
    /// real-traffic 429). Lets the UI label staleness/source.
    #[serde(rename = "codex_source", skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl CodexUsageSnapshot {
    /// True when at least one window's used-percent is known — i.e. this is a
    /// full probe snapshot rather than a bare passive exhaustion stamp.
    pub fn has_windows(&self) -> bool {
        self.primary_used_percent.is_some() || self.secondary_used_percent.is_some()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

struct Stored {
    snapshot: CodexUsageSnapshot,
    captured_at: Instant,
}

/// Per-account (`chatgpt_account_id`) Codex usage snapshots. Written by both the
/// active probe and the passive proxy hook; read by the providers usage API.
#[derive(Default)]
pub struct CodexUsageStore {
    entries: RwLock<HashMap<String, Stored>>,
}

impl CodexUsageStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Latest snapshot for an account if it is younger than [`ACTIVE_TTL`].
    pub async fn get_fresh(&self, account_id: &str) -> Option<CodexUsageSnapshot> {
        let entries = self.entries.read().await;
        entries.get(account_id).and_then(|s| {
            if s.captured_at.elapsed() < ACTIVE_TTL {
                Some(s.snapshot.clone())
            } else {
                None
            }
        })
    }

    /// Latest snapshot regardless of age (used as a last-resort fallback when a
    /// live probe fails).
    pub async fn get_any(&self, account_id: &str) -> Option<CodexUsageSnapshot> {
        let entries = self.entries.read().await;
        entries.get(account_id).map(|s| s.snapshot.clone())
    }

    /// Store a full snapshot from an active probe.
    pub async fn put_probe(&self, account_id: String, mut snapshot: CodexUsageSnapshot) {
        snapshot.source = Some("probe".to_string());
        let mut entries = self.entries.write().await;
        entries.insert(
            account_id,
            Stored {
                snapshot,
                captured_at: Instant::now(),
            },
        );
    }

    /// Fold a passive exhaustion signal (from a real-traffic `model_cooldown`
    /// 429) into the account's snapshot. Updates the secondary-window reset and
    /// marks it 100% used, but preserves any richer fields a prior probe set.
    pub async fn put_passive_cooldown(&self, account_id: String, secondary_reset_at: i64) {
        let mut entries = self.entries.write().await;
        let entry = entries.entry(account_id).or_insert_with(|| Stored {
            snapshot: CodexUsageSnapshot::default(),
            captured_at: Instant::now(),
        });
        entry.snapshot.secondary_used_percent = Some(100.0);
        entry.snapshot.secondary_reset_at = Some(secondary_reset_at);
        if entry.snapshot.source.is_none() {
            entry.snapshot.source = Some("passive".to_string());
        }
        entry.captured_at = Instant::now();
    }
}

/// Decode `chatgpt_account_id` from a ChatGPT OAuth access token (JWT). Mirrors
/// the helper in `ai_providers` but kept local to avoid widening its surface.
pub fn account_id_from_token(jwt: &str) -> Option<String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Build a [`CodexUsageSnapshot`] from a Codex backend response's `x-codex-*`
/// headers. Returns `None` when none of the rate-limit headers are present
/// (e.g. a 400 that never reached the limit layer).
pub fn parse_codex_headers(headers: &reqwest::header::HeaderMap) -> Option<CodexUsageSnapshot> {
    let mut snap = CodexUsageSnapshot {
        plan_type: header_str(headers, "x-codex-plan-type").map(str::to_string),
        active_limit: header_str(headers, "x-codex-active-limit").map(str::to_string),
        primary_used_percent: header_str(headers, "x-codex-primary-used-percent")
            .and_then(|v| v.parse().ok()),
        primary_window_minutes: header_str(headers, "x-codex-primary-window-minutes")
            .and_then(|v| v.parse().ok()),
        primary_reset_at: header_str(headers, "x-codex-primary-reset-at")
            .and_then(|v| v.parse().ok()),
        secondary_used_percent: header_str(headers, "x-codex-secondary-used-percent")
            .and_then(|v| v.parse().ok()),
        secondary_window_minutes: header_str(headers, "x-codex-secondary-window-minutes")
            .and_then(|v| v.parse().ok()),
        secondary_reset_at: header_str(headers, "x-codex-secondary-reset-at")
            .and_then(|v| v.parse().ok()),
        credits_balance: header_str(headers, "x-codex-credits-balance")
            .and_then(|v| v.parse().ok()),
        credits_unlimited: header_str(headers, "x-codex-credits-unlimited")
            .map(|v| v.eq_ignore_ascii_case("true")),
        source: None,
    };
    // Some deployments only carry the bare reset/used pair; treat the snapshot
    // as valid as long as at least one window field came through.
    if snap.plan_type.is_none()
        && snap.primary_used_percent.is_none()
        && snap.secondary_used_percent.is_none()
        && snap.primary_reset_at.is_none()
        && snap.secondary_reset_at.is_none()
    {
        return None;
    }
    snap.source = Some("probe".to_string());
    Some(snap)
}

/// Parse a CLIProxyAPI `model_cooldown` 429 body into the absolute epoch-seconds
/// at which the (weekly/secondary) limit resets. Shape:
/// `{"error":{"code":"model_cooldown","reset_seconds":229689,...}}`.
pub fn parse_cooldown_reset(body: &[u8], now_unix: i64) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    if err.get("code").and_then(|c| c.as_str()) != Some("model_cooldown") {
        return None;
    }
    let reset_secs = err.get("reset_seconds").and_then(|r| r.as_i64())?;
    Some(now_unix + reset_secs)
}

/// Active probe: ask the Codex backend for this account's limits and parse the
/// `x-codex-*` headers. `access_token` must be a fresh ChatGPT OAuth token.
///
/// Sends a minimal `gpt-5.6` request — the current model class accepted on a
/// ChatGPT account (codex-specific ids 400). When budget remains this consumes
/// one message; when exhausted the backend 429s (free) but still returns the
/// headers, which is exactly what we want.
pub async fn probe(
    client: &reqwest::Client,
    access_token: &str,
    account_id: &str,
) -> Result<CodexUsageSnapshot, String> {
    let body = serde_json::json!({
        "model": "gpt-5.6",
        "instructions": "x",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hi"}],
        }],
        "stream": true,
        "store": false,
    });
    let resp = client
        .post(CODEX_RESPONSES_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("chatgpt-account-id", account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(20))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Codex usage probe request failed: {e}"))?;

    let status = resp.status();
    if let Some(snap) = parse_codex_headers(resp.headers()) {
        tracing::debug!(
            account_id = %account_id,
            status = %status.as_u16(),
            "Codex usage probe captured rate-limit headers"
        );
        return Ok(snap);
    }
    Err(format!(
        "Codex usage probe returned HTTP {} with no x-codex-* headers",
        status.as_u16()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn parses_full_codex_headers() {
        let h = hdrs(&[
            ("x-codex-plan-type", "pro"),
            ("x-codex-active-limit", "premium"),
            ("x-codex-primary-used-percent", "23"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1780918001"),
            ("x-codex-secondary-used-percent", "100"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-secondary-reset-at", "1781139607"),
            ("x-codex-credits-balance", "0"),
            ("x-codex-credits-unlimited", "False"),
        ]);
        let s = parse_codex_headers(&h).expect("snapshot");
        assert_eq!(s.plan_type.as_deref(), Some("pro"));
        assert_eq!(s.primary_used_percent, Some(23.0));
        assert_eq!(s.primary_window_minutes, Some(300));
        assert_eq!(s.secondary_used_percent, Some(100.0));
        assert_eq!(s.secondary_window_minutes, Some(10080));
        assert_eq!(s.secondary_reset_at, Some(1781139607));
        assert_eq!(s.credits_unlimited, Some(false));
        assert!(s.has_windows());
        // Serializes under codex_* keys for the dashboard.
        let j = s.to_json();
        assert_eq!(j["codex_primary_used_percent"], 23.0);
        assert_eq!(j["codex_plan_type"], "pro");
    }

    #[test]
    fn returns_none_without_rate_limit_headers() {
        let h = hdrs(&[("content-type", "application/json")]);
        assert!(parse_codex_headers(&h).is_none());
    }

    #[test]
    fn parses_cooldown_reset() {
        let body = br#"{"error":{"code":"model_cooldown","reset_seconds":1000}}"#;
        assert_eq!(parse_cooldown_reset(body, 5_000), Some(6_000));
        // Non-cooldown errors are ignored.
        let other = br#"{"error":{"code":"usage_limit_reached"}}"#;
        assert_eq!(parse_cooldown_reset(other, 5_000), None);
    }
}
