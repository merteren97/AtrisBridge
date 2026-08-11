use std::{fs, path::PathBuf};

use tauri::{AppHandle, Manager};

use crate::models::Workspace;

const WORKSPACE_FILE: &str = "workspaces.json";

fn storage_path(app: &AppHandle) -> Result<PathBuf, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve app data directory: {error}"))?;
    fs::create_dir_all(&app_data)
        .map_err(|error| format!("Could not create app data directory: {error}"))?;
    Ok(app_data.join(WORKSPACE_FILE))
}

pub fn load_workspaces(app: &AppHandle) -> Result<Vec<Workspace>, String> {
    let path = storage_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read workspace metadata: {error}"))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&content)
        .map_err(|error| format!("Workspace metadata is invalid: {error}"))
}

pub fn save_workspaces(app: &AppHandle, workspaces: &[Workspace]) -> Result<(), String> {
    let path = storage_path(app)?;
    let temp_path = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(workspaces)
        .map_err(|error| format!("Could not serialize workspace metadata: {error}"))?;

    fs::write(&temp_path, content)
        .map_err(|error| format!("Could not write workspace metadata: {error}"))?;

    if path.exists() {
        fs::remove_file(&path)
            .map_err(|error| format!("Could not replace workspace metadata: {error}"))?;
    }

    fs::rename(&temp_path, &path)
        .map_err(|error| format!("Could not finalize workspace metadata: {error}"))
}
