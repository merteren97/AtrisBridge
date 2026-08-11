use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{
    database::open_database,
    models::{EncryptionEnableResult, WorkspaceEncryptionStatus},
    provider_sessions::ProviderSessionStore,
    provider_storage, secure_store,
    transport::rclone,
};

const MODE_CONTENT: &str = "content";
const CRYPT_NAMESPACE: &str = ".atrisbridge-crypt-v1";
const RECOVERY_PREFIX: &str = "AB1-";

pub struct CryptTransportContext {
    pub binding_root: String,
    pub remote_namespace: String,
    pub password: String,
    pub password2: String,
}

#[derive(Clone)]
struct EncryptionRecord {
    workspace_id: String,
    key_ref: String,
    bound_remote_path: String,
    remote_namespace: String,
    enabled_at: String,
    verified_at: String,
}

#[tauri::command]
pub fn workspace_encryption_status(
    app: AppHandle,
    id: String,
) -> Result<WorkspaceEncryptionStatus, String> {
    status(&app, &id)
}

#[tauri::command]
pub async fn enable_workspace_encryption(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<EncryptionEnableResult, String> {
    ensure_attach_is_safe(&app, &id)?;
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("Phase 7 client-side encryption currently supports Google Drive only.".into());
    }
    ensure_managed_root(&binding.remote_path)?;
    if load_record(&app, &id)?.is_some() {
        return Err("Client-side encryption is already enabled for this workspace.".into());
    }
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive credential is unavailable. Reconnect before enabling encryption.".to_string()
    })?;

    let account_identity = provider.account_label.as_deref().ok_or_else(|| {
        "Google Drive account identity is unavailable; reconnect before managing encryption."
            .to_string()
    })?;
    let key_ref = secure_store::workspace_key_reference(account_identity, &binding.remote_path)?;
    if let Some(existing) = secure_store::load_workspace_master_key(&key_ref)? {
        let master = parse_recovery_key(&existing)?;
        let context = context_from_master(&binding.remote_path, &master)?;
        match rclone::verify_encryption_sentinel(&app, &token, &context) {
            Ok(()) => {}
            Err(verify_error) => {
                let live_files =
                    rclone::list_raw_google_drive_files(&app, &token, &binding.remote_path)?;
                let namespace_has_content =
                    rclone::encrypted_namespace_exists(&app, &token, &context)?;
                if !live_files.is_empty() || namespace_has_content {
                    return Err(format!(
                        "A protected encryption key exists, but its remote sentinel could not be verified: {verify_error}"
                    ));
                }
                rclone::write_encryption_sentinel(&app, &token, &context)?;
                rclone::verify_encryption_sentinel(&app, &token, &context)?;
            }
        }
        save_record(
            &app,
            &id,
            &key_ref,
            &binding.remote_path,
            &context.remote_namespace,
        )?;
        return Ok(EncryptionEnableResult {
            status: status(&app, &id)?,
            recovery_key: existing,
        });
    }

    let live_files = rclone::list_raw_google_drive_files(&app, &token, &binding.remote_path)?;
    if !live_files.is_empty() {
        return Err(
            "Client-side encryption can only be enabled on an empty managed remote root. Existing plaintext or encrypted data was found; Phase 7 does not perform an in-place migration."
                .into(),
        );
    }

    let master = generate_master_key();
    let recovery_key = format_recovery_key(&master);
    let context = context_from_master(&binding.remote_path, &master)?;
    secure_store::store_workspace_master_key(&key_ref, &recovery_key)?;

    let setup_result = (|| -> Result<(), String> {
        rclone::write_encryption_sentinel(&app, &token, &context)?;
        rclone::verify_encryption_sentinel(&app, &token, &context)?;
        save_record(
            &app,
            &id,
            &key_ref,
            &binding.remote_path,
            &context.remote_namespace,
        )
    })();
    if let Err(error) = setup_result {
        // Keep the OS-vault key when a provider-side write may already have happened.
        // A retry can verify the sentinel and finish attachment without inventing a new key.
        return Err(format!(
            "Encryption setup did not complete: {error}. The generated key remains protected in the OS credential vault so the operation can be retried safely."
        ));
    }

    Ok(EncryptionEnableResult {
        status: status(&app, &id)?,
        recovery_key,
    })
}

#[tauri::command]
pub fn export_workspace_recovery_key(app: AppHandle, id: String) -> Result<String, String> {
    let record = load_record(&app, &id)?
        .ok_or_else(|| "Client-side encryption is not enabled for this workspace.".to_string())?;
    secure_store::load_workspace_master_key(&record.key_ref)?.ok_or_else(|| {
        "The workspace encryption key is missing from the OS credential vault.".to_string()
    })
}

#[tauri::command]
pub async fn import_workspace_recovery_key(
    app: AppHandle,
    id: String,
    recovery_key: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<WorkspaceEncryptionStatus, String> {
    let existing_record = load_record(&app, &id)?;
    if existing_record.is_some() {
        ensure_no_active_transfer_plans(&app, &id)?;
    } else {
        ensure_attach_is_safe(&app, &id)?;
    }
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("Phase 7 client-side encryption currently supports Google Drive only.".into());
    }
    ensure_managed_root(&binding.remote_path)?;
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive credential is unavailable. Reconnect before importing an encryption key."
            .to_string()
    })?;
    let master = parse_recovery_key(&recovery_key)?;
    let normalized_key = format_recovery_key(&master);
    let context = context_from_master(&binding.remote_path, &master)?;
    rclone::verify_encryption_sentinel(&app, &token, &context)?;

    let account_identity = provider.account_label.as_deref().ok_or_else(|| {
        "Google Drive account identity is unavailable; reconnect before managing encryption."
            .to_string()
    })?;
    let key_ref = secure_store::workspace_key_reference(account_identity, &binding.remote_path)?;
    if let Some(existing) = existing_record {
        if existing.bound_remote_path != binding.remote_path || existing.key_ref != key_ref {
            return Err(
                "This workspace is already attached to a different encrypted remote namespace."
                    .into(),
            );
        }
    }
    secure_store::store_workspace_master_key(&key_ref, &normalized_key)?;
    save_record(
        &app,
        &id,
        &key_ref,
        &binding.remote_path,
        &context.remote_namespace,
    )?;
    status(&app, &id)
}

pub fn status(app: &AppHandle, workspace_id: &str) -> Result<WorkspaceEncryptionStatus, String> {
    let record = load_record(app, workspace_id)?;
    let Some(record) = record else {
        return Ok(WorkspaceEncryptionStatus {
            workspace_id: workspace_id.to_string(),
            mode: "disabled".into(),
            key_available: false,
            filename_encrypted: false,
            remote_namespace: None,
            enabled_at: None,
            verified_at: None,
        });
    };
    let key_available = secure_store::load_workspace_master_key(&record.key_ref)?.is_some();
    Ok(WorkspaceEncryptionStatus {
        workspace_id: record.workspace_id,
        mode: MODE_CONTENT.into(),
        key_available,
        filename_encrypted: false,
        remote_namespace: Some(record.remote_namespace),
        enabled_at: Some(record.enabled_at),
        verified_at: Some(record.verified_at),
    })
}

pub fn ensure_binding_change_allowed(
    app: &AppHandle,
    workspace_id: &str,
    new_remote_path: &str,
) -> Result<(), String> {
    if let Some(record) = load_record(app, workspace_id)? {
        if record.bound_remote_path != new_remote_path {
            return Err(
                "This encrypted workspace is pinned to its current managed remote root. Phase 7 does not move or re-encrypt existing ciphertext automatically."
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn transport_context_for_remote_path(
    app: &AppHandle,
    requested_remote_path: &str,
) -> Result<Option<CryptTransportContext>, String> {
    let requested = rclone::normalize_remote_path(requested_remote_path)?;
    let connection = open_encryption_database(app)?;
    let mut statement = connection
        .prepare(
  "SELECT workspace_id, key_ref, bound_remote_path, remote_namespace, enabled_at, verified_at
   FROM workspace_encryption
   WHERE mode = 'content'
   ORDER BY length(bound_remote_path) DESC",
        )
        .map_err(|error| format!("Could not prepare workspace encryption routing: {error}"))?;
    let rows = statement
        .query_map([], record_from_row)
        .map_err(|error| format!("Could not query workspace encryption routing: {error}"))?;
    for row in rows {
        let record =
            row.map_err(|error| format!("Could not read workspace encryption routing: {error}"))?;
        let root = rclone::normalize_remote_path(&record.bound_remote_path)?;
        let prefix = format!("{root}/");
        if requested == root || requested.starts_with(&prefix) {
            return context_from_record(&record).map(Some);
        }
    }
    Ok(None)
}

pub fn transport_context_for_workspace(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<Option<CryptTransportContext>, String> {
    load_record(app, workspace_id)?
        .map(|record| context_from_record(&record))
        .transpose()
}

fn context_from_record(record: &EncryptionRecord) -> Result<CryptTransportContext, String> {
    let recovery_key = secure_store::load_workspace_master_key(&record.key_ref)?.ok_or_else(|| {
        "Client-side encryption is enabled, but its recovery key is missing from the OS credential vault. Import the recovery key before accessing this workspace."
            .to_string()
    })?;
    let master = parse_recovery_key(&recovery_key)?;
    let context = context_from_master(&record.bound_remote_path, &master)?;
    if context.remote_namespace != record.remote_namespace {
        return Err("Encrypted remote namespace metadata is inconsistent.".into());
    }
    Ok(context)
}

fn context_from_master(
    binding_root: &str,
    master: &[u8; 32],
) -> Result<CryptTransportContext, String> {
    let normalized = rclone::normalize_remote_path(binding_root)?;
    ensure_managed_root(&normalized)?;
    let remote_namespace =
        rclone::normalize_remote_path(&format!("{normalized}/{CRYPT_NAMESPACE}"))?;
    let password = blake3::derive_key("AtrisBridge rclone crypt password v1", master);
    let password2 = blake3::derive_key("AtrisBridge rclone crypt salt v1", master);
    Ok(CryptTransportContext {
        binding_root: normalized,
        remote_namespace,
        password: hex_encode(&password),
        password2: hex_encode(&password2),
    })
}

fn ensure_attach_is_safe(app: &AppHandle, workspace_id: &str) -> Result<(), String> {
    ensure_no_active_transfer_plans(app, workspace_id)?;
    let connection = open_encryption_database(app)?;
    let synchronized: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM file_entries
   WHERE workspace_id = ?1
     AND (last_synced_hash IS NOT NULL
OR last_synced_remote_checksum_type IS NOT NULL
OR last_synced_remote_checksum IS NOT NULL)",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect synchronized encryption baseline: {error}"))?;
    if synchronized > 0 {
        return Err(
  "Encryption mode cannot be attached after a plaintext synchronized baseline exists. Phase 7 intentionally avoids destructive in-place migration."
      .into(),
        );
    }
    Ok(())
}

fn ensure_no_active_transfer_plans(app: &AppHandle, workspace_id: &str) -> Result<(), String> {
    let connection = open_encryption_database(app)?;
    for table in ["backup_plans", "restore_plans", "sync_plans"] {
        if !table_exists(&connection, table)? {
            continue;
        }
        let sql = format!(
  "SELECT COUNT(*) FROM {table} WHERE workspace_id = ?1 AND status IN ('ready','running')"
        );
        let active: i64 = connection
            .query_row(&sql, params![workspace_id], |row| row.get(0))
            .map_err(|error| format!("Could not inspect active transfer plans: {error}"))?;
        if active > 0 {
            return Err(
                "Finish or retire the current transfer plan before changing workspace encryption."
                    .into(),
            );
        }
    }
    Ok(())
}

fn load_record(app: &AppHandle, workspace_id: &str) -> Result<Option<EncryptionRecord>, String> {
    let connection = open_encryption_database(app)?;
    connection
        .query_row(
            "SELECT workspace_id, key_ref, bound_remote_path, remote_namespace, enabled_at, verified_at
             FROM workspace_encryption WHERE workspace_id = ?1",
            params![workspace_id],
            record_from_row,
        )
        .optional()
        .map_err(|error| format!("Could not read workspace encryption metadata: {error}"))
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncryptionRecord> {
    Ok(EncryptionRecord {
        workspace_id: row.get(0)?,
        key_ref: row.get(1)?,
        bound_remote_path: row.get(2)?,
        remote_namespace: row.get(3)?,
        enabled_at: row.get(4)?,
        verified_at: row.get(5)?,
    })
}

fn save_record(
    app: &AppHandle,
    workspace_id: &str,
    key_ref: &str,
    bound_remote_path: &str,
    remote_namespace: &str,
) -> Result<(), String> {
    let connection = open_encryption_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO workspace_encryption (
                workspace_id, mode, key_ref, bound_remote_path, remote_namespace,
                key_version, filename_encryption, enabled_at, verified_at
             ) VALUES (?1, 'content', ?2, ?3, ?4, 1, 'off', ?5, ?5)
             ON CONFLICT(workspace_id) DO UPDATE SET
                mode = excluded.mode,
                key_ref = excluded.key_ref,
                bound_remote_path = excluded.bound_remote_path,
                remote_namespace = excluded.remote_namespace,
                key_version = excluded.key_version,
                filename_encryption = excluded.filename_encryption,
                verified_at = excluded.verified_at",
            params![
                workspace_id,
                key_ref,
                bound_remote_path,
                remote_namespace,
                now
            ],
        )
        .map_err(|error| format!("Could not save workspace encryption metadata: {error}"))?;
    Ok(())
}

fn open_encryption_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_encryption_schema(&connection)?;
    Ok(connection)
}

fn ensure_encryption_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_encryption (
                workspace_id TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('content')),
                key_ref TEXT NOT NULL,
                bound_remote_path TEXT NOT NULL,
                remote_namespace TEXT NOT NULL,
                key_version INTEGER NOT NULL CHECK(key_version = 1),
                filename_encryption TEXT NOT NULL CHECK(filename_encryption = 'off'),
                enabled_at TEXT NOT NULL,
                verified_at TEXT NOT NULL,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_encryption_remote_namespace
                 ON workspace_encryption(remote_namespace);",
        )
        .map_err(|error| format!("Could not initialize workspace encryption metadata: {error}"))
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|error| format!("Could not inspect encryption database schema: {error}"))
}

fn generate_master_key() -> [u8; 32] {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut master = [0_u8; 32];
    master[..16].copy_from_slice(first.as_bytes());
    master[16..].copy_from_slice(second.as_bytes());
    master
}

fn format_recovery_key(master: &[u8; 32]) -> String {
    format!("{RECOVERY_PREFIX}{}", hex_encode(master))
}

fn parse_recovery_key(value: &str) -> Result<[u8; 32], String> {
    let trimmed = value.trim();
    let payload = trimmed
        .strip_prefix(RECOVERY_PREFIX)
        .or_else(|| trimmed.strip_prefix("ab1-"))
        .ok_or_else(|| "Recovery key must start with AB1-.".to_string())?;
    if payload.len() != 64
        || !payload
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("Recovery key payload must contain exactly 64 hexadecimal characters.".into());
    }
    let mut output = [0_u8; 32];
    for (index, chunk) in payload.as_bytes().chunks_exact(2).enumerate() {
        let text =
            std::str::from_utf8(chunk).map_err(|_| "Recovery key is invalid.".to_string())?;
        output[index] = u8::from_str_radix(text, 16)
            .map_err(|_| "Recovery key contains invalid hexadecimal data.".to_string())?;
    }
    Ok(output)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn ensure_managed_root(remote_path: &str) -> Result<(), String> {
    let normalized = rclone::normalize_remote_path(remote_path)?;
    if normalized.starts_with("AtrisBridge/") {
        Ok(())
    } else {
        Err("Encrypted workspaces must remain under an AtrisBridge-managed remote root.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_key_round_trip_preserves_master_key() {
        let master = [0x5a; 32];
        let encoded = format_recovery_key(&master);
        assert_eq!(parse_recovery_key(&encoded).expect("parse"), master);
    }

    #[test]
    fn recovery_key_rejects_wrong_version_or_length() {
        assert!(parse_recovery_key("AB2-0011").is_err());
        assert!(parse_recovery_key("AB1-0011").is_err());
    }

    #[test]
    fn derived_crypt_credentials_are_domain_separated() {
        let master = [7_u8; 32];
        let context = context_from_master("AtrisBridge/Project", &master).expect("context");
        assert_ne!(context.password, context.password2);
        assert_eq!(context.password.len(), 64);
        assert_eq!(context.password2.len(), 64);
        assert!(context.remote_namespace.ends_with(CRYPT_NAMESPACE));
    }
}
