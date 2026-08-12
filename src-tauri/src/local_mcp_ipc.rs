use std::{
    env,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::{mcp_dispatch, secure_store, services};

const IPC_VERSION: u32 = 1;
const MAX_IPC_FRAME_BYTES: usize = 24 * 1024 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_ID_CHARS: usize = 128;
const MAX_METHOD_CHARS: usize = 64;
const MAX_SAFE_ERROR_CHARS: usize = 2_048;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalMcpEndpointCredential {
    version: u32,
    address: String,
    token: String,
    instance_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IpcRequest {
    version: u32,
    request_id: String,
    token: String,
    client_kind: String,
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IpcResponse {
    version: u32,
    request_id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<IpcError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IpcError {
    code: &'static str,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolCallParams {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskIdParams {
    task_id: String,
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    // Never leave a previous process credential as the active discovery record.
    secure_store::delete_local_mcp_endpoint()?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("Could not bind local MCP authority endpoint: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("Could not resolve local MCP authority endpoint: {error}"))?;
    if !address.ip().is_loopback() {
        return Err("Refusing to expose the local MCP authority beyond loopback.".into());
    }

    let endpoint = LocalMcpEndpointCredential {
        version: IPC_VERSION,
        address: address.to_string(),
        token: generate_endpoint_token(),
        instance_id: Uuid::new_v4().to_string(),
    };
    let encoded = serde_json::to_string(&endpoint)
        .map_err(|error| format!("Could not encode local MCP endpoint credential: {error}"))?;
    secure_store::store_local_mcp_endpoint(&encoded)?;

    let app = app.clone();
    let endpoint_for_thread = endpoint.clone();
    thread::Builder::new()
        .name("atrisbridge-local-mcp".into())
        .spawn(move || serve(listener, app, endpoint_for_thread))
        .map_err(|error| {
            let _ = secure_store::delete_local_mcp_endpoint();
            format!("Could not start local MCP authority endpoint: {error}")
        })?;
    Ok(())
}

fn serve(listener: TcpListener, app: AppHandle, endpoint: LocalMcpEndpointCredential) {
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("AtrisBridge local MCP accept failed: {error}");
                continue;
            }
        };
        let app = app.clone();
        let endpoint = endpoint.clone();
        if let Err(error) = thread::Builder::new()
            .name("atrisbridge-mcp-ipc".into())
            .spawn(move || {
                if let Err(error) = handle_connection(stream, &app, &endpoint) {
                    eprintln!("AtrisBridge local MCP request failed: {error}");
                }
            })
        {
            eprintln!("AtrisBridge could not create local MCP request worker: {error}");
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    app: &AppHandle,
    endpoint: &LocalMcpEndpointCredential,
) -> Result<(), String> {
    let peer = stream
        .peer_addr()
        .map_err(|error| format!("Could not inspect local MCP peer: {error}"))?;
    if !peer.ip().is_loopback() {
        return Err("Rejected non-loopback local MCP peer.".into());
    }
    stream
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("Could not configure MCP read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("Could not configure MCP write timeout: {error}"))?;

    let request: IpcRequest = read_frame(&mut stream)?;
    let request_id = validated_request_id(&request.request_id)?;
    if request.version != IPC_VERSION {
        return write_error(
            &mut stream,
            request_id,
            "unsupported_version",
            "Local MCP IPC protocol version is not supported.".into(),
        );
    }
    if !constant_time_eq(request.token.as_bytes(), endpoint.token.as_bytes()) {
        return write_error(
            &mut stream,
            request_id,
            "unauthorized",
            "Local MCP authority authentication failed.".into(),
        );
    }
    let principal = match principal_for_client_kind(&request.client_kind) {
        Some(value) => value,
        None => {
            return write_error(
                &mut stream,
                request_id,
                "unauthorized_client",
                "Local MCP client kind is not authorized.".into(),
            )
        }
    };
    if request.method.is_empty() || request.method.chars().count() > MAX_METHOD_CHARS {
        return write_error(
            &mut stream,
            request_id,
            "invalid_request",
            "Local MCP IPC method is invalid.".into(),
        );
    }

    let result = dispatch(app, principal, &request.method, request.params);
    match result {
        Ok(result) => write_frame(
            &mut stream,
            &IpcResponse {
                version: IPC_VERSION,
                request_id: request_id.to_string(),
                ok: true,
                result: Some(result),
                error: None,
            },
        ),
        Err(error) => write_error(
            &mut stream,
            request_id,
            "authority_error",
            redact_local_paths(app, &error),
        ),
    }
}

fn dispatch(
    app: &AppHandle,
    principal: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    match method {
        "health" => Ok(json!({
            "protocolVersion": crate::mcp_core::MCP_PROTOCOL_VERSION,
            "authority": "AtrisBridge Desktop",
        })),
        "manifest" => {
            require_empty_object(params)?;
            mcp_dispatch::manifest_value()
        }
        "tool.call" => {
            let params: ToolCallParams = decode_params(params)?;
            mcp_dispatch::dispatch_tool(app, principal, &params.name, params.arguments)
        }
        "task.get" => {
            let params: TaskIdParams = decode_params(params)?;
            serde_json::to_value(mcp_dispatch::task_snapshot(
                app,
                principal,
                &params.task_id,
            )?)
            .map_err(|error| format!("Could not encode MCP task snapshot: {error}"))
        }
        "task.cancel" => {
            let params: TaskIdParams = decode_params(params)?;
            serde_json::to_value(mcp_dispatch::cancel_task(app, principal, &params.task_id)?)
                .map_err(|error| format!("Could not encode MCP task cancellation: {error}"))
        }
        _ => Err("Local MCP IPC method is not supported.".into()),
    }
}

fn decode_params<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("Invalid local MCP IPC parameters: {error}"))
}

fn require_empty_object(value: Value) -> Result<(), String> {
    match value {
        Value::Object(value) if value.is_empty() => Ok(()),
        _ => Err("Local MCP IPC method does not accept parameters.".into()),
    }
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut TcpStream) -> Result<T, String> {
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|error| format!("Could not read local MCP frame header: {error}"))?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_IPC_FRAME_BYTES {
        return Err("Local MCP IPC frame exceeded the safety bound.".into());
    }
    let mut payload = vec![0u8; length];
    stream
        .read_exact(&mut payload)
        .map_err(|error| format!("Could not read local MCP frame payload: {error}"))?;
    serde_json::from_slice(&payload)
        .map_err(|error| format!("Could not decode local MCP frame: {error}"))
}

fn write_frame<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<(), String> {
    let payload = serde_json::to_vec(value)
        .map_err(|error| format!("Could not encode local MCP response: {error}"))?;
    if payload.is_empty() || payload.len() > MAX_IPC_FRAME_BYTES {
        return Err("Local MCP response exceeded the safety bound.".into());
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| "Local MCP response length is invalid.".to_string())?;
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|_| stream.write_all(&payload))
        .and_then(|_| stream.flush())
        .map_err(|error| format!("Could not write local MCP response: {error}"))
}

fn write_error(
    stream: &mut TcpStream,
    request_id: &str,
    code: &'static str,
    message: String,
) -> Result<(), String> {
    write_frame(
        stream,
        &IpcResponse {
            version: IPC_VERSION,
            request_id: request_id.to_string(),
            ok: false,
            result: None,
            error: Some(IpcError { code, message }),
        },
    )
}

fn principal_for_client_kind(client_kind: &str) -> Option<&'static str> {
    match client_kind {
        "codex" => Some("mcp.codex"),
        "claude" => Some("mcp.claude"),
        "generic" => Some("mcp.generic"),
        _ => None,
    }
}

fn validated_request_id(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || value.chars().count() > MAX_REQUEST_ID_CHARS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Local MCP request identifier is invalid.".into());
    }
    Ok(value)
}

fn generate_endpoint_token() -> String {
    let material = format!(
        "{}:{}:{}:{}",
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4()
    );
    blake3::hash(material.as_bytes()).to_hex().to_string()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

pub(crate) fn redact_local_paths(app: &AppHandle, message: &str) -> String {
    let mut redacted = message.to_string();
    if let Ok(workspaces) = services::workspace::list(app) {
        for workspace in workspaces {
            redact_path_variants(&mut redacted, &workspace.local_path, "<workspace>");
            if let Ok(canonical) = std::path::Path::new(&workspace.local_path).canonicalize() {
                redact_path_variants(&mut redacted, &canonical.to_string_lossy(), "<workspace>");
            }
        }
    }
    if let Ok(app_data) = app.path().app_data_dir() {
        redact_path_variants(&mut redacted, &app_data.to_string_lossy(), "<app-data>");
    }
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(home) = env::var(key) {
            redact_path_variants(&mut redacted, &home, "<home>");
        }
    }
    let mut chars = redacted.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_SAFE_ERROR_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn redact_path_variants(message: &mut String, path: &str, replacement: &str) {
    let path = path.trim();
    if path.len() < 3 {
        return;
    }
    *message = message.replace(path, replacement);
    let slash = path.replace('\\', "/");
    if slash != path {
        *message = message.replace(&slash, replacement);
    }
    let backslash = path.replace('/', "\\");
    if backslash != path {
        *message = message.replace(&backslash, replacement);
    }
}

fn empty_object() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_client_kinds_map_to_fixed_principals() {
        assert_eq!(principal_for_client_kind("codex"), Some("mcp.codex"));
        assert_eq!(principal_for_client_kind("claude"), Some("mcp.claude"));
        assert_eq!(principal_for_client_kind("generic"), Some("mcp.generic"));
        assert_eq!(principal_for_client_kind("mcp.codex"), None);
        assert_eq!(principal_for_client_kind("unknown"), None);
    }

    #[test]
    fn endpoint_token_comparison_does_not_accept_prefixes() {
        assert!(constant_time_eq(b"abcdef", b"abcdef"));
        assert!(!constant_time_eq(b"abcdef", b"abcdeg"));
        assert!(!constant_time_eq(b"abcdef", b"abc"));
    }

    #[test]
    fn request_ids_are_bounded_and_ascii_safe() {
        assert!(validated_request_id("request_1-2").is_ok());
        assert!(validated_request_id("../request").is_err());
        assert!(validated_request_id("").is_err());
    }
}
