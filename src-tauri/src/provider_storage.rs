use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    database::open_database,
    models::{
        ProviderConnection, RemoteFileObservation, RemoteInventoryReport, WorkspaceRemoteBinding,
    },
};

pub fn list_provider_connections(app: &AppHandle) -> Result<Vec<ProviderConnection>, String> {
    let connection = open_database(app)?;
    list_provider_connections_with_connection(&connection)
}

pub fn upsert_google_drive_connection(
    app: &AppHandle,
    account_label: Option<String>,
) -> Result<ProviderConnection, String> {
    let connection = open_database(app)?;
    upsert_google_drive_connection_with_connection(&connection, account_label)
}

pub fn remove_provider_connection(app: &AppHandle, provider_id: &str) -> Result<(), String> {
    let connection = open_database(app)?;
    let changed = connection
        .execute(
            "DELETE FROM provider_connections WHERE id = ?1",
            params![provider_id],
        )
        .map_err(|error| format!("Could not forget cloud provider metadata: {error}"))?;
    if changed == 0 {
        return Err("Cloud provider connection was not found.".into());
    }
    Ok(())
}

pub fn bind_workspace(
    app: &AppHandle,
    workspace_id: &str,
    provider_id: &str,
    remote_path: &str,
) -> Result<WorkspaceRemoteBinding, String> {
    let connection = open_database(app)?;
    bind_workspace_with_connection(&connection, workspace_id, provider_id, remote_path)
}

pub fn get_workspace_binding(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<Option<WorkspaceRemoteBinding>, String> {
    let connection = open_database(app)?;
    get_workspace_binding_with_connection(&connection, workspace_id)
}

pub fn get_provider_for_workspace(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<(ProviderConnection, WorkspaceRemoteBinding), String> {
    let connection = open_database(app)?;
    let binding = get_workspace_binding_with_connection(&connection, workspace_id)?
        .ok_or_else(|| "This workspace is not bound to a cloud provider yet.".to_string())?;
    let provider = connection
        .query_row(
            "SELECT id, provider_type, display_name, account_label, created_at, last_verified_at
             FROM provider_connections
             WHERE id = ?1",
            params![binding.provider_id],
            provider_from_row,
        )
        .optional()
        .map_err(|error| format!("Could not read cloud provider metadata: {error}"))?
        .ok_or_else(|| "Cloud provider connection was not found.".to_string())?;
    Ok((provider, binding))
}

pub fn record_remote_inventory(
    app: &AppHandle,
    report: &RemoteInventoryReport,
    observations: &[RemoteFileObservation],
) -> Result<(), String> {
    let mut connection = open_database(app)?;
    record_remote_inventory_with_connection(&mut connection, report, observations)
}

fn list_provider_connections_with_connection(
    connection: &Connection,
) -> Result<Vec<ProviderConnection>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, provider_type, display_name, account_label, created_at, last_verified_at
             FROM provider_connections
             ORDER BY created_at ASC",
        )
        .map_err(|error| format!("Could not prepare cloud provider query: {error}"))?;
    let rows = statement
        .query_map([], provider_from_row)
        .map_err(|error| format!("Could not query cloud providers: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read cloud provider metadata: {error}"))
}

fn provider_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderConnection> {
    Ok(ProviderConnection {
        id: row.get(0)?,
        provider_type: row.get(1)?,
        display_name: row.get(2)?,
        account_label: row.get(3)?,
        created_at: row.get(4)?,
        last_verified_at: row.get(5)?,
        session_active: false,
    })
}

fn upsert_google_drive_connection_with_connection(
    connection: &Connection,
    account_label: Option<String>,
) -> Result<ProviderConnection, String> {
    let now = Utc::now().to_rfc3339();
    let existing = connection
        .query_row(
            "SELECT id, provider_type, display_name, account_label, created_at, last_verified_at
             FROM provider_connections
             WHERE provider_type = 'google_drive'",
            [],
            provider_from_row,
        )
        .optional()
        .map_err(|error| format!("Could not inspect Google Drive metadata: {error}"))?;

    if let Some(mut provider) = existing {
        connection
            .execute(
                "UPDATE provider_connections
                 SET account_label = ?1, last_verified_at = ?2
                 WHERE id = ?3",
                params![account_label, now, provider.id],
            )
            .map_err(|error| format!("Could not update Google Drive metadata: {error}"))?;
        provider.account_label = account_label;
        provider.last_verified_at = Some(now);
        return Ok(provider);
    }

    let provider = ProviderConnection {
        id: Uuid::new_v4().to_string(),
        provider_type: "google_drive".into(),
        display_name: "Google Drive".into(),
        account_label,
        created_at: now.clone(),
        last_verified_at: Some(now),
        session_active: false,
    };
    connection
        .execute(
            "INSERT INTO provider_connections (
                id, provider_type, display_name, account_label, created_at, last_verified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                provider.id,
                provider.provider_type,
                provider.display_name,
                provider.account_label,
                provider.created_at,
                provider.last_verified_at,
            ],
        )
        .map_err(|error| format!("Could not save Google Drive metadata: {error}"))?;
    Ok(provider)
}

fn bind_workspace_with_connection(
    connection: &Connection,
    workspace_id: &str,
    provider_id: &str,
    remote_path: &str,
) -> Result<WorkspaceRemoteBinding, String> {
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO workspace_remote_bindings (
                workspace_id, provider_id, remote_path, created_at, last_inventory_at
             ) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(workspace_id) DO UPDATE SET
                provider_id = excluded.provider_id,
                remote_path = excluded.remote_path,
                last_inventory_at = CASE
                    WHEN workspace_remote_bindings.provider_id = excluded.provider_id
                     AND workspace_remote_bindings.remote_path = excluded.remote_path
                    THEN workspace_remote_bindings.last_inventory_at
                    ELSE NULL
                END",
            params![workspace_id, provider_id, remote_path, now],
        )
        .map_err(|error| {
            if error.to_string().contains("UNIQUE constraint failed") {
                "That remote path is already bound to another workspace.".to_string()
            } else {
                format!("Could not bind workspace to cloud provider: {error}")
            }
        })?;

    get_workspace_binding_with_connection(connection, workspace_id)?
        .ok_or_else(|| "Workspace cloud binding could not be read after saving.".to_string())
}

fn get_workspace_binding_with_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<WorkspaceRemoteBinding>, String> {
    connection
        .query_row(
            "SELECT workspace_id, provider_id, remote_path, created_at, last_inventory_at
             FROM workspace_remote_bindings
             WHERE workspace_id = ?1",
            params![workspace_id],
            |row| {
                Ok(WorkspaceRemoteBinding {
                    workspace_id: row.get(0)?,
                    provider_id: row.get(1)?,
                    remote_path: row.get(2)?,
                    created_at: row.get(3)?,
                    last_inventory_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read workspace cloud binding: {error}"))
}

fn record_remote_inventory_with_connection(
    connection: &mut Connection,
    report: &RemoteInventoryReport,
    observations: &[RemoteFileObservation],
) -> Result<(), String> {
    let file_count = to_i64(report.file_count, "remote file count")?;
    let total_bytes = to_i64(report.total_bytes, "remote byte count")?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start remote inventory transaction: {error}"))?;

    transaction
        .execute(
            "INSERT INTO remote_scan_runs (
                workspace_id, provider_id, scanned_at, file_count, total_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                report.workspace_id,
                report.provider_id,
                report.scanned_at,
                file_count,
                total_bytes,
            ],
        )
        .map_err(|error| format!("Could not create remote scan journal entry: {error}"))?;
    let remote_scan_id = transaction.last_insert_rowid();

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO file_entries (
                    workspace_id, relative_path, local_present,
                    remote_present, remote_id, remote_size, remote_modified_at,
                    remote_checksum_type, remote_checksum, state, tombstone,
                    first_seen_at, last_seen_at, last_remote_seen_at, last_remote_scan_id
                 ) VALUES (?1, ?2, 0, 1, ?3, ?4, ?5, ?6, ?7, 'remote_only', 0, ?8, ?8, ?8, ?9)
                 ON CONFLICT(workspace_id, relative_path) DO UPDATE SET
                    remote_present = 1,
                    remote_id = excluded.remote_id,
                    remote_size = excluded.remote_size,
                    remote_modified_at = excluded.remote_modified_at,
                    remote_checksum_type = excluded.remote_checksum_type,
                    remote_checksum = excluded.remote_checksum,
                    last_remote_seen_at = excluded.last_remote_seen_at,
                    last_remote_scan_id = excluded.last_remote_scan_id,
                    state = CASE
                        WHEN file_entries.local_present = 0
                            THEN 'remote_only'
                        WHEN file_entries.last_synced_hash IS NULL
                            THEN 'conflict'
                        WHEN file_entries.last_synced_remote_checksum IS NULL
                             OR excluded.remote_checksum IS NULL
                             OR file_entries.last_synced_remote_checksum_type IS NULL
                             OR excluded.remote_checksum_type IS NULL
                            THEN 'conflict'
                        WHEN file_entries.last_synced_remote_checksum_type != excluded.remote_checksum_type
                            THEN 'conflict'
                        WHEN file_entries.last_synced_remote_checksum = excluded.remote_checksum
                             AND file_entries.local_hash = file_entries.last_synced_hash
                            THEN 'synced'
                        WHEN file_entries.last_synced_remote_checksum = excluded.remote_checksum
                            THEN 'local_modified'
                        WHEN file_entries.local_hash = file_entries.last_synced_hash
                            THEN 'remote_modified'
                        ELSE 'conflict'
                    END",
            )
            .map_err(|error| format!("Could not prepare remote file journal update: {error}"))?;

        for observation in observations {
            statement
                .execute(params![
                    report.workspace_id,
                    observation.relative_path,
                    observation.remote_id,
                    to_i64(observation.size, "remote file size")?,
                    observation.modified_at,
                    observation.checksum_type,
                    observation.checksum,
                    report.scanned_at,
                    remote_scan_id,
                ])
                .map_err(|error| {
                    format!(
                        "Could not journal remote file {}: {error}",
                        observation.relative_path
                    )
                })?;
        }
    }

    transaction
        .execute(
            "UPDATE file_entries
             SET remote_present = 0,
                 state = CASE
                    WHEN local_present = 1 AND last_synced_hash IS NOT NULL
                        THEN 'conflict'
                    WHEN local_present = 1
                        THEN 'local_only'
                    ELSE 'removed_before_sync'
                 END
             WHERE workspace_id = ?1
               AND remote_present = 1
               AND (last_remote_scan_id IS NULL OR last_remote_scan_id != ?2)",
            params![report.workspace_id, remote_scan_id],
        )
        .map_err(|error| format!("Could not mark missing remote files: {error}"))?;

    transaction
        .execute(
            "UPDATE workspace_remote_bindings
             SET last_inventory_at = ?1
             WHERE workspace_id = ?2 AND provider_id = ?3",
            params![report.scanned_at, report.workspace_id, report.provider_id],
        )
        .map_err(|error| format!("Could not update remote inventory timestamp: {error}"))?;

    transaction
        .commit()
        .map_err(|error| format!("Could not commit remote inventory transaction: {error}"))
}

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    fn test_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("foreign keys");
        database::migrate_schema(&mut connection).expect("schema");
        connection
    }

    fn seed_workspace(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO workspaces (id, name, local_path, sync_mode, created_at)
                 VALUES ('workspace-1', 'Project', '/tmp/project', 'backup', '2026-08-11T07:00:00Z')",
                [],
            )
            .expect("workspace");
    }

    fn report(provider_id: &str, count: u64) -> RemoteInventoryReport {
        RemoteInventoryReport {
            workspace_id: "workspace-1".into(),
            provider_id: provider_id.into(),
            remote_path: "AtrisBridge/Project".into(),
            scanned_at: "2026-08-11T07:30:00Z".into(),
            file_count: count,
            total_bytes: count * 12,
        }
    }

    fn observation(path: &str) -> RemoteFileObservation {
        RemoteFileObservation {
            relative_path: path.into(),
            remote_id: Some("drive-id".into()),
            size: 12,
            modified_at: Some("2026-08-11T07:20:00Z".into()),
            checksum_type: Some("MD5".into()),
            checksum: Some("abc".into()),
        }
    }

    #[test]
    fn provider_metadata_never_contains_oauth_tokens() {
        let connection = test_connection();
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(provider_connections)")
            .expect("prepare")
            .query_map([], |row| row.get(1))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns");

        assert!(!columns.iter().any(|column| column.contains("token")));
        assert!(!columns.iter().any(|column| column.contains("secret")));
    }

    #[test]
    fn remote_only_files_are_journaled_without_local_delete_intent() {
        let mut connection = test_connection();
        seed_workspace(&connection);
        let provider =
            upsert_google_drive_connection_with_connection(&connection, None).expect("provider");
        bind_workspace_with_connection(
            &connection,
            "workspace-1",
            &provider.id,
            "AtrisBridge/Project",
        )
        .expect("binding");

        record_remote_inventory_with_connection(
            &mut connection,
            &report(&provider.id, 1),
            &[observation("src/main.rs")],
        )
        .expect("inventory");

        let (state, local_present, remote_present, tombstone): (String, i64, i64, i64) = connection
            .query_row(
                "SELECT state, local_present, remote_present, tombstone
                 FROM file_entries WHERE workspace_id = 'workspace-1' AND relative_path = 'src/main.rs'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("entry");
        assert_eq!(state, "remote_only");
        assert_eq!(local_present, 0);
        assert_eq!(remote_present, 1);
        assert_eq!(tombstone, 0);
    }

    #[test]
    fn overlapping_unverified_remote_file_is_a_conflict() {
        let mut connection = test_connection();
        seed_workspace(&connection);
        connection
            .execute(
                "INSERT INTO file_entries (
                    workspace_id, relative_path, local_present, local_size, local_hash,
                    state, first_seen_at, last_seen_at
                 ) VALUES (
                    'workspace-1', 'src/main.rs', 1, 12, 'blake3-local',
                    'local_only', '2026-08-11T07:00:00Z', '2026-08-11T07:00:00Z'
                 )",
                [],
            )
            .expect("local entry");
        let provider =
            upsert_google_drive_connection_with_connection(&connection, None).expect("provider");
        bind_workspace_with_connection(
            &connection,
            "workspace-1",
            &provider.id,
            "AtrisBridge/Project",
        )
        .expect("binding");

        record_remote_inventory_with_connection(
            &mut connection,
            &report(&provider.id, 1),
            &[observation("src/main.rs")],
        )
        .expect("inventory");

        let state: String = connection
            .query_row(
                "SELECT state FROM file_entries WHERE workspace_id = 'workspace-1' AND relative_path = 'src/main.rs'",
                [],
                |row| row.get(0),
            )
            .expect("state");
        assert_eq!(state, "conflict");
    }
}
