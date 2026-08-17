//! Kimi Code subscription usage (`GET /coding/v1/usages`).
//!
//! Official Kimi Code docs and the CLI `/usage` command read 5-hour + weekly
//! quota from `https://api.kimi.com/coding/v1/usages` with a Bearer token
//! (OAuth or `sk-kimi-…`) and the `KimiCLI/*` User-Agent. Pay-as-you-go
//! Open Platform keys instead expose cash/voucher balance on
//! `https://api.moonshot.ai/v1/users/me/balance`.

use serde::Serialize;
use serde_json::{json, Value};

pub const CODING_USAGES_URL: &str = "https://api.kimi.com/coding/v1/usages";
pub const OPEN_PLATFORM_BALANCE_URL: &str = "https://api.moonshot.ai/v1/users/me/balance";
pub const KIMI_USAGE_USER_AGENT: &str = "KimiCLI/1.5";

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct KimiUsageWindow {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct KimiUsageSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<KimiUsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<KimiUsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly: Option<KimiUsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_balance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cash_balance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voucher_balance: Option<f64>,
}

impl KimiUsageSnapshot {
    pub fn to_provider_fields(&self) -> Value {
        let mut map = serde_json::Map::new();
        if let Some(plan) = &self.plan {
            map.insert("kimi_plan".into(), json!(plan));
        }
        if !self.windows.is_empty() {
            map.insert("kimi_windows".into(), json!(self.windows));
        }
        if let Some(w) = &self.five_hour {
            if let Some(pct) = w.used_percent {
                map.insert("kimi_5h_used_percent".into(), json!(pct));
            }
            if let Some(pct) = w.remaining_percent {
                map.insert("kimi_5h_remaining_percent".into(), json!(pct));
            }
            if let Some(reset) = w.reset_at {
                map.insert("kimi_5h_reset".into(), json!(reset));
            }
        }
        if let Some(w) = &self.weekly {
            if let Some(pct) = w.used_percent {
                map.insert("kimi_weekly_used_percent".into(), json!(pct));
            }
            if let Some(pct) = w.remaining_percent {
                map.insert("kimi_weekly_remaining_percent".into(), json!(pct));
            }
            if let Some(reset) = w.reset_at {
                map.insert("kimi_weekly_reset".into(), json!(reset));
            }
        }
        if let Some(v) = self.available_balance {
            map.insert("kimi_available_balance".into(), json!(v));
        }
        if let Some(v) = self.cash_balance {
            map.insert("kimi_cash_balance".into(), json!(v));
        }
        if let Some(v) = self.voucher_balance {
            map.insert("kimi_voucher_balance".into(), json!(v));
        }
        Value::Object(map)
    }

    pub fn has_displayable_quota(&self) -> bool {
        self.five_hour.is_some()
            || self.weekly.is_some()
            || !self.windows.is_empty()
            || self.available_balance.is_some()
    }
}

pub fn parse_coding_usages(payload: &Value) -> KimiUsageSnapshot {
    let mut snap = KimiUsageSnapshot::default();
    let mut windows = Vec::new();

    if let Some(plan) = payload
        .get("plan")
        .or_else(|| payload.get("data").and_then(|d| d.get("plan")))
        .and_then(Value::as_str)
    {
        snap.plan = Some(plan.to_string());
    }

    if let Some(list) = payload.get("data").and_then(Value::as_array) {
        for item in list {
            if let Some(row) = window_from_map(item, &default_label_for_item(item, "Quota")) {
                windows.push((classify_item(item, &row.label), row));
            }
        }
    } else {
        if let Some(usage) = payload.get("usage") {
            if let Some(row) = window_from_map(usage, "Weekly") {
                windows.push(("weekly", row));
            }
        }
        if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
            for (idx, item) in limits.iter().enumerate() {
                let detail = item.get("detail").unwrap_or(item);
                let window = item.get("window");
                let label = window
                    .and_then(window_label)
                    .unwrap_or_else(|| format!("Limit {}", idx + 1));
                if let Some(row) = window_from_map(detail, &label) {
                    let kind = window
                        .map(classify_window_obj)
                        .unwrap_or_else(|| classify_label(&label));
                    windows.push((kind, row));
                }
            }
        }
    }

    for (kind, row) in windows {
        match kind {
            "5h" if snap.five_hour.is_none() => snap.five_hour = Some(row.clone()),
            "weekly" if snap.weekly.is_none() => snap.weekly = Some(row.clone()),
            _ => {}
        }
        snap.windows.push(row);
    }
    snap
}

pub fn parse_open_platform_balance(payload: &Value) -> KimiUsageSnapshot {
    let mut snap = KimiUsageSnapshot::default();
    let data = payload.get("data").unwrap_or(payload);
    snap.available_balance = number(data.get("available_balance"));
    snap.cash_balance = number(data.get("cash_balance"));
    snap.voucher_balance = number(data.get("voucher_balance"));
    snap
}

pub fn merge_snapshots(coding: KimiUsageSnapshot, balance: KimiUsageSnapshot) -> KimiUsageSnapshot {
    let mut out = coding;
    if out.available_balance.is_none() {
        out.available_balance = balance.available_balance;
    }
    if out.cash_balance.is_none() {
        out.cash_balance = balance.cash_balance;
    }
    if out.voucher_balance.is_none() {
        out.voucher_balance = balance.voucher_balance;
    }
    if out.plan.is_none() {
        out.plan = balance.plan;
    }
    out
}

fn window_from_map(data: &Value, default_label: &str) -> Option<KimiUsageWindow> {
    let limit = number(
        data.get("limit")
            .or_else(|| data.get("limit_amount"))
            .or_else(|| data.get("total")),
    );
    let remaining = number(data.get("remaining").or_else(|| data.get("left")));
    let mut used = number(
        data.get("used")
            .or_else(|| data.get("used_amount"))
            .or_else(|| data.get("consumed")),
    );
    if used.is_none() {
        if let (Some(limit), Some(remaining)) = (limit, remaining) {
            used = Some((limit - remaining).max(0.0));
        }
    }
    let used_percent = number(
        data.get("used_percent")
            .or_else(|| data.get("percentage"))
            .or_else(|| data.get("percent")),
    )
    .or_else(|| match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => Some((used / limit * 100.0).clamp(0.0, 100.0)),
        _ => None,
    });
    let remaining_percent = number(data.get("remaining_percent"))
        .or_else(|| used_percent.map(|pct| (100.0 - pct).clamp(0.0, 100.0)));
    if used.is_none() && limit.is_none() && remaining.is_none() && used_percent.is_none() {
        return None;
    }
    let label = data
        .get("name")
        .or_else(|| data.get("title"))
        .or_else(|| data.get("model_name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "all")
        .unwrap_or(default_label)
        .to_string();
    Some(KimiUsageWindow {
        label,
        used,
        limit,
        remaining,
        used_percent,
        remaining_percent,
        reset_at: parse_reset(data),
    })
}

fn parse_reset(data: &Value) -> Option<i64> {
    let raw = data
        .get("resetTime")
        .or_else(|| data.get("reset_at"))
        .or_else(|| data.get("reset_time"))
        .or_else(|| data.get("nextResetTime"))?;
    if let Some(n) = raw.as_i64() {
        return Some(normalize_epoch(n));
    }
    if let Some(n) = raw.as_f64() {
        return Some(normalize_epoch(n as i64));
    }
    if let Some(s) = raw.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return Some(normalize_epoch(n));
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")) {
            return Some(dt.timestamp());
        }
    }
    if let Some(secs) = number(data.get("reset_in")) {
        return Some(chrono::Utc::now().timestamp() + secs as i64);
    }
    None
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

fn default_label_for_item(item: &Value, fallback: &str) -> String {
    if item.get("model_name").and_then(Value::as_str) == Some("all") {
        "Weekly".to_string()
    } else {
        item.get("window")
            .and_then(window_label)
            .unwrap_or_else(|| fallback.to_string())
    }
}

fn classify_item(item: &Value, label: &str) -> &'static str {
    if item.get("model_name").and_then(Value::as_str) == Some("all") {
        return "weekly";
    }
    if let Some(window) = item.get("window") {
        return classify_window_obj(window);
    }
    classify_label(label)
}

fn classify_window_obj(window: &Value) -> &'static str {
    let duration = number(window.get("duration").or_else(|| window.get("number"))).unwrap_or(0.0);
    let unit = window
        .get("timeUnit")
        .or_else(|| window.get("time_unit"))
        .or_else(|| window.get("unit"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    let hours = if unit.contains("MINUTE") {
        duration / 60.0
    } else if unit.contains("HOUR") {
        duration
    } else if unit.contains("DAY") {
        duration * 24.0
    } else if unit.contains("WEEK") {
        duration * 24.0 * 7.0
    } else {
        duration
    };
    if (hours - 5.0).abs() < 0.1 {
        "5h"
    } else if hours >= 24.0 * 6.0 {
        "weekly"
    } else {
        "other"
    }
}

fn window_label(window: &Value) -> Option<String> {
    let duration = number(window.get("duration"))?;
    let unit = window
        .get("timeUnit")
        .or_else(|| window.get("time_unit"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match classify_window_obj(window) {
        "5h" => Some("5h".to_string()),
        "weekly" => Some("Weekly".to_string()),
        _ => Some(format!("{duration} {unit}")),
    }
}

fn classify_label(label: &str) -> &'static str {
    let lower = label.to_ascii_lowercase();
    if lower.contains("5h") || lower.contains("5-hour") || lower.contains("5 hour") {
        "5h"
    } else if lower.contains("week") || lower.contains("7d") || lower.contains("7-day") {
        "weekly"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_array_weekly_and_5h() {
        let payload = json!({
            "data": [
                {
                    "model_name": "all",
                    "used": 50,
                    "limit": 100,
                    "resetTime": 1_776_441_600
                },
                {
                    "model_name": "kimi-for-coding",
                    "used": 12,
                    "limit": 100,
                    "window": { "duration": 5, "timeUnit": "HOUR" },
                    "reset_at": "2026-08-17T14:00:00Z"
                }
            ]
        });
        let snap = parse_coding_usages(&payload);
        let weekly = snap.weekly.as_ref().expect("weekly");
        assert_eq!(weekly.used_percent, Some(50.0));
        assert_eq!(weekly.remaining_percent, Some(50.0));
        assert_eq!(weekly.reset_at, Some(1_776_441_600));
        let five = snap.five_hour.as_ref().expect("5h");
        assert_eq!(five.used_percent, Some(12.0));
        assert_eq!(five.reset_at, Some(1_786_975_200)); // 2026-08-17T14:00:00Z
        let fields = snap.to_provider_fields();
        assert_eq!(fields["kimi_weekly_used_percent"], 50.0);
        assert_eq!(fields["kimi_5h_used_percent"], 12.0);
    }

    #[test]
    fn parses_usage_plus_limits_shape() {
        let payload = json!({
            "usage": { "used": 20, "limit": 80, "name": "Weekly Usage" },
            "limits": [{
                "detail": { "used": 88, "limit": 100, "reset_in": 3600 },
                "window": { "duration": 300, "timeUnit": "MINUTE" }
            }]
        });
        let snap = parse_coding_usages(&payload);
        assert_eq!(snap.weekly.as_ref().unwrap().used_percent, Some(25.0));
        assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, Some(88.0));
    }

    #[test]
    fn parses_open_platform_balance() {
        let payload = json!({
            "code": 0,
            "data": {
                "available_balance": 49.58894,
                "voucher_balance": 46.58893,
                "cash_balance": 3.00001
            },
            "status": true
        });
        let snap = parse_open_platform_balance(&payload);
        assert_eq!(snap.available_balance, Some(49.58894));
        assert_eq!(snap.cash_balance, Some(3.00001));
        assert!(
            snap.to_provider_fields()["kimi_available_balance"]
                .as_f64()
                .unwrap()
                > 49.0
        );
    }

    #[test]
    fn remaining_is_derived_when_only_used_and_limit() {
        let payload = json!({
            "data": [{ "model_name": "all", "used": 30, "limit": 120 }]
        });
        let weekly = parse_coding_usages(&payload).weekly.unwrap();
        assert_eq!(weekly.remaining_percent, Some(75.0));
    }
}
