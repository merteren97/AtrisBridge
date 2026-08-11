use std::{sync::Mutex, time::Duration};

use serde::Serialize;
use tauri::{ipc::Channel, AppHandle, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

const UPDATE_TIMEOUT_SECONDS: u64 = 20;

pub struct PendingUpdate(Mutex<Option<Update>>);

impl Default for PendingUpdate {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRuntimeInfo {
    configured: bool,
    current_version: String,
    channel: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    version: String,
    current_version: String,
    notes: Option<String>,
    pub_date: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEvent {
    event: &'static str,
    content_length: Option<u64>,
    chunk_length: usize,
}

fn updater_public_key() -> Option<&'static str> {
    option_env!("TAURI_UPDATER_PUBLIC_KEY")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn updater_endpoint() -> Option<&'static str> {
    option_env!("ATRISBRIDGE_UPDATE_ENDPOINT")
        .map(str::trim)
        .filter(|value| value.starts_with("https://"))
}

fn updater_channel() -> &'static str {
    option_env!("ATRISBRIDGE_UPDATE_CHANNEL")
        .map(str::trim)
        .filter(|value| matches!(*value, "preview" | "stable"))
        .unwrap_or("development")
}

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let public_key = updater_public_key().ok_or_else(|| {
        "The updater is not configured in this build. Install an official signed AtrisBridge release to enable updates."
            .to_string()
    })?;
    let endpoint = updater_endpoint().ok_or_else(|| {
        "The updater endpoint is not configured in this build. Install an official signed AtrisBridge release."
            .to_string()
    })?;

    app.updater_builder()
        .pubkey(public_key)
        .endpoints(vec![endpoint
            .parse()
            .map_err(|error| format!("Invalid updater endpoint: {error}"))?])
        .map_err(|error| format!("Could not configure updater endpoint: {error}"))?
        .timeout(Duration::from_secs(UPDATE_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("Could not initialize updater: {error}"))
}

pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    app.handle()
        .plugin(tauri_plugin_updater::Builder::new().build())?;
    app.manage(PendingUpdate::default());
    Ok(())
}

#[tauri::command]
pub fn get_update_runtime_info(app: AppHandle) -> UpdateRuntimeInfo {
    UpdateRuntimeInfo {
        configured: updater_public_key().is_some() && updater_endpoint().is_some(),
        current_version: app.package_info().version.to_string(),
        channel: updater_channel().to_string(),
    }
}

#[tauri::command]
pub async fn check_for_updates(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
) -> Result<Option<UpdateMetadata>, String> {
    let update = updater(&app)?
        .check()
        .await
        .map_err(|error| format!("Could not check AtrisBridge releases for updates: {error}"))?;

    let metadata = update.as_ref().map(|update| UpdateMetadata {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
        pub_date: update.date.map(|date| date.to_string()),
    });

    let mut slot = pending_update
        .0
        .lock()
        .map_err(|_| "The pending update state is unavailable.".to_string())?;
    *slot = update;
    Ok(metadata)
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let update = pending_update
        .0
        .lock()
        .map_err(|_| "The pending update state is unavailable.".to_string())?
        .take()
        .ok_or_else(|| {
            "There is no checked update ready to install. Check for updates again.".to_string()
        })?;

    let mut started = false;
    update
        .download_and_install(
            |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = on_event.send(DownloadEvent {
                        event: "started",
                        content_length,
                        chunk_length: 0,
                    });
                }
                let _ = on_event.send(DownloadEvent {
                    event: "progress",
                    content_length: None,
                    chunk_length,
                });
            },
            || {
                let _ = on_event.send(DownloadEvent {
                    event: "finished",
                    content_length: None,
                    chunk_length: 0,
                });
            },
        )
        .await
        .map_err(|error| {
            format!("Could not download or install the AtrisBridge update: {error}")
        })?;

    app.restart();
}

#[cfg(test)]
mod tests {
    use super::{updater_channel, updater_endpoint, updater_public_key};

    #[test]
    fn unsigned_development_builds_fail_closed() {
        if option_env!("TAURI_UPDATER_PUBLIC_KEY").is_none() {
            assert!(updater_public_key().is_none());
        }
        if option_env!("ATRISBRIDGE_UPDATE_ENDPOINT").is_none() {
            assert!(updater_endpoint().is_none());
        }
    }

    #[test]
    fn update_channel_is_bounded() {
        assert!(matches!(
            updater_channel(),
            "preview" | "stable" | "development"
        ));
    }
}
