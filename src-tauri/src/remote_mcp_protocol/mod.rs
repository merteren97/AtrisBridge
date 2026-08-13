mod validation;

pub(crate) use validation::*;

use serde_json::Value;
use tauri::AppHandle;

use crate::{atris_auth::DesktopSessionCredential, remote_mcp_request};

pub(crate) fn process_relay_request(
    app: &AppHandle,
    credential: &DesktopSessionCredential,
    request: RelayRequest,
) -> Value {
    remote_mcp_request::process(app, credential, request)
}
