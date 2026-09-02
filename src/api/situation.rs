//! One bounded situation read per project.
//!
//! Every surface that used to count "N/M done" on its own — `/tasks`,
//! `get_project`, the roster row, the per-track health rollup, the desktop
//! rail — now renders a projection of [`ProjectSituation`]. The builder is a
//! pure function over the item inventory ([`mission_horizon::project_items`])
//! so the same fixture yields byte-identical summaries on every surface.
//!
//! Vocabulary (see `docs/AGENT_CONTROL_PLANE.md`, "Completion and truth"):
//!
//! - `lifecycle`: `active` | `cancelled`. The only stored track state.
//! - `derived_state`: `ready` | `executing` | `waiting` | `blocked` |
//!   `satisfied` | `claim_only` | `cancelled` | `inconsistent`. Computed here,
//!   never written back.
//!
//! Until receipts exist (plan step 2/3), a track whose stored status says
//! `done` / `closed` is a **claim**, not verified satisfaction: it counts under
//! `claim_only`, never under `verified_satisfied`. That is deliberately the
//! honest number, and it is the number the desktop will render.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use serde::Serialize;

use super::mission_horizon::{ProjectItem, ProjectItemAttempt};

/// Stored lifecycle of a track. Everything else is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    Cancelled,
}

/// Where a track came from. `absorbed` rows exist only because a mission was
/// tagged with a key nobody declared; the rail shows them as "unplanned".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Declared,
    Imported,
    Absorbed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DerivedState {
    Ready,
    Executing,
    Waiting,
    Blocked,
    Satisfied,
    ClaimOnly,
    Cancelled,
    Inconsistent,
}

impl DerivedState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Executing => "executing",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Satisfied => "satisfied",
            Self::ClaimOnly => "claim_only",
            Self::Cancelled => "cancelled",
            Self::Inconsistent => "inconsistent",
        }
    }

    /// Whether the track still needs work. Claims are *not* open: the
    /// operator asked for them to be shown as done-but-unproven, not as todo.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Executing | Self::Waiting | Self::Blocked | Self::Inconsistent
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemOwner {
    pub attempt_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemAcceptance {
    pub verified: usize,
    pub total: usize,
    pub claim_only: bool,
}

/// The canonical item shape shared by HTTP and MCP.
///
/// The `kind` / `open` / `status` / `desired_state` / `declared` / `attempts`
/// fields are the pre-existing contract that `compact_item` (assistant-mcp)
/// and the desktop plugin still read. They stay until step 5 of the plan
/// removes the last consumer; nothing new may depend on them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SituationItem {
    /// Stable id. Until `project_tracks_v2` lands this is the key.
    pub id: String,
    pub key: String,
    pub title: String,
    pub lifecycle: Lifecycle,
    pub derived_state: DerivedState,
    pub origin: Origin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<ItemOwner>,
    pub acceptance: ItemAcceptance,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    // ── compatibility projection ────────────────────────────────────────
    pub kind: &'static str,
    pub open: bool,
    pub declared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    pub attempts: Vec<ProjectItemAttempt>,
}

/// The one summary every surface renders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TrackSummary {
    /// Declared inventory, cancelled excluded.
    pub total: usize,
    /// Satisfied with accepted evidence. Zero until receipts exist.
    pub verified_satisfied: usize,
    /// Marked done without evidence (legacy `done` / `closed`).
    pub claim_only: usize,
    pub open: usize,
    pub blocked: usize,
    pub cancelled: usize,
    /// Attempts currently live across the project's tracks.
    pub live_attempts: usize,
    /// True when a source failed to load; counts above are then partial and
    /// must not be rendered as "zero items".
    pub source_unavailable: bool,
    pub as_of: String,
    /// Changes whenever any item's state or latest attempt changes. A
    /// controller that sees the same cursor twice has nothing new to reason
    /// about.
    pub cursor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectSituation {
    pub slug: String,
    pub summary: TrackSummary,
    pub items: Vec<SituationItem>,
}

/// What the builder could not read. Recorded on the summary rather than
/// turned into an empty list.
#[derive(Debug, Clone, Default)]
pub struct SourceStatus {
    pub tracks_failed: bool,
    pub missions_failed: bool,
}

impl SourceStatus {
    fn unavailable(&self) -> bool {
        self.tracks_failed || self.missions_failed
    }
}

fn attempt_is_live(status: &str) -> bool {
    matches!(
        status,
        "active" | "pending" | "running" | "starting" | "queued" | "waiting_background"
    )
}

fn attempt_is_waiting(status: &str) -> bool {
    matches!(status, "awaiting_user" | "paused" | "acknowledged")
}

fn attempt_failed(status: &str) -> bool {
    matches!(
        status,
        "failed" | "interrupted" | "blocked" | "not_feasible"
    )
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Turn a raw key like `ux1-pr229-cert` into `UX1 PR229 cert`. Only used
/// when a track carries no title at all; the rail must never show the key.
pub fn humanize_key(key: &str) -> String {
    key.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let has_digit = part.chars().any(|c| c.is_ascii_digit());
            // Short codes (`pr`, `ux1`, `eip`, `s3`) read as acronyms.
            if (has_digit && part.len() <= 6) || part.len() <= 3 {
                part.to_ascii_uppercase()
            } else {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Title precedence: declared title, desired state, newest attempt title,
/// humanized key. Same order `/tasks` always used, now in one place.
pub fn item_display_title(item: &ProjectItem) -> String {
    nonempty(item.title.as_deref())
        .map(str::to_string)
        .or_else(|| nonempty(item.desired_state.as_deref()).map(str::to_string))
        .or_else(|| {
            item.attempts
                .iter()
                .find_map(|attempt| nonempty(attempt.title.as_deref()).map(str::to_string))
        })
        .unwrap_or_else(|| humanize_key(&item.key))
}

fn lifecycle_of(item: &ProjectItem) -> Lifecycle {
    match nonempty(item.status.as_deref()) {
        Some("cancelled") => Lifecycle::Cancelled,
        _ => Lifecycle::Active,
    }
}

fn is_legacy_claim(item: &ProjectItem) -> bool {
    matches!(
        nonempty(item.status.as_deref()),
        Some("done") | Some("closed")
    )
}

fn origin_of(item: &ProjectItem) -> Origin {
    match item.origin.as_deref() {
        Some("imported") => Origin::Imported,
        Some("absorbed") => Origin::Absorbed,
        Some(_) => Origin::Declared,
        None if item.declared => Origin::Declared,
        None => Origin::Absorbed,
    }
}

fn is_verified(item: &ProjectItem) -> bool {
    matches!(nonempty(item.status.as_deref()), Some("satisfied"))
}

fn owner_of(item: &ProjectItem) -> Option<ItemOwner> {
    item.attempts
        .iter()
        .find(|attempt| attempt_is_live(&attempt.status) || attempt_is_waiting(&attempt.status))
        .map(|attempt| ItemOwner {
            attempt_id: attempt.id.to_string(),
            status: attempt.status.clone(),
            lease_until: None,
        })
}

/// Derive one state from lifecycle, claim, live attempts, and dependencies.
/// `open_keys` is the set of keys in this project that are still open, used
/// to resolve `depends_on`.
fn derive_state(item: &ProjectItem, open_keys: &BTreeSet<String>) -> (DerivedState, Vec<String>) {
    if lifecycle_of(item) == Lifecycle::Cancelled {
        return (DerivedState::Cancelled, Vec::new());
    }
    let live = item
        .attempts
        .iter()
        .any(|attempt| attempt_is_live(&attempt.status));
    if live {
        return (DerivedState::Executing, Vec::new());
    }
    if is_verified(item) {
        return (DerivedState::Satisfied, Vec::new());
    }
    if is_legacy_claim(item) {
        return (DerivedState::ClaimOnly, Vec::new());
    }
    let blocked_by: Vec<String> = item
        .depends_on
        .iter()
        .filter(|dep| open_keys.contains(dep.as_str()))
        .cloned()
        .collect();
    if !blocked_by.is_empty() || item.blocker.is_some() {
        return (DerivedState::Blocked, blocked_by);
    }
    let waiting = item
        .attempts
        .first()
        .map(|attempt| attempt_is_waiting(&attempt.status))
        .unwrap_or(false)
        || nonempty(item.desired_state.as_deref())
            .map(|state| state.starts_with("waiting"))
            .unwrap_or(false);
    if waiting {
        return (DerivedState::Waiting, Vec::new());
    }
    (DerivedState::Ready, Vec::new())
}

fn cursor_for(items: &[SituationItem], source: &SourceStatus) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in items {
        item.key.hash(&mut hasher);
        item.derived_state.hash(&mut hasher);
        item.updated_at.hash(&mut hasher);
        if let Some(owner) = &item.owner {
            owner.attempt_id.hash(&mut hasher);
            owner.status.hash(&mut hasher);
        }
    }
    source.unavailable().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Build the situation from the item inventory. Pure: no I/O, no clock other
/// than the `as_of` the caller passes so one response carries one instant.
pub fn build(
    slug: &str,
    items: &[ProjectItem],
    source: &SourceStatus,
    as_of: &str,
) -> ProjectSituation {
    // First pass: which keys are still open, so dependencies can resolve.
    let mut open_keys: BTreeSet<String> = BTreeSet::new();
    for item in items {
        let live = item
            .attempts
            .iter()
            .any(|attempt| attempt_is_live(&attempt.status));
        if lifecycle_of(item) == Lifecycle::Active
            && (live || !(is_legacy_claim(item) || is_verified(item)))
        {
            open_keys.insert(item.key.clone());
        }
    }

    let mut out: Vec<SituationItem> = Vec::with_capacity(items.len());
    for item in items {
        let (derived_state, blocked_by) = derive_state(item, &open_keys);
        let claim = derived_state == DerivedState::ClaimOnly;
        let latest = item.attempts.first();
        let updated_at = latest.map(|attempt| attempt.updated_at.clone());
        out.push(SituationItem {
            id: item.id.clone().unwrap_or_else(|| item.key.clone()),
            key: item.key.clone(),
            title: item_display_title(item),
            lifecycle: lifecycle_of(item),
            derived_state,
            origin: origin_of(item),
            owner: owner_of(item),
            acceptance: ItemAcceptance {
                verified: if derived_state == DerivedState::Satisfied {
                    item.acceptance_criteria.len().max(1)
                } else {
                    0
                },
                total: item.acceptance_criteria.len(),
                claim_only: claim,
            },
            depends_on: item.depends_on.clone(),
            blocked_by,
            blocker: item.blocker.clone(),
            revision: item.revision,
            updated_at,
            kind: item.kind,
            open: derived_state.is_open(),
            declared: item.declared,
            status: item.status.clone(),
            desired_state: item.desired_state.clone(),
            acceptance_criteria: item.acceptance_criteria.clone(),
            attempts: item.attempts.clone(),
        });
    }

    let mut summary = TrackSummary {
        as_of: as_of.to_string(),
        source_unavailable: source.unavailable(),
        ..TrackSummary::default()
    };
    for item in &out {
        // Mission-only rows (nobody declared the key) are inventory only
        // while something is live on them — same rule as the checklist.
        if !item.declared && item.derived_state != DerivedState::Executing {
            continue;
        }
        match item.derived_state {
            DerivedState::Cancelled => {
                summary.cancelled += 1;
                continue;
            }
            DerivedState::Satisfied => summary.verified_satisfied += 1,
            DerivedState::ClaimOnly => summary.claim_only += 1,
            DerivedState::Blocked => {
                summary.blocked += 1;
                summary.open += 1;
            }
            DerivedState::Ready
            | DerivedState::Executing
            | DerivedState::Waiting
            | DerivedState::Inconsistent => summary.open += 1,
        }
        summary.total += 1;
        summary.live_attempts += item
            .attempts
            .iter()
            .filter(|attempt| attempt_is_live(&attempt.status))
            .count();
    }
    summary.cursor = cursor_for(&out, source);

    ProjectSituation {
        slug: slug.to_string(),
        summary,
        items: out,
    }
}

/// Overlay live leases onto the items: the owner is whoever holds the writer
/// lease (with its expiry), falling back to the live attempt when no lease
/// exists yet (compatibility window).
pub fn apply_leases(
    situation: &mut ProjectSituation,
    leases: &[super::projects_store::TrackLease],
) {
    for item in &mut situation.items {
        let writer = leases
            .iter()
            .find(|lease| lease.track == item.key && lease.mode == "writer");
        if let Some(lease) = writer {
            let status = item
                .attempts
                .iter()
                .find(|attempt| attempt.id.to_string() == lease.attempt_id)
                .map(|attempt| attempt.status.clone())
                .unwrap_or_else(|| "leased".to_string());
            item.owner = Some(ItemOwner {
                attempt_id: lease.attempt_id.clone(),
                status,
                lease_until: Some(lease.lease_until.clone()),
            });
        } else if let Some(owner) = &mut item.owner {
            if let Some(lease) = leases
                .iter()
                .find(|lease| lease.track == item.key && lease.attempt_id == owner.attempt_id)
            {
                owner.lease_until = Some(lease.lease_until.clone());
            }
        }
    }
}

/// The `/tasks` checklist vocabulary (accepted / running / failed / proposed /
/// pending), derived from the canonical state so the compatibility endpoint
/// and the canonical one can never disagree.
pub fn roadmap_status(item: &SituationItem) -> &'static str {
    match item.derived_state {
        DerivedState::Satisfied | DerivedState::ClaimOnly => "accepted",
        DerivedState::Executing => "running",
        DerivedState::Cancelled => "cancelled",
        _ => {
            if item.status.as_deref() == Some("proposed") {
                return "proposed";
            }
            if !item.attempts.is_empty()
                && item
                    .attempts
                    .iter()
                    .all(|attempt| attempt_failed(&attempt.status))
            {
                return "failed";
            }
            "pending"
        }
    }
}

/// Which items the public checklist shows. Mission-only rows appear only
/// while they have a live attempt; cancelled rows never appear.
pub fn belongs_on_roadmap(item: &SituationItem) -> bool {
    if item.derived_state == DerivedState::Cancelled {
        return false;
    }
    if item.derived_state == DerivedState::Executing {
        return true;
    }
    if item.kind == "task" {
        return item.open;
    }
    item.declared
}

/// Keys whose derived state is satisfied or claim-only, for readers that
/// still speak the old "done" vocabulary (health rollup, `/tasks` summary).
pub fn done_keys(situation: &ProjectSituation) -> BTreeSet<String> {
    situation
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.derived_state,
                DerivedState::Satisfied | DerivedState::ClaimOnly
            )
        })
        .map(|item| item.key.clone())
        .collect()
}

/// Group missions by canonical project key so the overview can build one
/// situation per row from a single mission scan.
pub fn group_by<T, F>(rows: &[T], key: F) -> BTreeMap<String, Vec<&T>>
where
    F: Fn(&T) -> Option<String>,
{
    let mut groups: BTreeMap<String, Vec<&T>> = BTreeMap::new();
    for row in rows {
        if let Some(key) = key(row) {
            groups.entry(key).or_default().push(row);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn item(key: &str, status: Option<&str>, declared: bool) -> ProjectItem {
        ProjectItem {
            key: key.into(),
            kind: "track",
            desired_state: None,
            status: status.map(str::to_string),
            title: None,
            acceptance_criteria: Vec::new(),
            depends_on: Vec::new(),
            open: super::super::mission_horizon::track_status_is_open(status),
            declared,
            attempts: Vec::new(),
            id: None,
            origin: None,
            revision: 0,
            blocker: None,
        }
    }

    fn attempt(status: &str, at: &str) -> ProjectItemAttempt {
        ProjectItemAttempt {
            id: Uuid::new_v4(),
            status: status.into(),
            title: Some("attempt".into()),
            updated_at: at.into(),
            role: None,
        }
    }

    #[test]
    fn legacy_done_is_a_claim_not_verified() {
        let items = vec![item("a1", Some("done"), true), item("a2", None, true)];
        let situation = build("p", &items, &SourceStatus::default(), "now");
        assert_eq!(situation.summary.total, 2);
        assert_eq!(situation.summary.verified_satisfied, 0);
        assert_eq!(situation.summary.claim_only, 1);
        assert_eq!(situation.summary.open, 1);
        assert_eq!(situation.items[0].derived_state, DerivedState::ClaimOnly);
        assert!(!situation.items[0].open);
        assert_eq!(roadmap_status(&situation.items[0]), "accepted");
    }

    #[test]
    fn cancelled_leaves_the_total() {
        let items = vec![item("a1", Some("cancelled"), true), item("a2", None, true)];
        let situation = build("p", &items, &SourceStatus::default(), "now");
        assert_eq!(situation.summary.total, 1);
        assert_eq!(situation.summary.cancelled, 1);
        assert!(!belongs_on_roadmap(&situation.items[0]));
    }

    #[test]
    fn live_attempt_means_executing_and_owned() {
        let mut open = item("ux1", None, true);
        open.attempts
            .push(attempt("active", "2026-09-01T00:00:00Z"));
        let situation = build("p", &[open], &SourceStatus::default(), "now");
        let it = &situation.items[0];
        assert_eq!(it.derived_state, DerivedState::Executing);
        assert!(it.owner.is_some());
        assert_eq!(situation.summary.live_attempts, 1);
        assert_eq!(roadmap_status(it), "running");
    }

    #[test]
    fn open_dependency_blocks() {
        let mut child = item("s2", None, true);
        child.depends_on.push("s1".into());
        let items = vec![item("s1", None, true), child];
        let situation = build("p", &items, &SourceStatus::default(), "now");
        let s2 = situation.items.iter().find(|i| i.key == "s2").unwrap();
        assert_eq!(s2.derived_state, DerivedState::Blocked);
        assert_eq!(s2.blocked_by, vec!["s1".to_string()]);
        assert_eq!(situation.summary.blocked, 1);
        assert_eq!(situation.summary.open, 2);

        let items = vec![item("s1", Some("done"), true), {
            let mut c = item("s2", None, true);
            c.depends_on.push("s1".into());
            c
        }];
        let situation = build("p", &items, &SourceStatus::default(), "now");
        let s2 = situation.items.iter().find(|i| i.key == "s2").unwrap();
        assert_eq!(s2.derived_state, DerivedState::Ready);
    }

    #[test]
    fn absorbed_rows_carry_origin_and_never_a_raw_key_title() {
        let mut absorbed = item("pr-233-repair", None, false);
        absorbed
            .attempts
            .push(attempt("active", "2026-09-01T00:00:00Z"));
        absorbed.attempts[0].title = None;
        let situation = build("p", &[absorbed], &SourceStatus::default(), "now");
        let it = &situation.items[0];
        assert_eq!(it.origin, Origin::Absorbed);
        assert_eq!(it.title, "PR 233 Repair");
        assert!(belongs_on_roadmap(it));
    }

    #[test]
    fn mission_only_rows_leave_the_roadmap_when_nothing_is_live() {
        let mut absorbed = item("zombie", None, false);
        absorbed
            .attempts
            .push(attempt("failed", "2026-08-01T00:00:00Z"));
        let situation = build("p", &[absorbed], &SourceStatus::default(), "now");
        assert!(!belongs_on_roadmap(&situation.items[0]));
        assert_eq!(roadmap_status(&situation.items[0]), "failed");
    }

    #[test]
    fn satisfied_status_is_verified_and_explicit_blocker_blocks() {
        let mut verified = item("v1", Some("satisfied"), true);
        verified.acceptance_criteria.push("merged".into());
        let mut blocked = item("b1", None, true);
        blocked.blocker = Some("waiting on Lido".into());
        let situation = build("p", &[verified, blocked], &SourceStatus::default(), "now");
        assert_eq!(situation.summary.verified_satisfied, 1);
        assert_eq!(situation.summary.claim_only, 0);
        assert_eq!(situation.summary.blocked, 1);
        let v1 = situation.items.iter().find(|i| i.key == "v1").unwrap();
        assert_eq!(v1.derived_state, DerivedState::Satisfied);
        assert_eq!(v1.acceptance.verified, 1);
        let b1 = situation.items.iter().find(|i| i.key == "b1").unwrap();
        assert_eq!(b1.derived_state, DerivedState::Blocked);
        assert_eq!(b1.blocker.as_deref(), Some("waiting on Lido"));
    }

    #[test]
    fn source_failure_is_not_zero() {
        let source = SourceStatus {
            tracks_failed: true,
            missions_failed: false,
        };
        let situation = build("p", &[], &source, "now");
        assert!(situation.summary.source_unavailable);
        assert_eq!(situation.summary.total, 0);
    }

    #[test]
    fn cursor_moves_with_state_and_stays_otherwise() {
        let items = vec![item("a1", None, true)];
        let a = build("p", &items, &SourceStatus::default(), "t1");
        let b = build("p", &items, &SourceStatus::default(), "t2");
        assert_eq!(
            a.summary.cursor, b.summary.cursor,
            "as_of alone must not move the cursor"
        );
        let items = vec![item("a1", Some("done"), true)];
        let c = build("p", &items, &SourceStatus::default(), "t3");
        assert_ne!(a.summary.cursor, c.summary.cursor);
    }

    #[test]
    fn humanize_keeps_short_codes_upper() {
        assert_eq!(humanize_key("ux1-pr229-cert"), "UX1 PR229 Cert");
        assert_eq!(humanize_key("repair-pr-233"), "Repair PR 233");
    }
}
