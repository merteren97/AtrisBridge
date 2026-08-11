use chrono::Utc;
use rusqlite::{params, Connection};
use tauri::AppHandle;

use crate::database::open_database;

pub fn recover_interrupted_plans(app: &AppHandle) -> Result<(), String> {
    let mut connection = open_database(app)?;
    recover_interrupted_plans_with_connection(&mut connection)
}

fn recover_interrupted_plans_with_connection(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start backup recovery transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();

    transaction
        .execute(
            "UPDATE backup_plan_items
             SET status = 'failed',
                 last_error = COALESCE(last_error, 'AtrisBridge closed while this upload was running. Prepare a fresh backup plan before retrying.'),
                 updated_at = ?1
             WHERE status = 'running'",
            params![now],
        )
        .map_err(|error| format!("Could not recover interrupted backup items: {error}"))?;

    transaction
        .execute(
            "UPDATE backup_plans
             SET status = 'partial',
                 completed_at = ?1,
                 last_error = COALESCE(last_error, 'Previous backup execution was interrupted. Remote state must be observed again before another upload.')
             WHERE status = 'running'",
            params![now],
        )
        .map_err(|error| format!("Could not recover interrupted backup plans: {error}"))?;

    transaction
        .commit()
        .map_err(|error| format!("Could not commit interrupted backup recovery: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    #[test]
    fn interrupted_running_plan_is_retired_safely() {
        let mut connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("foreign keys");
        database::migrate_schema(&mut connection).expect("schema");
        connection
            .execute_batch(
                "INSERT INTO workspaces (id, name, local_path, sync_mode, created_at)
                 VALUES ('workspace-1', 'Project', '/tmp/project', 'backup', '2026-08-11T08:00:00Z');
                 INSERT INTO provider_connections (id, provider_type, display_name, created_at)
                 VALUES ('provider-1', 'google_drive', 'Google Drive', '2026-08-11T08:00:00Z');
                 INSERT INTO backup_plans (
                    id, workspace_id, provider_id, remote_path, status,
                    created_at, local_scan_at, remote_inventory_at
                 ) VALUES (
                    'plan-1', 'workspace-1', 'provider-1', 'AtrisBridge/Project', 'running',
                    '2026-08-11T08:01:00Z', '2026-08-11T08:01:00Z', '2026-08-11T08:01:00Z'
                 );
                 INSERT INTO backup_plan_items (
                    id, plan_id, workspace_id, relative_path, action, status,
                    local_hash, local_size, created_at, updated_at
                 ) VALUES (
                    'item-1', 'plan-1', 'workspace-1', 'src/main.rs', 'create', 'running',
                    'hash', 12, '2026-08-11T08:01:00Z', '2026-08-11T08:01:00Z'
                 );",
            )
            .expect("seed");

        recover_interrupted_plans_with_connection(&mut connection).expect("recovery");

        let plan_status: String = connection
            .query_row(
                "SELECT status FROM backup_plans WHERE id = 'plan-1'",
                [],
                |row| row.get(0),
            )
            .expect("plan status");
        let item_status: String = connection
            .query_row(
                "SELECT status FROM backup_plan_items WHERE id = 'item-1'",
                [],
                |row| row.get(0),
            )
            .expect("item status");

        assert_eq!(plan_status, "partial");
        assert_eq!(item_status, "failed");
    }
}
