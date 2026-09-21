//! Materialize Orb `@` chips into ordinary files before a harness starts.
//!
//! Paloma writes `.paloma/attach/…`, `.paloma/controller.md`, and
//! `.paloma/attach.md`. Vendor CLIs only see paths.

use std::path::{Component, Path, PathBuf};

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
    let path = sidecar_path(working_dir, mission_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
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
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn is_secret_path(rel: &str) -> bool {
    let lower = rel.replace('\\', "/").to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if lower.split('/').any(|part| part == ".git") {
        return true;
    }
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == "id_rsa"
        || name.starts_with("id_rsa.")
        || name.starts_with("id_ed25519")
        || name == "credentials"
        || name == "secrets.json"
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
}

fn safe_rel(rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() {
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
    std::fs::create_dir_all(paloma.join("attach")).map_err(|e| e.to_string())?;
    let mut report = MaterializeReport::default();
    let mut manifest = String::from("# Attached context\n\nYou were given these paths. Read them; do not invent Paloma-specific `@` syntax.\n\n");

    for attachment in &payload.attachments {
        match attachment.kind {
            AttachmentKind::Controller => {
                let dest = paloma.join("controller.md");
                let body = payload.controller_md.as_deref().unwrap_or(
                    "# Controller snapshot\n\nNo controller snapshot was available when this mission started.\n",
                );
                std::fs::write(&dest, body).map_err(|e| e.to_string())?;
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
                let bytes = std::fs::read(&src).map_err(|e| e.to_string())?;
                if bytes.len() > FILE_BYTE_CAP {
                    report
                        .skipped
                        .push(format!("{rel_str} (over {FILE_BYTE_CAP} bytes)"));
                    continue;
                }
                let dest = paloma.join("attach").join(&rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
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
                collect_files(&src_dir, &rel, &mut files);
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
                    let bytes = std::fs::read(&abs).map_err(|e| e.to_string())?;
                    let dest = paloma.join("attach").join(&rel_file);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
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
    std::fs::write(paloma.join("attach.md"), manifest).map_err(|e| e.to_string())?;
    report.written.push(".paloma/attach.md".into());
    Ok(report)
}

fn collect_files(dir: &Path, prefix: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let rel = prefix.join(&name);
        let abs = entry.path();
        if abs.is_dir() {
            collect_files(&abs, &rel, out);
        } else {
            out.push((rel, abs));
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
