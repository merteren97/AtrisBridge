mod backup;
mod backup_recovery;
mod commands;
mod database;
mod encryption;
mod models;
mod provider_sessions;
mod provider_storage;
mod restore;
mod scanner;
mod secure_store;
mod storage;
mod sync;
mod sync_recovery;
mod transport;

use provider_sessions::ProviderSessionStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ProviderSessionStore::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            backup_recovery::recover_interrupted_plans(app.handle())
                .map_err(std::io::Error::other)?;
            restore::recover_interrupted_restores(app.handle()).map_err(std::io::Error::other)?;
            sync::recover_interrupted_syncs(app.handle()).map_err(std::io::Error::other)?;
            Ok(())
        })
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
            commands::latest_backup_plan,
            commands::prepare_backup_plan,
            commands::execute_backup_plan,
            restore::latest_restore_plan,
            restore::prepare_restore_plan,
            restore::execute_restore_plan,
            sync::set_workspace_sync_mode,
            sync::latest_sync_plan,
            sync::prepare_sync_plan,
            sync::execute_sync_plan,
            sync_recovery::list_sync_recoveries,
            sync_recovery::restore_sync_recovery,
            encryption::workspace_encryption_status,
            encryption::enable_workspace_encryption,
            encryption::export_workspace_recovery_key,
            encryption::import_workspace_recovery_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AtrisBridge");
}
