#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[path = "../../../shared/file_browser.rs"]
mod file_browser;
mod local_agents;
mod machine_metrics;
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

fn main() {
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
            paloma_ssh_pubkey,
            browse_local_files,
            machine_metrics::local_machine_metrics,
            open_url,
            set_window_theme,
            voice::voice_capability,
            voice::voice_prewarm,
            voice::voice_transcribe,
            voice::voice_cancel,
            voice::voice_release,
            local_agents::local_agents_scan,
            local_agents::local_agents_workspace,
            local_agents::local_agents_write,
            local_agents::local_agents_start,
            local_agents::local_agents_poll,
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
        file_browser::execute(std::path::Path::new(&root), &request)
    })
    .await
    .map_err(|e| e.to_string())?
}
