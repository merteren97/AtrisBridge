mod ai_artifact_crypto;
mod ai_changeset;
mod ai_command;
mod ai_gateway;
mod ai_git;
mod ai_limits;
mod ai_output;
mod ai_task;
mod ai_workspace;
mod ai_worktree_cleanup;
mod app_updater;
mod atris_auth;
mod backup;
mod backup_recovery;
mod commands;
mod continuous;
mod continuous_commands;
mod database;
mod desktop_shell;
mod durable_fs;
mod encryption;
mod google_drive_identity;
mod local_mcp_clients;
mod local_mcp_commands;
mod local_mcp_ipc;
mod local_mcp_probe;
mod mcp_core;
mod mcp_dispatch;
mod models;
mod provider_sessions;
mod provider_storage;
mod remote_mcp_adapter;
mod remote_mcp_discovery;
mod remote_mcp_protocol;
mod remote_mcp_relay;
mod remote_mcp_request;
mod restore;
mod scanner;
mod secure_store;
mod services;
mod single_instance;
mod storage;
mod sync;
mod sync_recovery;
mod transport;
mod workspace_coordinator;

use ai_task::AiTaskManager;
use atris_auth::AtrisHubAuthState;
use continuous::ContinuousSyncManager;
use provider_sessions::ProviderSessionStore;
use remote_mcp_relay::RemoteMcpRelayManager;
use workspace_coordinator::WorkspaceMutationCoordinator;

fn install_tls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return;
    }

    // AtrisBridge uses rustls through both reqwest and tokio-tungstenite. Cargo
    // feature unification can make more than one built-in provider available,
    // in which case rustls intentionally refuses to guess and panics on the
    // first TLS client configuration. Select the same AWS-LC provider used by
    // reqwest before any desktop networking worker is spawned.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    if rustls::crypto::CryptoProvider::get_default().is_none() {
        panic!("AtrisBridge could not install a process-wide TLS crypto provider");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_tls_crypto_provider();

    let primary = match single_instance::acquire_or_notify()
        .expect("AtrisBridge could not establish its single-process authority")
    {
        single_instance::InstanceRole::Primary(primary) => primary,
        single_instance::InstanceRole::Secondary => return,
    };
    let single_instance::PrimaryInstance {
        guard,
        focus_requests,
    } = primary;

    let run_result = tauri::Builder::default()
        .manage(ProviderSessionStore::default())
        .manage(ContinuousSyncManager::default())
        .manage(WorkspaceMutationCoordinator::default())
        .manage(AtrisHubAuthState::default())
        .manage(AiTaskManager::default())
        .manage(RemoteMcpRelayManager::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let focus_app = app.handle().clone();
            std::thread::Builder::new()
                .name("atrisbridge-instance-focus".into())
                .spawn(move || {
                    while focus_requests.recv().is_ok() {
                        desktop_shell::show_main_window(&focus_app);
                    }
                })
                .map_err(std::io::Error::other)?;

            app_updater::setup(app)?;
            desktop_shell::setup(app)?;
            backup_recovery::recover_interrupted_plans(app.handle())
                .map_err(std::io::Error::other)?;
            restore::recover_interrupted_restores(app.handle()).map_err(std::io::Error::other)?;
            sync::recover_interrupted_syncs(app.handle()).map_err(std::io::Error::other)?;
            ai_gateway::initialize(app.handle()).map_err(std::io::Error::other)?;
            ai_git::initialize(app.handle()).map_err(std::io::Error::other)?;
            ai_worktree_cleanup::setup(app.handle());
            ai_command::initialize(app.handle()).map_err(std::io::Error::other)?;
            ai_task::initialize(app.handle()).map_err(std::io::Error::other)?;
            ai_changeset::initialize(app.handle()).map_err(std::io::Error::other)?;
            continuous::initialize(app.handle()).map_err(std::io::Error::other)?;
            // Publish both MCP transports only after every authority/recovery service is ready.
            // Transport-specific failures stay fail-closed without taking down normal desktop sync.
            if let Err(error) = local_mcp_ipc::setup(app.handle()) {
                eprintln!("AtrisBridge local MCP authority is unavailable: {error}");
            }
            if let Err(error) = remote_mcp_relay::setup(app.handle()) {
                eprintln!("AtrisBridge remote MCP relay is unavailable: {error}");
            }
            Ok(())
        })
        .on_window_event(desktop_shell::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            app_updater::get_update_runtime_info,
            app_updater::check_for_updates,
            app_updater::install_update,
            atris_auth::atrishub_auth_status,
            atris_auth::login_atrishub,
            atris_auth::restore_atrishub_session,
            atris_auth::logout_atrishub,
            ai_gateway::ai_gateway_overview,
            ai_gateway::list_ai_permissions,
            ai_gateway::set_ai_permission,
            ai_gateway::reset_ai_permission,
            ai_gateway::open_ai_session,
            ai_gateway::close_ai_session,
            ai_gateway::list_ai_sessions,
            ai_gateway::list_ai_audit,
            ai_git::provision_ai_worktree,
            ai_git::list_ai_worktrees,
            ai_git::discard_ai_worktree,
            ai_git::ai_git_status,
            ai_git::ai_git_diff,
            ai_git::ai_git_log,
            ai_git::ai_git_branches,
            ai_git::ai_git_stage,
            ai_git::ai_git_unstage,
            ai_git::ai_git_commit,
            ai_git::ai_git_create_branch,
            ai_git::ai_git_revert,
            ai_git::ai_git_push,
            ai_command::list_ai_command_profiles,
            ai_command::run_ai_command,
            ai_task::start_ai_command_task,
            ai_task::get_ai_task,
            ai_task::list_ai_tasks,
            ai_task::get_ai_task_result,
            ai_task::cancel_ai_task,
            mcp_core::ai_mcp_core_manifest,
            local_mcp_commands::list_local_mcp_clients,
            local_mcp_commands::register_local_mcp_client,
            local_mcp_commands::unregister_local_mcp_client,
            local_mcp_commands::test_local_mcp_client_connection,
            remote_mcp_relay::remote_mcp_relay_status,
            remote_mcp_relay::retry_remote_mcp_relay,
            remote_mcp_relay::list_remote_mcp_clients,
            remote_mcp_discovery::list_remote_mcp_grant_clients,
            remote_mcp_discovery::route_remote_mcp_grant_client_here,
            remote_mcp_discovery::revoke_remote_mcp_grant_client,
            desktop_shell::set_close_to_tray,
            ai_workspace::ai_file_stat,
            ai_workspace::ai_read_text_file,
            ai_workspace::ai_search_workspace,
            ai_changeset::prepare_ai_changeset,
            ai_changeset::execute_ai_changeset,
            ai_changeset::undo_ai_changeset,
            ai_changeset::get_ai_changeset,
            ai_changeset::list_ai_changesets,
            commands::list_workspaces,
            commands::add_workspace,
            continuous_commands::guarded_remove_workspace,
            continuous_commands::guarded_scan_workspace,
            continuous_commands::guarded_initialize_ignore_file,
            commands::journal_summary,
            commands::journal_summaries,
            workspace_coordinator::workspace_operation_status,
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
        .run(tauri::generate_context!());

    drop(guard);
    run_result.expect("error while running AtrisBridge");
}
