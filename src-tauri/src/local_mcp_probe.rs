use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::local_mcp_clients::{self, LocalMcpClientKind, LocalMcpClientStatus};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_PROBE_ERROR_BYTES: usize = 16 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMcpConnectionProbe {
    kind: LocalMcpClientKind,
    principal: &'static str,
    registration_healthy: bool,
    authority_reachable: bool,
    protocol_version: &'static str,
    detail: String,
}

pub fn register_and_test_local_mcp_client(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpClientStatus, String> {
    let status = local_mcp_clients::register_local_mcp_client(app.clone(), kind)?;
    run_authority_probe(&app, kind, &status)?;
    Ok(status)
}

pub fn test_local_mcp_client_connection(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpConnectionProbe, String> {
    let statuses = local_mcp_clients::list_local_mcp_clients(app.clone())?;
    let status = statuses
        .into_iter()
        .find(|status| status.kind == kind)
        .ok_or_else(|| "Local MCP client status was not available.".to_string())?;
    run_authority_probe(&app, kind, &status)
}

fn run_authority_probe(
    app: &AppHandle,
    kind: LocalMcpClientKind,
    status: &LocalMcpClientStatus,
) -> Result<LocalMcpConnectionProbe, String> {
    if !status.registration_healthy {
        return Err(format!(
            "{} is not pointing to the current AtrisBridge-managed MCP companion. Repair the registration before testing the authority connection.",
            status.label
        ));
    }

    let companion = managed_companion_path(app)?;
    let metadata = fs::symlink_metadata(&companion)
        .map_err(|error| format!("Could not inspect the managed MCP companion: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("AtrisBridge refused to execute an invalid managed MCP companion.".into());
    }

    let runtime = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve MCP probe runtime directory: {error}"))?
        .join("mcp")
        .join("runtime");
    fs::create_dir_all(&runtime)
        .map_err(|error| format!("Could not create MCP probe runtime directory: {error}"))?;
    set_owner_only_directory(&runtime)?;

    let id = Uuid::new_v4();
    let stderr_path = runtime.join(format!("probe-{id}.stderr"));
    let stderr = File::create(&stderr_path)
        .map_err(|error| format!("Could not prepare MCP probe diagnostics: {error}"))?;

    let mut command = Command::new(&companion);
    command
        .arg("--client")
        .arg(client_arg(kind))
        .current_dir(&runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    configure_no_window(&mut command);

    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start the AtrisBridge MCP companion probe: {error}"))?;
    let started = Instant::now();
    let status_code = loop {
        if let Some(exit) = child
            .try_wait()
            .map_err(|error| format!("Could not observe the MCP companion probe: {error}"))?
        {
            break exit;
        }
        if started.elapsed() >= PROBE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let detail = read_probe_error(&stderr_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(if detail.is_empty() {
                "AtrisBridge MCP companion did not complete its Desktop authority handshake within the safety timeout.".into()
            } else {
                format!("AtrisBridge MCP companion handshake timed out: {detail}")
            });
        }
        thread::sleep(PROBE_POLL_INTERVAL);
    };

    let detail = read_probe_error(&stderr_path);
    let _ = fs::remove_file(&stderr_path);
    if !status_code.success() {
        return Err(if detail.is_empty() {
            "AtrisBridge MCP companion could not complete the owner-authenticated Desktop authority handshake.".into()
        } else {
            format!("AtrisBridge MCP companion authority handshake failed: {detail}")
        });
    }

    Ok(LocalMcpConnectionProbe {
        kind,
        principal: principal(kind),
        registration_healthy: true,
        authority_reachable: true,
        protocol_version: crate::mcp_core::MCP_PROTOCOL_VERSION,
        detail: "The managed companion loaded the OS-vault endpoint, reached AtrisBridge Desktop over loopback IPC, and validated the current MCP manifest before stdio shutdown.".into(),
    })
}

fn managed_companion_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| {
            format!("Could not resolve AtrisBridge application data directory: {error}")
        })?
        .join("mcp")
        .join("bin")
        .join(env!("CARGO_PKG_VERSION"))
        .join(companion_file_name()))
}

fn companion_file_name() -> &'static str {
    #[cfg(windows)]
    {
        "atrisbridge-mcp.exe"
    }
    #[cfg(not(windows))]
    {
        "atrisbridge-mcp"
    }
}

fn client_arg(kind: LocalMcpClientKind) -> &'static str {
    match kind {
        LocalMcpClientKind::Codex => "codex",
        LocalMcpClientKind::Claude => "claude",
    }
}

fn principal(kind: LocalMcpClientKind) -> &'static str {
    match kind {
        LocalMcpClientKind::Codex => "mcp.codex",
        LocalMcpClientKind::Claude => "mcp.claude",
    }
}

fn read_probe_error(path: &Path) -> String {
    let Ok(bytes) = fs::read(path) else {
        return String::new();
    };
    let bounded = &bytes[..bytes.len().min(MAX_PROBE_ERROR_BYTES)];
    String::from_utf8_lossy(bounded)
        .trim()
        .chars()
        .take(800)
        .collect()
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("Could not protect MCP probe directory permissions: {error}"))
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_identity_is_fixed_per_supported_client() {
        assert_eq!(client_arg(LocalMcpClientKind::Codex), "codex");
        assert_eq!(principal(LocalMcpClientKind::Codex), "mcp.codex");
        assert_eq!(client_arg(LocalMcpClientKind::Claude), "claude");
        assert_eq!(principal(LocalMcpClientKind::Claude), "mcp.claude");
    }
}
