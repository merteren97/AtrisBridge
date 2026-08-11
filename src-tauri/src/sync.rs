use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    database::open_database,
    models::{RemoteFileObservation, RemoteInventoryReport, SyncMode, Workspace},
    provider_sessions::ProviderSessionStore,
    provider_storage, scanner,
    storage::{find_workspace, record_scan},
    transport::rclone,
};

const PREVIEW_LIMIT: usize = 160;
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlanItem {
    pub id: String,
    pub relative_path: String,
    pub action: String,
    pub status: String,
    pub size: Option<u64>,
    pub reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub id: String,
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
    pub status: String,
    pub created_at: String,
    pub local_scan_at: String,
    pub remote_inventory_at: String,
    pub upload_count: u64,
    pub download_count: u64,
    pub delete_count: u64,
    pub conflict_count: u64,
    pub blocked_count: u64,
    pub transfer_bytes: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub completed_at: Option<String>,
    pub preview_truncated: bool,
    pub items: Vec<SyncPlanItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncExecutionReport {
    pub plan_id: String,
    pub status: String,
    pub completed_count: u64,
    pub failed_count: u64,
    pub transferred_bytes: u64,
    pub finished_at: String,
}

#[derive(Debug)]
struct FileEvidence {
    relative_path: String,
    local_present: bool,
    local_size: Option<u64>,
    local_hash: Option<String>,
    remote_present: bool,
    remote_id: Option<String>,
    remote_size: Option<u64>,
    remote_checksum_type: Option<String>,
    remote_checksum: Option<String>,
    last_synced_hash: Option<String>,
    last_synced_remote_checksum_type: Option<String>,
    last_synced_remote_checksum: Option<String>,
}

#[derive(Debug, Clone)]
struct SyncOperation {
    id: String,
    workspace_id: String,
    relative_path: String,
    action: String,
    expected_local_present: bool,
    expected_local_hash: Option<String>,
    expected_local_size: Option<u64>,
    expected_remote_present: bool,
    expected_remote_id: Option<String>,
    expected_remote_size: Option<u64>,
    expected_remote_checksum_type: Option<String>,
    expected_remote_checksum: Option<String>,
    baseline_local_hash: Option<String>,
    baseline_remote_checksum_type: Option<String>,
    baseline_remote_checksum: Option<String>,
}

#[derive(Debug, Clone)]
struct SyncContext {
    workspace_id: String,
    provider_id: String,
    remote_path: String,
}

#[derive(Debug)]
struct InterruptedSyncItem {
    id: String,
    workspace_id: String,
    relative_path: String,
    action: String,
    status: String,
    downloaded_hash: Option<String>,
    downloaded_size: Option<u64>,
    recovery_path: Option<String>,
}

enum PlanDecision {
    Skip,
    Action(&'static str),
    Conflict(String),
    Blocked(String),
}

#[tauri::command]
pub fn set_workspace_sync_mode(
    app: AppHandle,
    id: String,
    mode: SyncMode,
) -> Result<Workspace, String> {
    let mut connection = open_sync_database(&app)?;
    let active_sync: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_plans WHERE workspace_id = ?1 AND status = 'running'",
            params![id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active sync plan: {error}"))?;
    if active_sync > 0 {
        return Err("A two-way synchronization is currently running for this workspace.".into());
    }
    let active_backup: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM backup_plans WHERE workspace_id = ?1 AND status = 'running'",
            params![id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active backup plan: {error}"))?;
    if active_backup > 0 {
        return Err("A backup is currently running for this workspace.".into());
    }
    if table_exists(&connection, "restore_plans")? {
        let active_restore: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM restore_plans WHERE workspace_id = ?1 AND status = 'running'",
                params![id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active restore plan: {error}"))?;
        if active_restore > 0 {
            return Err("A restore is currently running for this workspace.".into());
        }
    }

    let now = Utc::now().to_rfc3339();
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start workspace mode transaction: {error}"))?;
    let changed = transaction
        .execute(
            "UPDATE workspaces SET sync_mode = ?1 WHERE id = ?2",
            params![mode.as_str(), id],
        )
        .map_err(|error| format!("Could not update workspace sync mode: {error}"))?;
    if changed == 0 {
        return Err("Workspace was not found.".into());
    }

    if matches!(mode, SyncMode::TwoWay) {
        transaction
            .execute(
                "UPDATE backup_plans
                 SET status = 'cancelled', completed_at = ?1
                 WHERE workspace_id = ?2 AND status = 'ready'",
                params![now, id],
            )
            .map_err(|error| format!("Could not retire stale backup plans: {error}"))?;
        if table_exists(&transaction, "restore_plans")? {
            transaction
                .execute(
                    "UPDATE restore_plan_items
                     SET status = 'cancelled', updated_at = ?1
                     WHERE plan_id IN (
                        SELECT id FROM restore_plans WHERE workspace_id = ?2 AND status = 'ready'
                     ) AND status = 'ready'",
                    params![now, id],
                )
                .map_err(|error| format!("Could not retire stale restore items: {error}"))?;
            transaction
                .execute(
                    "UPDATE restore_plans
                     SET status = 'cancelled', completed_at = ?1
                     WHERE workspace_id = ?2 AND status = 'ready'",
                    params![now, id],
                )
                .map_err(|error| format!("Could not retire stale restore plans: {error}"))?;
        }
    } else {
        transaction
            .execute(
                "UPDATE sync_plan_items
                 SET status = 'cancelled', updated_at = ?1
                 WHERE plan_id IN (
                    SELECT id FROM sync_plans WHERE workspace_id = ?2 AND status = 'ready'
                 ) AND status = 'ready'",
                params![now, id],
            )
            .map_err(|error| format!("Could not retire stale two-way items: {error}"))?;
        transaction
            .execute(
                "UPDATE sync_plans
                 SET status = 'cancelled', completed_at = ?1
                 WHERE workspace_id = ?2 AND status = 'ready'",
                params![now, id],
            )
            .map_err(|error| format!("Could not retire stale two-way plans: {error}"))?;
    }

    transaction
        .commit()
        .map_err(|error| format!("Could not commit workspace mode change: {error}"))?;
    find_workspace(&app, &id)
}

#[tauri::command]
pub fn latest_sync_plan(app: AppHandle, id: String) -> Result<Option<SyncPlan>, String> {
    let connection = open_sync_database(&app)?;
    latest_plan_with_connection(&connection, &id)
}

#[tauri::command]
pub async fn prepare_sync_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<SyncPlan, String> {
    let workspace = find_workspace(&app, &id)?;
    if !matches!(workspace.sync_mode, SyncMode::TwoWay) {
        return Err(
            "Switch this workspace to Two-Way mode before preparing a Phase 6 sync plan.".into(),
        );
    }
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("Phase 6 currently supports Google Drive two-way synchronization only.".into());
    }
    ensure_managed_root(&binding.remote_path)?;
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive session is not active. Reconnect before preparing two-way sync.".to_string()
    })?;

    refresh_local_inventory(&app, &id).await?;
    refresh_remote_inventory(&app, &id, token).await?;

    let mut connection = open_sync_database(&app)?;
    create_plan_with_connection(&mut connection, &id)
}

#[tauri::command]
pub async fn execute_sync_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<SyncExecutionReport, String> {
    let context = execution_context(&app, &plan_id)?;
    ensure_managed_root(&context.remote_path)?;
    let token = sessions
        .google_drive_token(&context.provider_id)?
        .ok_or_else(|| {
            "Google Drive session is not active. Reconnect before two-way sync.".to_string()
        })?;

    refresh_local_inventory(&app, &context.workspace_id).await?;
    refresh_remote_inventory(&app, &context.workspace_id, token.clone()).await?;

    let context = execution_context(&app, &plan_id)?;
    let operations = begin_execution(&app, &plan_id)?;
    for operation in operations {
        if let Err(error) = mark_operation_running(&app, &operation.id) {
            let _ = fail_operation(&app, &operation.id, &error);
            continue;
        }
        if let Err(error) = execute_operation(&app, &context, &operation, &token).await {
            let _ = fail_operation(&app, &operation.id, &error);
        }
    }

    finalize_plan(&app, &plan_id)
}

pub fn recover_interrupted_syncs(app: &AppHandle) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let interrupted = load_interrupted_items(&connection)?;
    let mut messages = Vec::new();

    for item in interrupted {
        let workspace = match find_workspace(app, &item.workspace_id) {
            Ok(workspace) => workspace,
            Err(error) => {
                messages.push((
                    item.id,
                    format!("Interrupted two-way sync could not inspect its workspace: {error}"),
                ));
                continue;
            }
        };
        let message = match recover_interrupted_item(app, &workspace, &item) {
            Ok(message) => message,
            Err(error) => format!(
                "Interrupted two-way sync requires manual inspection. AtrisBridge preserved uncertain data: {error}"
            ),
        };
        messages.push((item.id, message));
    }

    let now = Utc::now().to_rfc3339();
    for (item_id, message) in messages {
        connection
            .execute(
                "UPDATE sync_plan_items
                 SET status = 'failed', last_error = ?1, updated_at = ?2
                 WHERE id = ?3 AND status IN ('running', 'applying')",
                params![truncate_error(&message), now, item_id],
            )
            .map_err(|error| format!("Could not record interrupted two-way recovery: {error}"))?;
    }
    connection
        .execute(
            "UPDATE sync_plans
             SET status = 'partial', completed_at = ?1,
                 last_error = COALESCE(last_error,
                    'Previous two-way synchronization was interrupted. Prepare a fresh plan before retrying.')
             WHERE status = 'running'",
            params![now],
        )
        .map_err(|error| format!("Could not retire interrupted two-way sync plans: {error}"))?;
    Ok(())
}

async fn refresh_local_inventory(app: &AppHandle, id: &str) -> Result<(), String> {
    let workspace = find_workspace(app, id)?;
    let root = PathBuf::from(workspace.local_path);
    let workspace_id = id.to_string();
    let outcome = tauri::async_runtime::spawn_blocking(move || scanner::scan(&workspace_id, &root))
        .await
        .map_err(|error| format!("Local scan worker failed: {error}"))??;
    record_scan(app, &outcome.report, &outcome.inventory)
}

async fn refresh_remote_inventory(app: &AppHandle, id: &str, token: String) -> Result<(), String> {
    let (provider, binding) = provider_storage::get_provider_for_workspace(app, id)?;
    if provider.provider_type != "google_drive" {
        return Err("This provider is not supported by the Phase 6 adapter.".into());
    }
    let inventory_app = app.clone();
    let inventory_path = binding.remote_path.clone();
    let observations = tauri::async_runtime::spawn_blocking(move || {
        rclone::list_google_drive_files(&inventory_app, &token, &inventory_path)
    })
    .await
    .map_err(|error| format!("Remote inventory worker failed: {error}"))??;
    let total_bytes = observations.iter().try_fold(0_u64, |sum, item| {
        sum.checked_add(item.size)
            .ok_or_else(|| "Remote inventory size exceeded supported range.".to_string())
    })?;
    let report = RemoteInventoryReport {
        workspace_id: id.to_string(),
        provider_id: provider.id,
        remote_path: binding.remote_path,
        scanned_at: Utc::now().to_rfc3339(),
        file_count: observations.len() as u64,
        total_bytes,
    };
    provider_storage::record_remote_inventory(app, &report, &observations)
}

fn open_sync_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_sync_schema(&connection)?;
    Ok(connection)
}

fn ensure_sync_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_plans (
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
                download_count INTEGER NOT NULL DEFAULT 0,
                delete_count INTEGER NOT NULL DEFAULT 0,
                conflict_count INTEGER NOT NULL DEFAULT 0,
                blocked_count INTEGER NOT NULL DEFAULT 0,
                transfer_bytes INTEGER NOT NULL DEFAULT 0,
                completed_count INTEGER NOT NULL DEFAULT 0,
                failed_count INTEGER NOT NULL DEFAULT 0,
                completed_at TEXT,
                last_error TEXT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
                FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_sync_plans_workspace_created_at
                ON sync_plans(workspace_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_sync_plans_workspace_status
                ON sync_plans(workspace_id, status);

            CREATE TABLE IF NOT EXISTS sync_plan_items (
                id TEXT PRIMARY KEY,
                plan_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                action TEXT NOT NULL CHECK(action IN (
                    'upload_create', 'upload_update',
                    'download_create', 'download_update',
                    'remote_trash', 'local_delete', 'acknowledge_delete',
                    'conflict', 'blocked'
                )),
                status TEXT NOT NULL CHECK(status IN (
                    'ready', 'running', 'applying', 'completed', 'failed',
                    'conflict', 'blocked', 'cancelled'
                )),
                expected_local_present INTEGER NOT NULL CHECK(expected_local_present IN (0,1)),
                expected_local_hash TEXT,
                expected_local_size INTEGER,
                expected_remote_present INTEGER NOT NULL CHECK(expected_remote_present IN (0,1)),
                expected_remote_id TEXT,
                expected_remote_size INTEGER,
                expected_remote_checksum_type TEXT,
                expected_remote_checksum TEXT,
                baseline_local_hash TEXT,
                baseline_remote_checksum_type TEXT,
                baseline_remote_checksum TEXT,
                downloaded_local_hash TEXT,
                downloaded_local_size INTEGER,
                recovery_path TEXT,
                reason TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(plan_id, relative_path),
                FOREIGN KEY(plan_id) REFERENCES sync_plans(id) ON DELETE CASCADE,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_sync_items_plan_status
                ON sync_plan_items(plan_id, status);
            CREATE INDEX IF NOT EXISTS idx_sync_items_workspace_status
                ON sync_plan_items(workspace_id, status);

            CREATE TABLE IF NOT EXISTS sync_recovery_entries (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                source_plan_id TEXT NOT NULL,
                source_item_id TEXT NOT NULL UNIQUE,
                recovery_path TEXT NOT NULL,
                original_hash TEXT NOT NULL,
                original_size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                restored_at TEXT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_recovery_source_item
                ON sync_recovery_entries(source_item_id);
            CREATE INDEX IF NOT EXISTS idx_sync_recovery_workspace_created
                ON sync_recovery_entries(workspace_id, created_at DESC);",
        )
        .map_err(|error| format!("Could not initialize Phase 6 sync journal: {error}"))
}

fn create_plan_with_connection(
    connection: &mut Connection,
    workspace_id: &str,
) -> Result<SyncPlan, String> {
    ensure_sync_schema(connection)?;
    let metadata = connection
        .query_row(
            "SELECT w.sync_mode, w.last_scan_at, w.local_path,
                    b.provider_id, b.remote_path, b.last_inventory_at
             FROM workspaces w
             LEFT JOIN workspace_remote_bindings b ON b.workspace_id = w.id
             WHERE w.id = ?1",
            params![workspace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read two-way planning metadata: {error}"))?
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    if metadata.0 != "two_way" {
        return Err("Only Two-Way workspaces can prepare a Phase 6 sync plan.".into());
    }
    let local_scan_at = metadata
        .1
        .ok_or_else(|| "Run a fresh local scan before preparing two-way sync.".to_string())?;
    let workspace_root = PathBuf::from(metadata.2);
    let provider_id = metadata
        .3
        .ok_or_else(|| "Bind this workspace to Google Drive before two-way sync.".to_string())?;
    let remote_path = metadata
        .4
        .ok_or_else(|| "Workspace remote path is missing.".to_string())?;
    ensure_managed_root(&remote_path)?;
    let remote_inventory_at = metadata
        .5
        .ok_or_else(|| "Read a fresh remote inventory before two-way sync.".to_string())?;

    ensure_no_other_transfer_running(connection, workspace_id)?;

    let evidence = load_file_evidence(connection, workspace_id)?;
    let collision_keys = portable_collision_keys(&evidence);
    let plan_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut upload_count = 0_u64;
    let mut download_count = 0_u64;
    let mut delete_count = 0_u64;
    let mut conflict_count = 0_u64;
    let mut blocked_count = 0_u64;
    let mut transfer_bytes = 0_u64;

    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start two-way planning transaction: {error}"))?;
    transaction
        .execute(
            "UPDATE sync_plan_items
             SET status = 'cancelled', updated_at = ?1
             WHERE plan_id IN (
                SELECT id FROM sync_plans WHERE workspace_id = ?2 AND status = 'ready'
             ) AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous sync items: {error}"))?;
    transaction
        .execute(
            "UPDATE sync_plans SET status = 'cancelled', completed_at = ?1
             WHERE workspace_id = ?2 AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous sync plan: {error}"))?;
    transaction
        .execute(
            "INSERT INTO sync_plans (
                id, workspace_id, provider_id, remote_path, status,
                created_at, local_scan_at, remote_inventory_at
             ) VALUES (?1, ?2, ?3, ?4, 'ready', ?5, ?6, ?7)",
            params![
                plan_id,
                workspace_id,
                provider_id,
                remote_path,
                now,
                local_scan_at,
                remote_inventory_at,
            ],
        )
        .map_err(|error| format!("Could not create two-way sync plan: {error}"))?;

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO sync_plan_items (
                    id, plan_id, workspace_id, relative_path, action, status,
                    expected_local_present, expected_local_hash, expected_local_size,
                    expected_remote_present, expected_remote_id, expected_remote_size,
                    expected_remote_checksum_type, expected_remote_checksum,
                    baseline_local_hash, baseline_remote_checksum_type, baseline_remote_checksum,
                    reason, created_at, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6,
                    ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?19
                 )",
            )
            .map_err(|error| format!("Could not prepare two-way sync item insert: {error}"))?;

        for file in evidence {
            let collides = collision_keys.contains(&portable_path_key(&file.relative_path));
            let ignored = if file.local_present || file.remote_present {
                scanner::is_path_ignored_for_sync(&workspace_root, &file.relative_path)?
            } else {
                false
            };
            let decision = if ignored {
                PlanDecision::Blocked(
                    "Path is excluded by AtrisBridge built-in safety rules or .atrisbridgeignore. Two-way sync will not recreate, upload, or delete excluded content."
                        .into(),
                )
            } else {
                classify(&file, collides)
            };

            let (action, status, reason) = match decision {
                PlanDecision::Skip => continue,
                PlanDecision::Action(action) => {
                    match action {
                        "upload_create" | "upload_update" => {
                            upload_count = checked_inc(upload_count, "upload count")?;
                            let size = file.local_size.ok_or_else(|| {
                                format!("Local size disappeared for {}.", file.relative_path)
                            })?;
                            transfer_bytes = checked_add(transfer_bytes, size, "transfer bytes")?;
                        }
                        "download_create" | "download_update" => {
                            download_count = checked_inc(download_count, "download count")?;
                            let size = file.remote_size.ok_or_else(|| {
                                format!("Remote size disappeared for {}.", file.relative_path)
                            })?;
                            transfer_bytes = checked_add(transfer_bytes, size, "transfer bytes")?;
                        }
                        "remote_trash" | "local_delete" => {
                            delete_count = checked_inc(delete_count, "delete count")?;
                        }
                        "acknowledge_delete" => {
                            delete_count = checked_inc(delete_count, "delete convergence count")?;
                        }
                        _ => return Err("Unsupported Phase 6 plan action.".into()),
                    }
                    (action, "ready", None)
                }
                PlanDecision::Conflict(reason) => {
                    conflict_count = checked_inc(conflict_count, "conflict count")?;
                    ("conflict", "conflict", Some(reason))
                }
                PlanDecision::Blocked(reason) => {
                    blocked_count = checked_inc(blocked_count, "blocked count")?;
                    ("blocked", "blocked", Some(reason))
                }
            };

            statement
                .execute(params![
                    Uuid::new_v4().to_string(),
                    plan_id,
                    workspace_id,
                    file.relative_path,
                    action,
                    status,
                    if file.local_present { 1 } else { 0 },
                    file.local_hash,
                    file.local_size
                        .map(|value| to_i64(value, "local size"))
                        .transpose()?,
                    if file.remote_present { 1 } else { 0 },
                    file.remote_id,
                    file.remote_size
                        .map(|value| to_i64(value, "remote size"))
                        .transpose()?,
                    file.remote_checksum_type,
                    file.remote_checksum,
                    file.last_synced_hash,
                    file.last_synced_remote_checksum_type,
                    file.last_synced_remote_checksum,
                    reason,
                    now,
                ])
                .map_err(|error| format!("Could not add two-way sync item: {error}"))?;
        }
    }

    let actionable = upload_count
        .checked_add(download_count)
        .and_then(|value| value.checked_add(delete_count))
        .ok_or_else(|| "Two-way action count exceeded supported range.".to_string())?;
    let acknowledge_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM sync_plan_items WHERE plan_id = ?1 AND action = 'acknowledge_delete'",
            params![plan_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not count converged deletions: {error}"))?;
    let has_ready = actionable > 0 || acknowledge_count > 0;
    let final_status = if has_ready {
        "ready"
    } else if conflict_count > 0 || blocked_count > 0 {
        "partial"
    } else {
        "completed"
    };
    let completed_at = if has_ready { None } else { Some(now.as_str()) };

    transaction
        .execute(
            "UPDATE sync_plans
             SET status = ?1,
                 upload_count = ?2, download_count = ?3, delete_count = ?4,
                 conflict_count = ?5, blocked_count = ?6, transfer_bytes = ?7,
                 completed_at = ?8
             WHERE id = ?9",
            params![
                final_status,
                to_i64(upload_count, "upload count")?,
                to_i64(download_count, "download count")?,
                to_i64(delete_count, "delete count")?,
                to_i64(conflict_count, "conflict count")?,
                to_i64(blocked_count, "blocked count")?,
                to_i64(transfer_bytes, "transfer bytes")?,
                completed_at,
                plan_id,
            ],
        )
        .map_err(|error| format!("Could not finalize two-way sync plan: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit two-way sync plan: {error}"))?;

    load_plan_with_connection(connection, &plan_id)
}

fn classify(file: &FileEvidence, portable_collision: bool) -> PlanDecision {
    if let Err(reason) = validate_portable_relative_path(&file.relative_path) {
        return PlanDecision::Blocked(reason);
    }
    if portable_collision {
        return PlanDecision::Blocked(
            "Multiple paths collide on a case-insensitive filesystem. AtrisBridge will not choose one automatically."
                .into(),
        );
    }

    let has_any_baseline = file.last_synced_hash.is_some()
        || file.last_synced_remote_checksum_type.is_some()
        || file.last_synced_remote_checksum.is_some();
    let baseline_complete = file.last_synced_hash.is_some()
        && has_valid_md5(
            file.last_synced_remote_checksum_type.as_deref(),
            file.last_synced_remote_checksum.as_deref(),
        );

    if !has_any_baseline {
        return match (file.local_present, file.remote_present) {
            (false, false) => PlanDecision::Skip,
            (true, false) => {
                if file.local_hash.is_none() || file.local_size.is_none() {
                    PlanDecision::Blocked("Local file has incomplete fingerprint evidence.".into())
                } else {
                    PlanDecision::Action("upload_create")
                }
            }
            (false, true) => {
                if valid_remote_evidence(file) {
                    PlanDecision::Action("download_create")
                } else {
                    PlanDecision::Blocked(
                        "Remote-only file has incomplete Google Drive ID/size/MD5 evidence.".into(),
                    )
                }
            }
            (true, true) => PlanDecision::Blocked(
                "Local and remote files overlap without an AtrisBridge synchronized baseline."
                    .into(),
            ),
        };
    }

    if !baseline_complete {
        return PlanDecision::Blocked(
            "The existing synchronized baseline is incomplete. AtrisBridge will not infer two-way intent from partial historical evidence."
                .into(),
        );
    }

    let baseline_local = file.last_synced_hash.as_deref().unwrap_or_default();
    let baseline_remote_type = file
        .last_synced_remote_checksum_type
        .as_deref()
        .unwrap_or_default();
    let baseline_remote = file
        .last_synced_remote_checksum
        .as_deref()
        .unwrap_or_default();

    match (file.local_present, file.remote_present) {
        (true, true) => {
            if file.local_hash.is_none() || file.local_size.is_none() {
                return PlanDecision::Blocked(
                    "Local file has incomplete fingerprint evidence.".into(),
                );
            }
            if !valid_remote_evidence(file) {
                return PlanDecision::Blocked(
                    "Google Drive did not provide stable ID/size/MD5 evidence for this file."
                        .into(),
                );
            }
            let local_matches = file.local_hash.as_deref() == Some(baseline_local);
            let remote_matches = file.remote_checksum_type.as_deref() == Some(baseline_remote_type)
                && file.remote_checksum.as_deref() == Some(baseline_remote);
            match (local_matches, remote_matches) {
                (true, true) => PlanDecision::Skip,
                (false, true) => PlanDecision::Action("upload_update"),
                (true, false) => PlanDecision::Action("download_update"),
                (false, false) => PlanDecision::Conflict(
                    "Local and remote content both changed after the last synchronized baseline."
                        .into(),
                ),
            }
        }
        (false, true) => {
            if !valid_remote_evidence(file) {
                return PlanDecision::Blocked(
                    "Remote file has incomplete evidence; local deletion cannot be propagated safely."
                        .into(),
                );
            }
            let remote_matches = file.remote_checksum_type.as_deref() == Some(baseline_remote_type)
                && file.remote_checksum.as_deref() == Some(baseline_remote);
            if remote_matches {
                PlanDecision::Action("remote_trash")
            } else {
                PlanDecision::Conflict(
                    "Local file was deleted while the remote copy changed. AtrisBridge preserves the remote change."
                        .into(),
                )
            }
        }
        (true, false) => {
            if file.local_hash.is_none() || file.local_size.is_none() {
                return PlanDecision::Blocked(
                    "Local file has incomplete fingerprint evidence.".into(),
                );
            }
            if file.local_hash.as_deref() == Some(baseline_local) {
                PlanDecision::Action("local_delete")
            } else {
                PlanDecision::Conflict(
                    "Remote file was deleted while the local copy changed. AtrisBridge preserves the local change."
                        .into(),
                )
            }
        }
        (false, false) => PlanDecision::Action("acknowledge_delete"),
    }
}

fn valid_remote_evidence(file: &FileEvidence) -> bool {
    file.remote_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && file.remote_size.is_some()
        && has_valid_md5(
            file.remote_checksum_type.as_deref(),
            file.remote_checksum.as_deref(),
        )
}

fn has_valid_md5(kind: Option<&str>, hash: Option<&str>) -> bool {
    matches!(kind, Some(value) if value.eq_ignore_ascii_case("MD5"))
        && hash.is_some_and(|value| {
            value.len() == 32 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
}

fn portable_collision_keys(evidence: &[FileEvidence]) -> HashSet<String> {
    let mut names: HashMap<String, HashSet<String>> = HashMap::new();
    for file in evidence
        .iter()
        .filter(|item| item.local_present || item.remote_present)
    {
        names
            .entry(portable_path_key(&file.relative_path))
            .or_default()
            .insert(file.relative_path.clone());
    }
    names
        .into_iter()
        .filter_map(|(key, values)| (values.len() > 1).then_some(key))
        .collect()
}

fn portable_path_key(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}

fn load_file_evidence(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Vec<FileEvidence>, String> {
    let mut statement = connection
        .prepare(
            "SELECT relative_path,
                    local_present, local_size, local_hash,
                    remote_present, remote_id, remote_size,
                    remote_checksum_type, remote_checksum,
                    last_synced_hash, last_synced_remote_checksum_type,
                    last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1
             ORDER BY relative_path ASC",
        )
        .map_err(|error| format!("Could not prepare two-way evidence query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id], |row| {
            Ok(FileEvidence {
                relative_path: row.get(0)?,
                local_present: row.get::<_, i64>(1)? != 0,
                local_size: optional_u64(row, 2)?,
                local_hash: row.get(3)?,
                remote_present: row.get::<_, i64>(4)? != 0,
                remote_id: row.get(5)?,
                remote_size: optional_u64(row, 6)?,
                remote_checksum_type: row.get(7)?,
                remote_checksum: row.get(8)?,
                last_synced_hash: row.get(9)?,
                last_synced_remote_checksum_type: row.get(10)?,
                last_synced_remote_checksum: row.get(11)?,
            })
        })
        .map_err(|error| format!("Could not query two-way evidence: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read two-way evidence: {error}"))
}

fn execution_context(app: &AppHandle, plan_id: &str) -> Result<SyncContext, String> {
    let connection = open_sync_database(app)?;
    let context = connection
        .query_row(
            "SELECT p.workspace_id, p.provider_id, p.remote_path, p.status,
                    w.sync_mode, b.provider_id, b.remote_path
             FROM sync_plans p
             JOIN workspaces w ON w.id = p.workspace_id
             LEFT JOIN workspace_remote_bindings b ON b.workspace_id = p.workspace_id
             WHERE p.id = ?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read two-way execution context: {error}"))?
        .ok_or_else(|| "Two-way sync plan was not found.".to_string())?;
    if context.3 != "ready" {
        return Err(format!(
            "Two-way sync plan is {} and cannot be started. Prepare a fresh plan.",
            context.3
        ));
    }
    if context.4 != "two_way" {
        return Err(
            "Workspace is no longer in Two-Way mode. Prepare a fresh plan after changing mode."
                .into(),
        );
    }
    if context.5.as_deref() != Some(context.1.as_str())
        || context.6.as_deref() != Some(context.2.as_str())
    {
        return Err("Workspace cloud binding changed after planning. Prepare a fresh plan.".into());
    }
    ensure_no_other_transfer_running_except_sync(&connection, &context.0)?;
    let other_syncs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_plans
             WHERE workspace_id = ?1 AND status = 'running' AND id != ?2",
            params![context.0, plan_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect concurrent two-way plans: {error}"))?;
    if other_syncs > 0 {
        return Err(
            "Another two-way synchronization is already running for this workspace.".into(),
        );
    }
    Ok(SyncContext {
        workspace_id: context.0,
        provider_id: context.1,
        remote_path: context.2,
    })
}

fn begin_execution(app: &AppHandle, plan_id: &str) -> Result<Vec<SyncOperation>, String> {
    let mut connection = open_sync_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start two-way execution transaction: {error}"))?;
    let changed = transaction
        .execute(
            "UPDATE sync_plans SET status = 'running', last_error = NULL
             WHERE id = ?1 AND status = 'ready'",
            params![plan_id],
        )
        .map_err(|error| format!("Could not start two-way sync plan: {error}"))?;
    if changed == 0 {
        return Err("Two-way sync plan is no longer ready.".into());
    }
    let operations = {
        let mut statement = transaction
            .prepare(
                "SELECT id, workspace_id, relative_path, action,
                        expected_local_present, expected_local_hash, expected_local_size,
                        expected_remote_present, expected_remote_id, expected_remote_size,
                        expected_remote_checksum_type, expected_remote_checksum,
                        baseline_local_hash, baseline_remote_checksum_type, baseline_remote_checksum
                 FROM sync_plan_items
                 WHERE plan_id = ?1 AND status = 'ready'
                 ORDER BY CASE action
                    WHEN 'upload_create' THEN 1
                    WHEN 'upload_update' THEN 1
                    WHEN 'download_create' THEN 2
                    WHEN 'download_update' THEN 2
                    WHEN 'acknowledge_delete' THEN 3
                    WHEN 'remote_trash' THEN 4
                    WHEN 'local_delete' THEN 4
                    ELSE 5 END,
                    relative_path ASC",
            )
            .map_err(|error| format!("Could not prepare two-way operation query: {error}"))?;
        let rows = statement
            .query_map(params![plan_id], operation_from_row)
            .map_err(|error| format!("Could not query two-way operations: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("Could not read two-way operations: {error}"))?
    };
    transaction
        .commit()
        .map_err(|error| format!("Could not commit two-way execution start: {error}"))?;
    Ok(operations)
}

fn mark_operation_running(app: &AppHandle, operation_id: &str) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE sync_plan_items SET status = 'running', updated_at = ?1, last_error = NULL
             WHERE id = ?2 AND status = 'ready'",
            params![now, operation_id],
        )
        .map_err(|error| format!("Could not start two-way sync item: {error}"))?;
    if changed == 0 {
        return Err("Two-way sync item is no longer ready.".into());
    }
    Ok(())
}

async fn execute_operation(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    token: &str,
) -> Result<(), String> {
    ensure_current_evidence_matches_plan(app, operation)?;
    let workspace = find_workspace(app, &operation.workspace_id)?;
    if scanner::is_path_ignored_for_sync(
        Path::new(&workspace.local_path),
        &operation.relative_path,
    )? {
        return Err("Path is now excluded by AtrisBridge safety or .atrisbridgeignore rules. Prepare a fresh plan.".into());
    }

    match operation.action.as_str() {
        "upload_create" | "upload_update" => {
            execute_upload(app, context, operation, &workspace, token).await
        }
        "download_create" | "download_update" => {
            execute_download(app, context, operation, &workspace, token).await
        }
        "remote_trash" => execute_remote_trash(app, context, operation, &workspace, token).await,
        "local_delete" => execute_local_delete(app, context, operation, &workspace, token).await,
        "acknowledge_delete" => {
            execute_acknowledge_delete(app, context, operation, &workspace, token).await
        }
        _ => Err("Unsupported Phase 6 execution action.".into()),
    }
}

async fn execute_upload(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    workspace: &Workspace,
    token: &str,
) -> Result<(), String> {
    let local_hash = operation
        .expected_local_hash
        .as_deref()
        .ok_or_else(|| "Upload plan is missing local BLAKE3 evidence.".to_string())?;
    let local_size = operation
        .expected_local_size
        .ok_or_else(|| "Upload plan is missing local size evidence.".to_string())?;
    let local_path = resolve_existing_local_file(workspace, &operation.relative_path)?;
    let (actual_size, actual_hash) = scanner::fingerprint_file(&local_path)?;
    if actual_size != local_size || actual_hash != local_hash {
        return Err("Local file changed after planning. AtrisBridge blocked the upload.".into());
    }

    let remote_file_path =
        rclone::join_remote_path(&context.remote_path, &operation.relative_path)?;
    let create_only = operation.action == "upload_create";
    if create_only {
        if rclone::try_stat_google_drive_file(
            app,
            token,
            &remote_file_path,
            &operation.relative_path,
        )?
        .is_some()
        {
            return Err(
                "Remote path appeared after planning. AtrisBridge blocked the create.".into(),
            );
        }
    } else {
        let current = rclone::stat_google_drive_file(
            app,
            token,
            &remote_file_path,
            &operation.relative_path,
        )?;
        ensure_remote_matches_plan(operation, &current)?;
    }

    let upload_app = app.clone();
    let upload_token = token.to_string();
    let upload_local = local_path.clone();
    let upload_remote = remote_file_path.clone();
    let observation = tauri::async_runtime::spawn_blocking(move || {
        rclone::upload_google_drive_file(
            &upload_app,
            &upload_token,
            &upload_local,
            &upload_remote,
            create_only,
        )
    })
    .await
    .map_err(|error| format!("Two-way upload worker failed: {error}"))??;

    let (after_size, after_hash) = scanner::fingerprint_file(&local_path)?;
    if after_size != local_size || after_hash != local_hash {
        return Err("Local file changed during upload. Remote content was not accepted as a synchronized baseline.".into());
    }
    complete_content_sync(
        app,
        operation,
        &observation,
        local_hash,
        local_size,
        file_modified_at(&local_path)?,
    )
}

async fn execute_download(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    workspace: &Workspace,
    token: &str,
) -> Result<(), String> {
    let remote_file_path =
        rclone::join_remote_path(&context.remote_path, &operation.relative_path)?;
    let preflight_app = app.clone();
    let preflight_token = token.to_string();
    let preflight_path = remote_file_path.clone();
    let preflight_relative = operation.relative_path.clone();
    let preflight = tauri::async_runtime::spawn_blocking(move || {
        rclone::stat_google_drive_file(
            &preflight_app,
            &preflight_token,
            &preflight_path,
            &preflight_relative,
        )
    })
    .await
    .map_err(|error| format!("Remote download preflight worker failed: {error}"))??;
    ensure_remote_matches_plan(operation, &preflight)?;

    let target = resolve_local_target(workspace, &operation.relative_path, true)?;
    validate_download_target(operation, &target)?;
    let (stage, backup) = artifact_paths(&target, &operation.id)?;
    ensure_artifact_absent(&stage)?;
    ensure_artifact_absent(&backup)?;

    let download_app = app.clone();
    let download_token = token.to_string();
    let download_path = remote_file_path.clone();
    let download_stage = stage.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        download_remote_to_stage(
            &download_app,
            &download_token,
            &download_path,
            &download_stage,
        )
    })
    .await
    .map_err(|error| format!("Two-way download worker failed: {error}"))?;
    if let Err(error) = result {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }

    let (downloaded_size, downloaded_hash) = match scanner::fingerprint_file(&stage) {
        Ok(value) => value,
        Err(error) => {
            remove_regular_file_best_effort(&stage);
            return Err(error);
        }
    };
    let local_md5 = match rclone::local_file_md5(app, &stage) {
        Ok(value) => value,
        Err(error) => {
            remove_regular_file_best_effort(&stage);
            return Err(error);
        }
    };
    ensure_download_matches_remote(operation, downloaded_size, &local_md5).map_err(|error| {
        remove_regular_file_best_effort(&stage);
        error
    })?;

    let final_remote = match rclone::stat_google_drive_file(
        app,
        token,
        &remote_file_path,
        &operation.relative_path,
    ) {
        Ok(observation) => observation,
        Err(error) => {
            remove_regular_file_best_effort(&stage);
            return Err(error);
        }
    };
    if let Err(error) = ensure_remote_matches_plan(operation, &final_remote) {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }
    if let Err(error) = validate_download_target(operation, &target) {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }
    if let Err(error) =
        mark_operation_applying_download(app, &operation.id, &downloaded_hash, downloaded_size)
    {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }

    if let Err(error) = apply_download(operation, &target, &stage, &backup) {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }
    let (applied_size, applied_hash) = match scanner::fingerprint_file(&target) {
        Ok(value) => value,
        Err(error) => {
            return rollback_after_download_failure(
                operation,
                &target,
                &backup,
                &downloaded_hash,
                downloaded_size,
                format!("Could not verify downloaded target: {error}"),
            )
        }
    };
    if applied_size != downloaded_size || applied_hash != downloaded_hash {
        return rollback_after_download_failure(
            operation,
            &target,
            &backup,
            &downloaded_hash,
            downloaded_size,
            "Downloaded target changed during local apply.".into(),
        );
    }
    let modified_at = match file_modified_at(&target) {
        Ok(value) => value,
        Err(error) => {
            return rollback_after_download_failure(
                operation,
                &target,
                &backup,
                &downloaded_hash,
                downloaded_size,
                format!("Could not read downloaded target metadata: {error}"),
            )
        }
    };

    if let Err(error) = complete_content_sync(
        app,
        operation,
        &final_remote,
        &downloaded_hash,
        downloaded_size,
        modified_at,
    ) {
        return rollback_after_download_failure(
            operation,
            &target,
            &backup,
            &downloaded_hash,
            downloaded_size,
            format!("Could not commit synchronized download baseline: {error}"),
        );
    }
    remove_regular_file_best_effort(&backup);
    Ok(())
}

async fn execute_remote_trash(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    workspace: &Workspace,
    token: &str,
) -> Result<(), String> {
    if operation.expected_local_present || !operation.expected_remote_present {
        return Err("Remote trash plan no longer represents a local deletion.".into());
    }
    ensure_local_path_absent(workspace, &operation.relative_path)?;
    let remote_file_path =
        rclone::join_remote_path(&context.remote_path, &operation.relative_path)?;
    let preflight =
        rclone::stat_google_drive_file(app, token, &remote_file_path, &operation.relative_path)?;
    ensure_remote_matches_plan(operation, &preflight)?;

    let reviewed_id = operation
        .expected_remote_id
        .as_deref()
        .ok_or_else(|| {
            "Remote trash plan is missing the reviewed Google Drive file ID.".to_string()
        })?
        .to_string();

    // Recheck local absence immediately before the remote mutation. A file recreated
    // after the inventory must turn into a fresh conflict/plan, not an inherited delete.
    ensure_local_path_absent(workspace, &operation.relative_path)?;
    let trash_token = token.to_string();
    let trash_id = reviewed_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        trash_google_drive_file_by_id(&trash_token, &trash_id)
    })
    .await
    .map_err(|error| format!("Google Drive exact-ID trash worker failed: {error}"))??;

    let still_present = rclone::try_stat_google_drive_file(
        app,
        token,
        &remote_file_path,
        &operation.relative_path,
    )?;
    if let Some(observation) = still_present {
        return Err(format!(
            "The reviewed Drive object {} moved to Trash, but path {} is now occupied by object {}. AtrisBridge preserved that new remote state and did not converge the deletion baseline.",
            reviewed_id,
            operation.relative_path,
            observation.remote_id.as_deref().unwrap_or("unknown")
        ));
    }

    // If local content appeared while the provider request was in flight, do not clear
    // the old synchronized baseline. The next plan will expose the new local state.
    ensure_local_path_absent(workspace, &operation.relative_path)?;
    complete_deletion_convergence(app, operation, None)
}

async fn execute_local_delete(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    workspace: &Workspace,
    token: &str,
) -> Result<(), String> {
    if !operation.expected_local_present || operation.expected_remote_present {
        return Err("Local delete plan no longer represents a verified remote deletion.".into());
    }
    ensure_remote_path_absent(app, context, operation, token).await?;
    let expected_hash = operation
        .expected_local_hash
        .as_deref()
        .ok_or_else(|| "Local delete plan is missing BLAKE3 evidence.".to_string())?;
    let expected_size = operation
        .expected_local_size
        .ok_or_else(|| "Local delete plan is missing size evidence.".to_string())?;
    let target = resolve_existing_local_file(workspace, &operation.relative_path)?;
    let (size, hash) = scanner::fingerprint_file(&target)?;
    if size != expected_size || hash != expected_hash {
        return Err(
            "Local file changed after planning. AtrisBridge blocked deletion propagation.".into(),
        );
    }

    let recovery = recovery_path(app, operation)?;
    if let Some(parent) = recovery.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create local recovery directory: {error}"))?;
    }
    if fs::symlink_metadata(&recovery).is_ok() {
        return Err("A local recovery artifact already exists for this operation.".into());
    }
    if let Err(error) = fs::copy(&target, &recovery) {
        return Err(format!(
            "Could not create recoverable local delete copy: {error}"
        ));
    }
    let (recovery_size, recovery_hash) = match scanner::fingerprint_file(&recovery) {
        Ok(value) => value,
        Err(error) => {
            remove_regular_file_best_effort(&recovery);
            return Err(error);
        }
    };
    if recovery_size != expected_size || recovery_hash != expected_hash {
        remove_regular_file_best_effort(&recovery);
        return Err(
            "Local recovery copy did not match the source; deletion was not applied.".into(),
        );
    }
    if let Err(error) = File::open(&recovery).and_then(|file| file.sync_all()) {
        remove_regular_file_best_effort(&recovery);
        return Err(format!("Could not flush local recovery copy: {error}"));
    }

    // A new Drive object may appear after the inventory or even while the recovery
    // copy is being prepared. Both checks must remain absent before local deletion.
    if let Err(error) = ensure_remote_path_absent(app, context, operation, token).await {
        remove_regular_file_best_effort(&recovery);
        return Err(error);
    }
    if let Err(error) =
        mark_operation_applying_delete(app, operation, &recovery, expected_hash, expected_size)
    {
        remove_regular_file_best_effort(&recovery);
        return Err(error);
    }
    let (size_again, hash_again) = scanner::fingerprint_file(&target)?;
    if size_again != expected_size || hash_again != expected_hash {
        return Err(
            "Local file changed after recovery copy creation. AtrisBridge preserved both files."
                .into(),
        );
    }
    ensure_remote_path_absent(app, context, operation, token).await?;
    fs::remove_file(&target)
        .map_err(|error| format!("Could not apply recoverable local deletion: {error}"))?;

    if let Err(error) = complete_deletion_convergence(
        app,
        operation,
        Some((&recovery, expected_hash, expected_size)),
    ) {
        restore_local_delete_from_recovery(&target, &recovery, expected_hash, expected_size)?;
        return Err(format!(
            "Local delete journal commit failed; original file was restored from recovery: {error}"
        ));
    }
    Ok(())
}

async fn execute_acknowledge_delete(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    workspace: &Workspace,
    token: &str,
) -> Result<(), String> {
    if operation.expected_local_present || operation.expected_remote_present {
        return Err("Deletion acknowledgement no longer represents two absent copies.".into());
    }
    ensure_local_path_absent(workspace, &operation.relative_path)?;
    ensure_remote_path_absent(app, context, operation, token).await?;
    ensure_local_path_absent(workspace, &operation.relative_path)?;
    complete_deletion_convergence(app, operation, None)
}

async fn ensure_remote_path_absent(
    app: &AppHandle,
    context: &SyncContext,
    operation: &SyncOperation,
    token: &str,
) -> Result<(), String> {
    let remote_file_path =
        rclone::join_remote_path(&context.remote_path, &operation.relative_path)?;
    let check_app = app.clone();
    let check_token = token.to_string();
    let check_path = remote_file_path;
    let check_relative = operation.relative_path.clone();
    let observation = tauri::async_runtime::spawn_blocking(move || {
        rclone::try_stat_google_drive_file(&check_app, &check_token, &check_path, &check_relative)
    })
    .await
    .map_err(|error| format!("Remote deletion preflight worker failed: {error}"))??;
    if let Some(observation) = observation {
        return Err(format!(
            "Remote path {} reappeared as Drive object {} after planning. AtrisBridge blocked local deletion/convergence.",
            operation.relative_path,
            observation.remote_id.as_deref().unwrap_or("unknown")
        ));
    }
    Ok(())
}

fn ensure_local_path_absent(workspace: &Workspace, relative_path: &str) -> Result<(), String> {
    let segments = validate_portable_relative_path(relative_path)?;
    let root = PathBuf::from(&workspace.local_path)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    let (file_name, parents) = segments
        .split_last()
        .ok_or_else(|| "Sync path has no file name.".to_string())?;
    let mut parent = root.clone();
    for segment in parents {
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("Local deletion preflight crosses an unsafe parent entry.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect local deletion preflight parent: {error}"
                ));
            }
        }
        parent = next
            .canonicalize()
            .map_err(|error| format!("Could not resolve local deletion parent: {error}"))?;
        if !parent.starts_with(&root) {
            return Err("Local deletion path escaped the selected workspace.".into());
        }
    }
    let target = parent.join(file_name);
    match fs::symlink_metadata(&target) {
        Ok(_) => Err(format!(
            "Local path {} reappeared after planning. AtrisBridge blocked remote deletion/convergence.",
            relative_path
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not inspect local deletion preflight target: {error}"
        )),
    }
}

fn ensure_current_evidence_matches_plan(
    app: &AppHandle,
    operation: &SyncOperation,
) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let current = connection
        .query_row(
            "SELECT local_present, local_hash, local_size,
                    remote_present, remote_id, remote_size,
                    remote_checksum_type, remote_checksum,
                    last_synced_hash, last_synced_remote_checksum_type,
                    last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1 AND relative_path = ?2",
            params![operation.workspace_id, operation.relative_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? != 0,
                    row.get::<_, Option<String>>(1)?,
                    optional_u64(row, 2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                    optional_u64(row, 5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read current two-way evidence: {error}"))?
        .ok_or_else(|| "File is no longer present in the AtrisBridge journal.".to_string())?;

    if current.0 != operation.expected_local_present
        || current.1 != operation.expected_local_hash
        || current.2 != operation.expected_local_size
        || current.3 != operation.expected_remote_present
        || current.4 != operation.expected_remote_id
        || current.5 != operation.expected_remote_size
        || current.6 != operation.expected_remote_checksum_type
        || current.7 != operation.expected_remote_checksum
        || current.8 != operation.baseline_local_hash
        || current.9 != operation.baseline_remote_checksum_type
        || current.10 != operation.baseline_remote_checksum
    {
        return Err("Local, remote, or baseline evidence changed after planning. Prepare a fresh two-way plan.".into());
    }
    Ok(())
}

fn ensure_remote_matches_plan(
    operation: &SyncOperation,
    observation: &RemoteFileObservation,
) -> Result<(), String> {
    if !operation.expected_remote_present
        || observation.remote_id != operation.expected_remote_id
        || Some(observation.size) != operation.expected_remote_size
        || observation.checksum_type != operation.expected_remote_checksum_type
        || observation.checksum != operation.expected_remote_checksum
    {
        return Err("Google Drive evidence changed during Phase 6 safety verification.".into());
    }
    Ok(())
}

fn ensure_download_matches_remote(
    operation: &SyncOperation,
    local_size: u64,
    local_md5: &str,
) -> Result<(), String> {
    let expected_size = operation
        .expected_remote_size
        .ok_or_else(|| "Download plan is missing remote size evidence.".to_string())?;
    let expected_type = operation
        .expected_remote_checksum_type
        .as_deref()
        .ok_or_else(|| "Download plan is missing remote checksum type.".to_string())?;
    let expected_hash = operation
        .expected_remote_checksum
        .as_deref()
        .ok_or_else(|| "Download plan is missing remote checksum.".to_string())?;
    if local_size != expected_size
        || !expected_type.eq_ignore_ascii_case("MD5")
        || !local_md5.eq_ignore_ascii_case(expected_hash)
    {
        return Err(
            "Downloaded content did not match the reviewed Google Drive size and MD5 evidence."
                .into(),
        );
    }
    Ok(())
}

fn complete_content_sync(
    app: &AppHandle,
    operation: &SyncOperation,
    observation: &RemoteFileObservation,
    local_hash: &str,
    local_size: u64,
    local_modified_at: Option<String>,
) -> Result<(), String> {
    let remote_id = observation
        .remote_id
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a file ID.".to_string())?;
    let checksum_type = observation
        .checksum_type
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a checksum type.".to_string())?;
    let checksum = observation
        .checksum
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a checksum.".to_string())?;
    if observation.size != local_size || !has_valid_md5(Some(checksum_type), Some(checksum)) {
        return Err(
            "Content synchronization completed without sufficient provider evidence.".into(),
        );
    }

    let mut connection = open_sync_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start synchronized baseline transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();
    let expected_local_size = operation
        .expected_local_size
        .map(|value| to_i64(value, "expected local size"))
        .transpose()?;
    let expected_remote_size = operation
        .expected_remote_size
        .map(|value| to_i64(value, "expected remote size"))
        .transpose()?;
    let changed = transaction
        .execute(
            "UPDATE file_entries
             SET local_present = 1,
                 local_size = ?1,
                 local_modified_at = ?2,
                 local_hash = ?3,
                 remote_present = 1,
                 remote_id = ?4,
                 remote_size = ?5,
                 remote_modified_at = ?6,
                 remote_checksum_type = ?7,
                 remote_checksum = ?8,
                 last_synced_hash = ?3,
                 last_synced_remote_checksum_type = ?7,
                 last_synced_remote_checksum = ?8,
                 last_synced_at = ?9,
                 last_seen_at = ?9,
                 last_remote_seen_at = ?9,
                 state = 'synced', tombstone = 0
             WHERE workspace_id = ?10 AND relative_path = ?11
               AND local_present = ?12
               AND local_hash IS ?13
               AND local_size IS ?14
               AND remote_present = ?15
               AND remote_id IS ?16
               AND remote_size IS ?17
               AND remote_checksum_type IS ?18
               AND remote_checksum IS ?19
               AND last_synced_hash IS ?20
               AND last_synced_remote_checksum_type IS ?21
               AND last_synced_remote_checksum IS ?22",
            params![
                to_i64(local_size, "local synchronized size")?,
                local_modified_at,
                local_hash,
                remote_id,
                to_i64(observation.size, "remote synchronized size")?,
                observation.modified_at,
                checksum_type,
                checksum,
                now,
                operation.workspace_id,
                operation.relative_path,
                if operation.expected_local_present {
                    1
                } else {
                    0
                },
                operation.expected_local_hash,
                expected_local_size,
                if operation.expected_remote_present {
                    1
                } else {
                    0
                },
                operation.expected_remote_id,
                expected_remote_size,
                operation.expected_remote_checksum_type,
                operation.expected_remote_checksum,
                operation.baseline_local_hash,
                operation.baseline_remote_checksum_type,
                operation.baseline_remote_checksum,
            ],
        )
        .map_err(|error| format!("Could not establish synchronized baseline: {error}"))?;
    if changed == 0 {
        return Err(
            "Local, remote, or baseline journal evidence changed before synchronization completion."
                .into(),
        );
    }
    complete_item_in_transaction(&transaction, &operation.id, &now)?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit synchronized baseline: {error}"))
}

fn complete_deletion_convergence(
    app: &AppHandle,
    operation: &SyncOperation,
    recovery: Option<(&Path, &str, u64)>,
) -> Result<(), String> {
    let mut connection = open_sync_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start deletion convergence transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();
    let expected_local_size = operation
        .expected_local_size
        .map(|value| to_i64(value, "expected local size"))
        .transpose()?;
    let expected_remote_size = operation
        .expected_remote_size
        .map(|value| to_i64(value, "expected remote size"))
        .transpose()?;
    let changed = transaction
        .execute(
            "UPDATE file_entries
             SET local_present = 0,
                 local_size = NULL, local_modified_at = NULL, local_hash = NULL,
                 remote_present = 0,
                 remote_id = NULL, remote_size = NULL, remote_modified_at = NULL,
                 remote_checksum_type = NULL, remote_checksum = NULL,
                 last_synced_hash = NULL,
                 last_synced_remote_checksum_type = NULL,
                 last_synced_remote_checksum = NULL,
                 last_synced_at = ?1,
                 state = 'removed_before_sync', tombstone = 0
             WHERE workspace_id = ?2 AND relative_path = ?3
               AND local_present = ?4
               AND local_hash IS ?5
               AND local_size IS ?6
               AND remote_present = ?7
               AND remote_id IS ?8
               AND remote_size IS ?9
               AND remote_checksum_type IS ?10
               AND remote_checksum IS ?11
               AND last_synced_hash IS ?12
               AND last_synced_remote_checksum_type IS ?13
               AND last_synced_remote_checksum IS ?14",
            params![
                now,
                operation.workspace_id,
                operation.relative_path,
                if operation.expected_local_present {
                    1
                } else {
                    0
                },
                operation.expected_local_hash,
                expected_local_size,
                if operation.expected_remote_present {
                    1
                } else {
                    0
                },
                operation.expected_remote_id,
                expected_remote_size,
                operation.expected_remote_checksum_type,
                operation.expected_remote_checksum,
                operation.baseline_local_hash,
                operation.baseline_remote_checksum_type,
                operation.baseline_remote_checksum,
            ],
        )
        .map_err(|error| format!("Could not converge deletion journal state: {error}"))?;
    if changed == 0 {
        return Err(
            "Local, remote, or baseline journal evidence changed before deletion completion."
                .into(),
        );
    }

    if let Some((recovery_path, original_hash, original_size)) = recovery {
        let plan_id: String = transaction
            .query_row(
                "SELECT plan_id FROM sync_plan_items WHERE id = ?1",
                params![operation.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read source plan for recovery entry: {error}"))?;
        transaction
            .execute(
                "INSERT INTO sync_recovery_entries (
                    id, workspace_id, relative_path, source_plan_id, source_item_id,
                    recovery_path, original_hash, original_size, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    Uuid::new_v4().to_string(),
                    operation.workspace_id,
                    operation.relative_path,
                    plan_id,
                    operation.id,
                    recovery_path.to_string_lossy(),
                    original_hash,
                    to_i64(original_size, "recovery size")?,
                    now,
                ],
            )
            .map_err(|error| format!("Could not journal local delete recovery copy: {error}"))?;
    }

    complete_item_in_transaction(&transaction, &operation.id, &now)?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit deletion convergence: {error}"))
}

fn complete_item_in_transaction(
    connection: &Connection,
    item_id: &str,
    now: &str,
) -> Result<(), String> {
    let changed = connection
        .execute(
            "UPDATE sync_plan_items SET status = 'completed', updated_at = ?1, last_error = NULL
             WHERE id = ?2 AND status IN ('running', 'applying')",
            params![now, item_id],
        )
        .map_err(|error| format!("Could not complete two-way sync item: {error}"))?;
    if changed == 0 {
        return Err("Two-way sync item journal changed before completion.".into());
    }
    Ok(())
}

fn mark_operation_applying_download(
    app: &AppHandle,
    item_id: &str,
    hash: &str,
    size: u64,
) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE sync_plan_items
             SET status = 'applying', downloaded_local_hash = ?1,
                 downloaded_local_size = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'running'",
            params![hash, to_i64(size, "downloaded size")?, now, item_id],
        )
        .map_err(|error| format!("Could not arm download recovery state: {error}"))?;
    if changed == 0 {
        return Err("Two-way download item changed before local apply.".into());
    }
    Ok(())
}

fn mark_operation_applying_delete(
    app: &AppHandle,
    operation: &SyncOperation,
    recovery: &Path,
    hash: &str,
    size: u64,
) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE sync_plan_items
             SET status = 'applying', downloaded_local_hash = ?1,
                 downloaded_local_size = ?2, recovery_path = ?3, updated_at = ?4
             WHERE id = ?5 AND status = 'running'",
            params![
                hash,
                to_i64(size, "delete recovery size")?,
                recovery.to_string_lossy(),
                now,
                operation.id,
            ],
        )
        .map_err(|error| format!("Could not arm local delete recovery state: {error}"))?;
    if changed == 0 {
        return Err("Local delete item changed before apply.".into());
    }
    Ok(())
}

fn fail_operation(app: &AppHandle, operation_id: &str, error: &str) -> Result<(), String> {
    let connection = open_sync_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE sync_plan_items
             SET status = 'failed', last_error = ?1, updated_at = ?2
             WHERE id = ?3 AND status IN ('ready', 'running', 'applying')",
            params![truncate_error(error), now, operation_id],
        )
        .map_err(|db_error| format!("Could not record failed two-way sync item: {db_error}"))?;
    Ok(())
}

fn finalize_plan(app: &AppHandle, plan_id: &str) -> Result<SyncExecutionReport, String> {
    let mut connection = open_sync_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start two-way finalization: {error}"))?;
    let (completed, failed, remaining): (i64, i64, i64) = transaction
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN status IN ('ready','running','applying') THEN 1 ELSE 0 END),0)
             FROM sync_plan_items WHERE plan_id = ?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| format!("Could not summarize two-way execution: {error}"))?;
    if remaining != 0 {
        return Err("Two-way sync plan still contains unfinished operations.".into());
    }
    let (conflicts, blocked): (i64, i64) = transaction
        .query_row(
            "SELECT conflict_count, blocked_count FROM sync_plans WHERE id = ?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read two-way plan summary: {error}"))?
        .ok_or_else(|| "Two-way sync plan disappeared during finalization.".to_string())?;
    let transfer_bytes: i64 = transaction
        .query_row(
            "SELECT COALESCE(SUM(
            CASE
              WHEN status = 'completed'
               AND action IN ('upload_create','upload_update','download_create','download_update')
              THEN COALESCE(expected_local_size, expected_remote_size, 0)
              ELSE 0
            END
         ), 0)
         FROM sync_plan_items WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not calculate completed transfer bytes: {error}"))?;
    let status = if failed > 0 && completed == 0 {
        "failed"
    } else if failed > 0 || conflicts > 0 || blocked > 0 {
        "partial"
    } else {
        "completed"
    };
    let finished_at = Utc::now().to_rfc3339();
    transaction
        .execute(
            "UPDATE sync_plans
             SET status = ?1, completed_count = ?2, failed_count = ?3,
                 completed_at = ?4,
                 last_error = CASE WHEN ?3 > 0 THEN 'One or more two-way operations failed.' ELSE NULL END
             WHERE id = ?5 AND status = 'running'",
            params![status, completed, failed, finished_at, plan_id],
        )
        .map_err(|error| format!("Could not finalize two-way sync plan: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit two-way finalization: {error}"))?;
    Ok(SyncExecutionReport {
        plan_id: plan_id.to_string(),
        status: status.to_string(),
        completed_count: from_i64(completed, "completed sync count")?,
        failed_count: from_i64(failed, "failed sync count")?,
        transferred_bytes: from_i64(transfer_bytes, "transfer bytes")?,
        finished_at,
    })
}

fn ensure_no_other_transfer_running(
    connection: &Connection,
    workspace_id: &str,
) -> Result<(), String> {
    let sync_running: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_plans WHERE workspace_id = ?1 AND status = 'running'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active two-way plans: {error}"))?;
    if sync_running > 0 {
        return Err("A two-way synchronization is already running for this workspace.".into());
    }
    ensure_no_other_transfer_running_except_sync(connection, workspace_id)
}

fn ensure_no_other_transfer_running_except_sync(
    connection: &Connection,
    workspace_id: &str,
) -> Result<(), String> {
    let backup_running: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM backup_plans WHERE workspace_id = ?1 AND status = 'running'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active backup plans: {error}"))?;
    if backup_running > 0 {
        return Err("A backup execution is already running for this workspace.".into());
    }
    if table_exists(connection, "restore_plans")? {
        let restore_running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM restore_plans WHERE workspace_id = ?1 AND status = 'running'",
                params![workspace_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active restore plans: {error}"))?;
        if restore_running > 0 {
            return Err("A restore execution is already running for this workspace.".into());
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
        .map_err(|error| format!("Could not inspect SQLite feature tables: {error}"))
}

fn latest_plan_with_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<SyncPlan>, String> {
    let plan_id = connection
        .query_row(
            "SELECT id FROM sync_plans WHERE workspace_id = ?1 ORDER BY created_at DESC LIMIT 1",
            params![workspace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not read latest two-way sync plan: {error}"))?;
    plan_id
        .map(|id| load_plan_with_connection(connection, &id))
        .transpose()
}

fn load_plan_with_connection(connection: &Connection, plan_id: &str) -> Result<SyncPlan, String> {
    let metadata = connection
        .query_row(
            "SELECT id, workspace_id, provider_id, remote_path, status,
                    created_at, local_scan_at, remote_inventory_at,
                    upload_count, download_count, delete_count, conflict_count, blocked_count,
                    transfer_bytes, completed_count, failed_count, completed_at
             FROM sync_plans WHERE id = ?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read two-way plan: {error}"))?
        .ok_or_else(|| "Two-way sync plan was not found.".to_string())?;
    let total_items: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_plan_items WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not count two-way plan items: {error}"))?;
    let mut statement = connection
        .prepare(
            "SELECT id, relative_path, action, status,
                    COALESCE(expected_local_size, expected_remote_size), reason, last_error
             FROM sync_plan_items
             WHERE plan_id = ?1
             ORDER BY CASE status WHEN 'conflict' THEN 0 WHEN 'blocked' THEN 1 ELSE 2 END,
                      relative_path ASC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare two-way plan preview: {error}"))?;
    let rows = statement
        .query_map(params![plan_id, PREVIEW_LIMIT as i64], |row| {
            Ok(SyncPlanItem {
                id: row.get(0)?,
                relative_path: row.get(1)?,
                action: row.get(2)?,
                status: row.get(3)?,
                size: optional_u64(row, 4)?,
                reason: row.get(5)?,
                last_error: row.get(6)?,
            })
        })
        .map_err(|error| format!("Could not query two-way plan preview: {error}"))?;
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read two-way plan preview: {error}"))?;
    Ok(SyncPlan {
        id: metadata.0,
        workspace_id: metadata.1,
        provider_id: metadata.2,
        remote_path: metadata.3,
        status: metadata.4,
        created_at: metadata.5,
        local_scan_at: metadata.6,
        remote_inventory_at: metadata.7,
        upload_count: from_i64(metadata.8, "upload count")?,
        download_count: from_i64(metadata.9, "download count")?,
        delete_count: from_i64(metadata.10, "delete count")?,
        conflict_count: from_i64(metadata.11, "conflict count")?,
        blocked_count: from_i64(metadata.12, "blocked count")?,
        transfer_bytes: from_i64(metadata.13, "transfer bytes")?,
        completed_count: from_i64(metadata.14, "completed count")?,
        failed_count: from_i64(metadata.15, "failed count")?,
        completed_at: metadata.16,
        preview_truncated: total_items > PREVIEW_LIMIT as i64,
        items,
    })
}

fn operation_from_row(row: &Row<'_>) -> rusqlite::Result<SyncOperation> {
    Ok(SyncOperation {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        relative_path: row.get(2)?,
        action: row.get(3)?,
        expected_local_present: row.get::<_, i64>(4)? != 0,
        expected_local_hash: row.get(5)?,
        expected_local_size: optional_u64(row, 6)?,
        expected_remote_present: row.get::<_, i64>(7)? != 0,
        expected_remote_id: row.get(8)?,
        expected_remote_size: optional_u64(row, 9)?,
        expected_remote_checksum_type: row.get(10)?,
        expected_remote_checksum: row.get(11)?,
        baseline_local_hash: row.get(12)?,
        baseline_remote_checksum_type: row.get(13)?,
        baseline_remote_checksum: row.get(14)?,
    })
}

fn validate_portable_relative_path(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') {
        return Err("Path cannot be mapped safely across supported Phase 6 filesystems.".into());
    }
    let mut segments = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("Path contains an unsafe segment.".into());
        }
        if segment.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        }) {
            return Err(
                "Path contains characters that are unsafe on supported filesystems.".into(),
            );
        }
        if segment.ends_with(' ') || segment.ends_with('.') {
            return Err("Path ends with a dot or space and is not portable.".into());
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
                .is_some_and(|value| (1..=9).contains(&value))
            || stem
                .strip_prefix("LPT")
                .and_then(|suffix| suffix.parse::<u8>().ok())
                .is_some_and(|value| (1..=9).contains(&value));
        if reserved {
            return Err("Path uses a Windows-reserved file name.".into());
        }
        segments.push(segment.to_string());
    }
    Ok(segments)
}

fn resolve_existing_local_file(
    workspace: &Workspace,
    relative_path: &str,
) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("Two-way item contains an unsafe local path.".into());
    }
    let root = PathBuf::from(&workspace.local_path)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    let candidate = root.join(relative);
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("Could not inspect local sync candidate: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Local sync candidate is not a regular file.".into());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("Could not resolve local sync candidate: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("Local sync candidate escaped the selected workspace.".into());
    }
    Ok(canonical)
}

fn resolve_local_target(
    workspace: &Workspace,
    relative_path: &str,
    create_parents: bool,
) -> Result<PathBuf, String> {
    let segments = validate_portable_relative_path(relative_path)?;
    let root = PathBuf::from(&workspace.local_path)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    let (file_name, parents) = segments
        .split_last()
        .ok_or_else(|| "Sync path has no file name.".to_string())?;
    let mut parent = root.clone();
    for segment in parents {
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("Local sync path crosses a symbolic link.".into());
                }
                if !metadata.is_dir() {
                    return Err("Local sync path crosses a non-directory entry.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&next)
                    .map_err(|error| format!("Could not create sync directory: {error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("Local sync parent directory is missing.".into());
            }
            Err(error) => return Err(format!("Could not inspect local sync parent: {error}")),
        }
        parent = next
            .canonicalize()
            .map_err(|error| format!("Could not resolve local sync parent: {error}"))?;
        if !parent.starts_with(&root) {
            return Err("Local sync path escaped the selected workspace.".into());
        }
    }
    let target = parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err("Local sync target is a symbolic link.".into());
        }
        if metadata.is_dir() {
            return Err("Local sync target collides with an existing directory.".into());
        }
    }
    Ok(target)
}

fn validate_download_target(operation: &SyncOperation, target: &Path) -> Result<(), String> {
    match operation.action.as_str() {
        "download_create" => match fs::symlink_metadata(target) {
            Ok(_) => Err("Local path appeared after planning; create was blocked.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Could not inspect local download target: {error}")),
        },
        "download_update" => {
            let metadata = fs::symlink_metadata(target)
                .map_err(|error| format!("Could not inspect local download target: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("Download update target is no longer the planned regular file.".into());
            }
            let (size, hash) = scanner::fingerprint_file(target)?;
            if Some(size) != operation.expected_local_size
                || Some(hash.as_str()) != operation.expected_local_hash.as_deref()
            {
                return Err("Local file changed while two-way sync was waiting.".into());
            }
            Ok(())
        }
        _ => Err("Unsupported download action.".into()),
    }
}

fn artifact_paths(target: &Path, operation_id: &str) -> Result<(PathBuf, PathBuf), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "Sync target has no parent directory.".to_string())?;
    let safe_id: String = operation_id
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect();
    if safe_id.len() < 16 {
        return Err("Sync operation identifier is invalid.".into());
    }
    Ok((
        parent.join(format!(".atrisbridge-sync-{safe_id}.part")),
        parent.join(format!(".atrisbridge-sync-{safe_id}.bak")),
    ))
}

fn ensure_artifact_absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            Err("A two-way recovery artifact already exists. Resolve it before retrying.".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not inspect two-way recovery artifact: {error}"
        )),
    }
}

fn apply_download(
    operation: &SyncOperation,
    target: &Path,
    stage: &Path,
    backup: &Path,
) -> Result<(), String> {
    match operation.action.as_str() {
        "download_create" => {
            if fs::symlink_metadata(target).is_ok() {
                return Err("Local path appeared immediately before create.".into());
            }
            fs::rename(stage, target)
                .map_err(|error| format!("Could not place downloaded file: {error}"))
        }
        "download_update" => {
            validate_download_target(operation, target)?;
            fs::rename(target, backup).map_err(|error| {
                format!("Could not create recoverable local update copy: {error}")
            })?;
            if let Err(error) = fs::rename(stage, target) {
                let rollback = fs::rename(backup, target);
                return match rollback {
                    Ok(()) => Err(format!("Could not place downloaded update; original was restored: {error}")),
                    Err(rollback_error) => Err(format!(
                        "Could not place downloaded update and rollback failed: {error}; {rollback_error}"
                    )),
                };
            }
            Ok(())
        }
        _ => Err("Unsupported download apply action.".into()),
    }
}

fn rollback_after_download_failure(
    operation: &SyncOperation,
    target: &Path,
    backup: &Path,
    downloaded_hash: &str,
    downloaded_size: u64,
    reason: String,
) -> Result<(), String> {
    let rollback = match operation.action.as_str() {
        "download_create" => {
            if file_matches(target, downloaded_hash, downloaded_size)? {
                fs::remove_file(target)
                    .map_err(|error| format!("Could not remove uncommitted download: {error}"))?;
                Ok(())
            } else {
                Err("Downloaded target changed after apply; it was preserved.".to_string())
            }
        }
        "download_update" => {
            if !backup.is_file() {
                Err("Original local recovery copy is missing.".to_string())
            } else if !file_matches(target, downloaded_hash, downloaded_size)? {
                Err("Downloaded target changed after apply; target and recovery copy were preserved.".to_string())
            } else {
                fs::remove_file(target).map_err(|error| {
                    format!("Could not remove uncommitted downloaded target: {error}")
                })?;
                fs::rename(backup, target)
                    .map_err(|error| format!("Could not restore original local file: {error}"))?;
                Ok(())
            }
        }
        _ => Err("Unsupported download rollback action.".to_string()),
    };
    match rollback {
        Ok(()) => Err(format!("{reason} Original local state was restored.")),
        Err(error) => Err(format!("{reason} Automatic rollback was not safe: {error}")),
    }
}

fn recovery_path(app: &AppHandle, operation: &SyncOperation) -> Result<PathBuf, String> {
    let mut path = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge recovery directory: {error}"))?
        .join("recovery")
        .join(&operation.workspace_id)
        .join(&operation.id);
    for segment in validate_portable_relative_path(&operation.relative_path)? {
        path.push(segment);
    }
    Ok(path)
}

fn restore_local_delete_from_recovery(
    target: &Path,
    recovery: &Path,
    expected_hash: &str,
    expected_size: u64,
) -> Result<(), String> {
    if fs::symlink_metadata(target).is_ok() {
        return Err("Local target reappeared; AtrisBridge preserved the recovery copy instead of overwriting it.".into());
    }
    if !file_matches(recovery, expected_hash, expected_size)? {
        return Err(
            "Local delete recovery copy no longer matches the original fingerprint.".into(),
        );
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not recreate local parent during rollback: {error}"))?;
    }
    fs::copy(recovery, target)
        .map_err(|error| format!("Could not restore local file from recovery: {error}"))?;
    if !file_matches(target, expected_hash, expected_size)? {
        return Err("Restored local file did not match the recovery fingerprint.".into());
    }
    Ok(())
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
        .map_err(|error| format!("Could not read synchronized file metadata: {error}"))?;
    Ok(metadata
        .modified()
        .ok()
        .map(|value| DateTime::<Utc>::from(value).to_rfc3339()))
}

fn remove_regular_file_best_effort(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            let _ = fs::remove_file(path);
        }
    }
}

fn download_remote_to_stage(
    app: &AppHandle,
    token: &str,
    remote_file_path: &str,
    destination: &Path,
) -> Result<(), String> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err("Two-way staging path already exists.".into());
    }
    let normalized = rclone::normalize_remote_path(remote_file_path)?;
    if normalized.is_empty() {
        return Err("Remote download path cannot be empty.".into());
    }
    let executable = locate_runtime(app)?;
    let source = format!(":drive:{normalized}");
    let output = drive_command(&executable, token)
        .arg("copyto")
        .arg(source)
        .arg(destination)
        .args([
            "--checksum",
            "--immutable",
            "--retries",
            "1",
            "--stats",
            "0",
        ])
        .output()
        .map_err(|error| format!("Could not start Google Drive download: {error}"))?;
    ensure_process_success("Google Drive two-way download", &output)
}

fn trash_google_drive_file_by_id(token_json: &str, file_id: &str) -> Result<(), String> {
    validate_drive_file_id(file_id)?;
    let access_token = extract_google_access_token(token_json)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("Could not initialize Google Drive API client: {error}"))?;
    let response = client
        .patch(format!(
            "https://www.googleapis.com/drive/v3/files/{file_id}"
        ))
        .query(&[("supportsAllDrives", "true"), ("fields", "id,trashed")])
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "trashed": true }))
        .send()
        .map_err(|error| {
            format!("Could not move reviewed Google Drive object to Trash: {error}")
        })?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Could not read Google Drive Trash response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Google Drive rejected the exact-ID Trash request ({}): {}",
            status.as_u16(),
            sanitize_drive_api_error(&body)
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("Could not decode Google Drive Trash response: {error}"))?;
    let returned_id = value.get("id").and_then(serde_json::Value::as_str);
    let trashed = value
        .get("trashed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if returned_id != Some(file_id) || !trashed {
        return Err(
            "Google Drive did not confirm that the reviewed file ID moved to Trash.".into(),
        );
    }
    Ok(())
}

fn extract_google_access_token(token_json: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(token_json)
        .map_err(|_| "Google Drive session token is not valid OAuth JSON.".to_string())?;
    value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Google Drive session token does not contain an access token.".to_string())
}

fn validate_drive_file_id(file_id: &str) -> Result<(), String> {
    if file_id.is_empty()
        || !file_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("Google Drive file ID contains unexpected characters.".into());
    }
    Ok(())
}

fn sanitize_drive_api_error(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Drive API request failed without a readable error message.".into());
    message.chars().take(300).collect()
}

fn locate_runtime(app: &AppHandle) -> Result<PathBuf, String> {
    let executable_name = if cfg!(target_os = "windows") {
        "rclone.exe"
    } else {
        "rclone"
    };
    let resource = app
        .path()
        .resource_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge resources: {error}"))?
        .join("rclone")
        .join(executable_name);
    if resource.is_file() {
        validate_runtime(&resource)?;
        return Ok(resource);
    }
    #[cfg(debug_assertions)]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(executable_name);
        if development.is_file() {
            validate_runtime(&development)?;
            return Ok(development);
        }
    }
    Err(format!(
        "AtrisBridge requires its pinned rclone sidecar (v{}). Run `npm run sidecar:prepare` for local development.",
        rclone::REQUIRED_RCLONE_VERSION
    ))
}

fn validate_runtime(executable: &Path) -> Result<(), String> {
    let output = clean_command(executable)
        .arg("version")
        .output()
        .map_err(|error| format!("Could not execute AtrisBridge rclone sidecar: {error}"))?;
    ensure_process_success("rclone version check", &output)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("rclone v"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| "Could not determine rclone sidecar version.".to_string())?;
    if version != rclone::REQUIRED_RCLONE_VERSION {
        return Err(format!(
            "Unsupported rclone sidecar version v{version}. AtrisBridge pins v{}.",
            rclone::REQUIRED_RCLONE_VERSION
        ));
    }
    Ok(())
}

fn drive_command(executable: &Path, token: &str) -> Command {
    let mut command = clean_command(executable);
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

fn ensure_process_success(action: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = sanitize_process_error(&String::from_utf8_lossy(&output.stderr));
    if stderr.is_empty() {
        Err(format!("{action} failed with status {}.", output.status))
    } else {
        Err(format!("{action} failed: {stderr}"))
    }
}

fn sanitize_process_error(value: &str) -> String {
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

fn load_interrupted_items(connection: &Connection) -> Result<Vec<InterruptedSyncItem>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, workspace_id, relative_path, action, status,
                    downloaded_local_hash, downloaded_local_size, recovery_path
             FROM sync_plan_items
             WHERE status IN ('running','applying')
             ORDER BY updated_at ASC",
        )
        .map_err(|error| format!("Could not prepare interrupted sync query: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(InterruptedSyncItem {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                relative_path: row.get(2)?,
                action: row.get(3)?,
                status: row.get(4)?,
                downloaded_hash: row.get(5)?,
                downloaded_size: optional_u64(row, 6)?,
                recovery_path: row.get(7)?,
            })
        })
        .map_err(|error| format!("Could not query interrupted sync items: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read interrupted sync items: {error}"))
}

fn recover_interrupted_item(
    _app: &AppHandle,
    workspace: &Workspace,
    item: &InterruptedSyncItem,
) -> Result<String, String> {
    if item.status == "running" {
        if matches!(item.action.as_str(), "download_create" | "download_update") {
            if let Ok(target) = resolve_local_target(workspace, &item.relative_path, false) {
                if let Ok((stage, _)) = artifact_paths(&target, &item.id) {
                    remove_regular_file_best_effort(&stage);
                }
            }
        }
        return Ok(
            "Interrupted two-way operation stopped before a persisted local apply state. Fresh evidence is required before retrying."
                .into(),
        );
    }

    let hash = item.downloaded_hash.as_deref().ok_or_else(|| {
        "Interrupted apply state has no persisted recovery fingerprint.".to_string()
    })?;
    let size = item
        .downloaded_size
        .ok_or_else(|| "Interrupted apply state has no persisted recovery size.".to_string())?;
    match item.action.as_str() {
        "download_create" | "download_update" => {
            let target = resolve_local_target(workspace, &item.relative_path, false)?;
            let (stage, backup) = artifact_paths(&target, &item.id)?;
            remove_regular_file_best_effort(&stage);
            if item.action == "download_create" {
                if file_matches(&target, hash, size)? {
                    fs::remove_file(&target)
                        .map_err(|error| format!("Could not roll back interrupted download create: {error}"))?;
                    return Ok("Interrupted download create was rolled back to the original absent-local state.".into());
                }
                return Ok("Interrupted download create target changed after apply; AtrisBridge preserved it.".into());
            }
            if backup.is_file() {
                if fs::symlink_metadata(&target).is_err() {
                    fs::rename(&backup, &target)
                        .map_err(|error| format!("Could not restore interrupted update backup: {error}"))?;
                    return Ok("Interrupted download update was rolled back from its .bak recovery copy.".into());
                }
                if file_matches(&target, hash, size)? {
                    fs::remove_file(&target)
                        .map_err(|error| format!("Could not remove interrupted downloaded target: {error}"))?;
                    fs::rename(&backup, &target)
                        .map_err(|error| format!("Could not restore original local target: {error}"))?;
                    return Ok("Interrupted download update was rolled back to the original local file.".into());
                }
                return Ok("Interrupted download update target changed; target and .bak recovery copy were preserved.".into());
            }
            Ok("Interrupted download update stopped before the original local file was moved.".into())
        }
        "local_delete" => {
            let recovery = item
                .recovery_path
                .as_deref()
                .map(PathBuf::from)
                .ok_or_else(|| "Interrupted local delete has no recovery path.".to_string())?;
            let target = resolve_local_target(workspace, &item.relative_path, true)?;
            if fs::symlink_metadata(&target).is_ok() {
                return Ok("Interrupted local delete target already exists. Recovery copy was preserved without overwrite.".into());
            }
            restore_local_delete_from_recovery(&target, &recovery, hash, size)?;
            Ok("Interrupted local deletion was restored from the verified AtrisBridge recovery copy.".into())
        }
        _ => Ok(
            "Interrupted remote-side operation cannot be proven complete without a fresh provider inventory. No blind retry was attempted."
                .into(),
        ),
    }
}

fn ensure_managed_root(remote_path: &str) -> Result<(), String> {
    let normalized = rclone::normalize_remote_path(remote_path)?;
    if normalized.starts_with("AtrisBridge/") {
        Ok(())
    } else {
        Err("Phase 6 writes and trash operations are restricted to an AtrisBridge-managed workspace path.".into())
    }
}

fn checked_inc(value: u64, label: &str) -> Result<u64, String> {
    value
        .checked_add(1)
        .ok_or_else(|| format!("{label} exceeded supported range."))
}

fn checked_add(value: u64, add: u64, label: &str) -> Result<u64, String> {
    value
        .checked_add(add)
        .ok_or_else(|| format!("{label} exceeded supported range."))
}

fn optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
        })
        .transpose()
}

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn from_i64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{label} was negative in the sync journal."))
}

fn truncate_error(value: &str) -> String {
    value.chars().take(900).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> FileEvidence {
        FileEvidence {
            relative_path: "src/main.rs".into(),
            local_present: true,
            local_size: Some(10),
            local_hash: Some("local-a".into()),
            remote_present: true,
            remote_id: Some("remote-id".into()),
            remote_size: Some(10),
            remote_checksum_type: Some("MD5".into()),
            remote_checksum: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            last_synced_hash: Some("local-a".into()),
            last_synced_remote_checksum_type: Some("MD5".into()),
            last_synced_remote_checksum: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
        }
    }

    #[test]
    fn local_change_uploads_when_remote_still_matches_baseline() {
        let mut file = baseline();
        file.local_hash = Some("local-b".into());
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Action("upload_update")
        ));
    }

    #[test]
    fn remote_change_downloads_when_local_still_matches_baseline() {
        let mut file = baseline();
        file.remote_checksum = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Action("download_update")
        ));
    }

    #[test]
    fn modify_modify_is_a_conflict() {
        let mut file = baseline();
        file.local_hash = Some("local-b".into());
        file.remote_checksum = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert!(matches!(classify(&file, false), PlanDecision::Conflict(_)));
    }

    #[test]
    fn local_delete_trashes_only_unchanged_remote() {
        let mut file = baseline();
        file.local_present = false;
        file.local_hash = None;
        file.local_size = None;
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Action("remote_trash")
        ));

        file.remote_checksum = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert!(matches!(classify(&file, false), PlanDecision::Conflict(_)));
    }

    #[test]
    fn remote_delete_removes_only_unchanged_local() {
        let mut file = baseline();
        file.remote_present = false;
        file.remote_id = None;
        file.remote_size = None;
        file.remote_checksum_type = None;
        file.remote_checksum = None;
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Action("local_delete")
        ));

        file.local_hash = Some("local-b".into());
        assert!(matches!(classify(&file, false), PlanDecision::Conflict(_)));
    }

    #[test]
    fn both_sides_missing_acknowledges_converged_delete() {
        let mut file = baseline();
        file.local_present = false;
        file.local_hash = None;
        file.local_size = None;
        file.remote_present = false;
        file.remote_id = None;
        file.remote_size = None;
        file.remote_checksum_type = None;
        file.remote_checksum = None;
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Action("acknowledge_delete")
        ));
    }

    #[test]
    fn unverified_overlap_stays_blocked() {
        let mut file = baseline();
        file.last_synced_hash = None;
        file.last_synced_remote_checksum_type = None;
        file.last_synced_remote_checksum = None;
        assert!(matches!(classify(&file, false), PlanDecision::Blocked(_)));
    }

    #[test]
    fn parses_in_memory_google_access_token_without_logging_session_json() {
        let token = r#"{"access_token":"secret-access-token","token_type":"Bearer"}"#;
        assert_eq!(
            extract_google_access_token(token).unwrap(),
            "secret-access-token"
        );
        assert!(extract_google_access_token("{}").is_err());
        assert!(validate_drive_file_id("1AbC_-xyz").is_ok());
        assert!(validate_drive_file_id("bad/id").is_err());
    }

    #[test]
    fn deletion_live_preflight_uses_portable_path_validation() {
        assert!(validate_portable_relative_path("src/main.rs").is_ok());
        assert!(validate_portable_relative_path("src/../main.rs").is_err());
    }

    #[test]
    fn portable_paths_reject_reserved_or_parent_segments() {
        assert!(validate_portable_relative_path("src/../secret.txt").is_err());
        assert!(validate_portable_relative_path("CON.txt").is_err());
        assert!(validate_portable_relative_path("src/main.rs").is_ok());
    }
}
