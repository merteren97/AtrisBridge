use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::atris_auth::DesktopSessionCredential;

pub(crate) const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_SAFE_ERROR_CHARS: usize = 2_048;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayRequest {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    request_id: String,
    reply_to: String,
    user_id: String,
    device_id: String,
    desktop_session_id: String,
    session_generation: u64,
    connection_id: String,
    pub(crate) client: RelayClient,
    pub(crate) mcp: RelayMcpRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayClient {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) principal: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayMcpRequest {
    protocol_version: String,
    method_header: String,
    name_header: Option<String>,
    pub(crate) message: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponseBinding {
    request_id: String,
    reply_to: String,
    connection_id: String,
}

impl ResponseBinding {
    pub(crate) fn from_request(request: &RelayRequest) -> Self {
        Self {
            request_id: request.request_id.clone(),
            reply_to: request.reply_to.clone(),
            connection_id: request.connection_id.clone(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RelayFailure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

pub(crate) fn validate_relay_request(
    credential: &DesktopSessionCredential,
    request: &RelayRequest,
) -> Result<(), RelayFailure> {
    if request.kind != "relay_request" || request.version != 1 {
        return Err(relay_failure(
            "invalid_envelope",
            "Remote relay envelope is not supported.",
        ));
    }
    if !canonical_uuid_v4(&request.request_id) || !canonical_uuid_v4(&request.connection_id) {
        return Err(relay_failure(
            "invalid_envelope",
            "Remote relay request or connection identifier is invalid.",
        ));
    }
    if !safe_instance_id(&request.reply_to) {
        return Err(relay_failure(
            "invalid_envelope",
            "Remote relay reply target is invalid.",
        ));
    }
    if request.user_id != credential.user_id || request.device_id != credential.device_id {
        return Err(relay_failure(
            "identity_mismatch",
            "Remote relay request is not bound to this signed-in AtrisBridge device.",
        ));
    }
    if request.desktop_session_id != credential.desktop_session_id
        || request.session_generation != credential.session_generation
    {
        return Err(relay_failure(
            "session_stale",
            "Remote relay request is bound to a stale AtrisBridge Desktop session.",
        ));
    }
    if request.client.id.is_empty()
        || request.client.id.len() > 512
        || request.client.name.trim().is_empty()
        || request.client.name.len() > 160
        || request.client.name.chars().any(char::is_control)
    {
        return Err(relay_failure(
            "invalid_client",
            "Remote MCP client metadata is invalid.",
        ));
    }
    if request.client.principal != remote_principal(&request.client.id) {
        return Err(relay_failure(
            "invalid_client",
            "Remote MCP client principal does not match its OAuth client identity.",
        ));
    }
    if request.mcp.protocol_version != MCP_PROTOCOL_VERSION {
        return Err(relay_failure(
            "unsupported_protocol",
            "Remote MCP protocol version is not supported.",
        ));
    }
    validate_mcp_request_metadata(&request.mcp)
        .map_err(|message| relay_failure("invalid_mcp_request", &message))
}

fn validate_mcp_request_metadata(mcp: &RelayMcpRequest) -> Result<(), String> {
    if mcp.method_header.is_empty() || mcp.method_header.len() > 120 {
        return Err("Mcp-Method is invalid.".into());
    }
    if mcp
        .name_header
        .as_ref()
        .is_some_and(|name| name.len() > 512)
    {
        return Err("Mcp-Name is too large.".into());
    }
    let message = mcp
        .message
        .as_object()
        .ok_or_else(|| "MCP request must be a JSON object.".to_string())?;
    if message.get("jsonrpc") != Some(&Value::String("2.0".into())) {
        return Err("MCP request must use JSON-RPC 2.0.".into());
    }
    let id = message
        .get("id")
        .ok_or_else(|| "MCP requests must have a request id.".to_string())?;
    if id.is_null() || !(id.is_string() || id.is_number()) {
        return Err("MCP request id must be a string or number.".into());
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP request method is missing.".to_string())?;
    if method != mcp.method_header {
        return Err("Mcp-Method does not match the JSON-RPC method.".into());
    }
    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP request params are required.".to_string())?;
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP request _meta is required.".to_string())?;
    if meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        != Some(MCP_PROTOCOL_VERSION)
    {
        return Err("MCP request _meta protocolVersion is invalid.".into());
    }
    if !meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .is_some_and(Value::is_object)
    {
        return Err("MCP request clientCapabilities are required.".into());
    }
    if method == "tools/call" {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "tools/call requires a tool name.".to_string())?;
        if mcp.name_header.as_deref() != Some(name) {
            return Err("Mcp-Name does not match the AtrisBridge tool name.".into());
        }
    } else if mcp.name_header.is_some() {
        return Err("Mcp-Name is not valid for this MCP method.".into());
    }
    Ok(())
}

pub(crate) fn relay_success_response(binding: &ResponseBinding, result: Value) -> Value {
    json!({
        "type": "relay_response",
        "version": 1,
        "requestId": binding.request_id,
        "replyTo": binding.reply_to,
        "connectionId": binding.connection_id,
        "ok": true,
        "result": result
    })
}

pub(crate) fn relay_error_response(
    request: &RelayRequest,
    code: &'static str,
    message: &str,
) -> Value {
    relay_error_response_parts(&ResponseBinding::from_request(request), code, message)
}

pub(crate) fn relay_error_response_parts(
    binding: &ResponseBinding,
    code: &'static str,
    message: &str,
) -> Value {
    json!({
        "type": "relay_response",
        "version": 1,
        "requestId": binding.request_id,
        "replyTo": binding.reply_to,
        "connectionId": binding.connection_id,
        "ok": false,
        "error": {"code": code, "message": bounded_error(message)}
    })
}

pub(crate) fn remote_principal(client_id: &str) -> String {
    let digest = Sha256::digest(client_id.as_bytes());
    let mut prefix = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(prefix, "{byte:02x}");
    }
    format!("mcp.remote.{prefix}")
}

fn relay_failure(code: &'static str, message: &str) -> RelayFailure {
    RelayFailure {
        code,
        message: bounded_error(message),
    }
}

fn canonical_uuid_v4(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|parsed| {
        parsed.get_version_num() == 4 && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
    })
}

fn safe_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

pub(crate) fn bounded_error(message: &str) -> String {
    let mut chars = message.chars();
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn test_credential() -> DesktopSessionCredential {
        DesktopSessionCredential {
            user_id: "user-1".into(),
            device_id: "device-1".into(),
            desktop_session_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            session_generation: 7,
            access_token: "test-value".into(),
            access_token_expires_at: chrono::Utc::now() + ChronoDuration::minutes(10),
            session_expires_at: chrono::Utc::now() + ChronoDuration::days(10),
        }
    }

    fn test_request() -> RelayRequest {
        let client_id = "client-123".to_string();
        RelayRequest {
            kind: "relay_request".into(),
            version: 1,
            request_id: "123e4567-e89b-42d3-a456-426614174001".into(),
            reply_to: "gateway-host-1-123e4567-e89b-42d3-a456-426614174002".into(),
            user_id: "user-1".into(),
            device_id: "device-1".into(),
            desktop_session_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            session_generation: 7,
            connection_id: "123e4567-e89b-42d3-a456-426614174003".into(),
            client: RelayClient {
                id: client_id.clone(),
                name: "Remote client".into(),
                principal: remote_principal(&client_id),
            },
            mcp: RelayMcpRequest {
                protocol_version: MCP_PROTOCOL_VERSION.into(),
                method_header: "tools/list".into(),
                name_header: None,
                message: json!({
                    "jsonrpc": "2.0",
                    "id": "rpc-1",
                    "method": "tools/list",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
                            "io.modelcontextprotocol/clientCapabilities": {}
                        }
                    }
                }),
            },
        }
    }

    #[test]
    fn remote_principal_matches_gateway_contract() {
        assert_eq!(
            remote_principal("client-123"),
            "mcp.remote.b44ea687b506d5ca725c434cbe69d0cd"
        );
    }

    #[test]
    fn relay_request_is_generation_and_connection_fenced() {
        let credential = test_credential();
        assert!(validate_relay_request(&credential, &test_request()).is_ok());

        let mut stale = test_request();
        stale.session_generation += 1;
        assert_eq!(
            validate_relay_request(&credential, &stale)
                .expect_err("stale generation")
                .code,
            "session_stale"
        );

        let mut invalid_connection = test_request();
        invalid_connection.connection_id = "not-a-uuid".into();
        assert_eq!(
            validate_relay_request(&credential, &invalid_connection)
                .expect_err("invalid connection")
                .code,
            "invalid_envelope"
        );
    }

    #[test]
    fn relay_response_echoes_connection_fence() {
        let request = test_request();
        let response = relay_success_response(&ResponseBinding::from_request(&request), json!({}));
        assert_eq!(response["requestId"], request.request_id);
        assert_eq!(response["replyTo"], request.reply_to);
        assert_eq!(response["connectionId"], request.connection_id);
    }
}
