use std::time::Duration;

use reqwest::{blocking::Client, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::atris_auth::{self, AtrisHubAuthState};

const ACTIVE_DEVICE_URL: &str = "https://atrishub.com/api/mcp/device/v1/active";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REMOTE_CLIENTS: usize = 50;
const MAX_DISPLAY_NAME_CHARS: usize = 160;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveDeviceResponse {
    clients: Vec<ActiveDeviceClient>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveDeviceClient {
    principal: String,
    display_name: String,
    active_on_this_device: bool,
    authorized_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMcpGrantClient {
    principal: String,
    display_name: String,
    active_on_this_device: bool,
    authorized_at: Option<String>,
    updated_at: Option<String>,
}

#[tauri::command]
pub async fn list_remote_mcp_grant_clients(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
) -> Result<Vec<RemoteMcpGrantClient>, String> {
    let auth_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(credential) = atris_auth::ensure_desktop_session_credential(&app, &auth_state)? else {
            return Ok(Vec::new());
        };

        let client = discovery_client()?;
        let response = client
            .get(ACTIVE_DEVICE_URL)
            .bearer_auth(&credential.access_token)
            .send()
            .map_err(|error| format!("Could not query AtrisHub remote MCP clients: {error}"))?;

        if matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err("AtrisHub rejected the current Desktop session while discovering remote MCP clients.".into());
        }
        if !response.status().is_success() {
            return Err(format!(
                "AtrisHub remote MCP client discovery failed ({}).",
                response.status()
            ));
        }

        let payload: ActiveDeviceResponse = response
            .json()
            .map_err(|error| format!("AtrisHub returned invalid remote MCP client metadata: {error}"))?;
        let mut clients = Vec::new();
        for client in payload.clients.into_iter().take(MAX_REMOTE_CLIENTS) {
            if !valid_remote_principal(&client.principal) {
                continue;
            }
            clients.push(RemoteMcpGrantClient {
                principal: client.principal,
                display_name: bounded_display_name(&client.display_name),
                active_on_this_device: client.active_on_this_device,
                authorized_at: client.authorized_at,
                updated_at: client.updated_at,
            });
        }
        clients.sort_by(|left, right| left.principal.cmp(&right.principal));
        clients.dedup_by(|left, right| left.principal == right.principal);
        clients.sort_by(|left, right| {
            right
                .active_on_this_device
                .cmp(&left.active_on_this_device)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.principal.cmp(&right.principal))
        });
        Ok(clients)
    })
    .await
    .map_err(|error| format!("AtrisHub MCP discovery worker failed: {error}"))?
}

#[tauri::command]
pub async fn revoke_remote_mcp_grant_client(
    app: AppHandle,
    state: State<'_, AtrisHubAuthState>,
    principal: String,
) -> Result<bool, String> {
    if !valid_remote_principal(&principal) {
        return Err("Remote MCP client principal is invalid.".into());
    }
    let auth_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(credential) = atris_auth::ensure_desktop_session_credential(&app, &auth_state)? else {
            return Err("Sign in to AtrisHub before revoking a remote MCP client.".into());
        };
        let client = discovery_client()?;
        let url = format!("{ACTIVE_DEVICE_URL}/clients/{principal}");
        let response = client
            .delete(url)
            .bearer_auth(&credential.access_token)
            .send()
            .map_err(|error| format!("Could not revoke AtrisHub remote MCP client: {error}"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err("AtrisHub rejected the current Desktop session while revoking the remote MCP client.".into());
        }
        if !response.status().is_success() {
            return Err(format!(
                "AtrisHub remote MCP client revocation failed ({}).",
                response.status()
            ));
        }
        Ok(true)
    })
    .await
    .map_err(|error| format!("AtrisHub MCP revocation worker failed: {error}"))?
}

fn discovery_client() -> Result<Client, String> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("AtrisBridge-Desktop")
        .build()
        .map_err(|error| format!("Could not initialize AtrisHub MCP discovery client: {error}"))
}

fn valid_remote_principal(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("mcp.remote.") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounded_display_name(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if matches!(character, '\r' | '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded = compact
        .chars()
        .take(MAX_DISPLAY_NAME_CHARS)
        .collect::<String>();
    if bounded.is_empty() {
        "Remote MCP client".into()
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_endpoint_is_https_and_device_scoped() {
        assert_eq!(
            ACTIVE_DEVICE_URL,
            "https://atrishub.com/api/mcp/device/v1/active"
        );
    }

    #[test]
    fn remote_principal_validation_requires_canonical_hash() {
        assert!(valid_remote_principal(
            "mcp.remote.0123456789abcdef0123456789abcdef"
        ));
        assert!(!valid_remote_principal("mcp.remote.0123456789abcdef"));
        assert!(!valid_remote_principal("mcp.local.codex"));
        assert!(!valid_remote_principal("mcp.remote.bad/value"));
        assert!(!valid_remote_principal(
            "mcp.remote.0123456789abcdef0123456789abcdeg"
        ));
    }

    #[test]
    fn display_names_are_compacted_and_bounded() {
        assert_eq!(
            bounded_display_name("  ChatGPT\n  Desktop  "),
            "ChatGPT Desktop"
        );
        assert_eq!(bounded_display_name("\n\t"), "Remote MCP client");
        assert!(bounded_display_name(&"x".repeat(400)).chars().count() <= MAX_DISPLAY_NAME_CHARS);
    }
}
