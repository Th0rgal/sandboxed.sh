#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn paloma_ssh_pubkey() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
    std::fs::read_to_string(format!("{home}/.ssh/paloma.pub")).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![paloma_ssh_pubkey])
        .run(tauri::generate_context!())
        .expect("error while running orb");
}
