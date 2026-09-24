//! Node-owned project replicas. Credentials never enter the harness environment.
use crate::{context_replica::Replica, project_context::Store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub endpoint: String,
    pub token: String,
    pub project: String,
}
fn replica(base: &Path, request: &Request) -> Result<Replica, String> {
    crate::project_context::valid_path(&request.project)?;
    if request.project.contains('/') {
        return Err("invalid project".into());
    }
    let key = crate::project_context::digest(request.endpoint.as_bytes());
    let base = base
        .join("project-context")
        .join(key)
        .join(&request.project);
    Ok(Replica {
        store: Store::new(base.join("files"), base.join("state")),
        endpoint: request.endpoint.clone(),
        token: request.token.clone(),
        project: request.project.clone(),
        source: "compute node".into(),
    })
}
pub async fn prepare(base: &Path, request: Request) -> Result<serde_json::Value, String> {
    let replica = replica(base, &request)?;
    let state = replica.tick().await?;
    if !state.ready || state.error.is_some() {
        return Err(state.error.unwrap_or("context replica is not ready".into()));
    }
    let config = replica.store.metadata.join("connection.json");
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(config).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"root":replica.store.root,"manifest":replica.store.manifest()?}))
}
pub fn start(base: PathBuf) {
    tokio::spawn(async move {
        loop {
            let root = base.join("project-context");
            let mut configs = Vec::new();
            if let Ok(servers) = std::fs::read_dir(&root) {
                for server in servers.flatten() {
                    if let Ok(projects) = std::fs::read_dir(server.path()) {
                        for project in projects.flatten() {
                            configs.push(project.path().join("state/connection.json"));
                        }
                    }
                }
            }
            for config in configs {
                let Ok(bytes) = std::fs::read(&config) else {
                    continue;
                };
                let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
                    continue;
                };
                if let Ok(replica) = replica(&base, &request) {
                    let _ = replica.tick().await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}
