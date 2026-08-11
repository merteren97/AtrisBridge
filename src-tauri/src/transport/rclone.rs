use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::{
    models::{RcloneStatus, RemoteFileObservation},
    scanner,
};

pub const REQUIRED_RCLONE_VERSION: &str = "1.74.4";
const DRIVE_SCOPE: &str = "drive.file";
const RCLONE_ENV_KEYS: &[&str] = &[
    "RCLONE_CONFIG",
    "RCLONE_CONFIG_PASS",
    "RCLONE_PASSWORD_COMMAND",
    "RCLONE_DRIVE_TOKEN",
    "RCLONE_DRIVE_CLIENT_ID",
    "RCLONE_DRIVE_CLIENT_SECRET",
    "RCLONE_DRIVE_SCOPE",
    "RCLONE_DRIVE_SKIP_GDOCS",
    "RCLONE_DRIVE_USE_TRASH",
    "RCLONE_DRIVE_KEEP_REVISION_FOREVER",
];

#[derive(Debug, Clone)]
pub struct RcloneRuntime {
    executable: PathBuf,
    version: String,
    source: &'static str,
}

#[derive(Debug, Deserialize)]
struct RcloneListItem {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Size")]
    size: i64,
    #[serde(rename = "ModTime")]
    modified_at: Option<String>,
    #[serde(rename = "ID")]
    id: Option<String>,
    #[serde(rename = "Hashes", default)]
    hashes: serde_json::Map<String, Value>,
}

pub fn status(app: &AppHandle) -> RcloneStatus {
    match locate_runtime(app) {
        Ok(runtime) => RcloneStatus {
            available: true,
            version: Some(runtime.version),
            required_version: REQUIRED_RCLONE_VERSION.to_string(),
            source: Some(runtime.source.to_string()),
            message: None,
        },
        Err(message) => RcloneStatus {
            available: false,
            version: None,
            required_version: REQUIRED_RCLONE_VERSION.to_string(),
            source: None,
            message: Some(message),
        },
    }
}

pub fn authorize_google_drive(app: &AppHandle) -> Result<String, String> {
    let runtime = locate_runtime(app)?;
    let mut command = clean_command(&runtime.executable);
    command.args([
        "authorize",
        "drive",
        "--drive-scope",
        DRIVE_SCOPE,
        "--config=",
    ]);

    let output = command
        .output()
        .map_err(|error| format!("Could not start Google Drive authorization: {error}"))?;
    ensure_success("Google Drive authorization", &output)?;
    parse_authorization_token(&output.stdout)
}

pub fn google_drive_userinfo(app: &AppHandle, token: &str) -> Result<Option<String>, String> {
    let runtime = locate_runtime(app)?;
    let output = drive_command(&runtime, token)
        .args(["config", "userinfo", ":drive:", "--json"])
        .output()
        .map_err(|error| format!("Could not query Google Drive account: {error}"))?;
    ensure_success("Google Drive account check", &output)?;

    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Google Drive account response was invalid: {error}"))?;
    Ok(extract_account_label(&value))
}

pub fn verify_google_drive(app: &AppHandle, token: &str) -> Result<(), String> {
    let runtime = locate_runtime(app)?;
    let output = drive_command(&runtime, token)
        .args(["about", ":drive:", "--json"])
        .output()
        .map_err(|error| format!("Could not verify Google Drive connection: {error}"))?;
    ensure_success("Google Drive connection check", &output)
}

pub fn list_google_drive_files(
    app: &AppHandle,
    token: &str,
    remote_path: &str,
) -> Result<Vec<RemoteFileObservation>, String> {
    let runtime = locate_runtime(app)?;
    let normalized_path = normalize_remote_path(remote_path)?;
    let target = if normalized_path.is_empty() {
        ":drive:".to_string()
    } else {
        format!(":drive:{normalized_path}")
    };
    let output = drive_command(&runtime, token)
        .args([
            "lsjson",
            target.as_str(),
            "--recursive",
            "--files-only",
            "--hash",
            "--hash-type",
            "MD5",
            "--no-mimetype",
            "--fast-list",
        ])
        .output()
        .map_err(|error| format!("Could not read Google Drive inventory: {error}"))?;

    if output.status.code() == Some(3) {
        return Ok(Vec::new());
    }
    ensure_success("Google Drive inventory", &output)?;

    let items: Vec<RcloneListItem> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Google Drive inventory response was invalid: {error}"))?;
    let mut seen_paths = HashSet::new();
    let mut observations = Vec::with_capacity(items.len());
    for item in items {
        if item.path.is_empty() {
            continue;
        }
        let relative_path = normalize_remote_path(&item.path)?;
        if !seen_paths.insert(relative_path.clone()) {
            return Err(format!(
                "Google Drive returned duplicate objects for {relative_path}. AtrisBridge blocks backup until duplicate Drive names are resolved."
            ));
        }
        observations.push(observation_from_item(item, relative_path)?);
    }
    Ok(observations)
}

pub fn stat_google_drive_file(
    app: &AppHandle,
    token: &str,
    remote_file_path: &str,
    relative_path: &str,
) -> Result<RemoteFileObservation, String> {
    try_stat_google_drive_file(app, token, remote_file_path, relative_path)?
        .ok_or_else(|| "Google Drive file was not found during safety verification.".to_string())
}

pub fn try_stat_google_drive_file(
    app: &AppHandle,
    token: &str,
    remote_file_path: &str,
    relative_path: &str,
) -> Result<Option<RemoteFileObservation>, String> {
    let runtime = locate_runtime(app)?;
    let normalized = normalize_remote_path(remote_file_path)?;
    if normalized.is_empty() {
        return Err("Remote file path cannot be empty.".into());
    }
    let target = format!(":drive:{normalized}");
    let output = drive_command(&runtime, token)
        .args([
            "lsjson",
            target.as_str(),
            "--stat",
            "--hash",
            "--hash-type",
            "MD5",
            "--no-mimetype",
        ])
        .output()
        .map_err(|error| format!("Could not inspect Google Drive file: {error}"))?;

    if matches!(output.status.code(), Some(3 | 4)) {
        return Ok(None);
    }
    ensure_success("Google Drive file preflight", &output)?;
    let item: RcloneListItem = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Google Drive file response was invalid: {error}"))?;
    observation_from_item(item, relative_path.to_string()).map(Some)
}

pub fn local_file_md5(app: &AppHandle, local_path: &Path) -> Result<String, String> {
    if !local_path.is_file() {
        return Err("Upload candidate is no longer a regular file.".into());
    }
    let runtime = locate_runtime(app)?;
    let output = clean_command(&runtime.executable)
        .arg("--config=")
        .args(["hashsum", "MD5"])
        .arg(local_path)
        .output()
        .map_err(|error| format!("Could not calculate local MD5 evidence: {error}"))?;
    ensure_success("Local MD5 calculation", &output)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let hash = stdout
        .lines()
        .find_map(|line| line.split_whitespace().next())
        .ok_or_else(|| "rclone did not return a local MD5 hash.".to_string())?
        .to_ascii_lowercase();
    if hash.len() != 32 || !hash.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("rclone returned an invalid local MD5 hash.".into());
    }
    Ok(hash)
}

pub fn upload_google_drive_file(
    app: &AppHandle,
    token: &str,
    local_path: &Path,
    remote_file_path: &str,
    create_only: bool,
) -> Result<RemoteFileObservation, String> {
    let (before_size, before_blake3) = scanner::fingerprint_file(local_path)?;
    let local_md5 = local_file_md5(app, local_path)?;
    let (after_hash_size, after_hash_blake3) = scanner::fingerprint_file(local_path)?;
    if before_size != after_hash_size || before_blake3 != after_hash_blake3 {
        return Err(
            "Local file changed while upload evidence was being prepared. Nothing was sent.".into(),
        );
    }

    let normalized = normalize_remote_path(remote_file_path)?;
    if normalized.is_empty() {
        return Err("Remote file path cannot be empty.".into());
    }

    if create_only && try_stat_google_drive_file(app, token, &normalized, &normalized)?.is_some() {
        return Err(
            "Remote path appeared immediately before upload. AtrisBridge blocked the create operation."
                .into(),
        );
    }

    let runtime = locate_runtime(app)?;
    let destination = format!(":drive:{normalized}");
    let mut command = drive_command(&runtime, token);
    command
        .arg("copyto")
        .arg(local_path)
        .arg(destination)
        .args(["--checksum", "--retries", "1", "--stats", "0"]);
    if create_only {
        command.arg("--immutable");
    }

    let output = command
        .output()
        .map_err(|error| format!("Could not start Google Drive upload: {error}"))?;
    let transfer_result = ensure_success("Google Drive upload", &output);

    let (after_size, after_blake3) = scanner::fingerprint_file(local_path)?;
    if before_size != after_size || before_blake3 != after_blake3 {
        return Err(
            "Local file changed while it was being uploaded. Remote content was not accepted as a synchronized baseline."
                .into(),
        );
    }

    let remote = try_stat_google_drive_file(app, token, &normalized, &normalized)?;
    let Some(remote) = remote else {
        return match transfer_result {
            Ok(()) => {
                Err("Upload returned success but the remote file could not be verified.".into())
            }
            Err(error) => Err(error),
        };
    };

    let remote_md5 = match (
        remote.checksum_type.as_deref(),
        remote.checksum.as_deref(),
    ) {
        (Some(kind), Some(hash)) if kind.eq_ignore_ascii_case("MD5") => hash,
        _ => {
            return Err(
                "Google Drive did not return MD5 evidence for the uploaded file; baseline was not accepted."
                    .into(),
            )
        }
    };
    if remote.size != before_size || !remote_md5.eq_ignore_ascii_case(&local_md5) {
        return Err(
            "Google Drive content evidence did not match the local file after upload; baseline was not accepted."
                .into(),
        );
    }

    // A transport error can occur after Drive has accepted the object. If exact local
    // size and MD5 are visible remotely, accepting this observation is safer than
    // blindly retrying and risking a duplicate Google Drive object.
    Ok(remote)
}

pub fn join_remote_path(root: &str, relative_path: &str) -> Result<String, String> {
    let root = normalize_remote_path(root)?;
    let relative = normalize_remote_path(relative_path)?;
    if root.is_empty() || relative.is_empty() {
        return Err("Remote backup path requires both a workspace folder and file path.".into());
    }
    normalize_remote_path(&format!("{root}/{relative}"))
}

pub fn normalize_remote_path(value: &str) -> Result<String, String> {
    let normalized = value.trim().replace('\\', "/");
    let normalized = normalized.trim_matches('/');
    if normalized.is_empty() {
        return Ok(String::new());
    }

    let mut segments = Vec::new();
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("Remote path contains an invalid segment.".into());
        }
        if segment.chars().any(|character| character.is_control()) {
            return Err("Remote path contains unsupported control characters.".into());
        }
        segments.push(segment);
    }
    Ok(segments.join("/"))
}

fn locate_runtime(app: &AppHandle) -> Result<RcloneRuntime, String> {
    let executable_name = if cfg!(target_os = "windows") {
        "rclone.exe"
    } else {
        "rclone"
    };

    let resource_path = app
        .path()
        .resource_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge resources: {error}"))?
        .join("rclone")
        .join(executable_name);
    if resource_path.is_file() {
        return validate_runtime(resource_path, "bundled");
    }

    #[cfg(debug_assertions)]
    {
        let development_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(executable_name);
        if development_path.is_file() {
            return validate_runtime(development_path, "development");
        }
    }

    Err(format!(
        "AtrisBridge requires its pinned rclone sidecar (v{REQUIRED_RCLONE_VERSION}). Run `npm run sidecar:prepare` for local development."
    ))
}

fn validate_runtime(executable: PathBuf, source: &'static str) -> Result<RcloneRuntime, String> {
    let output = clean_command(&executable)
        .arg("version")
        .output()
        .map_err(|error| format!("Could not execute AtrisBridge rclone sidecar: {error}"))?;
    ensure_success("rclone version check", &output)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("rclone v"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| "Could not determine rclone sidecar version.".to_string())?;

    if version != REQUIRED_RCLONE_VERSION {
        return Err(format!(
            "Unsupported rclone sidecar version v{version}. AtrisBridge pins v{REQUIRED_RCLONE_VERSION}."
        ));
    }

    Ok(RcloneRuntime {
        executable,
        version: version.to_string(),
        source,
    })
}

fn drive_command(runtime: &RcloneRuntime, token: &str) -> Command {
    let mut command = clean_command(&runtime.executable);
    command
        .arg("--config=")
        .env("RCLONE_DRIVE_TOKEN", token)
        .env("RCLONE_DRIVE_SCOPE", DRIVE_SCOPE)
        .env("RCLONE_DRIVE_SKIP_GDOCS", "true")
        .env("RCLONE_DRIVE_USE_TRASH", "true");
    command
}

fn clean_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    for key in RCLONE_ENV_KEYS {
        command.env_remove(key);
    }
    command
}

fn observation_from_item(
    item: RcloneListItem,
    relative_path: String,
) -> Result<RemoteFileObservation, String> {
    let (checksum_type, checksum) = item
        .hashes
        .iter()
        .find_map(|(kind, value)| {
            value
                .as_str()
                .map(|hash| (kind.clone(), hash.to_ascii_lowercase()))
        })
        .map(|(kind, hash)| (Some(kind), Some(hash)))
        .unwrap_or((None, None));
    let size = u64::try_from(item.size)
        .map_err(|_| format!("Remote file {} reported an invalid size.", item.path))?;

    Ok(RemoteFileObservation {
        relative_path,
        remote_id: item.id,
        size,
        modified_at: item.modified_at,
        checksum_type,
        checksum,
    })
}

fn ensure_success(action: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }

    let stderr = sanitize_error(&String::from_utf8_lossy(&output.stderr));
    if stderr.is_empty() {
        Err(format!("{action} failed with status {}.", output.status))
    } else {
        Err(format!("{action} failed: {stderr}"))
    }
}

fn sanitize_error(value: &str) -> String {
    value
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !lower.contains("access_token")
                && !lower.contains("refresh_token")
                && !lower.contains("rclone_drive_token")
        })
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn parse_authorization_token(stdout: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines().rev() {
        let candidate = line.trim();
        if !candidate.starts_with('{') || !candidate.ends_with('}') {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            if value.get("refresh_token").is_some() || value.get("access_token").is_some() {
                return Ok(candidate.to_string());
            }
        }
    }
    Err("Google authorization completed without a usable OAuth token.".into())
}

fn extract_account_label(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    for key in ["Email", "email", "User", "user", "Name", "name"] {
        if let Some(label) = object.get(key).and_then(Value::as_str) {
            if !label.trim().is_empty() {
                return Some(label.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_the_pinned_sidecar_version() {
        assert_eq!(REQUIRED_RCLONE_VERSION, "1.74.4");
    }

    #[test]
    fn normalizes_remote_paths_without_allowing_parent_segments() {
        assert_eq!(
            normalize_remote_path("/AtrisBridge\\Project/").expect("path"),
            "AtrisBridge/Project"
        );
        assert!(normalize_remote_path("AtrisBridge/../Other").is_err());
    }

    #[test]
    fn joins_workspace_and_file_paths_safely() {
        assert_eq!(
            join_remote_path("AtrisBridge/Project", "src/main.rs").expect("path"),
            "AtrisBridge/Project/src/main.rs"
        );
        assert!(join_remote_path("AtrisBridge/Project", "src/../secret.txt").is_err());
    }

    #[test]
    fn extracts_authorize_json_without_persisting_it() {
        let output = b"Paste this token:\n{\"access_token\":\"a\",\"refresh_token\":\"b\"}\n";
        let token = parse_authorization_token(output).expect("token");
        assert!(token.contains("refresh_token"));
    }
}
