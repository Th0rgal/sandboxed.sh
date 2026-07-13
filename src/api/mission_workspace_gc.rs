//! Background GC for stale mission workspace directories.
//!
//! When `auto_cleanup_enabled` is on in `SettingsStore`, this task wakes up
//! once an hour, walks every live control session's mission store, and for
//! each mission in a terminal status that hasn't been touched within the
//! configured retention window (`auto_cleanup_days`), it deletes the
//! per-mission workspace directory on disk
//! (`{workspace_root}/workspaces/mission-{first-8-of-id}/`).
//!
//! The conversation history in the SQLite mission store is left intact —
//! only the agent's sandboxed filesystem is collected. The mission can still
//! be opened from the dashboard; "Load earlier messages" continues to work.
//!
//! Terminal statuses we collect:
//!     Completed, Acknowledged, Failed, Interrupted, Blocked, NotFeasible
//!
//! We deliberately do NOT collect `AwaitingUser` (still expecting the user
//! to come back and reply) or anything currently running.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::control::MissionStatus;
use super::routes::AppState;
use crate::workspace;

/// How often the GC wakes up to scan for collectible workspaces.
const TICK_INTERVAL: Duration = Duration::from_secs(60 * 60); // 1 hour

/// Default retention when no value is configured in settings.
pub const DEFAULT_RETENTION_DAYS: u32 = 7;

/// Default long-stop retention for AwaitingUser/Paused mission dirs.
pub const DEFAULT_STOPPED_RETENTION_DAYS: u32 = 30;

/// Page size for `list_missions` pagination — keeps the scan bounded in
/// memory even when a session has thousands of missions.
const LIST_PAGE_SIZE: usize = 200;

/// Spawn the background GC loop. Safe to call once at server start.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        run_loop(state).await;
    });
}

async fn run_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    // First tick fires immediately; skip it so we don't run on the same
    // hot-path tick that's still booting telegram/openroute/etc.
    interval.tick().await;
    loop {
        interval.tick().await;
        let started = std::time::Instant::now();
        let settings = read_settings(&state).await;
        if !settings.enabled {
            tracing::trace!("mission workspace GC disabled");
            continue;
        }
        let now = Utc::now();
        let params = SweepParams {
            cutoff: now - chrono::Duration::days(settings.days as i64),
            stopped_cutoff: now - chrono::Duration::days(settings.stopped_days as i64),
            orphans_enabled: settings.orphans_enabled,
        };
        let report = run_once(&state, &params).await;
        tracing::info!(
            removed = report.removed,
            orphans_removed = report.orphans_removed,
            stopped_removed = report.stopped_removed,
            errors = report.errors,
            scanned = report.scanned,
            bytes_freed = report.bytes_freed,
            duration_ms = started.elapsed().as_millis() as u64,
            retention_days = settings.days,
            stopped_retention_days = settings.stopped_days,
            "mission workspace GC sweep finished",
        );
    }
}

struct GcSettings {
    enabled: bool,
    days: u32,
    stopped_days: u32,
    orphans_enabled: bool,
}

async fn read_settings(state: &Arc<AppState>) -> GcSettings {
    let snapshot = state.settings.get().await;
    let enabled = snapshot.auto_cleanup_enabled.unwrap_or(false);
    let days = snapshot
        .auto_cleanup_days
        .filter(|d| *d >= 1)
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    let stopped_days = snapshot
        .auto_cleanup_stopped_days
        .filter(|d| *d >= 1)
        .unwrap_or(DEFAULT_STOPPED_RETENTION_DAYS);
    let orphans_enabled = snapshot.auto_cleanup_orphans_enabled.unwrap_or(enabled);
    GcSettings {
        enabled,
        days,
        stopped_days,
        orphans_enabled,
    }
}

/// One mission's GC/reaper-relevant metadata, keyed by 8-hex short id.
#[derive(Debug, Clone)]
pub struct MissionIndexEntry {
    pub status: MissionStatus,
    pub updated_at: DateTime<Utc>,
    pub workspace_id: uuid::Uuid,
}

/// How protected a status is, for short-id collision resolution: running >
/// parked > terminal. The more protective entry wins so a collision can
/// never cause a live mission's dir/scope to be collected.
fn protection_rank(status: &MissionStatus) -> u8 {
    match status {
        MissionStatus::Active | MissionStatus::Pending | MissionStatus::WaitingBackground => 2,
        MissionStatus::AwaitingUser | MissionStatus::Paused => 1,
        _ => 0,
    }
}

/// Cross-store mission index keyed by 8-hex short id.
///
/// `complete` is the load-bearing bit: "no index hit" is only usable as
/// evidence that a dir/scope is orphaned when every persisted store on disk
/// was actually indexed. Stores belonging to users with no live session are
/// read offline (read-only SQLite); if any store can't be read, `complete`
/// is false and callers must skip their unknown-entry deletion branches.
pub(crate) struct MissionIndex {
    pub by_short: std::collections::HashMap<String, Vec<MissionIndexEntry>>,
    pub complete: bool,
}

/// Select the most protective mission that owns the directory/scope's actual
/// workspace. Short ids are only eight hex characters, so entries from other
/// workspaces must never influence collection on a collision.
pub(crate) fn entry_for_workspace(
    entries: &[MissionIndexEntry],
    workspace_id: uuid::Uuid,
) -> Option<&MissionIndexEntry> {
    entries
        .iter()
        .filter(|entry| entry.workspace_id == workspace_id)
        .max_by_key(|entry| {
            (
                protection_rank(&entry.status),
                entry.updated_at.timestamp_micros(),
            )
        })
}

/// Read one persisted SQLite mission store without booting a session.
fn index_store_file_blocking(
    path: &std::path::Path,
) -> Result<Vec<(String, MissionIndexEntry)>, String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT id, status, updated_at, workspace_id FROM missions")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (id, status, updated_at, workspace_id) = row.map_err(|e| e.to_string())?;
        if id.len() < 8 {
            continue;
        }
        // Unknown status strings deserialize to the most protective value.
        let status: MissionStatus = serde_json::from_value(serde_json::Value::String(status))
            .unwrap_or(MissionStatus::Active);
        let updated_at = DateTime::parse_from_rfc3339(&updated_at)
            .map(|ts| ts.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let workspace_id = uuid::Uuid::parse_str(&workspace_id).unwrap_or(uuid::Uuid::nil());
        out.push((
            id[..8].to_ascii_lowercase(),
            MissionIndexEntry {
                status,
                updated_at,
                workspace_id,
            },
        ));
    }
    Ok(out)
}

/// Resolve one exact mission from persisted stores without requiring its user
/// session to have booted in this process. Remote-build admission uses this
/// after restart so a still-running OAuth user's workspace is not rejected.
pub(crate) async fn persisted_mission_status(
    state: &AppState,
    mission_id: uuid::Uuid,
) -> Result<Option<MissionStatus>, String> {
    let missions_dir = state
        .config
        .working_dir
        .join(".sandboxed-sh")
        .join("missions");
    tokio::task::spawn_blocking(move || {
        let entries = match std::fs::read_dir(&missions_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("read {}: {err}", missions_dir.display())),
        };
        let id = mission_id.to_string();
        for entry in entries {
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("db") => {
                    let conn = rusqlite::Connection::open_with_flags(
                        &path,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .map_err(|err| format!("open {}: {err}", path.display()))?;
                    let mut stmt = conn
                        .prepare("SELECT status FROM missions WHERE id = ?1 LIMIT 1")
                        .map_err(|err| format!("query {}: {err}", path.display()))?;
                    let mut rows = stmt
                        .query([id.as_str()])
                        .map_err(|err| format!("query {}: {err}", path.display()))?;
                    if let Some(row) = rows.next().map_err(|err| err.to_string())? {
                        let raw: String = row.get(0).map_err(|err| err.to_string())?;
                        let status = serde_json::from_value(serde_json::Value::String(raw))
                            .map_err(|err| err.to_string())?;
                        return Ok(Some(status));
                    }
                }
                Some("json") => {
                    let bytes = std::fs::read(&path)
                        .map_err(|err| format!("read {}: {err}", path.display()))?;
                    let snapshot: serde_json::Value = serde_json::from_slice(&bytes)
                        .map_err(|err| format!("parse {}: {err}", path.display()))?;
                    if let Some(raw) = snapshot
                        .get("missions")
                        .and_then(|missions| missions.get(&id))
                        .and_then(|mission| mission.get("status"))
                    {
                        let status =
                            serde_json::from_value(raw.clone()).map_err(|err| err.to_string())?;
                        return Ok(Some(status));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Index every mission across all persisted stores by the first 8 hex
/// chars of its id — the same prefix embedded in workspace dir names
/// (`mission-<8hex>`) and exec scope units (`-m<8hex>-`). Shared by the
/// orphan-dir sweep and the scope reaper.
pub(crate) async fn build_mission_index(state: &Arc<AppState>) -> MissionIndex {
    let mut index: std::collections::HashMap<String, Vec<MissionIndexEntry>> =
        std::collections::HashMap::new();
    let mut complete = true;
    let sessions = state.control.all_sessions().await;
    for session in sessions {
        let store = session.mission_store.clone();
        let mut offset = 0usize;
        loop {
            let page = match store.list_missions(LIST_PAGE_SIZE, offset).await {
                Ok(page) => page,
                Err(err) => {
                    // Fail closed: a store we couldn't fully page may hold
                    // missions whose dirs/scopes must not be treated as
                    // unknown by the deletion branches.
                    tracing::warn!(
                        ?err,
                        "mission index: list_missions failed; index marked incomplete"
                    );
                    complete = false;
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            for mission in page {
                let short = mission.id.to_string()[..8].to_string();
                let updated_at = DateTime::parse_from_rfc3339(&mission.updated_at)
                    .map(|ts| ts.with_timezone(&Utc))
                    // Unparseable timestamp → treat as "just now" so the
                    // entry is maximally protective rather than collectable.
                    .unwrap_or_else(|_| Utc::now());
                let entry = MissionIndexEntry {
                    status: mission.status,
                    updated_at,
                    workspace_id: mission.workspace_id,
                };
                index.entry(short).or_default().push(entry);
            }
            if page_len < LIST_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
    }

    // Offline pass: persisted stores whose user has no live session (e.g.
    // OAuth users who haven't connected since restart). Their missions must
    // still protect their dirs/scopes.
    let live_users: std::collections::HashSet<String> = state
        .control
        .session_user_ids()
        .await
        .iter()
        .map(|u| super::mission_store::sanitize_filename(u))
        .collect();
    let missions_dir = state
        .config
        .working_dir
        .join(".sandboxed-sh")
        .join("missions");
    match tokio::fs::read_dir(&missions_dir).await {
        Ok(mut rd) => loop {
            let entry = match rd.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(
                        dir = %missions_dir.display(),
                        ?err,
                        "mission index: directory iteration failed; index marked incomplete"
                    );
                    complete = false;
                    break;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_prefix("missions-") else {
                continue;
            };
            if let Some(user) = stem.strip_suffix(".db") {
                if live_users.contains(user) {
                    continue;
                }
                let path = entry.path();
                let rows = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || index_store_file_blocking(&path)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                match rows {
                    Ok(rows) => {
                        for (short, entry) in rows {
                            index.entry(short).or_default().push(entry);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            store = %path.display(),
                            err,
                            "mission index: offline store read failed; index marked incomplete"
                        );
                        complete = false;
                    }
                }
            } else if stem.ends_with(".json") {
                // File-store format: not offline-indexed; play safe.
                if !live_users.contains(stem.trim_end_matches(".json")) {
                    tracing::warn!(
                        store = name,
                        "mission index: uncovered file-store; index marked incomplete"
                    );
                    complete = false;
                }
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(
                dir = %missions_dir.display(),
                ?err,
                "mission index: persisted-store directory unreadable; index marked incomplete"
            );
            complete = false;
        }
    }

    MissionIndex {
        by_short: index,
        complete,
    }
}

#[derive(Default)]
pub struct SweepReport {
    pub scanned: usize,
    pub removed: usize,
    /// Dirs matching no mission in any store (hard-deleted / legacy DBs).
    pub orphans_removed: usize,
    /// AwaitingUser/Paused dirs past the long-stop retention.
    pub stopped_removed: usize,
    pub errors: usize,
    pub bytes_freed: u64,
}

/// Cutoffs and toggles for one sweep.
pub struct SweepParams {
    /// Terminal missions older than this are collected.
    pub cutoff: DateTime<Utc>,
    /// AwaitingUser/Paused missions older than this are collected.
    pub stopped_cutoff: DateTime<Utc>,
    /// Whether unmatched `mission-*` dirs are collected.
    pub orphans_enabled: bool,
}

/// One full pass. Phase 1 is DB-driven (mission → its recorded workspace →
/// dir). Phase 2 is disk-driven (every `mission-*` dir under every known
/// workspace root, reconciled against the mission index) — it catches what
/// phase 1 structurally cannot: dirs of hard-deleted missions, dirs under a
/// different-but-existing workspace than the mission's recorded one, and
/// long-stopped AwaitingUser/Paused missions.
pub async fn run_once(state: &Arc<AppState>, params: &SweepParams) -> SweepReport {
    let cutoff = params.cutoff;
    let mut report = SweepReport::default();
    let sessions = state.control.all_sessions().await;
    for session in sessions {
        let store = session.mission_store.clone();
        let mut offset = 0usize;
        loop {
            let page = match store.list_missions(LIST_PAGE_SIZE, offset).await {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!(?err, "mission GC: list_missions failed; skipping session");
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            for mission in page {
                report.scanned += 1;
                if !is_gc_eligible_status(&mission.status) {
                    continue;
                }
                let updated_at = match DateTime::parse_from_rfc3339(&mission.updated_at) {
                    Ok(ts) => ts.with_timezone(&Utc),
                    Err(_) => continue,
                };
                if updated_at >= cutoff {
                    continue;
                }
                let workspace_id = mission.workspace_id;
                let ws = match state.workspaces.get(workspace_id).await {
                    Some(ws) => ws,
                    None => {
                        // The orphan sweep (phase 2) is what actually
                        // reclaims these; the log is forensics.
                        tracing::debug!(
                            mission_id = %mission.id,
                            workspace_id = %workspace_id,
                            "mission GC: workspace no longer exists; dir left to orphan sweep",
                        );
                        continue;
                    }
                };
                let dir = workspace::mission_workspace_dir_for_root(&ws.path, mission.id);
                if !dir.exists() {
                    continue;
                }
                let size = directory_size_bytes(&dir).await;
                match tokio::fs::remove_dir_all(&dir).await {
                    Ok(()) => {
                        report.removed += 1;
                        report.bytes_freed = report.bytes_freed.saturating_add(size);
                        tracing::info!(
                            mission_id = %mission.id,
                            workspace_id = %workspace_id,
                            path = %dir.display(),
                            bytes = size,
                            "mission GC: removed workspace directory",
                        );
                    }
                    Err(err) => {
                        report.errors += 1;
                        tracing::warn!(
                            mission_id = %mission.id,
                            path = %dir.display(),
                            ?err,
                            "mission GC: failed to remove workspace directory",
                        );
                    }
                }
            }
            if page_len < LIST_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
    }

    orphan_sweep(state, params, &mut report).await;

    report
}

/// Disk-driven reconciliation pass (phase 2). Every decision is logged with
/// its reason; deletion requires positive evidence of collectability, and a
/// live exec scope always wins.
async fn orphan_sweep(state: &Arc<AppState>, params: &SweepParams, report: &mut SweepReport) {
    let index_full = build_mission_index(state).await;
    let index = &index_full.by_short;
    // Any dir whose short id is referenced by a live exec scope is kept
    // unconditionally: a process may hold cwd/fds there.
    let scope_protected: std::collections::HashSet<(String, String)> =
        super::scope_reaper::list_exec_scope_units()
            .await
            .iter()
            .filter_map(|unit| {
                Some((
                    crate::workspace_exec::machine_name_from_exec_unit(unit)?,
                    crate::workspace_exec::mission_short_id_from_exec_unit(unit)?,
                ))
            })
            .collect();

    for ws in state.workspaces.list().await {
        let root = workspace::workspaces_root_for(&ws.path);
        let Ok(mut rd) = tokio::fs::read_dir(&root).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(short) = name.strip_prefix("mission-") else {
                continue;
            };
            if short.len() != 8 || !short.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let dir = entry.path();
            if !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let workspace_token = crate::workspace_exec::machine_name_for_path(&ws.path);
            if workspace_token
                .as_ref()
                .is_some_and(|token| scope_protected.contains(&(token.clone(), short.to_string())))
            {
                tracing::debug!(path = %dir.display(), "mission GC: kept (live exec scope)");
                continue;
            }
            enum Verdict {
                Keep(&'static str),
                Delete(&'static str, bool /*orphan*/, bool /*stopped*/),
            }
            let indexed_entry = index
                .get(short)
                .and_then(|entries| entry_for_workspace(entries, ws.id));
            let verdict = match indexed_entry {
                Some(e) => match e.status {
                    MissionStatus::Active
                    | MissionStatus::Pending
                    | MissionStatus::WaitingBackground => Verdict::Keep("mission running"),
                    MissionStatus::AwaitingUser | MissionStatus::Paused => {
                        if e.updated_at < params.stopped_cutoff {
                            Verdict::Delete("stopped mission past long-stop retention", false, true)
                        } else {
                            Verdict::Keep("awaiting user / paused within retention")
                        }
                    }
                    _ => {
                        if e.updated_at < params.cutoff {
                            Verdict::Delete("terminal mission past retention", false, false)
                        } else {
                            Verdict::Keep("terminal mission within retention")
                        }
                    }
                },
                None => {
                    if !params.orphans_enabled {
                        Verdict::Keep("orphan collection disabled")
                    } else if !index_full.complete {
                        Verdict::Keep("mission index incomplete; not trusting orphan verdicts")
                    } else {
                        // No mission anywhere claims this dir. Use the dir
                        // mtime as the age signal, with the normal retention
                        // as a grace period for freshly-created dirs whose
                        // mission row hasn't landed yet.
                        let old_enough = match tokio::fs::metadata(&dir).await {
                            Ok(meta) => match meta.modified() {
                                Ok(mtime) => chrono::DateTime::<Utc>::from(mtime) < params.cutoff,
                                Err(_) => false,
                            },
                            Err(_) => false,
                        };
                        if old_enough {
                            Verdict::Delete("no mission in any store", true, false)
                        } else {
                            Verdict::Keep("unmatched but too recent")
                        }
                    }
                }
            };
            match verdict {
                Verdict::Keep(reason) => {
                    tracing::debug!(path = %dir.display(), reason, "mission GC: kept");
                }
                Verdict::Delete(reason, orphan, stopped) => {
                    let size = directory_size_bytes(&dir).await;
                    match tokio::fs::remove_dir_all(&dir).await {
                        Ok(()) => {
                            report.removed += 1;
                            if orphan {
                                report.orphans_removed += 1;
                            }
                            if stopped {
                                report.stopped_removed += 1;
                            }
                            report.bytes_freed = report.bytes_freed.saturating_add(size);
                            tracing::info!(
                                path = %dir.display(),
                                workspace = %ws.name,
                                bytes = size,
                                reason,
                                "mission GC: removed workspace directory (orphan sweep)",
                            );
                        }
                        Err(err) => {
                            report.errors += 1;
                            tracing::warn!(
                                path = %dir.display(),
                                ?err,
                                "mission GC: failed to remove directory (orphan sweep)",
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Strict terminal-status filter — narrower than
/// `is_terminal_mission_status` because `AwaitingUser` should keep its
/// workspace dir alive (user may still come back to reply).
fn is_gc_eligible_status(status: &MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Completed
            | MissionStatus::Acknowledged
            | MissionStatus::Failed
            | MissionStatus::Interrupted
            | MissionStatus::Blocked
            | MissionStatus::NotFeasible
    )
}

/// Best-effort recursive size for telemetry. A failure here doesn't block
/// deletion — we just log 0 bytes freed for that entry.
async fn directory_size_bytes(path: &std::path::Path) -> u64 {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        fn walk(p: &std::path::Path) -> u64 {
            let mut total = 0u64;
            let entries = match std::fs::read_dir(p) {
                Ok(e) => e,
                Err(_) => return 0,
            };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    total = total.saturating_add(walk(&entry.path()));
                } else {
                    total = total.saturating_add(meta.len());
                }
            }
            total
        }
        walk(&path)
    })
    .await
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_collisions_are_resolved_within_the_actual_workspace() {
        let workspace_a = uuid::Uuid::new_v4();
        let workspace_b = uuid::Uuid::new_v4();
        let now = Utc::now();
        let entries = vec![
            MissionIndexEntry {
                status: MissionStatus::Paused,
                updated_at: now - chrono::Duration::days(40),
                workspace_id: workspace_a,
            },
            MissionIndexEntry {
                status: MissionStatus::Completed,
                updated_at: now,
                workspace_id: workspace_b,
            },
        ];

        let selected = entry_for_workspace(&entries, workspace_b).unwrap();
        assert_eq!(selected.workspace_id, workspace_b);
        assert_eq!(selected.status, MissionStatus::Completed);
        assert!(entry_for_workspace(&entries, uuid::Uuid::new_v4()).is_none());
    }

    #[test]
    fn same_workspace_collision_keeps_the_most_protective_entry() {
        let workspace_id = uuid::Uuid::new_v4();
        let now = Utc::now();
        let entries = vec![
            MissionIndexEntry {
                status: MissionStatus::Completed,
                updated_at: now,
                workspace_id,
            },
            MissionIndexEntry {
                status: MissionStatus::Active,
                updated_at: now - chrono::Duration::days(1),
                workspace_id,
            },
        ];

        assert_eq!(
            entry_for_workspace(&entries, workspace_id).unwrap().status,
            MissionStatus::Active
        );
    }
}
