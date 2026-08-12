use std::{
    borrow::Cow,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

use keyring::{Entry, Error as KeyringError};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams,
        ClientCapabilities, CreateTaskResult, DetailedTask, GetTaskParams, GetTaskResult,
        JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
        ServerInfo, Task, TaskPayload, TaskStatus, Tool, ToolAnnotations, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

const SERVICE_NAME: &str = "com.atrishub.atrisbridge";
const LOCAL_MCP_ENDPOINT_ACCOUNT: &str = "mcp.local.endpoint.v1";
const IPC_VERSION: u32 = 1;
const MAX_IPC_FRAME_BYTES: usize = 24 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const TASK_RESULT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const TASK_POLL_INTERVAL_MS: u64 = 750;
const MCP_PROTOCOL_VERSION: &str = "2026-07-28";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    protocol_version: String,
    instructions: String,
    tools: Vec<ToolContract>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolContract {
    name: String,
    title: String,
    description: String,
    input_schema: Value,
    annotations: ToolContractAnnotations,
    execution: Option<ToolExecution>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolContractAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecution {
    task_support: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointCredential {
    version: u32,
    address: String,
    token: String,
    instance_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IpcRequest {
    version: u32,
    request_id: String,
    token: String,
    client_kind: String,
    method: String,
    params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpcResponse {
    version: u32,
    request_id: String,
    ok: bool,
    result: Option<Value>,
    error: Option<IpcError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpcError {
    code: String,
    message: String,
}

#[derive(Debug, Clone)]
struct IpcFailure {
    code: String,
    message: String,
}

#[derive(Debug, Clone)]
struct IpcClient {
    client_kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskSnapshot {
    task: TaskRecord,
    result: Option<TaskResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskRecord {
    id: String,
    status: String,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    result_available: bool,
    error_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskResult {
    command: Option<Value>,
    error: Option<String>,
}

#[derive(Clone)]
struct AtrisBridgeMcp {
    ipc: IpcClient,
    manifest: Arc<Manifest>,
}

impl IpcClient {
    fn new(client_kind: String) -> Self {
        Self { client_kind }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, IpcFailure> {
        let client = self.clone();
        let method = method.to_string();
        tokio::task::spawn_blocking(move || client.request_blocking(&method, params))
            .await
            .map_err(|error| IpcFailure {
                code: "ipc_worker_failed".into(),
                message: format!("AtrisBridge local IPC worker failed: {error}"),
            })?
    }

    fn request_blocking(&self, method: &str, params: Value) -> Result<Value, IpcFailure> {
        let endpoint = load_endpoint()?;
        let address: SocketAddr = endpoint.address.parse().map_err(|_| IpcFailure {
            code: "invalid_endpoint".into(),
            message: "AtrisBridge Desktop published an invalid local MCP endpoint.".into(),
        })?;
        if !address.ip().is_loopback() {
            return Err(IpcFailure {
                code: "unsafe_endpoint".into(),
                message: "AtrisBridge refused a non-loopback local MCP endpoint.".into(),
            });
        }
        let mut stream =
            TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|_| IpcFailure {
                code: "desktop_unavailable".into(),
                message: "AtrisBridge Desktop is not available. Start AtrisBridge and try again."
                    .into(),
            })?;
        stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .map_err(transport_failure)?;
        stream
            .set_write_timeout(Some(SOCKET_TIMEOUT))
            .map_err(transport_failure)?;

        let request_id = Uuid::new_v4().to_string();
        write_frame(
            &mut stream,
            &IpcRequest {
                version: IPC_VERSION,
                request_id: request_id.clone(),
                token: endpoint.token,
                client_kind: self.client_kind.clone(),
                method: method.to_string(),
                params,
            },
        )?;
        let response: IpcResponse = read_frame(&mut stream)?;
        if response.version != IPC_VERSION || response.request_id != request_id {
            return Err(IpcFailure {
                code: "invalid_response".into(),
                message: "AtrisBridge Desktop returned an invalid local MCP response.".into(),
            });
        }
        if response.ok {
            response.result.ok_or_else(|| IpcFailure {
                code: "invalid_response".into(),
                message: "AtrisBridge Desktop returned an empty local MCP response.".into(),
            })
        } else {
            let error = response.error.unwrap_or(IpcError {
                code: "authority_error".into(),
                message: "AtrisBridge Desktop rejected the request.".into(),
            });
            Err(IpcFailure {
                code: error.code,
                message: error.message,
            })
        }
    }
}

impl ServerHandler for AtrisBridgeMcp {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
        .with_instructions(self.manifest.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .manifest
            .tools
            .iter()
            .map(tool_from_contract)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let contract = self
            .manifest
            .tools
            .iter()
            .find(|tool| tool.name == request.name)
            .ok_or_else(|| McpError::invalid_params("Unknown AtrisBridge MCP tool.", None))?;
        let requires_task = contract
            .execution
            .as_ref()
            .is_some_and(|execution| execution.task_support == "required");
        if requires_task
            && !context
                .client_capabilities()
                .is_some_and(|capabilities| capabilities.supports_tasks())
        {
            return Err(McpError::missing_required_client_capability(
                ClientCapabilities::builder().enable_tasks().build(),
            ));
        }

        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let result = self
            .ipc
            .request(
                "tool.call",
                json!({
                    "name": request.name.as_ref(),
                    "arguments": arguments,
                }),
            )
            .await;

        match result {
            Ok(value) if requires_task => {
                let task: TaskRecord = serde_json::from_value(value).map_err(|error| {
                    McpError::internal_error(
                        format!("AtrisBridge returned an invalid task record: {error}"),
                        None,
                    )
                })?;
                Ok(CallToolResponse::Task(CreateTaskResult::new(task_seed(
                    &task,
                ))))
            }
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error) => Ok(tool_error(error).into()),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let value = self
            .ipc
            .request("task.get", json!({"taskId": request.task_id}))
            .await
            .map_err(protocol_failure)?;
        let snapshot: TaskSnapshot = serde_json::from_value(value).map_err(|error| {
            McpError::internal_error(
                format!("AtrisBridge returned an invalid task snapshot: {error}"),
                None,
            )
        })?;
        Ok(GetTaskResult::new(detailed_task(snapshot)?))
    }

    async fn update_task(
        &self,
        _request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Err(McpError::invalid_params(
            "AtrisBridge command tasks never request client input.",
            None,
        ))
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.ipc
            .request("task.cancel", json!({"taskId": request.task_id}))
            .await
            .map_err(protocol_failure)?;
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("AtrisBridge MCP companion failed: {error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<(), String> {
    let mode = parse_args()?;
    if mode.printed_and_exit {
        return Ok(());
    }
    let ipc = IpcClient::new(mode.client_kind);
    ipc.request("health", json!({}))
        .await
        .map_err(|error| error.message)?;
    let manifest_value = ipc
        .request("manifest", json!({}))
        .await
        .map_err(|error| error.message)?;
    let manifest: Manifest = serde_json::from_value(manifest_value).map_err(|error| {
        format!("AtrisBridge Desktop returned an invalid MCP manifest: {error}")
    })?;
    if manifest.protocol_version != MCP_PROTOCOL_VERSION {
        return Err(format!(
            "AtrisBridge Desktop MCP protocol {} is incompatible with companion protocol {}.",
            manifest.protocol_version, MCP_PROTOCOL_VERSION
        ));
    }
    for contract in &manifest.tools {
        tool_from_contract(contract).map_err(|error| error.to_string())?;
    }

    let service = AtrisBridgeMcp {
        ipc,
        manifest: Arc::new(manifest),
    }
    .serve(stdio())
    .await
    .map_err(|error| format!("Could not start MCP stdio transport: {error}"))?;
    service
        .waiting()
        .await
        .map_err(|error| format!("MCP stdio transport ended unexpectedly: {error}"))?;
    Ok(())
}

struct ParsedArgs {
    client_kind: String,
    printed_and_exit: bool,
}

fn parse_args() -> Result<ParsedArgs, String> {
    let mut client_kind = "generic".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--client" => {
                client_kind = args
                    .next()
                    .ok_or_else(|| "--client requires codex, claude, or generic.".to_string())?;
            }
            "--version" | "-V" => {
                println!("atrisbridge-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(ParsedArgs {
                    client_kind,
                    printed_and_exit: true,
                });
            }
            "--help" | "-h" => {
                println!(
                    "AtrisBridge MCP companion\n\nUsage: atrisbridge-mcp [--client codex|claude|generic]\n"
                );
                return Ok(ParsedArgs {
                    client_kind,
                    printed_and_exit: true,
                });
            }
            _ => return Err(format!("Unknown AtrisBridge MCP argument '{argument}'.")),
        }
    }
    if !matches!(client_kind.as_str(), "codex" | "claude" | "generic") {
        return Err("--client must be codex, claude, or generic.".into());
    }
    Ok(ParsedArgs {
        client_kind,
        printed_and_exit: false,
    })
}

fn load_endpoint() -> Result<EndpointCredential, IpcFailure> {
    let entry = Entry::new(SERVICE_NAME, LOCAL_MCP_ENDPOINT_ACCOUNT).map_err(vault_failure)?;
    let encoded = match entry.get_password() {
        Ok(value) => value,
        Err(KeyringError::NoEntry) => return Err(IpcFailure {
            code: "desktop_unavailable".into(),
            message:
                "AtrisBridge Desktop is not running or has not published its local MCP endpoint."
                    .into(),
        }),
        Err(error) => return Err(vault_failure(error)),
    };
    let endpoint: EndpointCredential = serde_json::from_str(&encoded).map_err(|_| IpcFailure {
        code: "invalid_endpoint".into(),
        message: "AtrisBridge Desktop published an invalid local MCP endpoint credential.".into(),
    })?;
    if endpoint.version != IPC_VERSION
        || endpoint.instance_id.trim().is_empty()
        || endpoint.token.len() != 64
        || !endpoint.token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(IpcFailure {
            code: "invalid_endpoint".into(),
            message: "AtrisBridge Desktop published an invalid local MCP endpoint credential."
                .into(),
        });
    }
    Ok(endpoint)
}

fn tool_from_contract(contract: &ToolContract) -> Result<Tool, McpError> {
    let schema = contract.input_schema.as_object().cloned().ok_or_else(|| {
        McpError::internal_error("AtrisBridge tool schema is not an object.", None)
    })?;
    let annotations = ToolAnnotations::new()
        .read_only(contract.annotations.read_only_hint)
        .destructive(contract.annotations.destructive_hint)
        .idempotent(contract.annotations.idempotent_hint)
        .open_world(contract.annotations.open_world_hint);
    Ok(
        Tool::new(contract.name.clone(), contract.description.clone(), schema)
            .with_title(contract.title.clone())
            .with_annotations(annotations),
    )
}

fn task_seed(record: &TaskRecord) -> Task {
    let mut task = Task::new(
        record.id.clone(),
        protocol_task_status(&record.status),
        record.created_at.clone(),
        task_updated_at(record),
    )
    .with_ttl_ms(TASK_RESULT_TTL_MS)
    .with_poll_interval_ms(TASK_POLL_INTERVAL_MS);
    task.status_message = task_status_message(record);
    task
}

fn detailed_task(snapshot: TaskSnapshot) -> Result<DetailedTask, McpError> {
    let task = task_seed(&snapshot.task);
    let payload = match snapshot.task.status.as_str() {
        "queued" | "running" => TaskPayload::Working,
        "cancelled" => TaskPayload::Cancelled,
        "completed" => completed_payload(&snapshot)?,
        "failed" | "interrupted" => failed_payload(&snapshot),
        _ => {
            return Err(McpError::internal_error(
                "AtrisBridge task has an invalid lifecycle state.",
                None,
            ))
        }
    };
    Ok(DetailedTask::new(task, payload))
}

fn completed_payload(snapshot: &TaskSnapshot) -> Result<TaskPayload, McpError> {
    let Some(result) = snapshot.result.as_ref() else {
        return Ok(task_failure_payload(
            "result_unavailable",
            "AtrisBridge task result is no longer available.",
        ));
    };
    let Some(command) = result.command.clone() else {
        return Ok(task_failure_payload(
            "result_unavailable",
            result
                .error
                .as_deref()
                .unwrap_or("AtrisBridge task result is no longer available."),
        ));
    };
    let successful = command
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tool_result = if successful {
        CallToolResult::structured(command)
    } else {
        CallToolResult::structured_error(command)
    };
    let value = serde_json::to_value(tool_result).map_err(|error| {
        McpError::internal_error(
            format!("Could not encode AtrisBridge task tool result: {error}"),
            None,
        )
    })?;
    Ok(TaskPayload::Completed {
        result: into_json_object(value)?,
    })
}

fn failed_payload(snapshot: &TaskSnapshot) -> TaskPayload {
    let message = snapshot
        .result
        .as_ref()
        .and_then(|result| result.error.as_deref())
        .or_else(|| {
            if snapshot.task.status == "interrupted" {
                Some("AtrisBridge Desktop restarted while this task was running.")
            } else {
                None
            }
        })
        .unwrap_or("AtrisBridge command task failed.");
    task_failure_payload(
        snapshot
            .task
            .error_code
            .as_deref()
            .unwrap_or("command_failed"),
        message,
    )
}

fn task_failure_payload(code: &str, message: &str) -> TaskPayload {
    TaskPayload::Failed {
        error: json_object(json!({
            "code": -32603,
            "message": message,
            "data": {"atrisBridgeCode": code}
        })),
    }
}

fn task_updated_at(record: &TaskRecord) -> String {
    record
        .completed_at
        .clone()
        .or_else(|| record.started_at.clone())
        .unwrap_or_else(|| record.created_at.clone())
}

fn task_status_message(record: &TaskRecord) -> Option<String> {
    match record.status.as_str() {
        "queued" => Some("Waiting for AtrisBridge workspace authority.".into()),
        "running" => Some("AtrisBridge command profile is running.".into()),
        "interrupted" => Some("AtrisBridge Desktop restarted before completion.".into()),
        "completed" if !record.result_available => {
            Some("Task completed, but its retained result is no longer available.".into())
        }
        _ => None,
    }
}

fn protocol_task_status(status: &str) -> TaskStatus {
    match status {
        "completed" => TaskStatus::Completed,
        "failed" | "interrupted" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Working,
    }
}

fn tool_error(error: IpcFailure) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": error.code,
            "message": error.message,
        }
    }))
}

fn protocol_failure(error: IpcFailure) -> McpError {
    McpError::invalid_params(error.message, Some(json!({"atrisBridgeCode": error.code})))
}

fn into_json_object(value: Value) -> Result<JsonObject, McpError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| McpError::internal_error("AtrisBridge MCP result was not an object.", None))
}

fn json_object(value: Value) -> JsonObject {
    value
        .as_object()
        .cloned()
        .expect("static AtrisBridge task error is an object")
}

fn write_frame<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<(), IpcFailure> {
    let payload = serde_json::to_vec(value).map_err(|error| IpcFailure {
        code: "encode_failed".into(),
        message: format!("Could not encode AtrisBridge local IPC request: {error}"),
    })?;
    if payload.is_empty() || payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcFailure {
            code: "frame_too_large".into(),
            message: "AtrisBridge local IPC request exceeded the safety bound.".into(),
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| IpcFailure {
        code: "frame_too_large".into(),
        message: "AtrisBridge local IPC request exceeded the safety bound.".into(),
    })?;
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|_| stream.write_all(&payload))
        .and_then(|_| stream.flush())
        .map_err(transport_failure)
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut TcpStream) -> Result<T, IpcFailure> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).map_err(transport_failure)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_IPC_FRAME_BYTES {
        return Err(IpcFailure {
            code: "invalid_frame".into(),
            message: "AtrisBridge Desktop returned an invalid local IPC frame.".into(),
        });
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).map_err(transport_failure)?;
    serde_json::from_slice(&payload).map_err(|_| IpcFailure {
        code: "invalid_response".into(),
        message: "AtrisBridge Desktop returned an invalid local MCP response.".into(),
    })
}

fn transport_failure(error: std::io::Error) -> IpcFailure {
    IpcFailure {
        code: "desktop_unavailable".into(),
        message: format!("AtrisBridge Desktop local MCP transport failed: {error}"),
    }
}

fn vault_failure(error: KeyringError) -> IpcFailure {
    IpcFailure {
        code: "vault_unavailable".into(),
        message: format!(
            "Could not read AtrisBridge local MCP endpoint from the OS vault: {error}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_client_kind_is_explicitly_bounded() {
        let original = vec![
            "binary".to_string(),
            "--client".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(original[2], "codex");
        assert!(matches!("codex", "codex" | "claude" | "generic"));
        assert!(!matches!("mcp.codex", "codex" | "claude" | "generic"));
    }

    #[test]
    fn internal_task_states_map_to_mcp_task_states() {
        assert_eq!(protocol_task_status("queued"), TaskStatus::Working);
        assert_eq!(protocol_task_status("running"), TaskStatus::Working);
        assert_eq!(protocol_task_status("completed"), TaskStatus::Completed);
        assert_eq!(protocol_task_status("failed"), TaskStatus::Failed);
        assert_eq!(protocol_task_status("interrupted"), TaskStatus::Failed);
        assert_eq!(protocol_task_status("cancelled"), TaskStatus::Cancelled);
    }
}
