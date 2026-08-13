use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{
    blocking::{Client, Response},
    StatusCode,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{database, secure_store};

const AUTH_BASE_URL: &str = "https://atrishub.com/api/desktop/v1/auth";
const DEVICE_ID_KEY: &str = "atrishub_desktop_device_id";
const CACHED_IDENTITY_KEY: &str = "atrishub_cached_identity";
const REQUEST_TIMEOUT_SECONDS: u64 = 20;
const LOGOUT_TIMEOUT_SECONDS: u64 = 5;
const RELAY_REFRESH_MARGIN_SECONDS: i64 = 120;
const MAX_SAFE_SESSION_GENERATION: u64 = 9_007_199_254_740_991;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 1024;
static AUTH_REFRESH_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
pub struct AtrisHubAuthState {
    inner: Arc<Mutex<AuthRuntime>>,
}

#[derive(Default)]
struct AuthRuntime {
    access_token: Option<String>,
    access_token_expires_at: Option<DateTime<Utc>>,
    desktop_session_id: Option<String>,
    session_generation: Option<u64>,
    session_expires_at: Option<DateTime<Utc>>,
    volatile_refresh_token: Option<String>,
    snapshot: Option<AuthSnapshot>,
}

#[derive(Debug, Clone)]
pub(crate) struct DesktopSessionCredential {
    pub(crate) user_id: String,
    pub(crate) device_id: String,
    pub(crate) desktop_session_id: String,
    pub(crate) session_generation: u64,
    pub(crate) access_token: String,
    pub(crate) access_token_expires_at: DateTime<Utc>,
    pub(crate) session_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ValidatedDesktopSessionBinding {
    desktop_session_id: String,
    session_generation: u64,
    access_token_expires_at: DateTime<Utc>,
    session_expires_at: DateTime<Utc>,
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
    desktop_session_id: String,
    session_generation: u64,
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

fn client_with_timeout(timeout: Duration) -> Result<Client, AuthFailure> {
    Client::builder()
        .timeout(timeout)
        .user_agent("AtrisBridge-Desktop")
        .build()
        .map_err(|error| AuthFailure {
            kind: FailureKind::Network,
            message: format!("Could not initialize AtrisHub client: {error}"),
        })
}

fn client() -> Result<Client, AuthFailure> {
    client_with_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
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

fn send_login(
    app: &AppHandle,
    email: &str,
    password: &str,
) -> Result<DesktopSessionResponse, AuthFailure> {
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
    if !valid_refresh_token(refresh_token) {
        return;
    }
    let Ok(client) = client_with_timeout(Duration::from_secs(LOGOUT_TIMEOUT_SECONDS)) else {
        return;
    };
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
    AuthFailure {
        kind: FailureKind::Server,
        message,
    }
}

fn get_meta(app: &AppHandle, key: &str) -> Result<Option<String>, String> {
    let connection = database::open_database(app)?;
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
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
    let Some(value) = get_meta(app, CACHED_IDENTITY_KEY)? else {
        return Ok(None);
    };
    serde_json::from_str(&value)
        .map(Some)
        .map_err(|error| format!("Could not read AtrisHub identity cache: {error}"))
}

fn clear_local_identity(app: &AppHandle) -> Result<(), String> {
    let vault = secure_store::delete_atrishub_refresh_token();
    let cache = delete_meta(app, CACHED_IDENTITY_KEY);
    vault.and(cache)
}

fn valid_refresh_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_REFRESH_TOKEN_BYTES
}

fn parse_future_timestamp(value: &str, label: &str) -> Result<DateTime<Utc>, String> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| format!("AtrisHub returned an invalid {label}."))?;
    if parsed <= Utc::now() {
        return Err(format!("AtrisHub returned an expired {label}."));
    }
    Ok(parsed)
}

fn validate_desktop_session_binding(
    desktop_session_id: &str,
    session_generation: u64,
) -> Result<(), String> {
    let parsed = Uuid::parse_str(desktop_session_id)
        .map_err(|_| "AtrisHub returned an invalid Desktop session identifier.".to_string())?;
    let canonical = parsed.hyphenated().to_string();
    if !canonical.eq_ignore_ascii_case(desktop_session_id) || parsed.get_version_num() != 4 {
        return Err("AtrisHub returned a non-canonical Desktop session identifier.".into());
    }
    if session_generation == 0 || session_generation > MAX_SAFE_SESSION_GENERATION {
        return Err("AtrisHub returned an invalid Desktop session generation.".into());
    }
    Ok(())
}

fn validate_desktop_session_response(
    response: &DesktopSessionResponse,
) -> Result<ValidatedDesktopSessionBinding, String> {
    validate_desktop_session_binding(&response.desktop_session_id, response.session_generation)?;
    if response.access_token.is_empty() || response.access_token.len() > MAX_ACCESS_TOKEN_BYTES {
        return Err("AtrisHub returned an invalid desktop access token.".into());
    }
    if !valid_refresh_token(&response.refresh_token) {
        return Err("AtrisHub returned an invalid desktop refresh token.".into());
    }
    let access_token_expires_at = parse_future_timestamp(
        &response.access_token_expires_at,
        "desktop access-token expiry",
    )?;
    let session_expires_at =
        parse_future_timestamp(&response.session_expires_at, "Desktop session expiry")?;
    if session_expires_at <= access_token_expires_at {
        return Err("AtrisHub returned an inconsistent Desktop session lifetime.".into());
    }
    Ok(ValidatedDesktopSessionBinding {
        desktop_session_id: response.desktop_session_id.clone(),
        session_generation: response.session_generation,
        access_token_expires_at,
        session_expires_at,
    })
}

fn commit_session(
    app: &AppHandle,
    state: &AtrisHubAuthState,
    response: DesktopSessionResponse,
    remember: bool,
) -> Result<AuthSnapshot, String> {
    let binding = match validate_desktop_session_response(&response) {
        Ok(binding) => binding,
        Err(error) => {
            send_logout(&response.refresh_token);
            return Err(error);
        }
    };
    if remember {
        if let Err(error) = secure_store::store_atrishub_refresh_token(&response.refresh_token) {
            send_logout(&response.refresh_token);
            let _ = clear_local_identity(app);
            return Err(error);
        }
    } else if let Err(error) = secure_store::delete_atrishub_refresh_token() {
        send_logout(&response.refresh_token);
        let _ = clear_local_identity(app);
        return Err(error);
    }
    let snapshot = AuthSnapshot::signed_in(&response, remember);
    if let Err(error) = cache_snapshot(app, &snapshot) {
        send_logout(&response.refresh_token);
        let _ = clear_local_identity(app);
        return Err(error);
    }
    let mut runtime = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
    runtime.access_token = Some(response.access_token);
    runtime.access_token_expires_at = Some(binding.access_token_expires_at);
    runtime.desktop_session_id = Some(binding.desktop_session_id);
    runtime.session_generation = Some(binding.session_generation);
    runtime.session_expires_at = Some(binding.session_expires_at);
    runtime.volatile_refresh_token = (!remember).then_some(response.refresh_token);
    runtime.snapshot = Some(snapshot.clone());
    Ok(snapshot)
}

fn current_desktop_session_credential(
    app: &AppHandle,
    state: &AtrisHubAuthState,
    minimum_access_validity_seconds: i64,
) -> Result<Option<DesktopSessionCredential>, String> {
    let runtime = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
    let Some(snapshot) = runtime.snapshot.as_ref() else {
        return Ok(None);
    };
    if snapshot.state != "signed_in" {
        return Ok(None);
    }
    let Some(user) = snapshot.user.as_ref() else {
        return Ok(None);
    };
    let (
        Some(access_token),
        Some(access_token_expires_at),
        Some(desktop_session_id),
        Some(session_generation),
        Some(session_expires_at),
    ) = (
        runtime.access_token.as_ref(),
        runtime.access_token_expires_at.as_ref(),
        runtime.desktop_session_id.as_ref(),
        runtime.session_generation,
        runtime.session_expires_at.as_ref(),
    )
    else {
        return Ok(None);
    };
    let now = Utc::now();
    let refresh_deadline = now + ChronoDuration::seconds(minimum_access_validity_seconds.max(0));
    if *access_token_expires_at <= refresh_deadline || *session_expires_at <= now {
        return Ok(None);
    }
    Ok(Some(DesktopSessionCredential {
        user_id: user.id.clone(),
        device_id: get_or_create_device_id(app)?,
        desktop_session_id: desktop_session_id.clone(),
        session_generation,
        access_token: access_token.clone(),
        access_token_expires_at: *access_token_expires_at,
        session_expires_at: *session_expires_at,
    }))
}

pub(crate) fn desktop_session_credential(
    app: &AppHandle,
    state: &AtrisHubAuthState,
) -> Result<Option<DesktopSessionCredential>, String> {
    current_desktop_session_credential(app, state, 0)
}

pub(crate) fn ensure_desktop_session_credential(
    app: &AppHandle,
    state: &AtrisHubAuthState,
) -> Result<Option<DesktopSessionCredential>, String> {
    if let Some(current) =
        current_desktop_session_credential(app, state, RELAY_REFRESH_MARGIN_SECONDS)?
    {
        return Ok(Some(current));
    }

    let _refresh_operation = AUTH_REFRESH_LOCK
        .lock()
        .map_err(|_| "AtrisHub refresh operation lock is unavailable.".to_string())?;
    if let Some(current) =
        current_desktop_session_credential(app, state, RELAY_REFRESH_MARGIN_SECONDS)?
    {
        return Ok(Some(current));
    }

    let remembered = secure_store::load_atrishub_refresh_token()?;
    let volatile = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?
        .volatile_refresh_token
        .clone();
    let remember = remembered.is_some();
    let Some(refresh_token) = remembered.as_deref().or(volatile.as_deref()) else {
        return Ok(None);
    };
    if !valid_refresh_token(refresh_token) {
        clear_local_identity(app)?;
        let mut runtime = state
            .inner
            .lock()
            .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
        *runtime = AuthRuntime::default();
        return Err("AtrisHub desktop refresh credential is invalid.".into());
    }

    match send_refresh(refresh_token) {
        Ok(response) => {
            commit_session(app, state, response, remember)?;
            current_desktop_session_credential(app, state, RELAY_REFRESH_MARGIN_SECONDS)
        }
        Err(failure) if matches!(failure.kind, FailureKind::Network | FailureKind::Server) => {
            Err(failure.message)
        }
        Err(failure) => {
            clear_local_identity(app)?;
            let mut runtime = state
                .inner
                .lock()
                .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
            *runtime = AuthRuntime::default();
            Err(failure.message)
        }
    }
}

fn set_runtime_snapshot(
    state: &AtrisHubAuthState,
    snapshot: AuthSnapshot,
) -> Result<AuthSnapshot, String> {
    let mut runtime = state
        .inner
        .lock()
        .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
    if snapshot.state != "signed_in" {
        runtime.access_token = None;
        runtime.access_token_expires_at = None;
        runtime.desktop_session_id = None;
        runtime.session_generation = None;
        runtime.session_expires_at = None;
    }
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
    tauri::async_runtime::spawn_blocking(move || match send_login(&app, &identity, &password) {
        Ok(response) => commit_session(&app, &auth_state, response, remember_device),
        Err(failure) => Err(failure.message),
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
        let _refresh_operation = AUTH_REFRESH_LOCK
            .lock()
            .map_err(|_| "AtrisHub refresh operation lock is unavailable.".to_string())?;
        let refresh_token = secure_store::load_atrishub_refresh_token()?;
        let Some(refresh_token) = refresh_token else {
            let snapshot = AuthSnapshot::signed_out();
            return set_runtime_snapshot(&auth_state, snapshot);
        };
        if !valid_refresh_token(&refresh_token) {
            clear_local_identity(&app)?;
            set_runtime_snapshot(&auth_state, AuthSnapshot::signed_out())?;
            return Err("AtrisHub desktop refresh credential is invalid.".into());
        }
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
        let _refresh_operation = AUTH_REFRESH_LOCK
            .lock()
            .map_err(|_| "AtrisHub refresh operation lock is unavailable.".to_string())?;
        let remembered = secure_store::load_atrishub_refresh_token()?;
        let volatile = auth_state
            .inner
            .lock()
            .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?
            .volatile_refresh_token
            .clone();
        let remote_token = remembered
            .as_deref()
            .or(volatile.as_deref())
            .filter(|value| valid_refresh_token(value))
            .map(str::to_owned);

        let clear_result = clear_local_identity(&app);
        {
            let mut runtime = auth_state
                .inner
                .lock()
                .map_err(|_| "AtrisHub authentication state is unavailable.".to_string())?;
            *runtime = AuthRuntime::default();
        }
        if let Some(token) = remote_token.as_deref() {
            send_logout(token);
        }
        clear_result?;
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
    fn snapshots_never_serialize_credentials_or_relay_binding() {
        let snapshot = AuthSnapshot::signed_out();
        let json = serde_json::to_string(&snapshot).expect("snapshot");
        assert!(!json.contains("accessToken"));
        assert!(!json.contains("refreshToken"));
        assert!(!json.contains("desktopSessionId"));
        assert!(!json.contains("sessionGeneration"));
        assert!(!json.contains("password"));
    }

    #[test]
    fn desktop_session_binding_requires_canonical_uuid_v4_and_safe_generation() {
        assert!(
            validate_desktop_session_binding("123e4567-e89b-42d3-a456-426614174000", 1).is_ok()
        );
        assert!(
            validate_desktop_session_binding("123e4567-e89b-12d3-a456-426614174000", 1).is_err()
        );
        assert!(validate_desktop_session_binding("123e4567e89b42d3a456426614174000", 1).is_err());
        assert!(
            validate_desktop_session_binding("123e4567-e89b-42d3-a456-426614174000", 0).is_err()
        );
        assert!(validate_desktop_session_binding(
            "123e4567-e89b-42d3-a456-426614174000",
            MAX_SAFE_SESSION_GENERATION + 1
        )
        .is_err());
    }

    #[test]
    fn relay_refresh_margin_and_refresh_token_bounds_are_conservative() {
        assert!(RELAY_REFRESH_MARGIN_SECONDS >= 60);
        assert!(RELAY_REFRESH_MARGIN_SECONDS < 10 * 60);
        assert!(valid_refresh_token("abrt_example.token"));
        assert!(!valid_refresh_token(""));
        assert!(!valid_refresh_token(&"x".repeat(MAX_REFRESH_TOKEN_BYTES + 1)));
    }
}