//! Grok / xAI usage (Grok Build credits + API-key identity).
//!
//! SuperGrok / Grok Build (OAuth or `grok login`) reads the current credit
//! window from the Grok CLI billing backend:
//!   `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`
//! with `Authorization: Bearer <token>` and `x-xai-token-auth: xai-grok-cli`.
//! The plan name lives on `GET …/v1/settings` (`subscription_tier_display`).
//!
//! Inference keys (`xai-…`) authenticate the official Auth endpoint
//! `GET https://api.x.ai/v1/api-key`. Prepaid USD lives on the Management API
//! (`GET https://management-api.x.ai/v1/billing/teams/{team_id}/prepaid/balance`)
//! and needs a separate management key, so we only probe that when one is
//! configured.

use serde::Serialize;
use serde_json::{json, Value};

pub const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
pub const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
pub const API_KEY_INFO_URL: &str = "https://api.x.ai/v1/api-key";
pub const TOKEN_AUTH_HEADER: &str = "xai-grok-cli";

pub fn prepaid_balance_url(team_id: &str) -> String {
    format!("https://management-api.x.ai/v1/billing/teams/{team_id}/prepaid/balance")
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct XaiUsageSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_remaining_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_reset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_window_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand_cap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepaid_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

impl XaiUsageSnapshot {
    pub fn to_provider_fields(&self) -> Value {
        let mut map = serde_json::Map::new();
        if let Some(plan) = &self.plan {
            map.insert("xai_plan".into(), json!(plan));
        }
        if let Some(label) = &self.credit_label {
            map.insert("xai_credit_label".into(), json!(label));
        }
        if let Some(v) = self.credit_used_percent {
            map.insert("xai_credit_used_percent".into(), json!(v));
        }
        if let Some(v) = self.credit_remaining_percent {
            map.insert("xai_credit_remaining_percent".into(), json!(v));
        }
        if let Some(v) = self.credit_reset {
            map.insert("xai_credit_reset".into(), json!(v));
        }
        if let Some(v) = self.credit_window_seconds {
            map.insert("xai_credit_window_seconds".into(), json!(v));
        }
        if let Some(v) = self.on_demand_used {
            map.insert("xai_on_demand_used".into(), json!(v));
        }
        if let Some(v) = self.on_demand_cap {
            map.insert("xai_on_demand_cap".into(), json!(v));
        }
        if let Some(v) = self.prepaid_usd {
            map.insert("xai_prepaid_usd".into(), json!(v));
        }
        if let Some(name) = &self.key_name {
            map.insert("xai_key_name".into(), json!(name));
        }
        if let Some(team) = &self.team_id {
            map.insert("xai_team_id".into(), json!(team));
        }
        Value::Object(map)
    }

    pub fn has_displayable_quota(&self) -> bool {
        self.credit_used_percent.is_some() || self.prepaid_usd.is_some()
    }
}

pub fn parse_cli_billing(payload: &Value) -> XaiUsageSnapshot {
    let mut snap = XaiUsageSnapshot::default();
    let config = payload.get("config").unwrap_or(payload);

    snap.plan = string_field(
        config
            .get("subscriptionTier")
            .or_else(|| config.get("subscription_tier"))
            .or_else(|| payload.get("subscriptionTier"))
            .or_else(|| payload.get("subscription_tier")),
    )
    .map(display_plan);

    let period = config
        .get("currentPeriod")
        .or_else(|| config.get("current_period"));
    let period_start = parse_time(
        period
            .and_then(|p| p.get("start").or_else(|| p.get("begin")))
            .or_else(|| config.get("billingPeriodStart"))
            .or_else(|| config.get("billing_period_start")),
    );
    let billing_cycle = payload
        .get("billingCycle")
        .or_else(|| payload.get("billing_cycle"))
        .or_else(|| config.get("billingCycle"));
    let period_end = parse_time(
        period
            .and_then(|p| p.get("end"))
            .or_else(|| config.get("billingPeriodEnd"))
            .or_else(|| config.get("billing_period_end"))
            .or_else(|| billing_cycle.and_then(|c| c.get("billingPeriodEnd")))
            .or_else(|| billing_cycle.and_then(|c| c.get("billing_period_end"))),
    );
    snap.credit_reset = period_end;
    snap.credit_window_seconds = match (period_start, period_end) {
        (Some(start), Some(end)) if end > start => Some(end - start),
        _ => None,
    };

    if let Some(pct) = number(
        config
            .get("creditUsagePercent")
            .or_else(|| config.get("credit_usage_percent")),
    ) {
        apply_used_percent(&mut snap, pct);
    } else if let Some(pct) = on_demand_percent(config).or_else(|| rpc_used_percent(payload)) {
        apply_used_percent(&mut snap, pct);
    } else if period_end.is_some() {
        apply_used_percent(&mut snap, 0.0);
    }

    if let Some(used) = amount_val(
        config
            .get("onDemandUsed")
            .or_else(|| config.get("on_demand_used")),
    ) {
        snap.on_demand_used = Some(used);
    }
    if let Some(cap) = amount_val(
        config
            .get("onDemandCap")
            .or_else(|| config.get("on_demand_cap")),
    ) {
        snap.on_demand_cap = Some(cap);
    }

    snap.credit_label = Some(credit_label(
        snap.credit_window_seconds,
        snap.credit_reset,
        chrono::Utc::now().timestamp(),
    ));
    if snap.credit_window_seconds.is_none() {
        snap.credit_window_seconds = default_window_seconds(snap.credit_label.as_deref());
    }
    snap
}

pub fn parse_cli_settings(payload: &Value) -> Option<String> {
    string_field(
        payload
            .get("subscription_tier_display")
            .or_else(|| payload.get("subscriptionTierDisplay")),
    )
    .map(display_plan)
}

pub fn parse_api_key_info(payload: &Value) -> XaiUsageSnapshot {
    XaiUsageSnapshot {
        key_name: string_field(payload.get("name").or_else(|| payload.get("apiKeyName"))),
        team_id: string_field(
            payload
                .get("teamId")
                .or_else(|| payload.get("team_id"))
                .or_else(|| payload.get("teamID")),
        ),
        prepaid_usd: number(
            payload
                .get("remainingCredits")
                .or_else(|| payload.get("remaining_credits"))
                .or_else(|| payload.get("credits")),
        )
        .map(cents_to_usd),
        ..Default::default()
    }
}

/// Management prepaid ledger is inverted USD cents: a $10 top-up is `"-1000"`.
pub fn parse_prepaid_balance(payload: &Value) -> Option<f64> {
    let raw = payload
        .get("total")
        .and_then(|t| t.get("val").or(Some(t)))
        .or_else(|| payload.get("val"))?;
    let cents = number(Some(raw))?;
    Some((-cents) / 100.0)
}

pub fn merge_snapshots(mut base: XaiUsageSnapshot, extra: XaiUsageSnapshot) -> XaiUsageSnapshot {
    if base.plan.is_none() {
        base.plan = extra.plan;
    }
    if base.credit_used_percent.is_none() {
        base.credit_used_percent = extra.credit_used_percent;
        base.credit_remaining_percent = extra.credit_remaining_percent;
        base.credit_reset = extra.credit_reset.or(base.credit_reset);
        base.credit_window_seconds = extra.credit_window_seconds.or(base.credit_window_seconds);
        base.credit_label = extra.credit_label.or(base.credit_label);
    }
    if base.on_demand_used.is_none() {
        base.on_demand_used = extra.on_demand_used;
    }
    if base.on_demand_cap.is_none() {
        base.on_demand_cap = extra.on_demand_cap;
    }
    if base.prepaid_usd.is_none() {
        base.prepaid_usd = extra.prepaid_usd;
    }
    if base.key_name.is_none() {
        base.key_name = extra.key_name;
    }
    if base.team_id.is_none() {
        base.team_id = extra.team_id;
    }
    base
}

fn apply_used_percent(snap: &mut XaiUsageSnapshot, pct: f64) {
    let used = pct.clamp(0.0, 100.0);
    snap.credit_used_percent = Some(used);
    snap.credit_remaining_percent = Some((100.0 - used).clamp(0.0, 100.0));
}

fn on_demand_percent(config: &Value) -> Option<f64> {
    let cap = amount_val(
        config
            .get("onDemandCap")
            .or_else(|| config.get("on_demand_cap")),
    )?;
    if cap <= 0.0 {
        return None;
    }
    let used = amount_val(
        config
            .get("onDemandUsed")
            .or_else(|| config.get("on_demand_used")),
    )?;
    Some((used / cap * 100.0).clamp(0.0, 100.0))
}

fn rpc_used_percent(payload: &Value) -> Option<f64> {
    let usage = payload.get("usage")?;
    let limit = amount_val(
        payload
            .get("monthlyLimit")
            .or_else(|| payload.get("monthly_limit")),
    )?;
    if limit <= 0.0 {
        return None;
    }
    let used = amount_val(
        usage
            .get("totalUsed")
            .or_else(|| usage.get("total_used"))
            .or_else(|| usage.get("includedUsed")),
    )?;
    Some((used / limit * 100.0).clamp(0.0, 100.0))
}

fn credit_label(window_seconds: Option<i64>, reset_at: Option<i64>, now: i64) -> String {
    if let Some(secs) = window_seconds {
        return label_for_seconds(secs).to_string();
    }
    if let Some(reset) = reset_at {
        let remaining = reset - now;
        if remaining > 0 {
            return label_for_seconds(remaining).to_string();
        }
    }
    "Credits".to_string()
}

fn label_for_seconds(secs: i64) -> &'static str {
    if (20 * 24 * 3600..=40 * 24 * 3600).contains(&secs) {
        "Monthly"
    } else if (5 * 24 * 3600..=10 * 24 * 3600).contains(&secs) {
        "Weekly"
    } else {
        "Credits"
    }
}

fn default_window_seconds(label: Option<&str>) -> Option<i64> {
    match label {
        Some("Weekly") => Some(7 * 24 * 3600),
        Some("Monthly") => Some(30 * 24 * 3600),
        _ => None,
    }
}

fn display_plan(raw: String) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return raw;
    }
    match trimmed.to_ascii_lowercase().replace('_', " ").as_str() {
        "supergrok" | "super grok" => "SuperGrok".to_string(),
        "supergrok heavy" | "super grok heavy" => "SuperGrok Heavy".to_string(),
        other if other.eq_ignore_ascii_case(trimmed) && trimmed.contains(' ') => {
            trimmed.to_string()
        }
        _ => trimmed.to_string(),
    }
}

fn amount_val(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    number(value.get("val").or(Some(value)))
}

fn cents_to_usd(cents: f64) -> f64 {
    if cents.abs() > 1000.0 && cents.fract() == 0.0 {
        cents / 100.0
    } else {
        cents
    }
}

fn parse_time(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        return Some(normalize_epoch(n));
    }
    if let Some(n) = value.as_f64() {
        return Some(normalize_epoch(n as i64));
    }
    let s = value.as_str()?;
    if let Ok(n) = s.parse::<i64>() {
        return Some(normalize_epoch(n));
    }
    chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00"))
        .ok()
        .map(|dt| dt.timestamp())
}

fn normalize_epoch(n: i64) -> i64 {
    if n > 10_000_000_000 {
        n / 1000
    } else {
        n
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn string_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_credits_percent_and_weekly_period() {
        let payload = json!({
            "config": {
                "creditUsagePercent": 37.5,
                "currentPeriod": {
                    "start": "2026-08-10T00:00:00Z",
                    "end": "2026-08-17T00:00:00Z"
                },
                "subscriptionTier": "SUPERGROK"
            }
        });
        let snap = parse_cli_billing(&payload);
        assert_eq!(snap.credit_used_percent, Some(37.5));
        assert_eq!(snap.credit_remaining_percent, Some(62.5));
        assert_eq!(snap.credit_reset, Some(1_786_924_800)); // 2026-08-17T00:00:00Z
        assert_eq!(snap.credit_window_seconds, Some(7 * 24 * 3600));
        assert_eq!(snap.credit_label.as_deref(), Some("Weekly"));
        assert_eq!(snap.plan.as_deref(), Some("SuperGrok"));
        let fields = snap.to_provider_fields();
        assert_eq!(fields["xai_credit_used_percent"], 37.5);
        assert_eq!(fields["xai_credit_label"], "Weekly");
    }

    #[test]
    fn falls_back_to_on_demand_ratio() {
        let payload = json!({
            "config": {
                "onDemandUsed": { "val": 25 },
                "onDemandCap": { "val": 100 },
                "billingPeriodEnd": "2026-09-01T00:00:00Z"
            }
        });
        let snap = parse_cli_billing(&payload);
        assert_eq!(snap.credit_used_percent, Some(25.0));
        assert_eq!(snap.on_demand_used, Some(25.0));
        assert_eq!(snap.on_demand_cap, Some(100.0));
        assert_eq!(snap.credit_label.as_deref(), Some("Credits"));
    }

    #[test]
    fn zero_usage_when_period_exists_without_percent() {
        let payload = json!({
            "config": {
                "currentPeriod": { "end": "2026-08-20T00:00:00Z" }
            }
        });
        let snap = parse_cli_billing(&payload);
        assert_eq!(snap.credit_used_percent, Some(0.0));
        assert_eq!(snap.credit_reset, Some(1_787_184_000));
    }

    #[test]
    fn parses_rpc_style_usage_block() {
        let payload = json!({
            "monthlyLimit": { "val": 99900 },
            "usage": { "totalUsed": { "val": 24975 } },
            "billingCycle": { "billingPeriodEnd": "2026-09-01T00:00:00Z" }
        });
        let snap = parse_cli_billing(&payload);
        assert_eq!(snap.credit_used_percent, Some(25.0));
    }

    #[test]
    fn parses_settings_tier() {
        assert_eq!(
            parse_cli_settings(&json!({ "subscription_tier_display": "SuperGrok Heavy" }))
                .as_deref(),
            Some("SuperGrok Heavy")
        );
    }

    #[test]
    fn parses_api_key_info() {
        let snap = parse_api_key_info(&json!({
            "name": "prod",
            "teamId": "65c1e471-205f-4566-9c5a-07198bcdf4ce",
            "acls": ["api-key:model:*"]
        }));
        assert_eq!(snap.key_name.as_deref(), Some("prod"));
        assert_eq!(
            snap.team_id.as_deref(),
            Some("65c1e471-205f-4566-9c5a-07198bcdf4ce")
        );
    }

    #[test]
    fn prepaid_balance_inverts_ledger_cents() {
        let usd = parse_prepaid_balance(&json!({
            "total": { "val": "-2500" }
        }))
        .expect("balance");
        assert!((usd - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn merge_keeps_billing_and_adds_prepaid() {
        let billing = parse_cli_billing(&json!({
            "config": { "creditUsagePercent": 10.0, "currentPeriod": { "end": "2026-08-17T00:00:00Z" } }
        }));
        let prepaid = XaiUsageSnapshot {
            prepaid_usd: Some(12.5),
            key_name: Some("prod".into()),
            ..Default::default()
        };
        let merged = merge_snapshots(billing, prepaid);
        assert_eq!(merged.credit_used_percent, Some(10.0));
        assert_eq!(merged.prepaid_usd, Some(12.5));
        assert_eq!(merged.key_name.as_deref(), Some("prod"));
    }
}
