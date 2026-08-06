//! Durable "this status was forwarded" markers for the mission webhook.
//!
//! The forwarder's dedupe map used to live in memory, which left two windows
//! where a transition could be lost for good (2026-08-06 analysis of mission
//! `45e54e0b`, whose completed status never reached its waiting conversation):
//!
//! * a restart blanked the map, so anything that transitioned while the
//!   service was down was "adopted" silently and never forwarded;
//! * the marker was recorded when the event was *observed*, not when the POST
//!   *succeeded* — an unreachable consumer meant the event was considered
//!   delivered after three failed attempts.
//!
//! A marker here means "this status has been fully processed": for
//! non-forwardable statuses that is trivially true on observation, for
//! forwardable ones it is written only after a successful POST. Divergence
//! between a mission's current status and its marker is therefore exactly the
//! set of undelivered transitions, regardless of how they were missed —
//! broadcast lag, a writer that never broadcast, a restart, or a consumer
//! outage. The reconcile sweep retries them until they land.
//!
//! Storage is one small JSON file under the working directory (the same home
//! as the remote-job ledger), written atomically via tmp+rename. The file is
//! tiny — one entry per mission ever seen — and rewritten at most once per
//! processed transition.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::control::MissionStatus;

const FILE_NAME: &str = "webhook_forward_markers.json";

#[derive(Debug)]
pub struct MarkerStore {
    path: PathBuf,
    markers: HashMap<Uuid, MissionStatus>,
    /// Whether the backing file existed at load time. Its absence is how the
    /// forwarder distinguishes "first boot with this feature" (adopt current
    /// history without forwarding it) from "restart" (markers are authority).
    pub loaded_from_disk: bool,
}

impl MarkerStore {
    pub fn load(working_dir: &Path) -> Self {
        let path = working_dir.join(FILE_NAME);
        let (markers, loaded_from_disk) = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<HashMap<String, serde_json::Value>>(&raw) {
                Ok(parsed) => {
                    let markers = parsed
                        .into_iter()
                        .filter_map(|(id, status)| {
                            Some((
                                Uuid::parse_str(&id).ok()?,
                                serde_json::from_value::<MissionStatus>(status).ok()?,
                            ))
                        })
                        .collect();
                    (markers, true)
                }
                Err(error) => {
                    // A corrupt file must not brick forwarding; treat it as
                    // first boot. The cost is one silent re-adoption pass,
                    // never a replay storm (adoption forwards nothing).
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "webhook marker file unreadable; re-adopting current state"
                    );
                    (HashMap::new(), false)
                }
            },
            Err(_) => (HashMap::new(), false),
        };
        Self {
            path,
            markers,
            loaded_from_disk,
        }
    }

    pub fn get(&self, mission_id: Uuid) -> Option<MissionStatus> {
        self.markers.get(&mission_id).copied()
    }

    /// Record and persist. Failure to persist is logged, never fatal: the
    /// in-memory marker still dedupes this process, and the worst outcome of
    /// a lost write is one duplicate forward after a restart — the recoverable
    /// direction (consumers carry `mission_id` + status and tolerate repeats;
    /// a *dropped* wake-up is what they cannot recover from).
    pub fn set(&mut self, mission_id: Uuid, status: MissionStatus) {
        if self.markers.insert(mission_id, status) == Some(status) {
            return;
        }
        self.save();
    }

    fn save(&self) {
        let serializable: HashMap<String, &MissionStatus> = self
            .markers
            .iter()
            .map(|(id, status)| (id.to_string(), status))
            .collect();
        let Ok(raw) = serde_json::to_string(&serializable) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        let result = std::fs::write(&tmp, raw).and_then(|_| std::fs::rename(&tmp, &self.path));
        if let Err(error) = result {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "failed to persist webhook forward markers"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_first_boot_has_no_disk_state() {
        let d = dir();
        let store = MarkerStore::load(d.path());
        assert!(!store.loaded_from_disk);
        assert_eq!(store.get(Uuid::new_v4()), None);
    }

    /// The property the whole change exists for: markers survive a restart.
    #[test]
    fn markers_survive_a_reload() {
        let d = dir();
        let mission = Uuid::new_v4();
        let mut store = MarkerStore::load(d.path());
        store.set(mission, MissionStatus::Acknowledged);

        let reloaded = MarkerStore::load(d.path());
        assert!(reloaded.loaded_from_disk);
        assert_eq!(reloaded.get(mission), Some(MissionStatus::Acknowledged));
    }

    #[test]
    fn an_unchanged_set_does_not_rewrite_the_file() {
        let d = dir();
        let mission = Uuid::new_v4();
        let mut store = MarkerStore::load(d.path());
        store.set(mission, MissionStatus::Active);
        let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        let path = d.path().join(FILE_NAME);
        let before = mtime(&path);
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.set(mission, MissionStatus::Active);
        assert_eq!(mtime(&path), before, "idempotent set must not rewrite");
    }

    /// A corrupt file is first boot, not a crash: adoption forwards nothing,
    /// so the failure direction is one silent pass, never a replay storm.
    #[test]
    fn a_corrupt_file_reads_as_first_boot() {
        let d = dir();
        std::fs::write(d.path().join(FILE_NAME), "{not json").unwrap();
        let store = MarkerStore::load(d.path());
        assert!(!store.loaded_from_disk);
    }

    #[test]
    fn unknown_statuses_in_the_file_are_skipped_not_fatal() {
        let d = dir();
        let known = Uuid::new_v4();
        std::fs::write(
            d.path().join(FILE_NAME),
            format!(
                "{{\"{known}\": \"acknowledged\", \"{}\": \"status_from_the_future\"}}",
                Uuid::new_v4()
            ),
        )
        .unwrap();
        let store = MarkerStore::load(d.path());
        assert!(store.loaded_from_disk);
        assert_eq!(store.get(known), Some(MissionStatus::Acknowledged));
    }
}
