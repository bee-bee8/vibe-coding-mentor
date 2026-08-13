#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod watcher;
mod diff;
mod analysis;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(watcher::AppState::default())
        .invoke_handler(tauri::generate_handler![
            watcher::get_watcher_state,
            watcher::get_diff_state,
            watcher::get_analysis_state,
            watcher::get_file_preview,
            watcher::complete_change,
            watcher::start_watching,
            watcher::stop_watching,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Mentor");
}
