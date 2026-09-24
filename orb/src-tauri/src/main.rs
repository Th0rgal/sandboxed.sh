#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod local_origin;
#[path = "../../../shared/local_origin.rs"]
mod local_origin_wire;
#[path = "../../../shared/project_context.rs"]
mod project_context_store;
mod run_recovery;
// Shared replica code uses the same module name in both binaries.
use project_context_store as project_context;
#[path = "../../../shared/context_replica.rs"]
mod context_replica;
#[path = "project_context.rs"]
mod context_service;

#[path = "../../../shared/file_browser.rs"]
mod file_browser;
mod interactions;
mod local_agents;
mod local_stream;
mod machine_metrics;
mod session_preview;
mod transfers;
mod uploads;
mod voice;

use tauri::{Manager, Theme, WebviewWindow};

/// Follow the frontend's persisted preference. `None` delegates to the OS,
/// which makes titlebar material and the frontend change together for Auto.
#[tauri::command]
fn set_window_theme(window: WebviewWindow, theme: String) -> Result<(), String> {
    let theme = match theme.as_str() {
        "auto" => None,
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => return Err("theme must be auto, light, or dark".to_string()),
    };
    window.set_theme(theme).map_err(|e| e.to_string())
}

#[tauri::command]
fn paloma_ssh_pubkey() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
    std::fs::read_to_string(format!("{home}/.ssh/paloma.pub")).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only http(s) URLs".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    };
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

// Native storage is shared by packaged and development webview origins.
static BINDINGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[tauri::command]
fn local_bindings(
    id: Option<String>,
    binding: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let _guard = BINDINGS_LOCK.lock().map_err(|e| e.to_string())?;
    let dir =
        std::path::PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?).join(".orb");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("local-bindings.json");
    let mut all: serde_json::Map<String, serde_json::Value> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(e.to_string()),
    };
    if let (Some(id), Some(binding)) = (id, binding) {
        all.insert(id, binding);
        let tmp = dir.join("local-bindings.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&all).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
    }
    Ok(serde_json::Value::Object(all))
}

fn main() {
    if context_service::worker_entry() {
        return;
    }
    tauri::Builder::default()
        .manage(voice::VoiceState::new())
        .setup(|app| {
            // Local voice input: the Python worker starts on first use and
            // is released again after a stretch of inactivity.
            app.state::<voice::VoiceState>().start_idle_reaper();
            machine_metrics::start(app.state::<voice::VoiceState>().inner().clone());
            // macOS vibrancy: the window is transparent and the sidebar
            // shows the desktop through a sidebar-material blur, like
            // Cursor/Xcode. The main pane paints an opaque background in
            // CSS so only the sidebar is translucent.
            #[cfg(target_os = "macos")]
            {
                use tauri::Manager;
                use window_vibrancy::{
                    apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
                };
                if let Some(window) = app.get_webview_window("main") {
                    let _ = apply_vibrancy(
                        &window,
                        NSVisualEffectMaterial::Sidebar,
                        Some(NSVisualEffectState::Active),
                        Some(10.0),
                    );
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            local_bindings,
            local_origin::local_origin_launch,
            local_origin::local_origin_list,
            local_origin::local_origin_disconnect,
            run_recovery::local_run_launch,
            run_recovery::local_run_reconcile,
            interactions::local_interaction,
            interactions::local_interaction_answer,
            paloma_ssh_pubkey,
            session_preview::local_session_git,
            uploads::pick_upload_files,
            uploads::read_upload_file,
            browse_local_files,
            machine_metrics::local_machine_metrics,
            open_url,
            set_window_theme,
            voice::voice_capability,
            voice::voice_prewarm,
            voice::voice_transcribe,
            voice::voice_cancel,
            voice::voice_release,
            context_service::project_context_file,
            context_service::project_context_prepare,
            context_service::project_context_status,
            context_service::project_context_disconnect,
            local_agents::local_agents_scan,
            local_agents::local_agents_workspace,
            local_agents::local_agents_write,
            transfers::local_agents_start_authorized,
            transfers::local_machine_transfer,
            transfers::local_machine_identity,
            local_agents::local_agents_poll,
            local_agents::local_agents_subscribe,
            local_agents::local_agents_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running orb");
}

#[tauri::command]
async fn browse_local_files(
    root: String,
    request: file_browser::Request,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if request.action == "reveal" {
            let root = std::path::Path::new(&root)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let path = file_browser::resolve(&root, &request.path)?;
            #[cfg(target_os = "macos")]
            let status = std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .status();
            #[cfg(target_os = "windows")]
            let status = std::process::Command::new("explorer")
                .arg(format!("/select,{}", path.display()))
                .status();
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let status = std::process::Command::new("xdg-open")
                .arg(path.parent().unwrap_or(&root))
                .status();
            if !status.map_err(|e| e.to_string())?.success() {
                return Err("Could not reveal this file".into());
            }
            return Ok(serde_json::json!({}));
        }
        file_browser::execute(std::path::Path::new(&root), &request)
    })
    .await
    .map_err(|e| e.to_string())?
}
