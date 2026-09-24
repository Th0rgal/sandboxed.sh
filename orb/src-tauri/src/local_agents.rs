//! Local harnesses. Orb spawns the CLIs already installed on this machine.
//! It does not embed sandboxed.sh and does not read the backend's credentials.
//!
//! Claude Code: `claude --print --output-format stream-json`.
//! Codex: `codex app-server` (initialize, thread/start or thread/resume, turn/start).
//! OpenCode: `opencode run --format json`, `--session ses_*` or `--continue`.
//! Grok: `grok --prompt`. The repo has no resume flag for that CLI, so a follow-up
//! starts a new local session and says so.

use crate::local_stream::{Event as OutputEvent, Output};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const HARNESSES: &[(&str, &str)] = &[
    ("claudecode", "claude"),
    ("codex", "codex"),
    ("grok", "grok"),
    ("opencode", "opencode"),
];

#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    #[serde(default)]
    pub overrides: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ScanRow {
    pub id: String,
    pub bin: String,
    pub path: Option<String>,
    pub version: Option<String>,
    pub installed: bool,
    pub plan_supported: bool,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceRequest {
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct WriteFile {
    #[serde(default)]
    pub encoding: Option<String>,
    pub rel: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct WriteRequest {
    pub root: String,
    pub files: Vec<WriteFile>,
}

#[derive(Debug, Serialize)]
pub struct WriteReport {
    pub binary_supported: bool,
    pub written: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartRequest {
    #[serde(default)]
    pub image_paths: Vec<String>,
    pub id: String,
    pub harness: String,
    pub bin: String,
    pub cwd: String,
    pub prompt: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PollState {
    pub activities: Vec<crate::local_stream::Activity>,
    pub text: String,
    pub done: bool,
    pub exit_code: Option<i32>,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub resumed: bool,
}

#[derive(Clone)]
struct Run {
    generation: String,
    cwd: PathBuf,
    child: Arc<Mutex<Child>>,
    text: Arc<Output>,
    done: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
    session_id: Arc<Mutex<Option<String>>>,
    error: Arc<Mutex<Option<String>>>,
    resumed: bool,
}

fn runs() -> &'static Mutex<HashMap<String, Run>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Run>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[tauri::command]
pub async fn local_agents_scan(request: ScanRequest) -> Result<Vec<ScanRow>, String> {
    // CLI discovery launches subprocesses; never block the desktop event loop.
    tauri::async_runtime::spawn_blocking(move || scan_local_agents(request))
        .await
        .map_err(|error| error.to_string())
}

fn scan_local_agents(request: ScanRequest) -> Vec<ScanRow> {
    HARNESSES
        .iter()
        .map(|(id, bin)| {
            let override_path = request
                .overrides
                .get(*id)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let path = override_path
                .map(|p| PathBuf::from(p))
                .filter(|p| p.is_file())
                .or_else(|| which(bin));
            let version = path.as_ref().and_then(|p| version_of(p));
            ScanRow {
                id: (*id).to_string(),
                bin: (*bin).to_string(),
                installed: path.is_some() && version.is_some(),
                plan_supported: version
                    .as_deref()
                    .is_some_and(|v| native_plan_supported(id, v)),
                path: path.map(|p| p.display().to_string()),
                version,
            }
        })
        .collect()
}

#[tauri::command]
pub fn local_agents_workspace(request: WorkspaceRequest) -> Result<String, String> {
    let slug = safe_slug(&request.slug)?;
    let root = workspace_root()?.join(slug);
    std::fs::create_dir_all(root.join(".paloma").join("attach")).map_err(|e| e.to_string())?;
    Ok(root.display().to_string())
}

#[tauri::command]
pub fn local_agents_write(request: WriteRequest) -> Result<WriteReport, String> {
    let root = PathBuf::from(&request.root);
    if !root.is_dir() {
        return Err("workspace root is not a directory".into());
    }
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for file in request.files {
        let rel = match safe_rel(&file.rel) {
            Ok(rel) => rel,
            Err(error) => {
                skipped.push(format!("{} ({error})", file.rel));
                continue;
            }
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if is_secret_path(&rel_str) {
            skipped.push(format!("{rel_str} (secret)"));
            continue;
        }
        let limit = if file.encoding.as_deref() == Some("base64") {
            14 * 1024 * 1024
        } else {
            512 * 1024
        };
        if file.content.len() > limit {
            skipped.push(format!("{rel_str} (too large)"));
            continue;
        }
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = match file.encoding.as_deref() {
            Some("base64") => base64::engine::general_purpose::STANDARD
                .decode(&file.content)
                .map_err(|e| e.to_string())?,
            None => file.content.into_bytes(),
            Some(_) => return Err("Unsupported attachment encoding".into()),
        };
        std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
        written.push(rel_str);
    }
    Ok(WriteReport {
        written,
        skipped,
        binary_supported: true,
    })
}

#[tauri::command]
pub fn local_agents_start(request: StartRequest) -> Result<(), String> {
    if request
        .prompt
        .trim()
        .strip_prefix("/plan")
        .is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace))
        && !matches!(request.harness.as_str(), "codex" | "claudecode")
    {
        return Err("Native plan mode is not supported by this integration.".into());
    }
    if request.id.trim().is_empty() {
        return Err("run id is required".into());
    }
    let cwd = PathBuf::from(&request.cwd);
    if !cwd.is_dir() {
        return Err("working directory does not exist".into());
    }
    let bin = PathBuf::from(&request.bin);
    if !bin.is_file() {
        return Err(format!("CLI not found at {}", request.bin));
    }
    let mut map = runs().lock().map_err(|e| e.to_string())?;
    if let Some(previous) = map.get(&request.id) {
        if !previous.done.load(Ordering::SeqCst) {
            return Err("This mission is still running locally. Wait for it to finish or stop it before sending another message.".into());
        }
    }
    // Completed handles are retained for reconnect snapshots until the next turn.
    // Retire the old app-server before resuming its thread in a fresh process.
    if let Some(previous) = map.remove(&request.id) {
        if let Ok(mut child) = previous.child.lock() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
    let text = Arc::new(Output::default());
    let done = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(Mutex::new(None));
    let session_id = Arc::new(Mutex::new(request.session_id.clone()));
    let error = Arc::new(Mutex::new(None));
    let resumed =
        request.session_id.as_deref().is_some_and(|s| !s.is_empty()) && request.harness != "grok";
    let interaction = crate::interactions::begin(&request.id);
    let child = match spawn_harness(&request, &text, &session_id, &error, &done) {
        Ok(child) => child,
        Err(error) => {
            crate::interactions::finish(&interaction);
            return Err(error);
        }
    };
    let child = Arc::new(Mutex::new(child));
    watch_exit(
        interaction,
        Arc::clone(&child),
        Arc::clone(&done),
        Arc::clone(&exit_code),
        Arc::clone(&text),
    );
    map.insert(
        request.id,
        Run {
            generation: uuid::Uuid::new_v4().to_string(),
            cwd,
            child,
            text,
            done,
            exit_code,
            session_id,
            error,
            resumed,
        },
    );
    Ok(())
}

fn watch_exit(
    mission_id: crate::interactions::Session,
    child: Arc<Mutex<Child>>,
    done: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
    output: Arc<Output>,
) {
    thread::spawn(move || loop {
        let status = child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .flatten();
        if let Some(status) = status {
            crate::interactions::finish(&mission_id);
            if let Ok(mut slot) = exit_code.lock() {
                *slot = status.code();
            }
            while !output.drained() {
                thread::sleep(Duration::from_millis(5));
            }
            done.store(true, Ordering::SeqCst);
            break;
        }
        if done.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(80));
    });
}

#[tauri::command]
pub fn local_agents_poll(id: String) -> Result<PollState, String> {
    let map = runs().lock().map_err(|e| e.to_string())?;
    let run = map.get(&id).ok_or_else(|| "no local run".to_string())?;
    let snapshot = PollState {
        text: run.text.snapshot(),
        activities: run.text.activities(),
        done: run.done.load(Ordering::SeqCst),
        exit_code: *run.exit_code.lock().map_err(|e| e.to_string())?,
        session_id: run.session_id.lock().map_err(|e| e.to_string())?.clone(),
        error: run.error.lock().map_err(|e| e.to_string())?.clone(),
        resumed: run.resumed,
    };
    Ok(snapshot)
}

/// Subscribe atomically with the snapshot so startup text cannot be missed.
#[tauri::command]
pub fn local_agents_subscribe(
    id: String,
    on_event: tauri::ipc::Channel<OutputEvent>,
) -> Result<(), String> {
    let run = runs()
        .lock()
        .map_err(|e| e.to_string())?
        .get(&id)
        .cloned()
        .ok_or("no local run")?;
    let token = run.text.subscribe(on_event.clone())?;
    thread::spawn(move || {
        while !run.done.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(40));
        }
        let state = PollState {
            text: run.text.snapshot(),
            activities: run.text.activities(),
            done: true,
            exit_code: *run.exit_code.lock().unwrap(),
            session_id: run.session_id.lock().unwrap().clone(),
            error: run.error.lock().unwrap().clone(),
            resumed: run.resumed,
        };
        let _ = on_event.send(OutputEvent {
            text: String::new(),
            reset: false,
            state: Some(state),
        });
        run.text.unsubscribe(token);
    });
    Ok(())
}

#[tauri::command]
pub fn local_agents_stop(id: String) -> Result<(), String> {
    stop_generation(&id, None)
}
pub fn stop_generation(id: &str, expected: Option<&str>) -> Result<(), String> {
    let map = runs().lock().map_err(|e| e.to_string())?;
    if let Some(run) = map.get(id) {
        if expected.is_some_and(|token| token != run.generation) {
            return Ok(());
        }
        crate::interactions::cancel(id);
        let mut child = run.child.lock().map_err(|e| e.to_string())?;
        // Completed runs stay cached for their transcript. Their old process
        // group ID may have been reused, so never signal it after completion.
        #[cfg(unix)]
        if !run.done.load(Ordering::SeqCst) {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            child.kill().map_err(|e| e.to_string())?;
        }
        child.wait().map_err(|e| e.to_string())?;
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !run.text.drained() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !run.text.drained() {
            return Err("Local output is still draining; retry Stop before moving".into());
        }
        run.done.store(true, Ordering::SeqCst);
    }
    Ok(())
}

fn spawn_harness(
    request: &StartRequest,
    text: &Arc<Output>,
    session_id: &Arc<Mutex<Option<String>>>,
    error: &Arc<Mutex<Option<String>>>,
    done: &Arc<AtomicBool>,
) -> Result<Child, String> {
    match request.harness.as_str() {
        "claudecode" => spawn_claude(request, text, session_id, error),
        "codex" => spawn_codex(request, text, session_id, error, done),
        "grok" => spawn_piped(request, grok_args(&request.prompt), text, error, false),
        "opencode" => spawn_piped(request, opencode_args(request), text, error, true),
        other => Err(format!("unknown local harness {other}")),
    }
}

fn grok_args(prompt: &str) -> Vec<String> {
    vec![
        "--always-approve".into(),
        "--prompt".into(),
        prompt.to_string(),
    ]
}

fn opencode_args(request: &StartRequest) -> Vec<String> {
    let mut args = vec!["run".into(), "--format".into(), "json".into()];
    if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
        args.push("--model".into());
        // Backend aliases need their configured OpenCode provider namespace.
        args.push(if model.starts_with("builtin/") {
            format!("sandboxed-sh/{model}")
        } else {
            model.to_string()
        });
    }
    match request.session_id.as_deref().filter(|s| !s.is_empty()) {
        Some(sid) if sid.starts_with("ses_") => {
            args.push("--session".into());
            args.push(sid.to_string());
        }
        Some(_) => args.push("--continue".into()),
        None => {}
    }
    args.push(request.prompt.clone());
    for path in &request.image_paths {
        args.extend(["--file".into(), path.clone()]);
    }
    args
}

fn spawn_claude(
    request: &StartRequest,
    text: &Arc<Output>,
    session_id: &Arc<Mutex<Option<String>>>,
    error: &Arc<Mutex<Option<String>>>,
) -> Result<Child, String> {
    let plan = request
        .prompt
        .trim()
        .strip_prefix("/plan")
        .filter(|s| s.is_empty() || s.starts_with(char::is_whitespace))
        .map(str::trim);
    let mut cmd = Command::new(&request.bin);
    if plan.is_some() {
        cmd.args([
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan",
        ]);
    } else {
        cmd.arg("--dangerously-skip-permissions");
    }
    // Every turn needs a live permission channel, including resumed sessions.
    // Print mode otherwise denies tool requests that need user approval.
    cmd.args([
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
    ]);
    cmd.current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]);
    if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
        let bare = model.strip_prefix("anthropic/").unwrap_or(model);
        cmd.arg("--model").arg(bare);
    }
    if let Some(sid) = request.session_id.as_deref().filter(|s| !s.is_empty()) {
        cmd.arg("--resume").arg(sid);
    } else if let Ok(mut slot) = session_id.lock() {
        let fresh = uuid_like();
        cmd.arg("--session-id").arg(&fresh);
        *slot = Some(fresh);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start Claude Code: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("Claude stdin missing")?;
        let stdout = child.stdout.take().ok_or("Claude stdout missing")?;
        let mission_id = crate::interactions::session(&request.id);
        let prompt = plan.unwrap_or(&request.prompt).to_owned();
        let mut execution_approved = plan.is_none();
        let output = Arc::clone(text);
        let failures = Arc::clone(error);
        let guard = text.reader();
        thread::spawn(move || {
            let _guard = guard;
            let result = (|| -> Result<(), String> {
                write_line(&mut stdin, &json!({"type":"control_request","request_id":"orb-init","request":{"subtype":"initialize"}}).to_string())?;
                write_line(
                    &mut stdin,
                    &json!({"type":"user","message":{"role":"user","content":prompt}}).to_string(),
                )?;
                let mut implement_after_result = false;
                let mut claude_text = ClaudeText::default();
                let mut background = ClaudeBackground::default();
                for line in BufReader::new(stdout).lines() {
                    let line = line.map_err(|e| e.to_string())?;
                    let Ok(event) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    output.claude_activity(&event);
                    background.consume(&event);
                    if implement_after_result
                        && event["type"] == "assistant"
                        && event["message"]["content"]
                            .as_array()
                            .is_some_and(|blocks| {
                                blocks.iter().any(|b| {
                                    b["type"] == "tool_use"
                                        && b["name"] != "ExitPlanMode"
                                        && b["name"] != "AskUserQuestion"
                                })
                            })
                    {
                        implement_after_result = false;
                    }
                    if event["type"] == "control_response"
                        && event["response"]["subtype"] == "error"
                    {
                        return Err(format!(
                            "Claude control request failed: {}",
                            event["response"]["error"]
                        ));
                    }
                    if event["type"] == "control_request"
                        && event["request"]["subtype"] == "can_use_tool"
                    {
                        let tool = event["request"]["tool_name"].as_str().unwrap_or("");
                        let input = event["request"]["input"].clone();
                        let response = if tool == "AskUserQuestion" {
                            let answers = crate::interactions::ask(
                                &mission_id,
                                "claude_questions",
                                input.clone(),
                            )?;
                            let mut updated = input;
                            updated["answers"] = answers["answers"].clone();
                            json!({"behavior":"allow","updatedInput":updated})
                        } else if tool == "ExitPlanMode" {
                            let answer =
                                crate::interactions::ask(&mission_id, "plan", input.clone())?;
                            if answer["action"] == "accept" {
                                implement_after_result = true;
                                execution_approved = true;
                                json!({"behavior":"allow","updatedInput":input})
                            } else {
                                json!({"behavior":"deny","message":answer["feedback"].as_str().unwrap_or("Please revise the plan.")})
                            }
                        } else if execution_approved {
                            json!({"behavior":"allow","updatedInput":input})
                        } else {
                            {
                                let answer = crate::interactions::ask(
                                    &mission_id,
                                    "permission",
                                    json!({"tool":tool,"input":input}),
                                )?;
                                if answer["action"] == "accept" {
                                    json!({"behavior":"allow","updatedInput":input})
                                } else {
                                    json!({"behavior":"deny","message":"The user declined this action."})
                                }
                            }
                        };
                        write_line(&mut stdin,&json!({"type":"control_response","response":{"subtype":"success","request_id":event["request_id"],"response":response}}).to_string())?;
                    } else if event["type"] == "result" {
                        if let Some(piece) = claude_text.consume(&event) {
                            output.append(&piece);
                        }
                        if event["is_error"] == true {
                            return Err(event["result"]
                                .as_str()
                                .unwrap_or("Claude ended with an error")
                                .to_owned());
                        }
                        // A result ends one turn, not the session: background agents
                        // can trigger more turns and permission requests afterwards.
                        if background.running() {
                            continue;
                        }
                        if implement_after_result {
                            implement_after_result = false;
                            write_line(&mut stdin,&json!({"type":"control_request","request_id":"orb-execute","request":{"subtype":"set_permission_mode","mode":"bypassPermissions"}}).to_string())?;
                            write_line(&mut stdin,&json!({"type":"user","message":{"role":"user","content":"Implement the approved plan."}}).to_string())?;
                            continue;
                        }
                        break;
                    } else if let Some(piece) = claude_text.consume(&event) {
                        output.append(&piece);
                    }
                }
                Ok(())
            })();
            crate::interactions::finish(&mission_id);
            if let Err(e) = result {
                *failures.lock().unwrap() = Some(e);
            }
        });
        pipe_output(None, child.stderr.take(), text, error, true);
        return Ok(child);
    }
}

fn spawn_piped(
    request: &StartRequest,
    args: Vec<String>,
    text: &Arc<Output>,
    error: &Arc<Mutex<Option<String>>>,
    parse_json: bool,
) -> Result<Child, String> {
    let mut command = Command::new(&request.bin);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    // Share the user's provider credentials/config, but not a database whose
    // schema may belong to a different OpenCode build (e.g. the desktop app).
    if request.harness == "opencode" && std::env::var_os("OPENCODE_DB").is_none() {
        command.env("OPENCODE_DB", "orb-local.db");
    }
    if request.harness == "opencode" {
        command.env("OPENCODE_PERMISSION", r#"{"*":"allow"}"#);
    }
    command.env("NO_COLOR", "1");
    let mut child = command
        .current_dir(&request.cwd)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", request.harness))?;
    pipe_output(
        child.stdout.take(),
        child.stderr.take(),
        text,
        error,
        parse_json,
    );
    Ok(child)
}

fn stream_plain(reader: &mut impl Read, output: &Output) {
    let mut pending = Vec::new();
    let mut bytes = [0u8; 4096];
    while let Ok(n) = reader.read(&mut bytes) {
        if n == 0 {
            break;
        }
        pending.extend_from_slice(&bytes[..n]);
        loop {
            match std::str::from_utf8(&pending) {
                Ok(text) => {
                    output.append(text);
                    pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.append(std::str::from_utf8(&pending[..valid]).unwrap());
                    pending.drain(..valid);
                    if let Some(invalid) = error.error_len() {
                        output.append("�");
                        pending.drain(..invalid);
                    } else {
                        break;
                    }
                }
            }
        }
    }
    output.append(&String::from_utf8_lossy(&pending));
}

// Terminal styling is not meaningful inside a desktop error card.
fn strip_terminal_codes(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn pipe_output(
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    text: &Arc<Output>,
    error: &Arc<Mutex<Option<String>>>,
    parse_json: bool,
) {
    let text_out = Arc::clone(text);
    let error_out = Arc::clone(error);
    if let Some(stdout) = stdout {
        let guard = text.reader();
        thread::spawn(move || {
            let _guard = guard;
            let mut reader = BufReader::new(stdout);
            if !parse_json {
                stream_plain(&mut reader, &text_out);
                return;
            }
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let piece = if parse_json {
                    extract_text(&line)
                } else {
                    Some(line)
                };
                if let Some(piece) = piece.filter(|s| !s.is_empty()) {
                    text_out.append(&piece);
                }
            }
        });
    }
    if let Some(stderr) = stderr {
        let guard = text.reader();
        thread::spawn(move || {
            let _guard = guard;
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            let clean = strip_terminal_codes(&buf);
            let trimmed = clean.trim();
            if !trimmed.is_empty() {
                if let Ok(mut slot) = error_out.lock() {
                    if slot.is_none() {
                        *slot = Some(trimmed.chars().take(500).collect());
                    }
                }
            }
        });
    }
}

fn spawn_codex(
    request: &StartRequest,
    text: &Arc<Output>,
    session_out: &Arc<Mutex<Option<String>>>,
    error: &Arc<Mutex<Option<String>>>,
    done: &Arc<AtomicBool>,
) -> Result<Child, String> {
    let mut child = Command::new(&request.bin)
        .current_dir(&request.cwd)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start Codex: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("codex stdin missing")?;
    let stdout = child.stdout.take().ok_or("codex stdout missing")?;
    let stderr = child.stderr.take();
    let prompt = request.prompt.clone();
    let mission_id = crate::interactions::session(&request.id);
    let image_paths = request.image_paths.clone();
    let model = request.model.clone();
    let cwd = request.cwd.clone();
    let resume = request.session_id.clone().filter(|s| !s.is_empty());
    let text_bg = Arc::clone(text);
    let session_bg = Arc::clone(session_out);
    let error_bg = Arc::clone(error);
    let done_bg = Arc::clone(done);
    let stderr_error = Arc::clone(error);
    if let Some(stderr) = stderr {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            let clean = strip_terminal_codes(&buf);
            let trimmed = clean.trim();
            if !trimmed.is_empty() {
                if let Ok(mut slot) = stderr_error.lock() {
                    if slot.is_none() {
                        *slot = Some(trimmed.chars().take(500).collect());
                    }
                }
            }
        });
    }
    let guard = text.reader();
    thread::spawn(move || {
        let _guard = guard;
        let mut reader = BufReader::new(stdout);
        let result = drive_codex(
            &mut stdin,
            &mut reader,
            &prompt,
            &image_paths,
            model.as_deref(),
            &cwd,
            resume.as_deref(),
            &text_bg,
            &session_bg,
            &mission_id,
        );
        crate::interactions::finish(&mission_id);
        if let Err(message) = result {
            if let Ok(mut slot) = error_bg.lock() {
                *slot = Some(if busy_thread(&message) {
                    format!("Codex thread is busy: {message}")
                } else {
                    message
                });
            }
        }
        done_bg.store(true, Ordering::SeqCst);
    });
    Ok(child)
}

fn busy_thread(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("busy")
        || lower.contains("already")
        || lower.contains("in progress")
        || lower.contains("in use")
}

fn drive_codex(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    prompt: &str,
    image_paths: &[String],
    model: Option<&str>,
    cwd: &str,
    resume: Option<&str>,
    text: &Output,
    session_out: &Mutex<Option<String>>,
    mission_id: &crate::interactions::Session,
) -> Result<(), String> {
    rpc(
        stdin,
        reader,
        "initialize",
        json!({
            "clientInfo": {"name": "orb", "version": "0.1.0"},
            "capabilities": {"experimentalApi": true}
        }),
    )?;
    let _ = write_line(stdin, &json!({"method": "initialized"}).to_string());
    let params = json!({
        "model": model,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access"
    });
    let started = if let Some(thread_id) = resume {
        let mut resume_params = params.clone();
        resume_params["threadId"] = json!(thread_id);
        rpc(stdin, reader, "thread/resume", resume_params)?
    } else {
        rpc(stdin, reader, "thread/start", params)?
    };
    let resolved_model = model.or_else(|| started.get("model").and_then(Value::as_str));
    let thread_id = started
        .pointer("/thread/id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "codex did not return a thread id".to_string())?
        .to_string();
    if let Ok(mut slot) = session_out.lock() {
        *slot = Some(thread_id.clone());
    }
    let plan_prompt = prompt
        .trim()
        .strip_prefix("/plan")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
    let mut planning = plan_prompt.is_some();
    let mut input =
        vec![json!({"type":"text", "text":plan_prompt.map(str::trim).unwrap_or(prompt)})];
    input.extend(
        image_paths
            .iter()
            .map(|path| json!({"type":"localImage", "path":path})),
    );
    let mut pending = Vec::new();
    let _ = rpc_collect(
        stdin,
        reader,
        "turn/start",
        json!({
            "threadId": thread_id,
            "input": input,
            "collaborationMode": {"mode": if planning {"plan"} else {"default"}, "settings": {"model":resolved_model.ok_or("Codex did not resolve a model for collaboration mode")?, "reasoning_effort":null, "developer_instructions":null}}
        }),
        &mut pending,
    )?;
    let mut pending: std::collections::VecDeque<Value> = pending.into();
    let mut items = crate::local_stream::CodexText::default();
    let mut line = String::new();
    loop {
        let value = if let Some(event) = pending.pop_front() {
            event
        } else {
            line.clear();
            let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("Codex stream closed before the turn completed".into());
            }
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            value
        };
        if let Some(message) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return Err(message.to_string());
        }
        let method = value.get("method").and_then(|m| m.as_str()).unwrap_or("");
        if let Some(id) = value.get("id").filter(|_| !method.is_empty()) {
            if method == "item/tool/requestUserInput" || method == "tool/requestUserInput" {
                let answer =
                    crate::interactions::ask(mission_id, "questions", value["params"].clone())?;
                write_line(stdin, &json!({"id":id,"result":answer}).to_string())?;
            } else {
                // Never treat an unknown request as an approval.
                write_line(stdin, &json!({"id":id,"error":{"code":-32601,"message":"Unsupported interactive request"}}).to_string())?;
            }
            continue;
        }
        if method == "error" {
            if let Some(message) = value
                .pointer("/params/error/message")
                .and_then(Value::as_str)
            {
                return Err(message.to_string());
            }
        }
        if method == "turn/completed" || method == "turn/complete" {
            if value.pointer("/params/turn/status").and_then(Value::as_str) == Some("failed") {
                return Err(value
                    .pointer("/params/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed")
                    .to_owned());
            }
            if planning
                && value.pointer("/params/turn/status").and_then(Value::as_str)
                    != Some("interrupted")
            {
                let answer =
                    crate::interactions::ask(mission_id, "plan", json!({"plan":text.snapshot()}))?;
                planning = answer["action"] != "accept";
                let followup = if planning {
                    answer["feedback"].as_str().unwrap_or("Revise the plan.")
                } else {
                    "Implement the approved plan."
                };
                let mut early = Vec::new();
                let next = rpc_collect(
                    stdin,
                    reader,
                    "turn/start",
                    json!({"threadId":thread_id,"input":[{"type":"text","text":followup}],"collaborationMode":{"mode":if planning {"plan"} else {"default"},"settings":{"model":resolved_model.ok_or("Codex did not resolve a model for collaboration mode")?,"reasoning_effort":null,"developer_instructions":null}}}),
                    &mut early,
                );
                next?;
                pending.extend(early);
                continue;
            }
            break;
        }
        items.apply(&value, text);
    }
    Ok(())
}

fn rpc(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    rpc_collect(stdin, reader, method, params, &mut Vec::new())
}

fn rpc_collect(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    method: &str,
    params: Value,
    pending: &mut Vec<Value>,
) -> Result<Value, String> {
    let id = format!("orb-{method}");
    write_line(
        stdin,
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    )?;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("{method} closed the stream"));
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_str()) == Some(id.as_str())
            || value.get("id").is_some()
                && value.get("method").is_none()
                && value.get("result").is_some()
            || value.get("error").is_some() && value.get("method").is_none()
        {
            if let Some(message) = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return Err(format!("{method}: {message}"));
            }
            if value.get("id").and_then(|v| v.as_str()) == Some(id.as_str())
                || value.get("result").is_some()
            {
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        pending.push(value);
    }
}

fn write_line(stdin: &mut impl Write, line: &str) -> Result<(), String> {
    stdin
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin.write_all(b"\n").map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())
}

/// Prefer the CLI's authoritative live task snapshot; older CLIs expose edges.
#[derive(Default)]
struct ClaudeBackground {
    tasks: std::collections::HashSet<String>,
    has_snapshot: bool,
}
impl ClaudeBackground {
    fn consume(&mut self, event: &Value) {
        if event["type"] != "system" {
            return;
        }
        match event["subtype"].as_str() {
            Some("background_tasks_changed") => {
                if let Some(tasks) = event["tasks"].as_array() {
                    self.has_snapshot = true;
                    self.tasks = tasks
                        .iter()
                        .filter(|t| t["ambient"] != true)
                        .filter_map(|t| t["task_id"].as_str().map(str::to_owned))
                        .collect();
                }
            }
            Some("task_started") if !self.has_snapshot && event["ambient"] != true => {
                if let Some(id) = event["task_id"].as_str() {
                    self.tasks.insert(id.into());
                }
            }
            Some("task_notification") if !self.has_snapshot => {
                if let Some(id) = event["task_id"].as_str() {
                    self.tasks.remove(id);
                }
            }
            _ => {}
        }
    }
    fn running(&self) -> bool {
        !self.tasks.is_empty()
    }
}

/// Text deltas have no separator between independent Claude messages. Preserve
/// protocol boundaries without inserting whitespace between token fragments.
#[derive(Default)]
struct ClaudeText {
    emitted: bool,
    boundary: bool,
    message_emitted: bool,
    message_id: Option<String>,
}

impl ClaudeText {
    fn consume(&mut self, value: &Value) -> Option<String> {
        if value["type"] == "result" && value["is_error"] != true && !self.emitted {
            let text = value["result"]
                .as_str()
                .filter(|text| !text.trim().is_empty())?;
            self.emitted = true;
            return Some(text.to_owned());
        }
        let id = if value["type"] == "assistant" {
            value["message"]["id"].as_str()
        } else {
            value["event"]["message"]["id"].as_str()
        };
        if let Some(id) = id {
            if self.message_id.as_deref() != Some(id) {
                self.message_id = Some(id.to_owned());
                self.message_emitted = false;
            }
        }
        if value["type"] == "assistant" && !self.message_emitted {
            let text = value["message"]["content"]
                .as_array()?
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.is_empty() {
                return None;
            }
            let result = if self.emitted {
                format!("\n\n{text}")
            } else {
                text
            };
            self.emitted = true;
            self.message_emitted = true;
            self.boundary = false;
            return Some(result);
        }
        if value["type"] != "stream_event" {
            return None;
        }
        let event = &value["event"];
        match event["type"].as_str() {
            Some("message_start") => {
                self.boundary = self.emitted;
                self.message_emitted = false;
            }
            Some("content_block_start") if event["content_block"]["type"] == "text" => {
                self.boundary = self.emitted;
            }
            _ => {}
        }
        if event["delta"]["type"] != "text_delta" {
            return None;
        }
        let text = event["delta"]["text"].as_str()?;
        if text.is_empty() {
            return None;
        }
        let result = if self.boundary {
            format!("\n\n{text}")
        } else {
            text.to_owned()
        };
        self.boundary = false;
        self.emitted = true;
        self.message_emitted = true;
        Some(result)
    }
}

// Consume only protocol events representing new assistant output. Recursive
// text extraction also captures user items, tool arguments and final snapshots.
fn extract_text(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let text = if let Some(method) = value["method"].as_str() {
        match method {
            "item/agentMessage/delta" => value.pointer("/params/delta")?.as_str(),
            _ => None,
        }
    } else {
        match value["type"].as_str()? {
            "stream_event" if value.pointer("/event/delta/type")?.as_str()? == "text_delta" => {
                value.pointer("/event/delta/text")?.as_str()
            }
            "text" => value.pointer("/part/text")?.as_str(), // OpenCode
            "message" if value["role"] == "assistant" && value["delta"] == true => {
                value["content"].as_str()
            } // Gemini streaming output
            _ => None,
        }
    }?;
    (!text.is_empty()).then(|| text.to_owned())
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
        .chars()
        .take(32)
        .collect::<String>()
        .chars()
        .enumerate()
        .fold(String::new(), |mut acc, (i, ch)| {
            if i == 8 || i == 12 || i == 16 || i == 20 {
                acc.push('-');
            }
            acc.push(ch);
            acc
        })
}

fn which(name: &str) -> Option<PathBuf> {
    let output = Command::new("which").arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(text.lines().next()?.trim());
    path.is_file().then_some(path)
}

// Conservative floors: these are the versions exercised by the native
// round-trip tests, not merely a CLI binary being present on PATH.
fn native_plan_supported(id: &str, version: &str) -> bool {
    let minimum = match id {
        "codex" => (0, 155, 0),
        "claudecode" => (2, 1, 278),
        _ => return false,
    };
    version
        .split_whitespace()
        .find_map(|part| {
            let mut pieces = part.split('.');
            Some((
                pieces.next()?.parse::<u32>().ok()?,
                pieces.next()?.parse::<u32>().ok()?,
                pieces.next()?.parse::<u32>().ok()?,
            ))
        })
        .is_some_and(|v| v >= minimum)
}

fn version_of(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                if out.trim().is_empty() {
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut out);
                    }
                }
                return out
                    .lines()
                    .next()
                    .map(|line| line.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
            Ok(Some(_)) => return None,
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(_) => return None,
        }
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
    Ok(PathBuf::from(home).join(".orb").join("local-workspaces"))
}

fn safe_slug(slug: &str) -> Result<String, String> {
    let slug = slug.trim();
    if slug.is_empty() || slug.contains('/') || slug.contains("..") || slug.contains('\\') {
        return Err("project slug is not a single path segment".into());
    }
    Ok(slug.to_string())
}

pub(crate) fn safe_rel(rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() || rel.contains('\\') || rel.chars().any(char::is_control) {
        return Err("attachment path is required".into());
    }
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return Err("path must be relative with no '..' components".into()),
        }
    }
    Ok(out)
}

pub(crate) fn is_secret_path(rel: &str) -> bool {
    let lower = rel.replace('\\', "/").to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if lower.split('/').any(|part| {
        matches!(part, ".git" | ".ssh" | ".aws" | ".codex" | ".claude")
            || part == ".env"
            || part.starts_with(".env.")
    }) {
        return true;
    }
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == "id_rsa"
        || name.starts_with("id_rsa.")
        || name.starts_with("id_ed25519")
        || name.contains("credentials")
        || name == "auth.json"
        || name == "secrets"
        || name == "secrets.yaml"
        || name == "secrets.yml"
        || name == "secrets.json"
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_final_result_fills_missing_stream_without_duplicates() {
        let mut text = ClaudeText::default();
        let result = json!({"type":"result","is_error":false,"result":"Recovered final"});
        assert_eq!(text.consume(&result).as_deref(), Some("Recovered final"));
        assert!(text.consume(&result).is_none());
        let mut failed = ClaudeText::default();
        assert!(failed
            .consume(&json!({"type":"result","is_error":true,"result":"Rate limited"}))
            .is_none());
    }

    #[test]
    fn claude_snapshot_without_deltas_is_not_lost() {
        let mut text = ClaudeText::default();
        let event = json!({"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"Final answer"}]}});
        assert_eq!(text.consume(&event).as_deref(), Some("Final answer"));
        assert!(text.consume(&event).is_none());
        let next = json!({"type":"assistant","message":{"id":"b","content":[{"type":"text","text":"Next answer"}]}});
        assert_eq!(text.consume(&next).as_deref(), Some("\n\nNext answer"));
    }

    #[test]
    fn claude_text_preserves_tokens_and_separates_message_blocks() {
        let mut text = ClaudeText::default();
        let mut output = String::new();
        for event in [
            json!({"type":"message_start"}),
            json!({"type":"content_block_start","content_block":{"type":"text"}}),
            json!({"delta":{"type":"text_delta","text":"Bon"}}),
            json!({"delta":{"type":"text_delta","text":"jour."}}),
            json!({"type":"content_block_start","content_block":{"type":"tool_use"}}),
            json!({"type":"message_start"}),
            json!({"type":"content_block_start","content_block":{"type":"text"}}),
            json!({"delta":{"type":"text_delta","text":""}}),
            json!({"delta":{"type":"text_delta","text":"La suite."}}),
            json!({"type":"content_block_start","content_block":{"type":"text"}}),
            json!({"delta":{"type":"text_delta","text":"```rust\nfn main() {}\n```"}}),
        ] {
            if let Some(piece) = text.consume(&json!({"type":"stream_event","event":event})) {
                output.push_str(&piece);
            }
        }
        assert_eq!(
            output,
            "Bonjour.\n\nLa suite.\n\n```rust\nfn main() {}\n```"
        );
        assert!(text.consume(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"duplicate"}]}})).is_none());
    }

    #[test]
    fn rejects_parent_and_secret_paths() {
        assert!(safe_rel("../etc/passwd").is_err());
        assert!(safe_rel("/abs").is_err());
        assert!(is_secret_path(".env"));
        assert!(is_secret_path("notes/id_rsa"));
        assert!(!is_secret_path("notes/foo.md"));
        assert_eq!(
            safe_rel("notes/foo.md").unwrap(),
            PathBuf::from("notes/foo.md")
        );
    }

    #[test]
    fn native_plan_capability_requires_a_verified_protocol_version() {
        assert!(native_plan_supported("codex", "codex-cli 0.155.1"));
        assert!(native_plan_supported("claudecode", "2.1.278 (Claude Code)"));
        assert!(!native_plan_supported("codex", "codex-cli 0.120.0"));
        assert!(!native_plan_supported("claudecode", "unknown"));
        assert!(!native_plan_supported("grok", "1.0.40"));
        assert!(!native_plan_supported("opencode", "1.15.13"));
    }

    #[test]
    fn grok_and_opencode_args_match_the_pinned_flags() {
        assert_eq!(
            grok_args("hello"),
            vec![
                "--always-approve".to_string(),
                "--prompt".to_string(),
                "hello".to_string()
            ]
        );
        let fresh = StartRequest {
            image_paths: vec![],
            id: "1".into(),
            harness: "opencode".into(),
            bin: "opencode".into(),
            cwd: "/tmp".into(),
            prompt: "hi".into(),
            model: Some("xai/grok".into()),
            session_id: None,
        };
        assert_eq!(
            opencode_args(&fresh),
            vec!["run", "--format", "json", "--model", "xai/grok", "hi"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
        let smart = StartRequest {
            model: Some("builtin/smart".into()),
            ..fresh.clone()
        };
        assert!(opencode_args(&smart).contains(&"sandboxed-sh/builtin/smart".to_string()));
        let resumed = StartRequest {
            session_id: Some("ses_abc".into()),
            ..fresh
        };
        assert!(opencode_args(&resumed)
            .windows(2)
            .any(|pair| pair == ["--session".to_string(), "ses_abc".to_string()]));
    }

    #[test]
    fn output_excludes_user_echoes_snapshots_and_tools() {
        let events = [
            json!({"method":"item/started","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"PROMPT"}]}}}),
            json!({"method":"item/completed","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"PROMPT"}]}}}),
            json!({"method":"item/agentMessage/delta","params":{"delta":"Hello"}}),
            json!({"method":"item/completed","params":{"item":{"type":"agentMessage","text":"Hello"}}}),
            json!({"method":"item/commandExecution/outputDelta","params":{"delta":"TOOL"}}),
            json!({"type":"user","message":{"content":[{"type":"text","text":"PROMPT"}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}),
        ];
        let text: String = events
            .iter()
            .filter_map(|e| extract_text(&e.to_string()))
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(
            extract_text(r#"{"type":"text","part":{"text":"OpenCode"}}"#).as_deref(),
            Some("OpenCode")
        );
    }

    #[test]
    fn codex_keeps_early_deltas_and_reconciles_final_items() {
        let events = [
            json!({"id":"orb-initialize","result":{}}),
            json!({"id":"orb-thread/start","result":{"model":"test-model","thread":{"id":"thread"}}}),
            json!({"method":"item/agentMessage/delta","params":{"itemId":"a","delta":"Early"}}),
            json!({"id":"orb-turn/start","result":{}}),
            json!({"method":"item/completed","params":{"item":{"type":"agentMessage","id":"a","text":"Early answer"}}}),
            json!({"method":"turn/completed","params":{"turn":{"status":"completed"}}}),
        ];
        let input = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let output = Output::default();
        let mut requests = Vec::new();
        drive_codex(
            &mut requests,
            &mut std::io::Cursor::new(input),
            "prompt",
            &["/tmp/pasted.png".into()],
            None,
            "/tmp",
            None,
            &output,
            &Mutex::new(None),
            &crate::interactions::begin("test-codex"),
        )
        .unwrap();
        assert_eq!(output.snapshot(), "Early answer");
        let sent: Vec<Value> = String::from_utf8(requests)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let turn = sent
            .iter()
            .find(|event| event["method"] == "turn/start")
            .unwrap();
        assert_eq!(
            turn["params"]["input"][1],
            json!({"type":"localImage", "path":"/tmp/pasted.png"})
        );
    }

    #[test]
    fn plain_stream_does_not_wait_for_newlines_and_preserves_utf8() {
        struct Bytes<'a> {
            bytes: &'a [u8],
            output: &'a Output,
            index: usize,
        }
        impl Read for Bytes<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.index == 1 {
                    assert_eq!(self.output.snapshot(), "A");
                }
                if self.index == self.bytes.len() {
                    return Ok(0);
                }
                buf[0] = self.bytes[self.index];
                self.index += 1;
                Ok(1)
            }
        }
        let output = Output::default();
        stream_plain(
            &mut Bytes {
                bytes: "Aé🙂".as_bytes(),
                output: &output,
                index: 0,
            },
            &output,
        );
        assert_eq!(output.snapshot(), "Aé🙂");
    }

    #[cfg(unix)]
    #[test]
    fn argument_prompt_harnesses_receive_eof_on_stdin() {
        let request = StartRequest {
            id: "stdin-test".into(),
            harness: "opencode".into(),
            bin: "/bin/sh".into(),
            cwd: "/tmp".into(),
            prompt: "hello".into(),
            model: None,
            session_id: None,
            image_paths: vec![],
        };
        let output = Arc::new(Output::default());
        let mut child = spawn_piped(
            &request,
            vec!["-c".into(), "cat >/dev/null".into()],
            &output,
            &Arc::new(Mutex::new(None)),
            false,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("harness waited forever for stdin EOF");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_claude_turn_allows_tools_without_prompting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude-fixture");
        std::fs::write(&bin, r#"#!/bin/sh
read -r init
read -r prompt
printf '%s\n' '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"/outside/AGENTS.md"}}}'
read -r answer
printf '%s' "$answer" > answer.json
printf '%s\n' '{"type":"result"}'
"#).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let id = format!("claude-permission-{}", uuid_like());
        local_agents_start(StartRequest {
            id: id.clone(),
            harness: "claudecode".into(),
            bin: bin.to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: "Read the repository instructions".into(),
            model: None,
            session_id: Some("existing-session".into()),
            image_paths: vec![],
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !local_agents_poll(id.clone()).unwrap().done {
            assert!(crate::interactions::local_interaction(id.clone())
                .unwrap()
                .is_none());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let answer: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("answer.json")).unwrap())
                .unwrap();
        assert_eq!(answer["response"]["response"]["behavior"], "allow");
        runs().lock().unwrap().remove(&id);
    }

    #[test]
    fn claude_background_snapshots_replace_edges_and_ignore_ambient_tasks() {
        let mut state = ClaudeBackground::default();
        state.consume(&json!({"type":"system","subtype":"task_started","task_id":"old"}));
        assert!(state.running());
        state.consume(&json!({"type":"system","subtype":"task_notification","task_id":"old"}));
        assert!(!state.running());
        state.consume(&json!({"type":"system","subtype":"task_started","task_id":"stale"}));
        state.consume(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"watch","ambient":true}]}));
        assert!(!state.running());
        state.consume(&json!({"type":"system","subtype":"task_started","task_id":"stale"}));
        assert!(!state.running());
    }

    #[cfg(unix)]
    #[test]
    fn claude_background_result_keeps_question_and_plan_channel_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude-fixture");
        std::fs::write(&bin, r#"#!/bin/sh
read -r init
read -r prompt
printf '%s\n' '{"type":"system","subtype":"task_started","task_id":"a","description":"Explore"}'
printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a"}]}'
printf '%s\n' '{"type":"result","result":"Waiting for agent"}'
sleep 0.05
printf '%s\n' '{"type":"system","subtype":"task_notification","task_id":"a","status":"completed"}'
printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
printf '%s\n' '{"type":"control_request","request_id":"q","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[]}}}'
read -r answer || exit 21
printf '%s\n' '{"type":"control_request","request_id":"p","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"The plan"}}}'
read -r answer || exit 22
printf '%s\n' '{"type":"result"}'
read -r mode || exit 23
read -r execute || exit 24
printf '%s\n' '{"type":"assistant","message":{"id":"final","content":[{"type":"text","text":"Implemented"}]}}'
printf '%s\n' '{"type":"result"}'
"#).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let id = format!("claude-background-{}", uuid_like());
        local_agents_start(StartRequest {
            id: id.clone(),
            harness: "claudecode".into(),
            bin: bin.to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: "/plan Explore with background agents".into(),
            model: None,
            session_id: None,
            image_paths: vec![],
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut methods = Vec::new();
        loop {
            if let Some(request) = crate::interactions::local_interaction(id.clone()).unwrap() {
                methods.push(request.method.clone());
                crate::interactions::local_interaction_answer(
                    id.clone(),
                    request.id,
                    if request.method == "plan" {
                        json!({"action":"accept"})
                    } else {
                        json!({"answers":{}})
                    },
                )
                .unwrap();
            }
            let state = local_agents_poll(id.clone()).unwrap();
            if state.done {
                assert!(state.text.contains("Implemented"), "{}", state.text);
                break;
            }
            assert!(Instant::now() < deadline, "Claude did not finish");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(methods, vec!["claude_questions", "plan"]);
        runs().lock().unwrap().remove(&id);
    }

    #[test]
    fn pasted_image_bytes_are_written_without_text_conversion() {
        let root = std::env::temp_dir().join(format!("orb-image-{}", uuid_like()));
        std::fs::create_dir_all(&root).unwrap();
        let bytes = [137, 80, 78, 71, 0, 255];
        let result = local_agents_write(WriteRequest {
            root: root.to_string_lossy().into_owned(),
            files: vec![WriteFile {
                rel: ".paloma/images/test.png".into(),
                content: base64::engine::general_purpose::STANDARD.encode(bytes),
                encoding: Some("base64".into()),
            }],
        })
        .unwrap();
        assert!(result.skipped.is_empty());
        assert_eq!(
            std::fs::read(root.join(".paloma/images/test.png")).unwrap(),
            bytes
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn errors_do_not_include_terminal_color_sequences() {
        assert_eq!(
            strip_terminal_codes("\u{1b}[91m\u{1b}[1mError: \u{1b}[0mFailed query"),
            "Error: Failed query"
        );
    }

    #[cfg(unix)]
    #[test]
    fn completed_local_run_can_be_replaced_by_a_followup() {
        let request = StartRequest {
            image_paths: vec![],
            id: format!("followup-test-{}", uuid_like()),
            harness: "grok".into(),
            bin: "/usr/bin/true".into(),
            cwd: "/tmp".into(),
            prompt: "test".into(),
            model: None,
            session_id: None,
        };
        local_agents_start(request.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !local_agents_poll(request.id.clone()).unwrap().done {
            assert!(Instant::now() < deadline, "fixture did not complete");
            thread::sleep(Duration::from_millis(10));
        }
        local_agents_start(request.clone()).expect("a finished run must not block a new turn");
        local_agents_stop(request.id).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_transfer_heartbeat_cannot_stop_a_new_native_generation() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let id = format!("generation-test-{}", uuid_like());
        local_agents_start(StartRequest {
            id: id.clone(),
            harness: "grok".into(),
            bin: bin.to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: "test".into(),
            model: None,
            session_id: None,
            image_paths: vec![],
        })
        .unwrap();
        let generation = native_generation(&id).unwrap();
        stop_generation(&id, Some("old-generation")).unwrap();
        assert!(!local_agents_poll(id.clone()).unwrap().done);
        stop_generation(&id, Some(&generation)).unwrap();
        assert!(local_agents_poll(id.clone()).unwrap().done);
        runs().lock().unwrap().remove(&id);
    }

    #[test]
    fn extract_text_reads_claude_deltas() {
        let line = r#"{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"Hi"}}}"#;
        assert_eq!(extract_text(line).as_deref(), Some("Hi"));
    }
}

/// Only live harnesses launched by this Orb instance (not unrelated terminals).
pub fn active_pids() -> Vec<u32> {
    let Ok(all) = runs().lock() else {
        return Vec::new();
    };
    all.values()
        .filter(|r| !r.done.load(Ordering::SeqCst))
        .filter_map(|r| r.child.try_lock().ok().map(|c| c.id()))
        .collect()
}

#[cfg(test)]
mod plan_smoke {
    use super::*;
    #[test]
    #[ignore = "real authenticated CLI smoke test; ORB_PLAN_HARNESS and ORB_PLAN_BIN required"]
    fn native_plan_roundtrip() {
        let harness = std::env::var("ORB_PLAN_HARNESS").unwrap();
        let id = format!("plan-smoke-{}", uuid_like());
        let cwd = std::env::temp_dir().join(&id);
        std::fs::create_dir_all(&cwd).unwrap();
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = local_agents_stop(self.0.clone());
            }
        }
        let _cleanup = Cleanup(id.clone());
        local_agents_start(StartRequest{id:id.clone(),harness,bin:std::env::var("ORB_PLAN_BIN").unwrap(),cwd:cwd.to_string_lossy().into(),model:None,session_id:None,image_paths:vec![],prompt:"/plan Plan creating hello.txt containing hello. First ask me one question using your native question tool: should it say hello or bonjour? Then present a short plan for approval. Do not delegate. After approval implement it.".into()}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(150);
        let mut approved = false;
        let mut revised = std::env::var_os("ORB_PLAN_REVISE").is_none();
        let mut questions = 0;
        while Instant::now() < deadline {
            if let Some(req) = crate::interactions::local_interaction(id.clone()).unwrap() {
                eprintln!("native request: {}", req.method);
                assert!(
                    approved || !cwd.join("hello.txt").exists(),
                    "write before plan approval"
                );
                let answer = if req.method == "plan" {
                    if !revised {
                        revised = true;
                        json!({"action":"revise","feedback":"Revise the plan to explicitly verify the file contents after writing. Keep the content hello and ask for approval again."})
                    } else {
                        approved = true;
                        json!({"action":"accept"})
                    }
                } else if req.method == "permission" {
                    assert!(approved, "unexpected permission before plan approval");
                    json!({"action":"accept"})
                } else {
                    questions += 1;
                    let mut answers = serde_json::Map::new();
                    for q in req.params["questions"].as_array().unwrap() {
                        if req.method == "claude_questions" {
                            answers.insert(q["question"].as_str().unwrap().into(), json!("hello"));
                        } else {
                            answers.insert(
                                q["id"].as_str().unwrap().into(),
                                json!({"answers":["hello"]}),
                            );
                        }
                    }
                    json!({"answers":answers})
                };
                crate::interactions::local_interaction_answer(id.clone(), req.id, answer).unwrap();
            }
            let state = local_agents_poll(id.clone()).unwrap();
            if state.done {
                assert!(state.error.is_none(), "{:?}", state.error);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(approved, "no plan approval request");
        assert!(questions > 0, "no clarification request");
        assert!(
            cwd.join("hello.txt").exists(),
            "approved plan did not execute"
        );
    }
}

pub fn workspace_busy(root: &std::path::Path) -> Result<bool, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    Ok(runs()
        .lock()
        .map_err(|e| e.to_string())?
        .values()
        .any(|run| {
            !run.done.load(Ordering::SeqCst) && run.cwd.canonicalize().ok().as_ref() == Some(&root)
        }))
}

pub fn native_generation(id: &str) -> Option<String> {
    runs()
        .lock()
        .ok()?
        .get(id)
        .map(|run| run.generation.clone())
}
