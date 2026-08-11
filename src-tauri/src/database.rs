use std::{fs, path::PathBuf, time::Duration};

use rusqlite::{params, Connection, OptionalExtension};
use tauri::{AppHandle, Manager};

use crate::models::Workspace;

const DATABASE_FILE: &str = "atrisbridge.db";
const LEGACY_WORKSPACE_FILE: &str = "workspaces.json";
const SCHEMA_VERSION: i32 = 3;
const LEGACY_IMPORT_KEY: &str = "legacy_workspaces_imported";

pub fn open_database(app: &AppHandle) -> Result<Connection, String> {
    let app_data = app_data_dir(app)?;
    let database_path = app_data.join(DATABASE_FILE);
    let mut connection = Connection::open(&database_path)
        .map_err(|error| format!("Could not open AtrisBridge database: {error}"))?;

    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| format!("Could not configure database busy timeout: {error}"))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|error| format!("Could not configure AtrisBridge database: {error}"))?;

    migrate_schema(&mut connection)
        .map_err(|error| format!("Could not migrate AtrisBridge database: {error}"))?;
    migrate_legacy_workspaces(app, &mut connection)?;

    Ok(connection)
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve app data directory: {error}"))?;
    fs::create_dir_all(&app_data)
        .map_err(|error| format!("Could not create app data directory: {error}"))?;
    Ok(app_data)
}

pub(crate) fn migrate_schema(connection: &mut Connection) -> rusqlite::Result<()> {
    let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }

    if version == 0 {
        let transaction = connection.transaction()?;
        initialize_schema(&transaction)?;
        transaction.execute_batch("PRAGMA user_version = 3;")?;
        transaction.commit()?;
        return Ok(());
    }

    if version == 1 {
        let transaction = connection.transaction()?;
        migrate_to_v2(&transaction)?;
        migrate_to_v3(&transaction)?;
        transaction.execute_batch("PRAGMA user_version = 3;")?;
        transaction.commit()?;
        return Ok(());
    }

    if version == 2 {
        let transaction = connection.transaction()?;
        migrate_to_v3(&transaction)?;
        transaction.execute_batch("PRAGMA user_version = 3;")?;
        transaction.commit()?;
    }

    Ok(())
}

fn initialize_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS workspaces (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL CHECK(length(name) > 0),
            local_path TEXT NOT NULL UNIQUE,
            sync_mode TEXT NOT NULL CHECK(sync_mode IN ('backup', 'pull', 'two_way')),
            created_at TEXT NOT NULL,
            last_scan_at TEXT
        );

        CREATE TABLE IF NOT EXISTS scan_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            scanned_at TEXT NOT NULL,
            duration_ms INTEGER NOT NULL,
            file_count INTEGER NOT NULL,
            directory_count INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            skipped_entries INTEGER NOT NULL,
            warnings_json TEXT NOT NULL DEFAULT '[]',
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_scan_runs_workspace_scanned_at
            ON scan_runs(workspace_id, scanned_at DESC);

        CREATE TABLE IF NOT EXISTS file_entries (
            workspace_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            local_present INTEGER NOT NULL DEFAULT 1 CHECK(local_present IN (0, 1)),
            local_size INTEGER,
            local_modified_at TEXT,
            local_hash TEXT,
            remote_present INTEGER NOT NULL DEFAULT 0 CHECK(remote_present IN (0, 1)),
            remote_id TEXT,
            remote_size INTEGER,
            remote_modified_at TEXT,
            remote_hash TEXT,
            remote_checksum_type TEXT,
            remote_checksum TEXT,
            last_synced_hash TEXT,
            last_synced_remote_checksum_type TEXT,
            last_synced_remote_checksum TEXT,
            state TEXT NOT NULL CHECK(state IN (
                'local_only',
                'synced',
                'local_modified',
                'local_deleted',
                'removed_before_sync',
                'remote_only',
                'remote_modified',
                'conflict'
            )),
            tombstone INTEGER NOT NULL DEFAULT 0 CHECK(tombstone IN (0, 1)),
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            last_synced_at TEXT,
            last_seen_scan_id INTEGER,
            last_remote_seen_at TEXT,
            last_remote_scan_id INTEGER,
            PRIMARY KEY(workspace_id, relative_path),
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(last_seen_scan_id) REFERENCES scan_runs(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_file_entries_workspace_state
            ON file_entries(workspace_id, state);
        CREATE INDEX IF NOT EXISTS idx_file_entries_workspace_tombstone
            ON file_entries(workspace_id, tombstone);
        CREATE INDEX IF NOT EXISTS idx_file_entries_workspace_remote_present
            ON file_entries(workspace_id, remote_present);

        CREATE TABLE IF NOT EXISTS pending_operations (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            operation_type TEXT NOT NULL CHECK(operation_type IN (
                'upload',
                'download',
                'trash_remote',
                'restore_local',
                'keep_both'
            )),
            status TEXT NOT NULL CHECK(status IN (
                'pending',
                'running',
                'failed',
                'completed',
                'cancelled'
            )),
            attempt_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_pending_operations_workspace_status
            ON pending_operations(workspace_id, status);

        CREATE TABLE IF NOT EXISTS provider_connections (
            id TEXT PRIMARY KEY,
            provider_type TEXT NOT NULL CHECK(provider_type IN ('google_drive')),
            display_name TEXT NOT NULL,
            account_label TEXT,
            created_at TEXT NOT NULL,
            last_verified_at TEXT
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_connections_type
            ON provider_connections(provider_type);

        CREATE TABLE IF NOT EXISTS workspace_remote_bindings (
            workspace_id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            last_inventory_at TEXT,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE,
            UNIQUE(provider_id, remote_path)
        );

        CREATE TABLE IF NOT EXISTS remote_scan_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            scanned_at TEXT NOT NULL,
            file_count INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_remote_scan_runs_workspace_scanned_at
            ON remote_scan_runs(workspace_id, scanned_at DESC);

        CREATE TABLE IF NOT EXISTS backup_plans (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN (
                'ready', 'running', 'completed', 'partial', 'failed', 'cancelled'
            )),
            created_at TEXT NOT NULL,
            local_scan_at TEXT NOT NULL,
            remote_inventory_at TEXT NOT NULL,
            upload_count INTEGER NOT NULL DEFAULT 0,
            upload_bytes INTEGER NOT NULL DEFAULT 0,
            blocked_count INTEGER NOT NULL DEFAULT 0,
            completed_count INTEGER NOT NULL DEFAULT 0,
            failed_count INTEGER NOT NULL DEFAULT 0,
            completed_at TEXT,
            last_error TEXT,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_backup_plans_workspace_created_at
            ON backup_plans(workspace_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_backup_plans_workspace_status
            ON backup_plans(workspace_id, status);

        CREATE TABLE IF NOT EXISTS backup_plan_items (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('create', 'update', 'blocked')),
            status TEXT NOT NULL CHECK(status IN (
                'ready', 'running', 'completed', 'failed', 'blocked', 'cancelled'
            )),
            local_hash TEXT,
            local_size INTEGER,
            expected_remote_present INTEGER NOT NULL DEFAULT 0 CHECK(expected_remote_present IN (0, 1)),
            expected_remote_id TEXT,
            expected_remote_checksum_type TEXT,
            expected_remote_checksum TEXT,
            completed_remote_id TEXT,
            completed_remote_checksum_type TEXT,
            completed_remote_checksum TEXT,
            block_reason TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(plan_id, relative_path),
            FOREIGN KEY(plan_id) REFERENCES backup_plans(id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_backup_plan_items_plan_status
            ON backup_plan_items(plan_id, status);
        CREATE INDEX IF NOT EXISTS idx_backup_plan_items_workspace_status
            ON backup_plan_items(workspace_id, status);",
    )
}

fn migrate_to_v2(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "ALTER TABLE file_entries
            ADD COLUMN remote_present INTEGER NOT NULL DEFAULT 0 CHECK(remote_present IN (0, 1));
        ALTER TABLE file_entries ADD COLUMN remote_checksum_type TEXT;
        ALTER TABLE file_entries ADD COLUMN remote_checksum TEXT;
        ALTER TABLE file_entries ADD COLUMN last_synced_remote_checksum_type TEXT;
        ALTER TABLE file_entries ADD COLUMN last_synced_remote_checksum TEXT;
        ALTER TABLE file_entries ADD COLUMN last_remote_seen_at TEXT;
        ALTER TABLE file_entries ADD COLUMN last_remote_scan_id INTEGER;

        UPDATE file_entries
        SET remote_present = CASE
            WHEN remote_id IS NOT NULL OR remote_hash IS NOT NULL THEN 1
            ELSE 0
        END;

        CREATE INDEX IF NOT EXISTS idx_file_entries_workspace_remote_present
            ON file_entries(workspace_id, remote_present);

        CREATE TABLE IF NOT EXISTS provider_connections (
            id TEXT PRIMARY KEY,
            provider_type TEXT NOT NULL CHECK(provider_type IN ('google_drive')),
            display_name TEXT NOT NULL,
            account_label TEXT,
            created_at TEXT NOT NULL,
            last_verified_at TEXT
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_connections_type
            ON provider_connections(provider_type);

        CREATE TABLE IF NOT EXISTS workspace_remote_bindings (
            workspace_id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            last_inventory_at TEXT,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE,
            UNIQUE(provider_id, remote_path)
        );

        CREATE TABLE IF NOT EXISTS remote_scan_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            scanned_at TEXT NOT NULL,
            file_count INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_remote_scan_runs_workspace_scanned_at
            ON remote_scan_runs(workspace_id, scanned_at DESC);",
    )
}

fn migrate_to_v3(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS backup_plans (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN (
                'ready', 'running', 'completed', 'partial', 'failed', 'cancelled'
            )),
            created_at TEXT NOT NULL,
            local_scan_at TEXT NOT NULL,
            remote_inventory_at TEXT NOT NULL,
            upload_count INTEGER NOT NULL DEFAULT 0,
            upload_bytes INTEGER NOT NULL DEFAULT 0,
            blocked_count INTEGER NOT NULL DEFAULT 0,
            completed_count INTEGER NOT NULL DEFAULT 0,
            failed_count INTEGER NOT NULL DEFAULT 0,
            completed_at TEXT,
            last_error TEXT,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
            FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_backup_plans_workspace_created_at
            ON backup_plans(workspace_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_backup_plans_workspace_status
            ON backup_plans(workspace_id, status);

        CREATE TABLE IF NOT EXISTS backup_plan_items (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('create', 'update', 'blocked')),
            status TEXT NOT NULL CHECK(status IN (
                'ready', 'running', 'completed', 'failed', 'blocked', 'cancelled'
            )),
            local_hash TEXT,
            local_size INTEGER,
            expected_remote_present INTEGER NOT NULL DEFAULT 0 CHECK(expected_remote_present IN (0, 1)),
            expected_remote_id TEXT,
            expected_remote_checksum_type TEXT,
            expected_remote_checksum TEXT,
            completed_remote_id TEXT,
            completed_remote_checksum_type TEXT,
            completed_remote_checksum TEXT,
            block_reason TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(plan_id, relative_path),
            FOREIGN KEY(plan_id) REFERENCES backup_plans(id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_backup_plan_items_plan_status
            ON backup_plan_items(plan_id, status);
        CREATE INDEX IF NOT EXISTS idx_backup_plan_items_workspace_status
            ON backup_plan_items(workspace_id, status);",
    )
}

fn migrate_legacy_workspaces(app: &AppHandle, connection: &mut Connection) -> Result<(), String> {
    let already_imported = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            params![LEGACY_IMPORT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not inspect legacy migration state: {error}"))?
        .is_some();

    if already_imported {
        return Ok(());
    }

    let legacy_path = app_data_dir(app)?.join(LEGACY_WORKSPACE_FILE);
    let legacy_workspaces = if legacy_path.exists() {
        let content = fs::read_to_string(&legacy_path)
            .map_err(|error| format!("Could not read legacy workspace metadata: {error}"))?;
        if content.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str::<Vec<Workspace>>(&content)
                .map_err(|error| format!("Legacy workspace metadata is invalid: {error}"))?
        }
    } else {
        Vec::new()
    };

    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start legacy workspace migration: {error}"))?;

    for workspace in legacy_workspaces {
        transaction
            .execute(
                "INSERT OR IGNORE INTO workspaces (
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
            .map_err(|error| format!("Could not import legacy workspace metadata: {error}"))?;
    }

    transaction
        .execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, '1')",
            params![LEGACY_IMPORT_KEY],
        )
        .map_err(|error| format!("Could not finalize legacy workspace migration: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit legacy workspace migration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_phase_two_database_to_backup_plan_schema() {
        let mut connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch(
                "CREATE TABLE workspaces (id TEXT PRIMARY KEY);
                 CREATE TABLE file_entries (
                    workspace_id TEXT NOT NULL,
                    relative_path TEXT NOT NULL,
                    remote_id TEXT,
                    remote_hash TEXT
                 );
                 PRAGMA user_version = 1;",
            )
            .expect("phase two schema");

        migrate_schema(&mut connection).expect("phase four migration");

        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        let remote_present: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('file_entries') WHERE name = 'remote_present'",
                [],
                |row| row.get(0),
            )
            .expect("column");
        let provider_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'provider_connections'",
                [],
                |row| row.get(0),
            )
            .expect("provider table");
        let plan_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'backup_plans'",
                [],
                |row| row.get(0),
            )
            .expect("plan table");
        let plan_item_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'backup_plan_items'",
                [],
                |row| row.get(0),
            )
            .expect("plan item table");

        assert_eq!(version, 3);
        assert_eq!(remote_present, 1);
        assert_eq!(provider_table, 1);
        assert_eq!(plan_table, 1);
        assert_eq!(plan_item_table, 1);
    }

    #[test]
    fn migrates_phase_three_database_to_backup_plan_schema() {
        let mut connection = Connection::open_in_memory().expect("database");
        initialize_schema_without_phase_four(&connection).expect("phase three schema");
        connection
            .execute_batch("PRAGMA user_version = 2;")
            .expect("version");

        migrate_schema(&mut connection).expect("phase four migration");

        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        let plan_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'backup_plans'",
                [],
                |row| row.get(0),
            )
            .expect("plan table");

        assert_eq!(version, 3);
        assert_eq!(plan_table, 1);
    }

    fn initialize_schema_without_phase_four(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute_batch(
            "CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                local_path TEXT NOT NULL UNIQUE,
                sync_mode TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_scan_at TEXT
            );
            CREATE TABLE provider_connections (
                id TEXT PRIMARY KEY,
                provider_type TEXT NOT NULL,
                display_name TEXT NOT NULL,
                account_label TEXT,
                created_at TEXT NOT NULL,
                last_verified_at TEXT
            );
            CREATE TABLE workspace_remote_bindings (
                workspace_id TEXT PRIMARY KEY,
                provider_id TEXT NOT NULL,
                remote_path TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_inventory_at TEXT
            );",
        )
    }
}
