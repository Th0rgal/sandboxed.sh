use super::*;
use crate::api::mission_store::transfer::{Machine, Transfer};
pub(super) const SCHEMA:&str="CREATE TABLE IF NOT EXISTS machine_transfers (id TEXT PRIMARY KEY, mission_id TEXT NOT NULL, request_key TEXT NOT NULL, revision INTEGER NOT NULL, phase TEXT NOT NULL, data TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE(mission_id,request_key)); CREATE UNIQUE INDEX IF NOT EXISTS one_machine_transfer ON machine_transfers(mission_id) WHERE phase NOT IN ('activated','cancelled');";
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub(super) fn guard_start(conn: &Connection, id: Uuid) -> Result<(), String> {
    let active:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM machine_transfers WHERE mission_id=?1 AND phase NOT IN ('activated','cancelled'))",[id.to_string()],|r|r.get(0)).map_err(error)?;
    if active {
        Err("Machine transfer in progress; execution is fenced".into())
    } else {
        Ok(())
    }
}
pub(super) async fn list(store: &SqliteMissionStore, id: Uuid) -> Result<Vec<Transfer>, String> {
    let conn = store.conn.clone();
    tokio::task::spawn_blocking(move || {
        let c = conn.blocking_lock();
        let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='machine_transfers')", [], |r| r.get(0)).map_err(error)?;
        if !exists { return Ok(Vec::new()); }
        let mut q = c
            .prepare(
                "SELECT data FROM machine_transfers WHERE mission_id=?1 ORDER BY created_at,id",
            )
            .map_err(error)?;
        let result = q
            .query_map([id.to_string()], |r| r.get::<_, String>(0))
            .map_err(error)?
            .map(|v| serde_json::from_str(&v.map_err(error)?).map_err(error))
            .collect();
        result
    })
    .await
    .map_err(error)?
}
pub(super) async fn save(
    store: &SqliteMissionStore,
    mut action: Transfer,
    expected: Option<u64>,
) -> Result<Transfer, String> {
    let conn = store.conn.clone();
    tokio::task::spawn_blocking(move||{
        let mut c=conn.blocking_lock();let tx=c.transaction().map_err(error)?;let mid=action.mission_id.to_string();
        let existing:Option<String>=tx.query_row("SELECT data FROM machine_transfers WHERE mission_id=?1 AND request_key=?2",params![mid,action.key],|r|r.get(0)).optional().map_err(error)?;
        if expected.is_none(){
            if let Some(data)=existing{let old:Transfer=serde_json::from_str(&data).map_err(error)?;if old.destination!=action.destination||old.backend!=action.backend||old.model!=action.model{return Err("Idempotency key already used for another destination".into());}return Ok(old);}
            guard_start(&tx,action.mission_id)?;
            let (status,revision):(String,String)=tx.query_row("SELECT status,updated_at FROM missions WHERE id=?1",[&mid],|r|Ok((r.get(0)?,r.get(1)?))).map_err(error)?;
            if !matches!(status.as_str(),"awaiting_user"|"completed"|"failed"|"interrupted"|"paused") || revision!=action.source_revision{return Err("Mission changed or is running; stop it before moving".into());}
            let running:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mission_runs WHERE mission_id=?1 AND execution_state<>'terminal')",[&mid],|r|r.get(0)).map_err(error)?;
            if running{return Err("Source execution has not confirmed termination".into());}
            if let Some(root)=action.source_root.as_deref(){
                let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mission_runs r JOIN missions m ON m.id=r.mission_id WHERE r.execution_state<>'terminal' AND m.working_directory=?1)",[root],|r|r.get(0)).map_err(error)?;
                if other{return Err("Another conversation is writing to this workspace".into());}
                if let Some(source_id)=root.strip_prefix("mission:") {
                    let shared:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mission_runs r JOIN missions m ON m.id=r.mission_id WHERE r.execution_state<>'terminal' AND (m.id=?1 OR EXISTS(SELECT 1 FROM json_each(COALESCE(m.tags,'[]')) WHERE value=?2)))",params![source_id,format!("fork-workspace:{source_id}")],|r|r.get(0)).map_err(error)?;
                    if shared{return Err("Another node conversation is writing to this workspace".into());}
                }

            }

            let generation:u64=tx.query_row("SELECT COALESCE(MAX(generation),0) FROM mission_runs WHERE mission_id=?1",[&mid],|r|r.get(0)).map_err(error)?;
            if generation!=action.source_generation{return Err("Source generation changed".into());}
            action.generation=generation+1;action.revision=0;
            tx.execute("INSERT INTO machine_transfers(id,mission_id,request_key,revision,phase,data,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![action.id.to_string(),mid,action.key,action.revision,action.phase,serde_json::to_string(&action).map_err(error)?,action.created_at]).map_err(error)?;
        }else{
            let old:Transfer=serde_json::from_str(&existing.ok_or("Transfer not found")?).map_err(error)?;
            if Some(old.revision)!=expected||old.id!=action.id||!old.active(){return Err("Transfer revision changed; reload its receipt".into());}
            if action.phase=="activated"{
                if old.phase!="verified" || action.receipt.is_none(){return Err("Destination is not verified".into());}
                let current:(String,u64)=tx.query_row("SELECT updated_at,(SELECT COALESCE(MAX(generation),0) FROM mission_runs WHERE mission_id=?1) FROM missions WHERE id=?1",[&mid],|r|Ok((r.get(0)?,r.get(1)?))).map_err(error)?;
                if current.0!=old.source_revision || current.1!=old.source_generation{return Err("Source changed during preparation; cancel and prepare again".into());}
                let root=action.destination_root.as_deref().ok_or("Missing destination root")?;
                let mut tags:Vec<String>=tx.query_row("SELECT COALESCE(tags,'[]') FROM missions WHERE id=?1",[&mid],|r|r.get::<_,String>(0)).map_err(error).and_then(|s|serde_json::from_str(&s).map_err(error))?;
                tags.retain(|t|t!="placement:client"&&!t.starts_with("fork-workspace:"));
                if matches!(action.destination,Machine::Client{..}){tags.push("placement:client".into());}
                let now=now_string();
                tx.execute("UPDATE missions SET working_directory=?2,requires_local_disk=?3,tags=?4,backend=?5,model_override=?6,model_effort=?7,session_id=NULL,deferred_goal=NULL,workspace_id='00000000-0000-0000-0000-000000000000',status='awaiting_user',terminal_reason='machine_transfer',updated_at=?8,agent=NULL,config_profile=NULL WHERE id=?1",params![mid,root,matches!(action.destination,Machine::Core),serde_json::to_string(&tags).map_err(error)?,action.backend,action.model,action.effort,now]).map_err(error)?;
                // Reserve a terminal generation in the existing execution ledger.
                tx.execute("INSERT INTO mission_runs(run_id,mission_id,generation,execution_state,owner_actor_id,started_at,heartbeat_at,ended_at,terminal_reason) VALUES(?1,?2,?3,'terminal',?4,?5,?5,?5,'machine_transfer')",params![action.id.to_string(),mid,action.generation,format!("machine-transfer:{}",action.id),now]).map_err(error)?;

            }
            action.revision=old.revision+1;
            tx.execute("UPDATE machine_transfers SET revision=?2,phase=?3,data=?4 WHERE id=?1 AND revision=?5",params![action.id.to_string(),action.revision,action.phase,serde_json::to_string(&action).map_err(error)?,old.revision]).map_err(error)?;
        }
        tx.commit().map_err(error)?;Ok(action)
    }).await.map_err(error)?
}

pub(super) fn guard_placement(
    conn: &Connection,
    id: Uuid,
    owner: &str,
    scope: Option<&str>,
) -> Result<(), String> {
    let data:Option<String>=conn.query_row("SELECT data FROM machine_transfers WHERE mission_id=?1 AND phase='activated' ORDER BY created_at DESC,id DESC LIMIT 1",[id.to_string()],|r|r.get(0)).optional().map_err(error)?;
    if let Some(data) = data {
        let t: Transfer = serde_json::from_str(&data).map_err(error)?;
        let valid = match t.destination {
            Machine::Client { id } => owner == format!("orb-client:{id}"),
            Machine::Node { id } => {
                owner.starts_with("remote-job:")
                    && scope == Some(format!("remote-node:{id}").as_str())
            }
            Machine::Core => !owner.starts_with("orb-client:") && !owner.starts_with("remote-job:"),
        };
        if !valid {
            return Err("Execution placement changed; reload the conversation".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action(m: &Mission) -> Transfer {
        Transfer {
            id: Uuid::new_v4(),
            mission_id: m.id,
            key: Uuid::new_v4().to_string(),
            revision: 0,
            phase: "preparing".into(),
            source: Machine::Core,
            destination: Machine::Client {
                id: Uuid::new_v4().to_string(),
            },
            source_revision: m.updated_at.clone(),
            source_generation: 0,
            generation: 1,
            backend: m.backend.clone(),
            model: m.model_override.clone(),
            effort: None,
            source_root: Some("/source".into()),
            destination_root: None,
            manifest: None,
            receipt: None,
            context: "complete history".into(),
            created_at: now_string(),
        }
    }
    #[tokio::test]
    async fn machine_transfer_preserves_identity_and_fences_execution_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteMissionStore::new(dir.path().into(), "transfer")
            .await
            .unwrap();
        let m = store
            .create_mission(
                Some("same conversation"),
                None,
                None,
                None,
                None,
                Some("codex"),
                None,
            )
            .await
            .unwrap();
        store
            .update_mission_status(m.id, MissionStatus::AwaitingUser)
            .await
            .unwrap();
        let m = store.get_mission(m.id).await.unwrap().unwrap();
        let request = action(&m);
        let mut a = store
            .save_machine_transfer(request.clone(), None)
            .await
            .unwrap();
        assert_eq!(
            store.save_machine_transfer(request, None).await.unwrap().id,
            a.id
        );
        assert!(store.save_machine_transfer(action(&m), None).await.is_err());
        assert!(store
            .begin_mission_run(m.id, "orb-client:test", None)
            .await
            .is_err());
        let stale = a.clone();
        a.phase = "verified".into();
        a.destination_root = Some("/destination".into());
        a.receipt = Some(serde_json::json!({"digest":"abc"}));
        a = store.save_machine_transfer(a, Some(0)).await.unwrap();
        assert!(store.save_machine_transfer(stale, Some(0)).await.is_err());
        let rev = a.revision;
        a.phase = "activated".into();
        a = store.save_machine_transfer(a, Some(rev)).await.unwrap();
        drop(store);
        let store = SqliteMissionStore::new(dir.path().into(), "transfer")
            .await
            .unwrap();
        let moved = store.get_mission(m.id).await.unwrap().unwrap();
        assert_eq!(moved.id, m.id);
        assert_eq!(moved.title, m.title);
        assert_eq!(moved.status, MissionStatus::AwaitingUser);
        assert_eq!(moved.working_directory.as_deref(), Some("/destination"));
        assert!(moved.session_id.is_none());
        assert!(store.get_active_mission_run(m.id).await.unwrap().is_none());
        assert_eq!(
            store.machine_transfers(m.id).await.unwrap()[0].phase,
            "activated"
        );
        assert!(store
            .begin_mission_run(m.id, "orb-client:wrong", None)
            .await
            .is_err());
        let Machine::Client { id: client } = a.destination else {
            panic!()
        };
        let run = store
            .begin_mission_run(m.id, &format!("orb-client:{client}"), None)
            .await
            .unwrap();
        assert!(run.generation > a.generation);
    }
    #[tokio::test]
    async fn machine_transfer_cancellation_and_source_revision_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteMissionStore::new(dir.path().into(), "transfer")
            .await
            .unwrap();
        let m = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .unwrap();
        store
            .update_mission_status(m.id, MissionStatus::AwaitingUser)
            .await
            .unwrap();
        let m = store.get_mission(m.id).await.unwrap().unwrap();
        let mut a = store.save_machine_transfer(action(&m), None).await.unwrap();
        a.phase = "verified".into();
        a.destination_root = Some("/destination".into());
        a.receipt = Some(serde_json::json!({"digest":"abc"}));
        a = store.save_machine_transfer(a, Some(0)).await.unwrap();
        store
            .update_mission_title(m.id, "changed while preparing")
            .await
            .unwrap();
        let mut activate = a.clone();
        activate.phase = "activated".into();
        assert!(store
            .save_machine_transfer(activate, Some(a.revision))
            .await
            .is_err());
        let revision = a.revision;
        a.phase = "cancelled".into();
        store
            .save_machine_transfer(a, Some(revision))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_mission(m.id)
                .await
                .unwrap()
                .unwrap()
                .working_directory,
            m.working_directory
        );
        let m = store.get_mission(m.id).await.unwrap().unwrap();
        let mut request = action(&m);
        request.destination = Machine::Node {
            id: "target".into(),
        };
        let mut a = store.save_machine_transfer(request, None).await.unwrap();
        a.phase = "verified".into();
        a.destination_root = Some("/node/destination".into());
        a.receipt = Some(serde_json::json!({"digest":"abc"}));
        a = store.save_machine_transfer(a, Some(0)).await.unwrap();
        let revision = a.revision;
        a.phase = "activated".into();
        store
            .save_machine_transfer(a, Some(revision))
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        assert!(guard_placement(&conn, m.id, "remote-job:test", Some("remote-node:old")).is_err());
        assert!(
            guard_placement(&conn, m.id, "remote-job:test", Some("remote-node:target")).is_ok()
        );
    }
}

pub(super) fn guard_workspace(
    conn: &Connection,
    id: Uuid,
    cwd: Option<&str>,
) -> Result<(), String> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT working_directory FROM missions WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?
        .flatten();
    let tags: Option<String> = conn
        .query_row(
            "SELECT tags FROM missions WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .map_err(error)?;
    let tags: Vec<String> = serde_json::from_str(tags.as_deref().unwrap_or("[]")).map_err(error)?;
    let shared = tags
        .iter()
        .find_map(|t| t.strip_prefix("fork-workspace:"))
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string());
    let alias = format!("mission:{shared}");
    let fenced:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM machine_transfers WHERE phase NOT IN ('activated','cancelled') AND json_extract(data,'$.source_root')=?1)",[alias],|r|r.get(0)).map_err(error)?;
    if fenced {
        return Err("This shared workspace is being transferred".into());
    }
    if let Some(root) = cwd.or(stored.as_deref()) {
        let fenced:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM machine_transfers WHERE phase NOT IN ('activated','cancelled') AND json_extract(data,'$.source_root')=?1)",[root],|r|r.get(0)).map_err(error)?;
        if fenced {
            return Err("This shared workspace is being transferred".into());
        }
    }
    Ok(())
}
