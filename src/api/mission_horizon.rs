//! Item-first mission horizon.
//!
//! Controllers should see durable work items (`track` / `task_key`) and only
//! the live or unabsorbed attempts on those items. Historical, acknowledged,
//! completed, and replaced attempts stay in the store for lineage but drop
//! out of the default listing and the project snapshot.
//!
//! Wake routing prefers the bound project conversation over an isolated
//! `webhook:mission-complete:*` session.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use super::control::events::MissionStatus;
use super::mission_store::{
    default_attention_keeps, Mission, MissionFilter, MissionProjectPatch, MissionStore,
    SUPERSEDED_TAG,
};
use super::projects_store::{ProjectTrack, RoadmapProposal};

/// How this mission participates in a PR, if at all. Writer and reviewer
/// attempts on the same track are different items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterRole {
    Writer,
    Readonly,
    None,
}

pub fn writer_role(mission: &Mission) -> WriterRole {
    if mission.project.tags.iter().any(|tag| tag == "pr-writer") {
        WriterRole::Writer
    } else if mission.project.tags.iter().any(|tag| tag == "pr-readonly") {
        WriterRole::Readonly
    } else {
        WriterRole::None
    }
}

fn role_label(role: WriterRole) -> Option<&'static str> {
    match role {
        WriterRole::Writer => Some("pr-writer"),
        WriterRole::Readonly => Some("pr-readonly"),
        WriterRole::None => None,
    }
}

/// Absorb every prior attention-horizon attempt on the same
/// `(project, track, writer role)` so a new start replaces the old one
/// without a separate model acknowledge.
pub async fn supersede_prior_attempts(
    store: &dyn MissionStore,
    new_mission: &Mission,
) -> Result<Vec<Uuid>, String> {
    let Some(project) = new_mission
        .project
        .project
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    let Some(track) = new_mission
        .project
        .track
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    let role = writer_role(new_mission);
    let candidates = store
        .list_missions_filtered(
            &MissionFilter {
                project: Some(project.to_string()),
                track: Some(track.to_string()),
                attention_only: true,
                ..Default::default()
            },
            50,
            0,
        )
        .await?;
    let mut absorbed = Vec::new();
    for prior in candidates {
        if prior.id == new_mission.id || writer_role(&prior) != role {
            continue;
        }
        let mut tags = prior.project.tags.clone();
        if !tags.iter().any(|tag| tag == SUPERSEDED_TAG) {
            tags.push(SUPERSEDED_TAG.to_string());
        }
        store
            .update_mission_project(
                prior.id,
                MissionProjectPatch {
                    tags: Some(tags),
                    ..Default::default()
                },
            )
            .await?;
        if store.get_active_mission_run(prior.id).await?.is_none() {
            if let Err(error) = store
                .update_mission_status(prior.id, MissionStatus::Acknowledged)
                .await
            {
                tracing::warn!(
                    mission_id = %prior.id,
                    %error,
                    "could not acknowledge a superseded attempt"
                );
            }
        }
        absorbed.push(prior.id);
    }
    Ok(absorbed)
}

/// One durable work item as presented to a controller.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectItem {
    pub key: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub open: bool,
    pub attempts: Vec<ProjectItemAttempt>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectItemAttempt {
    pub id: Uuid,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Group declared tracks, open proposals, and attention-horizon missions
/// into items. Historical / absorbed missions never become the inventory.
pub fn project_items(
    tracks: &[ProjectTrack],
    proposals: &[RoadmapProposal],
    missions: &[Mission],
) -> Vec<ProjectItem> {
    let mut items: BTreeMap<String, ProjectItem> = BTreeMap::new();
    for track in tracks {
        items.insert(
            track.track.clone(),
            ProjectItem {
                key: track.track.clone(),
                kind: "track",
                desired_state: track.desired_state.clone(),
                status: track.status.clone(),
                open: !matches!(track.status.as_deref(), Some("done") | Some("cancelled")),
                attempts: Vec::new(),
            },
        );
    }
    for proposal in proposals {
        items
            .entry(proposal.task_key.clone())
            .or_insert_with(|| ProjectItem {
                key: proposal.task_key.clone(),
                kind: "task",
                desired_state: None,
                status: Some("proposed".to_string()),
                open: true,
                attempts: Vec::new(),
            });
    }
    for mission in missions {
        if !default_attention_keeps(mission) {
            continue;
        }
        let key = mission
            .project
            .track
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("untagged")
            .to_string();
        let item = items.entry(key.clone()).or_insert_with(|| ProjectItem {
            key: key.clone(),
            kind: "track",
            desired_state: mission.project.desired_state.clone(),
            status: None,
            open: true,
            attempts: Vec::new(),
        });
        item.attempts.push(ProjectItemAttempt {
            id: mission.id,
            status: mission.status.to_string(),
            title: mission.title.clone(),
            updated_at: mission.updated_at.clone(),
            role: role_label(writer_role(mission)).map(str::to_string),
        });
        item.open = true;
    }
    for item in items.values_mut() {
        item.attempts
            .sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    }
    items.into_values().collect()
}

/// Where a terminal / `awaiting_user` callback should land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeTarget {
    pub session: Option<String>,
    pub source: &'static str,
}

impl WakeTarget {
    pub fn isolated() -> Self {
        Self {
            session: None,
            source: "isolated",
        }
    }
}

/// Prefer the bound project conversation, then the creating origin.
/// Isolated is last: that is the `webhook:mission-complete:*` fallback.
pub fn resolve_mission_wake_target(
    origin_session: Option<&str>,
    project_session: Option<&str>,
) -> WakeTarget {
    if let Some(session) = project_session
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return WakeTarget {
            session: Some(session.to_string()),
            source: "project",
        };
    }
    if let Some(session) = origin_session
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return WakeTarget {
            session: Some(session.to_string()),
            source: "origin",
        };
    }
    WakeTarget::isolated()
}

/// Read-only lookup of the bound control session. Does not open the
/// projects store (which would migrate); a missing file is "no binding".
pub fn bound_session_from_projects_db(working_dir: &Path, slug: &str) -> Option<String> {
    let path = working_dir.join(".sandboxed-sh/projects.db");
    if !path.exists() {
        return None;
    }
    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    connection
        .query_row(
            "SELECT control_session_id FROM project_bindings WHERE slug = ?1",
            [slug],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(|session| session.trim().to_string())
        .filter(|session| !session.is_empty())
}

/// Shipped wake decision for a mission-status webhook: project binding,
/// then origin, else isolated.
pub fn wake_fields_for_mission(mission: &Mission, working_dir: &Path) -> WakeTarget {
    let bound = mission
        .project
        .project
        .as_deref()
        .and_then(|slug| bound_session_from_projects_db(working_dir, slug));
    resolve_mission_wake_target(mission.origin_session_id.as_deref(), bound.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::{InMemoryMissionStore, MissionStore};
    use crate::api::projects_store::ProjectsStore;
    use std::sync::Arc;

    async fn seed(
        store: &Arc<dyn MissionStore>,
        title: &str,
        project: &str,
        track: &str,
        status: MissionStatus,
        tags: &[&str],
    ) -> Mission {
        let mission = store
            .create_mission(Some(title), None, None, None, None, None, None)
            .await
            .expect("create");
        store
            .update_mission_project(
                mission.id,
                MissionProjectPatch {
                    project: Some(Some(project.to_string())),
                    track: Some(Some(track.to_string())),
                    tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
                    ..Default::default()
                },
            )
            .await
            .expect("tag");
        if status != MissionStatus::Pending {
            store
                .update_mission_status(mission.id, status)
                .await
                .expect("status");
        }
        store
            .get_mission(mission.id)
            .await
            .expect("get")
            .expect("exists")
    }

    async fn default_list(store: &Arc<dyn MissionStore>) -> Vec<Uuid> {
        store
            .list_missions_filtered(
                &MissionFilter {
                    attention_only: true,
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .expect("list")
            .into_iter()
            .map(|mission| mission.id)
            .collect()
    }

    #[tokio::test]
    async fn default_list_keeps_live_and_unabsorbed_failed() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let live = seed(&store, "live", "verity", "core", MissionStatus::Active, &[]).await;
        let waiting = seed(
            &store,
            "waiting",
            "verity",
            "core",
            MissionStatus::AwaitingUser,
            &[],
        )
        .await;
        let failed = seed(
            &store,
            "failed",
            "verity",
            "review",
            MissionStatus::Failed,
            &[],
        )
        .await;
        let interrupted = seed(
            &store,
            "interrupted",
            "verity",
            "review",
            MissionStatus::Interrupted,
            &[],
        )
        .await;
        let acked = seed(
            &store,
            "acked",
            "verity",
            "old",
            MissionStatus::Acknowledged,
            &[],
        )
        .await;
        let completed = seed(
            &store,
            "done",
            "verity",
            "old",
            MissionStatus::Completed,
            &[],
        )
        .await;
        let absorbed = seed(
            &store,
            "replaced",
            "verity",
            "core",
            MissionStatus::Failed,
            &[SUPERSEDED_TAG],
        )
        .await;

        let ids = default_list(&store).await;
        assert!(ids.contains(&live.id));
        assert!(ids.contains(&waiting.id));
        assert!(ids.contains(&failed.id));
        assert!(ids.contains(&interrupted.id));
        assert!(!ids.contains(&acked.id));
        assert!(!ids.contains(&completed.id));
        assert!(!ids.contains(&absorbed.id));
    }

    #[tokio::test]
    async fn start_attempt_absorbs_the_prior_same_item() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let first = seed(
            &store,
            "attempt-1",
            "coldcard",
            "skip-kernel",
            MissionStatus::Failed,
            &[],
        )
        .await;
        let second = seed(
            &store,
            "attempt-2",
            "coldcard",
            "skip-kernel",
            MissionStatus::Pending,
            &[],
        )
        .await;

        let absorbed = supersede_prior_attempts(store.as_ref(), &second)
            .await
            .expect("supersede");
        assert_eq!(absorbed, vec![first.id]);

        let ids = default_list(&store).await;
        assert!(ids.contains(&second.id));
        assert!(!ids.contains(&first.id));

        let prior = store
            .get_mission(first.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(prior.status, MissionStatus::Acknowledged);
        assert!(prior.project.tags.iter().any(|tag| tag == SUPERSEDED_TAG));
    }

    #[tokio::test]
    async fn writer_and_reviewer_do_not_absorb_each_other() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let writer = seed(
            &store,
            "write",
            "lido",
            "merge-66",
            MissionStatus::AwaitingUser,
            &["pr-writer"],
        )
        .await;
        let reviewer = seed(
            &store,
            "review",
            "lido",
            "merge-66",
            MissionStatus::Pending,
            &["pr-readonly"],
        )
        .await;

        let absorbed = supersede_prior_attempts(store.as_ref(), &reviewer)
            .await
            .expect("supersede");
        assert!(absorbed.is_empty());
        let ids = default_list(&store).await;
        assert!(ids.contains(&writer.id));
        assert!(ids.contains(&reviewer.id));
    }

    #[tokio::test]
    async fn project_items_are_keyed_by_track_not_history() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let live = seed(&store, "now", "verity", "core", MissionStatus::Active, &[]).await;
        seed(
            &store,
            "old",
            "verity",
            "core",
            MissionStatus::Completed,
            &[],
        )
        .await;
        seed(
            &store,
            "replaced",
            "verity",
            "core",
            MissionStatus::Failed,
            &[SUPERSEDED_TAG],
        )
        .await;
        let failed = seed(
            &store,
            "review-fail",
            "verity",
            "review",
            MissionStatus::Failed,
            &[],
        )
        .await;

        let tracks = vec![ProjectTrack {
            track: "core".to_string(),
            desired_state: Some("landed".to_string()),
            status: Some("active".to_string()),
            updated_at: "2026-08-14T00:00:00Z".to_string(),
        }];
        let proposals = vec![RoadmapProposal {
            task_key: "docs".to_string(),
            title: "Write the guide".to_string(),
            prompt: None,
            acceptance_criteria: Vec::new(),
            depends_on: Vec::new(),
            created_at: "2026-08-14T00:00:00Z".to_string(),
            updated_at: "2026-08-14T00:00:00Z".to_string(),
        }];
        let missions = store.list_missions(50, 0).await.expect("all");
        let items = project_items(&tracks, &proposals, &missions);

        let core = items.iter().find(|item| item.key == "core").expect("core");
        assert_eq!(core.kind, "track");
        assert!(core.open);
        assert_eq!(core.attempts.len(), 1);
        assert_eq!(core.attempts[0].id, live.id);

        let review = items
            .iter()
            .find(|item| item.key == "review")
            .expect("review");
        assert_eq!(review.attempts.len(), 1);
        assert_eq!(review.attempts[0].id, failed.id);

        let docs = items.iter().find(|item| item.key == "docs").expect("docs");
        assert_eq!(docs.kind, "task");
        assert!(docs.open);
        assert!(docs.attempts.is_empty());

        assert!(items.iter().all(|item| item
            .attempts
            .iter()
            .all(|attempt| attempt.id != live.id || item.key == "core")));

        let cancelled = vec![ProjectTrack {
            track: "old-pr46".to_string(),
            desired_state: Some("certify PR 46".to_string()),
            status: Some("cancelled".to_string()),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
        }];
        let closed = project_items(&cancelled, &[], &[]);
        assert_eq!(closed.len(), 1);
        assert!(!closed[0].open, "cancelled tracks are not open items");
        let historical = missions
            .iter()
            .filter(|mission| {
                matches!(
                    mission.status,
                    MissionStatus::Completed | MissionStatus::Acknowledged
                ) || mission.project.tags.iter().any(|tag| tag == SUPERSEDED_TAG)
            })
            .count();
        assert!(historical >= 2);
        assert!(items
            .iter()
            .flat_map(|item| item.attempts.iter())
            .all(|attempt| attempt.status != "completed" && attempt.status != "acknowledged"));
    }

    #[test]
    fn wake_prefers_project_binding_over_origin_and_isolated() {
        let isolated = resolve_mission_wake_target(None, None);
        assert_eq!(isolated.source, "isolated");
        assert!(isolated.session.is_none());

        let origin = resolve_mission_wake_target(Some("origin-sess"), None);
        assert_eq!(origin.source, "origin");
        assert_eq!(origin.session.as_deref(), Some("origin-sess"));

        let project = resolve_mission_wake_target(Some("origin-sess"), Some("project-tip"));
        assert_eq!(project.source, "project");
        assert_eq!(project.session.as_deref(), Some("project-tip"));
        assert_ne!(
            project.session.as_deref(),
            Some("webhook:mission-complete:x")
        );
    }

    #[tokio::test]
    async fn shipped_wake_path_uses_project_tip_from_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_dir = dir.path().join(".sandboxed-sh");
        std::fs::create_dir_all(&db_dir).expect("mkdir");
        let store = ProjectsStore::open(db_dir.join("projects.db")).expect("projects db");
        store
            .upsert_project("benchmark", Some("Benchmark"), None, None, None)
            .expect("project");
        store
            .set_binding("benchmark", "20260814_094937_8c5097", None)
            .expect("bind");

        let missions: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let mut mission = seed(
            &missions,
            "strat-50",
            "benchmark",
            "run",
            MissionStatus::AwaitingUser,
            &[],
        )
        .await;
        mission.origin_session_id = Some("20260814_review_child".to_string());

        let wake = wake_fields_for_mission(&mission, dir.path());
        assert_eq!(wake.source, "project");
        assert_eq!(wake.session.as_deref(), Some("20260814_094937_8c5097"));
    }
}
