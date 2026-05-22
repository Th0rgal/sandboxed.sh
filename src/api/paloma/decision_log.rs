use crate::api::mission_store::PalomaDecision;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn generated_text_preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

pub fn generated_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[allow(clippy::too_many_arguments)]
pub fn new_decision(
    event_source: &str,
    mission_id: Option<Uuid>,
    user_id: Option<i64>,
    channel: &str,
    reason_code: &str,
    proposed_action: &str,
    allowed: bool,
    suppression_reason: Option<&str>,
    policy_snapshot_json: &str,
    generated_text: Option<&str>,
    created_at: String,
) -> PalomaDecision {
    PalomaDecision {
        id: Uuid::new_v4(),
        event_source: event_source.to_string(),
        mission_id,
        user_id,
        channel: channel.to_string(),
        reason_code: reason_code.to_string(),
        proposed_action: proposed_action.to_string(),
        allowed,
        suppression_reason: suppression_reason.map(ToOwned::to_owned),
        policy_snapshot_json: policy_snapshot_json.to_string(),
        generated_text_hash: generated_text.map(generated_text_hash),
        generated_text_preview: generated_text.map(generated_text_preview),
        created_at,
    }
}
