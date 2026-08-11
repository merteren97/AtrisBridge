use std::{fs, path::PathBuf};

use chrono::Utc;
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{
    models::{
        JournalSummary, ProviderConnection, RcloneStatus, RemoteInventoryReport, ScanReport,
        SyncMode, Workspace, WorkspaceRemoteBinding,
    },
    provider_sessions::ProviderSessionStore,
    provider_storage,
    scanner,
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
    provider_storage::bind_workspace(&app, &id, &provider_id, &normalized)
}

#[tauri::command]
pub async fn scan_remote_inventory(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RemoteInventoryReport, String> {
    find_workspace(&app, &id)?;
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("This provider is not supported by the current transport adapter.".into());
    }
    let token = sessions
        .google_drive_token(&provider.id)?
        .ok_or_else(|| "Google Drive session is not active. Reconnect before scanning remote files.".to_string())?;

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
        workspace_id: id,
        provider_id: provider.id,
        remote_path: binding.remote_path,
        scanned_at: Utc::now().to_rfc3339(),
        file_count: observations.len() as u64,
        total_bytes,
    };
    provider_storage::record_remote_inventory(&app, &report, &observations)?;
    Ok(report)
}
