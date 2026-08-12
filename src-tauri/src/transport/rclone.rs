use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::{
    encryption::{self, CryptTransportContext},
    models::{RcloneStatus, RemoteFileObservation},
    scanner,
};

pub const REQUIRED_RCLONE_VERSION: &str = "1.74.4";
const DRIVE_SCOPE: &str = "drive.file";
pub const CRYPT_CHECKSUM_TYPE: &str = "RCLONE_CRYPT_MD5";
const ENCRYPTION_SENTINEL: &str = ".atrisbridge-key-check";
const ENCRYPTION_SENTINEL_CONTENT: &[u8] = b"AtrisBridge encrypted workspace sentinel v1\n";
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
    "RCLONE_CRYPT_REMOTE",
    "RCLONE_CRYPT_PASSWORD",
    "RCLONE_CRYPT_PASSWORD2",
    "RCLONE_CRYPT_FILENAME_ENCRYPTION",
    "RCLONE_CRYPT_DIRECTORY_NAME_ENCRYPTION",
    "RCLONE_CRYPT_NO_DATA_ENCRYPTION",
    "RCLONE_CRYPT_STRICT_NAMES",
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

pub fn list_raw_google_drive_files(
    app: &AppHandle,
    token: &str,
    remote_path: &str,
) -> Result<Vec<RemoteFileObservation>, String> {
    list_google_drive_files_plain(app, token, remote_path)
}

pub fn list_google_drive_files(
    app: &AppHandle,
    token: &str,
    remote_path: &str,
) -> Result<Vec<RemoteFileObservation>, String> {
    if let Some(context) = encryption::transport_context_for_remote_path(app, remote_path)? {
        return list_encrypted_google_drive_files(app, token, &context);
    }
    list_google_drive_files_plain(app, token, remote_path)
}

fn list_google_drive_files_plain(
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
    if let Some(context) = encryption::transport_context_for_remote_path(app, remote_file_path)? {
        return try_stat_encrypted_google_drive_file(
            app,
            token,
            &context,
            remote_file_path,
            relative_path,
        );
    }

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
    if let Some(context) = encryption::transport_context_for_remote_path(app, remote_file_path)? {
        return upload_encrypted_google_drive_file(
            app,
            token,
            &context,
            local_path,
            remote_file_path,
            create_only,
        );
    }

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

pub fn download_google_drive_file_to_stage(
    app: &AppHandle,
    token: &str,
    remote_file_path: &str,
    destination: &Path,
) -> Result<(), String> {
    if destination.exists() {
        return Err("Restore staging path already exists.".into());
    }
    let normalized = normalize_remote_path(remote_file_path)?;
    if normalized.is_empty() {
        return Err("Remote restore path cannot be empty.".into());
    }
    let runtime = locate_runtime(app)?;
    let mut command =
        if let Some(context) = encryption::transport_context_for_remote_path(app, &normalized)? {
            let relative = encrypted_relative_path(&context, &normalized)?;
            if relative.is_empty() {
                return Err("Encrypted restore path cannot point to the workspace root.".into());
            }
            let mut command = crypt_command(&runtime, token, &context)?;
            command
                .arg("copyto")
                .arg(format!(":crypt:{relative}"))
                .arg(destination)
                .args(["--immutable", "--retries", "1", "--stats", "0"]);
            command
        } else {
            let mut command = drive_command(&runtime, token);
            command
                .arg("copyto")
                .arg(format!(":drive:{normalized}"))
                .arg(destination)
                .args([
                    "--checksum",
                    "--immutable",
                    "--retries",
                    "1",
                    "--stats",
                    "0",
                ]);
            command
        };
    let output = command
        .output()
        .map_err(|error| format!("Could not start Google Drive restore download: {error}"))?;
    ensure_success("Google Drive restore download", &output)
}

pub fn encrypted_namespace_exists(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
) -> Result<bool, String> {
    Ok(!raw_encrypted_namespace_items(app, token, context)?.is_empty())
}

pub fn write_encryption_sentinel(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
) -> Result<(), String> {
    if !raw_encrypted_namespace_items(app, token, context)?.is_empty() {
        return Err(
            "Encrypted namespace is not empty. AtrisBridge refuses to initialize a new key over existing remote data."
                .into(),
        );
    }
    let runtime = locate_runtime(app)?;
    let mut command = crypt_command(&runtime, token, context)?;
    command
        .arg("rcat")
        .arg(format!(":crypt:{ENCRYPTION_SENTINEL}"))
        .args(["--retries", "1", "--stats", "0"]);
    let output = run_with_stdin(
        command,
        ENCRYPTION_SENTINEL_CONTENT,
        "encryption sentinel upload",
    )?;
    ensure_success("Encryption sentinel upload", &output)
}

pub fn verify_encryption_sentinel(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
) -> Result<(), String> {
    let runtime = locate_runtime(app)?;
    let output = crypt_command(&runtime, token, context)?
        .arg("cat")
        .arg(format!(":crypt:{ENCRYPTION_SENTINEL}"))
        .output()
        .map_err(|error| format!("Could not read encrypted workspace sentinel: {error}"))?;
    ensure_success("Encrypted workspace key verification", &output)?;
    if output.stdout != ENCRYPTION_SENTINEL_CONTENT {
        return Err(
            "Recovery key did not decrypt the AtrisBridge workspace sentinel exactly.".into(),
        );
    }
    Ok(())
}

fn list_encrypted_google_drive_files(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
) -> Result<Vec<RemoteFileObservation>, String> {
    // Verify the underlying namespace and sentinel first. Missing/corrupt ciphertext must never
    // be interpreted as a clean empty remote inventory because that could create delete intent.
    let raw_items = raw_encrypted_namespace_items(app, token, context)?;
    let sentinel_raw = format!("{ENCRYPTION_SENTINEL}.bin");
    let mut sentinel_seen = false;
    let mut raw_by_logical = HashMap::new();
    for item in raw_items {
        let raw_path = normalize_remote_path(&item.path)?;
        if raw_path == sentinel_raw {
            sentinel_seen = true;
            continue;
        }
        let logical_path = raw_path.strip_suffix(".bin").ok_or_else(|| {
            format!("Encrypted namespace contains a non-crypt object at {raw_path}.")
        })?;
        let logical_path = normalize_remote_path(logical_path)?;
        if raw_by_logical.insert(logical_path.clone(), item).is_some() {
            return Err(format!("Duplicate ciphertext exists for {logical_path}."));
        }
    }
    if !sentinel_seen {
        return Err(
            "Encrypted workspace sentinel is missing. AtrisBridge blocked remote reconciliation instead of treating the namespace as deleted."
                .into(),
        );
    }

    let runtime = locate_runtime(app)?;
    let logical_output = crypt_command(&runtime, token, context)?
        .args([
            "lsjson",
            ":crypt:",
            "--recursive",
            "--files-only",
            "--no-mimetype",
            "--fast-list",
        ])
        .output()
        .map_err(|error| format!("Could not read encrypted Google Drive inventory: {error}"))?;
    if logical_output.status.code() == Some(3) {
        return Err("Encrypted namespace exists but could not be decrypted/listed. AtrisBridge blocked reconciliation.".into());
    }
    ensure_success("Encrypted Google Drive inventory", &logical_output)?;
    let logical_items: Vec<RcloneListItem> = serde_json::from_slice(&logical_output.stdout)
        .map_err(|error| {
            format!("Encrypted Google Drive inventory response was invalid: {error}")
        })?;

    let mut seen_paths = HashSet::new();
    let mut observations = Vec::new();
    for logical in logical_items {
        let relative_path = normalize_remote_path(&logical.path)?;
        if relative_path == ENCRYPTION_SENTINEL {
            continue;
        }
        if !seen_paths.insert(relative_path.clone()) {
            return Err(format!(
                "Encrypted Google Drive returned duplicate logical path {relative_path}."
            ));
        }
        let raw = raw_by_logical.remove(&relative_path).ok_or_else(|| {
            format!("Ciphertext evidence is missing for encrypted logical path {relative_path}.")
        })?;
        observations.push(encrypted_observation(logical, raw, relative_path)?);
    }
    if let Some(unmapped) = raw_by_logical.keys().next() {
        return Err(format!(
            "Ciphertext object {unmapped} could not be mapped to an authenticated logical file."
        ));
    }
    Ok(observations)
}

fn try_stat_encrypted_google_drive_file(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
    remote_file_path: &str,
    relative_path: &str,
) -> Result<Option<RemoteFileObservation>, String> {
    let logical_relative = encrypted_relative_path(context, remote_file_path)?;
    if logical_relative.is_empty() {
        return Err("Encrypted file path cannot be the workspace root.".into());
    }
    let runtime = locate_runtime(app)?;
    if try_stat_raw_ciphertext(&runtime, token, context, ENCRYPTION_SENTINEL)?.is_none() {
        return Err(
            "Encrypted workspace sentinel is missing during targeted preflight. AtrisBridge blocked deletion inference."
      .into(),
        );
    }
    let logical_target = format!(":crypt:{logical_relative}");
    let logical_output = crypt_command(&runtime, token, context)?
        .args(["lsjson", logical_target.as_str(), "--stat", "--no-mimetype"])
        .output()
        .map_err(|error| format!("Could not inspect encrypted Google Drive file: {error}"))?;
    if matches!(logical_output.status.code(), Some(3) | Some(4)) {
        let raw = try_stat_raw_ciphertext(&runtime, token, context, &logical_relative)?;
        if raw.is_some() {
            return Err(
                "Ciphertext still exists for a logical path that crypt could not read. AtrisBridge blocked deletion inference."
                    .into(),
            );
        }
        return Ok(None);
    }
    ensure_success("Encrypted Google Drive file preflight", &logical_output)?;
    let logical: RcloneListItem = serde_json::from_slice(&logical_output.stdout)
        .map_err(|error| format!("Encrypted logical file response was invalid: {error}"))?;
    let raw =
        try_stat_raw_ciphertext(&runtime, token, context, &logical_relative)?.ok_or_else(|| {
            "Encrypted logical file exists but its ciphertext object is missing.".to_string()
        })?;
    encrypted_observation(logical, raw, relative_path.to_string()).map(Some)
}

fn upload_encrypted_google_drive_file(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
    local_path: &Path,
    remote_file_path: &str,
    create_only: bool,
) -> Result<RemoteFileObservation, String> {
    let (before_size, before_blake3) = scanner::fingerprint_file(local_path)?;
    let logical_relative = encrypted_relative_path(context, remote_file_path)?;
    if logical_relative.is_empty() {
        return Err("Encrypted upload path cannot be the workspace root.".into());
    }
    if create_only
        && try_stat_encrypted_google_drive_file(
            app,
            token,
            context,
            remote_file_path,
            &logical_relative,
        )?
        .is_some()
    {
        return Err("Encrypted remote path appeared immediately before upload.".into());
    }
    let runtime = locate_runtime(app)?;
    let mut command = crypt_command(&runtime, token, context)?;
    command
        .arg("copyto")
        .arg(local_path)
        .arg(format!(":crypt:{logical_relative}"))
        .args(["--retries", "1", "--stats", "0"]);
    if create_only {
        command.arg("--immutable");
    }
    let output = command
        .output()
        .map_err(|error| format!("Could not start encrypted Google Drive upload: {error}"))?;
    // Ciphertext uses a random nonce. On an ambiguous process failure AtrisBridge cannot
    // reconstruct the expected ciphertext checksum locally, so fail closed instead of retrying.
    ensure_success("Encrypted Google Drive upload", &output)?;
    let (after_size, after_blake3) = scanner::fingerprint_file(local_path)?;
    if before_size != after_size || before_blake3 != after_blake3 {
        return Err("Local file changed while it was being encrypted/uploaded.".into());
    }
    let observation = try_stat_encrypted_google_drive_file(
        app,
        token,
        context,
        remote_file_path,
        &logical_relative,
    )?
    .ok_or_else(|| "Encrypted upload succeeded but ciphertext evidence is missing.".to_string())?;
    if observation.size != before_size
        || observation.remote_id.is_none()
        || observation.checksum_type.as_deref() != Some(CRYPT_CHECKSUM_TYPE)
        || observation.checksum.is_none()
    {
        return Err("Encrypted upload completed without complete ciphertext evidence.".into());
    }
    Ok(observation)
}

fn encrypted_observation(
    logical: RcloneListItem,
    raw: RcloneListItem,
    relative_path: String,
) -> Result<RemoteFileObservation, String> {
    let size = u64::try_from(logical.size)
        .map_err(|_| format!("Encrypted file {relative_path} reported a negative size."))?;
    let remote_id = raw.id.ok_or_else(|| {
        format!("Google Drive did not provide a ciphertext ID for {relative_path}.")
    })?;
    let checksum = raw
        .hashes
        .iter()
        .find_map(|(kind, value)| {
            kind.eq_ignore_ascii_case("MD5")
                .then(|| value.as_str().map(str::to_ascii_lowercase))
                .flatten()
        })
        .ok_or_else(|| {
            format!("Google Drive did not provide ciphertext MD5 for {relative_path}.")
        })?;
    if checksum.len() != 32
        || !checksum
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!(
            "Google Drive returned invalid ciphertext MD5 for {relative_path}."
        ));
    }
    Ok(RemoteFileObservation {
        relative_path,
        remote_id: Some(remote_id),
        size,
        modified_at: logical.modified_at.or(raw.modified_at),
        checksum_type: Some(CRYPT_CHECKSUM_TYPE.into()),
        checksum: Some(checksum),
    })
}

fn raw_encrypted_namespace_items(
    app: &AppHandle,
    token: &str,
    context: &CryptTransportContext,
) -> Result<Vec<RcloneListItem>, String> {
    let runtime = locate_runtime(app)?;
    let target = format!(":drive:{}", context.remote_namespace);
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
        .map_err(|error| format!("Could not inspect encrypted remote namespace: {error}"))?;
    if output.status.code() == Some(3) {
        return Ok(Vec::new());
    }
    ensure_success("Encrypted remote namespace inspection", &output)?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Encrypted remote namespace response was invalid: {error}"))
}

fn try_stat_raw_ciphertext(
    runtime: &RcloneRuntime,
    token: &str,
    context: &CryptTransportContext,
    logical_relative: &str,
) -> Result<Option<RcloneListItem>, String> {
    let raw_path = encrypted_underlying_path(context, logical_relative)?;
    let raw_target = format!(":drive:{raw_path}");
    let output = drive_command(runtime, token)
        .args([
            "lsjson",
            raw_target.as_str(),
            "--stat",
            "--hash",
            "--hash-type",
            "MD5",
            "--no-mimetype",
        ])
        .output()
        .map_err(|error| format!("Could not inspect encrypted ciphertext evidence: {error}"))?;
    if matches!(output.status.code(), Some(3) | Some(4)) {
        return Ok(None);
    }
    ensure_success("Encrypted ciphertext preflight", &output)?;
    serde_json::from_slice(&output.stdout)
        .map(Some)
        .map_err(|error| format!("Encrypted ciphertext response was invalid: {error}"))
}

fn encrypted_underlying_path(
    context: &CryptTransportContext,
    logical_relative: &str,
) -> Result<String, String> {
    let relative = normalize_remote_path(logical_relative)?;
    if relative.is_empty() {
        return Err("Encrypted file path cannot be empty.".into());
    }
    join_remote_path(&context.remote_namespace, &format!("{relative}.bin"))
}

fn encrypted_relative_path(
    context: &CryptTransportContext,
    requested_remote_path: &str,
) -> Result<String, String> {
    let requested = normalize_remote_path(requested_remote_path)?;
    if requested == context.binding_root {
        return Ok(String::new());
    }
    let prefix = format!("{}/", context.binding_root);
    let relative = requested.strip_prefix(&prefix).ok_or_else(|| {
        "Encrypted transport path is outside the workspace's pinned remote root.".to_string()
    })?;
    normalize_remote_path(relative)
}

fn crypt_command(
    runtime: &RcloneRuntime,
    token: &str,
    context: &CryptTransportContext,
) -> Result<Command, String> {
    let password = obscure_secret(runtime, &context.password)?;
    let password2 = obscure_secret(runtime, &context.password2)?;
    let mut command = clean_command(&runtime.executable);
    command
        .arg("--config=")
        .env("RCLONE_DRIVE_TOKEN", token)
        .env("RCLONE_DRIVE_SCOPE", DRIVE_SCOPE)
        .env("RCLONE_DRIVE_SKIP_GDOCS", "true")
        .env("RCLONE_DRIVE_USE_TRASH", "true")
        .env(
            "RCLONE_CRYPT_REMOTE",
            format!(":drive:{}", context.remote_namespace),
        )
        .env("RCLONE_CRYPT_PASSWORD", password)
        .env("RCLONE_CRYPT_PASSWORD2", password2)
        .env("RCLONE_CRYPT_FILENAME_ENCRYPTION", "off")
        .env("RCLONE_CRYPT_DIRECTORY_NAME_ENCRYPTION", "false")
        .env("RCLONE_CRYPT_NO_DATA_ENCRYPTION", "false")
        .env("RCLONE_CRYPT_STRICT_NAMES", "true");
    Ok(command)
}

fn obscure_secret(runtime: &RcloneRuntime, secret: &str) -> Result<String, String> {
    let mut command = clean_command(&runtime.executable);
    command
        .args(["obscure", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_with_stdin(command, secret.as_bytes(), "rclone secret obscuring")?;
    ensure_success("rclone secret obscuring", &output)?;
    let value = String::from_utf8(output.stdout)
        .map_err(|_| "rclone returned non-UTF-8 obscured secret data.".to_string())?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("rclone returned an empty obscured secret.".into());
    }
    Ok(value)
}

fn run_with_stdin(mut command: Command, input: &[u8], action: &str) -> Result<Output, String> {
    command.stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start {action}: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("Could not open stdin for {action}."))?;
    stdin
        .write_all(input)
        .map_err(|error| format!("Could not write stdin for {action}: {error}"))?;
    if !input.ends_with(b"\n") {
        stdin
            .write_all(b"\n")
            .map_err(|error| format!("Could not terminate stdin for {action}: {error}"))?;
    }
    drop(stdin);
    child
        .wait_with_output()
        .map_err(|error| format!("Could not wait for {action}: {error}"))
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
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
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
