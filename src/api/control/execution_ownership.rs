//! Ownership evidence is independent of presentation status and heartbeat age.
//! Read-only snapshots combine every store with accepted remote handles and
//! actor-owned work. Duplicate store observations retain the more conservative
//! claim; a failed read cannot establish quiescence.
use super::*;
use rusqlite::Connection;

#[derive(Clone)]
struct Owner {
    status: MissionStatus,
    reason: Option<String>,
    pr: Option<String>,
    read_only: bool,
}

#[derive(Default)]
pub(crate) struct Snapshot {
    owners: HashMap<Uuid, Vec<Owner>>,
    unresolved: HashSet<Uuid>,
    remote: HashSet<Uuid>,
}

impl Snapshot {
    fn mission(&mut self, mission: Mission) {
        let read_only = crate::api::track_leases::lease_mode(
            None,
            &mission.project.tags,
            mission.project.intent.as_deref(),
        ) == "reader";
        self.owners.entry(mission.id).or_default().push(Owner {
            read_only,
            status: mission.status,
            reason: mission.terminal_reason,
            pr: mission.project.github_pr,
        });
    }

    fn queue(&mut self, payload: &str) -> Result<(), String> {
        if payload.trim().is_empty() {
            return Ok(());
        }
        let items: Vec<QueuedMessage> = serde_json::from_str(payload).map_err(|e| e.to_string())?;
        // Consumed messages are idempotency markers; their actual execution is
        // represented by a run/actor/remote handle, not by historical content.
        self.unresolved.extend(
            items
                .into_iter()
                .filter(|m| !m.inflight)
                .filter_map(|m| m.mission_id),
        );
        Ok(())
    }

    /// None proves that no enumerated store knows this owner. Some(false)
    /// proves all known presentations terminal and no unresolved execution.
    pub(crate) fn holds_track(&self, id: Uuid) -> Option<bool> {
        if self.unresolved.contains(&id) {
            return Some(true);
        }
        self.owners.get(&id).map(|owners| {
            owners.iter().any(|owner| {
                !(owner.status.is_terminal() || owner.status == MissionStatus::Acknowledged)
                    || native_goal_holds_ownership(owner.status, owner.reason.as_deref())
            })
        })
    }

    pub(super) fn unresolved_pr_writer(
        &self,
        pr: &str,
        exclude: Option<Uuid>,
    ) -> Option<PrWriterLease> {
        let target = canonical_github_pr(pr);
        self.unresolved
            .iter()
            .filter(|id| Some(**id) != exclude)
            .find_map(|id| {
                let owners = self.owners.get(id);
                // Missing assignment evidence cannot prove that a still-unresolved
                // execution is unrelated to the requested PR.
                let remote = self.remote.contains(id);
                let conflicting = owners.map_or(remote, |owners| {
                    owners.iter().any(|owner| {
                        !owner.read_only
                            && owner
                                .pr
                                .as_deref()
                                .map_or(remote, |pr| canonical_github_pr(pr) == target)
                    })
                });
                conflicting.then(|| PrWriterLease {
                    id: *id,
                    status: owners
                        .and_then(|owners| owners.first())
                        .map_or(MissionStatus::Active, |owner| owner.status),
                })
            })
    }
}

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get(1))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())
}

fn read_sqlite(path: &std::path::Path, snapshot: &mut Snapshot) -> Result<(), String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open {} read-only: {e}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    let cols = columns(&conn, "missions")?;
    let optional = |name: &str| {
        if cols.contains(name) {
            name.to_string()
        } else {
            "NULL".into()
        }
    };
    let query = format!(
        "SELECT id,status,{},{},{},{} FROM missions",
        optional("terminal_reason"),
        optional("github_pr"),
        optional("tags"),
        optional("intent")
    );
    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (id, status, reason, pr, tags, intent) = row.map_err(|e| e.to_string())?;
        let id = Uuid::parse_str(&id).map_err(|e| e.to_string())?;
        let status =
            serde_json::from_value(serde_json::Value::String(status)).map_err(|e| e.to_string())?;
        let tags: Vec<String> = tags
            .map(|tags| serde_json::from_str(&tags))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let read_only =
            crate::api::track_leases::lease_mode(None, &tags, intent.as_deref()) == "reader";
        snapshot.owners.entry(id).or_default().push(Owner {
            status,
            reason,
            pr,
            read_only,
        });
    }
    if !columns(&conn, "mission_runs")?.is_empty() {
        let mut stmt = conn
            .prepare("SELECT mission_id FROM mission_runs WHERE execution_state <> 'terminal'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for row in rows {
            snapshot.unresolved.insert(
                Uuid::parse_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
            );
        }
    }
    if !columns(&conn, "control_queue")?.is_empty() {
        let mut stmt = conn
            .prepare("SELECT payload FROM control_queue")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for row in rows {
            snapshot.queue(&row.map_err(|e| e.to_string())?)?;
        }
    }
    Ok(())
}

fn read_file(path: &std::path::Path, snapshot: &mut Snapshot) -> Result<(), String> {
    #[derive(Deserialize)]
    struct FileSnapshot {
        missions: HashMap<Uuid, Mission>,
        #[serde(default)]
        runs: HashMap<Uuid, MissionRun>,
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file: FileSnapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    for mission in file.missions.into_values() {
        snapshot.mission(mission);
    }
    snapshot.unresolved.extend(
        file.runs
            .into_values()
            .filter(|run| !run.execution_state.is_terminal())
            .map(|run| run.mission_id),
    );
    Ok(())
}

pub(crate) async fn snapshot(hub: &ControlHub) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::default();
    snapshot.remote.extend(
        crate::remote_node::job_ledger::load(&hub.config.working_dir)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|handle| handle.mission_id),
    );
    snapshot.unresolved.extend(snapshot.remote.iter().copied());
    for session in hub.all_sessions().await {
        snapshot
            .unresolved
            .extend(session.assignment_owners.read().await.iter().copied());
    }
    let inventory = hub.mission_store_inventory().await?;
    for store in inventory.live {
        snapshot.unresolved.extend(
            store
                .list_active_mission_runs()
                .await?
                .into_iter()
                .map(|run| run.mission_id),
        );
        // SQLite ignores this argument; file/memory have no persisted queue.
        snapshot.queue(&store.load_control_queue("").await?)?;
        let mut offset = 0;
        loop {
            let page = store.list_missions(200, offset).await?;
            let count = page.len();
            for mission in page {
                snapshot.mission(mission);
            }
            if count < 200 {
                break;
            }
            offset += count;
        }
    }
    tokio::task::spawn_blocking(move || {
        for path in inventory.offline_sqlite {
            read_sqlite(&path, &mut snapshot)?;
        }
        for user in inventory.offline_file_users {
            read_file(
                &inventory.base_dir.join(format!("missions-{user}.json")),
                &mut snapshot,
            )?;
        }
        Ok(snapshot)
    })
    .await
    .map_err(|e| e.to_string())?
}
