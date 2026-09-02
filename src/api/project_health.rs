//! Per-track health rollup for the projects overview.
//!
//! A project row already lists its missions, but a list of chips does not
//! answer the question an operator actually has: *which track is stuck, and
//! why?* On a project with 800 missions across a dozen tracks nobody reads the
//! chips. This aggregates what the mission records already carry — status,
//! `desired_state`, `next_check_at` — into one verdict per track, ordered so
//! the tracks that need a human come first.
//!
//! Everything here is derived. Nothing is stored, nothing is written, and no
//! new source of truth is introduced: if the rollup disagrees with the mission
//! list, the mission list is right.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::control::events::MissionStatus;

/// What a track needs from a human, if anything.
///
/// Deliberately coarse. A finer vocabulary would invite the UI to render
/// distinctions the underlying data cannot actually support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackVerdict {
    /// Something failed and nothing is working on it any more.
    Failing,
    /// A `waiting_*` mission blew through its own `next_check_at`.
    Overdue,
    /// Work is in flight.
    Active,
    /// Everything terminal, at least one success, nothing failed.
    Done,
    /// No work in flight, nothing failed, nothing overdue. Usually means the
    /// track finished its last mission and nobody queued another.
    Idle,
}

impl TrackVerdict {
    /// Sort key — lower sorts first, so the worst tracks lead.
    fn rank(self) -> u8 {
        match self {
            Self::Failing => 0,
            Self::Overdue => 1,
            Self::Active => 2,
            Self::Idle => 3,
            Self::Done => 4,
        }
    }

    /// Whether this verdict is something an operator should look at.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Failing | Self::Overdue)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackHealth {
    /// `None` for missions carrying a project but no track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    pub verdict: TrackVerdict,
    pub missions: usize,
    pub active: usize,
    pub failed: usize,
    pub completed: usize,
    /// Missions whose `next_check_at` is in the past.
    pub overdue: usize,
    /// Operator-declared states in play, with counts, e.g. `waiting_ci: 2`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub desired_states: BTreeMap<String, usize>,
    /// Newest `updated_at` across the track's missions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectHealth {
    pub missions: usize,
    pub active: usize,
    pub failed: usize,
    pub overdue: usize,
    /// Tracks needing attention, i.e. `Failing` or `Overdue`.
    pub tracks_needing_attention: usize,
    /// Worst-first. The UI can show the head of this and be showing the part
    /// that matters.
    pub tracks: Vec<TrackHealth>,
}

/// The five fields the rollup actually reads.
///
/// Taking this rather than `&Mission` keeps the rollup a pure function over
/// plain data: it can be tested with one-line fixtures instead of standing up
/// a mission store, and it cannot accidentally start depending on the other
/// forty fields of a mission.
#[derive(Debug, Clone, Copy)]
pub struct MissionHealthInput<'a> {
    pub track: Option<&'a str>,
    pub status: MissionStatus,
    pub desired_state: Option<&'a str>,
    pub next_check_at: Option<&'a str>,
    pub updated_at: &'a str,
}

#[derive(Default)]
struct TrackAccumulator {
    missions: usize,
    active: usize,
    failed: usize,
    completed: usize,
    overdue: usize,
    desired_states: BTreeMap<String, usize>,
    last_activity_at: Option<String>,
}

impl TrackAccumulator {
    fn push(&mut self, mission: MissionHealthInput<'_>, now: &str) {
        self.missions += 1;
        // "Active" means work is actually in flight, which is narrower than
        // "non-terminal". `AwaitingUser`, `Acknowledged` and `Paused` are all
        // parked: the agent's turn is over and nothing will move until a human
        // or a watchdog acts. Counting them as active is what makes a stalled
        // track look busy, which is precisely the failure this rollup exists
        // to surface. `Interrupted` counts as a failure to stay consistent
        // with the attention line the overview already renders.
        match mission.status {
            MissionStatus::Pending | MissionStatus::Active | MissionStatus::WaitingBackground => {
                self.active += 1
            }
            MissionStatus::Failed
            | MissionStatus::NotFeasible
            | MissionStatus::Blocked
            | MissionStatus::Interrupted => self.failed += 1,
            MissionStatus::Completed => self.completed += 1,
            MissionStatus::AwaitingUser | MissionStatus::Acknowledged | MissionStatus::Paused => {}
        }

        if let Some(state) = mission.desired_state {
            let state = state.trim();
            if !state.is_empty() {
                *self.desired_states.entry(state.to_string()).or_insert(0) += 1;
            }
        }

        // RFC3339 timestamps in the same offset compare correctly as strings,
        // and the store writes them that way. Comparing lexically avoids
        // dragging a date parser (and its failure modes) into a display path:
        // an unparseable timestamp would otherwise have to become either a
        // silent "not overdue" or an error, and neither is better than this.
        if let Some(due) = mission.next_check_at {
            if !mission.status.is_terminal() && due < now {
                self.overdue += 1;
            }
        }

        let updated = mission.updated_at;
        if self
            .last_activity_at
            .as_deref()
            .is_none_or(|seen| seen < updated)
        {
            self.last_activity_at = Some(updated.to_string());
        }
    }

    fn finish(self, track: Option<String>, plan: Option<&BTreeSet<String>>) -> TrackHealth {
        // `Done` is a statement about the *track*, not about its last
        // mission. When the caller knows the plan (the situation builder's
        // satisfied / claim-only keys), only those tracks may read as done; a
        // completed mission on an open track is `Idle`. Without a plan the
        // legacy mission-derived reading stays.
        let done_per_plan = match (plan, track.as_deref()) {
            (Some(plan), Some(key)) => plan.contains(key),
            (Some(_), None) => false,
            (None, _) => self.completed > 0 && self.failed == 0,
        };
        let verdict = if self.failed > 0 && self.active == 0 {
            TrackVerdict::Failing
        } else if self.overdue > 0 {
            TrackVerdict::Overdue
        } else if self.active > 0 {
            TrackVerdict::Active
        } else if done_per_plan {
            TrackVerdict::Done
        } else {
            TrackVerdict::Idle
        };
        TrackHealth {
            track,
            verdict,
            missions: self.missions,
            active: self.active,
            failed: self.failed,
            completed: self.completed,
            overdue: self.overdue,
            desired_states: self.desired_states,
            last_activity_at: self.last_activity_at,
        }
    }
}

/// Roll a project's missions up into one verdict per track.
///
/// `now` is passed in rather than read from the clock so the caller stamps
/// every project in a listing with the same instant — otherwise two tracks in
/// the same response could disagree about whether the same deadline had passed.
pub fn rollup(missions: &[MissionHealthInput<'_>], now: &str) -> ProjectHealth {
    rollup_with_plan(missions, now, None)
}

/// Like [`rollup`], but `Done` is gated by the plan: only tracks whose key is
/// in `done_keys` (satisfied or claim-only per the situation builder) may be
/// reported done. Pass `Some(&BTreeSet::new())` for "plan known, nothing done".
pub fn rollup_with_plan(
    missions: &[MissionHealthInput<'_>],
    now: &str,
    done_keys: Option<&BTreeSet<String>>,
) -> ProjectHealth {
    let mut by_track: BTreeMap<Option<String>, TrackAccumulator> = BTreeMap::new();
    for mission in missions {
        let track = mission
            .track
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        by_track.entry(track).or_default().push(*mission, now);
    }

    let mut tracks: Vec<TrackHealth> = by_track
        .into_iter()
        .map(|(track, accumulator)| accumulator.finish(track, done_keys))
        .collect();
    // Worst verdict first; within a verdict, the busiest track first, then by
    // name so the order is stable across requests.
    tracks.sort_by(|a, b| {
        a.verdict
            .rank()
            .cmp(&b.verdict.rank())
            .then_with(|| b.missions.cmp(&a.missions))
            .then_with(|| a.track.cmp(&b.track))
    });

    ProjectHealth {
        missions: tracks.iter().map(|t| t.missions).sum(),
        active: tracks.iter().map(|t| t.active).sum(),
        failed: tracks.iter().map(|t| t.failed).sum(),
        overdue: tracks.iter().map(|t| t.overdue).sum(),
        tracks_needing_attention: tracks
            .iter()
            .filter(|t| t.verdict.needs_attention())
            .count(),
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-08-04T12:00:00Z";

    fn mission<'a>(
        status: MissionStatus,
        track: Option<&'a str>,
        desired_state: Option<&'a str>,
        next_check_at: Option<&'a str>,
        updated_at: &'a str,
    ) -> MissionHealthInput<'a> {
        MissionHealthInput {
            track,
            status,
            desired_state,
            next_check_at,
            updated_at,
        }
    }

    fn rollup_of(missions: &[MissionHealthInput<'_>]) -> ProjectHealth {
        rollup(missions, NOW)
    }

    #[test]
    fn a_failure_with_nobody_working_on_it_is_failing() {
        let health = rollup_of(&[mission(
            MissionStatus::Failed,
            Some("core"),
            None,
            None,
            NOW,
        )]);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Failing);
        assert_eq!(health.tracks_needing_attention, 1);
    }

    #[test]
    fn a_failure_with_a_live_retry_is_active_not_failing() {
        // The distinction is the whole point: a track that failed once and is
        // already retrying does not need a human, and reporting it as failing
        // would bury the tracks that do.
        let health = rollup_of(&[
            mission(MissionStatus::Failed, Some("core"), None, None, NOW),
            mission(MissionStatus::Active, Some("core"), None, None, NOW),
        ]);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Active);
        assert_eq!(health.tracks[0].failed, 1);
        assert_eq!(health.tracks_needing_attention, 0);
    }

    #[test]
    fn a_missed_self_declared_deadline_is_overdue() {
        let health = rollup_of(&[mission(
            MissionStatus::AwaitingUser,
            Some("core"),
            Some("waiting_ci"),
            Some("2026-08-04T11:00:00Z"),
            NOW,
        )]);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Overdue);
        assert_eq!(health.tracks[0].overdue, 1);
        assert_eq!(health.tracks[0].desired_states.get("waiting_ci"), Some(&1));
    }

    #[test]
    fn a_deadline_in_the_future_is_not_overdue() {
        let health = rollup_of(&[mission(
            MissionStatus::AwaitingUser,
            Some("core"),
            Some("waiting_ci"),
            Some("2026-08-04T13:00:00Z"),
            NOW,
        )]);
        assert_eq!(health.tracks[0].overdue, 0);
        assert_ne!(health.tracks[0].verdict, TrackVerdict::Overdue);
    }

    #[test]
    fn a_terminal_mission_cannot_be_overdue() {
        // A completed mission's `next_check_at` is a leftover, not a promise.
        let health = rollup_of(&[mission(
            MissionStatus::Completed,
            Some("core"),
            Some("waiting_ci"),
            Some("2026-08-04T11:00:00Z"),
            NOW,
        )]);
        assert_eq!(health.tracks[0].overdue, 0);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Done);
    }

    #[test]
    fn a_mission_parked_for_a_human_is_not_active() {
        // AwaitingUser means the agent's turn ended and nothing will move
        // until someone reads it. Counting it as active is how a stalled
        // track disguises itself as a busy one.
        let health = rollup_of(&[mission(
            MissionStatus::AwaitingUser,
            Some("core"),
            None,
            None,
            NOW,
        )]);
        assert_eq!(health.tracks[0].active, 0);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Idle);
    }

    #[test]
    fn work_actually_in_flight_counts_as_active() {
        for status in [
            MissionStatus::Pending,
            MissionStatus::Active,
            MissionStatus::WaitingBackground,
        ] {
            let health = rollup_of(&[mission(status, Some("core"), None, None, NOW)]);
            assert_eq!(health.tracks[0].active, 1, "{status:?} should be active");
            assert_eq!(health.tracks[0].verdict, TrackVerdict::Active);
        }
    }

    #[test]
    fn paused_missions_are_not_counted_as_active() {
        // Paused is non-terminal but nothing is running, so calling it active
        // would make an intentionally parked track look healthy.
        let health = rollup_of(&[
            mission(MissionStatus::Failed, Some("core"), None, None, NOW),
            mission(MissionStatus::Paused, Some("core"), None, None, NOW),
        ]);
        assert_eq!(health.tracks[0].active, 0);
        assert_eq!(health.tracks[0].verdict, TrackVerdict::Failing);
    }

    #[test]
    fn worst_tracks_lead_and_ties_break_stably() {
        let health = rollup_of(&[
            mission(
                MissionStatus::Completed,
                Some("done-track"),
                None,
                None,
                NOW,
            ),
            mission(MissionStatus::Active, Some("busy"), None, None, NOW),
            mission(MissionStatus::Failed, Some("broken"), None, None, NOW),
            mission(
                MissionStatus::AwaitingUser,
                Some("late"),
                Some("waiting_ci"),
                Some("2026-08-04T01:00:00Z"),
                NOW,
            ),
        ]);
        let order: Vec<_> = health
            .tracks
            .iter()
            .map(|t| t.track.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(order, vec!["broken", "late", "busy", "done-track"]);
    }

    #[test]
    fn missions_without_a_track_get_their_own_bucket() {
        let health = rollup_of(&[
            mission(MissionStatus::Active, None, None, None, NOW),
            mission(MissionStatus::Active, Some("   "), None, None, NOW),
            mission(MissionStatus::Active, Some("core"), None, None, NOW),
        ]);
        assert_eq!(health.tracks.len(), 2);
        // Blank and absent are the same absence, not two buckets.
        let untracked = health.tracks.iter().find(|t| t.track.is_none()).unwrap();
        assert_eq!(untracked.missions, 2);
    }

    #[test]
    fn last_activity_is_the_newest_across_the_track() {
        let health = rollup_of(&[
            mission(
                MissionStatus::Active,
                Some("core"),
                None,
                None,
                "2026-08-01T00:00:00Z",
            ),
            mission(
                MissionStatus::Active,
                Some("core"),
                None,
                None,
                "2026-08-03T00:00:00Z",
            ),
            mission(
                MissionStatus::Active,
                Some("core"),
                None,
                None,
                "2026-08-02T00:00:00Z",
            ),
        ]);
        assert_eq!(
            health.tracks[0].last_activity_at.as_deref(),
            Some("2026-08-03T00:00:00Z")
        );
    }

    #[test]
    fn project_totals_are_the_sum_of_the_tracks() {
        let health = rollup_of(&[
            mission(MissionStatus::Failed, Some("a"), None, None, NOW),
            mission(MissionStatus::Active, Some("b"), None, None, NOW),
            mission(
                MissionStatus::AwaitingUser,
                Some("c"),
                None,
                Some("2026-08-04T01:00:00Z"),
                NOW,
            ),
        ]);
        assert_eq!(health.missions, 3);
        assert_eq!(health.failed, 1);
        assert_eq!(health.active, 1);
        assert_eq!(health.overdue, 1);
        assert_eq!(health.tracks_needing_attention, 2);
    }

    #[test]
    fn an_empty_project_rolls_up_to_nothing_rather_than_panicking() {
        let health = rollup(&[], NOW);
        assert_eq!(health.missions, 0);
        assert!(health.tracks.is_empty());
        assert_eq!(health.tracks_needing_attention, 0);
    }
}
