use tauri::AppHandle;

use crate::{
    local_mcp_clients::{self, LocalMcpClientKind, LocalMcpClientStatus},
    local_mcp_probe::{self, LocalMcpConnectionProbe},
};

#[tauri::command]
pub async fn list_local_mcp_clients(app: AppHandle) -> Result<Vec<LocalMcpClientStatus>, String> {
    run_blocking(move || local_mcp_clients::list_local_mcp_clients(app)).await
}

#[tauri::command]
pub async fn register_local_mcp_client(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpClientStatus, String> {
    run_blocking(move || local_mcp_probe::register_and_test_local_mcp_client(app, kind)).await
}

#[tauri::command]
pub async fn unregister_local_mcp_client(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpClientStatus, String> {
    run_blocking(move || local_mcp_clients::unregister_local_mcp_client(app, kind)).await
}

#[tauri::command]
pub async fn test_local_mcp_client_connection(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpConnectionProbe, String> {
    run_blocking(move || local_mcp_probe::test_local_mcp_client_connection(app, kind)).await
}

async fn run_blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| format!("Local MCP client worker failed: {error}"))?
}
