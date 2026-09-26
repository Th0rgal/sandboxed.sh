//! Same-machine recovery of execution receipts. Never expire a live agent by age.
use crate::{local_agents, transfers};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::Duration,
};

#[derive(Clone, Deserialize)]
pub struct Connection {
    pub api_url: String,
    pub token: String,
}

struct RunLock(File);
impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn lock_at(root: &Path, id: &str) -> Result<RunLock, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "Invalid mission identity")?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{id}.lock")))
        .map_err(|e| e.to_string())?;
    file.try_lock_exclusive()
        .map_err(|_| "This session is running or starting in another Orb window".to_string())?;
    Ok(RunLock(file))
}
fn lock(id: &str) -> Result<RunLock, String> {
    let root = std::path::PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?)
        .join(".orb/run-locks");
    lock_at(&root, id)
}
fn stopped(id: &str) -> Result<bool, String> {
    match local_agents::local_agents_poll(id.to_string()) {
        Ok(s) => Ok(s.done),
        Err(e) if e == "no local run" => Ok(true),
        Err(e) => Err(e),
    }
}
// Older Orb versions did not hold the file lock. Also catches an orphan CLI
// which survived the desktop process. Unknown cwd is conservatively busy.
fn workspace_quiet(cwd: &str) -> Result<(), String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let root = std::fs::canonicalize(cwd).map_err(|e| e.to_string())?;
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new()
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    for process in system.processes().values() {
        let name = process.name().to_string_lossy().to_lowercase();
        let harness = ["codex", "claude", "opencode", "grok"]
            .iter()
            .any(|n| name.contains(n));
        if process.cwd().is_some_and(|p| p.starts_with(&root))
            || (harness && process.cwd().is_none())
        {
            return Err(
                "An agent process may still be using this workspace. Stop it before retrying."
                    .into(),
            );
        }
    }
    Ok(())
}
async fn post(
    c: &Connection,
    id: &str,
    route: &str,
    body: Value,
) -> Result<reqwest::Response, String> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/control/missions/{id}/{route}",
            c.api_url.trim_end_matches('/')
        ))
        .bearer_auth(&c.token)
        .timeout(Duration::from_secs(20))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())
}
async fn settle(c: &Connection, id: &str, receipt: &Value) -> Result<(), String> {
    let response = post(c,id,"client-status",json!({"status":"interrupted","run_id":receipt["run_id"],"generation":receipt["generation"]})).await?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not reconcile previous run: {}",
            response.status()
        ));
    }
    Ok(())
}
async fn recover(c: &Connection, id: &str, cwd: &str) -> Result<(), String> {
    if !stopped(id)? {
        return Err("This mission is still running locally or waiting for your answer.".into());
    }
    // A completed app-server can remain alive to serve snapshots. Retire it
    // before checking for processes outside this native runner.
    if local_agents::local_agents_poll(id.to_string()).is_ok() {
        local_agents::local_agents_stop(id.to_string())?;
    }
    inspect_and_settle(c, id, cwd, &transfers::local_machine_identity()?).await
}

async fn inspect_and_settle(
    c: &Connection,
    id: &str,
    cwd: &str,
    client_id: &str,
) -> Result<(), String> {
    let response = post(
        c,
        id,
        "client-run",
        json!({"op":"inspect","client_id":client_id}),
    )
    .await?;
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    // Only this explicit response means absence; transport/auth/other conflicts
    // never grant permission to start or clear another owner's receipt.
    if status.as_u16() == 409 && body.contains("No active run on this computer") {
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("Could not inspect previous run ({status}): {body}"));
    }
    let receipt: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    if receipt["run_id"].as_str().is_none() || receipt["generation"].as_u64().is_none() {
        return Err("Invalid previous run receipt".into());
    }
    workspace_quiet(cwd)?;
    settle(c, id, &receipt).await
}

#[tauri::command]
pub async fn local_run_reconcile(id: String, connection: Connection) -> Result<(), String> {
    let _lock = lock(&id)?;
    let bindings = crate::local_bindings(None, None)?;
    let cwd = bindings[&id]["cwd"]
        .as_str()
        .ok_or("No session binding on this computer")?;
    recover(&connection, &id, cwd).await
}

#[tauri::command]
pub async fn local_run_launch(
    mut request: local_agents::StartRequest,
    connection: Connection,
) -> Result<Value, String> {
    let guard = lock(&request.id)?;
    recover(&connection, &request.id, &request.cwd).await?;
    let client_id = transfers::local_machine_identity()?;
    let response=post(&connection,&request.id,"client-run",json!({"op":"begin","client_id":client_id,"prompt":request.prompt,"cwd":request.cwd,"session_id":request.session_id})).await?;
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    let receipt: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    request.prompt = receipt["prompt"]
        .as_str()
        .ok_or("Missing prepared prompt")?
        .to_string();
    let permit = transfers::Permit {
        legacy: false,
        api_url: connection.api_url.clone(),
        token: connection.token.clone(),
        client_id,
        run_id: receipt["run_id"]
            .as_str()
            .ok_or("Missing run identity")?
            .to_string(),
        generation: receipt["generation"]
            .as_u64()
            .ok_or("Missing run generation")?,
    };
    let id = request.id.clone();
    if let Err(error) = transfers::local_agents_start_authorized(request, permit).await {
        // Keep an uncertain launch fenced. Next reconciliation will retry only
        // after checking the native process and the exact server receipt.
        return Err(error);
    }
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if stopped(&id).unwrap_or(false) {
                break;
            }
        }
    });
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_excludes_other_instances_until_owner_exits() {
        let d = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let a = lock_at(d.path(), &id).unwrap();
        assert!(lock_at(d.path(), &id).is_err());
        drop(a);
        assert!(lock_at(d.path(), &id).is_ok());
    }
    #[test]
    fn lock_rejects_non_mission_paths() {
        let d = tempfile::tempdir().unwrap();
        assert!(lock_at(d.path(), "../other").is_err());
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use std::io::{Read, Write};
    fn server(responses: Vec<(u16, String)>) -> (Connection, std::thread::JoinHandle<Vec<Value>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = vec![];
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![];
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.parse::<usize>().ok())
                    })
                    .unwrap();
                let mut payload = vec![0; len];
                socket.read_exact(&mut payload).unwrap();
                requests.push(serde_json::from_slice(&payload).unwrap());
                write!(socket,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            requests
        });
        (
            Connection {
                api_url: format!("http://{address}"),
                token: "test".into(),
            },
            handle,
        )
    }
    #[test]
    fn recovery_closes_only_the_inspected_generation() {
        let root = tempfile::tempdir().unwrap();
        let receipt = json!({"run_id":"previous","generation":5});
        let (connection, server) = server(vec![(200, receipt.to_string()), (200, "{}".into())]);
        tauri::async_runtime::block_on(inspect_and_settle(
            &connection,
            "mission",
            root.path().to_str().unwrap(),
            "this-machine",
        ))
        .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0],
            json!({"op":"inspect","client_id":"this-machine"})
        );
        assert_eq!(
            requests[1],
            json!({"status":"interrupted","run_id":"previous","generation":5})
        );
    }
    #[cfg(unix)]
    #[test]
    fn orphan_recovery_then_launch_is_one_exclusive_operation() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("grok");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let old = uuid::Uuid::new_v4().to_string();
        let new = uuid::Uuid::new_v4().to_string();
        let (connection, server) = server(vec![
            (200, json!({"run_id":old,"generation":5}).to_string()),
            (200, "{}".into()),
            (
                200,
                json!({"run_id":new,"generation":6,"prompt":"hello"}).to_string(),
            ),
            (200, "{}".into()),
        ]);
        let request = local_agents::StartRequest {
            id: id.clone(),
            harness: "grok".into(),
            bin: script.to_str().unwrap().into(),
            cwd: root.path().to_str().unwrap().into(),
            prompt: "hello".into(),
            session_id: None,
            model: None,
            image_paths: vec![],
        };
        let result =
            tauri::async_runtime::block_on(local_run_launch(request.clone(), connection.clone()))
                .unwrap();
        let second = tauri::async_runtime::block_on(local_run_launch(request, connection));
        local_agents::local_agents_stop(id).unwrap();
        assert_eq!(result["generation"], 6);
        assert!(second.unwrap_err().contains("another Orb window"));
        let requests = server.join().unwrap();
        assert_eq!(requests[1]["run_id"], old);
        assert_eq!(requests[2]["op"], "begin");
        assert_eq!(requests[3]["run_id"], new);
        assert_eq!(requests[3]["generation"], 6);
    }

    #[test]
    fn uncertain_inspection_never_closes_a_run() {
        for status in [401, 403, 409, 500] {
            let (connection, server) = server(vec![(status, "unavailable".into())]);
            assert!(tauri::async_runtime::block_on(inspect_and_settle(
                &connection,
                "mission",
                "/missing",
                "this-machine"
            ))
            .is_err());
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }
    #[test]
    fn generation_change_rejects_recovery_without_retrying_another_receipt() {
        let root = tempfile::tempdir().unwrap();
        let (connection, server) = server(vec![
            (200, json!({"run_id":"old","generation":5}).to_string()),
            (409, "stale receipt".into()),
        ]);
        assert!(tauri::async_runtime::block_on(inspect_and_settle(
            &connection,
            "mission",
            root.path().to_str().unwrap(),
            "this-machine"
        ))
        .is_err());
        assert_eq!(server.join().unwrap().len(), 2);
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn surviving_workspace_process_prevents_recovery() {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(root.path())
            .spawn()
            .unwrap();
        let result = workspace_quiet(root.path().to_str().unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(result.is_err());
        assert!(workspace_quiet(root.path().to_str().unwrap()).is_ok());
    }
    #[test]
    fn native_live_run_is_not_recovered_even_without_output() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("grok");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        local_agents::local_agents_start(local_agents::StartRequest {
            id: id.clone(),
            harness: "grok".into(),
            bin: script.to_str().unwrap().into(),
            cwd: root.path().to_str().unwrap().into(),
            prompt: "test".into(),
            session_id: None,
            model: None,
            image_paths: vec![],
        })
        .unwrap();
        let result = tauri::async_runtime::block_on(recover(
            &Connection {
                api_url: "http://127.0.0.1:1".into(),
                token: "test".into(),
            },
            &id,
            root.path().to_str().unwrap(),
        ));
        local_agents::local_agents_stop(id).unwrap();
        assert!(result
            .unwrap_err()
            .contains("still running locally or waiting"));
    }
}

#[cfg(test)]
mod btw_smoke {
    use super::*;
    #[test]
    #[ignore = "explicit operator replay; ORB_BTW_REPAIR_PROMPT and ORB_BTW_TEST_* required"]
    fn btw_replay_empty_response() {
        let id = std::env::var("ORB_BTW_TEST_ID").unwrap();
        let connection = Connection {
            api_url: std::env::var("ORB_BTW_TEST_URL").unwrap(),
            token: std::env::var("ORB_BTW_TEST_TOKEN").unwrap(),
        };
        let request = local_agents::StartRequest {
            id: id.clone(),
            harness: "opencode".into(),
            bin: "/opt/homebrew/bin/opencode".into(),
            cwd: std::env::var("ORB_BTW_TEST_CWD").unwrap(),
            prompt: std::fs::read_to_string(std::env::var("ORB_BTW_REPAIR_PROMPT").unwrap())
                .unwrap(),
            model: Some("builtin/smart".into()),
            session_id: None,
            image_paths: vec![],
        };
        let receipt =
            tauri::async_runtime::block_on(local_run_launch(request, connection.clone())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        loop {
            let state = local_agents::local_agents_poll(id.clone()).unwrap();
            if state.done {
                assert_eq!(state.exit_code, Some(0), "{:?}", state.error);
                assert!(!state.text.trim().is_empty(), "No response captured");
                for (route, body) in [
                    (
                        "client-transcript",
                        json!({"id":uuid::Uuid::new_v4().to_string(),"role":"assistant","content":state.text,"run_id":receipt["run_id"],"generation":receipt["generation"]}),
                    ),
                    (
                        "client-status",
                        json!({"status":"awaiting_user","run_id":receipt["run_id"],"generation":receipt["generation"]}),
                    ),
                ] {
                    let response =
                        tauri::async_runtime::block_on(post(&connection, &id, route, body))
                            .unwrap();
                    assert!(response.status().is_success(), "{}", response.status());
                }
                println!(
                    "Replayed side question and persisted {} response bytes",
                    state.text.len()
                );
                break;
            }
            if std::time::Instant::now() > deadline {
                local_agents::local_agents_stop(id.clone()).unwrap();
                panic!("Replay timed out");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    #[test]
    #[ignore = "authenticated OpenCode integration; ORB_BTW_TEST_ID/URL/TOKEN/CWD required"]
    fn btw_same_workspace_roundtrip() {
        let id = std::env::var("ORB_BTW_TEST_ID").unwrap();
        let cwd = std::env::var("ORB_BTW_TEST_CWD").unwrap();
        let connection = Connection {
            api_url: std::env::var("ORB_BTW_TEST_URL").unwrap(),
            token: std::env::var("ORB_BTW_TEST_TOKEN").unwrap(),
        };
        let request=local_agents::StartRequest{id:id.clone(),harness:"opencode".into(),bin:"/opt/homebrew/bin/opencode".into(),cwd:cwd.clone(),prompt:"Integration check: use the bash tool to run pwd, then read btw-fixture.txt in this directory. Reply with its exact content and the working directory. Do not edit any files or delegate.".into(),model:Some("builtin/smart".into()),session_id:None,image_paths:vec![]};
        let receipt =
            tauri::async_runtime::block_on(local_run_launch(request, connection.clone())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        loop {
            let state = local_agents::local_agents_poll(id.clone()).unwrap();
            if state.done {
                assert_eq!(state.exit_code, Some(0), "{:?}", state.error);
                assert!(state.text.contains("BTW-TOOLS-7319"), "{}", state.text);
                assert!(state.text.contains(&cwd), "{}", state.text);
                assert!(
                    state
                        .session_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("ses_")),
                    "OpenCode session identity was not captured"
                );
                tauri::async_runtime::block_on(settle(&connection, &id, &receipt)).unwrap();
                println!("BTW native tools, shared cwd and session identity verified");
                break;
            }
            if std::time::Instant::now() > deadline {
                local_agents::local_agents_stop(id.clone()).unwrap();
                panic!("side agent timed out");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}
