use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::{
    sync::{mpsc, Semaphore},
    time::{interval, sleep, Duration, MissedTickBehavior},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
};
use uuid::Uuid;

use crate::{
    ai_task::{AiTaskRecord, AiTaskResult},
    atris_auth::{self, AtrisHubAuthState, DesktopRelayCredential},
    local_mcp_ipc, mcp_core, mcp_dispatch,
};

const RELAY_URL: &str = "wss://atrishub.com/api/mcp/relay/v1/connect";
const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
const TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";
const MAX_RELAY_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;
const MAX_LOCAL_INFLIGHT: usize = 32;
const MAX_SAFE_ERROR_CHARS: usize = 2_048;
const AUTH_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const SIGNED_OUT_RETRY: Duration = Duration::from_secs(5);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const TASK_RESULT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const TASK_POLL_INTERVAL_MS: u64 = 750;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayRequest {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    request_id: String,
    reply_to: String,
    user_id: String,
    device_id: String,
    client: RelayClient,
    mcp: RelayMcpRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayClient {
    id: String,
    name: String,
    principal: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelayMcpRequest {
    protocol_version: String,
    method_header: String,
    name_header: Option<String>,
    message: Value,
}

#[derive(Debug)]
struct RelayFailure {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
struct RpcFailure {
    code: i64,
    message: &'static str,
    detail: String,
}

pub fn setup(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        relay_loop(app).await;
    });
}

async fn relay_loop(app: AppHandle) {
    let mut reconnect_seconds = 1u64;
    loop {
        let credential = match load_relay_credential(app.clone()).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                reconnect_seconds = 1;
                sleep(SIGNED_OUT_RETRY).await;
                continue;
            }
            Err(error) => {
                eprintln!("AtrisBridge remote MCP credential refresh failed: {error}");
                sleep(backoff_delay(reconnect_seconds)).await;
                reconnect_seconds = (reconnect_seconds.saturating_mul(2)).min(30);
                continue;
            }
        };

        match connect_relay(&credential).await {
            Ok(socket) => {
                reconnect_seconds = 1;
                if let Err(error) = run_connection(app.clone(), credential, socket).await {
                    eprintln!("AtrisBridge remote MCP relay disconnected: {error}");
                }
            }
            Err(error) => {
                eprintln!("AtrisBridge remote MCP relay connection failed: {error}");
            }
        }

        sleep(backoff_delay(reconnect_seconds)).await;
        reconnect_seconds = (reconnect_seconds.saturating_mul(2)).min(30);
    }
}

async fn load_relay_credential(app: AppHandle) -> Result<Option<DesktopRelayCredential>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AtrisHubAuthState>();
        atris_auth::desktop_relay_credential(&app, state.inner())
    })
    .await
    .map_err(|error| format!("AtrisBridge remote MCP auth worker failed: {error}"))?
}

async fn connect_relay(
    credential: &DesktopRelayCredential,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut request = RELAY_URL
        .into_client_request()
        .map_err(|error| format!("Could not build AtrisHub relay request: {error}"))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
        .map_err(|_| "AtrisHub desktop access token could not be encoded safely.".to_string())?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    let (socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("Could not establish secure AtrisHub relay WebSocket: {error}"))?;
    Ok(socket)
}

async fn run_connection(
    app: AppHandle,
    credential: DesktopRelayCredential,
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<(), String> {
    let (mut writer, mut reader) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Message>(MAX_LOCAL_INFLIGHT * 2);
    let permits = Arc::new(Semaphore::new(MAX_LOCAL_INFLIGHT));
    let mut auth_tick = interval(AUTH_CHECK_INTERVAL);
    auth_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    auth_tick.tick().await;

    loop {
        tokio::select! {
            outgoing = outgoing_rx.recv() => {
                let Some(outgoing) = outgoing else {
                    return Ok(());
                };
                writer.send(outgoing).await
                    .map_err(|error| format!("Could not write AtrisHub relay WebSocket: {error}"))?;
            }
            incoming = reader.next() => {
                let Some(incoming) = incoming else {
                    return Ok(());
                };
                match incoming.map_err(|error| format!("Could not read AtrisHub relay WebSocket: {error}"))? {
                    Message::Text(text) => {
                        if text.len() > MAX_RELAY_PAYLOAD_BYTES {
                            return Err("AtrisHub relay request exceeded the Desktop safety bound.".into());
                        }
                        let request: RelayRequest = match serde_json::from_str(text.as_ref()) {
                            Ok(value) => value,
                            Err(_) => return Err("AtrisHub relay sent invalid request JSON.".into()),
                        };
                        let permit = match permits.clone().try_acquire_owned() {
                            Ok(value) => value,
                            Err(_) => {
                                let response = relay_error_response(
                                    &request,
                                    "device_busy",
                                    "AtrisBridge Desktop has reached its bounded remote request limit.",
                                );
                                send_json(&outgoing_tx, response).await?;
                                continue;
                            }
                        };
                        let worker_app = app.clone();
                        let worker_credential = credential.clone();
                        let worker_tx = outgoing_tx.clone();
                        let worker_request_id = request.request_id.clone();
                        let worker_reply_to = request.reply_to.clone();
                        tauri::async_runtime::spawn(async move {
                            let response = tauri::async_runtime::spawn_blocking(move || {
                                let _permit = permit;
                                process_relay_request(&worker_app, &worker_credential, request)
                            }).await;
                            let value = match response {
                                Ok(value) => value,
                                Err(error) => relay_error_response_parts(
                                    &worker_request_id,
                                    &worker_reply_to,
                                    "desktop_worker_failed",
                                    &format!("Remote MCP worker failed: {error}"),
                                ),
                            };
                            let _ = send_json(&worker_tx, value).await;
                        });
                    }
                    Message::Ping(payload) => {
                        writer.send(Message::Pong(payload)).await
                            .map_err(|error| format!("Could not answer AtrisHub relay heartbeat: {error}"))?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Ok(()),
                    Message::Binary(_) => return Err("AtrisHub relay sent an unexpected binary payload.".into()),
                }
            }
            _ = auth_tick.tick() => {
                match load_relay_credential(app.clone()).await {
                    Ok(Some(current))
                        if current.user_id == credential.user_id
                            && current.device_id == credential.device_id
                            && current.access_token == credential.access_token => {}
                    Ok(Some(_)) => {
                        let _ = writer.send(Message::Close(None)).await;
                        return Ok(());
                    }
                    Ok(None) => {
                        let _ = writer.send(Message::Close(None)).await;
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = writer.send(Message::Close(None)).await;
                        return Err(error);
                    }
                }
            }
        }
    }
}

async fn send_json(sender: &mpsc::Sender<Message>, value: Value) -> Result<(), String> {
    let encoded = serde_json::to_string(&value)
        .map_err(|error| format!("Could not encode AtrisHub relay response: {error}"))?;
    if encoded.len() > MAX_RELAY_PAYLOAD_BYTES {
        return Err("AtrisBridge remote MCP response exceeded the relay safety bound.".into());
    }
    sender
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| "AtrisHub relay writer is no longer available.".to_string())
}

fn process_relay_request(
    app: &AppHandle,
    credential: &DesktopRelayCredential,
    request: RelayRequest,
) -> Value {
    let request_id = request.request_id.clone();
    let reply_to = request.reply_to.clone();
    match validate_relay_request(credential, &request) {
        Ok(()) => {
            let response =
                dispatch_mcp_message(app, &request.client.principal, &request.mcp.message);
            relay_success_response(&request_id, &reply_to, response)
        }
        Err(error) => {
            relay_error_response_parts(&request_id, &reply_to, error.code, &error.message)
        }
    }
}

fn validate_relay_request(
    credential: &DesktopRelayCredential,
    request: &RelayRequest,
) -> Result<(), RelayFailure> {
    if request.kind != "relay_request" || request.version != 1 {
        return Err(relay_failure(
            "invalid_envelope",
            "Remote relay envelope is not supported.",
        ));
    }
    if Uuid::parse_str(&request.request_id).is_err() {
        return Err(relay_failure(
            "invalid_envelope",
            "Remote relay request identifier is invalid.",
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
    if request.client.id.is_empty()
        || request.client.id.len() > 512
        || request.client.name.len() > 160
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
    validate_mcp_request_metadata(&request.mcp).map_err(|error| {
        relay_failure(
            "invalid_mcp_request",
            &format!("{}: {}", error.message, error.detail),
        )
    })
}

fn validate_mcp_request_metadata(mcp: &RelayMcpRequest) -> Result<(), RpcFailure> {
    let message = mcp
        .message
        .as_object()
        .ok_or_else(|| rpc_invalid("MCP request must be a JSON object."))?;
    if message.get("jsonrpc") != Some(&Value::String("2.0".into())) {
        return Err(rpc_invalid("MCP request must use JSON-RPC 2.0."));
    }
    let id = message.get("id").ok_or_else(|| {
        rpc_invalid("MCP 2026 requests exposed by AtrisBridge must have a request id.")
    })?;
    if id.is_null() || !(id.is_string() || id.is_number()) {
        return Err(rpc_invalid("MCP request id must be a string or number."));
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_invalid("MCP request method is missing."))?;
    if method != mcp.method_header {
        return Err(rpc_invalid(
            "Mcp-Method does not match the JSON-RPC method.",
        ));
    }
    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| rpc_invalid("MCP request params are required."))?;
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| rpc_invalid("MCP request _meta is required."))?;
    if meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        != Some(MCP_PROTOCOL_VERSION)
    {
        return Err(rpc_invalid("MCP request _meta protocolVersion is invalid."));
    }
    if !meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .is_some_and(Value::is_object)
    {
        return Err(rpc_invalid("MCP request clientCapabilities are required."));
    }
    if let Some(client_info) = meta.get("io.modelcontextprotocol/clientInfo") {
        let client_info = client_info
            .as_object()
            .ok_or_else(|| rpc_invalid("MCP clientInfo must be an object."))?;
        if !client_info.get("name").is_some_and(Value::is_string)
            || !client_info.get("version").is_some_and(Value::is_string)
        {
            return Err(rpc_invalid("MCP clientInfo is malformed."));
        }
    }
    if method == "tools/call" {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_invalid("tools/call requires a tool name."))?;
        if mcp.name_header.as_deref() != Some(name) {
            return Err(rpc_invalid(
                "Mcp-Name does not match the AtrisBridge tool name.",
            ));
        }
    }
    Ok(())
}

fn dispatch_mcp_message(app: &AppHandle, principal: &str, message: &Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match dispatch_mcp_result(app, principal, message) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": error.code,
                "message": error.message,
                "data": {"detail": bounded_error(&error.detail)}
            }
        }),
    }
}

fn dispatch_mcp_result(
    app: &AppHandle,
    principal: &str,
    message: &Value,
) -> Result<Value, RpcFailure> {
    validate_mcp_request_metadata(&RelayMcpRequest {
        protocol_version: MCP_PROTOCOL_VERSION.into(),
        method_header: message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        name_header: message
            .get("params")
            .and_then(Value::as_object)
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        message: message.clone(),
    })?;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| rpc_invalid("MCP params are required."))?;
    match method {
        "server/discover" => Ok(discover_result()),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => dispatch_tool_call(app, principal, params),
        "tasks/get" => dispatch_task_get(app, principal, params),
        "tasks/cancel" => dispatch_task_cancel(app, principal, params),
        _ => Err(RpcFailure {
            code: -32601,
            message: "Method not found",
            detail: "This MCP method is not exposed by AtrisBridge Remote Gateway.".into(),
        }),
    }
}

fn discover_result() -> Value {
    let manifest = mcp_core::manifest();
    json!({
        "resultType": "complete",
        "ttlMs": 0,
        "cacheScope": "private",
        "supportedVersions": [MCP_PROTOCOL_VERSION],
        "capabilities": {
            "tools": {},
            "extensions": { TASKS_EXTENSION: {} }
        },
        "instructions": manifest.instructions,
        "_meta": server_meta()
    })
}

fn tools_list_result() -> Value {
    let tools = mcp_core::manifest()
        .tools
        .into_iter()
        .map(|tool| {
            let mut value = json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "annotations": {
                    "readOnlyHint": tool.annotations.read_only_hint,
                    "destructiveHint": tool.annotations.destructive_hint,
                    "idempotentHint": tool.annotations.idempotent_hint,
                    "openWorldHint": tool.annotations.open_world_hint,
                }
            });
            if let Some(execution) = tool.execution {
                value["execution"] = json!({"taskSupport": execution.task_support});
            }
            value
        })
        .collect::<Vec<_>>();
    json!({
        "resultType": "complete",
        "ttlMs": 0,
        "cacheScope": "private",
        "tools": tools,
        "_meta": server_meta()
    })
}

fn dispatch_tool_call(
    app: &AppHandle,
    principal: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcFailure> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_invalid("tools/call requires a tool name."))?;
    let contract = mcp_core::manifest()
        .tools
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| RpcFailure {
            code: -32602,
            message: "Invalid params",
            detail: "Unknown AtrisBridge MCP tool.".into(),
        })?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(rpc_invalid("tools/call arguments must be an object."));
    }
    let task_required = contract
        .execution
        .is_some_and(|execution| execution.task_support == "required");
    if task_required && !request_supports_tasks(params) {
        return Err(rpc_invalid(
            "This AtrisBridge tool requires the io.modelcontextprotocol/tasks client extension.",
        ));
    }

    match mcp_dispatch::dispatch_tool(app, principal, name, arguments) {
        Ok(value) if task_required => task_create_result(value),
        Ok(value) => Ok(call_tool_result(value, false, None)),
        Err(error) => {
            let safe = local_mcp_ipc::redact_local_paths(app, &error);
            Ok(call_tool_result(
                json!({"error": {"code": "authority_error", "message": safe}}),
                true,
                Some("AtrisBridge Desktop rejected the tool request."),
            ))
        }
    }
}

fn request_supports_tasks(params: &Map<String, Value>) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/clientCapabilities"))
        .and_then(Value::as_object)
        .and_then(|caps| caps.get("extensions"))
        .and_then(Value::as_object)
        .is_some_and(|extensions| {
            extensions
                .get(TASKS_EXTENSION)
                .is_some_and(Value::is_object)
        })
}

fn call_tool_result(structured: Value, is_error: bool, text: Option<&str>) -> Value {
    let content = text
        .map(|text| vec![json!({"type": "text", "text": bounded_error(text)})])
        .unwrap_or_default();
    json!({
        "resultType": "complete",
        "content": content,
        "structuredContent": structured,
        "isError": is_error,
        "_meta": server_meta()
    })
}

fn task_create_result(value: Value) -> Result<Value, RpcFailure> {
    let task = task_record_from_value(&value)?;
    let mut result = task_base(&task);
    result.insert("resultType".into(), Value::String("task".into()));
    result.insert("_meta".into(), server_meta());
    Ok(Value::Object(result))
}

fn dispatch_task_get(
    app: &AppHandle,
    principal: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcFailure> {
    let task_id = task_id_param(params)?;
    let snapshot =
        mcp_dispatch::task_snapshot(app, principal, task_id).map_err(authority_rpc_failure)?;
    let mut result = task_base(&snapshot.task);
    result.insert("resultType".into(), Value::String("complete".into()));
    result.insert("_meta".into(), server_meta());
    apply_task_payload(&mut result, &snapshot.task, snapshot.result.as_ref());
    Ok(Value::Object(result))
}

fn dispatch_task_cancel(
    app: &AppHandle,
    principal: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcFailure> {
    let task_id = task_id_param(params)?;
    mcp_dispatch::cancel_task(app, principal, task_id).map_err(authority_rpc_failure)?;
    Ok(json!({"resultType": "complete", "_meta": server_meta()}))
}

fn task_id_param(params: &Map<String, Value>) -> Result<&str, RpcFailure> {
    let task_id = params
        .get("taskId")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_invalid("Task request requires taskId."))?;
    if task_id.is_empty() || task_id.len() > 128 {
        return Err(rpc_invalid("Task identifier is invalid."));
    }
    Ok(task_id)
}

fn task_record_from_value(value: &Value) -> Result<AiTaskRecord, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| rpc_internal("AtrisBridge returned an invalid task record."))?;
    Ok(AiTaskRecord {
        id: string_field(object, "id")?,
        session_id: string_field(object, "sessionId")?,
        client_id: string_field(object, "clientId")?,
        workspace_id: string_field(object, "workspaceId")?,
        kind: string_field(object, "kind")?,
        profile_id: string_field(object, "profileId")?,
        status: string_field(object, "status")?,
        created_at: string_field(object, "createdAt")?,
        started_at: optional_string_field(object, "startedAt")?,
        completed_at: optional_string_field(object, "completedAt")?,
        cancel_requested: object
            .get("cancelRequested")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        result_available: object
            .get("resultAvailable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error_code: optional_string_field(object, "errorCode")?,
    })
}

fn string_field(object: &Map<String, Value>, name: &str) -> Result<String, RpcFailure> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| rpc_internal("AtrisBridge task record is malformed."))
}

fn optional_string_field(
    object: &Map<String, Value>,
    name: &str,
) -> Result<Option<String>, RpcFailure> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(rpc_internal("AtrisBridge task record is malformed.")),
    }
}

fn task_base(task: &AiTaskRecord) -> Map<String, Value> {
    let mut value = Map::new();
    value.insert("taskId".into(), Value::String(task.id.clone()));
    value.insert(
        "status".into(),
        Value::String(protocol_task_status(&task.status).into()),
    );
    value.insert("createdAt".into(), Value::String(task.created_at.clone()));
    value.insert("lastUpdatedAt".into(), Value::String(task_updated_at(task)));
    value.insert("ttlMs".into(), json!(TASK_RESULT_TTL_MS));
    value.insert("pollIntervalMs".into(), json!(TASK_POLL_INTERVAL_MS));
    if let Some(message) = task_status_message(task) {
        value.insert("statusMessage".into(), Value::String(message));
    }
    value
}

fn apply_task_payload(
    result: &mut Map<String, Value>,
    task: &AiTaskRecord,
    task_result: Option<&AiTaskResult>,
) {
    match task.status.as_str() {
        "completed" => {
            if let Some(command) = task_result.and_then(|value| value.command.clone()) {
                let success = command
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                result.insert("result".into(), call_tool_result(command, !success, None));
            } else {
                result.insert("status".into(), Value::String("failed".into()));
                result.insert(
                    "error".into(),
                    task_error(
                        "result_unavailable",
                        task_result
                            .and_then(|value| value.error.as_deref())
                            .unwrap_or("AtrisBridge task result is no longer available."),
                    ),
                );
            }
        }
        "failed" | "interrupted" => {
            let message = task_result
                .and_then(|value| value.error.as_deref())
                .or_else(|| {
                    (task.status == "interrupted")
                        .then_some("AtrisBridge Desktop restarted while this task was running.")
                })
                .unwrap_or("AtrisBridge command task failed.");
            result.insert(
                "error".into(),
                task_error(
                    task.error_code.as_deref().unwrap_or("command_failed"),
                    message,
                ),
            );
        }
        _ => {}
    }
}

fn task_error(code: &str, message: &str) -> Value {
    json!({
        "code": -32603,
        "message": bounded_error(message),
        "data": {"atrisBridgeCode": code}
    })
}

fn protocol_task_status(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "failed" | "interrupted" => "failed",
        "cancelled" => "cancelled",
        _ => "working",
    }
}

fn task_updated_at(task: &AiTaskRecord) -> String {
    task.completed_at
        .clone()
        .or_else(|| task.started_at.clone())
        .unwrap_or_else(|| task.created_at.clone())
}

fn task_status_message(task: &AiTaskRecord) -> Option<String> {
    match task.status.as_str() {
        "queued" => Some("Waiting for AtrisBridge workspace authority.".into()),
        "running" => Some("AtrisBridge command profile is running.".into()),
        "interrupted" => Some("AtrisBridge Desktop restarted before completion.".into()),
        "completed" if !task.result_available => {
            Some("Task completed, but its retained result is no longer available.".into())
        }
        _ => None,
    }
}

fn server_meta() -> Value {
    json!({
        "io.modelcontextprotocol/serverInfo": {
            "name": "AtrisBridge",
            "title": "AtrisBridge AI Workspace Gateway",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Secure local-first workspace authority for AI coding clients."
        }
    })
}

fn remote_principal(client_id: &str) -> String {
    let digest = Sha256::digest(client_id.as_bytes());
    let mut prefix = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(prefix, "{byte:02x}");
    }
    format!("mcp.remote.{prefix}")
}

fn relay_success_response(request_id: &str, reply_to: &str, result: Value) -> Value {
    json!({
        "type": "relay_response",
        "version": 1,
        "requestId": request_id,
        "replyTo": reply_to,
        "ok": true,
        "result": result
    })
}

fn relay_error_response(request: &RelayRequest, code: &'static str, message: &str) -> Value {
    relay_error_response_parts(&request.request_id, &request.reply_to, code, message)
}

fn relay_error_response_parts(
    request_id: &str,
    reply_to: &str,
    code: &'static str,
    message: &str,
) -> Value {
    json!({
        "type": "relay_response",
        "version": 1,
        "requestId": request_id,
        "replyTo": reply_to,
        "ok": false,
        "error": {"code": code, "message": bounded_error(message)}
    })
}

fn relay_failure(code: &'static str, message: &str) -> RelayFailure {
    RelayFailure {
        code,
        message: bounded_error(message),
    }
}

fn rpc_invalid(detail: &str) -> RpcFailure {
    RpcFailure {
        code: -32602,
        message: "Invalid params",
        detail: detail.into(),
    }
}

fn rpc_internal(detail: &str) -> RpcFailure {
    RpcFailure {
        code: -32603,
        message: "Internal error",
        detail: detail.into(),
    }
}

fn authority_rpc_failure(error: String) -> RpcFailure {
    RpcFailure {
        code: -32602,
        message: "Invalid params",
        detail: bounded_error(&error),
    }
}

fn safe_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn bounded_error(message: &str) -> String {
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

fn backoff_delay(seconds: u64) -> Duration {
    let bounded = seconds.min(MAX_RECONNECT_DELAY.as_secs());
    let jitter_ms = (chrono::Utc::now().timestamp_subsec_millis() as u64) % 500;
    Duration::from_millis(bounded * 1_000 + jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_principal_matches_gateway_sha256_contract() {
        assert_eq!(
            remote_principal("client-123"),
            "mcp.remote.b44ea687b506d5ca725c434cbe69d0cd"
        );
    }

    #[test]
    fn discovery_is_modern_stateless_and_task_extension_aware() {
        let result = discover_result();
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["ttlMs"], 0);
        assert_eq!(result["cacheScope"], "private");
        assert_eq!(result["supportedVersions"][0], MCP_PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["extensions"][TASKS_EXTENSION].is_object());
        assert_eq!(
            result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "AtrisBridge"
        );
    }

    #[test]
    fn remote_tools_list_matches_authority_catalog() {
        let result = tools_list_result();
        let actual = result["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        let expected = mcp_core::manifest()
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn protocol_task_status_maps_durable_states() {
        assert_eq!(protocol_task_status("queued"), "working");
        assert_eq!(protocol_task_status("running"), "working");
        assert_eq!(protocol_task_status("completed"), "completed");
        assert_eq!(protocol_task_status("failed"), "failed");
        assert_eq!(protocol_task_status("interrupted"), "failed");
        assert_eq!(protocol_task_status("cancelled"), "cancelled");
    }

    #[test]
    fn relay_identity_fields_are_strictly_bounded() {
        assert!(safe_instance_id("gateway-1:abc.def"));
        assert!(!safe_instance_id(""));
        assert!(!safe_instance_id("gateway/unsafe"));
        assert!(!safe_instance_id(&"x".repeat(161)));
    }
}
