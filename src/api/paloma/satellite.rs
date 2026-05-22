use crate::api::paloma::capability::{CapabilityRegistry, PalomaCapability};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteRegistration {
    pub satellite_id: String,
    pub registered_at: String,
    pub expires_at: String,
    pub capabilities: Vec<PalomaCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteAuditRecord {
    pub id: Uuid,
    pub satellite_id: String,
    pub capability_name: String,
    pub allowed: bool,
    pub reason: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatelliteCapabilityDecision {
    Allowed,
    Denied(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteToolRequest {
    pub id: Uuid,
    pub satellite_id: String,
    pub capability_name: String,
    pub payload_json: String,
    pub requested_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteToolResponse {
    pub request_id: Uuid,
    pub accepted: bool,
    pub reason: String,
    pub created_at: String,
}

#[derive(Debug, Default)]
pub struct LocalSatelliteRegistry {
    capabilities: CapabilityRegistry,
    registration: Option<SatelliteRegistration>,
    audit_log: Vec<SatelliteAuditRecord>,
}

impl LocalSatelliteRegistry {
    pub fn register(
        &mut self,
        satellite_id: impl Into<String>,
        capabilities: Vec<PalomaCapability>,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> SatelliteRegistration {
        let registration = SatelliteRegistration {
            satellite_id: satellite_id.into(),
            registered_at: now.to_rfc3339(),
            expires_at: (now + ttl).to_rfc3339(),
            capabilities,
        };
        for capability in &registration.capabilities {
            self.capabilities.register(capability.clone());
        }
        self.registration = Some(registration.clone());
        registration
    }

    pub fn is_online_at(&self, now: DateTime<Utc>) -> bool {
        self.registration
            .as_ref()
            .and_then(|registration| {
                DateTime::parse_from_rfc3339(&registration.expires_at)
                    .ok()
                    .map(|expires_at| expires_at.with_timezone(&Utc) > now)
            })
            .unwrap_or(false)
    }

    pub fn decide_capability(
        &mut self,
        capability_name: &str,
        now: DateTime<Utc>,
    ) -> SatelliteCapabilityDecision {
        let Some(registration) = self.registration.as_ref() else {
            self.audit(capability_name, false, "satellite_offline", now);
            return SatelliteCapabilityDecision::Denied("satellite_offline".to_string());
        };
        let satellite_id = registration.satellite_id.clone();
        if !self.is_online_at(now) {
            self.audit_for(
                &satellite_id,
                capability_name,
                false,
                "satellite_offline",
                now,
            );
            return SatelliteCapabilityDecision::Denied("satellite_offline".to_string());
        }
        if !self.capabilities.local_tool_allowed(capability_name) {
            self.audit_for(
                &satellite_id,
                capability_name,
                false,
                "missing_explicit_capability",
                now,
            );
            return SatelliteCapabilityDecision::Denied("missing_explicit_capability".to_string());
        }
        self.audit_for(&satellite_id, capability_name, true, "allowed", now);
        SatelliteCapabilityDecision::Allowed
    }

    pub fn request_capability(
        &mut self,
        request: &SatelliteToolRequest,
        now: DateTime<Utc>,
    ) -> SatelliteToolResponse {
        match self.decide_capability(&request.capability_name, now) {
            SatelliteCapabilityDecision::Allowed => SatelliteToolResponse {
                request_id: request.id,
                accepted: true,
                reason: "accepted".to_string(),
                created_at: now.to_rfc3339(),
            },
            SatelliteCapabilityDecision::Denied(reason) => SatelliteToolResponse {
                request_id: request.id,
                accepted: false,
                reason,
                created_at: now.to_rfc3339(),
            },
        }
    }

    pub fn remote_paloma_can_answer(&self, now: DateTime<Utc>) -> bool {
        !self.is_online_at(now)
    }

    pub fn audit_log(&self) -> &[SatelliteAuditRecord] {
        &self.audit_log
    }

    fn audit(&mut self, capability_name: &str, allowed: bool, reason: &str, now: DateTime<Utc>) {
        self.audit_for("offline", capability_name, allowed, reason, now);
    }

    fn audit_for(
        &mut self,
        satellite_id: &str,
        capability_name: &str,
        allowed: bool,
        reason: &str,
        now: DateTime<Utc>,
    ) {
        self.audit_log.push(SatelliteAuditRecord {
            id: Uuid::new_v4(),
            satellite_id: satellite_id.to_string(),
            capability_name: capability_name.to_string(),
            allowed,
            reason: reason.to_string(),
            created_at: now.to_rfc3339(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn satellite_requires_online_explicit_audited_capability() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
        let mut registry = LocalSatelliteRegistry::default();

        assert!(registry.remote_paloma_can_answer(now));
        assert_eq!(
            registry.decide_capability("open_local_file", now),
            SatelliteCapabilityDecision::Denied("satellite_offline".to_string())
        );

        registry.register(
            "laptop",
            vec![PalomaCapability {
                id: Uuid::new_v4(),
                name: "open_local_file".to_string(),
                source: "laptop".to_string(),
                audit_required: true,
            }],
            now,
            Duration::minutes(5),
        );

        assert_eq!(
            registry.decide_capability("open_local_file", now),
            SatelliteCapabilityDecision::Allowed
        );
        assert_eq!(
            registry.decide_capability("read_keychain", now),
            SatelliteCapabilityDecision::Denied("missing_explicit_capability".to_string())
        );
        assert_eq!(registry.audit_log().len(), 3);
    }

    #[test]
    fn satellite_tool_request_protocol_accepts_only_audited_capabilities() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
        let mut registry = LocalSatelliteRegistry::default();
        registry.register(
            "laptop",
            vec![PalomaCapability {
                id: Uuid::new_v4(),
                name: "open_local_file".to_string(),
                source: "laptop".to_string(),
                audit_required: true,
            }],
            now,
            Duration::minutes(5),
        );

        let allowed = registry.request_capability(
            &SatelliteToolRequest {
                id: Uuid::new_v4(),
                satellite_id: "laptop".to_string(),
                capability_name: "open_local_file".to_string(),
                payload_json: "{}".to_string(),
                requested_at: now.to_rfc3339(),
            },
            now,
        );
        assert!(allowed.accepted);
        assert_eq!(allowed.reason, "accepted");

        let denied = registry.request_capability(
            &SatelliteToolRequest {
                id: Uuid::new_v4(),
                satellite_id: "laptop".to_string(),
                capability_name: "read_keychain".to_string(),
                payload_json: "{}".to_string(),
                requested_at: now.to_rfc3339(),
            },
            now,
        );
        assert!(!denied.accepted);
        assert_eq!(denied.reason, "missing_explicit_capability");
    }
}
