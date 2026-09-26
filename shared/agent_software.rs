//! Host software inventory and execution/update exclusion shared by Orb and nodes.
//! Never invokes a shell or reads credentials. Version probes are bounded.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TOOLS: &[(&str, &str, &str, Option<&str>)] = &[
    (
        "claudecode",
        "Claude Code",
        "claude",
        Some("@anthropic-ai/claude-code"),
    ),
    ("codex", "Codex", "codex", Some("@openai/codex")),
    ("opencode", "OpenCode", "opencode", Some("opencode-ai")),
    ("grok", "Grok", "grok", None),
    ("gemini", "Gemini", "gemini", Some("@google/gemini-cli")),
];
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn root() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("AGENT_SOFTWARE_STATE_DIR") {
        return Ok(p.into());
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unavailable")?)
            .join(".local/state/agent-software"),
    )
}
fn io(e: std::io::Error) -> String {
    e.to_string()
}
fn lock(name: &str, exclusive: bool) -> Result<File, String> {
    let root = root()?;
    std::fs::create_dir_all(&root).map_err(io)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(name))
        .map_err(io)?;
    if exclusive {
        f.try_lock_exclusive()
    } else {
        FileExt::try_lock_shared(&f)
    }
    .map_err(|_| "Software maintenance in progress; retry after the update finishes".to_string())?;
    Ok(f)
}
/// Held for the entire execution, across all local windows and worker processes.
fn execution_guards(harness: &str) -> Result<Vec<File>, String> {
    let known = TOOLS.iter().any(|tool| tool.0 == harness);
    let components: Vec<_> = TOOLS
        .iter()
        .filter(|tool| !known || tool.0 == harness)
        .map(|tool| tool.0)
        .collect();
    let guards = components
        .iter()
        .map(|component| lock(&format!("execution-{component}.lock"), false))
        .collect::<Result<Vec<_>, _>>()?;
    if jobs()
        .iter()
        .any(|job| job.state == "installing" && components.contains(&job.component.as_str()))
    {
        return Err("Software update recovery is pending; retry after recovery completes".into());
    }
    Ok(guards)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub path: Option<String>,
    pub installed: bool,
    pub owner: String,
    pub update_supported: bool,
    pub latest: Option<String>,
    pub release_error: Option<String>,
    pub running: Vec<Launch>,
    pub instructions: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Launch {
    pub session: String,
    pub harness: String,
    pub version: Option<String>,
    pub runner: String,
    pub started_at: u64,
    pub pid: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inventory {
    pub checked_at: u64,
    pub components: Vec<Component>,
    pub runtime: Runtime,
    pub jobs: Vec<UpdateJob>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runtime {
    pub name: String,
    pub version: String,
    pub build: String,
    pub path: Option<String>,
    pub restart_required: bool,
    pub running: Vec<Launch>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateJob {
    pub id: String,
    pub component: String,
    pub version: String,
    pub path: String,
    pub previous_target: String,
    #[serde(default)]
    pub installer: Option<String>,
    pub state: String,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

pub fn build_id() -> String {
    format!(
        "{} · {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("SOFTWARE_BUILD_ID").unwrap_or("build unknown")
    )
}
fn executable_stamp() -> Option<(u64, u128)> {
    let m = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((
        m.len(),
        m.modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos(),
    ))
}
static BOOT: OnceLock<Option<(u64, u128)>> = OnceLock::new();
pub fn init() {
    BOOT.get_or_init(executable_stamp);
}

pub fn resolve(bin: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        dirs.extend([
            h.join(".local/bin"),
            h.join(".bun/bin"),
            h.join(".cargo/bin"),
        ]);
    }
    dirs.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ]);
    dirs.into_iter().map(|d| d.join(bin)).find(|p| p.is_file())
}
/// Output goes to a temporary file, avoiding a full stdout pipe deadlocking wait.
pub fn probe(path: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let dir = root()?;
    std::fs::create_dir_all(&dir).map_err(io)?;
    let p = dir.join(format!(
        "probe-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&p)
        .map_err(io)?;
    let result = (|| {
        let mut command = Command::new(path);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(output.try_clone().map_err(io)?)
            .stderr(Stdio::null())
            .spawn()
            .map_err(io)?;
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(io)? {
                if !status.success() {
                    return Err(format!("Version/protocol check exited with {status}"));
                }
                break;
            }
            if started.elapsed() > timeout {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err("Version/protocol check timed out".into());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        use std::io::Read;
        let mut text = String::new();
        File::open(&p)
            .map_err(io)?
            .take(65536)
            .read_to_string(&mut text)
            .map_err(io)?;
        Ok(text)
    })();
    let _ = std::fs::remove_file(p);
    result
}
type VersionCache = Mutex<HashMap<PathBuf, (u64, Option<(u64, SystemTime)>, Option<String>)>>;
static VERSIONS: OnceLock<VersionCache> = OnceLock::new();
pub fn clear_versions() {
    if let Ok(mut cache) = VERSIONS.get_or_init(Default::default).lock() {
        cache.clear();
    }
}
pub fn version(path: &Path) -> Option<String> {
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    if let Some((_, _, v)) = VERSIONS
        .get_or_init(Default::default)
        .lock()
        .ok()
        .and_then(|c| c.get(path).cloned())
        .filter(|(t, s, _)| now().saturating_sub(*t) < 900 && *s == stamp)
    {
        return v;
    }
    let result = probe(path, &["--version"], Duration::from_secs(5))
        .ok()
        .and_then(|v| {
            v.lines()
                .find(|s| !s.trim().is_empty())
                .map(|s| s.trim().chars().take(200).collect())
        });
    if let Ok(mut c) = VERSIONS.get_or_init(Default::default).lock() {
        c.insert(path.into(), (now(), stamp, result.clone()));
    }
    result
}
pub fn numeric_version(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| {
            s.split('.').count() == 3
                && s.split('.')
                    .all(|v| !v.is_empty() && v.parse::<u64>().is_ok())
        })
        .map(str::to_owned)
}
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| {
        numeric_version(s).map(|v| {
            v.split('.')
                .filter_map(|p| p.parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
    };
    matches!((parse(latest),parse(current)),(Some(a),Some(b)) if a>b)
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}
fn write_json(p: &Path, value: &impl Serialize) -> Result<(), String> {
    let temp = p.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    use std::io::Write;
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(io)?;
    f.write_all(&bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    std::fs::rename(temp, p).map_err(io)
}
// Runtime launch records are historical; live state is maintained by RAII receipts.
pub struct Execution {
    _guard: Vec<File>,
    receipt: PathBuf,
}
impl Drop for Execution {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.receipt);
    }
}
pub fn begin(session: &str, harness: &str, path: Option<&Path>) -> Result<Execution, String> {
    let guard = execution_guards(harness)?;
    let record = Launch {
        session: session.into(),
        harness: harness.into(),
        version: path.and_then(version),
        runner: build_id(),
        started_at: now(),
        pid: std::process::id(),
    };
    let dir = root()?.join("active");
    std::fs::create_dir_all(&dir).map_err(io)?;
    let receipt = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    write_json(&receipt, &record)?;
    let history = root()?.join("launches");
    std::fs::create_dir_all(&history).map_err(io)?;
    write_json(&history.join(receipt.file_name().unwrap()), &record)?;
    Ok(Execution {
        _guard: guard,
        receipt,
    })
}
fn active() -> Vec<Launch> {
    let Ok(root) = root() else { return vec![] };
    std::fs::read_dir(root.join("active"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| read_json::<Launch>(&e.path()))
        .filter(|r| {
            #[cfg(unix)]
            {
                unsafe { libc::kill(r.pid as i32, 0) == 0 }
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
        .collect()
}
fn owner(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path)
        .unwrap_or(path.into())
        .to_string_lossy()
        .to_string();
    if fleet_component(path).is_some() {
        "Fleet installer"
    } else if resolved.contains("/Cellar/") || resolved.contains("/Caskroom/") {
        "Homebrew"
    } else if resolved.contains("/agent-software/packages/") {
        "Orb"
    } else if path.to_string_lossy().contains("/.bun/") || resolved.contains("/.bun/") {
        "Bun"
    } else if resolved.contains("/.pnpm/") {
        "pnpm"
    } else if resolved.contains("/node_modules/") {
        "npm"
    } else if resolved.contains("/claude/versions/") {
        "Claude installer"
    } else {
        "External"
    }
    .into()
}
/// Fleet launchers must resolve into the component's administrator-owned tree.
fn fleet_component(path: &Path) -> Option<&'static str> {
    let name = TOOLS
        .iter()
        .find(|t| path == Path::new("/usr/local/bin").join(t.2))?
        .2;
    let resolved = std::fs::canonicalize(path).ok()?;
    resolved
        .starts_with(Path::new("/opt/sandboxed-tools").join(name))
        .then_some(name)
}
/// Infer the package from its manifest, not merely a bin directory name.
fn native_update(path: &Path, component: &str, target: &str) -> Option<(PathBuf, Vec<String>)> {
    #[cfg(target_os = "linux")]
    if let Some(name) = fleet_component(path) {
        let expected = TOOLS.iter().find(|t| t.0 == component)?.2;
        let helper = Path::new("/usr/local/sbin/orb-update-harness");
        if name != expected || !helper.is_file() {
            return None;
        }
        let mut args = vec![name.into(), target.into()];
        if unsafe { libc::geteuid() } == 0 {
            return Some((helper.into(), args));
        }
        args.splice(0..0, ["-n".into(), helper.to_string_lossy().into_owned()]);
        return Some((PathBuf::from("/usr/bin/sudo"), args));
    }
    if component == "claudecode" && owner(path) == "Claude installer" {
        return Some((path.to_path_buf(), vec!["install".into(), target.into()]));
    }
    let package = TOOLS.iter().find(|t| t.0 == component)?.3?;
    let resolved = std::fs::canonicalize(path).ok()?;
    let package_root = resolved.ancestors().find(|dir| {
        read_json::<serde_json::Value>(&dir.join("package.json"))
            .is_some_and(|manifest| manifest["name"] == package)
    })?;
    let spec = format!("{package}@{target}");
    match owner(path).as_str() {
        "Bun" => {
            let bun = resolve("bun")?;
            let bin = probe(&bun, &["pm", "-g", "bin"], Duration::from_secs(5)).ok()?;
            if path.parent()? != Path::new(bin.trim()) {
                return None;
            }
            // Respect Bun's configured global directory (which need not be ~/.bun).
            Some((bun, vec!["add".into(), "--global".into(), spec]))
        }
        "pnpm" => {
            let pnpm = resolve("pnpm")?;
            let bin = probe(&pnpm, &["bin", "--global"], Duration::from_secs(5)).ok()?;
            if path.parent()? != Path::new(bin.trim()) {
                return None;
            }
            Some((pnpm, vec!["add".into(), "--global".into(), spec]))
        }
        "npm" => {
            let modules = package_root
                .ancestors()
                .find(|dir| dir.file_name().is_some_and(|n| n == "node_modules"))?;
            let lib = modules.parent()?;
            if lib.file_name()? != "lib" {
                return None;
            }
            let prefix = lib.parent()?;
            if std::fs::canonicalize(path.parent()?).ok()?
                != std::fs::canonicalize(prefix.join("bin")).ok()?
            {
                return None;
            }
            Some((
                resolve("npm")?,
                vec![
                    "install".into(),
                    "--global".into(),
                    "--prefix".into(),
                    prefix.to_str()?.into(),
                    spec,
                ],
            ))
        }
        _ => None,
    }
}
/// Only the official Codex npm launcher is eligible. No arbitrary executable/update commands.
fn codex_launcher(path: &Path) -> bool {
    let Ok(target) = std::fs::canonicalize(path) else {
        return false;
    };
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    if matches!(owner(path).as_str(), "Homebrew" | "Bun" | "pnpm") {
        return false;
    }
    let Some(package) = target.parent().and_then(Path::parent) else {
        return false;
    };
    read_json::<serde_json::Value>(&package.join("package.json"))
        .is_some_and(|p| p["name"] == "@openai/codex")
        && target.file_name().is_some_and(|n| n == "codex.js")
        && resolve("npm").is_some()
}
fn jobs() -> Vec<UpdateJob> {
    root()
        .ok()
        .and_then(|r| read_json(&r.join("jobs.json")))
        .unwrap_or_default()
}
fn save_jobs(jobs: &[UpdateJob]) -> Result<(), String> {
    write_json(&root()?.join("jobs.json"), &jobs)
}

pub fn scan(name: &str, overrides: &HashMap<String, String>) -> Inventory {
    let live = active();
    let components=TOOLS.iter().map(|(id,label,bin,_)|{
  let path=overrides.get(*id).filter(|p|!p.trim().is_empty()).map(PathBuf::from).or_else(||resolve(bin));
  let ver=path.as_deref().and_then(version);let installed=path.as_ref().is_some_and(|p|p.is_file());
  let owner=path.as_deref().map(owner).unwrap_or_else(||"External".into());
  let native=path.as_deref().is_some_and(|p|native_update(p,id,"0.0.0").is_some());
  let supported=native||(*id=="codex"&&path.as_deref().is_some_and(codex_launcher));
  Component{id:(*id).into(),name:(*label).into(),version:ver,path:path.map(|p|p.display().to_string()),installed,owner:owner.clone(),update_supported:supported,latest:None,release_error:None,running:live.iter().filter(|r|r.harness==*id).cloned().collect(),instructions:if native {format!("Updates use the detected {owner} installation and its existing package directory. Orb waits for agents to finish, then verifies the installed version. No installer is replaced.")} else if supported {"Update installs the exact official npm package in an isolated directory, verifies its CLI, then switches the launcher. Previous installation is preserved for rollback.".into()} else {"Managed externally. Update with the installer that owns this executable, then refresh. Running sessions keep their launch version.".into()}}
 }).collect();
    Inventory {
        checked_at: now(),
        components,
        runtime: Runtime {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            build: build_id(),
            path: std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string()),
            running: live,
            restart_required: BOOT.get().is_some_and(|s| *s != executable_stamp()),
        },
        jobs: jobs(),
    }
}
static CACHE: OnceLock<Mutex<HashMap<String, (u64, String)>>> = OnceLock::new();
pub async fn releases(inventory: &mut Inventory, force: bool) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    else {
        return;
    };
    for row in &mut inventory.components {
        if !row.installed {
            continue;
        }
        let Some(package) = TOOLS.iter().find(|t| t.0 == row.id).and_then(|t| t.3) else {
            continue;
        };
        let cached = CACHE
            .get_or_init(Default::default)
            .lock()
            .ok()
            .and_then(|m| m.get(package).cloned())
            .filter(|(t, _)| !force && now().saturating_sub(*t) < 21600);
        if let Some((_, v)) = cached {
            row.latest = Some(v);
            continue;
        }
        let url = format!(
            "https://registry.npmjs.org/{}/latest",
            package.replace('/', "%2f")
        );
        match client.get(url).send().await {
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    if let Some(v) = v["version"]
                        .as_str()
                        .filter(|v| numeric_version(v).as_deref() == Some(*v))
                    {
                        row.latest = Some(v.into());
                        if let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() {
                            cache.insert(package.into(), (now(), v.into()));
                        }
                        continue;
                    }
                }
                row.release_error = Some("Release metadata unavailable".into());
            }
            _ => row.release_error = Some("Could not check releases".into()),
        }
    }
}

pub fn queue(component: &str, target: &str, path: &str) -> Result<UpdateJob, String> {
    let native = native_update(Path::new(path), component, target).is_some();
    if numeric_version(target).as_deref() != Some(target)
        || !(native || (component == "codex" && codex_launcher(Path::new(path))))
    {
        return Err("This installation is managed externally".into());
    }
    let _lock = lock("jobs.lock", true)?;
    let mut all = jobs();
    if let Some(job) = all
        .iter()
        .find(|j| j.path == path && matches!(j.state.as_str(), "queued" | "installing"))
    {
        return Ok(job.clone());
    }
    let previous = std::fs::read_link(path).map_err(io)?;
    let job = UpdateJob {
        id: uuid::Uuid::new_v4().to_string(),
        component: component.into(),
        version: target.into(),
        path: path.into(),
        previous_target: previous.display().to_string(),
        installer: native.then(|| owner(Path::new(path))),
        state: "queued".into(),
        error: None,
        created_at: now(),
        updated_at: now(),
    };
    all.push(job.clone());
    save_jobs(&all)?;
    Ok(job)
}
pub fn cancel(id: &str) -> Result<(), String> {
    let _lock = lock("jobs.lock", true)?;
    let mut all = jobs();
    let job = all
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or("Update not found")?;
    if job.state != "queued" {
        return Err("Only pending updates can be cancelled".into());
    }
    job.state = "cancelled".into();
    job.updated_at = now();
    save_jobs(&all)
}

fn external_harness_running(component: &str) -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut s = System::new();
    s.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
    );
    s.processes().values().any(|p| {
        let n = p.name().to_string_lossy().to_lowercase();
        TOOLS
            .iter()
            .filter(|t| t.0 == component)
            .any(|t| n == t.2 || n.starts_with(&format!("{}-", t.2)))
            || p.cmd().iter().take(4).any(|arg| {
                let arg = arg.to_string_lossy();
                TOOLS
                    .iter()
                    .filter(|t| t.0 == component)
                    .filter_map(|t| t.3)
                    .any(|package| arg.contains(&format!("/node_modules/{package}/")))
            })
    })
}
#[cfg(unix)]
fn switch_link(path: &Path, target: &Path) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &tmp).map_err(io)?;
    let result = std::fs::rename(&tmp, path).map_err(io);
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
#[cfg(not(unix))]
fn switch_link(_: &Path, _: &Path) -> Result<(), String> {
    Err("Managed updates require Unix".into())
}
fn install(job: &UpdateJob) -> Result<(), String> {
    let path = Path::new(&job.path);
    if let Some(installer) = &job.installer {
        if owner(path) != *installer
            || std::fs::read_link(path).map_err(io)? != Path::new(&job.previous_target)
        {
            return Err(
                "Installation changed since this update was queued. Refresh and retry.".into(),
            );
        }
        let (manager, args) = native_update(path, &job.component, &job.version)
            .ok_or("Installer could not be verified; refresh and retry")?;
        let result = probe(
            &manager,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            Duration::from_secs(300),
        );
        clear_versions();
        result.map_err(|e| format!("{installer} update failed: {e}. Refresh to inspect the installed version before retrying."))?;
        if version(path).and_then(|v| numeric_version(&v)).as_deref() != Some(&job.version) {
            return Err("Installer finished but the active CLI has a different version. Refresh and inspect its path.".into());
        }
        if job.component == "codex" {
            probe(path, &["app-server", "--help"], Duration::from_secs(10))?;
        }
        return Ok(());
    }
    if std::fs::read_link(path).map_err(io)? != Path::new(&job.previous_target)
        || !codex_launcher(path)
    {
        return Err("Executable changed since this update was queued. Refresh and retry.".into());
    }
    let npm = resolve("npm").ok_or("npm is unavailable")?;
    let dir = root()?.join("packages").join(&job.id);
    std::fs::create_dir_all(&dir).map_err(io)?;
    let package = format!("@openai/codex@{}", job.version);
    // --ignore-scripts: Codex's native binaries are optional npm packages, not postinstall downloads.
    probe(
        &npm,
        &[
            "install",
            "--prefix",
            dir.to_str().ok_or("Invalid install path")?,
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--registry=https://registry.npmjs.org",
            "--@openai:registry=https://registry.npmjs.org",
            &package,
        ],
        Duration::from_secs(300),
    )?;
    let next = dir.join("node_modules/@openai/codex/bin/codex.js");
    if version(&next).and_then(|v| numeric_version(&v)).as_deref() != Some(&job.version) {
        return Err("Installed version did not match the requested release".into());
    }
    let help = probe(&next, &["app-server", "--help"], Duration::from_secs(10))?;
    if !help.contains("app-server") {
        return Err("Codex app-server protocol check failed".into());
    }
    if std::fs::read_link(path).map_err(io)? != Path::new(&job.previous_target) {
        return Err("Executable changed during staging; activation cancelled".into());
    }
    switch_link(path, &next)?;
    clear_versions();
    if version(path).and_then(|v| numeric_version(&v)).as_deref() != Some(&job.version) {
        switch_link(path, Path::new(&job.previous_target))?;
        return Err("Activation check failed; previous launcher restored".into());
    }
    Ok(())
}
/// A process-independent worker lock prevents two desktop windows installing concurrently.
pub fn tick() -> Result<(), String> {
    tick_with_busy(external_harness_running)
}
fn tick_with_busy(busy: fn(&str) -> bool) -> Result<(), String> {
    let _worker = match lock("worker.lock", true) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    let _jobs = lock("jobs.lock", true)?;
    let mut all = jobs();
    if !all
        .iter()
        .any(|j| matches!(j.state.as_str(), "queued" | "installing"))
    {
        return Ok(());
    }
    // Recover a crashed activation before admitting another execution. The
    // worker lock proves no other installer owns these in-progress receipts.
    for j in all.iter_mut().filter(|j| j.state == "installing") {
        let Ok(_guard) = lock(&format!("execution-{}.lock", j.component), true) else {
            continue;
        };
        if busy(&j.component) {
            continue;
        }
        if j.installer.is_some() {
            clear_versions();
            j.state = "failed".into();
            j.error = Some("Installer was interrupted. Refresh and verify the installation before retrying; no automatic rollback was attempted.".into());
            j.updated_at = now();
            continue;
        }
        let staged = root()?
            .join("packages")
            .join(&j.id)
            .join("node_modules/@openai/codex/bin/codex.js");
        if std::fs::read_link(&j.path).ok().as_deref() == Some(staged.as_path()) {
            switch_link(Path::new(&j.path), Path::new(&j.previous_target))?;
            clear_versions();
        }
        j.state = "failed".into();
        j.error=Some("Updater stopped before verification completed. Previous launcher restored where activation had occurred; refresh and retry.".into());
        j.updated_at = now();
    }
    save_jobs(&all)?;
    // Skip blocked components so one long-running Codex goal cannot starve
    // unrelated installers. Legacy launch receipts still identify their users.
    let live = active();
    let mut candidate = None;
    for (index, job) in all.iter().enumerate().filter(|(_, j)| j.state == "queued") {
        if live
            .iter()
            .any(|r| r.harness == job.component || !TOOLS.iter().any(|t| t.0 == r.harness))
            || busy(&job.component)
        {
            continue;
        }
        if let Ok(guard) = lock(&format!("execution-{}.lock", job.component), true) {
            candidate = Some((index, guard));
            break;
        }
    }
    let Some((index, _maintenance)) = candidate else {
        return Ok(());
    };
    all[index].state = "installing".into();
    all[index].updated_at = now();
    save_jobs(&all)?;
    let job = all[index].clone();
    drop(_jobs);
    let result = install(&job);
    let _jobs = lock("jobs.lock", true)?;
    let mut all = jobs();
    if let Some(j) = all.iter_mut().find(|j| j.id == job.id) {
        j.state = if result.is_ok() {
            "completed"
        } else {
            "failed"
        }
        .into();
        j.error = result.err();
        j.updated_at = now();
    }
    save_jobs(&all)
}
pub fn start_worker() {
    init();
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| loop {
            let _ = tick();
            std::thread::sleep(Duration::from_secs(5));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("1.10.0", "codex-cli 1.9.0"));
        assert!(!is_newer("1.9.0", "1.10.0"));
        assert!(!is_newer("unknown", "1.0.0"));
    }
    #[test]
    fn invalid_targets_cannot_be_commands() {
        assert!(queue("codex", "1.0.0;echo test", "/tmp/missing").is_err());
        assert!(queue("unknown", "1.0.0", "/tmp/missing").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn durable_queue_waits_cancels_switches_and_rolls_back() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let old_root = std::env::var_os("AGENT_SOFTWARE_STATE_DIR");
        let old_path = std::env::var_os("PATH");
        let root = temp.path().join("state");
        std::env::set_var("AGENT_SOFTWARE_STATE_DIR", &root);
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                old_path.as_deref().unwrap_or_default().to_string_lossy()
            ),
        );
        let script = |path: &Path, text: &str| {
            std::fs::write(path, text).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let package = temp.path().join("node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("package.json"), r#"{"name":"@openai/codex"}"#).unwrap();
        script(
            &package.join("bin/codex.js"),
            "#!/bin/sh\necho 'codex-cli 1.0.0'\n",
        );
        let launcher = bin.join("codex");
        symlink(package.join("bin/codex.js"), &launcher).unwrap();
        script(
            &bin.join("npm"),
            r#"#!/bin/sh
prefix="$3"
mkdir -p "$prefix/node_modules/@openai/codex/bin"
printf '%s' '{"name":"@openai/codex"}' > "$prefix/node_modules/@openai/codex/package.json"
cat > "$prefix/node_modules/@openai/codex/bin/codex.js" <<'SCRIPT'
#!/bin/sh
if [ "$1" = "--version" ]; then echo 'codex-cli 1.1.0'; else echo 'codex app-server'; fi
SCRIPT
chmod +x "$prefix/node_modules/@openai/codex/bin/codex.js"
"#,
        );
        let claude_version = temp.path().join("claude/versions/2.1.281");
        std::fs::create_dir_all(claude_version.parent().unwrap()).unwrap();
        script(&claude_version, "#!/bin/sh\necho '2.1.281 (Claude Code)'\n");
        let claude = bin.join("claude");
        symlink(&claude_version, &claude).unwrap();
        assert_eq!(
            native_update(&claude, "claudecode", "2.1.283"),
            Some((claude.clone(), vec!["install".into(), "2.1.283".into()]))
        );
        let path = launcher.to_str().unwrap();
        let job = queue("codex", "1.1.0", path).unwrap();
        assert_eq!(
            queue("codex", "1.1.0", path).unwrap().id,
            job.id,
            "duplicate clicks share one job"
        );
        let run = begin("test-session", "codex", Some(&launcher)).unwrap();
        tick_with_busy(|_| false).unwrap();
        assert_eq!(jobs()[0].state, "queued", "live sessions fence updates");
        cancel(&job.id).unwrap();
        drop(run);
        let job = queue("codex", "1.1.0", path).unwrap();
        tick_with_busy(|_| true).unwrap();
        assert_eq!(
            jobs().last().unwrap().state,
            "queued",
            "untracked agents also fence updates"
        );
        let unrelated = begin("gemini-session", "gemini", None).unwrap();
        tick_with_busy(|_| false).unwrap();
        assert_eq!(jobs().last().unwrap().state, "completed");
        drop(unrelated);
        assert_eq!(
            numeric_version(&version(&launcher).unwrap()).as_deref(),
            Some("1.1.0")
        );
        assert!(
            Path::new(&job.previous_target).is_file(),
            "previous installation is retained"
        );
        assert!(
            cancel(&job.id).is_err(),
            "completed jobs cannot be cancelled"
        );
        let bad = queue("codex", "1.2.0", path).unwrap();
        let current = std::fs::read_link(&launcher).unwrap();
        tick_with_busy(|_| false).unwrap();
        assert_eq!(
            jobs().last().unwrap().state,
            "failed",
            "wrong downloaded version fails validation"
        );
        assert_eq!(
            std::fs::read_link(&launcher).unwrap(),
            current,
            "failed staging keeps current launcher"
        );
        let npm_script=std::fs::read_to_string(bin.join("npm")).unwrap().replace("codex-cli 1.1.0", "codex-cli 1.2.0").replace("if [ \"$1\" = \"--version\" ];", "case \"$0\" in */bin/codex) echo 'codex-cli 0.0.0'; exit 0;; esac\nif [ \"$1\" = \"--version\" ];");
        script(&bin.join("npm"), &npm_script);
        queue("codex", "1.2.0", path).unwrap();
        tick_with_busy(|_| false).unwrap();
        assert!(jobs()
            .last()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("previous launcher restored"));
        assert_eq!(
            std::fs::read_link(&launcher).unwrap(),
            current,
            "failed activation rolls back atomically"
        );
        let mut all = jobs();
        all.last_mut().unwrap().state = "installing".into();
        save_jobs(&all).unwrap();
        let interrupted = all.last().unwrap();
        let staged = root
            .join("packages")
            .join(&interrupted.id)
            .join("node_modules/@openai/codex/bin/codex.js");
        switch_link(&launcher, &staged).unwrap();
        assert!(
            begin("new-session", "codex", Some(&launcher)).is_err(),
            "uncertain activation fences new launches"
        );
        tick_with_busy(|_| false).unwrap();
        assert_eq!(
            std::fs::read_link(&launcher).unwrap(),
            current,
            "crashed activation restores previous launcher"
        );
        assert_eq!(
            jobs().iter().find(|j| j.id == bad.id).unwrap().state,
            "failed",
            "crashed installs are not blindly replayed"
        );
        // Bun's launcher can point outside ~/.bun (a configured globalDir).
        let bun_bin = temp.path().join(".bun/bin");
        std::fs::create_dir_all(&bun_bin).unwrap();
        let bun_launcher = bun_bin.join("codex");
        symlink(package.join("bin/codex.js"), &bun_launcher).unwrap();
        script(
            &bin.join("bun"),
            &format!("#!/bin/sh\necho '{}'\n", bun_bin.display()),
        );
        assert_eq!(owner(&bun_launcher), "Bun");
        assert!(
            !codex_launcher(&bun_launcher),
            "Bun must never fall back to npm staging"
        );
        let (manager, args) = native_update(&bun_launcher, "codex", "1.2.0").unwrap();
        assert_eq!(manager, bin.join("bun"));
        assert_eq!(args, vec!["add", "--global", "@openai/codex@1.2.0"]);
        assert!(native_update(&bun_launcher, "gemini", "1.2.0").is_none());
        script(
            &bin.join("bun"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = "pm" ]; then echo '{}'; exit 0; fi
[ "$1" = "add" ] && [ "$2" = "--global" ] && [ "$3" = "@openai/codex@1.3.0" ] || exit 9
cat > '{}' <<'CLI'
#!/bin/sh
if [ "$1" = "--version" ]; then echo 'codex-cli 1.3.0'; else echo 'codex app-server'; fi
CLI
"#,
                bun_bin.display(),
                package.join("bin/codex.js").display()
            ),
        );
        let bun_job = queue("codex", "1.3.0", bun_launcher.to_str().unwrap()).unwrap();
        assert_eq!(bun_job.installer.as_deref(), Some("Bun"));
        tick_with_busy(|_| false).unwrap();
        assert_eq!(jobs().last().unwrap().state, "completed");
        assert_eq!(
            owner(&bun_launcher),
            "Bun",
            "native updates retain installer ownership"
        );
        assert_eq!(
            std::fs::read_link(&bun_launcher).unwrap(),
            package.join("bin/codex.js")
        );

        script(&bin.join("bun"), "#!/bin/sh\necho /another/bin\n");
        assert!(native_update(&bun_launcher, "codex", "1.2.0").is_none());
        let global = temp.path().join("global");
        let npm_package = global.join("lib/node_modules/@openai/codex");
        std::fs::create_dir_all(npm_package.join("bin")).unwrap();
        std::fs::create_dir_all(global.join("bin")).unwrap();
        std::fs::write(
            npm_package.join("package.json"),
            r#"{"name":"@openai/codex"}"#,
        )
        .unwrap();
        script(&npm_package.join("bin/codex.js"), "#!/bin/sh\necho 1.0.0\n");
        let npm_launcher = global.join("bin/codex");
        symlink(npm_package.join("bin/codex.js"), &npm_launcher).unwrap();
        let (_, args) = native_update(&npm_launcher, "codex", "1.2.0").unwrap();
        assert_eq!(
            args[3],
            std::fs::canonicalize(&global).unwrap().to_str().unwrap(),
            "preserve the owning npm prefix"
        );
        match old_root {
            Some(v) => std::env::set_var("AGENT_SOFTWARE_STATE_DIR", v),
            None => std::env::remove_var("AGENT_SOFTWARE_STATE_DIR"),
        };
        match old_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        };
    }
}
