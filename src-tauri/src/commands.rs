use std::{fs, path::PathBuf};

use chrono::Utc;
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    models::{JournalSummary, ScanReport, SyncMode, Workspace},
    scanner,
    storage::{
        delete_workspace, find_workspace, get_journal_summary, insert_workspace,
        list_journal_summaries, load_workspaces, record_scan,
    },
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
