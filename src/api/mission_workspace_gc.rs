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
        let (enabled, days) = read_settings(&state).await;
        if !enabled {
            tracing::trace!("mission workspace GC disabled");
            continue;
        }
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let report = run_once(&state, cutoff).await;
        tracing::info!(
            removed = report.removed,
            errors = report.errors,
            scanned = report.scanned,
            bytes_freed = report.bytes_freed,
            duration_ms = started.elapsed().as_millis() as u64,
            retention_days = days,
            "mission workspace GC sweep finished",
        );
    }
}

async fn read_settings(state: &Arc<AppState>) -> (bool, u32) {
    let snapshot = state.settings.get().await;
    let enabled = snapshot.auto_cleanup_enabled.unwrap_or(false);
    let days = snapshot
        .auto_cleanup_days
        .filter(|d| *d >= 1)
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    (enabled, days)
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

/// Index every mission across all live sessions' stores by the first 8 hex
/// chars of its id — the same prefix embedded in workspace dir names
/// (`mission-<8hex>`) and exec scope units (`-m<8hex>-`). Shared by the
/// orphan-dir sweep and the scope reaper.
pub(crate) async fn build_mission_index(
    state: &Arc<AppState>,
) -> std::collections::HashMap<String, MissionIndexEntry> {
    let mut index: std::collections::HashMap<String, MissionIndexEntry> =
        std::collections::HashMap::new();
    let sessions = state.control.all_sessions().await;
    for session in sessions {
        let store = session.mission_store.clone();
        let mut offset = 0usize;
        loop {
            let page = match store.list_missions(LIST_PAGE_SIZE, offset).await {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "mission index: list_missions failed; skipping session"
                    );
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
                match index.get(&short) {
                    Some(existing)
                        if (protection_rank(&existing.status), existing.updated_at)
                            >= (protection_rank(&entry.status), entry.updated_at) => {}
                    _ => {
                        index.insert(short, entry);
                    }
                }
            }
            if page_len < LIST_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
    }
    index
}

#[derive(Default)]
pub struct SweepReport {
    pub scanned: usize,
    pub removed: usize,
    pub errors: usize,
    pub bytes_freed: u64,
}

/// One full pass: enumerate sessions → missions → eligible → delete.
pub async fn run_once(state: &Arc<AppState>, cutoff: DateTime<Utc>) -> SweepReport {
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
                    None => continue,
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
    report
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
