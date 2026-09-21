//! Materialize Orb `@` chips into ordinary files before a harness starts.
//!
//! Paloma writes `.paloma/attach/…`, `.paloma/controller.md`, and
//! `.paloma/attach.md`. Vendor CLIs only see paths.

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
            libc::renameat(
                parent.as_raw_fd(),
                temp_name.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    })();
    if result.is_err() {
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializeReport {
    pub written: Vec<String>,
    pub skipped: Vec<String>,
    pub truncated: bool,
}

pub fn project_files_root(working_dir: &Path, slug: &str) -> PathBuf {
    working_dir
        .join(".sandboxed-sh")
        .join("project-files")
        .join(slug)
}

pub fn sidecar_path(working_dir: &Path, mission_id: Uuid) -> PathBuf {
    working_dir
        .join(".sandboxed-sh")
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
    if !path.exists() {
        return Ok(None);
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
    let paloma = cwd.join(".paloma");
    validate(payload)?;
    directory(&paloma.join("attach"), true)?;
    let mut report = MaterializeReport::default();
    let mut manifest = String::from("# Attached context\n\nYou were given these paths. Read them; do not invent Paloma-specific `@` syntax.\n\n");

    for attachment in &payload.attachments {
        match attachment.kind {
            AttachmentKind::Controller => {
                let dest = paloma.join("controller.md");
                let body = payload.controller_md.as_deref().unwrap_or(
                    "# Controller snapshot\n\nNo controller snapshot was available when this mission started.\n",
                );
                safe_write(&dest, body.as_bytes())?;
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
                if !src.is_file() {
                    report.skipped.push(format!("{rel_str} (missing)"));
                    continue;
                }
                let bytes = match bounded_read(&src, FILE_BYTE_CAP) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        report.skipped.push(format!("{rel_str} ({error})"));
                        continue;
                    }
                };
                if bytes.len() > FILE_BYTE_CAP {
                    report
                        .skipped
                        .push(format!("{rel_str} (over {FILE_BYTE_CAP} bytes)"));
                    continue;
                }
                let dest = paloma.join("attach").join(&rel);
                safe_write(&dest, &bytes)?;
                let dest_rel = format!(".paloma/attach/{rel_str}");
                report.written.push(dest_rel.clone());
                manifest.push_str(&format!("- `{dest_rel}` (from `{rel_str}`)\n"));
            }
            AttachmentKind::Folder => {
                let rel = safe_rel(attachment.path.as_deref().unwrap_or(""))?;
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let src_dir = project_files_root.join(&rel);
                if !src_dir.is_dir() {
                    report.skipped.push(format!("{rel_str}/ (missing)"));
                    continue;
                }
                let mut files = Vec::new();
                let mut visited = 0;
                collect_files(&src_dir, &rel, &mut files, &mut visited, 0, &mut report);
                files.sort();
                let mut used = 0usize;
                let mut count = 0usize;
                for (rel_file, abs) in files {
                    let rel_file_str = rel_file.to_string_lossy().replace('\\', "/");
                    if is_secret_path(&rel_file_str) {
                        report.skipped.push(format!("{rel_file_str} (secret)"));
                        continue;
                    }
                    let meta = match std::fs::metadata(&abs) {
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
                    let bytes = match bounded_read(&abs, FOLDER_BYTE_CAP - used) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            report.truncated = true;
                            report.skipped.push(format!("{rel_file_str} ({error})"));
                            continue;
                        }
                    };
                    let dest = paloma.join("attach").join(&rel_file);
                    safe_write(&dest, &bytes)?;
                    used += bytes.len();
                    count += 1;
                    let dest_rel = format!(".paloma/attach/{rel_file_str}");
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
    safe_write(&paloma.join("attach.md"), manifest.as_bytes())?;
    report.written.push(".paloma/attach.md".into());
    Ok(report)
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
        if attachment.kind != AttachmentKind::Controller {
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
    dir: &Path,
    prefix: &Path,
    out: &mut Vec<(PathBuf, PathBuf)>,
    visited: &mut usize,
    depth: usize,
    report: &mut MaterializeReport,
) {
    if depth > 32 || *visited >= 4000 {
        report.truncated = true;
        return;
    }
    if directory(dir, false).is_err() {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        *visited += 1;
        if *visited > 4000 {
            report.truncated = true;
            break;
        }
        let rel = prefix.join(entry.file_name());
        if is_secret_path(&rel.to_string_lossy()) {
            report.skipped.push(format!("{} (secret)", rel.display()));
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_files(&entry.path(), &rel, out, visited, depth + 1, report);
        } else if kind.is_file() {
            out.push((rel, entry.path()));
        }
    }
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
