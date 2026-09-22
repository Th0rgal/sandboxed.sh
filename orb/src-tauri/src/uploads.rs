use base64::Engine;
use serde::Serialize;
use std::{
    collections::HashSet,
    io::Read,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

fn selected() -> &'static Mutex<HashSet<PathBuf>> {
    static PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(HashSet::new()))
}
#[derive(Serialize)]
pub struct Selection {
    name: String,
    path: String,
}

#[tauri::command]
pub async fn pick_upload_files() -> Result<Vec<Selection>, String> {
    let files = rfd::AsyncFileDialog::new()
        .set_title("Attach files or images")
        .pick_files()
        .await
        .unwrap_or_default();
    let mut selections = Vec::new();
    let mut allowed = selected().lock().map_err(|e| e.to_string())?;
    for file in files {
        let path = file.path().canonicalize().map_err(|e| e.to_string())?;
        if !path.is_file() {
            return Err("Only regular files can be attached".into());
        }
        selections.push(Selection {
            name: file.file_name(),
            path: path.to_string_lossy().into_owned(),
        });
        allowed.insert(path);
    }
    Ok(selections)
}

#[tauri::command]
pub async fn read_upload_file(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        if canonical != path
            || !selected()
                .lock()
                .map_err(|e| e.to_string())?
                .contains(&canonical)
        {
            return Err("Choose this file again using the file picker".into());
        }
        let file = std::fs::File::open(&canonical).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Only regular files can be attached".into());
        }
        let mut bytes = Vec::new();
        file.take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 20 * 1024 * 1024 {
            return Err("Files must be 20 MiB or smaller".into());
        }
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
    .await
    .map_err(|e| e.to_string())?
}
