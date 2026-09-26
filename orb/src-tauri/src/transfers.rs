use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
#[path = "../../../shared/machine_transfer.rs"]
pub mod checkpoint;
fn home() -> Result<PathBuf, String> {
    Ok(PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?).join(".orb"))
}
#[tauri::command]
pub fn local_machine_identity() -> Result<String, String> {
    let _lock = crate::BINDINGS_LOCK.lock().map_err(|e| e.to_string())?;
    let home = home()?;
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let path = home.join("machine-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        uuid::Uuid::parse_str(id.trim()).map_err(|e| e.to_string())?;
        return Ok(id.trim().into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(path, &id).map_err(|e| e.to_string())?;
    Ok(id)
}
#[tauri::command]
pub async fn local_machine_transfer(
    id: String,
    transfer_id: String,
    side: String,
    operation: checkpoint::Operation,
) -> Result<Value, String> {
    let tid = uuid::Uuid::parse_str(&transfer_id).map_err(|e| e.to_string())?;
    if !matches!(side.as_str(), "source" | "destination") {
        return Err("Invalid transfer side".into());
    }
    if matches!(operation, checkpoint::Operation::Snapshot) {
        match crate::local_agents::local_agents_poll(id.clone()) {
            Ok(run) if !run.done => return Err("Stop the local agent before moving".into()),
            Err(e) if !e.contains("no local run") => return Err(e),
            _ => {}
        }
        crate::local_agents::local_agents_stop(id.clone())?;
    }
    let binding = crate::local_bindings(None, None)?;
    let source = binding[&id]["cwd"].as_str().map(PathBuf::from);
    if matches!(operation, checkpoint::Operation::Snapshot)
        && source
            .as_ref()
            .is_some_and(|p| crate::local_agents::workspace_busy(p).unwrap_or(true))
    {
        return Err("Another local agent is writing to this workspace".into());
    }
    let area = home()?.join("transfers").join(tid.to_string()).join(side);
    tauri::async_runtime::spawn_blocking(move || {
        checkpoint::operate(&area, source.as_deref(), operation)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[derive(Deserialize)]
pub struct Permit {
    #[serde(default)]
    pub legacy: bool,
    pub api_url: String,
    pub token: String,
    pub run_id: String,
    pub generation: u64,
    pub client_id: String,
}
#[tauri::command]
pub async fn local_agents_start_authorized(
    request: crate::local_agents::StartRequest,
    permit: Permit,
) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(&request.id).map_err(|_| "Invalid mission identity")?;
    if !permit.legacy && permit.client_id != local_machine_identity()? {
        return Err("Execution permit belongs to another computer".into());
    }
    let response=reqwest::Client::new().post(format!("{}/api/control/missions/{id}/client-run",permit.api_url.trim_end_matches('/'))).bearer_auth(&permit.token).timeout(std::time::Duration::from_secs(20)).json(&json!({"op":"verify","client_id":permit.client_id,"run_id":permit.run_id,"generation":permit.generation})).send().await.map_err(|_|"Unable to verify local execution permit")?;
    if !response.status().is_success() {
        if permit.legacy && matches!(response.status().as_u16(), 404 | 405) {
            let capabilities = reqwest::Client::new()
                .get(format!(
                    "{}/api/control/missions/{id}/machine-transfer",
                    permit.api_url.trim_end_matches('/')
                ))
                .bearer_auth(&permit.token)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| "Could not verify legacy backend")?;
            if !matches!(capabilities.status().as_u16(), 404 | 405) {
                return Err("Backend supports execution permits; reload before starting".into());
            }
        } else {
            return Err("Local execution permit is stale; refresh the conversation".into());
        }
    }
    let mission_id = request.id.clone();
    crate::routed_opencode::start(request, &permit.api_url, &permit.token).await?;
    if !permit.legacy {
        let generation = crate::local_agents::native_generation(&mission_id);
        tauri::async_runtime::spawn(async move {
            let http = reqwest::Client::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if crate::local_agents::native_generation(&mission_id) != generation {
                    break;
                }
                if crate::local_agents::local_agents_poll(mission_id.clone())
                    .map(|r| r.done)
                    .unwrap_or(true)
                {
                    break;
                }
                let response=http.post(format!("{}/api/control/missions/{mission_id}/client-run",permit.api_url.trim_end_matches('/'))).bearer_auth(&permit.token).timeout(std::time::Duration::from_secs(10)).json(&json!({"op":"verify","client_id":permit.client_id,"run_id":permit.run_id,"generation":permit.generation})).send().await;
                // Connectivity loss preserves the fence; only a definitive stale
                // receipt stops this particular native generation.
                if response.is_ok_and(|r| r.status().as_u16() == 409) {
                    if crate::local_agents::native_generation(&mission_id) == generation {
                        let _ = crate::local_agents::stop_generation(
                            &mission_id,
                            generation.as_deref(),
                        );
                    }
                    break;
                }
            }
        });
    }
    Ok(())
}
