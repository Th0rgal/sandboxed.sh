use std::collections::HashMap;
use uuid::Uuid;

pub fn paloma_channel_job_name(base_name: &str, channel_id: Uuid) -> String {
    format!("{base_name}:{channel_id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalomaJobState {
    pub name: &'static str,
    pub runs: u64,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub struct PalomaJobRegistry {
    jobs: HashMap<&'static str, PalomaJobState>,
}

impl PalomaJobRegistry {
    pub fn with_default_jobs() -> Self {
        let mut registry = Self::default();
        for name in [
            "paloma_alert_scan",
            "paloma_due_messages",
            "paloma_memory_consolidation",
            "paloma_digest_flush",
            "paloma_stale_recovery",
            "paloma_mission_cards",
        ] {
            registry.jobs.insert(
                name,
                PalomaJobState {
                    name,
                    runs: 0,
                    last_success_at: None,
                    last_error: None,
                },
            );
        }
        registry
    }

    pub fn record_success(&mut self, name: &'static str, at: String) {
        let state = self.jobs.entry(name).or_insert(PalomaJobState {
            name,
            runs: 0,
            last_success_at: None,
            last_error: None,
        });
        state.runs += 1;
        state.last_success_at = Some(at);
        state.last_error = None;
    }

    pub fn record_failure(&mut self, name: &'static str, error: String) {
        let state = self.jobs.entry(name).or_insert(PalomaJobState {
            name,
            runs: 0,
            last_success_at: None,
            last_error: None,
        });
        state.runs += 1;
        state.last_error = Some(error);
    }

    pub fn states(&self) -> Vec<PalomaJobState> {
        let mut states = self.jobs.values().cloned().collect::<Vec<_>>();
        states.sort_by_key(|state| state.name);
        states
    }
}
