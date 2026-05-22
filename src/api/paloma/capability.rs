use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalomaCapability {
    pub id: Uuid,
    pub name: String,
    pub source: String,
    pub audit_required: bool,
}

#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    capabilities: Vec<PalomaCapability>,
}

impl CapabilityRegistry {
    pub fn register(&mut self, capability: PalomaCapability) {
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|item| item.name == capability.name && item.source == capability.source)
        {
            *existing = capability;
        } else {
            self.capabilities.push(capability);
        }
    }

    pub fn list(&self) -> &[PalomaCapability] {
        &self.capabilities
    }

    pub fn local_tool_allowed(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability.name == name && capability.audit_required)
    }
}

pub fn default_remote_capabilities() -> Vec<PalomaCapability> {
    [
        "list_missions",
        "summarize_mission",
        "send_message_to_mission",
        "schedule_reminder",
        "update_notification_preference",
        "request_local_satellite_capability",
    ]
    .into_iter()
    .map(|name| PalomaCapability {
        id: Uuid::new_v4(),
        name: name.to_string(),
        source: "remote_paloma".to_string(),
        audit_required: true,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_upserts_by_name_and_source_and_requires_audit_flag_for_local_tools() {
        let mut registry = CapabilityRegistry::default();
        registry.register(PalomaCapability {
            id: Uuid::new_v4(),
            name: "open_local_file".to_string(),
            source: "laptop".to_string(),
            audit_required: false,
        });
        assert!(!registry.local_tool_allowed("open_local_file"));

        registry.register(PalomaCapability {
            id: Uuid::new_v4(),
            name: "open_local_file".to_string(),
            source: "laptop".to_string(),
            audit_required: true,
        });

        assert_eq!(registry.list().len(), 1);
        assert!(registry.local_tool_allowed("open_local_file"));
        assert!(!registry.local_tool_allowed("read_keychain"));
    }

    #[test]
    fn default_remote_capabilities_cover_paloma_control_surface() {
        let capabilities = default_remote_capabilities();
        let names = capabilities
            .iter()
            .map(|capability| capability.name.as_str())
            .collect::<std::collections::HashSet<_>>();

        for expected in [
            "list_missions",
            "summarize_mission",
            "send_message_to_mission",
            "schedule_reminder",
            "update_notification_preference",
            "request_local_satellite_capability",
        ] {
            assert!(names.contains(expected), "missing {expected}");
        }
        assert!(capabilities
            .iter()
            .all(|capability| capability.audit_required));
        assert!(capabilities
            .iter()
            .all(|capability| capability.source == "remote_paloma"));
    }
}
