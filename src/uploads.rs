//! Bounded binary uploads with server-owned, immutable paths.
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::Write, path::Path};

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 28 * 1024 * 1024;
pub static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Deserialize, Serialize)]
pub struct Upload {
    pub name: String,
    pub data_base64: String,
}
#[derive(Deserialize, Serialize)]
pub struct Receipt {
    pub name: String,
    pub path: String,
    pub size: usize,
    pub sha256: String,
}

pub fn store(root: &Path, upload: Upload) -> Result<Receipt, String> {
    if upload.name.is_empty()
        || upload.name.len() > 240
        || upload.name == "."
        || upload.name == ".."
        || upload.name.contains(['/', '\\'])
        || upload.name.chars().any(char::is_control)
    {
        return Err("Choose a file with a plain filename (up to 240 bytes)".into());
    }
    if upload.data_base64.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err("Files must be 20 MiB or smaller".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(upload.data_base64)
        .map_err(|_| "Invalid file encoding")?;
    if bytes.len() > MAX_BYTES {
        return Err("Files must be 20 MiB or smaller".into());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("Create upload directory: {e}"))?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let directory = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(&upload.name);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Create uploaded file: {e}"))?;
        file.write_all(&bytes)
            .map_err(|e| format!("Write uploaded file: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Persist uploaded file: {e}"))?;
        Ok(Receipt {
            name: upload.name,
            path: path.to_string_lossy().into_owned(),
            size: bytes.len(),
            sha256: hex::encode(Sha256::digest(&bytes)),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&directory);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_uploads_are_exact_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let make = || Upload {
            name: "image é.png".into(),
            data_base64: "AP8BAg==".into(),
        };
        let a = store(root.path(), make()).unwrap();
        let b = store(root.path(), make()).unwrap();
        assert_ne!(a.path, b.path);
        assert_eq!(a.size, 4);
        assert_eq!(std::fs::read(a.path).unwrap(), [0, 255, 1, 2]);
        assert_eq!(a.sha256, b.sha256);
    }
    #[test]
    fn rejects_paths_and_invalid_data_without_writes() {
        let root = tempfile::tempdir().unwrap();
        for name in ["../secret", "/tmp/x", "..", "a\\b", "a\nb"] {
            assert!(store(
                root.path(),
                Upload {
                    name: name.into(),
                    data_base64: "YQ==".into()
                }
            )
            .is_err());
        }
        assert!(store(
            root.path(),
            Upload {
                name: "a".into(),
                data_base64: "!".into()
            }
        )
        .is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
