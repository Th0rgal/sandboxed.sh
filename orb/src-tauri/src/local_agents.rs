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
    pub text: String,
    pub done: bool,
    pub exit_code: Option<i32>,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub resumed: bool,
}

#[derive(Clone)]
struct Run {
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
pub fn local_agents_scan(request: ScanRequest) -> Vec<ScanRow> {
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
    let child = spawn_harness(&request, &text, &session_id, &error, &done)?;
    let child = Arc::new(Mutex::new(child));
    watch_exit(
        Arc::clone(&child),
        Arc::clone(&done),
        Arc::clone(&exit_code),
        Arc::clone(&text),
    );
    map.insert(
        request.id,
        Run {
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
    let mut map = runs().lock().map_err(|e| e.to_string())?;
    if let Some(run) = map.remove(&id) {
        if let Ok(mut child) = run.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // A cancelled tool can leave an inherited pipe open. Do not block Stop.
        let deadline = Instant::now() + Duration::from_millis(500);
        while !run.text.drained() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
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
    vec!["--prompt".into(), prompt.to_string()]
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
    let mut cmd = Command::new(&request.bin);
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
        cmd.arg("--session-id").arg(sid);
    } else if let Ok(mut slot) = session_id.lock() {
        let fresh = uuid_like();
        cmd.arg("--session-id").arg(&fresh);
        *slot = Some(fresh);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start Claude Code: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = request.prompt.clone();
        thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
            let _ = stdin.flush();
        });
    }
    pipe_output(child.stdout.take(), child.stderr.take(), text, error, true);
    Ok(child)
}

fn spawn_piped(
    request: &StartRequest,
    args: Vec<String>,
    text: &Arc<Output>,
    error: &Arc<Mutex<Option<String>>>,
    parse_json: bool,
) -> Result<Child, String> {
    let mut command = Command::new(&request.bin);
    // Share the user's provider credentials/config, but not a database whose
    // schema may belong to a different OpenCode build (e.g. the desktop app).
    if request.harness == "opencode" && std::env::var_os("OPENCODE_DB").is_none() {
        command.env("OPENCODE_DB", "orb-local.db");
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
        );
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
    let thread_id = started
        .pointer("/thread/id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "codex did not return a thread id".to_string())?
        .to_string();
    if let Ok(mut slot) = session_out.lock() {
        *slot = Some(thread_id.clone());
    }
    let mut input = vec![json!({"type":"text", "text":prompt})];
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
            "input": input
        }),
        &mut pending,
    )?;
    let mut pending = pending.into_iter();
    let mut items = crate::local_stream::CodexText::default();
    let mut line = String::new();
    loop {
        let value = if let Some(event) = pending.next() {
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
    fn grok_and_opencode_args_match_the_pinned_flags() {
        assert_eq!(
            grok_args("hello"),
            vec!["--prompt".to_string(), "hello".to_string()]
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
            json!({"id":"orb-thread/start","result":{"thread":{"id":"thread"}}}),
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
