#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod diff;
mod learning_memory;
mod mentor;
mod teaching;
mod watcher;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(watcher::AppState::default())
        .manage(mentor::MentorAppState::default())
        .manage(teaching::TeachingAppState::default())
        .manage(learning_memory::LearningMemoryAppState::default())
        .invoke_handler(tauri::generate_handler![
            watcher::get_watcher_state,
            watcher::get_diff_state,
            watcher::get_analysis_state,
            watcher::get_file_preview,
            watcher::complete_change,
            watcher::start_watching,
            watcher::stop_watching,
            mentor::get_mentor_state,
            mentor::ask_mentor,
            mentor::cancel_mentor,
            mentor::reset_mentor,
            teaching::get_teaching_state,
            teaching::teach_change,
            teaching::reset_teaching,
            learning_memory::get_learning_memory_state,
            learning_memory::get_relevant_learning_memory,
            learning_memory::update_learning_memory_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Mentor");
}
