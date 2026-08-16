use std::{
    collections::HashSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    ai_gateway::{self, AiAuditEvent, AiSession},
    ai_git,
    ai_workspace::canonical_workspace_root,
    storage::find_workspace,
    workspace_coordinator::{
        WorkspaceLeaseError, WorkspaceMutationCoordinator, WorkspaceMutationLease,
        WorkspaceOperationKind,
    },
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_COMMAND_STDOUT_BYTES: usize = 256 * 1024;
const MAX_COMMAND_STDERR_BYTES: usize = 256 * 1024;
const MAX_PROJECT_SCAN_ENTRIES: usize = 10_000;
const MAX_PROJECT_SCAN_DEPTH: usize = 6;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(40);
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);
const PIPE_CHANNEL_CAPACITY: usize = 32;
const MIN_TIMEOUT_SECONDS: u64 = 30;
pub(crate) const MAX_TIMEOUT_SECONDS: u64 = 15 * 60;
const TASK_COORDINATOR_WAIT: Duration = Duration::from_secs(2 * 60);
const TASK_COORDINATOR_RETRY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
struct CommandProfileSpec {
    id: &'static str,
    label: &'static str,
    ecosystem: &'static str,
    tool: String,
    args: Vec<String>,
    timeout: Duration,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCommandProfile {
    pub id: String,
    pub label: String,
    pub ecosystem: String,
    pub tool: String,
    pub args: Vec<String>,
    pub command_preview: String,
    pub timeout_seconds: u64,
    pub available: bool,
    pub execution_policy: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCommandRunResult {
    pub run_id: String,
    pub workspace_id: String,
    pub profile_id: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub workspace_dirty_before: bool,
    pub workspace_dirty_after: bool,
    pub runtime_cleanup_incomplete: bool,
    pub output_capture_incomplete: bool,
    pub execution_policy: &'static str,
}

#[derive(Debug, Clone, Copy)]
enum PipeKind {
    Stdout,
    Stderr,
}

enum PipeEvent {
    Data(PipeKind, Vec<u8>),
    Closed(PipeKind),
    Failed(PipeKind),
}

#[derive(Default)]
struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    stdout_closed: bool,
    stderr_closed: bool,
}

struct CommandOutput {
    success: bool,
    code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    output_capture_incomplete: bool,
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let runtime_root = command_runtime_root(app)?;
    if runtime_root.is_dir() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    fs::create_dir_all(&runtime_root)
        .map_err(|error| format!("Could not initialize AI command runtime directory: {error}"))
}

#[tauri::command]
pub fn list_ai_command_profiles(
    app: AppHandle,
    session_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<Vec<AiCommandProfile>, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "command.execute")?;
    let result = inspect_ai_command_profiles(&app, &session_id, coordinator.inner());
    record_profile_result(&app, &session, started, &result)?;
    result
}

pub(crate) fn inspect_ai_command_profiles(
    app: &AppHandle,
    session_id: &str,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<Vec<AiCommandProfile>, String> {
    let session = ai_gateway::authorize_session(app, session_id, "command.execute")?;
    let workspace = find_workspace(app, &session.workspace_id)?;
    let primary_root = canonical_workspace_root(&workspace.local_path)?;
    let root = ai_git::session_workspace_root(app, &session, coordinator)?;
    let _lease = acquire_command(coordinator, &session, None)?;
    let specs = detect_profiles(&root)?;
    Ok(specs
        .into_iter()
        .map(|spec| public_profile(spec, &root, &primary_root))
        .collect())
}

#[tauri::command]
pub async fn run_ai_command(
    app: AppHandle,
    session_id: String,
    profile_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiCommandRunResult, String> {
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_ai_command_blocking(app, session_id, profile_id, coordinator, None)
    })
    .await
    .map_err(|error| format!("AI command worker failed: {error}"))?
}

pub(crate) fn run_ai_command_cancellable(
    app: AppHandle,
    session_id: String,
    profile_id: String,
    coordinator: WorkspaceMutationCoordinator,
    cancel: &AtomicBool,
) -> Result<AiCommandRunResult, String> {
    run_ai_command_blocking(app, session_id, profile_id, coordinator, Some(cancel))
}

fn run_ai_command_blocking(
    app: AppHandle,
    session_id: String,
    profile_id: String,
    coordinator: WorkspaceMutationCoordinator,
    cancel: Option<&AtomicBool>,
) -> Result<AiCommandRunResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "command.execute")?;
    let run_id = Uuid::new_v4().to_string();
    let result = (|| {
        validate_profile_id(&profile_id)?;

        let workspace = find_workspace(&app, &session.workspace_id)?;
        let primary_root = canonical_workspace_root(&workspace.local_path)?;
        let root = ai_git::session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_command(&coordinator, &session, cancel)?;

        let spec = detect_profiles(&root)?
            .into_iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| {
                "Requested command profile is not available for the current workspace state."
                    .to_string()
            })?;
        let executable =
            resolve_executable(&spec.tool, &root, &primary_root)?.ok_or_else(|| {
                format!(
                    "Required tool '{}' was not found in a trusted system/toolchain location.",
                    spec.tool
                )
            })?;

        let runtime_root = prepare_runtime(&app, &run_id)?;
        let workspace_dirty_before =
            match workspace_dirty(&app, &session, &run_id, &root, &primary_root, &runtime_root) {
                Ok(value) => value,
                Err(error) => {
                    let _ = cleanup_runtime(&runtime_root);
                    return Err(error);
                }
            };
        let output = match execute_profile(
            &app,
            &session,
            &run_id,
            &root,
            &primary_root,
            &runtime_root,
            &executable,
            &spec,
            cancel,
        ) {
            Ok(output) => output,
            Err(error) => {
                let _ = cleanup_runtime(&runtime_root);
                return Err(error);
            }
        };
        let workspace_dirty_after_result =
            workspace_dirty(&app, &session, &run_id, &root, &primary_root, &runtime_root);
        let runtime_cleanup_incomplete = cleanup_runtime(&runtime_root).is_err();
        let workspace_dirty_after = workspace_dirty_after_result?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(AiCommandRunResult {
            run_id: run_id.clone(),
            workspace_id: session.workspace_id.clone(),
            profile_id: spec.id.to_string(),
            success: output.success,
            exit_code: output.code,
            timed_out: output.timed_out,
            cancelled: output.cancelled,
            duration_ms,
            stdout: output_text(&output.stdout),
            stderr: output_text(&output.stderr),
            stdout_truncated: output.stdout_truncated,
            stderr_truncated: output.stderr_truncated,
            workspace_dirty_before,
            workspace_dirty_after,
            runtime_cleanup_incomplete,
            output_capture_incomplete: output.output_capture_incomplete,
            execution_policy: execution_policy(&session),
        })
    })();
    record_command_result(
        &app,
        &session,
        &run_id,
        started,
        &result,
        cancellation_requested(cancel),
    )?;
    result
}

fn execution_policy(session: &AiSession) -> &'static str {
    if session.mode == "direct" {
        "direct_workspace_fixed_profile_credential_minimized"
    } else {
        "isolated_worktree_fixed_profile_credential_minimized"
    }
}

fn public_profile(spec: CommandProfileSpec, root: &Path, primary_root: &Path) -> AiCommandProfile {
    let available = resolve_executable(&spec.tool, root, primary_root)
        .ok()
        .flatten()
        .is_some();
    let preview = if spec.args.is_empty() {
        spec.tool.clone()
    } else {
        format!("{} {}", spec.tool, spec.args.join(" "))
    };
    AiCommandProfile {
        id: spec.id.to_string(),
        label: spec.label.to_string(),
        ecosystem: spec.ecosystem.to_string(),
        tool: spec.tool,
        args: spec.args,
        command_preview: preview,
        timeout_seconds: spec.timeout.as_secs(),
        available,
        execution_policy: "fixed_profile_credential_minimized",
    }
}

fn detect_profiles(root: &Path) -> Result<Vec<CommandProfileSpec>, String> {
    let mut profiles = Vec::new();
    detect_node_profiles(root, &mut profiles)?;
    detect_rust_profiles(root, &mut profiles);
    detect_dotnet_profiles(root, &mut profiles)?;
    detect_python_profiles(root, &mut profiles);
    detect_go_profiles(root, &mut profiles);
    Ok(profiles)
}

fn safe_regular_root_file(root: &Path, name: &str) -> bool {
    fs::symlink_metadata(root.join(name))
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn detect_node_profiles(root: &Path, profiles: &mut Vec<CommandProfileSpec>) -> Result<(), String> {
    let manifest = root.join("package.json");
    if !safe_regular_root_file(root, "package.json") {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&manifest)
        .map_err(|error| format!("Could not inspect package.json: {error}"))?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err("package.json exceeds the AI command manifest safety bound.".into());
    }
    let text = fs::read_to_string(&manifest)
        .map_err(|error| format!("Could not read package.json: {error}"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("Could not parse package.json for command profiles: {error}"))?;
    let Some(scripts) = value.get("scripts").and_then(Value::as_object) else {
        return Ok(());
    };
    let manager = detect_node_package_manager(root, &value);
    for (script, id, label, timeout_seconds) in [
        ("build", "node.build", "Build", 10 * 60),
        ("test", "node.test", "Test", 10 * 60),
        ("lint", "node.lint", "Lint", 5 * 60),
        ("typecheck", "node.typecheck", "Typecheck", 5 * 60),
        ("check", "node.check", "Check", 5 * 60),
    ] {
        if scripts.get(script).and_then(Value::as_str).is_some() {
            profiles.push(CommandProfileSpec {
                id,
                label,
                ecosystem: "node",
                tool: manager.clone(),
                args: vec!["run".into(), script.into()],
                timeout: bounded_timeout(timeout_seconds),
            });
        }
    }
    Ok(())
}

fn detect_node_package_manager(root: &Path, manifest: &Value) -> String {
    if let Some(value) = manifest.get("packageManager").and_then(Value::as_str) {
        let name = value.split('@').next().unwrap_or_default();
        if matches!(name, "npm" | "pnpm" | "yarn" | "bun") {
            return name.to_string();
        }
    }
    if safe_regular_root_file(root, "pnpm-lock.yaml") {
        "pnpm".into()
    } else if safe_regular_root_file(root, "yarn.lock") {
        "yarn".into()
    } else if safe_regular_root_file(root, "bun.lock") || safe_regular_root_file(root, "bun.lockb")
    {
        "bun".into()
    } else {
        "npm".into()
    }
}

fn detect_rust_profiles(root: &Path, profiles: &mut Vec<CommandProfileSpec>) {
    if !safe_regular_root_file(root, "Cargo.toml") {
        return;
    }
    let locked = safe_regular_root_file(root, "Cargo.lock");
    profiles.push(CommandProfileSpec {
        id: "rust.check",
        label: "Cargo check",
        ecosystem: "rust",
        tool: "cargo".into(),
        args: cargo_args("check", locked),
        timeout: bounded_timeout(10 * 60),
    });
    profiles.push(CommandProfileSpec {
        id: "rust.test",
        label: "Cargo test",
        ecosystem: "rust",
        tool: "cargo".into(),
        args: cargo_args("test", locked),
        timeout: bounded_timeout(10 * 60),
    });
    profiles.push(CommandProfileSpec {
        id: "rust.fmt_check",
        label: "Cargo format check",
        ecosystem: "rust",
        tool: "cargo".into(),
        args: vec!["fmt".into(), "--all".into(), "--".into(), "--check".into()],
        timeout: bounded_timeout(2 * 60),
    });
    profiles.push(CommandProfileSpec {
        id: "rust.clippy",
        label: "Cargo clippy",
        ecosystem: "rust",
        tool: "cargo".into(),
        args: vec![
            "clippy".into(),
            "--all-targets".into(),
            "--all-features".into(),
        ],
        timeout: bounded_timeout(10 * 60),
    });
}

fn cargo_args(command: &str, locked: bool) -> Vec<String> {
    let mut args = vec![command.to_string()];
    if locked {
        args.push("--locked".into());
    }
    args
}

fn detect_dotnet_profiles(
    root: &Path,
    profiles: &mut Vec<CommandProfileSpec>,
) -> Result<(), String> {
    let targets = discover_dotnet_targets(root)?;
    let Some(target) = preferred_dotnet_target(&targets) else {
        return Ok(());
    };

    for (id, label, configuration, platform) in [
        ("dotnet.build.debug", "dotnet build Debug", "Debug", None),
        (
            "dotnet.build.debug.x64",
            "dotnet build Debug x64",
            "Debug",
            Some("x64"),
        ),
        (
            "dotnet.build.release",
            "dotnet build Release",
            "Release",
            None,
        ),
        (
            "dotnet.build.release.x64",
            "dotnet build Release x64",
            "Release",
            Some("x64"),
        ),
    ] {
        profiles.push(CommandProfileSpec {
            id,
            label,
            ecosystem: "dotnet",
            tool: "dotnet".into(),
            args: dotnet_build_args(&target, configuration, platform),
            timeout: bounded_timeout(15 * 60),
        });
    }
    profiles.push(CommandProfileSpec {
        id: "dotnet.test",
        label: "dotnet test",
        ecosystem: "dotnet",
        tool: "dotnet".into(),
        args: vec!["test".into(), target.clone(), "--nologo".into()],
        timeout: bounded_timeout(15 * 60),
    });

    #[cfg(windows)]
    for (id, label, configuration, platform) in [
        (
            "msbuild.build.debug.x64",
            "MSBuild Debug x64",
            "Debug",
            "x64",
        ),
        (
            "msbuild.build.release.x64",
            "MSBuild Release x64",
            "Release",
            "x64",
        ),
        (
            "msbuild.build.debug.anycpu",
            "MSBuild Debug Any CPU",
            "Debug",
            "Any CPU",
        ),
        (
            "msbuild.build.release.anycpu",
            "MSBuild Release Any CPU",
            "Release",
            "Any CPU",
        ),
    ] {
        profiles.push(CommandProfileSpec {
            id,
            label,
            ecosystem: "dotnet",
            tool: "msbuild".into(),
            args: msbuild_args(&target, configuration, platform),
            timeout: bounded_timeout(15 * 60),
        });
    }

    Ok(())
}

fn dotnet_build_args(target: &str, configuration: &str, platform: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "build".into(),
        target.to_string(),
        "--nologo".into(),
        "--configuration".into(),
        configuration.to_string(),
    ];
    if let Some(platform) = platform {
        args.push(format!("-p:Platform={platform}"));
    }
    args
}

#[cfg(windows)]
fn msbuild_args(target: &str, configuration: &str, platform: &str) -> Vec<String> {
    vec![
        target.to_string(),
        "/nologo".into(),
        "/m".into(),
        "/restore".into(),
        format!("/p:Configuration={configuration}"),
        format!("/p:Platform={platform}"),
    ]
}

fn discover_dotnet_targets(root: &Path) -> Result<Vec<String>, String> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut inspected = 0usize;
    let mut targets = Vec::new();

    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("Could not inspect workspace project files: {error}"))?
        {
            if inspected >= MAX_PROJECT_SCAN_ENTRIES {
                return Err(
                    "Workspace contains too many entries for bounded .NET project discovery."
                        .into(),
                );
            }
            inspected += 1;
            let entry = entry
                .map_err(|error| format!("Could not inspect workspace project entry: {error}"))?;
            let file_type = entry.file_type().map_err(|error| {
                format!("Could not inspect workspace project entry type: {error}")
            })?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if depth < MAX_PROJECT_SCAN_DEPTH && !skip_project_directory(&entry.file_name()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let extension = path
                .extension()
                .and_then(OsStr::to_str)
                .map(str::to_ascii_lowercase);
            if !extension
                .as_deref()
                .map(|value| matches!(value, "sln" | "slnx" | "csproj" | "fsproj" | "vbproj"))
                .unwrap_or(false)
            {
                continue;
            }
            let relative = path.strip_prefix(root).map_err(|_| {
                "Discovered .NET project escaped the workspace root unexpectedly.".to_string()
            })?;
            targets.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }

    targets.sort_by(|left, right| dotnet_target_sort_key(left).cmp(&dotnet_target_sort_key(right)));
    targets.dedup();
    Ok(targets)
}

fn skip_project_directory(name: &OsStr) -> bool {
    matches!(
        name.to_string_lossy().to_ascii_lowercase().as_str(),
        ".git"
            | ".hg"
            | ".svn"
            | ".vs"
            | ".idea"
            | "bin"
            | "obj"
            | "node_modules"
            | "target"
            | ".venv"
            | "venv"
    )
}

fn dotnet_target_sort_key(path: &str) -> (u8, usize, String) {
    let extension_rank = if path.to_ascii_lowercase().ends_with(".slnx") {
        0
    } else if path.to_ascii_lowercase().ends_with(".sln") {
        1
    } else {
        2
    };
    (
        extension_rank,
        path.matches('/').count(),
        path.to_ascii_lowercase(),
    )
}

fn preferred_dotnet_target(targets: &[String]) -> Option<String> {
    targets.first().cloned()
}

fn detect_python_profiles(root: &Path, profiles: &mut Vec<CommandProfileSpec>) {
    if !["pyproject.toml", "pytest.ini", "tox.ini", "setup.cfg"]
        .iter()
        .any(|name| safe_regular_root_file(root, name))
    {
        return;
    }
    profiles.push(CommandProfileSpec {
        id: "python.pytest",
        label: "pytest",
        ecosystem: "python",
        tool: "python".into(),
        args: vec!["-m".into(), "pytest".into(), "-q".into()],
        timeout: bounded_timeout(10 * 60),
    });
}

fn detect_go_profiles(root: &Path, profiles: &mut Vec<CommandProfileSpec>) {
    if !safe_regular_root_file(root, "go.mod") {
        return;
    }
    profiles.push(CommandProfileSpec {
        id: "go.test",
        label: "go test",
        ecosystem: "go",
        tool: "go".into(),
        args: vec!["test".into(), "./...".into()],
        timeout: bounded_timeout(10 * 60),
    });
    profiles.push(CommandProfileSpec {
        id: "go.vet",
        label: "go vet",
        ecosystem: "go",
        tool: "go".into(),
        args: vec!["vet".into(), "./...".into()],
        timeout: bounded_timeout(5 * 60),
    });
}

fn bounded_timeout(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS))
}

pub(crate) fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    if profile_id.is_empty()
        || profile_id.len() > 64
        || profile_id.starts_with('-')
        || !profile_id.bytes().all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || b"._-".contains(&value)
        })
    {
        return Err("Command profile identifier is invalid.".into());
    }
    Ok(())
}

fn resolve_executable(
    tool: &str,
    root: &Path,
    primary_root: &Path,
) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    if tool == "msbuild" {
        if let Some(path) = resolve_windows_msbuild(root, primary_root) {
            return Ok(Some(path));
        }
    }

    let path = env::var_os("PATH")
        .ok_or_else(|| "PATH is unavailable for command discovery.".to_string())?;
    let mut seen = HashSet::<PathBuf>::new();
    for directory in env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        let Ok(directory) = directory.canonicalize() else {
            continue;
        };
        if directory.starts_with(root)
            || directory.starts_with(primary_root)
            || !seen.insert(directory.clone())
        {
            continue;
        }
        for candidate in tool_candidates(tool) {
            let candidate = directory.join(candidate);
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() || !is_executable_file(&metadata) {
                continue;
            }
            let Ok(candidate) = candidate.canonicalize() else {
                continue;
            };
            if candidate.starts_with(root) || candidate.starts_with(primary_root) {
                continue;
            }
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn resolve_windows_msbuild(root: &Path, primary_root: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(install) = env::var_os("VSINSTALLDIR") {
        candidates.push(
            PathBuf::from(install)
                .join("MSBuild")
                .join("Current")
                .join("Bin")
                .join("MSBuild.exe"),
        );
    }
    for key in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
        let Some(base) = env::var_os(key) else {
            continue;
        };
        for edition in ["Enterprise", "Professional", "Community", "BuildTools"] {
            candidates.push(
                PathBuf::from(&base)
                    .join("Microsoft Visual Studio")
                    .join("2022")
                    .join(edition)
                    .join("MSBuild")
                    .join("Current")
                    .join("Bin")
                    .join("MSBuild.exe"),
            );
        }
    }
    candidates.into_iter().find_map(|candidate| {
        let candidate = candidate.canonicalize().ok()?;
        if candidate.is_file()
            && !candidate.starts_with(root)
            && !candidate.starts_with(primary_root)
        {
            Some(candidate)
        } else {
            None
        }
    })
}

#[cfg(unix)]
fn is_executable_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_file(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(windows)]
fn tool_candidates(tool: &str) -> Vec<String> {
    match tool {
        "npm" | "pnpm" | "yarn" | "bun" => vec![format!("{tool}.cmd"), format!("{tool}.exe")],
        "python" => vec!["python.exe".into(), "python3.exe".into()],
        _ => vec![format!("{tool}.exe"), tool.to_string()],
    }
}

#[cfg(not(windows))]
fn tool_candidates(tool: &str) -> Vec<String> {
    if tool == "python" {
        vec!["python3".into(), "python".into()]
    } else {
        vec![tool.to_string()]
    }
}

fn sanitized_path(root: &Path, primary_root: &Path) -> Result<OsString, String> {
    let path = env::var_os("PATH")
        .ok_or_else(|| "PATH is unavailable for command execution.".to_string())?;
    let mut directories = Vec::new();
    let mut seen = HashSet::<PathBuf>::new();
    for directory in env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        let Ok(directory) = directory.canonicalize() else {
            continue;
        };
        if directory.starts_with(root)
            || directory.starts_with(primary_root)
            || !seen.insert(directory.clone())
        {
            continue;
        }
        directories.push(directory);
    }
    if directories.is_empty() {
        return Err("No trusted PATH entries remain for AI command execution.".into());
    }
    env::join_paths(directories)
        .map_err(|error| format!("Could not construct sanitized command PATH: {error}"))
}

fn prepare_runtime(app: &AppHandle, run_id: &str) -> Result<PathBuf, String> {
    let runtime = command_runtime_root(app)?.join(run_id);
    if runtime.exists() {
        return Err("AI command runtime directory unexpectedly already exists.".into());
    }
    for path in [
        runtime.join("home"),
        runtime.join("tmp"),
        runtime.join("cache"),
        runtime.join("config"),
        runtime.join("data"),
        runtime.join("appdata"),
        runtime.join("localappdata"),
    ] {
        fs::create_dir_all(&path)
            .map_err(|error| format!("Could not prepare AI command runtime: {error}"))?;
    }
    fs::write(runtime.join("gitconfig"), b"")
        .map_err(|error| format!("Could not create isolated Git config: {error}"))?;
    fs::write(runtime.join("npmrc"), b"")
        .map_err(|error| format!("Could not create isolated npm config: {error}"))?;
    fs::write(runtime.join("pip.conf"), b"")
        .map_err(|error| format!("Could not create isolated pip config: {error}"))?;
    Ok(runtime)
}

fn command_runtime_root(app: &AppHandle) -> Result<PathBuf, String> {
    let process_id = std::process::id().to_string();
    app.path()
        .app_data_dir()
        .map(|path| path.join("ai-command-runtime").join(process_id))
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))
}

fn cleanup_runtime(runtime: &Path) -> Result<(), String> {
    if runtime.exists() {
        fs::remove_dir_all(runtime)
            .map_err(|error| format!("Could not clean AI command runtime directory: {error}"))?;
    }
    Ok(())
}

fn execute_profile(
    app: &AppHandle,
    session: &AiSession,
    run_id: &str,
    root: &Path,
    primary_root: &Path,
    runtime_root: &Path,
    executable: &Path,
    spec: &CommandProfileSpec,
    cancel: Option<&AtomicBool>,
) -> Result<CommandOutput, String> {
    if cancellation_requested(cancel) {
        return Ok(CommandOutput {
            success: false,
            code: None,
            timed_out: false,
            cancelled: true,
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            output_capture_incomplete: false,
        });
    }
    let mut command = Command::new(executable);
    command
        .args(&spec.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_environment(
        &mut command,
        app,
        session,
        run_id,
        root,
        primary_root,
        runtime_root,
    )?;
    configure_process_isolation(&mut command);
    let child = command.spawn().map_err(|error| {
        format!(
            "Could not start fixed command profile '{}': {error}",
            spec.id
        )
    })?;
    collect_child_output(
        child,
        spec.timeout,
        MAX_COMMAND_STDOUT_BYTES,
        MAX_COMMAND_STDERR_BYTES,
        cancel,
    )
}

fn configure_environment(
    command: &mut Command,
    _app: &AppHandle,
    _session: &AiSession,
    _run_id: &str,
    root: &Path,
    primary_root: &Path,
    runtime_root: &Path,
) -> Result<(), String> {
    let home = runtime_root.join("home");
    let tmp = runtime_root.join("tmp");
    let cache = runtime_root.join("cache");
    let config = runtime_root.join("config");
    let data = runtime_root.join("data");
    let appdata = runtime_root.join("appdata");
    let localappdata = runtime_root.join("localappdata");
    let path = sanitized_path(root, primary_root)?;

    command.env_clear();
    command
        .env("PATH", path)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .env("TMPDIR", &tmp)
        .env("APPDATA", &appdata)
        .env("LOCALAPPDATA", &localappdata)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("DOTNET_CLI_HOME", &home)
        .env("NUGET_PACKAGES", cache.join("nuget"))
        .env("CARGO_HOME", cache.join("cargo"))
        .env("GOPATH", cache.join("go"))
        .env("GOCACHE", cache.join("go-build"))
        .env("NPM_CONFIG_USERCONFIG", runtime_root.join("npmrc"))
        .env("PIP_CONFIG_FILE", runtime_root.join("pip.conf"))
        .env("GIT_CONFIG_GLOBAL", runtime_root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("CARGO_TERM_COLOR", "never")
        .env("DOTNET_NOLOGO", "1")
        .env("NPM_CONFIG_AUDIT", "false")
        .env("NPM_CONFIG_FUND", "false")
        .env("PIP_DISABLE_PIP_VERSION_CHECK", "1");

    preserve_platform_environment(command);
    preserve_toolchain_environment(command, root, primary_root);
    Ok(())
}

#[cfg(windows)]
fn preserve_platform_environment(command: &mut Command) {
    for key in [
        "SystemRoot",
        "WINDIR",
        "PATHEXT",
        "COMSPEC",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

#[cfg(not(windows))]
fn preserve_platform_environment(_command: &mut Command) {}

fn preserve_toolchain_environment(command: &mut Command, root: &Path, primary_root: &Path) {
    for key in [
        "RUSTUP_HOME",
        "DOTNET_ROOT",
        "GOROOT",
        "JAVA_HOME",
        "PYENV_ROOT",
        "VSINSTALLDIR",
        "WindowsSdkDir",
        "FrameworkDir",
        "FrameworkSDKDir",
    ] {
        let Some(value) = env::var_os(key) else {
            continue;
        };
        let path = PathBuf::from(&value);
        if trusted_external_directory(&path, root, primary_root) {
            command.env(key, value);
        }
    }
    if env::var_os("RUSTUP_HOME").is_none() {
        if let Some(home) = original_home() {
            let rustup = home.join(".rustup");
            if trusted_external_directory(&rustup, root, primary_root) {
                command.env("RUSTUP_HOME", rustup);
            }
        }
    }
}

fn original_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn trusted_external_directory(path: &Path, root: &Path, primary_root: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    path.is_dir() && !path.starts_with(root) && !path.starts_with(primary_root)
}

fn workspace_dirty(
    app: &AppHandle,
    session: &AiSession,
    run_id: &str,
    root: &Path,
    primary_root: &Path,
    runtime_root: &Path,
) -> Result<bool, String> {
    let git_marker = root.join(".git");
    let is_git_workspace = fs::symlink_metadata(&git_marker)
        .map(|metadata| {
            !metadata.file_type().is_symlink() && (metadata.is_dir() || metadata.is_file())
        })
        .unwrap_or(false);
    if !is_git_workspace {
        return Ok(false);
    }
    let Some(executable) = resolve_executable("git", root, primary_root)? else {
        return Ok(false);
    };
    let mut command = Command::new(executable);
    command
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_environment(
        &mut command,
        app,
        session,
        run_id,
        root,
        primary_root,
        runtime_root,
    )?;
    configure_process_isolation(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("Could not inspect command workspace status: {error}"))?;
    let output = collect_child_output(
        child,
        Duration::from_secs(30),
        MAX_COMMAND_STDOUT_BYTES,
        16 * 1024,
        None,
    )?;
    if output.timed_out {
        return Err("Git status inspection exceeded the command safety timeout.".into());
    }
    if !output.success || output.output_capture_incomplete {
        return Err("Could not safely inspect command workspace status.".into());
    }
    if output.stdout_truncated {
        return Err("Git status output exceeded the command safety bound.".into());
    }
    Ok(!output.stdout.is_empty())
}

fn collect_child_output(
    mut child: Child,
    timeout: Duration,
    stdout_max: usize,
    stderr_max: usize,
    cancel: Option<&AtomicBool>,
) -> Result<CommandOutput, String> {
    let stdout: ChildStdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not capture command stdout.".to_string())?;
    let stderr: ChildStderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not capture command stderr.".to_string())?;
    let (sender, receiver) = mpsc::sync_channel::<PipeEvent>(PIPE_CHANNEL_CAPACITY);
    let stdout_thread = spawn_pipe_reader(stdout, PipeKind::Stdout, sender.clone());
    let stderr_thread = spawn_pipe_reader(stderr, PipeKind::Stderr, sender.clone());
    drop(sender);

    let started = Instant::now();
    let mut capture = CapturedOutput::default();
    let mut status: Option<ExitStatus> = None;
    let mut timed_out = false;
    let mut cancelled = false;
    loop {
        drain_pipe_events(&receiver, &mut capture, stdout_max, stderr_max);
        if cancellation_requested(cancel) {
            cancelled = true;
            break;
        }
        if let Some(observed) = child
            .try_wait()
            .map_err(|error| format!("Could not observe command process: {error}"))?
        {
            status = Some(observed);
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            break;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }

    terminate_process_tree(&mut child);
    let drain_started = Instant::now();
    while !(capture.stdout_closed && capture.stderr_closed)
        && drain_started.elapsed() < OUTPUT_DRAIN_GRACE
    {
        match receiver.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(event) => apply_pipe_event(event, &mut capture, stdout_max, stderr_max),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                capture.stdout_closed = true;
                capture.stderr_closed = true;
            }
        }
    }
    drain_pipe_events(&receiver, &mut capture, stdout_max, stderr_max);
    let output_capture_incomplete = !(capture.stdout_closed && capture.stderr_closed);
    drop(receiver);
    if !output_capture_incomplete {
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
    }

    let parent_success = status
        .as_ref()
        .map(|value| value.success())
        .unwrap_or(false);
    let code = status.and_then(|value| value.code());
    Ok(CommandOutput {
        success: parent_success && !timed_out && !cancelled && !output_capture_incomplete,
        code,
        timed_out,
        cancelled,
        stdout: capture.stdout,
        stderr: capture.stderr,
        stdout_truncated: capture.stdout_truncated || output_capture_incomplete,
        stderr_truncated: capture.stderr_truncated || output_capture_incomplete,
        output_capture_incomplete,
    })
}

fn cancellation_requested(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|signal| signal.load(Ordering::SeqCst))
        .unwrap_or(false)
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    mut reader: R,
    kind: PipeKind,
    sender: SyncSender<PipeEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(PipeEvent::Closed(kind));
                    break;
                }
                Ok(read) => {
                    if sender
                        .send(PipeEvent::Data(kind, buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => {
                    let _ = sender.send(PipeEvent::Failed(kind));
                    break;
                }
            }
        }
    })
}

fn drain_pipe_events(
    receiver: &Receiver<PipeEvent>,
    capture: &mut CapturedOutput,
    stdout_max: usize,
    stderr_max: usize,
) {
    loop {
        match receiver.try_recv() {
            Ok(event) => apply_pipe_event(event, capture, stdout_max, stderr_max),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn apply_pipe_event(
    event: PipeEvent,
    capture: &mut CapturedOutput,
    stdout_max: usize,
    stderr_max: usize,
) {
    match event {
        PipeEvent::Data(PipeKind::Stdout, bytes) => {
            append_bounded(
                &mut capture.stdout,
                &mut capture.stdout_truncated,
                &bytes,
                stdout_max,
            );
        }
        PipeEvent::Data(PipeKind::Stderr, bytes) => {
            append_bounded(
                &mut capture.stderr,
                &mut capture.stderr_truncated,
                &bytes,
                stderr_max,
            );
        }
        PipeEvent::Closed(PipeKind::Stdout) => capture.stdout_closed = true,
        PipeEvent::Closed(PipeKind::Stderr) => capture.stderr_closed = true,
        PipeEvent::Failed(PipeKind::Stdout) => {
            capture.stdout_truncated = true;
            capture.stdout_closed = true;
        }
        PipeEvent::Failed(PipeKind::Stderr) => {
            capture.stderr_truncated = true;
            capture.stderr_closed = true;
        }
    }
}

fn append_bounded(stored: &mut Vec<u8>, truncated: &mut bool, bytes: &[u8], max: usize) {
    let remaining = max.saturating_sub(stored.len());
    let keep = remaining.min(bytes.len());
    if keep > 0 {
        stored.extend_from_slice(&bytes[..keep]);
    }
    if keep < bytes.len() {
        *truncated = true;
    }
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|value| *value != '\0')
        .collect()
}

#[cfg(unix)]
fn configure_process_isolation(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_isolation(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn configure_process_isolation(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_tree(child: &mut Child) {
    let group = format!("-{}", child.id());
    if let Some(kill) = [Path::new("/bin/kill"), Path::new("/usr/bin/kill")]
        .into_iter()
        .find(|path| path.is_file())
    {
        let _ = Command::new(kill)
            .args(["-9", group.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn terminate_process_tree(child: &mut Child) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let pid = child.id().to_string();
    let taskkill = env::var_os("SystemRoot")
        .or_else(|| env::var_os("WINDIR"))
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("taskkill.exe"));
    if let Some(taskkill) = taskkill.filter(|path| path.is_file()) {
        let mut killer = Command::new(taskkill);
        killer
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = killer.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(any(unix, windows)))]
fn terminate_process_tree(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn acquire_command(
    coordinator: &WorkspaceMutationCoordinator,
    session: &AiSession,
    cancel: Option<&AtomicBool>,
) -> Result<WorkspaceMutationLease, String> {
    let started = Instant::now();
    loop {
        match coordinator.acquire(
            &session.workspace_id,
            &format!("ai:{}", session.client_id),
            WorkspaceOperationKind::Command,
        ) {
            Ok(lease) => return Ok(lease),
            Err(WorkspaceLeaseError::Busy(active)) if cancel.is_some() => {
                if cancellation_requested(cancel) {
                    return Err(
                        "AI command task was cancelled while waiting for workspace ownership."
                            .into(),
                    );
                }
                if started.elapsed() >= TASK_COORDINATOR_WAIT {
                    return Err(format!(
                        "Workspace remained busy with '{}' owned by '{}' beyond the bounded task queue wait.",
                        active.kind, active.owner
                    ));
                }
                thread::sleep(TASK_COORDINATOR_RETRY);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn record_profile_result<T>(
    app: &AppHandle,
    session: &AiSession,
    started: Instant,
    result: &Result<T, String>,
) -> Result<(), String> {
    ai_gateway::record_audit(
        app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some("command.execute"),
            tool_name: "command.profiles",
            outcome: if result.is_ok() { "success" } else { "failed" },
            duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            operation_id: None,
            detail_code: Some(if result.is_ok() {
                "ok"
            } else {
                "operation_failed"
            }),
        },
    )
}

fn record_command_result(
    app: &AppHandle,
    session: &AiSession,
    run_id: &str,
    started: Instant,
    result: &Result<AiCommandRunResult, String>,
    cancellation_was_requested: bool,
) -> Result<(), String> {
    let (outcome, detail_code) = match result {
        Err(_) if cancellation_was_requested => ("cancelled", "cancelled"),
        Ok(run) if run.cancelled => ("cancelled", "cancelled"),
        Ok(run) if run.output_capture_incomplete => ("failed", "output_capture_incomplete"),
        Ok(run) if run.success && run.runtime_cleanup_incomplete => {
            ("failed", "cleanup_incomplete")
        }
        Ok(run) if run.success => ("success", "ok"),
        Ok(run) if run.timed_out => ("failed", "timeout"),
        Ok(run) if run.stdout_truncated || run.stderr_truncated => {
            ("failed", "exit_nonzero_output_truncated")
        }
        Ok(_) => ("failed", "exit_nonzero"),
        Err(_) => ("failed", "operation_failed"),
    };
    ai_gateway::record_audit(
        app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some("command.execute"),
            tool_name: "command.run",
            outcome,
            duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            operation_id: Some(run_id),
            detail_code: Some(detail_code),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("atrisbridge-ai-command-{name}-{}", Uuid::new_v4()))
    }

    #[test]
    fn profile_ids_reject_option_or_shell_like_input() {
        assert!(validate_profile_id("rust.test").is_ok());
        assert!(validate_profile_id("node.typecheck").is_ok());
        assert!(validate_profile_id("--help").is_err());
        assert!(validate_profile_id("rust.test;echo").is_err());
        assert!(validate_profile_id("../../test").is_err());
    }

    #[test]
    fn cargo_locked_profiles_are_reproducible() {
        assert_eq!(cargo_args("test", true), vec!["test", "--locked"]);
        assert_eq!(cargo_args("check", false), vec!["check"]);
    }

    #[test]
    fn node_detection_only_exposes_named_safe_slots() {
        let root = test_root("node");
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            root.join("package.json"),
            r#"{
              "packageManager": "pnpm@10.0.0",
              "scripts": {
                "build": "vite build",
                "test": "vitest",
                "deploy-prod": "dangerous custom script"
              }
            }"#,
        )
        .expect("write manifest");
        let profiles = detect_profiles(&root).expect("detect profiles");
        let ids = profiles
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"node.build"));
        assert!(ids.contains(&"node.test"));
        assert!(!ids.iter().any(|value| value.contains("deploy")));
        assert!(profiles
            .iter()
            .filter(|profile| profile.ecosystem == "node")
            .all(|profile| profile.tool == "pnpm"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn package_manager_lockfile_is_used_when_manifest_does_not_pin_one() {
        let root = test_root("manager");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").expect("write lockfile");
        let value: Value =
            serde_json::from_str(r#"{"scripts":{"test":"x"}}"#).expect("manifest json");
        assert_eq!(detect_node_package_manager(&root, &value), "pnpm");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_manifest_is_not_used_for_profile_detection() {
        use std::os::unix::fs::symlink;
        let root = test_root("symlink-manifest");
        let outside = test_root("outside-manifest");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside).expect("create outside");
        fs::write(
            outside.join("package.json"),
            r#"{"scripts":{"test":"echo outside"}}"#,
        )
        .expect("write outside manifest");
        symlink(outside.join("package.json"), root.join("package.json"))
            .expect("create manifest symlink");
        let profiles = detect_profiles(&root).expect("detect profiles");
        assert!(!profiles.iter().any(|profile| profile.ecosystem == "node"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn cancellation_signal_is_observed() {
        let signal = AtomicBool::new(false);
        assert!(!cancellation_requested(Some(&signal)));
        signal.store(true, Ordering::SeqCst);
        assert!(cancellation_requested(Some(&signal)));
        assert!(!cancellation_requested(None));
    }

    #[test]
    fn bounded_output_marks_truncation_without_exceeding_limit() {
        let mut stored = vec![1, 2];
        let mut truncated = false;
        append_bounded(&mut stored, &mut truncated, &[3, 4, 5], 4);
        assert_eq!(stored, vec![1, 2, 3, 4]);
        assert!(truncated);
    }

    #[test]
    fn dotnet_detection_finds_nested_solution_and_ignores_build_outputs() {
        let root = test_root("dotnet-nested");
        fs::create_dir_all(root.join("src/App")).expect("create source tree");
        fs::create_dir_all(root.join("src/App/obj")).expect("create obj tree");
        fs::write(root.join("src/App/App.csproj"), "<Project />").expect("write project");
        fs::write(
            root.join("Product.sln"),
            "Microsoft Visual Studio Solution File",
        )
        .expect("write solution");
        fs::write(root.join("src/App/obj/Generated.csproj"), "<Project />")
            .expect("write ignored generated project");

        let targets = discover_dotnet_targets(&root).expect("discover .NET targets");
        assert_eq!(targets.first().map(String::as_str), Some("Product.sln"));
        assert!(targets.iter().any(|target| target == "src/App/App.csproj"));
        assert!(!targets
            .iter()
            .any(|target| target.contains("Generated.csproj")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dotnet_profiles_offer_configuration_and_x64_builds() {
        let root = test_root("dotnet-profiles");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("App.slnx"), "<Solution />").expect("write solution");
        let profiles = detect_profiles(&root).expect("detect profiles");
        let ids = profiles
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"dotnet.build.debug"));
        assert!(ids.contains(&"dotnet.build.debug.x64"));
        assert!(ids.contains(&"dotnet.build.release"));
        assert!(ids.contains(&"dotnet.build.release.x64"));
        assert!(ids.contains(&"dotnet.test"));
        let x64 = profiles
            .iter()
            .find(|profile| profile.id == "dotnet.build.debug.x64")
            .expect("x64 profile");
        assert!(x64.args.iter().any(|arg| arg == "-p:Platform=x64"));
        let _ = fs::remove_dir_all(root);
    }
}
