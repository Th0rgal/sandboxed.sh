//! Identity envelope for the existing deferred-goal scheduler. Prompt and message
//! identities are one durable value, written before acceptance. The envelope is
//! removed before harness execution; SSE/history expose it for reconciliation.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MARKER: &str = "\n<!-- sandboxed:messages:v1:";
#[derive(Serialize, Deserialize)]
struct Envelope {
    messages: Vec<(Uuid, String)>,
}

pub(crate) fn wrap(prompt: &str, messages: Vec<(Uuid, String)>) -> String {
    if messages.is_empty() {
        return prompt.to_string();
    }
    let data = STANDARD
        .encode(serde_json::to_vec(&Envelope { messages }).expect("serializable message envelope"));
    format!("{prompt}{MARKER}{data} -->")
}
pub(crate) fn encode(id: Uuid, content: &str) -> String {
    wrap(content, vec![(id, content.to_string())])
}
pub(crate) fn decode(content: &str) -> (String, Vec<(Uuid, String)>) {
    let Some(end) = content.strip_suffix(" -->") else {
        return (content.into(), vec![]);
    };
    let Some((prompt, encoded)) = end.rsplit_once(MARKER) else {
        return (content.into(), vec![]);
    };
    let parsed = STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Envelope>(&bytes).ok());
    match parsed {
        Some(envelope) if !envelope.messages.is_empty() => (prompt.into(), envelope.messages),
        _ => (content.into(), vec![]),
    }
}
pub(crate) fn strip(content: &str) -> String {
    decode(content).0
}
pub(crate) fn join(previous: &str, incoming: &str) -> String {
    let (before, mut messages) = decode(previous);
    let (after, next) = decode(incoming);
    // Scheduler capacity retries carry the same persisted batch. They must
    // neither repeat its prompt nor duplicate the original message identities.
    if !next.is_empty()
        && next
            .iter()
            .all(|(id, _)| messages.iter().any(|(old, _)| old == id))
    {
        return previous.to_string();
    }
    messages.extend(next);
    wrap(&format!("{before}\n{after}"), messages)
}

/// Public events carry structured identity metadata, never the persistence trailer.
pub(crate) fn stream_payload(event: &super::AgentEvent) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(event)?;
    if let super::AgentEvent::UserMessage {
        content, source, ..
    } = event
    {
        if source.as_deref() == Some("scheduler") {
            let (prompt, messages) = decode(content);
            if !messages.is_empty() {
                value["content"] = prompt.into();
                value["messages"] = serde_json::to_value(messages)?;
            }
        }
    }
    serde_json::to_string(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_survive_identical_text_unicode_and_attachments() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let text = "héllo 🦀\n<!-- paloma:attachment:receipt -->\n";
        let combined = join(&encode(a, text), &encode(b, text));
        assert_eq!(
            decode(&combined),
            (
                format!("{text}\n{text}"),
                vec![(a, text.into()), (b, text.into())]
            )
        );
        assert_eq!(strip(&combined), format!("{text}\n{text}"));
        assert_eq!(join(&combined, &combined), combined);
    }
    #[test]
    fn literal_inner_envelopes_and_goal_rewrites_preserve_original_messages() {
        let id = Uuid::new_v4();
        let literal = encode(Uuid::new_v4(), "literal inner marker");
        let stored = join("legacy goal", &encode(id, &literal));
        let (prompt, messages) = decode(&stored);
        assert_eq!(prompt, format!("legacy goal\n{literal}"));
        assert_eq!(messages, vec![(id, literal)]);
        let rewritten = wrap("rewritten harness goal", messages.clone());
        assert_eq!(
            decode(&rewritten),
            ("rewritten harness goal".into(), messages)
        );
    }
    #[test]
    fn malformed_envelopes_are_ordinary_content() {
        for value in [
            "hi\n<!-- sandboxed:messages:v1:not-base64 -->",
            "plain prompt",
        ] {
            assert!(decode(value).1.is_empty());
            assert_eq!(strip(value), value);
        }
    }
}
