use super::*;
use crate::local_origin::Snapshot;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub(super) async fn sync(store: &SqliteMissionStore, snapshot: Snapshot) -> Result<(), String> {
    snapshot.validate()?;
    let conn = store.conn.clone();
    tokio::task::spawn_blocking(move||{
  let mut c=conn.blocking_lock();let tx=c.transaction().map_err(err)?;
  tx.execute_batch("CREATE TABLE IF NOT EXISTS local_origins (mission_id TEXT PRIMARY KEY, origin TEXT NOT NULL, sequence INTEGER NOT NULL, terminal INTEGER NOT NULL DEFAULT 0)").map_err(err)?;
  let o=&snapshot.origin;let id=o.id.to_string();let run=o.run_id.to_string();let now=now_string();let origin=serde_json::to_string(o).map_err(err)?;
  let old:Option<(String,u64,bool)>=tx.query_row("SELECT origin,sequence,terminal FROM local_origins WHERE mission_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
  if let Some((previous,sequence,terminal))=old {
   if previous!=origin{return Err("Local mission identity already belongs to another origin".into());}
   if snapshot.sequence<=sequence{
    if !terminal{
     let updated=tx.execute("UPDATE mission_runs SET heartbeat_at=?2 WHERE run_id=?1 AND execution_state<>'terminal' AND generation=1",params![run,now]).map_err(err)?;
     if updated==0{return Err("Local execution receipt is stale".into());}
    }
    return tx.commit().map_err(err);
   }
   if terminal{return Err("Local initial run has ended; reconnect before continuing".into());}
   machine_transfer::guard_start(&tx,o.id)?;
   let owns:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM mission_runs WHERE run_id=?1 AND mission_id=?2 AND generation=1 AND execution_state<>'terminal' AND owner_actor_id=?3)",params![run,id,format!("orb-client:{}",o.client_id)],|r|r.get(0)).map_err(err)?;
   if !owns{return Err("Local execution receipt is stale".into());}
  }else{
   // Plain INSERT is intentional: an offline record can NEVER adopt, replace,
   // or restart an existing mission, even when its UUID collides.
   let tags=serde_json::to_string(&o.tags.iter().cloned().chain(std::iter::once("placement:client".to_owned())).collect::<Vec<_>>()).map_err(err)?;
   tx.execute("INSERT INTO missions (id,status,title,workspace_id,backend,model_override,created_at,updated_at,working_directory,requires_local_disk,project,tags,origin) VALUES (?1,'active',?2,?3,?4,?5,?6,?7,?8,0,?9,?10,'orb-client')",params![id,o.title,crate::workspace::DEFAULT_WORKSPACE_ID.to_string(),o.backend,o.model,o.created_at,now,o.cwd,o.project,tags]).map_err(err)?;
   tx.execute("INSERT INTO mission_runs (run_id,mission_id,generation,execution_state,owner_actor_id,scope_unit,started_at,heartbeat_at) VALUES (?1,?2,1,'running',?3,?4,?5,?6)",params![run,id,format!("orb-client:{}",o.client_id),format!("orb-cwd:{}",o.cwd),o.created_at,now]).map_err(err)?;
   tx.execute("INSERT INTO local_origins (mission_id,origin,sequence) VALUES (?1,?2,0)",params![id,origin]).map_err(err)?;
   tx.execute("INSERT INTO mission_events (mission_id,sequence,event_type,timestamp,event_id,content) VALUES (?1,1,'user_message',?2,?1,?3)",params![id,o.created_at,o.prompt]).map_err(err)?;
  }
  if !snapshot.text.is_empty(){
   let changed=tx.execute("UPDATE mission_events SET content=?3,timestamp=?4 WHERE mission_id=?1 AND event_id=?2 AND event_type='assistant_message'",params![id,run,snapshot.text,now]).map_err(err)?;
   if changed==0{tx.execute("INSERT INTO mission_events (mission_id,sequence,event_type,timestamp,event_id,content) VALUES (?1,(SELECT COALESCE(MAX(sequence),0)+1 FROM mission_events WHERE mission_id=?1),'assistant_message',?2,?3,?4)",params![id,now,run,snapshot.text]).map_err(err)?;}
  }
  let terminal=snapshot.status!="active";
  tx.execute("UPDATE mission_runs SET execution_state=?2,heartbeat_at=?3,ended_at=?4,terminal_reason=?5 WHERE run_id=?1",params![run,if terminal{"terminal"}else{"running"},now,terminal.then_some(&now),terminal.then_some("client_runner")]).map_err(err)?;
  tx.execute("UPDATE missions SET status=?2,updated_at=?3,terminal_reason=?4 WHERE id=?1",params![id,snapshot.status,now,snapshot.error]).map_err(err)?;
  tx.execute("UPDATE local_origins SET sequence=?2,terminal=?3 WHERE mission_id=?1",params![id,snapshot.sequence,terminal]).map_err(err)?;
  tx.commit().map_err(err)
 }).await.map_err(err)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_origin::{Origin, Snapshot};
    fn snapshot() -> Snapshot {
        Snapshot {
            origin: Origin {
                id: Uuid::new_v4(),
                run_id: Uuid::new_v4(),
                client_id: Uuid::new_v4(),
                title: "Offline task".into(),
                project: "test".into(),
                backend: "claudecode".into(),
                model: None,
                cwd: "/local/work".into(),
                prompt: "Work locally".into(),
                created_at: now_string(),
                tags: vec![],
            },
            sequence: 1,
            text: "First output".into(),
            status: "active".into(),
            error: None,
        }
    }
    #[tokio::test]
    async fn local_origin_replay_is_atomic_and_cannot_restart_finished_run() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteMissionStore::new(temp.path().to_path_buf(), "origin")
            .await
            .unwrap();
        let mut s = snapshot();
        let id = s.origin.id;
        sync(&store, s.clone()).await.unwrap();
        sync(&store, s.clone()).await.unwrap();
        let m = store.get_mission(id).await.unwrap().unwrap();
        assert_eq!(m.history.len(), 2);
        assert!(!m.requires_local_disk);
        assert!(m.project.tags.contains(&"placement:client".into()));
        assert!(store.begin_mission_run(id, "core", None).await.is_err());
        s.sequence = 2;
        s.status = "awaiting_user".into();
        s.text = "Final output".into();
        sync(&store, s.clone()).await.unwrap();
        sync(&store, s.clone()).await.unwrap();
        let m = store.get_mission(id).await.unwrap().unwrap();
        assert_eq!(m.history.len(), 2);
        assert_eq!(m.history[1].content, "Final output");
        assert!(store.get_active_mission_run(id).await.unwrap().is_none());
        s.sequence = 3;
        s.status = "active".into();
        assert!(sync(&store, s).await.is_err());
    }
    #[tokio::test]
    async fn local_origin_cannot_adopt_existing_mission_or_change_owner() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteMissionStore::new(temp.path().to_path_buf(), "origin")
            .await
            .unwrap();
        let m = store
            .create_mission(Some("Existing"), None, None, None, None, None, None)
            .await
            .unwrap();
        let mut s = snapshot();
        s.origin.id = m.id;
        assert!(sync(&store, s).await.is_err());
        assert_eq!(
            store.get_mission(m.id).await.unwrap().unwrap().title,
            m.title
        );
        let s = snapshot();
        sync(&store, s.clone()).await.unwrap();
        let mut collision = s.clone();
        collision.origin.client_id = Uuid::new_v4();
        collision.sequence += 1;
        assert!(sync(&store, collision).await.is_err());
        let run = store
            .get_active_mission_run(s.origin.id)
            .await
            .unwrap()
            .unwrap();
        store
            .finish_mission_run(run.run_id, run.generation, Some("moved"))
            .await
            .unwrap();
        let mut stale = s;
        stale.sequence += 1;
        assert!(sync(&store, stale).await.is_err());
    }
}
