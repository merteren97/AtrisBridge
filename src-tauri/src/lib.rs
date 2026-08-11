mod commands;
mod database;
mod models;
mod provider_sessions;
mod provider_storage;
mod scanner;
mod storage;
mod transport;

use provider_sessions::ProviderSessionStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ProviderSessionStore::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_workspaces,
            commands::add_workspace,
            commands::remove_workspace,
            commands::scan_workspace,
            commands::initialize_ignore_file,
            commands::journal_summary,
            commands::journal_summaries,
            commands::rclone_runtime_status,
            commands::provider_connections,
            commands::connect_google_drive,
            commands::disconnect_provider_session,
            commands::forget_provider,
            commands::workspace_remote_binding,
            commands::bind_workspace_remote,
            commands::scan_remote_inventory,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AtrisBridge");
}
