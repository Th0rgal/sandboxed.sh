use crate::agent_software::{self as software, Inventory, UpdateJob};
use std::collections::HashMap;
#[tauri::command]
pub async fn software_inventory(
    overrides: HashMap<String, String>,
    force: bool,
) -> Result<Inventory, String> {
    let mut inventory = tauri::async_runtime::spawn_blocking(move || {
        if force {
            software::clear_versions();
        }
        software::scan("Orb runner", &overrides)
    })
    .await
    .map_err(|e| e.to_string())?;
    software::releases(&mut inventory, force).await;
    Ok(inventory)
}
#[tauri::command]
pub async fn software_update(
    component: String,
    version: String,
    path: String,
) -> Result<UpdateJob, String> {
    tauri::async_runtime::spawn_blocking(move || software::queue(&component, &version, &path))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn software_cancel(id: String) -> Result<(), String> {
    software::cancel(&id)
}
