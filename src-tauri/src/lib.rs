mod app_updater;
mod atris_auth;
mod backup;
mod backup_recovery;
mod commands;
mod continuous;
mod continuous_commands;
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

use atris_auth::AtrisHubAuthState;
use continuous::ContinuousSyncManager;
use provider_sessions::ProviderSessionStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ProviderSessionStore::default())
        .manage(ContinuousSyncManager::default())
        .manage(AtrisHubAuthState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app_updater::setup(app)?;
            backup_recovery::recover_interrupted_plans(app.handle())
                .map_err(std::io::Error::other)?;
            restore::recover_interrupted_restores(app.handle()).map_err(std::io::Error::other)?;
            sync::recover_interrupted_syncs(app.handle()).map_err(std::io::Error::other)?;
            continuous::initialize(app.handle()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_updater::get_update_runtime_info,
            app_updater::check_for_updates,
            app_updater::install_update,
            atris_auth::atrishub_auth_status,
            atris_auth::login_atrishub,
            atris_auth::restore_atrishub_session,
            atris_auth::logout_atrishub,
            commands::list_workspaces,
            commands::add_workspace,
            continuous_commands::guarded_remove_workspace,
            continuous_commands::guarded_scan_workspace,
            continuous_commands::guarded_initialize_ignore_file,
            commands::journal_summary,
            commands::journal_summaries,
            commands::rclone_runtime_status,
            commands::provider_connections,
            continuous_commands::guarded_connect_google_drive,
            continuous_commands::guarded_disconnect_provider_session,
            continuous_commands::guarded_forget_provider,
            commands::workspace_remote_binding,
            continuous_commands::guarded_bind_workspace_remote,
            continuous_commands::guarded_scan_remote_inventory,
            commands::latest_backup_plan,
            continuous_commands::guarded_prepare_backup_plan,
            continuous_commands::guarded_execute_backup_plan,
            restore::latest_restore_plan,
            continuous_commands::guarded_prepare_restore_plan,
            continuous_commands::guarded_execute_restore_plan,
            continuous_commands::guarded_set_workspace_sync_mode,
            sync::latest_sync_plan,
            continuous_commands::guarded_prepare_sync_plan,
            continuous_commands::guarded_execute_sync_plan,
            sync_recovery::list_sync_recoveries,
            continuous_commands::guarded_restore_sync_recovery,
            encryption::workspace_encryption_status,
            continuous_commands::guarded_enable_workspace_encryption,
            encryption::export_workspace_recovery_key,
            continuous_commands::guarded_import_workspace_recovery_key,
            continuous::continuous_sync_status,
            continuous::set_continuous_sync_enabled,
            continuous::update_continuous_sync_settings,
            continuous::run_continuous_sync_now,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AtrisBridge");
}
