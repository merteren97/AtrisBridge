use std::{fs, path::PathBuf};

use chrono::Utc;
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    models::{ScanReport, SyncMode, Workspace},
    scanner,
    storage::{delete_workspace, find_workspace, insert_workspace, load_workspaces, record_scan},
};

pub fn list(app: &AppHandle) -> Result<Vec<Workspace>, String> {
    load_workspaces(app)
}

pub fn add(app: &AppHandle, name: String, path: String) -> Result<Workspace, String> {
    let requested = PathBuf::from(path.trim());
    if !requested.is_dir() {
        return Err("Choose an existing directory.".into());
    }

    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("Could not access the selected directory: {error}"))?;
    let workspaces = load_workspaces(app)?;

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

    insert_workspace(app, &workspace)?;
    Ok(workspace)
}

pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    delete_workspace(app, id)
}

pub fn scan(app: &AppHandle, id: &str) -> Result<ScanReport, String> {
    let workspace = find_workspace(app, id)?;
    let root = PathBuf::from(&workspace.local_path);
    let outcome = scanner::scan(id, &root)?;
    record_scan(app, &outcome.report, &outcome.inventory)?;
    Ok(outcome.report)
}

pub fn initialize_ignore_file(app: &AppHandle, id: &str) -> Result<bool, String> {
    let workspace = find_workspace(app, id)?;
    let path = PathBuf::from(&workspace.local_path).join(".atrisbridgeignore");
    if path.exists() {
        return Ok(false);
    }

    fs::write(&path, scanner::default_ignore_file())
        .map_err(|error| format!("Could not create .atrisbridgeignore: {error}"))?;
    Ok(true)
}
