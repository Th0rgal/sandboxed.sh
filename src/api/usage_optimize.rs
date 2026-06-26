//! Provider-faithful "optimize" normalization for the usage API.
//!
//! The per-provider `/api/ai/providers/:id/usage` response is a flat bag of
//! vendor-specific fields (Anthropic unified `unified_5h_utilization`, Codex
//! `codex_primary_used_percent`, OpenAI-style `tokens_remaining`/`tokens_limit`,
//! Cerebras `*_day`/`*_minute`, …). A client that wants to "optimize for token
//! spending" otherwise has to special-case every vendor to answer three
//! questions: how much is left (so `pct_remaining` is computable), exactly when
//! does it reset, and how fast am I burning it.
//!
//! This module derives a single normalized `optimize` block from whatever the
//! provider already reported. The guiding rule is **provider truth**: a field
//! is only emitted when the underlying provider actually exposes it, every
//! window is tagged with its `source`, and nothing is fabricated — windows
//! differ per provider and carry different data on purpose.
//!
//! Two burn/pace signals are produced, each only where it is genuinely
//! supported:
//!   * **window pace** (`projected_pct_at_reset`) for percentage-window
//!     providers whose window length + reset time are known — pure provider
//!     truth, no second sample needed. Answers "am I on track to exhaust this
//!     window before it resets?".
//!   * **observed burn** (`tokens_per_min`) for absolute-token windows, derived
//!     from the delta between the previous cached snapshot and this one. Only
//!     possible when two snapshots exist and the window didn't reset between
//!     them.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

/// Anthropic unified subscription window lengths. These are not in the response
/// (only the reset time is), so we pin the documented window spans to derive a
/// pace. 5-hour and 7-day rolling windows.
const ANTHROPIC_5H_SECONDS: i64 = 5 * 3600;
const ANTHROPIC_7D_SECONDS: i64 = 7 * 24 * 3600;

/// MiniMax coding plan and Z.AI GLM coding plan both meter on a 5-hour rolling
/// window plus a weekly window. Like Anthropic, the response carries only the
/// reset time, so we pin the documented spans to derive a pace.
const FIVE_HOURS_SECONDS: i64 = 5 * 3600;
const WEEKLY_SECONDS: i64 = 7 * 24 * 3600;

/// A single normalized usage window plus the derived pace/burn for it.
struct Window {
    key: &'static str,
    label: String,
    /// "tokens" | "requests" | "mixed" — what the window meters.
    metric: &'static str,
    pct_used: Option<f64>,
    pct_remaining: Option<f64>,
    limit: Option<u64>,
    remaining: Option<u64>,
    used: Option<u64>,
    reset_at: Option<DateTime<Utc>>,
    window_seconds: Option<i64>,
    source: &'static str,
    /// Extra provider-truth fields that don't fit the normalized shape but are
    /// worth surfacing transparently (e.g. the raw Anthropic utilization and
    /// the assumption we made about its direction).
    extra: Map<String, Value>,
}

impl Window {
    fn new(
        key: &'static str,
        label: impl Into<String>,
        metric: &'static str,
        source: &'static str,
    ) -> Self {
        Window {
            key,
            label: label.into(),
            metric,
            pct_used: None,
            pct_remaining: None,
            limit: None,
            remaining: None,
            used: None,
            reset_at: None,
            window_seconds: None,
            source,
            extra: Map::new(),
        }
    }

    /// Fill pct_used/pct_remaining from an absolute limit+remaining pair.
    fn with_absolute(mut self, limit: u64, remaining: u64) -> Self {
        let remaining = remaining.min(limit);
        self.limit = Some(limit);
        self.remaining = Some(remaining);
        self.used = Some(limit - remaining);
        if limit > 0 {
            self.pct_remaining = Some(remaining as f64 / limit as f64 * 100.0);
            self.pct_used = Some((limit - remaining) as f64 / limit as f64 * 100.0);
        }
        self
    }

    /// Fill pct_used/pct_remaining from a 0..=100 used-percent value.
    fn with_used_percent(mut self, used_pct: f64) -> Self {
        let used_pct = used_pct.max(0.0);
        self.pct_used = Some(used_pct);
        self.pct_remaining = Some((100.0 - used_pct).max(0.0));
        self
    }

    /// Fill pct_used/pct_remaining from a 0..=100 remaining-percent value.
    /// MiniMax reports `*_remaining_percent` directly, so we take it as truth
    /// rather than recomputing it from the (often zero / unreliable) raw counts.
    fn with_remaining_percent(mut self, remaining_pct: f64) -> Self {
        let remaining_pct = remaining_pct.clamp(0.0, 100.0);
        self.pct_remaining = Some(remaining_pct);
        self.pct_used = Some(100.0 - remaining_pct);
        self
    }

    /// Window pace: project the used-percent forward across the full window
    /// using the elapsed fraction so far. Only valid when we know both the
    /// reset time and the window length, and at least one second has elapsed in
    /// the current window. Pure provider truth — no second sample needed.
    fn window_pace(&self, now: DateTime<Utc>) -> Option<(f64, bool)> {
        let pct_used = self.pct_used?;
        let reset_at = self.reset_at?;
        let window_seconds = self.window_seconds?;
        if window_seconds <= 0 {
            return None;
        }
        let window_start = reset_at - chrono::Duration::seconds(window_seconds);
        let elapsed = (now - window_start).num_seconds();
        // Outside the window (clock skew / stale reset) → can't project.
        if elapsed <= 0 || elapsed > window_seconds {
            return None;
        }
        let projected = pct_used * (window_seconds as f64 / elapsed as f64);
        Some((projected, projected >= 100.0))
    }

    fn into_json(self, now: DateTime<Utc>, observed: Option<ObservedBurn>) -> Value {
        let mut obj = Map::new();
        obj.insert("key".into(), json!(self.key));
        obj.insert("label".into(), json!(self.label));
        obj.insert("metric".into(), json!(self.metric));
        obj.insert("source".into(), json!(self.source));
        obj.insert("pct_used".into(), round2(self.pct_used));
        obj.insert("pct_remaining".into(), round2(self.pct_remaining));
        obj.insert("limit".into(), opt_u64(self.limit));
        obj.insert("remaining".into(), opt_u64(self.remaining));
        obj.insert("used".into(), opt_u64(self.used));
        obj.insert("window_seconds".into(), opt_i64(self.window_seconds));

        match self.reset_at {
            Some(reset_at) => {
                obj.insert("reset_at".into(), json!(reset_at.to_rfc3339()));
                obj.insert(
                    "reset_in_seconds".into(),
                    json!((reset_at - now).num_seconds()),
                );
            }
            None => {
                obj.insert("reset_at".into(), Value::Null);
                obj.insert("reset_in_seconds".into(), Value::Null);
            }
        }

        // Burn / pace block — only present when something was computable.
        let mut burn = Map::new();
        if let Some((projected, exhaust)) = self.window_pace(now) {
            burn.insert("projected_pct_at_reset".into(), round2(Some(projected)));
            burn.insert("on_track_to_exhaust".into(), json!(exhaust));
            burn.insert("basis".into(), json!("window_pace"));
        }
        if let Some(ob) = observed {
            burn.insert("tokens_per_min".into(), round2(Some(ob.per_min)));
            burn.insert("sample_seconds".into(), json!(ob.sample_seconds));
            match ob.projected_exhaustion_at {
                Some(at) => burn.insert("projected_exhaustion_at".into(), json!(at.to_rfc3339())),
                None => burn.insert("projected_exhaustion_at".into(), Value::Null),
            };
            // If both bases contributed, label it as such.
            let basis = if burn.contains_key("projected_pct_at_reset") {
                "window_pace+observed_delta"
            } else {
                "observed_delta"
            };
            burn.insert("basis".into(), json!(basis));
        }
        obj.insert(
            "burn".into(),
            if burn.is_empty() {
                Value::Null
            } else {
                Value::Object(burn)
            },
        );

        for (k, v) in self.extra {
            obj.insert(k, v);
        }
        Value::Object(obj)
    }
}

/// Observed burn derived from the delta between two snapshots of an
/// absolute-token window.
struct ObservedBurn {
    per_min: f64,
    sample_seconds: i64,
    projected_exhaustion_at: Option<DateTime<Utc>>,
}

/// Build the normalized `optimize` block from a usage value, optionally using
/// the previous cached snapshot (and its age) to derive observed burn rates.
pub fn build_optimize_block(value: &Value, prev: Option<(&Value, Duration)>) -> Value {
    build_optimize_block_at(value, prev, Utc::now())
}

/// Testable core: `now` is injected so unit tests are deterministic.
fn build_optimize_block_at(
    value: &Value,
    prev: Option<(&Value, Duration)>,
    now: DateTime<Utc>,
) -> Value {
    let windows = collect_windows(value);

    let mut window_values: Vec<Value> = Vec::with_capacity(windows.len());
    let mut primary: Option<(&'static str, f64)> = None;

    for w in windows {
        // Pick the most binding window = lowest pct_remaining.
        if let Some(pr) = w.pct_remaining {
            match primary {
                Some((_, best)) if best <= pr => {}
                _ => primary = Some((w.key, pr)),
            }
        }
        let observed = w
            .remaining
            .and_then(|rem| observed_burn(prev, w.key, rem, now));
        window_values.push(w.into_json(now, observed));
    }

    json!({
        "as_of": now.to_rfc3339(),
        "primary_window": primary.map(|(k, _)| k),
        "windows": window_values,
    })
}

/// Detect every window the provider reported. Order matters only for display.
fn collect_windows(v: &Value) -> Vec<Window> {
    let mut out = Vec::new();

    // ── Anthropic unified subscription windows (Claude Code OAuth) ──
    // `unified_5h_utilization` is a 0..1 fraction. Its direction (used vs
    // remaining) is undocumented; the Anthropic API convention is "fraction
    // used", so we assume that and ALSO surface the raw value + the assumption
    // so a consumer is never locked into our interpretation.
    for (util_key, reset_key, key, label, window_seconds) in [
        (
            "unified_5h_utilization",
            "unified_5h_reset",
            "anthropic_5h",
            "5-hour",
            ANTHROPIC_5H_SECONDS,
        ),
        (
            "unified_7d_utilization",
            "unified_7d_reset",
            "anthropic_7d",
            "7-day",
            ANTHROPIC_7D_SECONDS,
        ),
    ] {
        if let Some(util) = get_f64(v, util_key) {
            let used_pct = (util * 100.0).max(0.0);
            let mut w =
                Window::new(key, label, "mixed", "anthropic_unified").with_used_percent(used_pct);
            w.window_seconds = Some(window_seconds);
            w.reset_at = get_rfc3339(v, reset_key);
            w.extra.insert("raw_utilization".into(), json!(util));
            w.extra
                .insert("assumes".into(), json!("utilization=fraction_used"));
            out.push(w);
        }
    }

    // ── Codex (OpenAI ChatGPT subscription) windows — used-percent is explicit ──
    for (pct_key, win_min_key, reset_key, key, label) in [
        (
            "codex_primary_used_percent",
            "codex_primary_window_minutes",
            "codex_primary_reset_at",
            "codex_primary",
            "5-hour",
        ),
        (
            "codex_secondary_used_percent",
            "codex_secondary_window_minutes",
            "codex_secondary_reset_at",
            "codex_secondary",
            "weekly",
        ),
    ] {
        if let Some(used_pct) = get_f64(v, pct_key) {
            let mut w = Window::new(key, label, "mixed", "codex").with_used_percent(used_pct);
            w.window_seconds = get_i64(v, win_min_key).map(|m| m * 60);
            w.reset_at = get_epoch_secs(v, reset_key);
            out.push(w);
        }
    }

    // ── Absolute token/request windows (Anthropic legacy, OpenAI, xAI, Groq) ──
    if let (Some(limit), Some(remaining)) =
        (get_u64(v, "tokens_limit"), get_u64(v, "tokens_remaining"))
    {
        let mut w = Window::new("tokens", "tokens", "tokens", "provider_tokens")
            .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("tokens_reset"));
        out.push(w);
    }
    if let (Some(limit), Some(remaining)) = (
        get_u64(v, "requests_limit"),
        get_u64(v, "requests_remaining"),
    ) {
        let mut w = Window::new("requests", "requests", "requests", "provider_requests")
            .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("requests_reset"));
        out.push(w);
    }
    if let (Some(limit), Some(remaining)) = (
        get_u64(v, "input_tokens_limit"),
        get_u64(v, "input_tokens_remaining"),
    ) {
        let mut w = Window::new("input_tokens", "input tokens", "tokens", "provider_tokens")
            .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("input_tokens_reset"));
        out.push(w);
    }
    if let (Some(limit), Some(remaining)) = (
        get_u64(v, "output_tokens_limit"),
        get_u64(v, "output_tokens_remaining"),
    ) {
        let mut w = Window::new(
            "output_tokens",
            "output tokens",
            "tokens",
            "provider_tokens",
        )
        .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("output_tokens_reset"));
        out.push(w);
    }

    // ── Cerebras (per-day requests, per-minute tokens) ──
    if let (Some(limit), Some(remaining)) = (
        get_u64(v, "tokens_limit_minute"),
        get_u64(v, "tokens_remaining_minute"),
    ) {
        let mut w = Window::new("tokens_minute", "tokens / minute", "tokens", "cerebras")
            .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("tokens_reset_minute"));
        out.push(w);
    }
    if let (Some(limit), Some(remaining)) = (
        get_u64(v, "requests_limit_day"),
        get_u64(v, "requests_remaining_day"),
    ) {
        let mut w = Window::new("requests_day", "requests / day", "requests", "cerebras")
            .with_absolute(limit, remaining);
        w.reset_at = normalize_reset(v.get("requests_reset_day"));
        out.push(w);
    }

    // ── MiniMax coding plan (5-hour interval + weekly) — remaining-percent ──
    // MiniMax exposes `current_interval_remaining_percent` / weekly equivalent;
    // the raw token counts are frequently 0/0 for the text pool, so the percent
    // is the binding signal. Resets are normalized to epoch seconds by the
    // handler before they reach here.
    for (rem_key, reset_key, key, label, window_seconds) in [
        (
            "minimax_interval_remaining_percent",
            "minimax_interval_reset",
            "minimax_5h",
            "5-hour",
            FIVE_HOURS_SECONDS,
        ),
        (
            "minimax_weekly_remaining_percent",
            "minimax_weekly_reset",
            "minimax_weekly",
            "weekly",
            WEEKLY_SECONDS,
        ),
    ] {
        if let Some(rem_pct) = get_f64(v, rem_key) {
            let mut w =
                Window::new(key, label, "tokens", "minimax").with_remaining_percent(rem_pct);
            w.window_seconds = Some(window_seconds);
            w.reset_at = get_epoch_secs(v, reset_key);
            out.push(w);
        }
    }

    // ── Z.AI GLM coding plan (5-hour + weekly) — used-percent windows ──
    // Z.AI's monitor endpoint reports `percentage` as percent USED; it omits a
    // reset time for a window that hasn't been touched yet. Resets are
    // normalized to epoch seconds by the handler before they reach here.
    for (pct_key, reset_key, key, label, window_seconds) in [
        (
            "zai_5h_used_percent",
            "zai_5h_reset",
            "zai_5h",
            "5-hour",
            FIVE_HOURS_SECONDS,
        ),
        (
            "zai_weekly_used_percent",
            "zai_weekly_reset",
            "zai_weekly",
            "weekly",
            WEEKLY_SECONDS,
        ),
    ] {
        if let Some(used_pct) = get_f64(v, pct_key) {
            let mut w = Window::new(key, label, "tokens", "zai").with_used_percent(used_pct);
            w.window_seconds = Some(window_seconds);
            w.reset_at = get_epoch_secs(v, reset_key);
            out.push(w);
        }
    }

    out
}

/// Observed burn for an absolute window keyed by its `remaining` field name.
/// Compares the previous snapshot's remaining to the current one over the
/// snapshot age. Returns None when there's no prior sample, the window reset
/// between samples (remaining went up), or nothing was consumed.
fn observed_burn(
    prev: Option<(&Value, Duration)>,
    window_key: &str,
    cur_remaining: u64,
    now: DateTime<Utc>,
) -> Option<ObservedBurn> {
    let remaining_field = match window_key {
        "tokens" => "tokens_remaining",
        "requests" => "requests_remaining",
        "input_tokens" => "input_tokens_remaining",
        "output_tokens" => "output_tokens_remaining",
        "tokens_minute" => "tokens_remaining_minute",
        "requests_day" => "requests_remaining_day",
        _ => return None,
    };
    let (prev_value, age) = prev?;
    let prev_remaining = get_u64(prev_value, remaining_field)?;
    let sample_seconds = age.as_secs() as i64;
    if sample_seconds <= 0 || prev_remaining < cur_remaining {
        // No elapsed time, or remaining grew → the window replenished/reset;
        // a delta-based rate would be meaningless.
        return None;
    }
    let consumed = prev_remaining - cur_remaining;
    if consumed == 0 {
        return None;
    }
    let per_min = consumed as f64 / (sample_seconds as f64 / 60.0);
    let projected_exhaustion_at = if per_min > 0.0 {
        let secs_left = cur_remaining as f64 / per_min * 60.0;
        Some(now + chrono::Duration::seconds(secs_left as i64))
    } else {
        None
    };
    Some(ObservedBurn {
        per_min,
        sample_seconds,
        projected_exhaustion_at,
    })
}

// ── small JSON helpers ────────────────────────────────────────────────────

fn get_f64(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

fn get_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

fn get_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| {
        x.as_u64()
            .or_else(|| x.as_i64().filter(|n| *n >= 0).map(|n| n as u64))
            .or_else(|| x.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
    })
}

fn get_rfc3339(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = v.get(key)?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn get_epoch_secs(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let n = get_i64(v, key)?;
    DateTime::<Utc>::from_timestamp(n, 0)
}

/// Normalize a provider "reset" field that may be an RFC 3339 string, a unix
/// epoch (seconds), or a relative duration string like "2s" / "1m30s".
fn normalize_reset(v: Option<&Value>) -> Option<DateTime<Utc>> {
    normalize_reset_at(v, Utc::now())
}

fn normalize_reset_at(v: Option<&Value>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let v = v?;
    if let Some(n) = v.as_i64() {
        // Heuristic: a plausible epoch (after 2001) is absolute; a small number
        // is a relative seconds offset.
        if n > 1_000_000_000 {
            return DateTime::<Utc>::from_timestamp(n, 0);
        }
        if n >= 0 {
            return Some(now + chrono::Duration::seconds(n));
        }
    }
    let s = v.as_str()?;
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(n) = s.parse::<i64>() {
        if n > 1_000_000_000 {
            return DateTime::<Utc>::from_timestamp(n, 0);
        }
        return Some(now + chrono::Duration::seconds(n));
    }
    parse_duration_string(s).map(|secs| now + chrono::Duration::seconds(secs))
}

/// Parse "1m30s", "2s", "500ms", "1h" → whole seconds. ms rounds up to ≥1s when
/// non-zero so a sub-second reset still reads as imminent rather than "now".
fn parse_duration_string(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total_ms: i64 = 0;
    let mut num = String::new();
    let mut unit = String::new();
    let mut saw_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if saw_unit {
                total_ms += flush_unit(&num, &unit)?;
                num.clear();
                unit.clear();
                saw_unit = false;
            }
            num.push(ch);
        } else if ch.is_ascii_alphabetic() {
            saw_unit = true;
            unit.push(ch);
        } else {
            return None;
        }
    }
    if num.is_empty() && unit.is_empty() {
        return None;
    }
    total_ms += flush_unit(&num, &unit)?;
    Some(if total_ms > 0 && total_ms < 1000 {
        1
    } else {
        total_ms / 1000
    })
}

fn flush_unit(num: &str, unit: &str) -> Option<i64> {
    if num.is_empty() {
        return None;
    }
    let val: f64 = num.parse().ok()?;
    let ms = match unit {
        "ms" => val,
        "s" | "" => val * 1000.0,
        "m" => val * 60_000.0,
        "h" => val * 3_600_000.0,
        "d" => val * 86_400_000.0,
        _ => return None,
    };
    Some(ms as i64)
}

fn round2(v: Option<f64>) -> Value {
    match v {
        Some(f) if f.is_finite() => json!((f * 100.0).round() / 100.0),
        _ => Value::Null,
    }
}

fn opt_u64(v: Option<u64>) -> Value {
    v.map(|n| json!(n)).unwrap_or(Value::Null)
}

fn opt_i64(v: Option<i64>) -> Value {
    v.map(|n| json!(n)).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn window<'a>(opt: &'a Value, key: &str) -> &'a Value {
        opt["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["key"] == key)
            .unwrap_or_else(|| panic!("window {key} missing in {opt}"))
    }

    #[test]
    fn anthropic_unified_used_and_remaining() {
        // 1h into a 5h window, 40% used → pace projects 200% (will exhaust).
        let reset = (now() + chrono::Duration::hours(4)).to_rfc3339();
        let v = json!({ "unified_5h_utilization": 0.4, "unified_5h_reset": reset });
        let opt = build_optimize_block_at(&v, None, now());
        let w = window(&opt, "anthropic_5h");
        assert_eq!(w["pct_used"], json!(40.0));
        assert_eq!(w["pct_remaining"], json!(60.0));
        assert_eq!(w["raw_utilization"], json!(0.4));
        assert_eq!(w["source"], json!("anthropic_unified"));
        assert_eq!(w["burn"]["on_track_to_exhaust"], json!(true));
        assert_eq!(w["burn"]["projected_pct_at_reset"], json!(200.0));
        assert_eq!(opt["primary_window"], json!("anthropic_5h"));
    }

    #[test]
    fn codex_used_percent_window_and_reset() {
        let reset_epoch = (now() + chrono::Duration::hours(2)).timestamp();
        let v = json!({
            "codex_primary_used_percent": 25.0,
            "codex_primary_window_minutes": 300,
            "codex_primary_reset_at": reset_epoch,
        });
        let opt = build_optimize_block_at(&v, None, now());
        let w = window(&opt, "codex_primary");
        assert_eq!(w["pct_used"], json!(25.0));
        assert_eq!(w["pct_remaining"], json!(75.0));
        assert_eq!(w["window_seconds"], json!(18000));
        assert_eq!(w["reset_in_seconds"], json!(7200));
        assert_eq!(w["source"], json!("codex"));
    }

    #[test]
    fn absolute_tokens_pct_and_observed_burn() {
        let cur = json!({ "tokens_limit": 1000u64, "tokens_remaining": 400u64 });
        let prev = json!({ "tokens_limit": 1000u64, "tokens_remaining": 700u64 });
        // 300 tokens consumed over 60s → 300/min; 400 left → exhaust in 80s.
        let opt = build_optimize_block_at(&cur, Some((&prev, Duration::from_secs(60))), now());
        let w = window(&opt, "tokens");
        assert_eq!(w["limit"], json!(1000));
        assert_eq!(w["remaining"], json!(400));
        assert_eq!(w["used"], json!(600));
        assert_eq!(w["pct_remaining"], json!(40.0));
        assert_eq!(w["burn"]["tokens_per_min"], json!(300.0));
        assert_eq!(w["burn"]["basis"], json!("observed_delta"));
        let eta = w["burn"]["projected_exhaustion_at"].as_str().unwrap();
        assert_eq!(eta, (now() + chrono::Duration::seconds(80)).to_rfc3339());
    }

    #[test]
    fn observed_burn_skipped_on_reset() {
        // remaining grew (window replenished) → no burn.
        let cur = json!({ "tokens_limit": 1000u64, "tokens_remaining": 900u64 });
        let prev = json!({ "tokens_limit": 1000u64, "tokens_remaining": 100u64 });
        let opt = build_optimize_block_at(&cur, Some((&prev, Duration::from_secs(60))), now());
        let w = window(&opt, "tokens");
        assert_eq!(w["burn"], Value::Null);
    }

    #[test]
    fn primary_window_is_most_binding() {
        let v = json!({
            "tokens_limit": 1000u64, "tokens_remaining": 800u64,      // 80% remaining
            "requests_limit": 100u64, "requests_remaining": 5u64,     // 5% remaining ← binding
        });
        let opt = build_optimize_block_at(&v, None, now());
        assert_eq!(opt["primary_window"], json!("requests"));
    }

    #[test]
    fn reset_duration_string_and_epoch() {
        assert_eq!(parse_duration_string("1m30s"), Some(90));
        assert_eq!(parse_duration_string("2s"), Some(2));
        assert_eq!(parse_duration_string("500ms"), Some(1));
        assert_eq!(parse_duration_string("1h"), Some(3600));
        let n = now();
        let from_dur = normalize_reset_at(Some(&json!("90s")), n).unwrap();
        assert_eq!((from_dur - n).num_seconds(), 90);
        let from_epoch = normalize_reset_at(Some(&json!(n.timestamp() + 100)), n).unwrap();
        assert_eq!((from_epoch - n).num_seconds(), 100);
    }

    #[test]
    fn minimax_remaining_percent_windows() {
        // 93% remaining on the 5h window, reset 3h out (epoch seconds).
        let reset = (now() + chrono::Duration::hours(3)).timestamp();
        let v = json!({
            "minimax_interval_remaining_percent": 93,
            "minimax_interval_reset": reset,
            "minimax_weekly_remaining_percent": 100,
            "minimax_weekly_reset": (now() + chrono::Duration::days(4)).timestamp(),
        });
        let opt = build_optimize_block_at(&v, None, now());
        let w = window(&opt, "minimax_5h");
        assert_eq!(w["pct_remaining"], json!(93.0));
        assert_eq!(w["pct_used"], json!(7.0));
        assert_eq!(w["window_seconds"], json!(18000));
        assert_eq!(w["source"], json!("minimax"));
        assert_eq!(w["reset_in_seconds"], json!(10800));
        // Most binding window = the 5h (93% < 100%).
        assert_eq!(opt["primary_window"], json!("minimax_5h"));
    }

    #[test]
    fn zai_used_percent_windows() {
        // Weekly window 12% used, reset 2 days out; 5h window untouched (no reset).
        let weekly_reset = (now() + chrono::Duration::days(2)).timestamp();
        let v = json!({
            "zai_5h_used_percent": 0,
            "zai_weekly_used_percent": 12,
            "zai_weekly_reset": weekly_reset,
        });
        let opt = build_optimize_block_at(&v, None, now());
        let w5 = window(&opt, "zai_5h");
        assert_eq!(w5["pct_remaining"], json!(100.0));
        assert_eq!(w5["reset_at"], Value::Null);
        let ww = window(&opt, "zai_weekly");
        assert_eq!(ww["pct_used"], json!(12.0));
        assert_eq!(ww["pct_remaining"], json!(88.0));
        assert_eq!(ww["source"], json!("zai"));
        assert_eq!(ww["window_seconds"], json!(604800));
        // 5h is least-remaining is 100, weekly is 88 → weekly binds.
        assert_eq!(opt["primary_window"], json!("zai_weekly"));
    }

    #[test]
    fn empty_when_no_usage_fields() {
        let v = json!({ "provider_type": "anthropic", "status": "connected" });
        let opt = build_optimize_block_at(&v, None, now());
        assert_eq!(opt["windows"].as_array().unwrap().len(), 0);
        assert_eq!(opt["primary_window"], Value::Null);
    }
}
