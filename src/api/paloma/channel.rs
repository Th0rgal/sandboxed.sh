use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalomaChannelKind {
    Telegram,
    LocalSatellite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalomaChannelAddress {
    pub kind: PalomaChannelKind,
    pub channel_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalomaInboundMessage {
    pub address: PalomaChannelAddress,
    pub sender_id: Option<i64>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalomaDeliveryIntent {
    Reply,
    ProactiveAlert,
    Digest,
    ScheduledReminder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalomaOutboundMessage {
    pub address: PalomaChannelAddress,
    pub intent: PalomaDeliveryIntent,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
}

impl PalomaChannelAddress {
    pub fn telegram(channel_id: Uuid, chat_id: i64, mission_id: Option<Uuid>) -> Self {
        Self {
            kind: PalomaChannelKind::Telegram,
            channel_id: channel_id.to_string(),
            session_id: chat_id.to_string(),
            mission_id,
        }
    }

    pub fn queue_session_key(&self) -> String {
        match self.mission_id {
            Some(mission_id) => format!("{}:{}:{}", self.channel_id, self.session_id, mission_id),
            None => format!("{}:{}", self.channel_id, self.session_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_address_preserves_channel_chat_and_optional_mission() {
        let channel_id = Uuid::new_v4();
        let mission_id = Uuid::new_v4();

        let address = PalomaChannelAddress::telegram(channel_id, 123, Some(mission_id));

        assert_eq!(address.kind, PalomaChannelKind::Telegram);
        assert_eq!(address.channel_id, channel_id.to_string());
        assert_eq!(address.session_id, "123");
        assert_eq!(address.mission_id, Some(mission_id));
        assert_eq!(
            address.queue_session_key(),
            format!("{channel_id}:123:{mission_id}")
        );
    }

    #[test]
    fn outbound_message_keeps_delivery_intent_outside_text() {
        let address = PalomaChannelAddress::telegram(Uuid::new_v4(), 123, None);
        let outbound = PalomaOutboundMessage {
            address: address.clone(),
            intent: PalomaDeliveryIntent::Digest,
            text: "2 mission updates".to_string(),
            reply_to_message_id: None,
        };

        assert_eq!(outbound.address, address);
        assert_eq!(outbound.intent, PalomaDeliveryIntent::Digest);
        assert_eq!(outbound.text, "2 mission updates");
    }
}
