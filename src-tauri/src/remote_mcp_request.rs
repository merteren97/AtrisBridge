use serde_json::Value;
use tauri::AppHandle;

use crate::{
    atris_auth::DesktopSessionCredential,
    remote_mcp_adapter,
    remote_mcp_protocol::{self, RelayRequest, ResponseBinding},
    remote_mcp_relay,
};

pub(crate) fn process(
    app: &AppHandle,
    credential: &DesktopSessionCredential,
    request: RelayRequest,
) -> Value {
    let binding = ResponseBinding::from_request(&request);
    match remote_mcp_protocol::validate_relay_request(credential, &request) {
        Ok(()) => {
            remote_mcp_relay::remember_remote_client(
                app,
                &request.client.principal,
                &request.client.name,
            );
            let response =
                remote_mcp_adapter::dispatch(app, &request.client.principal, &request.mcp.message);
            remote_mcp_protocol::relay_success_response(&binding, response)
        }
        Err(error) => {
            remote_mcp_protocol::relay_error_response_parts(&binding, error.code, &error.message)
        }
    }
}
