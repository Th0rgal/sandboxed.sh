//! MiniMax Token Plan quota; missing counters are unknown, never exhausted.
use serde_json::{json, Value};
pub fn apply(info: &mut Value, data: &Value) {
    match data
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
    {
        Some(2062) => {
            info["usage_note"] = json!("No active Token Plan subscription for this API key.");
            return;
        }
        Some(code) if code != 0 => {
            info["usage_note"] = json!("Token Plan usage unavailable for this API key.");
            return;
        }
        _ => {}
    }
    let rows = data.get("model_remains").and_then(Value::as_array);
    let representative = rows.and_then(|rows| {
        rows.iter()
            .find(|r| r["model_name"] == "general")
            .or_else(|| rows.first())
    });
    if let Some(row) = representative {
        for (source, target, reset_source, reset_target) in [
            (
                "current_interval_remaining_percent",
                "minimax_interval_remaining_percent",
                "end_time",
                "minimax_interval_reset",
            ),
            (
                "current_weekly_remaining_percent",
                "minimax_weekly_remaining_percent",
                "weekly_end_time",
                "minimax_weekly_reset",
            ),
        ] {
            if let Some(percent) = row
                .get(source)
                .and_then(Value::as_f64)
                .filter(|p| (0.0..=100.0).contains(p))
            {
                info[target] = json!(percent);
                if let Some(reset) = row
                    .get(reset_source)
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
                {
                    info[reset_target] = json!(reset / 1000);
                }
            }
        }
    }
    if info.get("minimax_interval_remaining_percent").is_none()
        && info.get("minimax_weekly_remaining_percent").is_none()
    {
        info["usage_note"] = json!("No Token Plan quota reported for this API key.");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_reported_windows_without_inventing_missing_values() {
        let mut info = json!({});
        apply(
            &mut info,
            &json!({"model_remains":[{"model_name":"general","current_interval_remaining_percent":0,"end_time":1900000000000i64}]}),
        );
        assert_eq!(info["minimax_interval_remaining_percent"], 0.0);
        assert_eq!(info["minimax_interval_reset"], 1900000000);
        assert!(info.get("minimax_weekly_remaining_percent").is_none());
    }
    #[test]
    fn reports_absent_subscription_without_claiming_exhaustion() {
        let mut info = json!({});
        apply(
            &mut info,
            &json!({"model_remains":null,"base_resp":{"status_code":2062}}),
        );
        assert!(info["usage_note"].as_str().unwrap().contains("No active"));
        assert!(info.get("error").is_none());
        assert!(info.get("minimax_interval_remaining_percent").is_none());
    }
}
