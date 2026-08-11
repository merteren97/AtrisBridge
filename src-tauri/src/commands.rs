use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::Utc;
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{
    backup, encryption,
    models::{
        BackupExecutionReport, BackupPlan, JournalSummary, ProviderConnection, RcloneStatus,
        RemoteFileObservation, RemoteInventoryReport, ScanReport, SyncMode, Workspace,
        WorkspaceRemoteBinding,
    },
    provider_sessions::ProviderSessionStore,
    provider_storage, scanner,
    storage::{
        delete_workspace, find_workspace, get_journal_summary, insert_workspace,
        list_journal_summaries, load_workspaces, record_scan,
    },
    transport::rclone,
};

#[tauri::command]
pub fn list_workspaces(app: AppHandle) -> Result<Vec<Workspace>, String> {
    load_workspaces(&app)
}

#[tauri::command]
pub fn add_workspace(app: AppHandle, name: String, path: String) -> Result<Workspace, String> {
    let requested = PathBuf::from(path.trim());
    if !requested.is_dir() {
        return Err("Choose an existing directory.".into());
    }

    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("Could not access the selected directory: {error}"))?;
    let workspaces = load_workspaces(&app)?;

    for existing in &workspaces {
        if PathBuf::from(&existing.local_path)
            .canonicalize()
            .map(|value| value == canonical)
            .unwrap_or(false)
        {
            return Err("This directory is already an AtrisBridge workspace.".into());
        }
    }

    let trimmed_name = name.trim();
    let fallback_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Workspace");

    let workspace = Workspace {
        id: Uuid::new_v4().to_string(),
        name: if trimmed_name.is_empty() {
            fallback_name
        } else {
            trimmed_name
        }
        .to_owned(),
        local_path: requested.to_string_lossy().to_string(),
        sync_mode: SyncMode::Backup,
        created_at: Utc::now().to_rfc3339(),
        last_scan_at: None,
    };

    insert_workspace(&app, &workspace)?;
    Ok(workspace)
}

#[tauri::command]
pub fn remove_workspace(app: AppHandle, id: String) -> Result<(), String> {
    delete_workspace(&app, &id)
}

#[tauri::command]
pub fn scan_workspace(app: AppHandle, id: String) -> Result<ScanReport, String> {
    let workspace = find_workspace(&app, &id)?;
    let root = PathBuf::from(&workspace.local_path);
    let outcome = scanner::scan(&id, &root)?;
    record_scan(&app, &outcome.report, &outcome.inventory)?;
    Ok(outcome.report)
}

#[tauri::command]
pub fn initialize_ignore_file(app: AppHandle, id: String) -> Result<bool, String> {
    let workspace = find_workspace(&app, &id)?;
    let path = PathBuf::from(&workspace.local_path).join(".atrisbridgeignore");
    if path.exists() {
        return Ok(false);
    }

    fs::write(&path, scanner::default_ignore_file())
        .map_err(|error| format!("Could not create .atrisbridgeignore: {error}"))?;
    Ok(true)
}

#[tauri::command]
pub fn journal_summary(app: AppHandle, id: String) -> Result<JournalSummary, String> {
    get_journal_summary(&app, &id)
}

#[tauri::command]
pub fn journal_summaries(app: AppHandle) -> Result<Vec<JournalSummary>, String> {
    list_journal_summaries(&app)
}

#[tauri::command]
pub fn rclone_runtime_status(app: AppHandle) -> RcloneStatus {
    rclone::status(&app)
}

#[tauri::command]
pub fn provider_connections(
    app: AppHandle,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<Vec<ProviderConnection>, String> {
    let mut providers = provider_storage::list_provider_connections(&app)?;
    for provider in &mut providers {
        provider.session_active = sessions.is_active(&provider.id)?;
        provider.credential_persisted = sessions.is_persisted(&provider.id)?;
    }
    Ok(providers)
}

#[tauri::command]
pub async fn connect_google_drive(
    app: AppHandle,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<ProviderConnection, String> {
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
        rclone::google_drive_userinfo(&verify_app, &verify_token)
    })
    .await
    .map_err(|error| format!("Google verification worker failed: {error}"))??;

    let mut provider = provider_storage::upsert_google_drive_connection(&app, account_label)?;
    sessions.set_google_drive_token(&provider.id, token)?;
    provider.session_active = true;
    provider.credential_persisted = true;
    Ok(provider)
}

#[tauri::command]
pub fn disconnect_provider_session(
    provider_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<(), String> {
    sessions.remove(&provider_id)
}

#[tauri::command]
pub fn forget_provider(
    app: AppHandle,
    provider_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<(), String> {
    sessions.remove(&provider_id)?;
    provider_storage::remove_provider_connection(&app, &provider_id)
}

#[tauri::command]
pub fn workspace_remote_binding(
    app: AppHandle,
    id: String,
) -> Result<Option<WorkspaceRemoteBinding>, String> {
    provider_storage::get_workspace_binding(&app, &id)
}

#[tauri::command]
pub fn bind_workspace_remote(
    app: AppHandle,
    id: String,
    provider_id: String,
    remote_path: String,
) -> Result<WorkspaceRemoteBinding, String> {
    find_workspace(&app, &id)?;
    let normalized = rclone::normalize_remote_path(&remote_path)?;
    if normalized.is_empty() {
        return Err("Choose a dedicated Google Drive folder for this workspace.".into());
    }
    encryption::ensure_binding_change_allowed(&app, &id, &normalized)?;
    provider_storage::bind_workspace(&app, &id, &provider_id, &normalized)
}

#[tauri::command]
pub async fn scan_remote_inventory(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RemoteInventoryReport, String> {
    let (provider, _) = provider_storage::get_provider_for_workspace(&app, &id)?;
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive session is not active. Reconnect before scanning remote files.".to_string()
    })?;
    refresh_remote_inventory(&app, &id, token).await
}

#[tauri::command]
pub fn latest_backup_plan(app: AppHandle, id: String) -> Result<Option<BackupPlan>, String> {
    backup::latest_plan(&app, &id)
}

#[tauri::command]
pub async fn prepare_backup_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<BackupPlan, String> {
    let workspace = find_workspace(&app, &id)?;
    if !matches!(workspace.sync_mode, SyncMode::Backup) {
        return Err("Phase 4 only prepares upload plans for backup-mode workspaces.".into());
    }
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("Phase 4 currently supports Google Drive backup only.".into());
    }
    ensure_managed_backup_root(&binding.remote_path)?;
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive session is not active. Reconnect before preparing a backup.".to_string()
    })?;

    refresh_local_inventory(&app, &id).await?;
    refresh_remote_inventory(&app, &id, token).await?;
    backup::create_plan(&app, &id)
}

#[tauri::command]
pub async fn execute_backup_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<BackupExecutionReport, String> {
    let context = backup::execution_context(&app, &plan_id)?;
    let workspace = find_workspace(&app, &context.workspace_id)?;
    if !matches!(workspace.sync_mode, SyncMode::Backup) {
        return Err(
            "Backup plan execution is disabled while this workspace is not in Backup mode. Prepare a fresh plan after changing mode."
      .into(),
        );
    }
    ensure_managed_backup_root(&context.remote_path)?;
    let token = sessions
        .google_drive_token(&context.provider_id)?
        .ok_or_else(|| {
            "Google Drive session is not active. Reconnect before backup.".to_string()
        })?;

    refresh_local_inventory(&app, &context.workspace_id).await?;
    refresh_remote_inventory(&app, &context.workspace_id, token.clone()).await?;

    let context = backup::execution_context(&app, &plan_id)?;
    ensure_managed_backup_root(&context.remote_path)?;
    let operations = backup::begin_execution(&app, &plan_id)?;
    for operation in operations {
        if let Err(error) = backup::mark_operation_running(&app, &operation.id) {
            let _ = backup::fail_operation(&app, &operation.id, &error);
            continue;
        }

        if let Err(error) = execute_backup_operation(&app, &context, &operation, &token).await {
            let _ = backup::fail_operation(&app, &operation.id, &error);
        }
    }

    backup::finalize_plan(&app, &plan_id)
}

async fn refresh_local_inventory(app: &AppHandle, id: &str) -> Result<ScanReport, String> {
    let workspace = find_workspace(app, id)?;
    let root = PathBuf::from(workspace.local_path);
    let workspace_id = id.to_string();
    let outcome = tauri::async_runtime::spawn_blocking(move || scanner::scan(&workspace_id, &root))
        .await
        .map_err(|error| format!("Local scan worker failed: {error}"))??;
    record_scan(app, &outcome.report, &outcome.inventory)?;
    Ok(outcome.report)
}

async fn refresh_remote_inventory(
    app: &AppHandle,
    id: &str,
    token: String,
) -> Result<RemoteInventoryReport, String> {
    find_workspace(app, id)?;
    let (provider, binding) = provider_storage::get_provider_for_workspace(app, id)?;
    if provider.provider_type != "google_drive" {
        return Err("This provider is not supported by the current transport adapter.".into());
    }

    let inventory_app = app.clone();
    let inventory_path = binding.remote_path.clone();
    let observations = tauri::async_runtime::spawn_blocking(move || {
        rclone::list_google_drive_files(&inventory_app, &token, &inventory_path)
    })
    .await
    .map_err(|error| format!("Remote inventory worker failed: {error}"))??;

    let total_bytes = observations.iter().try_fold(0_u64, |sum, item| {
        sum.checked_add(item.size)
            .ok_or_else(|| "Remote inventory size exceeded supported range.".to_string())
    })?;
    let report = RemoteInventoryReport {
        workspace_id: id.to_string(),
        provider_id: provider.id,
        remote_path: binding.remote_path,
        scanned_at: Utc::now().to_rfc3339(),
        file_count: observations.len() as u64,
        total_bytes,
    };
    provider_storage::record_remote_inventory(app, &report, &observations)?;
    Ok(report)
}

async fn execute_backup_operation(
    app: &AppHandle,
    context: &backup::BackupExecutionContext,
    operation: &backup::BackupOperation,
    token: &str,
) -> Result<(), String> {
    let current =
        backup::current_file_evidence(app, &operation.workspace_id, &operation.relative_path)?;
    if !current.local_present
        || current.local_hash.as_deref() != Some(operation.local_hash.as_str())
        || current.local_size != Some(operation.local_size)
    {
        return Err(
            "Local file changed after the backup plan was prepared. Prepare a fresh plan.".into(),
        );
    }

    match operation.action.as_str() {
        "create" => {
            if operation.expected_remote_present || current.remote_present {
                return Err(
                    "Remote path appeared after planning. AtrisBridge will not overwrite it."
                        .into(),
                );
            }
        }
        "update" => {
            if !operation.expected_remote_present || !current.remote_present {
                return Err(
                    "Remote file disappeared after planning. AtrisBridge will not recreate it automatically."
                        .into(),
                );
            }
            ensure_current_remote_matches_plan(operation, &current)?;
        }
        _ => return Err("Unsupported Phase 4 backup action.".into()),
    }

    let workspace = find_workspace(app, &operation.workspace_id)?;
    let local_path = resolve_upload_path(&workspace, &operation.relative_path)?;
    let (actual_size, actual_hash) = scanner::fingerprint_file(&local_path)?;
    if actual_size != operation.local_size || actual_hash != operation.local_hash {
        return Err(
            "Local file changed during upload preflight. Nothing was sent; prepare a fresh plan."
                .into(),
        );
    }

    let remote_file_path =
        rclone::join_remote_path(&context.remote_path, &operation.relative_path)?;
    if operation.action == "update" {
        let stat_app = app.clone();
        let stat_token = token.to_string();
        let stat_path = remote_file_path.clone();
        let stat_relative = operation.relative_path.clone();
        let observation = tauri::async_runtime::spawn_blocking(move || {
            rclone::stat_google_drive_file(&stat_app, &stat_token, &stat_path, &stat_relative)
        })
        .await
        .map_err(|error| format!("Remote preflight worker failed: {error}"))??;
        ensure_remote_observation_matches_plan(operation, &observation)?;
    }

    let upload_app = app.clone();
    let upload_token = token.to_string();
    let upload_local = local_path;
    let upload_remote = remote_file_path;
    let create_only = operation.action == "create";
    let observation = tauri::async_runtime::spawn_blocking(move || {
        rclone::upload_google_drive_file(
            &upload_app,
            &upload_token,
            &upload_local,
            &upload_remote,
            create_only,
        )
    })
    .await
    .map_err(|error| format!("Upload worker failed: {error}"))??;

    if observation.size != operation.local_size
        || observation.remote_id.is_none()
        || observation.checksum_type.is_none()
        || observation.checksum.is_none()
    {
        return Err(
            "Google Drive upload completed without enough evidence to establish a safe baseline."
                .into(),
        );
    }

    backup::complete_operation(app, operation, &observation)
}

fn ensure_current_remote_matches_plan(
    operation: &backup::BackupOperation,
    current: &backup::CurrentFileEvidence,
) -> Result<(), String> {
    if current.remote_id != operation.expected_remote_id
        || current.remote_checksum_type != operation.expected_remote_checksum_type
        || current.remote_checksum != operation.expected_remote_checksum
    {
        return Err(
            "Remote evidence changed after planning. AtrisBridge blocked the overwrite.".into(),
        );
    }
    Ok(())
}

fn ensure_remote_observation_matches_plan(
    operation: &backup::BackupOperation,
    observation: &RemoteFileObservation,
) -> Result<(), String> {
    if observation.remote_id != operation.expected_remote_id
        || observation.checksum_type != operation.expected_remote_checksum_type
        || observation.checksum != operation.expected_remote_checksum
    {
        return Err(
            "Remote file changed during upload preflight. AtrisBridge blocked the overwrite."
                .into(),
        );
    }
    Ok(())
}

fn ensure_managed_backup_root(remote_path: &str) -> Result<(), String> {
    let normalized = rclone::normalize_remote_path(remote_path)?;
    if normalized == "AtrisBridge" || normalized.starts_with("AtrisBridge/") {
        return Ok(());
    }
    Err(
        "Phase 4 writes are restricted to an AtrisBridge-managed Google Drive path. Rebind this workspace under AtrisBridge/... before preparing a backup."
            .into(),
    )
}

fn resolve_upload_path(workspace: &Workspace, relative_path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Backup item contains an unsafe local path.".into());
    }

    let root = PathBuf::from(&workspace.local_path)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    let candidate = root.join(relative);
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("Could not resolve upload candidate: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("Backup item escaped the selected workspace root.".into());
    }
    if !canonical.is_file() {
        return Err("Backup item is no longer a regular file.".into());
    }
    Ok(canonical)
}
