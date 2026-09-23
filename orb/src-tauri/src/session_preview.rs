//! Read-only, on-demand local Git identity for the session title preview.
use serde::Serialize;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Serialize)]
pub struct GitIdentity {
    repository: String,
    branch: Option<String>,
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(1) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}
fn identity(cwd: &Path) -> Option<GitIdentity> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return None;
    }
    let root = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let repository = Path::new(&root).file_name()?.to_string_lossy().to_string();
    let branch = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).or_else(|| {
        git(cwd, &["rev-parse", "--short", "HEAD"]).map(|sha| format!("Detached HEAD · {sha}"))
    });
    Some(GitIdentity { repository, branch })
}
#[tauri::command]
pub async fn local_session_git(cwd: String) -> Result<Option<GitIdentity>, String> {
    tauri::async_runtime::spawn_blocking(move || identity(Path::new(&cwd)))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_git_identity_from_a_subdirectory_and_ignores_non_repositories() {
        let directory = std::env::temp_dir().join(format!(
            "orb-preview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        assert!(identity(&directory).is_none());
        assert!(identity(Path::new("relative-path")).is_none());
        assert!(Command::new("git")
            .arg("init")
            .arg("-q")
            .arg("--initial-branch=preview-test")
            .arg(&directory)
            .status()
            .unwrap()
            .success());
        let child = directory.join("nested");
        std::fs::create_dir(&child).unwrap();
        let info = identity(&child).unwrap();
        assert_eq!(
            info.repository,
            directory.file_name().unwrap().to_string_lossy()
        );
        assert_eq!(info.branch.as_deref(), Some("preview-test"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
