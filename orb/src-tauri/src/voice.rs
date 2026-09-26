//! Local voice input.
//!
//! Orb drives a persistent Python worker (`../../voice/worker.py`, embedded
//! into the binary and written to the voice home at spawn time) over stdio.
//! The worker loads the pinned MLX build of Cohere Transcribe once and stays
//! warm; this module owns its lifecycle: lazy start, one request at a time,
//! timeouts, cancellation, and an idle reaper that frees the ~1 GB the model
//! keeps resident on a 16 GB machine.
//!
//! Everything here compiles on every platform so the protocol logic is
//! checked on Linux CI; only macOS on Apple Silicon reports `supported`.
//!
//! Shared model: when the `voiced` daemon (github.com/Th0rgal/murmure) is
//! installed, Orb speaks the same protocol v1 over its Unix socket instead
//! of spawning a private worker, so Orb, Murmure and any other client share
//! one resident model. Default socket:
//! `~/Library/Application Support/md.thomas.voice/voiced.sock`
//! (`ORB_VOICE_SOCKET` overrides it; `off` disables). No socket → the
//! private worker below, unchanged.

use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const MODEL_REPO: &str = "MarkChen1214/cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit";
pub const MODEL_REVISION: &str = "553445e84959f9ec3fcd43443bce75ea05c400f3";
/// Languages the pinned checkpoint accepts. The model does not auto-detect;
/// the frontend always names one of these.
pub const SUPPORTED_LANGUAGES: [&str; 14] = [
    "en", "fr", "de", "es", "it", "pt", "nl", "pl", "el", "ar", "ja", "zh", "vi", "ko",
];
/// Longest utterance accepted end to end (the recorder auto-stops here too).
pub const MAX_SECONDS: f64 = 120.0;
/// Absolute cap on the request body regardless of what the header claims.
const HARD_CAP_BYTES: usize = 24 * 1024 * 1024;
/// Longest response line accepted from the worker. A transcript of a
/// two-minute utterance is a few kilobytes; anything near this is a broken
/// or hostile worker, not a result.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const PROTOCOL_VERSION: u64 = 1;
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
const LOAD_TIMEOUT: Duration = Duration::from_secs(240);
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);
const REAPER_TICK: Duration = Duration::from_secs(15);

const WORKER_PY: &str = include_str!("../../voice/worker.py");
const PATCH_PY: &str = include_str!("../../voice/mlx_audio_cohere_quant_patch.py");

#[derive(Debug, Clone, Serialize)]
pub struct VoiceError {
    pub code: String,
    pub message: String,
}

impl VoiceError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for VoiceError {
    fn from(e: std::io::Error) -> Self {
        VoiceError::new("io", e.to_string())
    }
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub supported: bool,
    pub reason: Option<String>,
    pub platform: &'static str,
    pub arch: &'static str,
    pub python: String,
    pub python_ready: bool,
    pub model_repo: &'static str,
    pub model_revision: &'static str,
    pub model_dir: String,
    pub model_ready: bool,
    /// "off" (no worker), "warm" (model resident), "busy" (request in flight).
    pub worker: &'static str,
    /// Connected to a shared engine, rather than an Orb-owned child.
    pub shared: bool,
    pub languages: Vec<&'static str>,
    pub max_seconds: f64,
    pub idle_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarmInfo {
    pub loaded: bool,
    pub load_secs: f64,
    pub warmup_secs: f64,
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    pub duration_secs: f64,
    pub infer_secs: f64,
    pub load_secs: f64,
}

// ---------------------------------------------------------------------------
// Paths

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub python: PathBuf,
    pub model_dir: PathBuf,
    pub runtime: PathBuf,
    pub log: PathBuf,
    /// Shared `voiced` socket, tried before spawning a private worker.
    pub socket: Option<PathBuf>,
}

impl Paths {
    pub fn discover() -> Paths {
        let env = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home_dir = env("HOME").unwrap_or_else(|| PathBuf::from("/"));
        let socket = match std::env::var("ORB_VOICE_SOCKET") {
            Ok(v) if v == "off" => None,
            Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
            _ => Some(home_dir.join("Library/Application Support/md.thomas.voice/voiced.sock")),
        };
        let mut paths = Self::from_env(
            home_dir,
            env("ORB_VOICE_HOME"),
            env("ORB_VOICE_PYTHON"),
            env("ORB_VOICE_MODEL_DIR"),
            env("HF_HUB_CACHE"),
            env("HF_HOME"),
        );
        paths.socket = socket;
        paths
    }

    fn from_env(
        home_dir: PathBuf,
        voice_home: Option<PathBuf>,
        python: Option<PathBuf>,
        model_dir: Option<PathBuf>,
        hf_hub_cache: Option<PathBuf>,
        hf_home: Option<PathBuf>,
    ) -> Paths {
        let home =
            voice_home.unwrap_or_else(|| home_dir.join("Library/Application Support/Orb/voice"));
        let python = python.unwrap_or_else(|| home.join(".venv/bin/python"));
        let model_dir = model_dir.unwrap_or_else(|| {
            let hub = hf_hub_cache.unwrap_or_else(|| {
                hf_home
                    .unwrap_or_else(|| home_dir.join(".cache/huggingface"))
                    .join("hub")
            });
            hub.join(format!("models--{}", MODEL_REPO.replace('/', "--")))
                .join("snapshots")
                .join(MODEL_REVISION)
        });
        Paths {
            runtime: home.join("runtime"),
            log: home.join("logs/worker.log"),
            socket: None,
            home,
            python,
            model_dir,
        }
    }

    /// The shared daemon's socket exists (launchd starts it on connect).
    pub fn shared_ready(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            self.socket
                .as_ref()
                .and_then(|s| s.metadata().ok())
                .is_some_and(|m| m.file_type().is_socket())
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub fn python_ready(&self) -> bool {
        self.python.is_file()
    }

    pub fn model_ready(&self) -> bool {
        self.model_dir.join("config.json").is_file()
            && self.model_dir.join("model.safetensors").is_file()
    }
}

// ---------------------------------------------------------------------------
// WAV validation

/// Duration in seconds of a PCM WAV blob, from its header only.
pub fn wav_duration(bytes: &[u8]) -> Result<f64, VoiceError> {
    let bad = |m: &str| VoiceError::new("bad_wav", m);
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data_len: Option<usize> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if size < 16 || body + 16 > bytes.len() {
                return Err(bad("truncated fmt chunk"));
            }
            fmt = Some((
                u16_at(body),
                u16_at(body + 2),
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            data_len = Some(size.min(bytes.len().saturating_sub(body)));
            break;
        }
        pos = body + size + (size & 1);
    }
    let (format, channels, rate, bits) = fmt.ok_or_else(|| bad("missing fmt chunk"))?;
    let data_len = data_len.ok_or_else(|| bad("missing data chunk"))?;
    if format != 1 || bits != 16 {
        return Err(bad("expected 16-bit PCM"));
    }
    if channels == 0 || rate == 0 {
        return Err(bad("invalid channel count or sample rate"));
    }
    let bytes_per_sec = rate as f64 * channels as f64 * 2.0;
    Ok(data_len as f64 / bytes_per_sec)
}

pub fn validate_language(language: &str) -> Result<&'static str, VoiceError> {
    SUPPORTED_LANGUAGES
        .iter()
        .copied()
        .find(|l| *l == language)
        .ok_or_else(|| {
            VoiceError::new(
                "unsupported_language",
                format!(
                    "{language:?} is not one of {}",
                    SUPPORTED_LANGUAGES.join(", ")
                ),
            )
        })
}

pub fn validate_audio(bytes: &[u8]) -> Result<f64, VoiceError> {
    if bytes.len() > HARD_CAP_BYTES {
        return Err(VoiceError::new("audio_too_long", "recording is too large"));
    }
    let secs = wav_duration(bytes)?;
    if secs > MAX_SECONDS {
        return Err(VoiceError::new(
            "audio_too_long",
            format!("{secs:.0}s exceeds the {MAX_SECONDS:.0}s limit"),
        ));
    }
    Ok(secs)
}

// ---------------------------------------------------------------------------
// Worker process

/// What a worker connection runs on: a private child process, or a
/// connection to the shared `voiced` daemon.
enum Handle {
    Child(Child),
    #[cfg(unix)]
    Shared {
        stream: UnixStream,
        pid: Option<u32>,
    },
}

impl Handle {
    /// Unblock pending I/O. For a child: kill it. For the shared daemon:
    /// close only this connection; the daemon and its model stay up for the
    /// other clients, and a result still in flight is discarded.
    fn kill(&mut self) {
        match self {
            Handle::Child(c) => {
                let _ = c.kill();
            }
            #[cfg(unix)]
            Handle::Shared { stream, .. } => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn reap(&mut self) {
        self.kill();
        if let Handle::Child(c) = self {
            let _ = c.wait();
        }
    }

    fn is_shared(&self) -> bool {
        match self {
            Self::Child(_) => false,
            #[cfg(unix)]
            Self::Shared { .. } => true,
        }
    }

    fn id(&self) -> Option<u32> {
        match self {
            Handle::Child(c) => Some(c.id()),
            #[cfg(unix)]
            Handle::Shared { pid, .. } => *pid,
        }
    }
}

struct Worker {
    child: Arc<Mutex<Handle>>,
    stdin: Box<dyn Write + Send>,
    stdout: BufReader<Box<dyn Read + Send>>,
    next_id: u64,
    loaded: bool,
}

fn parse_response(line: &str, expect_id: u64) -> Result<Value, VoiceError> {
    let v: Value = serde_json::from_str(line)
        .map_err(|e| VoiceError::new("protocol", format!("malformed worker response: {e}")))?;
    if v.get("id").and_then(Value::as_u64) != Some(expect_id) {
        return Err(VoiceError::new("protocol", "worker response id mismatch"));
    }
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    let err = v.get("error").cloned().unwrap_or(Value::Null);
    Err(VoiceError::new(
        err.get("code")
            .and_then(Value::as_str)
            .unwrap_or("internal"),
        err.get("message")
            .and_then(Value::as_str)
            .unwrap_or("worker error")
            .to_string(),
    ))
}

impl Worker {
    fn spawn(paths: &Paths) -> Result<Worker, VoiceError> {
        std::fs::create_dir_all(&paths.runtime)?;
        std::fs::write(paths.runtime.join("worker.py"), WORKER_PY)?;
        std::fs::write(
            paths.runtime.join("mlx_audio_cohere_quant_patch.py"),
            PATCH_PY,
        )?;
        if let Some(dir) = paths.log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = std::fs::File::create(&paths.log)?;
        let child = Command::new(&paths.python)
            .arg(paths.runtime.join("worker.py"))
            .env("ORB_VOICE_MODEL_DIR", &paths.model_dir)
            .env("ORB_VOICE_MAX_SECS", format!("{MAX_SECONDS}"))
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| {
                VoiceError::new(
                    "python_missing",
                    format!("cannot start {}: {e}", paths.python.display()),
                )
            })?;
        let mut w = Worker::attach(child)?;
        w.handshake(HELLO_TIMEOUT)?;
        Ok(w)
    }

    fn handshake(&mut self, timeout: Duration) -> Result<Value, VoiceError> {
        let hello = self.request(json!({"op": "hello"}), None, timeout)?;
        if hello.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
            self.kill();
            return Err(VoiceError::new(
                "protocol",
                "voice worker protocol mismatch",
            ));
        }
        Ok(hello)
    }

    /// Connect to the shared `voiced` daemon. `None` when it is not
    /// installed or not answering, so the caller falls back to a private
    /// worker.
    #[cfg(unix)]
    fn connect_shared(
        paths: &Paths,
        connected: impl FnOnce(&Arc<Mutex<Handle>>),
    ) -> Option<Worker> {
        let socket = paths.socket.as_ref().filter(|s| s.exists())?;
        let stream = UnixStream::connect(socket).ok()?;
        let reader = stream.try_clone().ok()?;
        let writer = stream.try_clone().ok()?;
        let mut w = Worker {
            child: Arc::new(Mutex::new(Handle::Shared { stream, pid: None })),
            stdin: Box::new(writer),
            stdout: BufReader::new(Box::new(reader)),
            next_id: 0,
            loaded: false,
        };
        // Publish before hello so cancel can interrupt a queued handshake.
        connected(&w.child);
        // voiced serializes hello behind other clients' load/inference jobs.
        let hello = w.handshake(LOAD_TIMEOUT).ok()?;
        if hello.get("shared").and_then(Value::as_bool) != Some(true)
            || hello.get("daemon").and_then(Value::as_str) != Some("voiced")
            || hello.get("model_repo").and_then(Value::as_str) != Some(MODEL_REPO)
            || hello.get("model_revision").and_then(Value::as_str) != Some(MODEL_REVISION)
        {
            return None;
        }
        w.loaded = hello
            .get("loaded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let (Some(pid), Ok(mut h)) = (hello.get("pid").and_then(Value::as_u64), w.child.lock()) {
            if let Handle::Shared { pid: slot, .. } = &mut *h {
                *slot = u32::try_from(pid).ok().filter(|pid| *pid > 0);
            }
        }
        Some(w)
    }

    #[cfg(not(unix))]
    fn connect_shared(_: &Paths, _: impl FnOnce(&Arc<Mutex<Handle>>)) -> Option<Worker> {
        None
    }

    /// Wrap an already-spawned child whose stdin/stdout are piped.
    fn attach(mut child: Child) -> Result<Worker, VoiceError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| VoiceError::new("io", "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| VoiceError::new("io", "no stdout"))?;
        Ok(Worker {
            child: Arc::new(Mutex::new(Handle::Child(child))),
            stdin: Box::new(stdin),
            stdout: BufReader::new(Box::new(stdout)),
            next_id: 0,
            loaded: false,
        })
    }

    /// Send one frame and wait for its reply, all under one deadline.
    ///
    /// The watchdog is armed before the first byte is written: a worker that
    /// stops draining its stdin would otherwise block `write_all` on a full
    /// pipe forever, with no timer running. At the deadline it kills the
    /// child, which fails the pending write (EPIPE) or read (EOF). The
    /// response line is bounded too, so a runaway worker cannot make this
    /// side allocate without limit.
    fn request(
        &mut self,
        mut req: Value,
        payload: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Value, VoiceError> {
        self.next_id += 1;
        let id = self.next_id;
        req["id"] = json!(id);
        if let Some(p) = payload {
            req["audio_bytes"] = json!(p.len());
        }
        let mut line =
            serde_json::to_string(&req).map_err(|e| VoiceError::new("internal", e.to_string()))?;
        line.push('\n');

        let watchdog = Watchdog::arm(self.child.clone(), timeout);
        let stdin = &mut self.stdin;
        let stdout = &mut self.stdout;
        let io = (move || -> std::io::Result<Option<String>> {
            stdin.write_all(line.as_bytes())?;
            if let Some(p) = payload {
                stdin.write_all(p)?;
            }
            stdin.flush()?;
            let mut buf = String::new();
            let n = stdout.take(MAX_RESPONSE_BYTES).read_line(&mut buf)?;
            Ok((n > 0).then_some(buf))
        })();
        let fired = watchdog.disarm();

        match io {
            Ok(Some(buf)) if buf.ends_with('\n') => parse_response(buf.trim_end(), id),
            Ok(Some(_)) => {
                // Hit MAX_RESPONSE_BYTES without a newline: not a frame.
                self.kill();
                Err(VoiceError::new(
                    "protocol",
                    "voice worker response exceeded the size limit",
                ))
            }
            Ok(None) | Err(_) => {
                if fired {
                    Err(VoiceError::new(
                        "timeout",
                        format!("voice worker took longer than {}s", timeout.as_secs()),
                    ))
                } else {
                    Err(VoiceError::new(
                        "worker_exited",
                        "voice worker exited unexpectedly (see logs/worker.log)",
                    ))
                }
            }
        }
    }

    fn kill(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            c.reap();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Deadline for one request. Kills the child when it expires unless
/// disarmed first; dropping it (including on an early `?` return or a
/// panic) always stops the timer thread.
struct Watchdog {
    done: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(child: Arc<Mutex<Handle>>, timeout: Duration) -> Watchdog {
        let done = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        {
            let done = done.clone();
            let fired = fired.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + timeout;
                while !done.load(Ordering::Acquire) {
                    if Instant::now() >= deadline {
                        fired.store(true, Ordering::Release);
                        if let Ok(mut c) = child.lock() {
                            c.kill();
                        }
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            });
        }
        Watchdog { done, fired }
    }

    /// Stop the timer and report whether the deadline had already passed.
    fn disarm(self) -> bool {
        self.done.store(true, Ordering::Release);
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Shared state

pub struct Shared {
    paths: Paths,
    idle: Duration,
    /// Serializes every worker interaction, hence inference.
    worker: Mutex<Option<Worker>>,
    /// Handle to the live child for cancel while `worker` is locked by a request.
    child: Mutex<Option<Arc<Mutex<Handle>>>>,
    busy: AtomicBool,
    cancelled: AtomicBool,
    loaded: AtomicBool,
    last_used: Mutex<Instant>,
}

#[derive(Clone)]
pub struct VoiceState(pub Arc<Shared>);

impl Default for VoiceState {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceState {
    pub fn worker_pid(&self) -> Option<u32> {
        self.0
            .child
            .try_lock()
            .ok()?
            .as_ref()?
            .try_lock()
            .ok()
            .and_then(|c| c.id())
    }

    pub fn worker_shared(&self) -> bool {
        self.0
            .child
            .try_lock()
            .ok()
            .and_then(|handle| handle.as_ref()?.try_lock().ok().map(|h| h.is_shared()))
            .unwrap_or(false)
    }

    pub fn new() -> Self {
        let idle = std::env::var("ORB_VOICE_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE);
        Self::with_paths(Paths::discover(), idle)
    }

    pub fn with_paths(paths: Paths, idle: Duration) -> Self {
        VoiceState(Arc::new(Shared {
            paths,
            idle,
            worker: Mutex::new(None),
            child: Mutex::new(None),
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            last_used: Mutex::new(Instant::now()),
        }))
    }

    /// Background thread that drops the worker after `idle` without use.
    pub fn start_idle_reaper(&self) {
        let shared = self.0.clone();
        thread::Builder::new()
            .name("orb-voice-reaper".into())
            .spawn(move || loop {
                thread::sleep(REAPER_TICK);
                if shared.busy.load(Ordering::Acquire) {
                    continue;
                }
                let idle_for = shared
                    .last_used
                    .lock()
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                if idle_for >= shared.idle {
                    shared.release();
                }
            })
            .ok();
    }
}

fn supported() -> (bool, Option<String>) {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        (true, None)
    } else {
        (
            false,
            Some("Local voice input runs on macOS with Apple Silicon only.".into()),
        )
    }
}

impl Shared {
    pub fn capability(&self) -> Capability {
        let (ok, reason) = supported();
        let worker = if self.busy.load(Ordering::Acquire) {
            "busy"
        } else if self.loaded.load(Ordering::Acquire) {
            "warm"
        } else {
            "off"
        };
        Capability {
            supported: ok,
            reason,
            platform: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            python: self.paths.python.display().to_string(),
            python_ready: self.paths.python_ready() || self.paths.shared_ready(),
            model_repo: MODEL_REPO,
            model_revision: MODEL_REVISION,
            model_dir: self.paths.model_dir.display().to_string(),
            model_ready: self.paths.model_ready() || self.paths.shared_ready(),
            worker,
            shared: self
                .child
                .try_lock()
                .ok()
                .and_then(|c| c.as_ref()?.try_lock().ok().map(|h| h.is_shared()))
                .unwrap_or(false),
            languages: SUPPORTED_LANGUAGES.to_vec(),
            max_seconds: MAX_SECONDS,
            idle_seconds: self.idle.as_secs(),
        }
    }

    fn preflight(&self) -> Result<(), VoiceError> {
        let (ok, reason) = supported();
        if !ok {
            return Err(VoiceError::new("unsupported", reason.unwrap_or_default()));
        }
        if !self.paths.python_ready() {
            return Err(VoiceError::new(
                "python_missing",
                format!(
                    "voice runtime not installed ({}). Run orb/voice/install.sh.",
                    self.paths.python.display()
                ),
            ));
        }
        if !self.paths.model_ready() {
            return Err(VoiceError::new(
                "model_missing",
                format!(
                    "model snapshot not found at {}. Run orb/voice/install.sh --download-model.",
                    self.paths.model_dir.display()
                ),
            ));
        }
        Ok(())
    }

    fn ensure<'a>(&self, slot: &'a mut Option<Worker>) -> Result<&'a mut Worker, VoiceError> {
        if slot.is_none() {
            let w = match Worker::connect_shared(&self.paths, |handle| {
                if let Ok(mut child) = self.child.lock() {
                    *child = Some(handle.clone());
                }
                if self.cancelled.load(Ordering::Acquire) {
                    if let Ok(mut handle) = handle.lock() {
                        handle.kill();
                    }
                }
            }) {
                Some(w) => w,
                None => {
                    if self.cancelled.load(Ordering::Acquire) {
                        return Err(VoiceError::new("cancelled", "Voice request cancelled"));
                    }
                    if let Ok(mut child) = self.child.lock() {
                        *child = None;
                    }
                    self.preflight()?;
                    Worker::spawn(&self.paths)?
                }
            };
            if let Ok(mut c) = self.child.lock() {
                *c = Some(w.child.clone());
            }
            *slot = Some(w);
        }
        Ok(slot.as_mut().expect("worker present"))
    }

    fn touch(&self) {
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }
    }

    /// Run `f` against the (lazily started) worker with the busy flag set.
    /// Any error tears the worker down so the next call starts clean.
    fn with_worker<T>(
        &self,
        f: impl FnOnce(&mut Worker) -> Result<T, VoiceError>,
    ) -> Result<T, VoiceError> {
        let mut slot = self
            .worker
            .lock()
            .map_err(|_| VoiceError::new("internal", "voice state poisoned"))?;
        self.cancelled.store(false, Ordering::Release);
        self.busy.store(true, Ordering::Release);
        self.touch();
        let result = self.ensure(&mut slot).and_then(|w| {
            let r = f(w);
            if r.is_ok() && w.loaded {
                self.loaded.store(true, Ordering::Release);
            }
            r
        });
        if result.is_err() {
            *slot = None;
            self.loaded.store(false, Ordering::Release);
            if let Ok(mut c) = self.child.lock() {
                *c = None;
            }
        }
        self.busy.store(false, Ordering::Release);
        self.touch();
        result
    }

    pub fn prewarm(&self) -> Result<WarmInfo, VoiceError> {
        self.with_worker(|w| {
            let r = w.request(json!({"op": "load"}), None, LOAD_TIMEOUT)?;
            w.loaded = true;
            Ok(WarmInfo {
                loaded: true,
                load_secs: r.get("load_secs").and_then(Value::as_f64).unwrap_or(0.0),
                warmup_secs: r.get("warmup_secs").and_then(Value::as_f64).unwrap_or(0.0),
                cached: r.get("cached").and_then(Value::as_bool).unwrap_or(false),
            })
        })
    }

    pub fn transcribe(&self, wav: Vec<u8>, language: &str) -> Result<Transcript, VoiceError> {
        let language = validate_language(language)?;
        validate_audio(&wav)?;
        self.with_worker(|w| {
            let timeout = if w.loaded {
                TRANSCRIBE_TIMEOUT
            } else {
                LOAD_TIMEOUT
            };
            let r = w.request(
                json!({"op": "transcribe", "language": language, "punctuation": true}),
                Some(&wav),
                timeout,
            )?;
            w.loaded = true;
            Ok(Transcript {
                text: r
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                language: language.to_string(),
                duration_secs: r
                    .get("duration_secs")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                infer_secs: r.get("infer_secs").and_then(Value::as_f64).unwrap_or(0.0),
                load_secs: r.get("load_secs").and_then(Value::as_f64).unwrap_or(0.0),
            })
        })
    }

    /// Abort this client's I/O. A private worker is killed; a shared daemon
    /// only loses this connection. The next request reconnects.
    pub fn cancel(&self) -> bool {
        if !self.busy.load(Ordering::Acquire) {
            return false;
        }
        self.cancelled.store(true, Ordering::Release);
        if let Ok(c) = self.child.lock() {
            if let Some(child) = c.as_ref() {
                if let Ok(mut child) = child.lock() {
                    child.kill();
                    return true;
                }
            }
        }
        false
    }

    /// Release an idle worker. Shared engine residency belongs to voiced.
    pub fn release(&self) -> bool {
        if let Ok(mut slot) = self.worker.try_lock() {
            let had = slot.take().is_some();
            self.loaded.store(false, Ordering::Release);
            if let Ok(mut c) = self.child.lock() {
                *c = None;
            }
            had
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, VoiceError> + Send + 'static,
) -> Result<T, VoiceError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| VoiceError::new("internal", e.to_string()))?
}

#[tauri::command]
pub fn voice_capability(state: tauri::State<'_, VoiceState>) -> Capability {
    state.0.capability()
}

#[tauri::command]
pub async fn voice_prewarm(state: tauri::State<'_, VoiceState>) -> Result<WarmInfo, VoiceError> {
    let shared = state.0.clone();
    blocking(move || shared.prewarm()).await
}

/// Body: raw PCM WAV bytes (16 kHz mono 16-bit from the recorder).
/// Header `x-language`: one of [`SUPPORTED_LANGUAGES`], defaults to `en`.
#[tauri::command]
pub async fn voice_transcribe(
    request: tauri::ipc::Request<'_>,
    state: tauri::State<'_, VoiceState>,
) -> Result<Transcript, VoiceError> {
    let wav = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        tauri::ipc::InvokeBody::Json(_) => {
            return Err(VoiceError::new(
                "bad_request",
                "voice_transcribe expects a raw WAV body",
            ));
        }
    };
    let language = request
        .headers()
        .get("x-language")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("en")
        .to_string();
    let shared = state.0.clone();
    blocking(move || shared.transcribe(wav, &language)).await
}

#[tauri::command]
pub fn voice_cancel(state: tauri::State<'_, VoiceState>) -> bool {
    state.0.cancel()
}

#[tauri::command]
pub fn voice_release(state: tauri::State<'_, VoiceState>) -> bool {
    state.0.release()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, bits: u16, data_len: u32, format: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&format.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        v.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    #[test]
    fn wav_duration_reads_header() {
        assert_eq!(wav_duration(&wav(16000, 1, 16, 32000, 1)).unwrap(), 1.0);
        assert_eq!(
            wav_duration(&wav(48000, 2, 16, 48000 * 4 * 3, 1)).unwrap(),
            3.0
        );
    }

    #[test]
    fn wav_duration_skips_unknown_chunks() {
        let mut v = wav(16000, 1, 16, 0, 1);
        // Insert a LIST chunk (odd size, padded) between fmt and data.
        let data_at = v.len() - 8;
        let mut list = Vec::new();
        list.extend_from_slice(b"LIST");
        list.extend_from_slice(&3u32.to_le_bytes());
        list.extend_from_slice(&[1, 2, 3, 0]);
        let tail = v.split_off(data_at);
        v.extend_from_slice(&list);
        v.extend_from_slice(&tail);
        v.truncate(v.len() - 4);
        v.extend_from_slice(&16000u32.to_le_bytes());
        v.resize(v.len() + 16000, 0);
        assert_eq!(wav_duration(&v).unwrap(), 0.5);
    }

    #[test]
    fn wav_duration_rejects_bad_input() {
        assert_eq!(wav_duration(b"nope").unwrap_err().code, "bad_wav");
        assert_eq!(
            wav_duration(&wav(16000, 1, 8, 100, 1)).unwrap_err().code,
            "bad_wav"
        );
        assert_eq!(
            wav_duration(&wav(16000, 1, 16, 100, 3)).unwrap_err().code,
            "bad_wav"
        );
        assert_eq!(
            wav_duration(&wav(0, 1, 16, 100, 1)).unwrap_err().code,
            "bad_wav"
        );
    }

    #[test]
    fn validate_audio_enforces_duration_bound() {
        assert!(validate_audio(&wav(16000, 1, 16, 32000 * 120, 1)).is_ok());
        let e = validate_audio(&wav(16000, 1, 16, 32000 * 121, 1)).unwrap_err();
        assert_eq!(e.code, "audio_too_long");
    }

    #[test]
    fn languages_are_the_checkpoint_list() {
        assert_eq!(validate_language("fr").unwrap(), "fr");
        assert_eq!(
            validate_language("xx").unwrap_err().code,
            "unsupported_language"
        );
        assert_eq!(
            validate_language("EN").unwrap_err().code,
            "unsupported_language"
        );
        assert_eq!(SUPPORTED_LANGUAGES.len(), 14);
    }

    #[test]
    fn parse_response_handles_ok_and_error_frames() {
        let ok = parse_response(r#"{"id":3,"ok":true,"result":{"text":"hi"}}"#, 3).unwrap();
        assert_eq!(ok["text"], "hi");
        let err = parse_response(
            r#"{"id":3,"ok":false,"error":{"code":"model_missing","message":"m"}}"#,
            3,
        )
        .unwrap_err();
        assert_eq!(err.code, "model_missing");
        assert_eq!(
            parse_response(r#"{"id":4,"ok":true}"#, 3).unwrap_err().code,
            "protocol"
        );
        assert_eq!(parse_response("garbage", 3).unwrap_err().code, "protocol");
    }

    #[test]
    fn paths_follow_env_and_hf_cache_layout() {
        let p = Paths::from_env(PathBuf::from("/Users/t"), None, None, None, None, None);
        assert_eq!(
            p.home,
            PathBuf::from("/Users/t/Library/Application Support/Orb/voice")
        );
        assert_eq!(
            p.python,
            PathBuf::from("/Users/t/Library/Application Support/Orb/voice/.venv/bin/python")
        );
        assert_eq!(
            p.model_dir,
            PathBuf::from(
                "/Users/t/.cache/huggingface/hub/models--MarkChen1214--cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit/snapshots/553445e84959f9ec3fcd43443bce75ea05c400f3"
            )
        );
        let p = Paths::from_env(
            PathBuf::from("/Users/t"),
            Some(PathBuf::from("/tmp/vh")),
            None,
            None,
            None,
            Some(PathBuf::from("/tmp/hf")),
        );
        assert_eq!(p.python, PathBuf::from("/tmp/vh/.venv/bin/python"));
        assert!(p
            .model_dir
            .to_string_lossy()
            .starts_with("/tmp/hf/hub/models--MarkChen1214--"));
        assert_eq!(p.log, PathBuf::from("/tmp/vh/logs/worker.log"));
    }

    #[test]
    fn capability_is_honest_off_mac() {
        let state = VoiceState::with_paths(
            Paths::from_env(PathBuf::from("/nonexistent"), None, None, None, None, None),
            Duration::from_secs(1),
        );
        let cap = state.0.capability();
        assert_eq!(
            cap.supported,
            cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")
        );
        assert!(!cap.python_ready);
        assert!(!cap.model_ready);
        assert_eq!(cap.worker, "off");
        assert_eq!(cap.languages.len(), 14);
        let err = state
            .0
            .transcribe(wav(16000, 1, 16, 3200, 1), "en")
            .unwrap_err();
        assert!(
            ["unsupported", "python_missing"].contains(&err.code.as_str()),
            "{err}"
        );
        assert!(!state.0.cancel());
        assert!(!state.0.release());
    }

    fn find_python() -> Option<PathBuf> {
        ["python3", "python"].iter().find_map(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|_| PathBuf::from(p))
        })
    }

    /// A stand-in child running the given Python snippet with piped stdio.
    fn fake_child(python: &PathBuf, code: &str) -> Worker {
        let child = Command::new(python)
            .arg("-c")
            .arg(code)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fake child");
        Worker::attach(child).expect("attach")
    }

    /// A worker that never reads its stdin must not hang the request: the
    /// payload is far larger than any pipe buffer, so without the watchdog
    /// armed before the writes `write_all` would block forever.
    #[test]
    fn request_times_out_when_worker_never_drains_stdin() {
        let Some(python) = find_python() else {
            eprintln!("skipping: no python on PATH");
            return;
        };
        let mut w = fake_child(&python, "import time; time.sleep(60)");
        let payload = vec![0u8; 8 * 1024 * 1024];
        let t0 = Instant::now();
        let e = w
            .request(
                json!({"op": "transcribe", "language": "en"}),
                Some(&payload),
                Duration::from_millis(500),
            )
            .unwrap_err();
        assert_eq!(e.code, "timeout", "{e}");
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
        // The deadline killed the child; a later call sees it gone.
        let e = w
            .request(json!({"op": "status"}), None, Duration::from_secs(5))
            .unwrap_err();
        assert!(
            ["worker_exited", "io", "timeout"].contains(&e.code.as_str()),
            "{e}"
        );
    }

    /// A worker that answers with an endless line is cut off at
    /// MAX_RESPONSE_BYTES instead of growing the buffer without bound.
    #[test]
    fn request_rejects_oversized_response_line() {
        let Some(python) = find_python() else {
            eprintln!("skipping: no python on PATH");
            return;
        };
        let mut w = fake_child(
            &python,
            "import sys; sys.stdin.readline(); sys.stdout.write('x' * (3 * 1024 * 1024)); sys.stdout.flush(); sys.stdin.read()",
        );
        let e = w
            .request(json!({"op": "status"}), None, Duration::from_secs(20))
            .unwrap_err();
        assert_eq!(e.code, "protocol", "{e}");
    }

    /// A well-behaved child that replies before the deadline is unaffected
    /// by the watchdog, and the watchdog is disarmed afterwards.
    #[test]
    fn request_disarms_watchdog_after_a_prompt_reply() {
        let Some(python) = find_python() else {
            eprintln!("skipping: no python on PATH");
            return;
        };
        let mut w = fake_child(
            &python,
            "import sys, json\nfor line in sys.stdin:\n    req = json.loads(line)\n    sys.stdout.write(json.dumps({'id': req['id'], 'ok': True, 'result': {'echo': req['op']}}) + '\\n'); sys.stdout.flush()",
        );
        let r = w
            .request(json!({"op": "ping"}), None, Duration::from_millis(300))
            .unwrap();
        assert_eq!(r["echo"], "ping");
        thread::sleep(Duration::from_millis(500)); // past the old deadline: child must still be alive
        let r = w
            .request(json!({"op": "again"}), None, Duration::from_secs(5))
            .unwrap();
        assert_eq!(r["echo"], "again");
    }

    /// With a `voiced` socket present, requests go to the shared daemon: no
    /// Python, venv or model is needed on Orb's side, the daemon pid is
    /// reported for metrics, and releasing only drops the connection.
    #[cfg(unix)]
    #[test]
    fn shared_daemon_is_preferred_over_a_private_worker() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("orb-voiced-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("voiced.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut ops = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return ops;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let n = req["audio_bytes"].as_u64().unwrap_or(0) as usize;
                let mut body = vec![0u8; n];
                reader.read_exact(&mut body).unwrap();
                let op = req["op"].as_str().unwrap().to_string();
                let result = match op.as_str() {
                    "hello" => {
                        json!({"protocol": 1, "shared": true, "daemon": "voiced", "model_repo": MODEL_REPO, "model_revision": MODEL_REVISION, "pid": 4242, "loaded": true})
                    }
                    "transcribe" => {
                        json!({"text": "shared", "duration_secs": wav_duration(&body).unwrap()})
                    }
                    _ => json!({}),
                };
                ops.push(op);
                let resp = json!({"id": req["id"], "ok": true, "result": result});
                writeln!(writer, "{resp}").unwrap();
            }
        });
        let mut paths = Paths::from_env(
            dir.clone(),
            Some(dir.clone()),
            Some(dir.join("no-python")),
            Some(dir.join("no-model")),
            None,
            None,
        );
        paths.socket = Some(sock);
        assert!(!paths.python_ready());
        let state = VoiceState::with_paths(paths, DEFAULT_IDLE);
        let cap = state.0.capability();
        assert!(cap.python_ready && cap.model_ready);
        let t = state
            .0
            .transcribe(wav(16000, 1, 16, 32000, 1), "fr")
            .unwrap();
        assert_eq!(t.text, "shared");
        assert_eq!(state.worker_pid(), Some(4242));
        assert!(state.worker_shared());
        assert!(state.0.capability().shared);
        assert!(state.0.release());
        assert_eq!(server.join().unwrap(), vec!["hello", "transcribe"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_shared_hello_disconnects_without_spawning_or_killing_daemon() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::Builder::new()
            .prefix("voice-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("v.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            for round in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["op"], "hello");
                if round == 0 {
                    ready_tx.send(()).unwrap();
                    line.clear();
                    assert_eq!(reader.read_line(&mut line).unwrap(), 0);
                } else {
                    writeln!(stream, "{}", json!({"id":req["id"],"ok":true,"result":{"protocol":1,"shared":true,"daemon":"voiced","model_repo":MODEL_REPO,"model_revision":MODEL_REVISION,"pid":4242,"loaded":true}})).unwrap();
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let req: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(req["op"], "load");
                    writeln!(
                        stream,
                        "{}",
                        json!({"id":req["id"],"ok":true,"result":{"loaded":true}})
                    )
                    .unwrap();
                }
            }
        });
        let mut paths = Paths::from_env(
            dir.path().into(),
            Some(dir.path().into()),
            Some(dir.path().join("missing")),
            None,
            None,
            None,
        );
        paths.socket = Some(socket);
        let state = VoiceState::with_paths(paths, DEFAULT_IDLE);
        let copy = state.clone();
        let request = thread::spawn(move || copy.0.prewarm());
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(state.0.cancel());
        assert!(request.join().unwrap().is_err());
        state.0.prewarm().unwrap();
        assert_eq!(state.worker_pid(), Some(4242));
        state.0.release();
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_shared_socket_falls_back_to_private_preflight() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::Builder::new()
            .prefix("voice-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("v.sock");
        drop(UnixListener::bind(&socket).unwrap());
        let mut paths = Paths::from_env(
            dir.path().into(),
            Some(dir.path().into()),
            Some(dir.path().join("missing")),
            None,
            None,
            None,
        );
        paths.socket = Some(socket);
        let state = VoiceState::with_paths(paths, DEFAULT_IDLE);
        let error = state.0.prewarm().unwrap_err();
        assert!(matches!(
            error.code.as_str(),
            "python_missing" | "unsupported"
        ));
        assert!(state.worker_pid().is_none());
    }

    /// Opt-in smoke test through Orb's Rust client against installed voiced.
    /// Supply a 16 kHz mono PCM WAV via ORB_VOICE_TEST_WAV. Never installs,
    /// starts, stops, or changes the daemon/venv itself.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn installed_voiced_transcribe_cancel_reconnect() {
        let state = VoiceState::with_paths(Paths::discover(), DEFAULT_IDLE);
        assert!(state.0.paths.shared_ready());
        state.0.prewarm().unwrap();
        assert!(state.worker_shared());
        let pid = state.worker_pid();
        let wav =
            std::fs::read(std::env::var("ORB_VOICE_TEST_WAV").expect("supply test WAV")).unwrap();
        let first = state.0.transcribe(wav.clone(), "en").unwrap();
        assert!(!first.text.trim().is_empty());
        let copy = state.clone();
        let audio = wav.clone();
        let request = thread::spawn(move || copy.0.transcribe(audio, "en"));
        let start = Instant::now();
        while !state.0.busy.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(1));
        }
        // Allow this test clip to enter inference before closing the socket.
        thread::sleep(Duration::from_millis(100));
        assert!(state.0.cancel());
        assert!(request.join().unwrap().is_err());
        assert!(!state
            .0
            .transcribe(wav, "en")
            .unwrap()
            .text
            .trim()
            .is_empty());
        assert!(state.worker_shared());
        assert_eq!(state.worker_pid(), pid);
        state.0.release();
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn installed_private_worker_fallback() {
        let mut paths = Paths::discover();
        assert!(paths.socket.is_none(), "run with ORB_VOICE_SOCKET=off");
        paths.socket = None;
        let state = VoiceState::with_paths(paths, DEFAULT_IDLE);
        let wav =
            std::fs::read(std::env::var("ORB_VOICE_TEST_WAV").expect("supply test WAV")).unwrap();
        assert!(!state
            .0
            .transcribe(wav, "en")
            .unwrap()
            .text
            .trim()
            .is_empty());
        assert!(!state.worker_shared());
        assert!(state.worker_pid().is_some());
        state.0.release();
    }

    /// Drive the real worker.py through the Rust side with the stub backend.
    #[test]
    fn worker_round_trip_with_fake_backend() {
        let Some(python) = find_python() else {
            eprintln!("skipping: no python on PATH");
            return;
        };
        let dir = std::env::temp_dir().join(format!("orb-voice-test-{}", std::process::id()));
        let paths = Paths::from_env(
            dir.clone(),
            Some(dir.clone()),
            Some(python),
            Some(dir.clone()),
            None,
            None,
        );
        std::env::set_var("ORB_VOICE_FAKE", "1");
        let mut w = Worker::spawn(&paths).expect("spawn worker");
        let status = w
            .request(json!({"op": "status"}), None, Duration::from_secs(20))
            .unwrap();
        assert_eq!(status["loaded"], false);
        let audio = wav(16000, 1, 16, 16000, 1);
        let r = w
            .request(
                json!({"op": "transcribe", "language": "de"}),
                Some(&audio),
                Duration::from_secs(20),
            )
            .unwrap();
        assert_eq!(r["language"], "de");
        assert_eq!(r["duration_secs"], 0.5);
        assert_eq!(r["text"], ""); // all-zero samples: nothing heard
        let e = w
            .request(
                json!({"op": "transcribe", "language": "xx"}),
                Some(&audio),
                Duration::from_secs(20),
            )
            .unwrap_err();
        assert_eq!(e.code, "unsupported_language");
        // Timeout path: an op the fake backend answers instantly still returns
        // before the deadline; a killed child surfaces as worker_exited.
        w.kill();
        let e = w
            .request(json!({"op": "status"}), None, Duration::from_secs(5))
            .unwrap_err();
        assert!(["worker_exited", "io"].contains(&e.code.as_str()), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
