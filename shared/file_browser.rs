//! Read-only, bounded filesystem operations shared by core, nodes and Orb.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Request {
    pub action: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub offset: u64,
}
const LIMIT: usize = 1024 * 1024;
fn denied(path: &Path) -> bool {
    path.components().any(|c| {
        let n = c.as_os_str().to_string_lossy().to_lowercase();
        n == ".env"
            || n.starts_with(".env.")
            || [
                ".ssh",
                ".aws",
                ".gnupg",
                ".git",
                "credentials.json",
                ".credentials.json",
                "auth.json",
                "id_rsa",
                "id_ed25519",
                ".git-credentials",
            ]
            .contains(&n.as_str())
            || n.ends_with(".pem")
            || n.ends_with(".key")
    })
}
pub(crate) fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let input = Path::new(raw);
    let relative = if input.is_absolute() {
        input
            .strip_prefix(root)
            .map_err(|_| "File is outside this source")?
    } else {
        input
    };
    if relative.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) || denied(relative)
    {
        return Err("File is not available in this source".into());
    }
    // Do not follow symlinks: prevents races redirecting a validated read outside
    // its source and avoids traversing shared build/cache mounts.
    let mut path = root.to_path_buf();
    for c in relative.components() {
        path.push(c);
        let meta = fs::symlink_metadata(&path).map_err(|_| "File is unavailable")?;
        if meta.file_type().is_symlink() {
            return Err("Symbolic links are not exposed".into());
        }
    }
    Ok(path)
}
/// Walk from a directory descriptor: no checked-path/open race through symlinks.
#[cfg(unix)]
pub(crate) fn open_beneath(root: &Path, path: &Path) -> Result<fs::File, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let mut dir = fs::File::open(root).map_err(|_| "Source is unavailable")?;
    let parts: Vec<_> = path
        .strip_prefix(root)
        .map_err(|_| "Outside source")?
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let name =
            std::ffi::CString::new(part.as_os_str().as_bytes()).map_err(|_| "Invalid path")?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            };
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err("File is unavailable".into());
        }
        dir = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(dir)
}
#[cfg(not(unix))]
pub(crate) fn open_beneath(root: &Path, path: &Path) -> Result<fs::File, String> {
    if !path
        .canonicalize()
        .map_err(|_| "File unavailable")?
        .starts_with(root)
    {
        return Err("Outside source".into());
    }
    fs::File::open(path).map_err(|_| "File unavailable".into())
}
fn entry(root: &Path, path: &Path) -> Option<Value> {
    let rel = path.strip_prefix(root).ok()?;
    if denied(rel) {
        return None;
    }
    let m = fs::symlink_metadata(path).ok()?;
    if m.file_type().is_symlink() || !(m.is_dir() || m.is_file()) {
        return None;
    }
    Some(
        json!({"name":path.file_name()?.to_string_lossy(),"path":rel.to_string_lossy(),"kind":if m.is_dir(){"dir"}else{"file"},"size":m.len(),"modified":m.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t|t.as_secs())}),
    )
}
fn scan(root: &Path, query: &str, limit: usize) -> (Vec<Value>, bool) {
    let mut pending = vec![root.to_path_buf()];
    let mut out = Vec::new();
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                visited += 1;
                if visited > 20000 || out.len() >= limit {
                    return (out, true);
                }
                let Some(item) = entry(root, &e.path()) else {
                    continue;
                };
                let path = item["path"].as_str().unwrap_or("");
                if item["kind"] == "dir" {
                    if !["node_modules", "target", ".lake", ".cache"]
                        .contains(&e.file_name().to_string_lossy().as_ref())
                    {
                        pending.push(e.path());
                    }
                } else if path.to_lowercase().contains(&query.to_lowercase()) {
                    out.push(item);
                }
            }
        }
    }
    out.sort_by_key(|e| e["path"].as_str().unwrap_or("").to_owned());
    (out, false)
}
pub fn execute(root: &Path, req: &Request) -> Result<Value, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "File source is unavailable")?;
    match req.action.as_str() {
        "roots" => Ok(json!({"path":root.to_string_lossy()})),
        "resolve" => {
            if req.paths.len() > 64 {
                return Err("Too many references".into());
            }
            let mut index: Option<Vec<Value>> = None;
            let results: Vec<_> = req
                .paths
                .iter()
                .map(|p| {
                    let direct = resolve(&root, p)
                        .ok()
                        .and_then(|p| entry(&root, &p))
                        .filter(|e| e["kind"] == "file");
                    let matches = if let Some(e) = direct {
                        vec![e]
                    } else if !Path::new(p).is_absolute()
                        && !p.contains("..")
                        && !denied(Path::new(p))
                    {
                        index
                            .get_or_insert_with(|| scan(&root, "", 20000).0)
                            .iter()
                            .filter(|e| {
                                let s = e["path"].as_str().unwrap_or("");
                                s == p || s.ends_with(&format!("/{p}"))
                            })
                            .cloned()
                            .collect()
                    } else {
                        vec![]
                    };
                    json!({"reference":p,"matches":matches})
                })
                .collect();
            Ok(json!({"results":results}))
        }
        "search" => {
            let (entries, truncated) = scan(&root, &req.query, 200);
            Ok(json!({"entries":entries,"truncated":truncated}))
        }
        "list" => {
            let dir = resolve(&root, &req.path)?;
            let mut entries = Vec::new();
            let mut truncated = false;
            for e in fs::read_dir(dir)
                .map_err(|_| "Directory is unavailable")?
                .flatten()
            {
                if let Some(e) = entry(&root, &e.path()) {
                    entries.push(e);
                }
                if entries.len() > 1000 {
                    truncated = true;
                    break;
                }
            }
            entries.sort_by_key(|e| {
                (
                    e["kind"] != "dir",
                    e["name"].as_str().unwrap_or("").to_lowercase(),
                )
            });
            entries.truncate(1000);
            Ok(json!({"entries":entries,"truncated":truncated}))
        }
        "read" | "download" => {
            let path = resolve(&root, &req.path)?;
            let mut file = open_beneath(&root, &path)?;
            let meta = file.metadata().map_err(|_| "File is unavailable")?;
            if !meta.is_file() {
                return Err("Not a regular file".into());
            }
            file.seek(SeekFrom::Start(req.offset))
                .map_err(|_| "Cannot seek file")?;
            let mut bytes = Vec::new();
            file.take(LIMIT as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read file")?;
            let next = req.offset + bytes.len() as u64;
            if req.action == "download" {
                return Ok(json!({"bytes":bytes,"next":next,"size":meta.len()}));
            }
            let binary = bytes.contains(&0);
            let content = if binary {
                None
            } else {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            };
            Ok(
                json!({"content":content,"binary":binary,"size":meta.len(),"truncated":next<meta.len(),"modified":meta.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t|t.as_secs())}),
            )
        }
        _ => Err("Unknown file operation".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confines_reads_and_finds_files() {
        let root = std::env::temp_dir().join(format!("orb-file-test-{}", std::process::id()));
        fs::create_dir_all(root.join("audit")).unwrap();
        fs::write(root.join("audit/guarantees.yaml"), "hello").unwrap();
        fs::write(root.join(".env"), "secret").unwrap();
        assert!(execute(
            &root,
            &Request {
                action: "read".into(),
                path: "../outside".into(),
                ..Default::default()
            }
        )
        .is_err());
        assert!(execute(
            &root,
            &Request {
                action: "read".into(),
                path: ".env".into(),
                ..Default::default()
            }
        )
        .is_err());
        let r = execute(
            &root,
            &Request {
                action: "resolve".into(),
                paths: vec!["guarantees.yaml".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            r["results"][0]["matches"][0]["path"],
            "audit/guarantees.yaml"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
            assert!(execute(
                &root,
                &Request {
                    action: "read".into(),
                    path: "escape/hosts".into(),
                    ..Default::default()
                }
            )
            .is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
