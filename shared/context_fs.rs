//! Descriptor-relative file operations prevent symlink replacement races.
#[cfg(unix)]
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::Path,
};
#[cfg(unix)]
fn parent(root: &Path, path: &str, create: bool) -> Result<(File, CString), String> {
    super::valid_path(path)?;
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|e| e.to_string())?;
    let parts: Vec<_> = Path::new(path).components().collect();
    for part in &parts[..parts.len() - 1] {
        let name = CString::new(part.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        if create && unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.to_string());
            }
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok((
        directory,
        CString::new(parts.last().unwrap().as_os_str().as_bytes()).map_err(|e| e.to_string())?,
    ))
}
#[cfg(unix)]
pub fn read(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    let (parent, name) = parent(root, path, false)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("context entry is not a regular file".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(super::FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("context file changed while reading".into());
    }
    if bytes.len() > super::FILE_LIMIT {
        return Err("context file exceeds 10 MiB".into());
    }
    Ok(bytes)
}
#[cfg(unix)]
pub fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), String> {
    let (parent, name) = parent(root, path, true)?;
    let temp = CString::new(format!(".context-{}.tmp", uuid::Uuid::new_v4())).unwrap();
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    parent.sync_all().map_err(|e| e.to_string())
}
#[cfg(unix)]
pub fn mkdir(root: &Path, path: &str) -> Result<(), String> {
    let (parent, name) = parent(root, path, true)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.to_string());
        }
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    drop(unsafe { File::from_raw_fd(fd) });
    Ok(())
}
#[cfg(unix)]
pub fn remove(root: &Path, path: &str, directory: bool) -> Result<(), String> {
    let (parent, name) = parent(root, path, false)?;
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
#[cfg(not(unix))]
pub fn read(_: &std::path::Path, _: &str) -> Result<Vec<u8>, String> {
    Err("Secure context storage requires Unix".into())
}
#[cfg(not(unix))]
pub fn write(_: &std::path::Path, _: &str, _: &[u8]) -> Result<(), String> {
    Err("Secure context storage requires Unix".into())
}
#[cfg(not(unix))]
pub fn mkdir(_: &std::path::Path, _: &str) -> Result<(), String> {
    Err("Secure context storage requires Unix".into())
}
#[cfg(not(unix))]
pub fn remove(_: &std::path::Path, _: &str, _: bool) -> Result<(), String> {
    Err("Secure context storage requires Unix".into())
}
