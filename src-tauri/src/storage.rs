use rusqlite::{params, Connection, OptionalExtension, Row};
use tauri::AppHandle;

use crate::{
    database::open_database,
    models::{JournalSummary, ScanFile, ScanReport, SyncMode, Workspace},
};

pub fn load_workspaces(app: &AppHandle) -> Result<Vec<Workspace>, String> {
    let connection = open_database(app)?;
    load_workspaces_with_connection(&connection)
}

pub fn insert_workspace(app: &AppHandle, workspace: &Workspace) -> Result<(), String> {
    let connection = open_database(app)?;
    connection
        .execute(
            "INSERT INTO workspaces (
                id, name, local_path, sync_mode, created_at, last_scan_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace.id,
                workspace.name,
                workspace.local_path,
                workspace.sync_mode.as_str(),
                workspace.created_at,
                workspace.last_scan_at,
            ],
        )
        .map_err(|error| {
            if error.to_string().contains("UNIQUE constraint failed") {
                "This directory is already an AtrisBridge workspace.".to_string()
            } else {
                format!("Could not save workspace metadata: {error}")
            }
        })?;
    Ok(())
}

pub fn delete_workspace(app: &AppHandle, workspace_id: &str) -> Result<(), String> {
    let connection = open_database(app)?;
    let changed = connection
        .execute(
            "DELETE FROM workspaces WHERE id = ?1",
            params![workspace_id],
        )
        .map_err(|error| format!("Could not remove workspace metadata: {error}"))?;
    if changed == 0 {
        return Err("Workspace was not found.".into());
    }
    Ok(())
}

pub fn find_workspace(app: &AppHandle, workspace_id: &str) -> Result<Workspace, String> {
    let connection = open_database(app)?;
    find_workspace_with_connection(&connection, workspace_id)
}

pub fn record_scan(
    app: &AppHandle,
    report: &ScanReport,
    inventory: &[ScanFile],
) -> Result<(), String> {
    let mut connection = open_database(app)?;
    record_scan_with_connection(&mut connection, report, inventory)
}

pub fn get_journal_summary(app: &AppHandle, workspace_id: &str) -> Result<JournalSummary, String> {
    let connection = open_database(app)?;
    journal_summary_with_connection(&connection, workspace_id)
}

pub fn list_journal_summaries(app: &AppHandle) -> Result<Vec<JournalSummary>, String> {
    let connection = open_database(app)?;
    let workspace_ids = {
        let mut statement = connection
            .prepare("SELECT id FROM workspaces ORDER BY created_at ASC")
            .map_err(|error| format!("Could not prepare workspace summary query: {error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Could not query workspace summaries: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("Could not read workspace summaries: {error}"))?
    };

    workspace_ids
        .into_iter()
        .map(|workspace_id| journal_summary_with_connection(&connection, &workspace_id))
        .collect()
}

fn load_workspaces_with_connection(connection: &Connection) -> Result<Vec<Workspace>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, name, local_path, sync_mode, created_at, last_scan_at
             FROM workspaces
             ORDER BY created_at ASC",
        )
        .map_err(|error| format!("Could not prepare workspace query: {error}"))?;

    let rows = statement
        .query_map([], workspace_from_row)
        .map_err(|error| format!("Could not query workspaces: {error}"))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read workspace metadata: {error}"))
}

fn find_workspace_with_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Workspace, String> {
    connection
        .query_row(
            "SELECT id, name, local_path, sync_mode, created_at, last_scan_at
             FROM workspaces WHERE id = ?1",
            params![workspace_id],
            workspace_from_row,
        )
        .optional()
        .map_err(|error| format!("Could not read workspace metadata: {error}"))?
        .ok_or_else(|| "Workspace was not found.".to_string())
}

fn workspace_from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let sync_mode: String = row.get(3)?;
    let sync_mode = SyncMode::from_storage(&sync_mode).ok_or(rusqlite::Error::InvalidQuery)?;
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        local_path: row.get(2)?,
        sync_mode,
        created_at: row.get(4)?,
        last_scan_at: row.get(5)?,
    })
}

fn record_scan_with_connection(
    connection: &mut Connection,
    report: &ScanReport,
    inventory: &[ScanFile],
) -> Result<(), String> {
    let duration_ms = to_i64_u128(report.duration_ms, "scan duration")?;
    let file_count = to_i64_u64(report.file_count, "file count")?;
    let directory_count = to_i64_u64(report.directory_count, "directory count")?;
    let total_bytes = to_i64_u64(report.total_bytes, "byte count")?;
    let skipped_entries = to_i64_u64(report.skipped_entries, "skipped entry count")?;
    let warnings_json = serde_json::to_string(&report.warnings)
        .map_err(|error| format!("Could not serialize scan warnings: {error}"))?;

    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start scan journal transaction: {error}"))?;

    transaction
        .execute(
            "INSERT INTO scan_runs (
                workspace_id, scanned_at, duration_ms, file_count, directory_count,
                total_bytes, skipped_entries, warnings_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                report.workspace_id,
                report.scanned_at,
                duration_ms,
                file_count,
                directory_count,
                total_bytes,
                skipped_entries,
                warnings_json,
            ],
        )
        .map_err(|error| format!("Could not create scan journal entry: {error}"))?;
    let scan_id = transaction.last_insert_rowid();

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO file_entries (
                    workspace_id, relative_path, local_present, local_size,
                    local_modified_at, local_hash, state, tombstone,
                    first_seen_at, last_seen_at, last_seen_scan_id
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, 'local_only', 0, ?6, ?6, ?7)
                 ON CONFLICT(workspace_id, relative_path) DO UPDATE SET
                    local_present = 1,
                    local_size = excluded.local_size,
                    local_modified_at = excluded.local_modified_at,
                    local_hash = excluded.local_hash,
                    last_seen_at = excluded.last_seen_at,
                    last_seen_scan_id = excluded.last_seen_scan_id,
                    tombstone = 0,
                    state = CASE
                        WHEN file_entries.last_synced_hash IS NULL
                             AND file_entries.remote_present = 1
                            THEN 'conflict'
                        WHEN file_entries.last_synced_hash IS NULL
                            THEN 'local_only'
                        WHEN excluded.local_hash = file_entries.last_synced_hash
                             AND file_entries.remote_present = 1
                             AND file_entries.last_synced_remote_checksum IS NOT NULL
                             AND file_entries.last_synced_remote_checksum_type IS NOT NULL
                             AND file_entries.remote_checksum IS NOT NULL
                             AND file_entries.remote_checksum_type IS NOT NULL
                             AND file_entries.remote_checksum_type = file_entries.last_synced_remote_checksum_type
                             AND file_entries.remote_checksum = file_entries.last_synced_remote_checksum
                            THEN 'synced'
                        WHEN excluded.local_hash = file_entries.last_synced_hash
                            THEN 'remote_modified'
                        WHEN file_entries.remote_present = 1
                             AND file_entries.last_synced_remote_checksum IS NOT NULL
                             AND file_entries.last_synced_remote_checksum_type IS NOT NULL
                             AND file_entries.remote_checksum IS NOT NULL
                             AND file_entries.remote_checksum_type IS NOT NULL
                             AND file_entries.remote_checksum_type = file_entries.last_synced_remote_checksum_type
                             AND file_entries.remote_checksum = file_entries.last_synced_remote_checksum
                            THEN 'local_modified'
                        ELSE 'conflict'
                    END",
            )
            .map_err(|error| format!("Could not prepare file journal update: {error}"))?;

        for file in inventory {
            let size = to_i64_u64(file.size, "file size")?;
            statement
                .execute(params![
                    report.workspace_id,
                    file.relative_path,
                    size,
                    file.modified_at,
                    file.blake3,
                    report.scanned_at,
                    scan_id,
                ])
                .map_err(|error| {
                    format!("Could not journal file {}: {error}", file.relative_path)
                })?;
        }
    }

    transaction
        .execute(
            "UPDATE file_entries
             SET local_present = 0,
                 local_size = NULL,
                 local_modified_at = NULL,
                 local_hash = NULL,
                 state = CASE
                    WHEN last_synced_hash IS NOT NULL
                         AND remote_present = 1
                         AND last_synced_remote_checksum IS NOT NULL
                         AND last_synced_remote_checksum_type IS NOT NULL
                         AND remote_checksum IS NOT NULL
                         AND remote_checksum_type IS NOT NULL
                         AND remote_checksum_type = last_synced_remote_checksum_type
                         AND remote_checksum = last_synced_remote_checksum
                        THEN 'local_deleted'
                    WHEN last_synced_hash IS NOT NULL
                        THEN 'conflict'
                    WHEN remote_present = 1
                        THEN 'remote_only'
                    ELSE 'removed_before_sync'
                 END,
                 tombstone = CASE
                    WHEN last_synced_hash IS NOT NULL
                         AND remote_present = 1
                         AND last_synced_remote_checksum IS NOT NULL
                         AND last_synced_remote_checksum_type IS NOT NULL
                         AND remote_checksum IS NOT NULL
                         AND remote_checksum_type IS NOT NULL
                         AND remote_checksum_type = last_synced_remote_checksum_type
                         AND remote_checksum = last_synced_remote_checksum
                        THEN 1
                    ELSE 0
                 END
             WHERE workspace_id = ?1
               AND local_present = 1
               AND (last_seen_scan_id IS NULL OR last_seen_scan_id != ?2)",
            params![report.workspace_id, scan_id],
        )
        .map_err(|error| format!("Could not mark missing files in scan journal: {error}"))?;

    let updated = transaction
        .execute(
            "UPDATE workspaces SET last_scan_at = ?1 WHERE id = ?2",
            params![report.scanned_at, report.workspace_id],
        )
        .map_err(|error| format!("Could not update workspace scan timestamp: {error}"))?;

    if updated == 0 {
        return Err("Workspace was not found while recording scan.".into());
    }

    transaction
        .commit()
        .map_err(|error| format!("Could not commit scan journal transaction: {error}"))
}

fn journal_summary_with_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<JournalSummary, String> {
    let workspace = connection
        .query_row(
            "SELECT id, last_scan_at FROM workspaces WHERE id = ?1",
            params![workspace_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read workspace journal metadata: {error}"))?
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    let counts = connection
        .query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN local_present = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_present = 1 THEN local_size ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN state IN (
                    'local_only', 'local_modified', 'local_deleted',
                    'remote_only', 'remote_modified', 'conflict'
                ) THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(tombstone), 0),
                COALESCE(SUM(CASE WHEN state = 'conflict' THEN 1 ELSE 0 END), 0)
             FROM file_entries
             WHERE workspace_id = ?1",
            params![workspace_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .map_err(|error| format!("Could not summarize file journal: {error}"))?;

    let pending_operations: i64 = connection
        .query_row(
            "SELECT COUNT(*)
             FROM pending_operations
             WHERE workspace_id = ?1
               AND status IN ('pending', 'running', 'failed')",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not summarize operation journal: {error}"))?;

    Ok(JournalSummary {
        workspace_id: workspace.0,
        tracked_files: from_i64(counts.0, "tracked file count")?,
        present_files: from_i64(counts.1, "present file count")?,
        present_bytes: from_i64(counts.2, "present byte count")?,
        changed_files: from_i64(counts.3, "changed file count")?,
        tombstones: from_i64(counts.4, "tombstone count")?,
        conflicts: from_i64(counts.5, "conflict count")?,
        pending_operations: from_i64(pending_operations, "pending operation count")?,
        last_scan_at: workspace.1,
    })
}

fn to_i64_u64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn to_i64_u128(value: u128, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn from_i64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Stored {label} is invalid."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("foreign keys");
        crate::database::migrate_schema(&mut connection).expect("schema");
        connection
    }

    fn workspace() -> Workspace {
        Workspace {
            id: "workspace-1".into(),
            name: "Project".into(),
            local_path: "/tmp/project".into(),
            sync_mode: SyncMode::Backup,
            created_at: "2026-08-11T07:00:00Z".into(),
            last_scan_at: None,
        }
    }

    fn report(at: &str, files: &[ScanFile]) -> ScanReport {
        ScanReport {
            workspace_id: "workspace-1".into(),
            scanned_at: at.into(),
            duration_ms: 5,
            file_count: files.len() as u64,
            directory_count: 1,
            total_bytes: files.iter().map(|file| file.size).sum(),
            skipped_entries: 0,
            preview_truncated: false,
            files: files.to_vec(),
            warnings: vec![],
        }
    }

    fn file(path: &str, hash: &str) -> ScanFile {
        ScanFile {
            relative_path: path.into(),
            size: 12,
            modified_at: Some("2026-08-11T07:00:00Z".into()),
            blake3: hash.into(),
        }
    }

    #[test]
    fn missing_file_becomes_tombstone_only_with_unchanged_remote_evidence() {
        let mut connection = connection();
        insert_workspace_with_connection(&connection, &workspace()).expect("workspace");
        let first = vec![file("src/main.rs", "local-a")];
        record_scan_with_connection(
            &mut connection,
            &report("2026-08-11T07:10:00Z", &first),
            &first,
        )
        .expect("first scan");
        connection
            .execute(
                "UPDATE file_entries
                 SET remote_present = 1,
                     remote_checksum_type = 'MD5',
                     remote_checksum = 'remote-a',
                     last_synced_hash = 'local-a',
                     last_synced_remote_checksum_type = 'MD5',
                     last_synced_remote_checksum = 'remote-a',
                     state = 'synced'
                 WHERE workspace_id = 'workspace-1' AND relative_path = 'src/main.rs'",
                [],
            )
            .expect("baseline");

        record_scan_with_connection(&mut connection, &report("2026-08-11T07:20:00Z", &[]), &[])
            .expect("missing scan");

        let (state, tombstone): (String, i64) = connection
            .query_row(
                "SELECT state, tombstone FROM file_entries
                 WHERE workspace_id = 'workspace-1' AND relative_path = 'src/main.rs'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("file state");
        assert_eq!(state, "local_deleted");
        assert_eq!(tombstone, 1);
    }

    fn insert_workspace_with_connection(
        connection: &Connection,
        workspace: &Workspace,
    ) -> Result<(), String> {
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, name, local_path, sync_mode, created_at, last_scan_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    workspace.id,
                    workspace.name,
                    workspace.local_path,
                    workspace.sync_mode.as_str(),
                    workspace.created_at,
                    workspace.last_scan_at,
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
