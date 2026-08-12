use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::services;

const SERVER_NAME: &str = "atrisbridge";
const MAX_CLI_OUTPUT_BYTES: usize = 256 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const VERSION_TIMEOUT: Duration = Duration::from_secs(6);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalMcpClientKind {
    Codex,
    Claude,
}

impl LocalMcpClientKind {
    fn executable_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    fn principal(self) -> &'static str {
        match self {
            Self::Codex => "mcp.codex",
            Self::Claude => "mcp.claude",
        }
    }

    fn companion_arg(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMcpClientStatus {
    pub kind: LocalMcpClientKind,
    pub label: &'static str,
    pub principal: &'static str,
    pub executable_detected: bool,
    pub version: Option<String>,
    pub registration_state: &'static str,
    pub registration_healthy: bool,
    pub managed_companion_ready: bool,
    pub managed_companion_version: String,
    pub can_register: bool,
    pub can_remove: bool,
    pub detail: String,
}

#[derive(Debug)]
struct ManagedCompanion {
    path: PathBuf,
    root: PathBuf,
}

#[derive(Debug)]
struct CliOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Debug)]
enum RegistrationProbe {
    Absent,
    Exact,
    OwnedStale,
    Conflict(String),
    Error(String),
}

pub fn list_local_mcp_clients(app: AppHandle) -> Result<Vec<LocalMcpClientStatus>, String> {
    let managed = ensure_managed_companion(&app);
    Ok([LocalMcpClientKind::Codex, LocalMcpClientKind::Claude]
        .into_iter()
        .map(|kind| client_status(&app, kind, managed.as_ref()))
        .collect())
}

pub fn register_local_mcp_client(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpClientStatus, String> {
    let managed = ensure_managed_companion(&app)?;
    let executable = resolve_client_executable(&app, kind)?.ok_or_else(|| {
        format!(
            "{} was not found in a trusted PATH location. Install {} first, then retry.",
            kind.label(),
            kind.label()
        )
    })?;

    match probe_registration(&app, kind, &executable, &managed) {
        RegistrationProbe::Exact => return Ok(client_status(&app, kind, Ok(&managed))),
        RegistrationProbe::OwnedStale => remove_registration(&app, kind, &executable)?,
        RegistrationProbe::Absent => {}
        RegistrationProbe::Conflict(detail) => {
            return Err(format!(
                "A different MCP server named '{SERVER_NAME}' is already configured in {}. AtrisBridge refused to overwrite it. {detail}",
                kind.label()
            ))
        }
        RegistrationProbe::Error(detail) => {
            return Err(format!(
                "Could not safely inspect the existing {} MCP registration: {detail}",
                kind.label()
            ))
        }
    }

    add_registration(&app, kind, &executable, &managed.path)?;
    match probe_registration(&app, kind, &executable, &managed) {
        RegistrationProbe::Exact => Ok(client_status(&app, kind, Ok(&managed))),
        RegistrationProbe::Conflict(detail)
        | RegistrationProbe::Error(detail) => Err(format!(
            "{} accepted the AtrisBridge registration command, but the resulting configuration could not be verified: {detail}",
            kind.label()
        )),
        RegistrationProbe::Absent | RegistrationProbe::OwnedStale => Err(format!(
            "{} did not retain the verified AtrisBridge MCP registration.",
            kind.label()
        )),
    }
}

pub fn unregister_local_mcp_client(
    app: AppHandle,
    kind: LocalMcpClientKind,
) -> Result<LocalMcpClientStatus, String> {
    let managed = ensure_managed_companion(&app)?;
    let executable = resolve_client_executable(&app, kind)?.ok_or_else(|| {
        format!(
            "{} is not available, so AtrisBridge cannot safely remove its MCP registration through the official client CLI.",
            kind.label()
        )
    })?;

    match probe_registration(&app, kind, &executable, &managed) {
        RegistrationProbe::Absent => return Ok(client_status(&app, kind, Ok(&managed))),
        RegistrationProbe::Exact | RegistrationProbe::OwnedStale => {
            remove_registration(&app, kind, &executable)?;
        }
        RegistrationProbe::Conflict(detail) => {
            return Err(format!(
                "AtrisBridge refused to remove the '{SERVER_NAME}' entry because it is not an AtrisBridge-owned registration. {detail}"
            ))
        }
        RegistrationProbe::Error(detail) => {
            return Err(format!(
                "Could not safely inspect the {} MCP registration before removal: {detail}",
                kind.label()
            ))
        }
    }

    match probe_registration(&app, kind, &executable, &managed) {
        RegistrationProbe::Absent => Ok(client_status(&app, kind, Ok(&managed))),
        RegistrationProbe::Conflict(detail)
        | RegistrationProbe::Error(detail) => Err(format!(
            "{} returned from the removal command, but AtrisBridge could not verify the result: {detail}",
            kind.label()
        )),
        RegistrationProbe::Exact | RegistrationProbe::OwnedStale => Err(format!(
            "{} still reports an AtrisBridge MCP registration after removal.",
            kind.label()
        )),
    }
}

fn client_status(
    app: &AppHandle,
    kind: LocalMcpClientKind,
    managed: Result<&ManagedCompanion, &String>,
) -> LocalMcpClientStatus {
    let managed_version = env!("CARGO_PKG_VERSION").to_string();
    let managed = match managed {
        Ok(value) => value,
        Err(error) => {
            return LocalMcpClientStatus {
                kind,
                label: kind.label(),
                principal: kind.principal(),
                executable_detected: false,
                version: None,
                registration_state: "companion_unavailable",
                registration_healthy: false,
                managed_companion_ready: false,
                managed_companion_version: managed_version,
                can_register: false,
                can_remove: false,
                detail: error.clone(),
            }
        }
    };

    let executable = match resolve_client_executable(app, kind) {
        Ok(Some(value)) => value,
        Ok(None) => {
            return LocalMcpClientStatus {
                kind,
                label: kind.label(),
                principal: kind.principal(),
                executable_detected: false,
                version: None,
                registration_state: "not_installed",
                registration_healthy: false,
                managed_companion_ready: true,
                managed_companion_version: managed_version,
                can_register: false,
                can_remove: false,
                detail: format!("{} was not found in a trusted PATH location.", kind.label()),
            }
        }
        Err(error) => {
            return LocalMcpClientStatus {
                kind,
                label: kind.label(),
                principal: kind.principal(),
                executable_detected: false,
                version: None,
                registration_state: "error",
                registration_healthy: false,
                managed_companion_ready: true,
                managed_companion_version: managed_version,
                can_register: false,
                can_remove: false,
                detail: error,
            }
        }
    };

    let version = client_version(app, &executable).ok().flatten();
    let (registration_state, registration_healthy, can_register, can_remove, detail) =
        match probe_registration(app, kind, &executable, managed) {
            RegistrationProbe::Absent => (
                "not_registered",
                false,
                true,
                false,
                "Client detected. AtrisBridge is not registered yet.".to_string(),
            ),
            RegistrationProbe::Exact => (
                "registered",
                true,
                true,
                true,
                "The client points to the current AtrisBridge managed companion.".to_string(),
            ),
            RegistrationProbe::OwnedStale => (
                "update_available",
                false,
                true,
                true,
                "An older AtrisBridge-managed companion is registered. Register again to repair the entry.".to_string(),
            ),
            RegistrationProbe::Conflict(detail) => {
                ("conflict", false, false, false, detail)
            }
            RegistrationProbe::Error(detail) => ("error", false, false, false, detail),
        };

    LocalMcpClientStatus {
        kind,
        label: kind.label(),
        principal: kind.principal(),
        executable_detected: true,
        version,
        registration_state,
        registration_healthy,
        managed_companion_ready: true,
        managed_companion_version: managed_version,
        can_register,
        can_remove,
        detail,
    }
}

fn add_registration(
    app: &AppHandle,
    kind: LocalMcpClientKind,
    executable: &Path,
    managed_path: &Path,
) -> Result<(), String> {
    let managed = managed_path
        .to_str()
        .ok_or_else(|| "Managed MCP companion path is not valid UTF-8.".to_string())?;
    let args = match kind {
        LocalMcpClientKind::Codex => vec![
            OsString::from("mcp"),
            OsString::from("add"),
            OsString::from(SERVER_NAME),
            OsString::from("--"),
            OsString::from(managed),
            OsString::from("--client"),
            OsString::from("codex"),
        ],
        LocalMcpClientKind::Claude => {
            let config = serde_json::json!({
                "type": "stdio",
                "command": managed,
                "args": ["--client", "claude"]
            });
            vec![
                OsString::from("mcp"),
                OsString::from("add-json"),
                OsString::from("--scope"),
                OsString::from("user"),
                OsString::from(SERVER_NAME),
                OsString::from(config.to_string()),
            ]
        }
    };
    let output = run_cli(app, executable, &args, CLI_TIMEOUT)?;
    require_cli_success(kind, "register", output)
}

fn remove_registration(
    app: &AppHandle,
    kind: LocalMcpClientKind,
    executable: &Path,
) -> Result<(), String> {
    let args = match kind {
        LocalMcpClientKind::Codex => vec![
            OsString::from("mcp"),
            OsString::from("remove"),
            OsString::from(SERVER_NAME),
        ],
        LocalMcpClientKind::Claude => vec![
            OsString::from("mcp"),
            OsString::from("remove"),
            OsString::from("--scope"),
            OsString::from("user"),
            OsString::from(SERVER_NAME),
        ],
    };
    let output = run_cli(app, executable, &args, CLI_TIMEOUT)?;
    require_cli_success(kind, "remove", output)
}

fn require_cli_success(
    kind: LocalMcpClientKind,
    action: &str,
    output: CliOutput,
) -> Result<(), String> {
    if output.status.success() && !output.stdout_truncated && !output.stderr_truncated {
        return Ok(());
    }
    let detail = bounded_cli_detail(&output);
    if output.stdout_truncated || output.stderr_truncated {
        return Err(format!(
            "{} MCP {action} output exceeded the safety bound.",
            kind.label()
        ));
    }
    Err(format!(
        "{} MCP {action} command failed{}.",
        kind.label(),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

fn probe_registration(
    app: &AppHandle,
    kind: LocalMcpClientKind,
    executable: &Path,
    managed: &ManagedCompanion,
) -> RegistrationProbe {
    match kind {
        LocalMcpClientKind::Codex => probe_codex(app, executable, managed),
        LocalMcpClientKind::Claude => probe_claude(app, executable, managed),
    }
}

fn probe_codex(
    app: &AppHandle,
    executable: &Path,
    managed: &ManagedCompanion,
) -> RegistrationProbe {
    let args = [
        OsString::from("mcp"),
        OsString::from("get"),
        OsString::from(SERVER_NAME),
        OsString::from("--json"),
    ];
    let output = match run_cli(app, executable, &args, CLI_TIMEOUT) {
        Ok(value) => value,
        Err(error) => return RegistrationProbe::Error(error),
    };
    if output.stdout_truncated || output.stderr_truncated {
        return RegistrationProbe::Error(
            "Codex MCP inspection output exceeded the safety bound.".into(),
        );
    }
    if !output.status.success() {
        return if looks_absent(&output) {
            RegistrationProbe::Absent
        } else {
            RegistrationProbe::Error(bounded_cli_detail(&output))
        };
    }
    let value: Value = match serde_json::from_str(output.stdout.trim()) {
        Ok(value) => value,
        Err(_) => {
            return RegistrationProbe::Error(
                "Codex returned an invalid JSON response for the MCP registration.".into(),
            )
        }
    };
    let transport = match value.get("transport").and_then(Value::as_object) {
        Some(value) => value,
        None => {
            return RegistrationProbe::Conflict(
                "The existing Codex entry does not use a supported stdio transport.".into(),
            )
        }
    };
    if transport.get("type").and_then(Value::as_str) != Some("stdio") {
        return RegistrationProbe::Conflict(
            "The existing Codex entry is not a local stdio AtrisBridge server.".into(),
        );
    }
    let command = transport
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = transport
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    classify_registration(command, &args, LocalMcpClientKind::Codex, managed)
}

fn probe_claude(
    app: &AppHandle,
    executable: &Path,
    managed: &ManagedCompanion,
) -> RegistrationProbe {
    let args = [
        OsString::from("mcp"),
        OsString::from("get"),
        OsString::from(SERVER_NAME),
    ];
    let output = match run_cli(app, executable, &args, CLI_TIMEOUT) {
        Ok(value) => value,
        Err(error) => return RegistrationProbe::Error(error),
    };
    if output.stdout_truncated || output.stderr_truncated {
        return RegistrationProbe::Error(
            "Claude MCP inspection output exceeded the safety bound.".into(),
        );
    }
    if !output.status.success() {
        return if looks_absent(&output) {
            RegistrationProbe::Absent
        } else {
            RegistrationProbe::Error(bounded_cli_detail(&output))
        };
    }

    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let normalized = normalize_display_path(&combined);
    let current_path = normalize_display_path(&managed.path.to_string_lossy());
    if normalized.contains(&current_path)
        && normalized.contains("--client")
        && normalized.contains("claude")
    {
        return RegistrationProbe::Exact;
    }

    let root = normalize_display_path(&managed.root.to_string_lossy());
    if normalized.contains(&root)
        && normalized.contains("atrisbridge-mcp")
        && normalized.contains("--client")
        && normalized.contains("claude")
    {
        return RegistrationProbe::OwnedStale;
    }

    RegistrationProbe::Conflict(
        "Claude Code already has an 'atrisbridge' server whose command does not match the AtrisBridge-managed companion.".into(),
    )
}

fn classify_registration(
    command: &str,
    args: &[String],
    kind: LocalMcpClientKind,
    managed: &ManagedCompanion,
) -> RegistrationProbe {
    let expected_args = ["--client", kind.companion_arg()];
    if args.iter().map(String::as_str).eq(expected_args) {
        if path_equivalent(Path::new(command), &managed.path) {
            return RegistrationProbe::Exact;
        }
        if path_is_within_managed_root(Path::new(command), &managed.root)
            && Path::new(command)
                .file_stem()
                .and_then(OsStr::to_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("atrisbridge-mcp"))
        {
            return RegistrationProbe::OwnedStale;
        }
    }
    RegistrationProbe::Conflict(format!(
        "The existing {} entry uses a different command or argument contract.",
        kind.label()
    ))
}

fn client_version(app: &AppHandle, executable: &Path) -> Result<Option<String>, String> {
    let output = run_cli(
        app,
        executable,
        &[OsString::from("--version")],
        VERSION_TIMEOUT,
    )?;
    if !output.status.success() || output.stdout_truncated || output.stderr_truncated {
        return Ok(None);
    }
    let value = if !output.stdout.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    Ok((!value.is_empty()).then(|| value.chars().take(160).collect()))
}

fn resolve_client_executable(
    app: &AppHandle,
    kind: LocalMcpClientKind,
) -> Result<Option<PathBuf>, String> {
    let Some(path) = env::var_os("PATH") else {
        return Ok(None);
    };
    let workspace_roots = services::workspace::list(app)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|workspace| PathBuf::from(workspace.local_path).canonicalize().ok())
        .collect::<Vec<_>>();

    for directory in env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        for name in executable_candidates(kind.executable_name()) {
            let candidate = directory.join(name);
            let Ok(metadata) = fs::symlink_metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if workspace_roots
                .iter()
                .any(|root| canonical.starts_with(root))
            {
                continue;
            }
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

fn executable_candidates(base: &str) -> Vec<OsString> {
    #[cfg(windows)]
    {
        [".exe", ".cmd", ".bat", ".com"]
            .into_iter()
            .map(|extension| OsString::from(format!("{base}{extension}")))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![OsString::from(base)]
    }
}

fn ensure_managed_companion(app: &AppHandle) -> Result<ManagedCompanion, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| {
            format!("Could not resolve AtrisBridge application data directory: {error}")
        })?
        .join("mcp")
        .join("bin");
    let version_root = root.join(env!("CARGO_PKG_VERSION"));
    fs::create_dir_all(&version_root)
        .map_err(|error| format!("Could not create the managed MCP binary directory: {error}"))?;
    set_owner_only_directory(&root)?;
    set_owner_only_directory(&version_root)?;

    let destination = version_root.join(companion_file_name());
    let source = locate_bundled_companion(app)?;
    let source_hash = hash_file(&source)?;
    if destination.is_file() && hash_file(&destination).ok().as_deref() == Some(&source_hash) {
        set_owner_executable(&destination)?;
        return Ok(ManagedCompanion {
            path: destination,
            root,
        });
    }

    let temporary = version_root.join(format!(
        ".{}.{}.tmp",
        companion_file_name().to_string_lossy(),
        Uuid::new_v4()
    ));
    fs::copy(&source, &temporary)
        .map_err(|error| format!("Could not stage the managed MCP companion: {error}"))?;
    set_owner_executable(&temporary)?;
    let copied_hash = hash_file(&temporary)?;
    if copied_hash != source_hash {
        let _ = fs::remove_file(&temporary);
        return Err("Managed MCP companion verification failed after copying.".into());
    }
    if destination.exists() {
        fs::remove_file(&destination)
            .map_err(|error| format!("Could not replace the managed MCP companion: {error}"))?;
    }
    fs::rename(&temporary, &destination)
        .map_err(|error| format!("Could not activate the managed MCP companion: {error}"))?;
    set_owner_executable(&destination)?;
    Ok(ManagedCompanion {
        path: destination,
        root,
    })
}

fn locate_bundled_companion(app: &AppHandle) -> Result<PathBuf, String> {
    let name = companion_file_name();
    let mut candidates = Vec::<PathBuf>::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join(&name));
        candidates.push(resource_dir.join("bin").join(&name));
        candidates.push(resource_dir.join("binaries").join(&name));
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(&name));
            candidates.push(directory.join("resources").join(&name));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(
            cwd.join("src-tauri")
                .join("binaries")
                .join(staged_companion_file_name()),
        );
        candidates.push(cwd.join("binaries").join(staged_companion_file_name()));
    }

    for candidate in candidates {
        let Ok(metadata) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            return candidate
                .canonicalize()
                .map_err(|error| format!("Could not canonicalize bundled MCP companion: {error}"));
        }
    }
    Err("AtrisBridge could not locate its packaged MCP companion. Reinstall AtrisBridge or rebuild the MCP sidecar before registering a client.".into())
}

fn companion_file_name() -> OsString {
    #[cfg(windows)]
    {
        OsString::from("atrisbridge-mcp.exe")
    }
    #[cfg(not(windows))]
    {
        OsString::from("atrisbridge-mcp")
    }
}

fn staged_companion_file_name() -> OsString {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        OsString::from("atrisbridge-mcp-x86_64-pc-windows-msvc.exe")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        OsString::from("atrisbridge-mcp-x86_64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        OsString::from("atrisbridge-mcp-aarch64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        OsString::from("atrisbridge-mcp-x86_64-apple-darwin")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        OsString::from("atrisbridge-mcp-aarch64-apple-darwin")
    }
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("Could not open MCP companion for verification: {error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not verify MCP companion: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn run_cli(
    app: &AppHandle,
    executable: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<CliOutput, String> {
    let runtime = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve MCP client runtime directory: {error}"))?
        .join("mcp")
        .join("runtime");
    fs::create_dir_all(&runtime)
        .map_err(|error| format!("Could not create MCP client runtime directory: {error}"))?;
    set_owner_only_directory(&runtime)?;
    let id = Uuid::new_v4();
    let stdout_path = runtime.join(format!("{id}.stdout"));
    let stderr_path = runtime.join(format!("{id}.stderr"));
    let stdout = File::create(&stdout_path)
        .map_err(|error| format!("Could not prepare MCP client stdout capture: {error}"))?;
    let stderr = File::create(&stderr_path)
        .map_err(|error| format!("Could not prepare MCP client stderr capture: {error}"))?;

    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(&runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    configure_no_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start MCP client CLI: {error}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not observe MCP client CLI: {error}"))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_capture_files(&stdout_path, &stderr_path);
            return Err("MCP client CLI exceeded the safety timeout.".into());
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    };

    let (stdout, stdout_truncated) = read_bounded_text(&stdout_path, MAX_CLI_OUTPUT_BYTES)?;
    let (stderr, stderr_truncated) = read_bounded_text(&stderr_path, MAX_CLI_OUTPUT_BYTES)?;
    cleanup_capture_files(&stdout_path, &stderr_path);
    Ok(CliOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_bounded_text(path: &Path, max: usize) -> Result<(String, bool), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not inspect MCP client output: {error}"))?;
    let truncated = metadata.len() > max as u64;
    let mut file =
        File::open(path).map_err(|error| format!("Could not open MCP client output: {error}"))?;
    let mut buffer = Vec::with_capacity(max.min(metadata.len() as usize));
    file.by_ref()
        .take(max as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("Could not read MCP client output: {error}"))?;
    Ok((String::from_utf8_lossy(&buffer).into_owned(), truncated))
}

fn cleanup_capture_files(stdout: &Path, stderr: &Path) {
    let _ = fs::remove_file(stdout);
    let _ = fs::remove_file(stderr);
}

fn bounded_cli_detail(output: &CliOutput) -> String {
    let value = if !output.stderr.trim().is_empty() {
        output.stderr.trim()
    } else {
        output.stdout.trim()
    };
    value.chars().take(800).collect()
}

fn looks_absent(output: &CliOutput) -> bool {
    let lower = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    lower.contains("no mcp server named")
        || lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("no server")
}

fn path_equivalent(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => path_key(&left) == path_key(&right),
        _ => path_key(left) == path_key(right),
    }
}

fn path_is_within_managed_root(path: &Path, root: &Path) -> bool {
    if let (Ok(path), Ok(root)) = (path.canonicalize(), root.canonicalize()) {
        return path.starts_with(root);
    }
    let path = lexical_absolute(path);
    let root = lexical_absolute(root);
    path.starts_with(root)
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

fn path_key(path: &Path) -> String {
    let value = normalize_display_path(&path.to_string_lossy());
    #[cfg(windows)]
    {
        value.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value
    }
}

fn normalize_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Could not protect MCP directory permissions: {error}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("Could not protect MCP companion permissions: {error}"))
}

#[cfg(not(unix))]
fn set_owner_executable(_path: &Path) -> Result<(), String> {
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

    fn managed(path: &str, root: &str) -> ManagedCompanion {
        ManagedCompanion {
            path: PathBuf::from(path),
            root: PathBuf::from(root),
        }
    }

    #[test]
    fn codex_registration_requires_exact_companion_args() {
        let managed = managed("/safe/mcp/0.1/atrisbridge-mcp", "/safe/mcp");
        assert!(matches!(
            classify_registration(
                "/safe/mcp/0.1/atrisbridge-mcp",
                &["--client".into(), "codex".into()],
                LocalMcpClientKind::Codex,
                &managed,
            ),
            RegistrationProbe::Exact
        ));
        assert!(matches!(
            classify_registration(
                "/tmp/atrisbridge-mcp",
                &["--client".into(), "codex".into()],
                LocalMcpClientKind::Codex,
                &managed,
            ),
            RegistrationProbe::Conflict(_)
        ));
        assert!(matches!(
            classify_registration(
                "/safe/mcp/0.1/atrisbridge-mcp",
                &["--client".into(), "generic".into()],
                LocalMcpClientKind::Codex,
                &managed,
            ),
            RegistrationProbe::Conflict(_)
        ));
    }

    #[test]
    fn old_managed_version_is_repairable_but_foreign_path_is_not() {
        let managed = managed("/safe/mcp/0.2/atrisbridge-mcp", "/safe/mcp");
        assert!(matches!(
            classify_registration(
                "/safe/mcp/0.1/atrisbridge-mcp",
                &["--client".into(), "claude".into()],
                LocalMcpClientKind::Claude,
                &managed,
            ),
            RegistrationProbe::OwnedStale
        ));
        assert!(matches!(
            classify_registration(
                "/opt/foreign/atrisbridge-mcp",
                &["--client".into(), "claude".into()],
                LocalMcpClientKind::Claude,
                &managed,
            ),
            RegistrationProbe::Conflict(_)
        ));
    }

    #[test]
    fn relative_parent_segments_do_not_escape_managed_root_check() {
        assert!(!path_is_within_managed_root(
            Path::new("/safe/mcp/../foreign/atrisbridge-mcp"),
            Path::new("/safe/mcp")
        ));
    }
}
