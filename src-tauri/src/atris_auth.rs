use std::{sync::{Arc, Mutex}, time::Duration};

use reqwest::{blocking::{Client, Response}, StatusCode};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{database, secure_store};

const AUTH_BASE_URL: &str = "https://atrishub.com/api/desktop/v1/auth";
const DEVICE_ID_KEY: &str = "atrishub_desktop_device_id";
const CACHED_IDENTITY_KEY: &str = "atrishub_cached_identity";
const REQUEST_TIMEOUT_SECONDS: u64 = 20;

#[derive(Clone, Default)]
pub struct AtrisHubAuthState {
    inner: Arc<Mutex<AuthRuntime>>,
}

#[derive(Default)]
struct AuthRuntime {
    access_token: Option<String>,
    volatile_refresh_token: Option<String>,
    snapshot: Option<AuthSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtrisUser {
    id: String,
    email: String,
    username: String,
    name: Option<String>,
    avatar_url: Option<String>,
    role: String,
    locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtrisMembership {
    status: String,
    plan: String,
    starts_at: Option<String>,
    ends_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSnapshot {
    state: String,
    user: Option<AtrisUser>,
    membership: Option<AtrisMembership>,
    remembered: bool,
    offline: bool,
    message: Option<String>,
}

impl AuthSnapshot {
    fn signed_out() -> Self {
        Self {
            state: "signed_out".into(),
            user: None,
            membership: None,
            remembered: false,
            offline: false,
            message: None,
        }
    }

    fn signed_in(response: &DesktopSessionResponse, remembered: bool) -> Self {
        Self {
            state: "signed_in".into(),
            user: Some(response.user.clone()),
            membership: Some(response.membership.clone()),
            remembered,
            offline: false,
            message: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSessionResponse {
    user: AtrisUser,
    membership: AtrisMembership,
    access_token: String,
    access_token_expires_at: String,
    refresh_token: String,
    session_expires_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest<'a> {
    email: &'a str,
    password: &'a str,
    device_id: &'a str,
    device_name: &'a str,
    platform: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Network,
    Unauthorized,
    Server,
    Invalid,
}

#[derive(Debug)]
struct AuthFailure {
    kind: FailureKind,
    message: String,
}

fn client() -> Result<Client, AuthFailure> {
    Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .user_agent("AtrisBridge-Desktop")
        .build()
        .map_err(|error| AuthFailure {
            kind: FailureKind::Network,
            message: format!("Could not initialize AtrisHub client: {error}"),
        })
}

fn parse_response(response: Response) -> Result<DesktopSessionResponse, AuthFailure> {
    let status = response.status();
    if status.is_success() {
        return response.json().map_err(|error| AuthFailure {
            kind: FailureKind::Server,
            message: format!("AtrisHub returned an invalid desktop session response: {error}"),
        });
    }
    let message = response
        .json::<ErrorResponse>()
        .ok()
        .and_then(|value| value.error)
        .unwrap_or_else(|| format!("AtrisHub request failed ({status})."));
    let kind = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        FailureKind::Unauthorized
    } else if status.is_server_error() {
        FailureKind::Server
    } else {
        FailureKind::Invalid
    };
    Err(AuthFailure { kind, message })
}

fn send_login(app: &AppHandle, email: &str, password: &str) -> Result<DesktopSessionResponse, AuthFailure> {
    let device_id = get_or_create_device_id(app).map_err(storage_failure)?;
    let device_name = format!("AtrisBridge {}", std::env::consts::OS);
    let request = LoginRequest {
        email,
        password,
        device_id: &device_id,
        device_name: &device_name,
        platform: std::env::consts::OS,
    };
    let response = client()?
        .post(format!("{AUTH_BASE_URL}/login"))
        .json(&request)
        .send()
        .map_err(network_failure)?;
    parse_response(response)
}

fn send_refresh(refresh_token: &str) -> Result<DesktopSessionResponse, AuthFailure> {
    let response = client()?
        .post(format!("{AUTH_BASE_URL}/refresh"))
        .json(&RefreshRequest { refresh_token })
        .send()
        .map_err(network_failure)?;
    parse_response(response)
}

fn send_logout(refresh_token: &str) {
    let Ok(client) = client() else { return; };
    let _ = client
        .post(format!("{AUTH_BASE_URL}/logout"))
        .json(&RefreshRequest { refresh_token })
        .send();
}

fn network_failure(error: reqwest::Error) -> AuthFailure {
    AuthFailure {
        kind: FailureKind::Network,
        message: format!("AtrisHub is unreachable: {error}"),
    }
}

fn storage_failure(message: String) -> AuthFailure {
    AuthFailure { kind: FailureKind::Server, message }
}

fn get_meta(app: &AppHandle, key: &str) -> Result<Option<String>, String> {
    let connection = database::open_database(app)?;
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| row.get(0))
        .optional()
        .map_err(|error| format!("Could not read AtrisBridge app metadata: {error}"))
}

fn set_meta(app: &AppHandle, key: &str, value: &str) -> Result<(), String> {
    let connection = database::open_database(app)?;
    connection
        .execute(
            "INSERT INTO app_meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|error| format!("Could not save AtrisBridge app metadata: {error}"))?;
    Ok(())
}

fn delete_meta(app: &AppHandle, key: &str) -> Result<(), String> {
    let connection = database::open_database(app)?;
    connection
        .execute("DELETE FROM app_meta WHERE key = ?1", [key])
        .map_err(|error| format!("Could not clear AtrisBridge app metadata: {error}"))?;
    Ok(())
}

fn get_or_create_device_id(app: &AppHandle) -> Result<String, String> {
    if let Some(value) = get_meta(app, DEVICE_ID_KEY)? {
        if Uuid::parse_str(&value).is_ok() {
            return Ok(value);
        }
    }
    let value = Uuid::new_v4().to_string();
    set_meta(app, DEVICE_ID_KEY, &value)?;
    Ok(value)
}

fn cache_snapshot(app: &AppHandle, snapshot: &AuthSnapshot) -> Result<(), String> {
    let value = serde_json::to_string(snapshot)
        .map_err(|error| format!("Could not serialize AtrisHub identity cache: {error}"))?;
    set_meta(app, CACHED_IDENTITY_KEY, &value)
}

fn cached_snapshot(app: &AppHandle) -> Result<Option<AuthSnapshot>, String> {
    let Some(value) = get_meta(app, CACHED_IDENTITY_KEY)? else { return Ok(None); };
    serde_json::from_str(&value)
        .map(Some)
        .map_err(|error| format!("Could not read AtrisHub identity cache: {error}"))
}

fn clear_local_identity(app: &AppHandle) -> Result<(), String> {
    let vault = secure_store::delete_atrishub_refresh_token();
    let cache = delete_meta(app, CACHED_IDENTITY_KEY);
    vault.and(cache)
}

fn commit_session(
    app: &AppHandle,
    state: &AtrisHubAuthState,
    response: DesktopSessionResponse,
    remember: bool,
) -> Result<AuthSnapshot, String> {
    if remember {
        if let Err(error) = secure_store::store_atrishub_refresh_token(&response.refresh_token) {
            send_logout(&response.refresh_token);
            return Err(error);
        }
    } else {
        secure_store::delete_atrishub_refresh_token()?;
    }
    let snapshot = AuthSnapshot::signed_in(&response, remember);
    cache_snapshot(app, &snapshot)?;
    let mut runtime = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
    runtime.access_token = Some(response.access_token);
    runtime.volatile_refresh_token = (!remember).then_some(response.refresh_token);
    runtime.snapshot = Some(snapshot.clone());
    Ok(snapshot)
}

fn set_runtime_snapshot(state: &AtrisHubAuthState, snapshot: AuthSnapshot) -> Result<AuthSnapshot, String> {
    let mut runtime = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
    runtime.snapshot = Some(snapshot.clone());
    Ok(snapshot)
}

#[tauri::command]
pub fn atrishub_auth_status(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
) -> Result<AuthSnapshot, String> {
    if let Some(snapshot) = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?
        .snapshot
        .clone()
    {
        return Ok(snapshot);
    }
    cached_snapshot(&app).map(|value| value.unwrap_or_else(AuthSnapshot::signed_out))
}

#[tauri::command]
pub async fn login_atrishub(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
    email: String,
    password: String,
    remember_device: bool,
) -> Result<AuthSnapshot, String> {
    let identity = email.trim().to_string();
    if identity.is_empty() || password.is_empty() {
        return Err("Email/username and password are required.".into());
    }
    let auth_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        match send_login(&app, &identity, &password) {
            Ok(response) => commit_session(&app, &auth_state, response, remember_device),
            Err(failure) => Err(failure.message),
        }
    })
    .await
    .map_err(|error| format!("AtrisHub login task failed: {error}"))?
}

#[tauri::command]
pub async fn restore_atrishub_session(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
) -> Result<AuthSnapshot, String> {
    let auth_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let refresh_token = secure_store::load_atrishub_refresh_token()?;
        let Some(refresh_token) = refresh_token else {
            let snapshot = AuthSnapshot::signed_out();
            return set_runtime_snapshot(&auth_state, snapshot);
        };
        match send_refresh(&refresh_token) {
            Ok(response) => commit_session(&app, &auth_state, response, true),
            Err(failure) if matches!(failure.kind, FailureKind::Network | FailureKind::Server) => {
                let mut snapshot = cached_snapshot(&app)?.unwrap_or_else(AuthSnapshot::signed_out);
                if snapshot.user.is_some() {
                    snapshot.state = "offline_cached".into();
                    snapshot.remembered = true;
                    snapshot.offline = true;
                    snapshot.message = Some("AtrisHub is temporarily unreachable. Your remembered account remains on this device; local AtrisBridge work stays available.".into());
                }
                set_runtime_snapshot(&auth_state, snapshot)
            }
            Err(failure) => {
                clear_local_identity(&app)?;
                set_runtime_snapshot(&auth_state, AuthSnapshot::signed_out())?;
                Err(failure.message)
            }
        }
    })
    .await
    .map_err(|error| format!("AtrisHub session restore task failed: {error}"))?
}

#[tauri::command]
pub async fn logout_atrishub(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
) -> Result<AuthSnapshot, String> {
    let auth_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let remembered = secure_store::load_atrishub_refresh_token()?;
        let volatile = auth_state
            .inner
            .lock()
            .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?
            .volatile_refresh_token
            .clone();
        if let Some(token) = remembered.as_deref().or(volatile.as_deref()) {
            send_logout(token);
        }
        clear_local_identity(&app)?;
        let mut runtime = auth_state
            .inner
            .lock()
            .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
        *runtime = AuthRuntime::default();
        Ok(AuthSnapshot::signed_out())
    })
    .await
    .map_err(|error| format!("AtrisHub logout task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_endpoint_is_https_and_dedicated_to_desktop_sessions() {
        assert_eq!(AUTH_BASE_URL, "https://atrishub.com/api/desktop/v1/auth");
    }

    #[test]
    fn snapshots_never_serialize_credentials() {
        let snapshot = AuthSnapshot::signed_out();
        let json = serde_json::to_string(&snapshot).expect("snapshot");
        assert!(!json.contains("accessToken"));
        assert!(!json.contains("refreshToken"));
        assert!(!json.contains("password"));
    }
}
