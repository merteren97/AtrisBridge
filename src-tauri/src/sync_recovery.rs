use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{database::open_database, durable_fs, scanner, storage::find_workspace};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRecoveryEntry {
    pub id: String,
    pub workspace_id: String,
    pub relative_path: String,
    pub size: u64,
    pub created_at: String,
    pub restored_at: Option<String>,
}

#[derive(Debug)]
struct RecoveryRecord {
    public: SyncRecoveryEntry,
    recovery_path: String,
    original_hash: String,
}

#[tauri::command]
pub fn list_sync_recoveries(app: AppHandle, id: String) -> Result<Vec<SyncRecoveryEntry>, String> {
    let connection = open_database(&app)?;
    if !table_exists(&connection, "sync_recovery_entries")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT id, workspace_id, relative_path, original_size, created_at, restored_at
             FROM sync_recovery_entries
             WHERE workspace_id = ?1
             ORDER BY created_at DESC
             LIMIT 50",
        )
        .map_err(|error| format!("Could not prepare recovery-copy query: {error}"))?;
    let rows = statement
        .query_map(params![id], recovery_public_from_row)
        .map_err(|error| format!("Could not query recovery copies: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read recovery copies: {error}"))
}

#[tauri::command]
pub async fn restore_sync_recovery(
    app: AppHandle,
    recovery_id: String,
) -> Result<SyncRecoveryEntry, String> {
    tauri::async_runtime::spawn_blocking(move || restore_sync_recovery_blocking(&app, &recovery_id))
        .await
        .map_err(|error| format!("Recovery restore worker failed: {error}"))?
}

fn restore_sync_recovery_blocking(
    app: &AppHandle,
    recovery_id: &str,
) -> Result<SyncRecoveryEntry, String> {
    let connection = open_database(app)?;
    if !table_exists(&connection, "sync_recovery_entries")? {
        return Err("No Phase 6 recovery journal exists yet.".into());
    }
    let record = connection
        .query_row(
            "SELECT id, workspace_id, relative_path, recovery_path,
                    original_hash, original_size, created_at, restored_at
             FROM sync_recovery_entries
             WHERE id = ?1",
            params![recovery_id],
            recovery_record_from_row,
        )
        .optional()
        .map_err(|error| format!("Could not read recovery copy metadata: {error}"))?
        .ok_or_else(|| "Recovery copy was not found.".to_string())?;
    if record.public.restored_at.is_some() {
        return Err("This recovery copy has already been restored locally.".into());
    }
    ensure_no_transfer_running(&connection, &record.public.workspace_id)?;

    let workspace = find_workspace(app, &record.public.workspace_id)?;
    if scanner::is_path_ignored_for_sync(
        Path::new(&workspace.local_path),
        &record.public.relative_path,
    )? {
        return Err(
            "Recovery path is excluded by AtrisBridge safety or .atrisbridgeignore rules.".into(),
        );
    }
    ensure_recovery_target_is_journal_safe(
        &connection,
        &record.public.workspace_id,
        &record.public.relative_path,
    )?;
    drop(connection);

    let recovery_source = validate_recovery_source(app, &record)?;
    let target = resolve_absent_target(&workspace.local_path, &record.public.relative_path, true)?;
    let stage = recovery_stage_path(&target, &record.public.id)?;
    ensure_absent(&stage, "recovery staging artifact")?;

    if let Err(error) = durable_fs::copy_new_file(&recovery_source, &stage) {
        return Err(format!("Could not durably stage recovery copy: {error}"));
    }
    if !file_matches(&stage, &record.original_hash, record.public.size)? {
        remove_regular_file_best_effort(&stage);
        return Err(
            "Staged recovery content did not match its stored BLAKE3 + size evidence.".into(),
        );
    }

    if let Err(error) = ensure_absent(&target, "local recovery destination") {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }
    if let Err(error) = fs::rename(&stage, &target) {
        remove_regular_file_best_effort(&stage);
        return Err(format!("Could not place restored recovery file: {error}"));
    }
    if !file_matches(&target, &record.original_hash, record.public.size)? {
        remove_verified_file_best_effort(&target, &record.original_hash, record.public.size);
        return Err("Restored local recovery target failed final fingerprint verification.".into());
    }

    let journal_result = (|| -> Result<String, String> {
        let modified_at = file_modified_at(&target)?;
        let mut connection = open_database(app)?;
        ensure_no_transfer_running(&connection, &record.public.workspace_id)?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start recovery journal transaction: {error}"))?;
        let now = Utc::now().to_rfc3339();
        let file_changed = transaction
            .execute(
                "UPDATE file_entries
                 SET local_present = 1,
                     local_size = ?1,
                     local_modified_at = ?2,
                     local_hash = ?3,
                     state = 'local_only',
                     tombstone = 0,
                     last_seen_at = ?4
                 WHERE workspace_id = ?5
                   AND relative_path = ?6
                   AND local_present = 0
                   AND remote_present = 0
                   AND last_synced_hash IS NULL
                   AND last_synced_remote_checksum_type IS NULL
                   AND last_synced_remote_checksum IS NULL",
                params![
                    to_i64(record.public.size, "recovery size")?,
                    modified_at,
                    record.original_hash,
                    now,
                    record.public.workspace_id,
                    record.public.relative_path,
                ],
            )
            .map_err(|error| format!("Could not journal restored local recovery: {error}"))?;
        if file_changed == 0 {
            return Err("Workspace evidence changed before recovery completion.".into());
        }
        let recovery_changed = transaction
            .execute(
                "UPDATE sync_recovery_entries
                 SET restored_at = ?1
                 WHERE id = ?2 AND restored_at IS NULL",
                params![now, record.public.id],
            )
            .map_err(|error| format!("Could not mark recovery copy as restored: {error}"))?;
        if recovery_changed == 0 {
            return Err("Recovery metadata changed before completion.".into());
        }
        transaction
            .commit()
            .map_err(|error| format!("Recovery journal commit failed: {error}"))?;
        Ok(now)
    })();

    let now = match journal_result {
        Ok(now) => now,
        Err(error) => {
            return match rollback_recovery_target(&target, &record) {
                Ok(()) => Err(format!(
                    "{error} The restored local target was rolled back."
                )),
                Err(rollback_error) => Err(format!(
                    "{error} Automatic rollback was not safe: {rollback_error}"
                )),
            };
        }
    };

    let mut restored = record.public;
    restored.restored_at = Some(now);
    Ok(restored)
}

fn ensure_recovery_target_is_journal_safe(
    connection: &Connection,
    workspace_id: &str,
    relative_path: &str,
) -> Result<(), String> {
    let state = connection
        .query_row(
            "SELECT local_present, remote_present,
                    last_synced_hash, last_synced_remote_checksum_type,
                    last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1 AND relative_path = ?2",
            params![workspace_id, relative_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? != 0,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not inspect recovery destination journal state: {error}"))?
        .ok_or_else(|| {
            "Recovery destination no longer exists in the AtrisBridge journal.".to_string()
        })?;
    if state.0 || state.1 || state.2.is_some() || state.3.is_some() || state.4.is_some() {
        return Err(
            "The recovery destination is no longer an empty converged-delete state. AtrisBridge will not create an unverified overlap."
                .into(),
        );
    }
    Ok(())
}

fn validate_recovery_source(app: &AppHandle, record: &RecoveryRecord) -> Result<PathBuf, String> {
    let recovery_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))?
        .join("recovery");
    let root = recovery_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve AtrisBridge recovery root: {error}"))?;
    let source = PathBuf::from(&record.recovery_path);
    let metadata = fs::symlink_metadata(&source)
        .map_err(|error| format!("Could not inspect recovery source: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Recovery source is not a regular AtrisBridge-owned file.".into());
    }
    let canonical = source
        .canonicalize()
        .map_err(|error| format!("Could not resolve recovery source: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("Recovery source escaped the AtrisBridge app-data recovery root.".into());
    }
    if !file_matches(&canonical, &record.original_hash, record.public.size)? {
        return Err(
            "Recovery source no longer matches its recorded BLAKE3 + size evidence.".into(),
        );
    }
    Ok(canonical)
}

fn resolve_absent_target(
    workspace_root: &str,
    relative_path: &str,
    create_parents: bool,
) -> Result<PathBuf, String> {
    let segments = validate_portable_relative_path(relative_path)?;
    let root = PathBuf::from(workspace_root)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    let (file_name, parents) = segments
        .split_last()
        .ok_or_else(|| "Recovery path has no file name.".to_string())?;
    let mut parent = root.clone();
    for segment in parents {
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("Recovery path crosses an unsafe local parent.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&next).map_err(|error| {
                    format!("Could not create recovery parent directory: {error}")
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("Recovery parent directory is missing.".into());
            }
            Err(error) => return Err(format!("Could not inspect recovery parent: {error}")),
        }
        parent = next
            .canonicalize()
            .map_err(|error| format!("Could not resolve recovery parent: {error}"))?;
        if !parent.starts_with(&root) {
            return Err("Recovery destination escaped the workspace root.".into());
        }
    }
    let target = parent.join(file_name);
    ensure_absent(&target, "local recovery destination")?;
    Ok(target)
}

fn recovery_stage_path(target: &Path, recovery_id: &str) -> Result<PathBuf, String> {
    let safe_id: String = recovery_id
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect();
    if safe_id.len() < 16 {
        return Err("Recovery identifier is invalid.".into());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "Recovery target has no parent directory.".to_string())?;
    Ok(parent.join(format!(".atrisbridge-recovery-{safe_id}.part")))
}

fn rollback_recovery_target(target: &Path, record: &RecoveryRecord) -> Result<(), String> {
    if !file_matches(target, &record.original_hash, record.public.size)? {
        return Err(
            "Recovery journal completion failed and the restored target changed; AtrisBridge preserved it for manual inspection."
                .into(),
        );
    }
    durable_fs::remove_regular_file(target)
        .map_err(|error| format!("Could not roll back uncommitted recovery target: {error}"))
}

fn remove_verified_file_best_effort(path: &Path, hash: &str, size: u64) {
    if file_matches(path, hash, size).unwrap_or(false) {
        let _ = durable_fs::remove_regular_file(path);
    }
}

fn ensure_absent(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(format!("{label} already exists.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not inspect {label}: {error}")),
    }
}

fn file_matches(path: &Path, expected_hash: &str, expected_size: u64) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Could not inspect recovery file: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let (size, hash) = scanner::fingerprint_file(path)?;
    Ok(size == expected_size && hash == expected_hash)
}

fn file_modified_at(path: &Path) -> Result<Option<String>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not read restored recovery metadata: {error}"))?;
    Ok(metadata
        .modified()
        .ok()
        .map(|value| DateTime::<Utc>::from(value).to_rfc3339()))
}

fn remove_regular_file_best_effort(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            let _ = durable_fs::remove_regular_file(path);
        }
    }
}

fn ensure_no_transfer_running(connection: &Connection, workspace_id: &str) -> Result<(), String> {
    if table_exists(connection, "sync_plans")? {
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sync_plans WHERE workspace_id = ?1 AND status = 'running'",
                params![workspace_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active two-way sync: {error}"))?;
        if running > 0 {
            return Err(
                "A two-way synchronization is currently running for this workspace.".into(),
            );
        }
    }
    if table_exists(connection, "backup_plans")? {
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM backup_plans WHERE workspace_id = ?1 AND status = 'running'",
                params![workspace_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active backup: {error}"))?;
        if running > 0 {
            return Err("A backup is currently running for this workspace.".into());
        }
    }
    if table_exists(connection, "restore_plans")? {
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM restore_plans WHERE workspace_id = ?1 AND status = 'running'",
                params![workspace_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active restore: {error}"))?;
        if running > 0 {
            return Err("A restore is currently running for this workspace.".into());
        }
    }
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            params![table],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|error| format!("Could not inspect SQLite recovery tables: {error}"))
}

fn recovery_public_from_row(row: &Row<'_>) -> rusqlite::Result<SyncRecoveryEntry> {
    Ok(SyncRecoveryEntry {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        relative_path: row.get(2)?,
        size: required_u64(row, 3)?,
        created_at: row.get(4)?,
        restored_at: row.get(5)?,
    })
}

fn recovery_record_from_row(row: &Row<'_>) -> rusqlite::Result<RecoveryRecord> {
    Ok(RecoveryRecord {
        public: SyncRecoveryEntry {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            relative_path: row.get(2)?,
            size: required_u64(row, 5)?,
            created_at: row.get(6)?,
            restored_at: row.get(7)?,
        },
        recovery_path: row.get(3)?,
        original_hash: row.get(4)?,
    })
}

fn required_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn validate_portable_relative_path(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') {
        return Err("Recovery path cannot be mapped safely across supported filesystems.".into());
    }
    let mut segments = Vec::new();
    for component in Path::new(value).components() {
        let Component::Normal(segment) = component else {
            return Err("Recovery path contains an unsafe segment.".into());
        };
        let segment = segment
            .to_str()
            .ok_or_else(|| "Recovery path contains non-Unicode data.".to_string())?;
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.chars().any(|character| {
                character.is_control()
                    || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
            })
            || segment.ends_with(' ')
            || segment.ends_with('.')
        {
            return Err("Recovery path contains a non-portable segment.".into());
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or(segment)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .and_then(|suffix| suffix.parse::<u8>().ok())
                .is_some_and(|number| (1..=9).contains(&number))
            || stem
                .strip_prefix("LPT")
                .and_then(|suffix| suffix.parse::<u8>().ok())
                .is_some_and(|number| (1..=9).contains(&number));
        if reserved {
            return Err("Recovery path uses a Windows-reserved file name.".into());
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        return Err("Recovery path is empty.".into());
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_paths_reject_parent_or_reserved_segments() {
        assert!(validate_portable_relative_path("src/../secret.txt").is_err());
        assert!(validate_portable_relative_path("NUL.txt").is_err());
        assert!(validate_portable_relative_path("src/main.rs").is_ok());
    }
}
