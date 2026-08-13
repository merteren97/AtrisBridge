use serde_json::{json, Map, Value};
use tauri::AppHandle;

use crate::{
    ai_task::{AiTaskRecord, AiTaskResult},
    local_mcp_ipc, mcp_core, mcp_dispatch,
    remote_mcp_protocol::MCP_PROTOCOL_VERSION,
};

const TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";
const TASK_RESULT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const TASK_POLL_INTERVAL_MS: u64 = 750;
const MAX_SAFE_ERROR_CHARS: usize = 2_048;

#[derive(Debug)]
struct RpcFailure {
    code: i64,
    message: &'static str,
    detail: String,
}

pub(crate) fn dispatch(app: &AppHandle, principal: &str, message: &Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match dispatch_result(app, principal, message) {
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

fn dispatch_result(app: &AppHandle, principal: &str, message: &Value) -> Result<Value, RpcFailure> {
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
    let snapshot = mcp_dispatch::task_snapshot(app, principal, task_id)
        .map_err(|error| authority_failure(app, error))?;
    let mut result = task_base(&snapshot.task);
    result.insert("resultType".into(), Value::String("complete".into()));
    result.insert("_meta".into(), server_meta());
    apply_task_payload(app, &mut result, &snapshot.task, snapshot.result.as_ref());
    Ok(Value::Object(result))
}

fn dispatch_task_cancel(
    app: &AppHandle,
    principal: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcFailure> {
    let task_id = task_id_param(params)?;
    mcp_dispatch::cancel_task(app, principal, task_id)
        .map_err(|error| authority_failure(app, error))?;
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
    app: &AppHandle,
    result: &mut Map<String, Value>,
    task: &AiTaskRecord,
    task_result: Option<&AiTaskResult>,
) {
    match task.status.as_str() {
        "completed" => {
            if let Some(mut command) = task_result.and_then(|value| value.command.clone()) {
                redact_command_value(app, &mut command);
                let success = command
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                result.insert("result".into(), call_tool_result(command, !success, None));
            } else {
                result.insert("status".into(), Value::String("failed".into()));
                let message = task_result
                    .and_then(|value| value.error.as_deref())
                    .unwrap_or("AtrisBridge task result is no longer available.");
                let safe = local_mcp_ipc::redact_local_paths(app, message);
                result.insert("error".into(), task_error("result_unavailable", &safe));
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
            let safe = local_mcp_ipc::redact_local_paths(app, message);
            result.insert(
                "error".into(),
                task_error(
                    task.error_code.as_deref().unwrap_or("command_failed"),
                    &safe,
                ),
            );
        }
        _ => {}
    }
}

fn redact_command_value(app: &AppHandle, value: &mut Value) {
    match value {
        Value::String(text) => *text = local_mcp_ipc::redact_local_paths(app, text),
        Value::Array(values) => {
            for value in values {
                redact_command_value(app, value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_command_value(app, value);
            }
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

fn authority_failure(app: &AppHandle, error: String) -> RpcFailure {
    RpcFailure {
        code: -32602,
        message: "Invalid params",
        detail: bounded_error(&local_mcp_ipc::redact_local_paths(app, &error)),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_stateless_and_task_extension_aware() {
        let result = discover_result();
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["ttlMs"], 0);
        assert_eq!(result["cacheScope"], "private");
        assert_eq!(result["supportedVersions"][0], MCP_PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["extensions"][TASKS_EXTENSION].is_object());
    }

    #[test]
    fn tools_list_matches_authority_catalog() {
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
    fn durable_task_states_map_to_mcp_states() {
        assert_eq!(protocol_task_status("queued"), "working");
        assert_eq!(protocol_task_status("running"), "working");
        assert_eq!(protocol_task_status("completed"), "completed");
        assert_eq!(protocol_task_status("failed"), "failed");
        assert_eq!(protocol_task_status("interrupted"), "failed");
        assert_eq!(protocol_task_status("cancelled"), "cancelled");
    }
}
