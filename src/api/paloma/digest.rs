use crate::api::mission_store::TelegramAlert;

pub fn alert_rank(alert: &TelegramAlert) -> i32 {
    match alert.importance.as_str() {
        "high" => 0,
        "normal" => 1,
        _ => 2,
    }
}

pub fn alert_digest_line(alert: &TelegramAlert) -> String {
    let mut lines = alert.body.lines();
    let lead = lines
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or(alert.title.as_str());
    let latest = lines.find_map(|line| line.trim().strip_prefix("Latest: "));
    match latest {
        Some(latest) if !latest.trim().is_empty() => {
            format!("- {lead} Latest: {}", latest.trim())
        }
        _ => format!("- {lead}"),
    }
}

pub fn alert_digest_text<F>(alerts: &[TelegramAlert], redact: F) -> String
where
    F: Fn(&str) -> String,
{
    if alerts.len() == 1 {
        return redact(alerts[0].body.trim());
    }

    let mut sorted = alerts.to_vec();
    sorted.sort_by(|a, b| {
        alert_rank(a)
            .cmp(&alert_rank(b))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    let high_count = sorted
        .iter()
        .filter(|alert| alert.importance == "high")
        .count();
    let mut text = if high_count > 0 {
        format!(
            "{} mission update{} {} attention:",
            high_count,
            if high_count == 1 { "" } else { "s" },
            if high_count == 1 { "needs" } else { "need" }
        )
    } else {
        format!(
            "{} mission update{}:",
            sorted.len(),
            if sorted.len() == 1 { "" } else { "s" }
        )
    };
    for alert in sorted.iter().take(8) {
        text.push('\n');
        text.push_str(&alert_digest_line(alert));
    }
    let remaining = sorted.len().saturating_sub(8);
    if remaining > 0 {
        text.push_str(&format!(
            "\n- {} more update{}",
            remaining,
            if remaining == 1 { "" } else { "s" }
        ));
    }
    redact(&text)
}
