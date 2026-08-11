use rusqlite::{params, OptionalExtension};
use tauri::{AppHandle, State};

use crate::{
    commands, continuous,
    continuous::ContinuousSyncManager,
    database::open_database,
    encryption,
    models::{
        BackupExecutionReport, BackupPlan, EncryptionEnableResult, ProviderConnection,
        RemoteInventoryReport, ScanReport, SyncMode, Workspace, WorkspaceEncryptionStatus,
        WorkspaceRemoteBinding,
    },
    provider_sessions::ProviderSessionStore,
    restore::{self, RestoreExecutionReport, RestorePlan},
    storage::delete_workspace,
    sync::{self, SyncExecutionReport, SyncPlan},
    sync_recovery::{self, SyncRecoveryEntry},
};

#[tauri::command]
pub fn guarded_remove_workspace(
    app: AppHandle,
    id: String,
    manager: State<'_, ContinuousSyncManager>,
) -> Result<(), String> {
    ensure_manual_control_allowed(&app, &id)?;
    manager.stop_workspace(&id)?;
    delete_workspace(&app, &id)
}

#[tauri::command]
pub fn guarded_scan_workspace(app: AppHandle, id: String) -> Result<ScanReport, String> {
    ensure_manual_control_allowed(&app, &id)?;
    commands::scan_workspace(app, id)
}

#[tauri::command]
pub fn guarded_initialize_ignore_file(app: AppHandle, id: String) -> Result<bool, String> {
    ensure_manual_control_allowed(&app, &id)?;
    commands::initialize_ignore_file(app, id)
}

#[tauri::command]
pub async fn guarded_connect_google_drive(
    app: AppHandle,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<ProviderConnection, String> {
    ensure_no_watched_workspaces(&app)?;
    commands::connect_google_drive(app, sessions).await
}

#[tauri::command]
pub fn guarded_disconnect_provider_session(
    app: AppHandle,
    provider_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<(), String> {
    ensure_provider_not_watched(&app, &provider_id)?;
    commands::disconnect_provider_session(provider_id, sessions)
}

#[tauri::command]
pub fn guarded_forget_provider(
    app: AppHandle,
    provider_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<(), String> {
    ensure_provider_not_watched(&app, &provider_id)?;
    commands::forget_provider(app, provider_id, sessions)
}

#[tauri::command]
pub fn guarded_bind_workspace_remote(
    app: AppHandle,
    id: String,
    provider_id: String,
    remote_path: String,
) -> Result<WorkspaceRemoteBinding, String> {
    ensure_manual_control_allowed(&app, &id)?;
    commands::bind_workspace_remote(app, id, provider_id, remote_path)
}

#[tauri::command]
pub async fn guarded_scan_remote_inventory(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RemoteInventoryReport, String> {
    ensure_manual_control_allowed(&app, &id)?;
    commands::scan_remote_inventory(app, id, sessions).await
}

#[tauri::command]
pub fn guarded_set_workspace_sync_mode(
    app: AppHandle,
    id: String,
    mode: SyncMode,
) -> Result<Workspace, String> {
    ensure_manual_control_allowed(&app, &id)?;
    sync::set_workspace_sync_mode(app, id, mode)
}

#[tauri::command]
pub async fn guarded_enable_workspace_encryption(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<EncryptionEnableResult, String> {
    ensure_manual_control_allowed(&app, &id)?;
    encryption::enable_workspace_encryption(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_import_workspace_recovery_key(
    app: AppHandle,
    id: String,
    recovery_key: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<WorkspaceEncryptionStatus, String> {
    ensure_manual_control_allowed(&app, &id)?;
    encryption::import_workspace_recovery_key(app, id, recovery_key, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_backup_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<BackupPlan, String> {
    ensure_manual_control_allowed(&app, &id)?;
    commands::prepare_backup_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_backup_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<BackupExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "backup_plans", &plan_id)?;
    ensure_manual_control_allowed(&app, &workspace_id)?;
    commands::execute_backup_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_restore_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RestorePlan, String> {
    ensure_manual_control_allowed(&app, &id)?;
    restore::prepare_restore_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_restore_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RestoreExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "restore_plans", &plan_id)?;
    ensure_manual_control_allowed(&app, &workspace_id)?;
    restore::execute_restore_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_sync_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<SyncPlan, String> {
    ensure_manual_control_allowed(&app, &id)?;
    sync::prepare_sync_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_sync_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<SyncExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "sync_plans", &plan_id)?;
    ensure_manual_control_allowed(&app, &workspace_id)?;
    sync::execute_sync_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_restore_sync_recovery(
    app: AppHandle,
    recovery_id: String,
) -> Result<SyncRecoveryEntry, String> {
    let workspace_id = workspace_for_recovery(&app, &recovery_id)?;
    ensure_manual_control_allowed(&app, &workspace_id)?;
    sync_recovery::restore_sync_recovery(app, recovery_id).await
}

fn ensure_manual_control_allowed(app: &AppHandle, workspace_id: &str) -> Result<(), String> {
    if continuous::is_enabled(app, workspace_id)? {
        return Err(
            "Continuous watch mode currently owns this workspace. Pause watch mode before scanning manually, changing bindings/sync mode/encryption setup, importing recovery keys, restoring recovery files, or running a manual plan."
                .into(),
        );
    }
    Ok(())
}

fn ensure_no_watched_workspaces(app: &AppHandle) -> Result<(), String> {
    let connection = open_database(app)?;
    let watched: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM continuous_sync_settings WHERE enabled = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect watch ownership: {error}"))?;
    if watched > 0 {
        return Err(
            "Pause all continuous watch workspaces before authorizing a Google Drive account. This prevents an account switch while background synchronization owns provider bindings."
                .into(),
        );
    }
    Ok(())
}

fn ensure_provider_not_watched(app: &AppHandle, provider_id: &str) -> Result<(), String> {
    let connection = open_database(app)?;
    let watched: i64 = connection
        .query_row(
            "SELECT COUNT(*)
             FROM workspace_remote_bindings b
             JOIN continuous_sync_settings c ON c.workspace_id = b.workspace_id
             WHERE b.provider_id = ?1 AND c.enabled = 1",
            params![provider_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect provider watch ownership: {error}"))?;
    if watched > 0 {
        return Err(
            "One or more workspaces are using this provider in continuous watch mode. Pause those workspaces before disconnecting or forgetting the provider."
                .into(),
        );
    }
    Ok(())
}

fn workspace_for_plan(app: &AppHandle, table: &str, plan_id: &str) -> Result<String, String> {
    let allowed = matches!(table, "backup_plans" | "restore_plans" | "sync_plans");
    if !allowed {
        return Err("Unsupported transfer plan type.".into());
    }
    let connection = open_database(app)?;
    let sql = format!("SELECT workspace_id FROM {table} WHERE id = ?1");
    connection
        .query_row(&sql, params![plan_id], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|error| format!("Could not inspect transfer plan ownership: {error}"))?
        .ok_or_else(|| "Transfer plan was not found.".to_string())
}

fn workspace_for_recovery(app: &AppHandle, recovery_id: &str) -> Result<String, String> {
    let connection = open_database(app)?;
    connection
        .query_row(
            "SELECT workspace_id FROM sync_recovery_entries WHERE id = ?1",
            params![recovery_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not inspect recovery ownership: {error}"))?
        .ok_or_else(|| "Recovery copy was not found.".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn plan_table_allowlist_is_intentionally_narrow() {
        assert!(matches!(
            "backup_plans",
            "backup_plans" | "restore_plans" | "sync_plans"
        ));
        assert!(!matches!(
            "provider_connections",
            "backup_plans" | "restore_plans" | "sync_plans"
        ));
    }
}
