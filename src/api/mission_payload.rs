//! Materialize Orb `@` chips into ordinary files before a harness starts.
//!
//! Paloma writes `.paloma/attach/…`, `.paloma/controller.md`, and
//! `.paloma/attach.md`. Vendor CLIs only see paths.

use base64::Engine;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

// Hold directory descriptors through each operation. Checking canonical paths
// and then opening by name would allow an agent to swap a symlink in between.
fn open_child(dir: &File, name: &std::ffi::OsStr, flags: i32) -> Result<File, String> {
    let name = CString::new(name.as_bytes()).map_err(|_| "invalid path")?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn directory(path: &Path, create: bool) -> Result<File, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut dir = File::open("/").map_err(|e| e.to_string())?;
    for part in absolute.components() {
        match part {
            Component::RootDir => continue,
            Component::Normal(name) => {
                if create {
                    let c = CString::new(name.as_bytes()).map_err(|_| "invalid path")?;
                    let result = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o700) };
                    if result == 0 {
                        dir.sync_all().map_err(|e| e.to_string())?;
                    }
                    if result != 0
                        && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                    {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                }
                dir = open_child(&dir, name, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            _ => return Err("invalid directory path".into()),
        }
    }
    Ok(dir)
}

fn bounded_read(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let parent = directory(path.parent().ok_or("missing parent")?, false)?;
    let file = open_child(
        &parent,
        path.file_name().ok_or("missing name")?,
        libc::O_RDONLY | libc::O_NONBLOCK,
    )?;
    bounded_read_file(file, cap)
}

fn bounded_read_file(file: File, cap: usize) -> Result<Vec<u8>, String> {
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err("not a regular single-link file".into());
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > cap {
        return Err(format!("over {cap} bytes"));
    }
    Ok(bytes)
}

fn safe_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write(path, bytes, false).map(|_| ())
}

fn atomic_write(path: &Path, bytes: &[u8], exclusive: bool) -> Result<bool, String> {
    let parent = directory(path.parent().ok_or("missing parent")?, true)?;
    let name = CString::new(path.file_name().ok_or("missing name")?.as_bytes())
        .map_err(|_| "invalid path")?;
    let tmp = format!(".attachment-{}", Uuid::new_v4());
    let mut file = open_child(
        &parent,
        std::ffi::OsStr::new(&tmp),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    let temp_name = CString::new(tmp).unwrap();
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let rc = unsafe {
            if exclusive {
                #[cfg(target_os = "linux")]
                {
                    libc::renameat2(
                        parent.as_raw_fd(),
                        temp_name.as_ptr(),
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                }
                #[cfg(target_vendor = "apple")]
                {
                    libc::renameatx_np(
                        parent.as_raw_fd(),
                        temp_name.as_ptr(),
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        libc::RENAME_EXCL,
                    )
                }
                #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
                {
                    // linkat publishes the complete file exclusively; the temporary
                    // name is removed below before this operation returns.
                    libc::linkat(
                        parent.as_raw_fd(),
                        temp_name.as_ptr(),
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        0,
                    )
                }
            } else {
                libc::renameat(
                    parent.as_raw_fd(),
                    temp_name.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            }
        };
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            if exclusive && error.raw_os_error() == Some(libc::EEXIST) {
                return Ok(false);
            }
            return Err(error.to_string());
        }
        parent.sync_all().map_err(|e| e.to_string())?;
        Ok(true)
    })();
    if exclusive || result.is_err() {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temp_name.as_ptr(), 0);
        }
    }
    result
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FOLDER_FILE_CAP: usize = 40;
pub const FOLDER_BYTE_CAP: usize = 256 * 1024;
pub const FILE_BYTE_CAP: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Context,
    File,
    Folder,
    Controller,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionAttachment {
    pub kind: AttachmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionPayload {
    #[serde(default)]
    pub attachments: Vec<MissionAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_md: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializeReport {
    pub written: Vec<String>,
    pub skipped: Vec<String>,
    pub truncated: bool,
}

/// The operator-owned storage root may deliberately be a volume symlink.
/// Resolve only this trust boundary, never project/attachment-controlled
/// descendants: those still pass through the descriptor-based no-follow walk.
pub(crate) fn storage_root(working_dir: &Path) -> PathBuf {
    let root = working_dir.join(".sandboxed-sh");
    root.canonicalize().unwrap_or_else(|_| {
        working_dir
            .canonicalize()
            .unwrap_or_else(|_| working_dir.to_path_buf())
            .join(".sandboxed-sh")
    })
}

pub fn project_files_root(working_dir: &Path, slug: &str) -> PathBuf {
    storage_root(working_dir).join("project-files").join(slug)
}

pub fn sidecar_path(working_dir: &Path, mission_id: Uuid) -> PathBuf {
    storage_root(working_dir)
        .join("mission-payloads")
        .join(format!("{mission_id}.json"))
}

pub fn write_sidecar(
    working_dir: &Path,
    mission_id: Uuid,
    payload: &MissionPayload,
) -> Result<PathBuf, String> {
    validate(payload)?;
    let path = sidecar_path(working_dir, mission_id);
    let json = serde_json::to_string_pretty(payload).map_err(|e| e.to_string())?;
    safe_write(&path, json.as_bytes())?;
    Ok(path)
}

pub fn read_sidecar(
    working_dir: &Path,
    mission_id: Uuid,
) -> Result<Option<MissionPayload>, String> {
    let path = sidecar_path(working_dir, mission_id);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let raw = bounded_read(&path, 2 * 1024 * 1024)?;
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn is_secret_path(rel: &str) -> bool {
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

fn safe_rel(rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
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

/// Write attachments under `.paloma/` so a git checkout cwd stays clean.
pub fn materialize(
    cwd: &Path,
    project_files_root: &Path,
    payload: &MissionPayload,
) -> Result<MaterializeReport, String> {
    let (files, report) = prepare(project_files_root, payload)?;
    for (path, bytes) in files {
        safe_write(&cwd.join(path), &bytes)?;
    }
    Ok(report)
}

fn prepare(
    project_files_root: &Path,
    payload: &MissionPayload,
) -> Result<(BTreeMap<String, Vec<u8>>, MaterializeReport), String> {
    validate(payload)?;
    let mut files = BTreeMap::new();
    let mut report = MaterializeReport::default();
    let mut manifest = String::from("# Attached context\n\nYou were given these paths. Read them; do not invent Paloma-specific `@` syntax.\n\n");

    for attachment in &payload.attachments {
        match attachment.kind {
            AttachmentKind::Context => {
                let written = attachment.path.as_deref().unwrap_or("context");
                let relative = written
                    .strip_prefix("context/")
                    .unwrap_or("")
                    .trim_end_matches('/');
                if !relative.is_empty() {
                    crate::project_context::valid_path(relative)?;
                }
                let source = project_files_root.join(relative);
                if !source.exists() {
                    return Err(format!("context path does not exist: {written}"));
                }
                manifest.push_str(
                    "- Shared context paths in the message are writable and synchronized.\n",
                );
            }
            AttachmentKind::Controller => {
                let body = payload.controller_md.as_deref().unwrap_or(
                    "# Controller snapshot\n\nNo controller snapshot was available when this mission started.\n",
                );
                files.insert(".paloma/controller.md".into(), body.as_bytes().to_vec());
                report.written.push(".paloma/controller.md".into());
                manifest.push_str("- `.paloma/controller.md` — controller snapshot (grant, last `[CTRL:]`, tracks, live missions, pending steers)\n");
            }
            AttachmentKind::File => {
                let rel = safe_rel(attachment.path.as_deref().unwrap_or(""))?;
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if is_secret_path(&rel_str) {
                    report.skipped.push(format!("{rel_str} (secret)"));
                    continue;
                }
                let src = project_files_root.join(&rel);
                let bytes = match bounded_read(&src, FILE_BYTE_CAP) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        report.skipped.push(format!("{rel_str} ({error})"));
                        continue;
                    }
                };
                let dest_rel = format!(".paloma/attach/{rel_str}");
                files.insert(dest_rel.clone(), bytes);
                report.written.push(dest_rel.clone());
                manifest.push_str(&format!("- `{dest_rel}` (from `{rel_str}`)\n"));
            }
            AttachmentKind::Folder => {
                let rel = safe_rel(attachment.path.as_deref().unwrap_or(""))?;
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let src_dir = project_files_root.join(&rel);
                let src_dir = match directory(&src_dir, false) {
                    Ok(dir) => dir,
                    Err(error) => {
                        report.skipped.push(format!("{rel_str}/ ({error})"));
                        continue;
                    }
                };
                let mut folder_files = Vec::new();
                let mut visited = 0;
                collect_files(
                    &src_dir,
                    &rel,
                    &mut folder_files,
                    &mut visited,
                    0,
                    &mut report,
                );
                folder_files.sort_by(|a, b| a.0.cmp(&b.0));
                let mut used = 0usize;
                let mut count = 0usize;
                for (rel_file, file) in folder_files {
                    let rel_file_str = rel_file.to_string_lossy().replace('\\', "/");
                    if is_secret_path(&rel_file_str) {
                        report.skipped.push(format!("{rel_file_str} (secret)"));
                        continue;
                    }
                    let meta = match file.metadata() {
                        Ok(meta) => meta,
                        Err(_) => continue,
                    };
                    if !meta.is_file() {
                        continue;
                    }
                    if count >= FOLDER_FILE_CAP || used + meta.len() as usize > FOLDER_BYTE_CAP {
                        report.truncated = true;
                        report.skipped.push(format!("{rel_file_str} (folder cap)"));
                        continue;
                    }
                    let bytes = match bounded_read_file(file, FOLDER_BYTE_CAP - used) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            report.truncated = true;
                            report.skipped.push(format!("{rel_file_str} ({error})"));
                            continue;
                        }
                    };
                    used += bytes.len();
                    count += 1;
                    let dest_rel = format!(".paloma/attach/{rel_file_str}");
                    files.insert(dest_rel.clone(), bytes);
                    report.written.push(dest_rel.clone());
                    manifest.push_str(&format!("- `{dest_rel}` (from `{rel_file_str}`)\n"));
                }
            }
        }
    }

    if report.truncated {
        manifest.push_str(
            "\nFolder listing was truncated (max 40 files / 256 KiB). Secrets were skipped.\n",
        );
    }
    if !report.skipped.is_empty() {
        manifest.push_str("\nSkipped:\n");
        for skip in &report.skipped {
            manifest.push_str(&format!("- {skip}\n"));
        }
    }
    files.insert(".paloma/attach.md".into(), manifest.into_bytes());
    report.written.push(".paloma/attach.md".into());
    Ok((files, report))
}

pub fn validate(payload: &MissionPayload) -> Result<(), String> {
    if payload.attachments.len() > 16 {
        return Err("at most 16 attachments are allowed".into());
    }
    if payload
        .controller_md
        .as_ref()
        .is_some_and(|s| s.len() > FILE_BYTE_CAP)
    {
        return Err("controller snapshot too large".into());
    }
    if payload
        .project
        .as_deref()
        .is_some_and(|s| !super::projects_overview::is_plain_key(s))
    {
        return Err("invalid attachment project".into());
    }
    for attachment in &payload.attachments {
        if attachment.kind == AttachmentKind::Context {
            let path = attachment.path.as_deref().unwrap_or("context");
            if path != "context" && path != "context/" {
                crate::project_context::valid_path(
                    path.strip_prefix("context/")
                        .ok_or("invalid context reference")?
                        .trim_end_matches('/'),
                )?;
            }
        } else if attachment.kind != AttachmentKind::Controller {
            let path = attachment.path.as_deref().unwrap_or("");
            safe_rel(path)?;
            if path.len() > 4096 {
                return Err("attachment path too long".into());
            }
        }
    }
    Ok(())
}

fn collect_files(
    dir: &File,
    prefix: &Path,
    out: &mut Vec<(PathBuf, File)>,
    visited: &mut usize,
    depth: usize,
    report: &mut MaterializeReport,
) {
    if depth > 32 || *visited >= 4000 || out.len() >= FOLDER_FILE_CAP {
        report.truncated = true;
        return;
    }
    // Linux procfs exposes this already-open descriptor. Keep it alive while
    // enumerating and open every child relative to it, never via a raced path.
    let read = match std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())) {
        Ok(read) => read,
        Err(error) => {
            report
                .skipped
                .push(format!("{} ({error})", prefix.display()));
            return;
        }
    };
    for entry in read {
        if *visited >= 4000 || out.len() >= FOLDER_FILE_CAP {
            report.truncated = true;
            break;
        }
        *visited += 1;
        let Ok(entry) = entry else {
            report
                .skipped
                .push(format!("{} (unreadable entry)", prefix.display()));
            continue;
        };
        let rel = prefix.join(entry.file_name());
        if is_secret_path(&rel.to_string_lossy()) {
            report.skipped.push(format!("{} (secret)", rel.display()));
            continue;
        }
        let file = match open_child(dir, &entry.file_name(), libc::O_RDONLY | libc::O_NONBLOCK) {
            Ok(file) => file,
            Err(error) => {
                report.skipped.push(format!("{} ({error})", rel.display()));
                continue;
            }
        };
        let Ok(meta) = file.metadata() else { continue };
        if meta.is_dir() {
            collect_files(&file, &rel, out, visited, depth + 1, report);
        } else if meta.is_file() {
            out.push((rel, file));
        } else {
            report
                .skipped
                .push(format!("{} (not a regular file)", rel.display()));
        }
    }
}

// Queue content carries a durable, mission-scoped reference. Keeping the ID in
// the reference also survives the scheduler combining deferred messages or
// assigning a new dispatch ID. Only this exact server format is resolved; paths
// from prose are never opened. Snapshots are immutable once published.
const SNAPSHOT_MARKER: &str = "<!-- paloma:attachment:";
const SNAPSHOT_CAP: usize = 16 * 1024 * 1024;

/// Attachment references are generated only after a snapshot is durably staged.
/// Reject the reserved prefix at public ingress, before accepting any work, so
/// arbitrary prose can neither forge a reference nor fail later at dispatch.
pub fn validate_user_content(content: &str) -> Result<(), String> {
    if content.contains(SNAPSHOT_MARKER) {
        return Err("Message contains a reserved attachment reference. Remove it and use the attachment picker to attach context.".into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct MessageSnapshot {
    mission_id: Uuid,
    message_id: Uuid,
    content: String,
    payload: MissionPayload,
    files: BTreeMap<String, String>, // base64, including the manifest
    report: MaterializeReport,
}

fn message_snapshot_path(working_dir: &Path, mission_id: Uuid, message_id: Uuid) -> PathBuf {
    storage_root(working_dir)
        .join("message-payloads")
        .join(mission_id.to_string())
        .join(format!("{message_id}.json"))
}

pub fn stage_message(
    working_dir: &Path,
    mission_id: Uuid,
    message_id: Uuid,
    content: &str,
    payload: &MissionPayload,
) -> Result<(String, MaterializeReport), String> {
    validate_user_content(content)?;
    validate(payload)?;
    let path = message_snapshot_path(working_dir, mission_id, message_id);
    if !path.try_exists().map_err(|e| e.to_string())? {
        let project = payload
            .project
            .as_deref()
            .ok_or("attachments require a project")?;
        let (files, report) = prepare(&project_files_root(working_dir, project), payload)?;
        let snapshot = MessageSnapshot {
            mission_id,
            message_id,
            content: content.into(),
            payload: payload.clone(),
            report,
            files: files
                .into_iter()
                .map(|(p, bytes)| (p, base64::engine::general_purpose::STANDARD.encode(bytes)))
                .collect(),
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;
        if bytes.len() > SNAPSHOT_CAP {
            return Err("attachment snapshot too large".into());
        }
        // A competing retry may win publication. Read and verify the winner;
        // never overwrite the context attached to an already accepted ID.
        atomic_write(&path, &bytes, true)?;
    }
    let snapshot = read_message_snapshot(working_dir, mission_id, message_id)?;
    if snapshot.content != content
        || snapshot.payload.attachments != payload.attachments
        || snapshot.payload.project != payload.project
    {
        return Err("message ID already has different attachment content".into());
    }
    let relative = format!(".paloma/messages/{message_id}");
    Ok((format!("{content}\n\n{SNAPSHOT_MARKER}{message_id} -->\nAttached context: read `{relative}/.paloma/attach.md` (paths in that manifest are relative to `{relative}`)."), snapshot.report))
}

fn read_message_snapshot(
    working_dir: &Path,
    mission_id: Uuid,
    message_id: Uuid,
) -> Result<MessageSnapshot, String> {
    let bytes = bounded_read(
        &message_snapshot_path(working_dir, mission_id, message_id),
        SNAPSHOT_CAP,
    )?;
    let snapshot: MessageSnapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if snapshot.mission_id != mission_id || snapshot.message_id != message_id {
        return Err("attachment identity mismatch".into());
    }
    Ok(snapshot)
}

pub fn materialize_turn(
    working_dir: &Path,
    cwd: &Path,
    mission_id: Uuid,
    content: &str,
) -> Result<String, String> {
    let mut content = content.to_owned();
    if let Some(payload) = read_sidecar(working_dir, mission_id)? {
        if !payload.attachments.is_empty() {
            let project = payload
                .project
                .as_deref()
                .ok_or("attachments require a project")?;
            materialize(cwd, &project_files_root(working_dir, project), &payload)?;
            content = rewrite_context(
                &content,
                &project_files_root(working_dir, project),
                &payload,
            )?;
            content.push_str("\n\nRead attached context in `.paloma/attach.md`.");
        }
    }
    let mut contexts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for part in content.split(SNAPSHOT_MARKER).skip(1) {
        let id = part
            .split_once(" -->")
            .ok_or("invalid attachment reference")?
            .0;
        let id = Uuid::parse_str(id).map_err(|_| "invalid attachment ID")?;
        if !seen.insert(id) {
            continue;
        }
        if seen.len() > 64 {
            return Err("too many attachment references in one turn".into());
        }
        let snapshot = read_message_snapshot(working_dir, mission_id, id)?;
        contexts.push(snapshot.payload.clone());
        let dest = cwd.join(format!(".paloma/messages/{id}"));
        for (path, encoded) in snapshot.files {
            let path = safe_rel(&path)?;
            if !path.starts_with(".paloma") {
                return Err("invalid snapshot file".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| e.to_string())?;
            safe_write(&dest.join(path), &bytes)?;
        }
    }
    for payload in contexts {
        if let Some(project) = &payload.project {
            content = rewrite_context(
                &content,
                &project_files_root(working_dir, project),
                &payload,
            )?;
        }
    }
    Ok(content)
}

fn rewrite_context(content: &str, root: &Path, payload: &MissionPayload) -> Result<String, String> {
    let pattern = regex::Regex::new(r#"(^|[\s(])@(?:"([^"]+)"|([^\s)\]},;]+))"#)
        .map_err(|e| e.to_string())?;
    Ok(pattern
        .replace_all(content, |captures: &regex::Captures| {
            let value = captures
                .get(2)
                .or_else(|| captures.get(3))
                .unwrap()
                .as_str();
            let path = value.trim_end_matches('/');
            if payload.attachments.iter().any(|item| {
                item.kind == AttachmentKind::Context
                    && item
                        .path
                        .as_deref()
                        .unwrap_or("context")
                        .trim_end_matches('/')
                        == path
            }) {
                let relative = path.strip_prefix("context/").unwrap_or("");
                format!(
                    "{}{}",
                    &captures[1],
                    serde_json::to_string(&root.join(relative).to_string_lossy()).unwrap()
                )
            } else {
                captures[0].to_string()
            }
        })
        .into_owned())
}

#[derive(Debug, Clone, Default)]
pub struct ControllerSnapshot {
    pub slug: String,
    pub grant: Option<String>,
    pub ctrl: Option<String>,
    pub last_tick: Option<String>,
    pub tracks: Vec<String>,
    pub live_missions: Vec<String>,
    pub pending_steers: Vec<String>,
}

pub fn render_controller_md(snap: &ControllerSnapshot) -> String {
    let mut out = format!("# Controller snapshot — `{}`\n\n", snap.slug);
    out.push_str("This is what the orchestrator last believed. Changing what it will do is a **steer**, then Run — not an edit to this file.\n\n");
    if let Some(grant) = &snap.grant {
        out.push_str("## Grant\n\n");
        out.push_str(grant);
        out.push_str("\n\n");
    }
    if let Some(ctrl) = &snap.ctrl {
        out.push_str("## Last `[CTRL:]`\n\n```\n");
        out.push_str(ctrl);
        out.push_str("\n```\n\n");
    }
    if let Some(tick) = &snap.last_tick {
        out.push_str("## Last non-silent tick\n\n");
        out.push_str(tick);
        out.push_str("\n\n");
    }
    if !snap.tracks.is_empty() {
        out.push_str("## Open tracks\n\n");
        for track in &snap.tracks {
            out.push_str(&format!("- {track}\n"));
        }
        out.push('\n');
    }
    if !snap.live_missions.is_empty() {
        out.push_str("## Live missions\n\n");
        for mission in &snap.live_missions {
            out.push_str(&format!("- {mission}\n"));
        }
        out.push('\n');
    }
    if !snap.pending_steers.is_empty() {
        out.push_str("## Pending steers\n\n");
        for steer in &snap.pending_steers {
            out.push_str(&format!("- {steer}\n"));
        }
        out.push('\n');
    }
    out
}

pub fn parse_attachments(value: Option<&serde_json::Value>) -> Vec<MissionAttachment> {
    let Some(value) = value else {
        return Vec::new();
    };
    serde_json::from_value(value.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_git_are_skipped() {
        assert!(is_secret_path("notes/.env"));
        assert!(is_secret_path(".env.local"));
        assert!(is_secret_path("certs/prod.pem"));
        assert!(is_secret_path("id_rsa"));
        assert!(is_secret_path("repo/.git/config"));
        assert!(!is_secret_path("notes/foo.md"));
    }

    #[test]
    fn materialize_writes_overlay_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let files = tmp.path().join("project-files");
        let cwd = tmp.path().join("checkout");
        std::fs::create_dir_all(files.join("notes")).unwrap();
        std::fs::write(files.join("notes/foo.md"), "hello notes").unwrap();
        std::fs::write(files.join("notes/.env"), "SECRET=1").unwrap();
        std::fs::write(files.join("notes/secret.pem"), "nope").unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let payload = MissionPayload {
            project: Some("lido".into()),
            attachments: vec![
                MissionAttachment {
                    kind: AttachmentKind::File,
                    path: Some("notes/foo.md".into()),
                },
                MissionAttachment {
                    kind: AttachmentKind::Folder,
                    path: Some("notes".into()),
                },
                MissionAttachment {
                    kind: AttachmentKind::Controller,
                    path: None,
                },
            ],
            controller_md: Some("# Controller snapshot — `lido`\n\ngrant: review-first\n".into()),
        };
        let report = materialize(&cwd, &files, &payload).expect("materialize");
        assert!(report
            .written
            .iter()
            .any(|p| p == ".paloma/attach/notes/foo.md"));
        assert!(report.written.iter().any(|p| p == ".paloma/controller.md"));
        assert!(report.written.iter().any(|p| p == ".paloma/attach.md"));
        assert!(report.skipped.iter().any(|s| s.contains(".env")));
        assert!(report.skipped.iter().any(|s| s.contains("secret.pem")));
        assert_eq!(
            std::fs::read_to_string(cwd.join(".paloma/attach/notes/foo.md")).unwrap(),
            "hello notes"
        );
        let manifest = std::fs::read_to_string(cwd.join(".paloma/attach.md")).unwrap();
        assert!(manifest.contains("You were given these paths"));
        assert!(std::fs::read_to_string(cwd.join(".paloma/controller.md"))
            .unwrap()
            .contains("lido"));

        // Git checkout cwd uses the same overlay — the repo stays clean.
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        let again = materialize(&cwd, &files, &payload).expect("git cwd");
        assert!(again
            .written
            .iter()
            .any(|p| p == ".paloma/attach/notes/foo.md"));
        assert!(!cwd.join("notes/foo.md").exists());
    }

    #[test]
    fn folder_listing_honors_size_and_count_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let files = tmp.path().join("project-files");
        let cwd = tmp.path().join("checkout");
        std::fs::create_dir_all(files.join("notes")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        for i in 0..(FOLDER_FILE_CAP + 3) {
            std::fs::write(files.join(format!("notes/f{i}.md")), "x").unwrap();
        }
        let payload = MissionPayload {
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::Folder,
                path: Some("notes".into()),
            }],
            ..Default::default()
        };
        let report = materialize(&cwd, &files, &payload).expect("cap");
        assert!(report.truncated);
        let copied = report
            .written
            .iter()
            .filter(|p| p.starts_with(".paloma/attach/"))
            .count();
        assert!(copied <= FOLDER_FILE_CAP);
        let manifest = std::fs::read_to_string(cwd.join(".paloma/attach.md")).unwrap();
        assert!(manifest.contains("truncated"));
    }

    #[test]
    fn rejects_traversal_absolute_and_secret_ancestors() {
        for path in [
            "../outside",
            "/etc/passwd",
            "notes/../../x",
            "notes\\x",
            "a\nb",
        ] {
            assert!(safe_rel(path).is_err(), "{path:?}");
        }
        for path in [
            ".env/password",
            ".aws/config",
            ".codex/auth.json",
            "x/.credentials.json",
        ] {
            assert!(is_secret_path(path), "{path}");
        }
    }

    #[test]
    fn source_links_cycles_and_large_files_cannot_escape_or_exhaust_caps() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("files");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, "private fixture").unwrap();
        symlink(&outside, root.join("notes/link")).unwrap();
        symlink(&root, root.join("notes/cycle")).unwrap();
        std::fs::hard_link(&outside, root.join("notes/hardlink")).unwrap();
        File::create(root.join("notes/large"))
            .unwrap()
            .set_len(100_000_000)
            .unwrap();
        std::fs::write(root.join("notes/ok"), "allowed").unwrap();
        let payload = MissionPayload {
            attachments: vec![
                MissionAttachment {
                    kind: AttachmentKind::File,
                    path: Some("notes/link".into()),
                },
                MissionAttachment {
                    kind: AttachmentKind::File,
                    path: Some("notes/large".into()),
                },
                MissionAttachment {
                    kind: AttachmentKind::Folder,
                    path: Some("notes".into()),
                },
            ],
            ..Default::default()
        };
        let report = materialize(&cwd, &root, &payload).unwrap();
        assert!(report.truncated);
        assert_eq!(
            std::fs::read_to_string(cwd.join(".paloma/attach/notes/ok")).unwrap(),
            "allowed"
        );
        for name in ["link", "cycle", "hardlink", "large"] {
            assert!(
                !cwd.join(".paloma/attach/notes").join(name).exists(),
                "{name}"
            );
        }
    }

    #[test]
    fn destination_links_never_modify_their_targets() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("files");
        let cwd = tmp.path().join("cwd");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(cwd.join(".paloma/attach")).unwrap();
        std::fs::write(root.join("note"), "public").unwrap();
        std::fs::write(&outside, "unchanged").unwrap();
        symlink(&outside, cwd.join(".paloma/attach/note")).unwrap();
        symlink(&outside, cwd.join(".paloma/attach.md")).unwrap();
        let payload = MissionPayload {
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::File,
                path: Some("note".into()),
            }],
            ..Default::default()
        };
        materialize(&cwd, &root, &payload).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "unchanged");
        assert_eq!(
            std::fs::read_to_string(cwd.join(".paloma/attach/note")).unwrap(),
            "public"
        );
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        symlink(&cwd, other.join(".paloma")).unwrap();
        assert!(materialize(&other, &root, &payload).is_err());
    }

    #[test]
    fn message_snapshots_keep_distinct_file_versions_and_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("files");
        std::fs::create_dir(&root).unwrap();
        let payload = MissionPayload {
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::File,
                path: Some("note".into()),
            }],
            ..Default::default()
        };
        for version in ["first", "second"] {
            std::fs::write(root.join("note"), version).unwrap();
            materialize(&tmp.path().join(version), &root, &payload).unwrap();
        }
        for version in ["first", "second"] {
            assert_eq!(
                std::fs::read_to_string(tmp.path().join(version).join(".paloma/attach/note"))
                    .unwrap(),
                version
            );
            assert!(tmp.path().join(version).join(".paloma/attach.md").is_file());
        }
    }

    #[test]
    fn source_top_folder_and_ancestor_symlinks_are_skipped() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("files");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(outside.join("nested")).unwrap();
        std::fs::write(outside.join("nested/private"), "must not copy").unwrap();
        symlink(&outside, root.join("top")).unwrap();
        for path in ["top", "top/nested"] {
            let payload = MissionPayload {
                attachments: vec![MissionAttachment {
                    kind: AttachmentKind::Folder,
                    path: Some(path.into()),
                }],
                ..Default::default()
            };
            let (files, report) = prepare(&root, &payload).unwrap();
            assert_eq!(files.len(), 1, "only manifest, {path}");
            assert!(!report.skipped.is_empty(), "explain top folder exclusion");
        }
        let dir = directory(&outside, false).unwrap();
        std::fs::rename(&outside, tmp.path().join("moved")).unwrap();
        symlink(&root, &outside).unwrap();
        let mut files = Vec::new();
        let mut report = MaterializeReport::default();
        collect_files(
            &dir,
            Path::new("original"),
            &mut files,
            &mut 0,
            0,
            &mut report,
        );
        assert_eq!(
            files.len(),
            1,
            "enumeration stays attached to the opened directory"
        );
        assert_eq!(
            bounded_read_file(files.pop().unwrap().1, 100).unwrap(),
            b"must not copy"
        );
    }

    #[test]
    fn folder_traversal_stops_at_file_entry_and_depth_caps() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..100 {
            std::fs::write(tmp.path().join(format!("f{i}")), "x").unwrap();
        }
        let dir = directory(tmp.path(), false).unwrap();
        let mut files = Vec::new();
        let mut visited = 0;
        let mut report = MaterializeReport::default();
        collect_files(
            &dir,
            Path::new("notes"),
            &mut files,
            &mut visited,
            0,
            &mut report,
        );
        assert_eq!(files.len(), FOLDER_FILE_CAP);
        assert_eq!(visited, FOLDER_FILE_CAP);
        assert!(report.truncated);
        files.clear();
        visited = 3999;
        collect_files(
            &dir,
            Path::new("notes"),
            &mut files,
            &mut visited,
            0,
            &mut report,
        );
        assert_eq!(visited, 4000);
        assert_eq!(files.len(), 1);
        files.clear();
        collect_files(
            &dir,
            Path::new("notes"),
            &mut files,
            &mut 0,
            33,
            &mut report,
        );
        assert!(files.is_empty());
    }

    #[test]
    fn staged_messages_are_immutable_scoped_and_replayed_only_at_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project_files_root(tmp.path(), "lido");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note"), "accepted version").unwrap();
        let payload = MissionPayload {
            project: Some("lido".into()),
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::File,
                path: Some("note".into()),
            }],
            ..Default::default()
        };
        let mid = Uuid::new_v4();
        let id = Uuid::new_v4();
        let (message, _) = stage_message(tmp.path(), mid, id, "read", &payload).unwrap();
        assert!(!cwd.exists());
        std::fs::write(root.join("note"), "later version").unwrap();
        assert_eq!(
            stage_message(tmp.path(), mid, id, "read", &payload)
                .unwrap()
                .0,
            message
        );
        assert!(stage_message(tmp.path(), mid, id, "different", &payload).is_err());
        std::fs::remove_dir_all(&root).unwrap();
        // Reconstruct from the durable reference alone, including a scheduler
        // prompt containing multiple original message references.
        let combined = format!("{message}\n{message}");
        materialize_turn(tmp.path(), &cwd, mid, &combined).unwrap();
        let copied = cwd.join(format!(".paloma/messages/{id}/.paloma/attach/note"));
        assert_eq!(
            std::fs::read_to_string(&copied).unwrap(),
            "accepted version"
        );
        std::fs::remove_dir_all(&cwd).unwrap();
        materialize_turn(tmp.path(), &cwd, mid, &message).unwrap();
        assert_eq!(std::fs::read_to_string(copied).unwrap(), "accepted version");
        assert!(materialize_turn(tmp.path(), &cwd, Uuid::new_v4(), &message).is_err());
        std::fs::remove_file(message_snapshot_path(tmp.path(), mid, id)).unwrap();
        assert!(
            materialize_turn(tmp.path(), &cwd, mid, &message).is_err(),
            "missing payload cannot silently dispatch"
        );
    }

    #[test]
    fn dispatch_rejects_destination_ancestor_and_top_folder_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = project_files_root(tmp.path(), "lido");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note"), "data").unwrap();
        let payload = MissionPayload {
            project: Some("lido".into()),
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::File,
                path: Some("note".into()),
            }],
            ..Default::default()
        };
        let mid = Uuid::new_v4();
        let (message, _) =
            stage_message(tmp.path(), mid, Uuid::new_v4(), "read", &payload).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let alias = tmp.path().join("alias");
        symlink(&outside, &alias).unwrap();
        for cwd in [alias.clone(), alias.join("nested")] {
            assert!(materialize_turn(tmp.path(), &cwd, mid, &message).is_err());
        }
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn dispatch_uses_resolved_container_cwd_and_relative_manifest_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let rootfs = tmp.path().join("rootfs");
        std::fs::create_dir_all(rootfs.join("workspace/checkout")).unwrap();
        let files = project_files_root(tmp.path(), "lido");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("note"), "container context").unwrap();
        let payload = MissionPayload {
            project: Some("lido".into()),
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::File,
                path: Some("note".into()),
            }],
            ..Default::default()
        };
        let mid = Uuid::new_v4();
        let id = Uuid::new_v4();
        let (message, _) = stage_message(tmp.path(), mid, id, "read", &payload).unwrap();
        let cwd = super::super::mission_runner::resolve_mission_working_directory(
            &rootfs,
            crate::workspace::WorkspaceType::Container,
            "/workspace/checkout",
        )
        .unwrap();
        materialize_turn(tmp.path(), &cwd, mid, &message).unwrap();
        let relative = format!(".paloma/messages/{id}/.paloma/attach.md");
        assert!(message.contains(&relative));
        assert!(!message.contains(rootfs.to_str().unwrap()));
        assert!(rootfs.join("workspace/checkout").join(relative).is_file());
    }

    #[test]
    fn controller_payload_supports_operator_storage_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let host = tmp.path().join("host");
        let volume = tmp.path().join("storage");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(&volume).unwrap();
        symlink(&volume, host.join(".sandboxed-sh")).unwrap();
        let mid = Uuid::new_v4();
        let payload = MissionPayload {
            project: Some("pareto".into()),
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::Controller,
                path: None,
            }],
            controller_md: Some("# Pareto controller snapshot".into()),
        };
        let prompt = "Peux-tu me faire un résumé du status de l’audit Pareto? Tu peux check le travail fait par @controller et me dire où on en est (les garanties choisies, les properties qui en découlent, classées en 3 catégories, et ce qui a été formalisé en Verity / Lean [est-ce que c’est parfait ou est-ce qu’il reste des choses à corriger sur les modèles / specs pour que ce soit rigoureux], puis ce qui a été prouvé et ce qui ne l’est pas)";
        write_sidecar(&host, mid, &payload).unwrap();
        assert_eq!(read_sidecar(&host, mid).unwrap(), Some(payload.clone()));
        assert!(volume
            .join("mission-payloads")
            .join(format!("{mid}.json"))
            .is_file());
        let (message, _) = stage_message(&host, mid, Uuid::new_v4(), prompt, &payload).unwrap();
        let cwd = tmp.path().join("mission");
        std::fs::create_dir_all(&cwd).unwrap();
        materialize_turn(&host, &cwd, mid, &message).unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join(".paloma/controller.md")).unwrap(),
            "# Pareto controller snapshot"
        );
        // Resolving the configured volume must not permit a symlink below it.
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::rename(
            volume.join("mission-payloads"),
            volume.join("saved-payloads"),
        )
        .unwrap();
        symlink(&outside, volume.join("mission-payloads")).unwrap();
        assert!(write_sidecar(&host, Uuid::new_v4(), &payload).is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn sidecar_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let payload = MissionPayload {
            project: Some("lido".into()),
            attachments: vec![MissionAttachment {
                kind: AttachmentKind::Controller,
                path: None,
            }],
            controller_md: Some("snap".into()),
        };
        write_sidecar(tmp.path(), id, &payload).unwrap();
        let read = read_sidecar(tmp.path(), id).unwrap().unwrap();
        assert_eq!(read, payload);
    }
}

#[cfg(test)]
mod atomic_publish_tests {
    #[test]
    fn exclusive_publish_has_one_winner_and_preserves_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("payload");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4u8)
            .map(|byte| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        byte,
                        super::atomic_write(&path, &[byte; 1024], true).unwrap(),
                    )
                })
            })
            .collect();
        let winners: Vec<_> = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|(_, won)| *won)
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), vec![winners[0].0; 1024]);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
