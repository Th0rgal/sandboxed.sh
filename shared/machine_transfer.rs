//! Bounded workspace checkpoints shared by Core, nodes and Orb. Roots are supplied
//! by the trusted host adapter, never by the network caller.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

pub const BLOCK: usize = 1024 * 1024;
pub const MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 50_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub executable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub files: Vec<Entry>,
    pub excluded: Vec<String>,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Snapshot,
    CheckSource,
    Read {
        path: String,
        offset: u64,
    },
    Stage {
        manifest: Manifest,
    },
    Write {
        path: String,
        offset: u64,
        data: String,
    },
    Verify,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn relative(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Invalid checkpoint path".into());
    }
    Ok(path.to_owned())
}
fn confined(root: &Path, value: &str) -> Result<PathBuf, String> {
    let path = relative(value)?;
    let mut current = root.to_owned();
    if fs::symlink_metadata(root)
        .map_err(err)?
        .file_type()
        .is_symlink()
    {
        return Err("Checkpoint root is a symlink".into());
    }
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("Symlink is not transferable: {value}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(err(e)),
            _ => {}
        }
    }
    Ok(current)
}
fn excluded(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        ".git"
            | ".transfers"
            | "node_modules"
            | "target"
            | ".next"
            | ".cache"
            | "__pycache__"
            | ".venv"
            | ".ssh"
            | ".aws"
            | ".codex"
            | ".claude"
            | ".opencode"
            | ".grok"
            | ".gemini"
            | ".gnupg"
            | ".npmrc"
            | ".netrc"
            | ".pypirc"
            | ".envrc"
            | ".git-credentials"
            | "secrets"
            | "secrets.json"
            | "secrets.yaml"
            | "secrets.yml"
            | "credentials.json"
            | "auth.json"
            | "opencode.json"
    ) || n == ".env"
        || (n.starts_with(".env.") && !n.ends_with(".example"))
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".pfx")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.contains("credentials")
}
fn digest(path: &Path) -> Result<String, String> {
    digest_beneath(path.parent().ok_or("Invalid file")?, path)
}
fn digest_beneath(root: &Path, path: &Path) -> Result<String, String> {
    let mut input = crate::file_browser::open_beneath(root, path)?;
    if !input.metadata().map_err(err)?.is_file() {
        return Err("Not a regular file".into());
    }
    let mut hash = Sha256::new();
    let mut buf = vec![0; BLOCK];
    loop {
        let n = input.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn inventory(root: &Path, dir: &Path, manifest: &mut Manifest) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    entries.sort_by_key(|e| e.file_name());
    for item in entries {
        let path = item.path();
        let rel = path
            .strip_prefix(root)
            .map_err(err)?
            .to_str()
            .ok_or("Non-UTF-8 filename")?
            .replace('\\', "/");
        let name = item.file_name();
        let name = name.to_str().ok_or("Non-UTF-8 filename")?;
        if excluded(name) || name == ".transfer-git.bundle" {
            manifest.excluded.push(rel);
            if manifest.excluded.len() > MAX_FILES {
                return Err("Too many excluded paths".into());
            }
            continue;
        }
        let m = fs::symlink_metadata(&path).map_err(err)?;
        if m.is_dir() {
            inventory(root, &path, manifest)?;
            continue;
        }
        if !m.is_file() {
            return Err(format!(
                "Resolve symlink or special file before moving: {rel}"
            ));
        }
        manifest.bytes = manifest
            .bytes
            .checked_add(m.len())
            .ok_or("Workspace size overflow")?;
        if manifest.bytes > MAX_BYTES || manifest.files.len() >= MAX_FILES {
            return Err("Workspace exceeds transfer limit (10 GiB / 50,000 files)".into());
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        let sha256 = digest_beneath(root, &path)?;
        let after = fs::symlink_metadata(&path).map_err(err)?;
        if m.len() != after.len() || m.modified().ok() != after.modified().ok() {
            return Err(format!("File changed during snapshot: {rel}"));
        }
        manifest.files.push(Entry {
            path: rel,
            bytes: m.len(),
            sha256,
            executable,
        });
    }
    Ok(())
}
fn manifest_path(area: &Path) -> PathBuf {
    area.join("manifest.json")
}
fn load(area: &Path) -> Result<Manifest, String> {
    serde_json::from_slice(&fs::read(manifest_path(area)).map_err(err)?).map_err(err)
}
fn validate(m: &Manifest) -> Result<(), String> {
    if serde_json::to_vec(m).map_err(err)?.len() > 8 * 1024 * 1024 {
        return Err("Workspace inventory exceeds 8 MiB".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    if m.files.len() > MAX_FILES {
        return Err("Too many files".into());
    }
    for f in &m.files {
        relative(&f.path)?;
        if f.path.split('/').any(|part| excluded(part)) {
            return Err("Excluded path in manifest".into());
        }
        if !seen.insert(f.path.clone())
            || f.sha256.len() != 64
            || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid manifest".into());
        }
        total = total.checked_add(f.bytes).ok_or("Size overflow")?;
        if total > MAX_BYTES {
            return Err("Checkpoint exceeds 10 GiB".into());
        }
    }
    if total != m.bytes {
        return Err("Manifest size mismatch".into());
    }
    for f in &m.files {
        let mut p = Path::new(&f.path);
        while let Some(parent) = p.parent() {
            if seen.contains(parent.to_str().unwrap_or("")) {
                return Err("Overlapping checkpoint paths".into());
            }
            p = parent;
        }
    }
    Ok(())
}
fn save(area: &Path, m: &Manifest) -> Result<(), String> {
    let bytes = serde_json::to_vec(m).map_err(err)?;
    let temporary = area.join("manifest.pending");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(err)?;
    f.write_all(&bytes).map_err(err)?;
    f.sync_all().map_err(err)?;
    fs::rename(temporary, manifest_path(area)).map_err(err)?;
    File::open(area).map_err(err)?.sync_all().map_err(err)
}
fn git(root: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(err)?;
    if !out.status.success() {
        return Err("Git checkpoint failed; inspect the repository before moving".into());
    }
    Ok(())
}
fn bundle_repository(repo: &Path, destination: &Path, limit: u64) -> Result<u64, String> {
    let mut output = File::create(destination).map_err(err)?;
    let limit = limit.min(
        fs2::available_space(destination.parent().ok_or("Invalid bundle path")?)
            .map_err(err)?
            .saturating_sub(BLOCK as u64),
    );
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["bundle", "create", "-", "--all"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(err)?;
    let copied = std::io::copy(
        &mut child
            .stdout
            .take()
            .ok_or("Git output unavailable")?
            .take(limit.saturating_add(1)),
        &mut output,
    );
    if copied.as_ref().is_err() || copied.as_ref().is_ok_and(|n| *n > limit) {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Git history exceeds transfer size or available disk space".into());
    }
    if !child.wait().map_err(err)?.success() {
        return Err("Could not checkpoint Git history".into());
    }
    output.sync_all().map_err(err)?;
    copied.map_err(err)
}

/// `area` is a host-owned, unique per-action directory; `source` exists only on
/// the source adapter. The restored root is always area/workspace.
pub fn operate(
    area: &Path,
    source: Option<&Path>,
    op: Operation,
) -> Result<serde_json::Value, String> {
    fs::create_dir_all(area).map_err(err)?;
    let root = area.join("workspace");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(area.join("lock"))
        .map_err(err)?;
    fs2::FileExt::lock_exclusive(&lock).map_err(err)?;
    match op {
        Operation::Snapshot => {
            if manifest_path(area).exists() {
                return Ok(serde_json::json!(load(area)?));
            }
            let source = source
                .ok_or("Source workspace unavailable")?
                .canonicalize()
                .map_err(err)?;
            if area.starts_with(&source) {
                return Err("Checkpoint directory must be outside the workspace".into());
            }
            if root.exists() {
                fs::remove_dir_all(&root).map_err(err)?;
            }
            fs::create_dir_all(&root).map_err(err)?;
            let mut m = Manifest {
                files: vec![],
                excluded: vec![],
                bytes: 0,
            };
            inventory(&source, &source, &mut m)?;
            if fs2::available_space(area).map_err(err)? < m.bytes.saturating_add(BLOCK as u64) {
                return Err("Insufficient snapshot disk space".into());
            }
            for f in &m.files {
                let from = confined(&source, &f.path)?;
                let to = confined(&root, &f.path)?;
                fs::create_dir_all(to.parent().unwrap()).map_err(err)?;
                let input = crate::file_browser::open_beneath(&source, &from)?;
                if !input.metadata().map_err(err)?.is_file() {
                    return Err("Source is not a regular file".into());
                }
                let mut output = File::create(&to).map_err(err)?;
                let copied = std::io::copy(&mut input.take(f.bytes.saturating_add(1)), &mut output)
                    .map_err(err)?;
                if copied != f.bytes {
                    return Err("File changed during snapshot".into());
                }
                output.sync_all().map_err(err)?;
                if digest(&to)? != f.sha256 {
                    return Err(format!("File changed during snapshot: {}", f.path));
                }
            }
            let mut after = Manifest {
                files: vec![],
                excluded: vec![],
                bytes: 0,
            };
            inventory(&source, &source, &mut after)?;
            if after != m {
                return Err("Workspace changed during snapshot; stop all writers and retry".into());
            }
            let repositories: Vec<_> = m
                .excluded
                .iter()
                .filter(|p| Path::new(p).file_name().is_some_and(|n| n == ".git"))
                .cloned()
                .collect();
            for git_path in repositories {
                let rel = Path::new(&git_path).parent().unwrap_or(Path::new(""));
                let repo = source.join(rel);
                let bundle = root.join(rel).join(".transfer-git.bundle");
                fs::create_dir_all(bundle.parent().unwrap()).map_err(err)?;
                let refs = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(["show-ref"])
                    .output()
                    .map_err(err)?;
                if !refs.status.success() && refs.status.code() != Some(1) {
                    return Err("Cannot read Git history".into());
                }
                if refs.status.success() {
                    let bytes =
                        bundle_repository(&repo, &bundle, MAX_BYTES.saturating_sub(m.bytes))?;
                    m.bytes = m
                        .bytes
                        .checked_add(bytes)
                        .ok_or("Workspace size overflow")?;
                    m.files.push(Entry {
                        path: bundle
                            .strip_prefix(&root)
                            .map_err(err)?
                            .to_str()
                            .ok_or("Invalid path")?
                            .into(),
                        bytes,
                        sha256: digest(&bundle)?,
                        executable: false,
                    });
                }
            }
            validate(&m)?;
            save(area, &m)?;
            Ok(serde_json::json!(m))
        }
        Operation::CheckSource => {
            let source = source
                .ok_or("Source workspace unavailable")?
                .canonicalize()
                .map_err(err)?;
            let expected = load(area)?;
            let mut current = Manifest {
                files: vec![],
                excluded: vec![],
                bytes: 0,
            };
            inventory(&source, &source, &mut current)?;
            let files: Vec<_> = expected
                .files
                .iter()
                .filter(|f| !f.path.ends_with(".transfer-git.bundle"))
                .cloned()
                .collect();
            if current.files != files {
                return Err(
                    "Source files changed after preparation; cancel and prepare again".into(),
                );
            }
            for bundle in expected
                .files
                .iter()
                .filter(|f| f.path.ends_with(".transfer-git.bundle"))
            {
                let relative = Path::new(&bundle.path).parent().unwrap_or(Path::new(""));
                let refs = Command::new("git")
                    .arg("-C")
                    .arg(source.join(relative))
                    .args(["show-ref"])
                    .output()
                    .map_err(err)?;
                let bundled = Command::new("git")
                    .args(["bundle", "list-heads"])
                    .arg(root.join(&bundle.path))
                    .output()
                    .map_err(err)?;
                if !refs.status.success() || !bundled.status.success() {
                    return Err("Git source is no longer available".into());
                }
                let normalize = |bytes: &[u8]| {
                    let text = String::from_utf8_lossy(bytes);
                    let mut lines: Vec<_> = text
                        .lines()
                        .filter(|s| !s.ends_with(" HEAD"))
                        .map(str::to_owned)
                        .collect();
                    lines.sort();
                    lines
                };
                if normalize(&refs.stdout) != normalize(&bundled.stdout) {
                    return Err("Git history changed after preparation; prepare again".into());
                }
            }
            Ok(serde_json::json!({"unchanged":true}))
        }
        Operation::Stage { manifest } => {
            validate(&manifest)?;
            if manifest_path(area).exists() {
                if load(area)? != manifest {
                    return Err("Transfer manifest differs from staged checkpoint".into());
                }
            } else {
                if fs2::available_space(area).map_err(err)?
                    < manifest.bytes.saturating_add(BLOCK as u64)
                {
                    return Err("Insufficient destination disk space".into());
                }
                fs::create_dir_all(&root).map_err(err)?;
                save(area, &manifest)?;
            }
            let received: std::collections::BTreeMap<_, _> = manifest
                .files
                .iter()
                .map(|f| {
                    let size = confined(&root, &f.path)
                        .ok()
                        .and_then(|p| fs::metadata(p).ok())
                        .map(|m| m.len().min(f.bytes))
                        .unwrap_or(0);
                    (
                        f.path.clone(),
                        if size == f.bytes {
                            size
                        } else {
                            size / BLOCK as u64 * BLOCK as u64
                        },
                    )
                })
                .collect();
            Ok(
                serde_json::json!({"ok":true,"sealed":area.join("verified").exists(),"received":received}),
            )
        }
        Operation::Read { path, offset } => {
            let m = load(area)?;
            let f = m
                .files
                .iter()
                .find(|f| f.path == path)
                .ok_or("File absent from manifest")?;
            if offset > f.bytes || offset % BLOCK as u64 != 0 {
                return Err("Invalid block offset".into());
            }
            let mut file = File::open(confined(&root, &path)?).map_err(err)?;
            file.seek(SeekFrom::Start(offset)).map_err(err)?;
            let mut data = vec![0; (f.bytes - offset).min(BLOCK as u64) as usize];
            file.read_exact(&mut data).map_err(err)?;
            Ok(serde_json::json!({"data":STANDARD.encode(data)}))
        }
        Operation::Write { path, offset, data } => {
            if area.join("verified").exists() {
                return Err("Checkpoint already sealed".into());
            }
            let m = load(area)?;
            let f = m
                .files
                .iter()
                .find(|f| f.path == path)
                .ok_or("File absent from manifest")?;
            if data.len() > BLOCK * 2 {
                return Err("Oversized block".into());
            }
            let bytes = STANDARD.decode(data).map_err(err)?;
            if offset > f.bytes
                || offset % BLOCK as u64 != 0
                || bytes.len() as u64 != (f.bytes - offset).min(BLOCK as u64)
            {
                return Err("Invalid block extent".into());
            }
            let to = confined(&root, &path)?;
            fs::create_dir_all(to.parent().unwrap()).map_err(err)?;
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(to)
                .map_err(err)?;
            file.seek(SeekFrom::Start(offset)).map_err(err)?;
            file.write_all(&bytes).map_err(err)?;
            file.sync_data().map_err(err)?;
            Ok(serde_json::json!({"ok":true}))
        }
        Operation::Verify => {
            let m = load(area)?;
            validate(&m)?;
            if !area.join("verified").exists() {
                for f in &m.files {
                    let path = confined(&root, &f.path)?;
                    if fs::metadata(&path).map_err(err)?.len() != f.bytes
                        || digest(&path)? != f.sha256
                    {
                        return Err(format!("Checkpoint mismatch: {}", f.path));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(
                            path,
                            fs::Permissions::from_mode(if f.executable { 0o700 } else { 0o600 }),
                        )
                        .map_err(err)?;
                    }
                }
                for (i, f) in m.files.iter().enumerate().filter(|(_, f)| {
                    Path::new(&f.path)
                        .file_name()
                        .is_some_and(|n| n == ".transfer-git.bundle")
                }) {
                    let bundle = confined(&root, &f.path)?;
                    let working = bundle.parent().ok_or("Invalid repository path")?;
                    let repo = area.join(format!("git-restore-{i}"));
                    if repo.exists() {
                        fs::remove_dir_all(&repo).map_err(err)?;
                    }
                    git(
                        area,
                        &[
                            "clone",
                            "--mirror",
                            bundle.to_str().ok_or("Invalid path")?,
                            repo.to_str().ok_or("Invalid path")?,
                        ],
                    )?;
                    git(&repo, &["config", "--remove-section", "remote.origin"])?;
                    if !working.join(".git").exists() {
                        fs::rename(&repo, working.join(".git")).map_err(err)?;
                    }
                    git(working, &["config", "core.bare", "false"])?;
                    git(working, &["reset", "--mixed", "HEAD"])?;
                }
                let mut marker = File::create(area.join("verified")).map_err(err)?;
                marker
                    .write_all(digest(&manifest_path(area))?.as_bytes())
                    .map_err(err)?;
                marker.sync_all().map_err(err)?;
            }
            Ok(
                serde_json::json!({"root":root,"digest":digest(&manifest_path(area))?,"bytes":m.bytes,"files":m.files.len()}),
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_binary_and_excludes_credentials_and_retries_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("é.bin"), vec![17; BLOCK + 7]).unwrap();
        fs::write(src.join(".env"), "secret").unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let m: Manifest =
            serde_json::from_value(operate(&a, Some(&src), Operation::Snapshot).unwrap()).unwrap();
        assert_eq!(m.excluded, vec![".env"]);
        operate(
            &b,
            None,
            Operation::Stage {
                manifest: m.clone(),
            },
        )
        .unwrap();
        for f in &m.files {
            for off in (0..f.bytes).step_by(BLOCK) {
                let read = operate(
                    &a,
                    None,
                    Operation::Read {
                        path: f.path.clone(),
                        offset: off,
                    },
                )
                .unwrap();
                let op = Operation::Write {
                    path: f.path.clone(),
                    offset: off,
                    data: read["data"].as_str().unwrap().into(),
                };
                operate(&b, None, op.clone()).unwrap();
                operate(&b, None, op).unwrap();
            }
        }
        operate(&b, None, Operation::Verify).unwrap();
        assert_eq!(
            fs::read(src.join("é.bin")).unwrap(),
            fs::read(b.join("workspace/é.bin")).unwrap()
        );
        assert!(!b.join("workspace/.env").exists());
    }
    #[test]
    fn rejects_traversal_and_corrupt_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest {
            files: vec![Entry {
                path: "../escape".into(),
                bytes: 0,
                sha256: "0".repeat(64),
                executable: false,
            }],
            excluded: vec![],
            bytes: 0,
        };
        assert!(operate(dir.path(), None, Operation::Stage { manifest: m }).is_err());
        assert!(relative("/absolute").is_err());
        assert!(relative("a/../b").is_err());
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    fn copy(a: &Path, b: &Path, m: &Manifest) {
        operate(
            b,
            None,
            Operation::Stage {
                manifest: m.clone(),
            },
        )
        .unwrap();
        for f in &m.files {
            for offset in (0..f.bytes.max(1)).step_by(BLOCK) {
                let data = operate(
                    a,
                    None,
                    Operation::Read {
                        path: f.path.clone(),
                        offset,
                    },
                )
                .unwrap()["data"]
                    .as_str()
                    .unwrap()
                    .into();
                operate(
                    b,
                    None,
                    Operation::Write {
                        path: f.path.clone(),
                        offset,
                        data,
                    },
                )
                .unwrap();
            }
        }
    }
    #[test]
    fn machine_transfer_detects_corruption_and_supports_sealed_retry() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("empty"), "").unwrap();
        fs::write(src.join("file"), "original").unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let m: Manifest =
            serde_json::from_value(operate(&a, Some(&src), Operation::Snapshot).unwrap()).unwrap();
        copy(&a, &b, &m);
        fs::write(b.join("workspace/file"), "corrupt!").unwrap();
        assert!(operate(&b, None, Operation::Verify).is_err());
        fs::write(b.join("workspace/file"), "original").unwrap();
        let receipt = operate(&b, None, Operation::Verify).unwrap();
        assert_eq!(receipt, operate(&b, None, Operation::Verify).unwrap());
        assert_eq!(
            operate(&b, None, Operation::Stage { manifest: m }).unwrap()["sealed"],
            true
        );
        assert!(operate(
            &b,
            None,
            Operation::Write {
                path: "file".into(),
                offset: 0,
                data: STANDARD.encode(b"changed!")
            }
        )
        .is_err());
    }
    #[cfg(unix)]
    #[test]
    fn machine_transfer_refuses_symlinks_and_preserves_executable_bits() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir(&src).unwrap();
        symlink("/etc/passwd", src.join("escape")).unwrap();
        let a = temp.path().join("a");
        assert!(operate(&a, Some(&src), Operation::Snapshot).is_err());
        fs::remove_file(src.join("escape")).unwrap();
        fs::write(src.join("run.sh"), "#!/bin/sh\ntrue\n").unwrap();
        fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let m: Manifest =
            serde_json::from_value(operate(&a, Some(&src), Operation::Snapshot).unwrap()).unwrap();
        let b = temp.path().join("b");
        copy(&a, &b, &m);
        operate(&b, None, Operation::Verify).unwrap();
        assert_ne!(
            fs::metadata(b.join("workspace/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    #[test]
    fn machine_transfer_preserves_git_history_and_uncommitted_work() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir(&src).unwrap();
        git(&src, &["init"]).unwrap();
        fs::write(src.join("file"), "committed").unwrap();
        git(&src, &["add", "file"]).unwrap();
        git(
            &src,
            &[
                "-c",
                "user.name=Transfer Test",
                "-c",
                "user.email=transfer@example.invalid",
                "commit",
                "-m",
                "checkpoint",
            ],
        )
        .unwrap();
        fs::write(src.join("file"), "uncommitted").unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let m: Manifest =
            serde_json::from_value(operate(&a, Some(&src), Operation::Snapshot).unwrap()).unwrap();
        copy(&a, &b, &m);
        operate(&b, None, Operation::Verify).unwrap();
        assert_eq!(
            fs::read_to_string(b.join("workspace/file")).unwrap(),
            "uncommitted"
        );
        git(&b.join("workspace"), &["rev-parse", "HEAD"]).unwrap();
        // A second move snapshots the updated repository, not the old bundle.
        let c = temp.path().join("c");
        operate(&c, Some(&b.join("workspace")), Operation::Snapshot).unwrap();
        operate(&c, Some(&b.join("workspace")), Operation::CheckSource).unwrap();
        fs::write(b.join("workspace/file"), "edited after preparation").unwrap();
        assert!(operate(&c, Some(&b.join("workspace")), Operation::CheckSource).is_err());
        fs::write(b.join("workspace/file"), "uncommitted").unwrap();
        git(&b.join("workspace"), &["branch", "created-after-snapshot"]).unwrap();
        assert!(operate(&c, Some(&b.join("workspace")), Operation::CheckSource).is_err());
    }
}
