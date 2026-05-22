use crate::api::mission_store::TelegramStructuredMemoryEntry;

pub fn consolidate_latest_explicit(
    mut entries: Vec<TelegramStructuredMemoryEntry>,
) -> Vec<TelegramStructuredMemoryEntry> {
    entries.sort_by(|a, b| {
        a.channel_id
            .cmp(&b.channel_id)
            .then_with(|| format!("{:?}", a.scope).cmp(&format!("{:?}", b.scope)))
            .then_with(|| a.chat_id.cmp(&b.chat_id))
            .then_with(|| a.subject_user_id.cmp(&b.subject_user_id))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    entries.dedup_by(|a, b| {
        a.channel_id == b.channel_id
            && a.scope == b.scope
            && a.chat_id == b.chat_id
            && a.subject_user_id == b.subject_user_id
            && a.label == b.label
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::{TelegramStructuredMemoryKind, TelegramStructuredMemoryScope};
    use uuid::Uuid;

    fn entry(label: &str, value: &str, updated_at: &str) -> TelegramStructuredMemoryEntry {
        TelegramStructuredMemoryEntry {
            id: Uuid::new_v4(),
            channel_id: Uuid::new_v4(),
            chat_id: 123,
            mission_id: None,
            subject_user_id: Some(42),
            subject_username: Some("tester".to_string()),
            subject_display_name: Some("Tester".to_string()),
            scope: TelegramStructuredMemoryScope::User,
            kind: TelegramStructuredMemoryKind::Preference,
            label: Some(label.to_string()),
            value: value.to_string(),
            source_message_id: Some(7),
            source_role: "user".to_string(),
            created_at: "2026-05-20T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn consolidation_keeps_latest_explicit_rule_per_subject_and_label() {
        let channel_id = Uuid::new_v4();
        let mut older = entry("alerts", "tell me everything", "2026-05-20T00:00:00Z");
        older.channel_id = channel_id;
        let mut newer = entry("alerts", "only failures", "2026-05-20T01:00:00Z");
        newer.channel_id = channel_id;
        let mut other_label = entry("tone", "short", "2026-05-20T00:30:00Z");
        other_label.channel_id = channel_id;

        let consolidated = consolidate_latest_explicit(vec![older, other_label, newer]);

        assert_eq!(consolidated.len(), 2);
        assert!(consolidated.iter().any(
            |entry| entry.label.as_deref() == Some("alerts") && entry.value == "only failures"
        ));
        assert!(consolidated
            .iter()
            .any(|entry| entry.label.as_deref() == Some("tone") && entry.value == "short"));
    }
}
