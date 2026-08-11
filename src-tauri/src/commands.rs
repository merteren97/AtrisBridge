use std::{fs, path::PathBuf};

use chrono::Utc;
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    models::{ScanReport, SyncMode, Workspace},
    scanner,
    storage::{load_workspaces, save_workspaces},
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
    let mut workspaces = load_workspaces(&app)?;

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

    workspaces.push(workspace.clone());
    save_workspaces(&app, &workspaces)?;
    Ok(workspace)
}

#[tauri::command]
pub fn remove_workspace(app: AppHandle, id: String) -> Result<(), String> {
    let mut workspaces = load_workspaces(&app)?;
    let initial_count = workspaces.len();
    workspaces.retain(|workspace| workspace.id != id);

    if workspaces.len() == initial_count {
        return Err("Workspace was not found.".into());
    }

    save_workspaces(&app, &workspaces)
}

#[tauri::command]
pub fn scan_workspace(app: AppHandle, id: String) -> Result<ScanReport, String> {
    let mut workspaces = load_workspaces(&app)?;
    let index = workspaces
        .iter()
        .position(|workspace| workspace.id == id)
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    let root = PathBuf::from(&workspaces[index].local_path);
    let report = scanner::scan(&id, &root)?;
    workspaces[index].last_scan_at = Some(report.scanned_at.clone());
    save_workspaces(&app, &workspaces)?;
    Ok(report)
}

#[tauri::command]
pub fn initialize_ignore_file(app: AppHandle, id: String) -> Result<bool, String> {
    let workspaces = load_workspaces(&app)?;
    let workspace = workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    let path = PathBuf::from(&workspace.local_path).join(".atrisbridgeignore");
    if path.exists() {
        return Ok(false);
    }

    fs::write(&path, scanner::default_ignore_file())
        .map_err(|error| format!("Could not create .atrisbridgeignore: {error}"))?;
    Ok(true)
}
