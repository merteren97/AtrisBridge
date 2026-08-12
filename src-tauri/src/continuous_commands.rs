use rusqlite::{params, OptionalExtension};
use tauri::{AppHandle, State};

use crate::{
    commands,
    continuous::ContinuousSyncManager,
    database::open_database,
    encryption, google_drive_identity,
    models::{
        BackupExecutionReport, BackupPlan, EncryptionEnableResult, ProviderConnection,
        RemoteInventoryReport, ScanReport, SyncMode, Workspace, WorkspaceEncryptionStatus,
        WorkspaceRemoteBinding,
    },
    provider_sessions::ProviderSessionStore,
    provider_storage,
    restore::{self, RestoreExecutionReport, RestorePlan},
    services::workspace as workspace_service,
    sync::{self, SyncExecutionReport, SyncPlan},
    sync_recovery::{self, SyncRecoveryEntry},
    transport::rclone,
    workspace_coordinator::{
        WorkspaceMutationCoordinator, WorkspaceMutationLease, WorkspaceOperationKind,
    },
};

#[tauri::command]
pub fn guarded_remove_workspace(
    app: AppHandle,
    id: String,
    manager: State<'_, ContinuousSyncManager>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<(), String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    manager.stop_workspace(&id)?;
    workspace_service::remove(&app, &id)
}

#[tauri::command]
pub fn guarded_scan_workspace(
    app: AppHandle,
    id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<ScanReport, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Observe)?;
    commands::scan_workspace(app, id)
}

#[tauri::command]
pub fn guarded_initialize_ignore_file(
    app: AppHandle,
    id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<bool, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    commands::initialize_ignore_file(app, id)
}

#[tauri::command]
pub async fn guarded_connect_google_drive(
    app: AppHandle,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<ProviderConnection, String> {
    ensure_no_watched_workspaces(&app)?;

    let authorize_app = app.clone();
    let token = tauri::async_runtime::spawn_blocking(move || {
        rclone::authorize_google_drive(&authorize_app)
    })
    .await
    .map_err(|error| format!("Google authorization worker failed: {error}"))??;

    let verify_app = app.clone();
    let verify_token = token.clone();
    let account_label = tauri::async_runtime::spawn_blocking(move || {
        rclone::verify_google_drive(&verify_app, &verify_token)?;
        google_drive_identity::account_label_from_token(&verify_token)
    })
    .await
    .map_err(|error| format!("Google verification worker failed: {error}"))??;

    let mut provider = provider_storage::upsert_google_drive_connection(&app, Some(account_label))?;
    sessions.set_google_drive_token(&provider.id, token)?;
    provider.session_active = true;
    provider.credential_persisted = true;
    Ok(provider)
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
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<WorkspaceRemoteBinding, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    commands::bind_workspace_remote(app, id, provider_id, remote_path)
}

#[tauri::command]
pub async fn guarded_scan_remote_inventory(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<RemoteInventoryReport, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Observe)?;
    commands::scan_remote_inventory(app, id, sessions).await
}

#[tauri::command]
pub fn guarded_set_workspace_sync_mode(
    app: AppHandle,
    id: String,
    mode: SyncMode,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<Workspace, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    sync::set_workspace_sync_mode(app, id, mode)
}

#[tauri::command]
pub async fn guarded_enable_workspace_encryption(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<EncryptionEnableResult, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    encryption::enable_workspace_encryption(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_import_workspace_recovery_key(
    app: AppHandle,
    id: String,
    recovery_key: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<WorkspaceEncryptionStatus, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Configure)?;
    encryption::import_workspace_recovery_key(app, id, recovery_key, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_backup_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<BackupPlan, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Plan)?;
    commands::prepare_backup_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_backup_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<BackupExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "backup_plans", &plan_id)?;
    let _lease = acquire_manual(
        &app,
        &workspace_id,
        &coordinator,
        WorkspaceOperationKind::Execute,
    )?;
    commands::execute_backup_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_restore_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<RestorePlan, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Plan)?;
    restore::prepare_restore_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_restore_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<RestoreExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "restore_plans", &plan_id)?;
    let _lease = acquire_manual(
        &app,
        &workspace_id,
        &coordinator,
        WorkspaceOperationKind::Execute,
    )?;
    restore::execute_restore_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_prepare_sync_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<SyncPlan, String> {
    let _lease = acquire_manual(&app, &id, &coordinator, WorkspaceOperationKind::Plan)?;
    sync::prepare_sync_plan(app, id, sessions).await
}

#[tauri::command]
pub async fn guarded_execute_sync_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<SyncExecutionReport, String> {
    let workspace_id = workspace_for_plan(&app, "sync_plans", &plan_id)?;
    let _lease = acquire_manual(
        &app,
        &workspace_id,
        &coordinator,
        WorkspaceOperationKind::Execute,
    )?;
    sync::execute_sync_plan(app, plan_id, sessions).await
}

#[tauri::command]
pub async fn guarded_restore_sync_recovery(
    app: AppHandle,
    recovery_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<SyncRecoveryEntry, String> {
    let workspace_id = workspace_for_recovery(&app, &recovery_id)?;
    let _lease = acquire_manual(
        &app,
        &workspace_id,
        &coordinator,
        WorkspaceOperationKind::Recovery,
    )?;
    sync_recovery::restore_sync_recovery(app, recovery_id).await
}

fn acquire_manual(
    _app: &AppHandle,
    workspace_id: &str,
    coordinator: &WorkspaceMutationCoordinator,
    kind: WorkspaceOperationKind,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(workspace_id, "desktop-manual", kind)
        .map_err(|error| error.to_string())
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
