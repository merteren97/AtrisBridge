use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    database::open_database,
    models::{RemoteFileObservation, RemoteInventoryReport, Workspace},
    provider_sessions::ProviderSessionStore,
    provider_storage, scanner,
    storage::{find_workspace, record_scan},
    transport::rclone,
};

const PREVIEW_LIMIT: usize = 120;
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
pub struct RestorePlanItem {
    pub id: String,
    pub relative_path: String,
    pub action: String,
    pub status: String,
    pub size: Option<u64>,
    pub block_reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    pub id: String,
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
    pub status: String,
    pub created_at: String,
    pub local_scan_at: String,
    pub remote_inventory_at: String,
    pub restore_count: u64,
    pub restore_bytes: u64,
    pub blocked_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub completed_at: Option<String>,
    pub preview_truncated: bool,
    pub items: Vec<RestorePlanItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreExecutionReport {
    pub plan_id: String,
    pub status: String,
    pub completed_count: u64,
    pub failed_count: u64,
    pub restored_bytes: u64,
    pub finished_at: String,
}

#[derive(Debug, Clone)]
struct RestoreExecutionContext {
    workspace_id: String,
    provider_id: String,
    remote_path: String,
}

#[derive(Debug, Clone)]
struct RestoreOperation {
    id: String,
    workspace_id: String,
    relative_path: String,
    action: String,
    expected_local_present: bool,
    expected_local_hash: Option<String>,
    expected_local_size: Option<u64>,
    expected_remote_id: String,
    expected_remote_size: u64,
    expected_remote_checksum_type: String,
    expected_remote_checksum: String,
}

#[derive(Debug, Clone)]
struct CurrentFileEvidence {
    local_present: bool,
    local_hash: Option<String>,
    local_size: Option<u64>,
    remote_present: bool,
    remote_id: Option<String>,
    remote_size: Option<u64>,
    remote_checksum_type: Option<String>,
    remote_checksum: Option<String>,
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

enum PlanDecision {
    Skip,
    Restore(&'static str),
    Blocked(String),
}

#[derive(Debug)]
struct InterruptedItem {
    id: String,
    workspace_id: String,
    relative_path: String,
    action: String,
    status: String,
    downloaded_hash: Option<String>,
    downloaded_size: Option<u64>,
}

#[tauri::command]
pub fn latest_restore_plan(app: AppHandle, id: String) -> Result<Option<RestorePlan>, String> {
    let connection = open_restore_database(&app)?;
    latest_plan_with_connection(&connection, &id)
}

#[tauri::command]
pub async fn prepare_restore_plan(
    app: AppHandle,
    id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RestorePlan, String> {
    let workspace = find_workspace(&app, &id)?;
    if matches!(workspace.sync_mode, crate::models::SyncMode::TwoWay) {
        return Err(
            "One-way restore planning is disabled while this workspace is in Two-Way mode. Use Prepare sync instead."
  .into(),
        );
    }
    let (provider, binding) = provider_storage::get_provider_for_workspace(&app, &id)?;
    if provider.provider_type != "google_drive" {
        return Err("Phase 5 currently supports Google Drive restore only.".into());
    }
    ensure_managed_restore_root(&binding.remote_path)?;
    let token = sessions.google_drive_token(&provider.id)?.ok_or_else(|| {
        "Google Drive session is not active. Reconnect before preparing a restore.".to_string()
    })?;

    refresh_local_inventory(&app, &id).await?;
    refresh_remote_inventory(&app, &id, token).await?;

    let mut connection = open_restore_database(&app)?;
    create_plan_with_connection(&mut connection, &id)
}

#[tauri::command]
pub async fn execute_restore_plan(
    app: AppHandle,
    plan_id: String,
    sessions: State<'_, ProviderSessionStore>,
) -> Result<RestoreExecutionReport, String> {
    let context = execution_context(&app, &plan_id)?;
    let workspace = find_workspace(&app, &context.workspace_id)?;
    if matches!(workspace.sync_mode, crate::models::SyncMode::TwoWay) {
        return Err(
            "One-way restore execution is disabled while this workspace is in Two-Way mode. Prepare a fresh two-way plan instead."
      .into(),
        );
    }
    ensure_managed_restore_root(&context.remote_path)?;
    let token = sessions
        .google_drive_token(&context.provider_id)?
        .ok_or_else(|| {
            "Google Drive session is not active. Reconnect before restore.".to_string()
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
        if let Err(error) = execute_restore_operation(&app, &context, &operation, &token).await {
            let _ = fail_operation(&app, &operation.id, &error);
        }
    }

    finalize_plan(&app, &plan_id)
}

pub fn recover_interrupted_restores(app: &AppHandle) -> Result<(), String> {
    let connection = open_restore_database(app)?;
    let interrupted = load_interrupted_items(&connection)?;
    let mut recovery_messages = Vec::new();

    for item in interrupted {
        let workspace = match find_workspace(app, &item.workspace_id) {
            Ok(workspace) => workspace,
            Err(error) => {
                recovery_messages.push((
                    item.id,
                    format!(
                        "Interrupted restore could not inspect its workspace. No files were changed during recovery: {error}"
                    ),
                ));
                continue;
            }
        };
        let message = match recover_interrupted_item(&workspace, &item) {
            Ok(message) => message,
            Err(error) => format!(
                "Interrupted restore requires manual inspection. AtrisBridge did not overwrite any uncertain file during recovery: {error}"
            ),
        };
        recovery_messages.push((item.id, message));
    }

    let now = Utc::now().to_rfc3339();
    for (item_id, message) in recovery_messages {
        connection
            .execute(
                "UPDATE restore_plan_items
                 SET status = 'failed', last_error = ?1, updated_at = ?2
                 WHERE id = ?3 AND status IN ('running', 'applying')",
                params![truncate_error(&message), now, item_id],
            )
            .map_err(|error| format!("Could not record interrupted restore recovery: {error}"))?;
    }
    connection
        .execute(
            "UPDATE restore_plans
             SET status = 'partial', completed_at = ?1,
                 last_error = COALESCE(
                    last_error,
                    'Previous restore execution was interrupted. Prepare a fresh restore plan before retrying.'
                 )
             WHERE status = 'running'",
            params![now],
        )
        .map_err(|error| format!("Could not retire interrupted restore plans: {error}"))?;
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
    find_workspace(app, id)?;
    let (provider, binding) = provider_storage::get_provider_for_workspace(app, id)?;
    if provider.provider_type != "google_drive" {
        return Err("This provider is not supported by the Phase 5 restore adapter.".into());
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

fn open_restore_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_restore_schema(&connection)?;
    Ok(connection)
}

fn ensure_restore_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS restore_plans (
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
                restore_count INTEGER NOT NULL DEFAULT 0,
                restore_bytes INTEGER NOT NULL DEFAULT 0,
                blocked_count INTEGER NOT NULL DEFAULT 0,
                completed_count INTEGER NOT NULL DEFAULT 0,
                failed_count INTEGER NOT NULL DEFAULT 0,
                completed_at TEXT,
                last_error TEXT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
                FOREIGN KEY(provider_id) REFERENCES provider_connections(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_restore_plans_workspace_created_at
                ON restore_plans(workspace_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_restore_plans_workspace_status
                ON restore_plans(workspace_id, status);
            CREATE TABLE IF NOT EXISTS restore_plan_items (
                id TEXT PRIMARY KEY,
                plan_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                action TEXT NOT NULL CHECK(action IN ('create', 'update', 'blocked')),
                status TEXT NOT NULL CHECK(status IN (
                    'ready', 'running', 'applying', 'completed', 'failed', 'blocked', 'cancelled'
                )),
                expected_local_present INTEGER NOT NULL DEFAULT 0 CHECK(expected_local_present IN (0, 1)),
                expected_local_hash TEXT,
                expected_local_size INTEGER,
                expected_remote_id TEXT,
                expected_remote_size INTEGER,
                expected_remote_checksum_type TEXT,
                expected_remote_checksum TEXT,
                downloaded_local_hash TEXT,
                downloaded_local_size INTEGER,
                completed_local_hash TEXT,
                block_reason TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(plan_id, relative_path),
                FOREIGN KEY(plan_id) REFERENCES restore_plans(id) ON DELETE CASCADE,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_restore_plan_items_plan_status
                ON restore_plan_items(plan_id, status);
            CREATE INDEX IF NOT EXISTS idx_restore_plan_items_workspace_status
                ON restore_plan_items(workspace_id, status);",
        )
        .map_err(|error| format!("Could not initialize Phase 5 restore journal: {error}"))
}

fn latest_plan_with_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<RestorePlan>, String> {
    let plan_id = connection
        .query_row(
            "SELECT id FROM restore_plans
             WHERE workspace_id = ?1
             ORDER BY created_at DESC LIMIT 1",
            params![workspace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not read latest restore plan: {error}"))?;
    plan_id
        .map(|id| load_plan_with_connection(connection, &id))
        .transpose()
}

fn create_plan_with_connection(
    connection: &mut Connection,
    workspace_id: &str,
) -> Result<RestorePlan, String> {
    ensure_restore_schema(connection)?;
    let metadata = connection
        .query_row(
            "SELECT w.last_scan_at, w.local_path, b.provider_id, b.remote_path, b.last_inventory_at
             FROM workspaces w
             LEFT JOIN workspace_remote_bindings b ON b.workspace_id = w.id
             WHERE w.id = ?1",
            params![workspace_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read restore planning metadata: {error}"))?
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    let local_scan_at = metadata
        .0
        .ok_or_else(|| "Run a fresh local scan before preparing a restore plan.".to_string())?;
    let workspace_root = PathBuf::from(metadata.1);
    let provider_id = metadata.2.ok_or_else(|| {
        "Bind this workspace to Google Drive before preparing a restore plan.".to_string()
    })?;
    let remote_path = metadata
        .3
        .ok_or_else(|| "Workspace remote path is missing.".to_string())?;
    ensure_managed_restore_root(&remote_path)?;
    let remote_inventory_at = metadata.4.ok_or_else(|| {
        "Read a fresh remote inventory before preparing a restore plan.".to_string()
    })?;

    let active_restore: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM restore_plans
             WHERE workspace_id = ?1 AND status = 'running'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active restore plans: {error}"))?;
    if active_restore > 0 {
        return Err("A restore execution is already running for this workspace.".into());
    }
    let active_backup: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM backup_plans
             WHERE workspace_id = ?1 AND status = 'running'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active backup plans: {error}"))?;
    if active_backup > 0 {
        return Err("A backup execution is already running for this workspace.".into());
    }

    let evidence = load_file_evidence(connection, workspace_id)?;
    let collision_keys = portable_collision_keys(&evidence);
    let plan_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut restore_count = 0_u64;
    let mut restore_bytes = 0_u64;
    let mut blocked_count = 0_u64;

    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start restore planning transaction: {error}"))?;
    transaction
        .execute(
            "UPDATE restore_plan_items
             SET status = 'cancelled', updated_at = ?1
             WHERE plan_id IN (
                SELECT id FROM restore_plans WHERE workspace_id = ?2 AND status = 'ready'
             ) AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous restore items: {error}"))?;
    transaction
        .execute(
            "UPDATE restore_plans
             SET status = 'cancelled', completed_at = ?1
             WHERE workspace_id = ?2 AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous restore plan: {error}"))?;
    transaction
        .execute(
            "INSERT INTO restore_plans (
                id, workspace_id, provider_id, remote_path, status,
                created_at, local_scan_at, remote_inventory_at,
                restore_count, restore_bytes, blocked_count
             ) VALUES (?1, ?2, ?3, ?4, 'ready', ?5, ?6, ?7, 0, 0, 0)",
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
        .map_err(|error| format!("Could not create restore plan: {error}"))?;

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO restore_plan_items (
                    id, plan_id, workspace_id, relative_path, action, status,
                    expected_local_present, expected_local_hash, expected_local_size,
                    expected_remote_id, expected_remote_size,
                    expected_remote_checksum_type, expected_remote_checksum,
                    block_reason, created_at, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6,
                    ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15
                 )",
            )
            .map_err(|error| format!("Could not prepare restore plan item insert: {error}"))?;

        for file in evidence {
            let collides = collision_keys.contains(&portable_path_key(&file.relative_path));
            let decision = if file.remote_present
                && scanner::is_path_ignored_for_sync(&workspace_root, &file.relative_path)?
            {
                PlanDecision::Blocked(
                    "Path is excluded by AtrisBridge built-in safety rules or .atrisbridgeignore. Restore will not recreate excluded content."
          .into(),
                )
            } else {
                classify(&file, collides)
            };
            match decision {
                PlanDecision::Skip => {}
                PlanDecision::Restore(action) => {
                    let remote_id = file.remote_id.as_deref().ok_or_else(|| {
                        format!("Remote identity disappeared for {}.", file.relative_path)
                    })?;
                    let remote_size = file.remote_size.ok_or_else(|| {
                        format!("Remote size disappeared for {}.", file.relative_path)
                    })?;
                    let checksum_type = file.remote_checksum_type.as_deref().ok_or_else(|| {
                        format!(
                            "Remote checksum type disappeared for {}.",
                            file.relative_path
                        )
                    })?;
                    let checksum = file.remote_checksum.as_deref().ok_or_else(|| {
                        format!("Remote checksum disappeared for {}.", file.relative_path)
                    })?;
                    restore_count = restore_count.checked_add(1).ok_or_else(|| {
                        "Restore plan file count exceeded supported range.".to_string()
                    })?;
                    restore_bytes = restore_bytes.checked_add(remote_size).ok_or_else(|| {
                        "Restore plan byte count exceeded supported range.".to_string()
                    })?;
                    statement
                        .execute(params![
                            Uuid::new_v4().to_string(),
                            plan_id,
                            workspace_id,
                            file.relative_path,
                            action,
                            "ready",
                            if file.local_present { 1 } else { 0 },
                            file.local_hash,
                            file.local_size
                                .map(|size| to_i64(size, "planned local file size"))
                                .transpose()?,
                            remote_id,
                            to_i64(remote_size, "planned remote file size")?,
                            checksum_type,
                            checksum,
                            Option::<String>::None,
                            now,
                        ])
                        .map_err(|error| format!("Could not add file to restore plan: {error}"))?;
                }
                PlanDecision::Blocked(reason) => {
                    blocked_count = blocked_count.checked_add(1).ok_or_else(|| {
                        "Restore block count exceeded supported range.".to_string()
                    })?;
                    statement
                        .execute(params![
                            Uuid::new_v4().to_string(),
                            plan_id,
                            workspace_id,
                            file.relative_path,
                            "blocked",
                            "blocked",
                            if file.local_present { 1 } else { 0 },
                            file.local_hash,
                            file.local_size
                                .map(|size| to_i64(size, "blocked local file size"))
                                .transpose()?,
                            file.remote_id,
                            file.remote_size
                                .map(|size| to_i64(size, "blocked remote file size"))
                                .transpose()?,
                            file.remote_checksum_type,
                            file.remote_checksum,
                            reason,
                            now,
                        ])
                        .map_err(|error| {
                            format!("Could not add safety block to restore plan: {error}")
                        })?;
                }
            }
        }
    }

    let final_status = if restore_count > 0 {
        "ready"
    } else if blocked_count > 0 {
        "partial"
    } else {
        "completed"
    };
    let completed_at = if restore_count == 0 {
        Some(now.as_str())
    } else {
        None
    };
    transaction
        .execute(
            "UPDATE restore_plans
             SET status = ?1, restore_count = ?2, restore_bytes = ?3,
                 blocked_count = ?4, completed_at = ?5
             WHERE id = ?6",
            params![
                final_status,
                to_i64(restore_count, "planned restore count")?,
                to_i64(restore_bytes, "planned restore bytes")?,
                to_i64(blocked_count, "blocked restore count")?,
                completed_at,
                plan_id,
            ],
        )
        .map_err(|error| format!("Could not finalize restore plan summary: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit restore plan: {error}"))?;
    load_plan_with_connection(connection, &plan_id)
}

fn execution_context(app: &AppHandle, plan_id: &str) -> Result<RestoreExecutionContext, String> {
    let connection = open_restore_database(app)?;
    let context = connection
        .query_row(
            "SELECT
                p.workspace_id, p.provider_id, p.remote_path, p.status,
                b.provider_id, b.remote_path
             FROM restore_plans p
             LEFT JOIN workspace_remote_bindings b ON b.workspace_id = p.workspace_id
             WHERE p.id = ?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read restore execution context: {error}"))?
        .ok_or_else(|| "Restore plan was not found.".to_string())?;
    if context.3 != "ready" {
        return Err(format!(
            "Restore plan is {} and cannot be started again. Prepare a fresh plan.",
            context.3
        ));
    }
    if context.4.as_deref() != Some(context.1.as_str())
        || context.5.as_deref() != Some(context.2.as_str())
    {
        return Err(
            "Workspace cloud binding changed after this restore plan was prepared. Prepare a fresh plan."
                .into(),
        );
    }
    let active_backup: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM backup_plans
             WHERE workspace_id = ?1 AND status = 'running'",
            params![context.0],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active backup execution: {error}"))?;
    if active_backup > 0 {
        return Err("A backup execution is already running for this workspace.".into());
    }
    Ok(RestoreExecutionContext {
        workspace_id: context.0,
        provider_id: context.1,
        remote_path: context.2,
    })
}

fn begin_execution(app: &AppHandle, plan_id: &str) -> Result<Vec<RestoreOperation>, String> {
    let mut connection = open_restore_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start restore execution transaction: {error}"))?;
    let changed = transaction
        .execute(
            "UPDATE restore_plans
             SET status = 'running', last_error = NULL
             WHERE id = ?1 AND status = 'ready'",
            params![plan_id],
        )
        .map_err(|error| format!("Could not start restore plan: {error}"))?;
    if changed == 0 {
        return Err("Restore plan is no longer ready. Prepare a fresh plan.".into());
    }
    let operations = {
        let mut statement = transaction
            .prepare(
                "SELECT
                    id, workspace_id, relative_path, action,
                    expected_local_present, expected_local_hash, expected_local_size,
                    expected_remote_id, expected_remote_size,
                    expected_remote_checksum_type, expected_remote_checksum
                 FROM restore_plan_items
                 WHERE plan_id = ?1 AND status = 'ready'
                   AND action IN ('create', 'update')
                 ORDER BY relative_path ASC",
            )
            .map_err(|error| format!("Could not prepare restore operation query: {error}"))?;
        let rows = statement
            .query_map(params![plan_id], operation_from_row)
            .map_err(|error| format!("Could not query restore operations: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("Could not read restore operations: {error}"))?
    };
    transaction
        .commit()
        .map_err(|error| format!("Could not commit restore execution start: {error}"))?;
    Ok(operations)
}

fn mark_operation_running(app: &AppHandle, operation_id: &str) -> Result<(), String> {
    let connection = open_restore_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE restore_plan_items
             SET status = 'running', updated_at = ?1, last_error = NULL
             WHERE id = ?2 AND status = 'ready'",
            params![now, operation_id],
        )
        .map_err(|error| format!("Could not start restore item: {error}"))?;
    if changed == 0 {
        return Err("Restore item is no longer ready.".into());
    }
    Ok(())
}

fn mark_operation_applying(
    app: &AppHandle,
    operation_id: &str,
    downloaded_hash: &str,
    downloaded_size: u64,
) -> Result<(), String> {
    let connection = open_restore_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE restore_plan_items
             SET status = 'applying', downloaded_local_hash = ?1,
                 downloaded_local_size = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'running'",
            params![
                downloaded_hash,
                to_i64(downloaded_size, "downloaded local size")?,
                now,
                operation_id,
            ],
        )
        .map_err(|error| format!("Could not arm recoverable restore mutation: {error}"))?;
    if changed == 0 {
        return Err("Restore item changed before local apply.".into());
    }
    Ok(())
}

async fn execute_restore_operation(
    app: &AppHandle,
    context: &RestoreExecutionContext,
    operation: &RestoreOperation,
    token: &str,
) -> Result<(), String> {
    let current = current_file_evidence(app, &operation.workspace_id, &operation.relative_path)?;
    ensure_current_evidence_matches_plan(operation, &current)?;
    let workspace = find_workspace(app, &operation.workspace_id)?;
    if scanner::is_path_ignored_for_sync(
        Path::new(&workspace.local_path),
        &operation.relative_path,
    )? {
        return Err(
            "Restore path is now excluded by AtrisBridge safety or .atrisbridgeignore rules. Prepare a fresh plan."
      .into(),
        );
    }
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
    .map_err(|error| format!("Remote restore preflight worker failed: {error}"))??;
    ensure_remote_matches_plan(operation, &preflight)?;

    let target = resolve_restore_target(&workspace, &operation.relative_path, true)?;
    validate_local_target(operation, &target)?;
    let (stage, backup) = artifact_paths(&target, &operation.id)?;
    ensure_artifact_absent(&stage)?;
    ensure_artifact_absent(&backup)?;

    let download_app = app.clone();
    let download_token = token.to_string();
    let download_path = remote_file_path.clone();
    let download_stage = stage.clone();
    let download_result = match tauri::async_runtime::spawn_blocking(move || {
        download_remote_to_stage(
            &download_app,
            &download_token,
            &download_path,
            &download_stage,
        )
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(format!("Restore download worker failed: {error}")),
    };
    if let Err(error) = download_result {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }

    let staged_evidence = (|| -> Result<(u64, String), String> {
        let (downloaded_size, downloaded_hash) = scanner::fingerprint_file(&stage)?;
        let stage_md5 = rclone::local_file_md5(app, &stage)?;
        ensure_download_matches_remote(operation, downloaded_size, &stage_md5)?;
        Ok((downloaded_size, downloaded_hash))
    })();
    let (downloaded_size, downloaded_hash) = match staged_evidence {
        Ok(evidence) => evidence,
        Err(error) => {
            remove_regular_file_best_effort(&stage);
            return Err(error);
        }
    };

    let final_remote_app = app.clone();
    let final_remote_token = token.to_string();
    let final_remote_path = remote_file_path.clone();
    let final_remote_relative = operation.relative_path.clone();
    let final_remote_result = match tauri::async_runtime::spawn_blocking(move || {
        rclone::stat_google_drive_file(
            &final_remote_app,
            &final_remote_token,
            &final_remote_path,
            &final_remote_relative,
        )
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(format!(
            "Final remote restore verification worker failed: {error}"
        )),
    };
    let final_remote = match final_remote_result {
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
    if let Err(error) = validate_local_target(operation, &target) {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }
    if let Err(error) =
        mark_operation_applying(app, &operation.id, &downloaded_hash, downloaded_size)
    {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }

    if let Err(error) = apply_verified_download(operation, &target, &stage, &backup) {
        remove_regular_file_best_effort(&stage);
        return Err(error);
    }

    let (applied_size, applied_hash) = match scanner::fingerprint_file(&target) {
        Ok(evidence) => evidence,
        Err(error) => {
            return rollback_after_apply_failure(
                operation,
                &target,
                &backup,
                &downloaded_hash,
                downloaded_size,
                format!("Could not verify restored local target: {error}"),
            )
        }
    };
    if applied_size != downloaded_size || applied_hash != downloaded_hash {
        return rollback_after_apply_failure(
            operation,
            &target,
            &backup,
            &downloaded_hash,
            downloaded_size,
            "Local content changed while the verified restore was being applied.".into(),
        );
    }

    let modified_at = match file_modified_at(&target) {
        Ok(value) => value,
        Err(error) => {
            return rollback_after_apply_failure(
                operation,
                &target,
                &backup,
                &downloaded_hash,
                downloaded_size,
                format!("Could not read restored local metadata: {error}"),
            )
        }
    };
    if let Err(error) = complete_operation(
        app,
        operation,
        &final_remote,
        &downloaded_hash,
        downloaded_size,
        modified_at,
    ) {
        let rollback = rollback_applied_restore(
            operation,
            &target,
            &backup,
            &downloaded_hash,
            downloaded_size,
        );
        return match rollback {
            Ok(()) => Err(format!(
                "Restore journal commit failed and the local change was rolled back: {error}"
            )),
            Err(rollback_error) => Err(format!(
                "Restore journal commit failed and automatic rollback could not be proven safe: {error}. Recovery detail: {rollback_error}"
            )),
        };
    }

    remove_regular_file_best_effort(&backup);
    Ok(())
}

fn apply_verified_download(
    operation: &RestoreOperation,
    target: &Path,
    stage: &Path,
    backup: &Path,
) -> Result<(), String> {
    match operation.action.as_str() {
        "create" => {
            if fs::symlink_metadata(target).is_ok() {
                return Err(
                    "Local path appeared immediately before restore. AtrisBridge blocked the create."
                        .into(),
                );
            }
            fs::rename(stage, target)
                .map_err(|error| format!("Could not atomically place restored file: {error}"))?;
        }
        "update" => {
            validate_local_target(operation, target)?;
            fs::rename(target, backup)
                .map_err(|error| format!("Could not create recoverable local backup: {error}"))?;
            if let Err(error) = fs::rename(stage, target) {
                let restore_result = fs::rename(backup, target);
                return match restore_result {
                    Ok(()) => Err(format!(
                        "Could not place restored file; original local file was restored: {error}"
                    )),
                    Err(restore_error) => Err(format!(
                        "Could not place restored file and could not restore the original automatically: {error}; rollback error: {restore_error}"
                    )),
                };
            }
        }
        _ => return Err("Unsupported Phase 5 restore action.".into()),
    }
    Ok(())
}

fn rollback_applied_restore(
    operation: &RestoreOperation,
    target: &Path,
    backup: &Path,
    downloaded_hash: &str,
    downloaded_size: u64,
) -> Result<(), String> {
    match operation.action.as_str() {
        "create" => {
            if file_matches(target, downloaded_hash, downloaded_size)? {
                fs::remove_file(target).map_err(|error| {
                    format!("Could not remove uncommitted restored file: {error}")
                })?;
                Ok(())
            } else {
                Err(
                    "Restored target changed after apply; AtrisBridge preserved it for manual inspection."
                        .into(),
                )
            }
        }
        "update" => {
            if !backup.is_file() {
                return Err("Original local recovery copy is missing.".into());
            }
            if !file_matches(target, downloaded_hash, downloaded_size)? {
                return Err(
                    "Restored target changed after apply; AtrisBridge preserved both files for manual inspection."
                        .into(),
                );
            }
            fs::remove_file(target).map_err(|error| {
                format!("Could not remove uncommitted restored target: {error}")
            })?;
            fs::rename(backup, target)
                .map_err(|error| format!("Could not restore original local file: {error}"))
        }
        _ => Err("Unsupported restore rollback action.".into()),
    }
}

fn rollback_after_apply_failure(
    operation: &RestoreOperation,
    target: &Path,
    backup: &Path,
    downloaded_hash: &str,
    downloaded_size: u64,
    reason: String,
) -> Result<(), String> {
    match rollback_applied_restore(operation, target, backup, downloaded_hash, downloaded_size) {
        Ok(()) => Err(format!("{reason} The original local state was restored.")),
        Err(rollback_error) => Err(format!(
            "{reason} Automatic rollback could not be proven safe: {rollback_error}"
        )),
    }
}

fn complete_operation(
    app: &AppHandle,
    operation: &RestoreOperation,
    observation: &RemoteFileObservation,
    local_hash: &str,
    local_size: u64,
    local_modified_at: Option<String>,
) -> Result<(), String> {
    ensure_remote_matches_plan(operation, observation)?;
    let mut connection = open_restore_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start restore completion transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();

    let file_changed = transaction
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
                 state = 'synced',
                 tombstone = 0
             WHERE workspace_id = ?10
               AND relative_path = ?11
               AND remote_present = 1
               AND remote_id = ?4
               AND remote_checksum_type = ?7
               AND remote_checksum = ?8",
            params![
                to_i64(local_size, "restored local size")?,
                local_modified_at,
                local_hash,
                operation.expected_remote_id,
                to_i64(observation.size, "restored remote size")?,
                observation.modified_at,
                operation.expected_remote_checksum_type,
                operation.expected_remote_checksum,
                now,
                operation.workspace_id,
                operation.relative_path,
            ],
        )
        .map_err(|error| format!("Could not establish restored synchronized baseline: {error}"))?;
    if file_changed == 0 {
        return Err(
            "Restore journal evidence changed before completion; baseline was not accepted.".into(),
        );
    }
    let item_changed = transaction
        .execute(
            "UPDATE restore_plan_items
             SET status = 'completed', completed_local_hash = ?1,
                 updated_at = ?2, last_error = NULL
             WHERE id = ?3 AND status = 'applying'",
            params![local_hash, now, operation.id],
        )
        .map_err(|error| format!("Could not complete restore item journal: {error}"))?;
    if item_changed == 0 {
        return Err("Restore item journal changed before completion.".into());
    }
    transaction
        .commit()
        .map_err(|error| format!("Could not commit restore completion: {error}"))
}

fn fail_operation(app: &AppHandle, operation_id: &str, error: &str) -> Result<(), String> {
    let connection = open_restore_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE restore_plan_items
             SET status = 'failed', last_error = ?1, updated_at = ?2
             WHERE id = ?3 AND status IN ('ready', 'running', 'applying')",
            params![truncate_error(error), now, operation_id],
        )
        .map_err(|db_error| format!("Could not record failed restore item: {db_error}"))?;
    Ok(())
}

fn finalize_plan(app: &AppHandle, plan_id: &str) -> Result<RestoreExecutionReport, String> {
    let mut connection = open_restore_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start restore finalization: {error}"))?;
    let (completed, failed, remaining, restored_bytes): (i64, i64, i64, i64) = transaction
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN ('ready', 'running', 'applying') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'completed' THEN expected_remote_size ELSE 0 END), 0)
             FROM restore_plan_items
             WHERE plan_id = ?1 AND action IN ('create', 'update')",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| format!("Could not summarize restore execution: {error}"))?;
    if remaining != 0 {
        return Err("Restore plan still contains unfinished operations.".into());
    }
    let blocked: i64 = transaction
        .query_row(
            "SELECT blocked_count FROM restore_plans WHERE id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not read restore block count: {error}"))?
        .ok_or_else(|| "Restore plan was not found during finalization.".to_string())?;
    let status = if failed > 0 && completed == 0 {
        "failed"
    } else if failed > 0 || blocked > 0 {
        "partial"
    } else {
        "completed"
    };
    let finished_at = Utc::now().to_rfc3339();
    transaction
        .execute(
            "UPDATE restore_plans
             SET status = ?1, completed_count = ?2, failed_count = ?3,
                 completed_at = ?4,
                 last_error = CASE
                    WHEN ?3 > 0 THEN 'One or more restore items failed.'
                    ELSE NULL
                 END
             WHERE id = ?5 AND status = 'running'",
            params![status, completed, failed, finished_at, plan_id],
        )
        .map_err(|error| format!("Could not finalize restore plan: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit restore finalization: {error}"))?;
    Ok(RestoreExecutionReport {
        plan_id: plan_id.to_string(),
        status: status.to_string(),
        completed_count: from_i64(completed, "completed restore count")?,
        failed_count: from_i64(failed, "failed restore count")?,
        restored_bytes: from_i64(restored_bytes, "restored byte count")?,
        finished_at,
    })
}

fn load_file_evidence(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Vec<FileEvidence>, String> {
    let mut statement = connection
        .prepare(
            "SELECT
                relative_path,
                local_present, local_size, local_hash,
                remote_present, remote_id, remote_size,
                remote_checksum_type, remote_checksum,
                last_synced_hash,
                last_synced_remote_checksum_type,
                last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1
             ORDER BY relative_path ASC",
        )
        .map_err(|error| format!("Could not prepare restore evidence query: {error}"))?;
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
        .map_err(|error| format!("Could not query restore evidence: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read restore evidence: {error}"))
}

fn classify(file: &FileEvidence, portable_collision: bool) -> PlanDecision {
    if let Err(reason) = validate_restore_relative_path(&file.relative_path) {
        return PlanDecision::Blocked(reason);
    }
    if portable_collision {
        return PlanDecision::Blocked(
            "Two remote paths collide on a case-insensitive local filesystem. Phase 5 preserves both remotely and restores neither automatically."
                .into(),
        );
    }
    if !file.remote_present {
        if file.local_present && file.last_synced_hash.is_some() {
            return PlanDecision::Blocked(
                "Remote file is missing. Phase 5 never converts a remote deletion into a local deletion."
                    .into(),
            );
        }
        return PlanDecision::Skip;
    }
    if file.remote_id.as_deref().unwrap_or("").is_empty() {
        return PlanDecision::Blocked(
            "Google Drive did not provide a stable file ID for this object.".into(),
        );
    }
    if file.remote_size.is_none() {
        return PlanDecision::Blocked("Google Drive did not provide a file size.".into());
    }
    if !has_valid_md5(
        file.remote_checksum_type.as_deref(),
        file.remote_checksum.as_deref(),
    ) {
        return PlanDecision::Blocked(
            "Google Drive did not provide verifiable MD5 evidence for this file.".into(),
        );
    }
    if !file.local_present {
        return PlanDecision::Restore("create");
    }

    let Some(last_local) = file.last_synced_hash.as_deref() else {
        return PlanDecision::Blocked(
            "Local and remote files overlap without an AtrisBridge synchronized baseline.".into(),
        );
    };
    let Some(last_remote_type) = file.last_synced_remote_checksum_type.as_deref() else {
        return PlanDecision::Blocked(
            "The synchronized baseline has no remote checksum type.".into(),
        );
    };
    let Some(last_remote_checksum) = file.last_synced_remote_checksum.as_deref() else {
        return PlanDecision::Blocked("The synchronized baseline has no remote checksum.".into());
    };
    let local_matches = file.local_hash.as_deref() == Some(last_local);
    let remote_matches = file.remote_checksum_type.as_deref() == Some(last_remote_type)
        && file.remote_checksum.as_deref() == Some(last_remote_checksum);
    match (local_matches, remote_matches) {
        (true, true) => PlanDecision::Skip,
        (true, false) => PlanDecision::Restore("update"),
        (false, true) => PlanDecision::Blocked(
            "Local file changed after the last synchronized baseline. Restore preserves the local change."
                .into(),
        ),
        (false, false) => PlanDecision::Blocked(
            "Local and remote files both changed after the synchronized baseline. Manual conflict resolution is required."
                .into(),
        ),
    }
}

fn portable_collision_keys(evidence: &[FileEvidence]) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for file in evidence.iter().filter(|file| file.remote_present) {
        *counts
            .entry(portable_path_key(&file.relative_path))
            .or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(key, count)| (count > 1).then_some(key))
        .collect()
}

fn portable_path_key(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}

fn current_file_evidence(
    app: &AppHandle,
    workspace_id: &str,
    relative_path: &str,
) -> Result<CurrentFileEvidence, String> {
    let connection = open_restore_database(app)?;
    connection
        .query_row(
            "SELECT
                local_present, local_hash, local_size,
                remote_present, remote_id, remote_size,
                remote_checksum_type, remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1 AND relative_path = ?2",
            params![workspace_id, relative_path],
            |row| {
                Ok(CurrentFileEvidence {
                    local_present: row.get::<_, i64>(0)? != 0,
                    local_hash: row.get(1)?,
                    local_size: optional_u64(row, 2)?,
                    remote_present: row.get::<_, i64>(3)? != 0,
                    remote_id: row.get(4)?,
                    remote_size: optional_u64(row, 5)?,
                    remote_checksum_type: row.get(6)?,
                    remote_checksum: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read current restore evidence: {error}"))?
        .ok_or_else(|| "File is no longer present in the AtrisBridge journal.".to_string())
}

fn ensure_current_evidence_matches_plan(
    operation: &RestoreOperation,
    current: &CurrentFileEvidence,
) -> Result<(), String> {
    if current.local_present != operation.expected_local_present
        || current.local_hash != operation.expected_local_hash
        || current.local_size != operation.expected_local_size
    {
        return Err(
            "Local evidence changed after the restore plan was prepared. Prepare a fresh plan."
                .into(),
        );
    }
    if !current.remote_present
        || current.remote_id.as_deref() != Some(operation.expected_remote_id.as_str())
        || current.remote_size != Some(operation.expected_remote_size)
        || current.remote_checksum_type.as_deref()
            != Some(operation.expected_remote_checksum_type.as_str())
        || current.remote_checksum.as_deref() != Some(operation.expected_remote_checksum.as_str())
    {
        return Err(
            "Remote evidence changed after the restore plan was prepared. Prepare a fresh plan."
                .into(),
        );
    }
    Ok(())
}

fn ensure_remote_matches_plan(
    operation: &RestoreOperation,
    observation: &RemoteFileObservation,
) -> Result<(), String> {
    if observation.remote_id.as_deref() != Some(operation.expected_remote_id.as_str())
        || observation.size != operation.expected_remote_size
        || observation.checksum_type.as_deref()
            != Some(operation.expected_remote_checksum_type.as_str())
        || observation.checksum.as_deref() != Some(operation.expected_remote_checksum.as_str())
    {
        return Err(
            "Google Drive file changed during restore verification. AtrisBridge blocked the local write."
                .into(),
        );
    }
    Ok(())
}

fn ensure_download_matches_remote(
    operation: &RestoreOperation,
    local_size: u64,
    local_md5: &str,
) -> Result<(), String> {
    if local_size != operation.expected_remote_size {
        return Err("Downloaded content size did not match verified remote evidence.".into());
    }
    if operation
        .expected_remote_checksum_type
        .eq_ignore_ascii_case("MD5")
    {
        if local_md5.eq_ignore_ascii_case(&operation.expected_remote_checksum) {
            return Ok(());
        }
        return Err("Downloaded content did not match verified Google Drive MD5 evidence.".into());
    }
    if operation.expected_remote_checksum_type == rclone::CRYPT_CHECKSUM_TYPE {
        return Ok(());
    }
    Err("Downloaded content uses an unsupported remote checksum type.".into())
}

fn validate_local_target(operation: &RestoreOperation, target: &Path) -> Result<(), String> {
    match operation.action.as_str() {
        "create" => match fs::symlink_metadata(target) {
            Ok(_) => {
                Err("Local path exists; AtrisBridge will not overwrite an unplanned target.".into())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Could not inspect local restore target: {error}")),
        },
        "update" => {
            let metadata = fs::symlink_metadata(target)
                .map_err(|error| format!("Could not inspect local restore target: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(
                    "Restore target is not the regular file that was present during planning."
                        .into(),
                );
            }
            let (size, hash) = scanner::fingerprint_file(target)?;
            if Some(size) != operation.expected_local_size
                || Some(hash.as_str()) != operation.expected_local_hash.as_deref()
            {
                return Err(
                    "Local file changed while restore was waiting. AtrisBridge blocked the overwrite."
                        .into(),
                );
            }
            Ok(())
        }
        _ => Err("Unsupported Phase 5 restore action.".into()),
    }
}

fn resolve_restore_target(
    workspace: &Workspace,
    relative_path: &str,
    create_parents: bool,
) -> Result<PathBuf, String> {
    let segments = validate_restore_relative_path(relative_path)?;
    let root = PathBuf::from(&workspace.local_path)
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    if !root.is_dir() {
        return Err("Workspace root is no longer a directory.".into());
    }
    let (file_name, parent_segments) = segments
        .split_last()
        .ok_or_else(|| "Restore path has no file name.".to_string())?;
    let mut parent = root.clone();
    for segment in parent_segments {
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(
                        "Restore path crosses a symbolic link. AtrisBridge does not follow symlinks."
                            .into(),
                    );
                }
                if !metadata.is_dir() {
                    return Err("Restore path crosses a non-directory local entry.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&next)
                    .map_err(|error| format!("Could not create restore directory: {error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("Restore parent directory is missing during recovery.".into());
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect restore parent directory: {error}"
                ))
            }
        }
        parent = next
            .canonicalize()
            .map_err(|error| format!("Could not resolve restore parent directory: {error}"))?;
        if !parent.starts_with(&root) {
            return Err("Restore path escaped the selected workspace root.".into());
        }
    }
    let target = parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err("Restore target is a symbolic link and was blocked.".into());
        }
        if metadata.is_dir() {
            return Err("Restore target collides with an existing directory.".into());
        }
    }
    Ok(target)
}

fn validate_restore_relative_path(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') {
        return Err(
            "Remote path cannot be mapped safely to a portable local file name in Phase 5.".into(),
        );
    }
    let mut segments = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("Remote path contains an unsafe local path segment.".into());
        }
        if segment.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        }) {
            return Err(
                "Remote file name contains characters that are unsafe on supported local filesystems."
                    .into(),
            );
        }
        if segment.ends_with(' ') || segment.ends_with('.') {
            return Err(
                "Remote file name ends with a dot or space and cannot be restored portably.".into(),
            );
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
            return Err(
                "Remote file name is reserved on Windows and was blocked from local restore."
                    .into(),
            );
        }
        segments.push(segment.to_string());
    }
    Ok(segments)
}

fn artifact_paths(target: &Path, operation_id: &str) -> Result<(PathBuf, PathBuf), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "Restore target has no parent directory.".to_string())?;
    let safe_id: String = operation_id
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect();
    if safe_id.len() < 16 {
        return Err("Restore operation identifier is invalid.".into());
    }
    Ok((
        parent.join(format!(".atrisbridge-{safe_id}.part")),
        parent.join(format!(".atrisbridge-{safe_id}.bak")),
    ))
}

fn ensure_artifact_absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(
            "A restore recovery artifact already exists. Resolve the previous recovery before retrying."
                .into(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not inspect restore recovery artifact: {error}")),
    }
}

fn file_modified_at(path: &Path) -> Result<Option<String>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not read restored file metadata: {error}"))?;
    match metadata.modified() {
        Ok(value) => Ok(Some(DateTime::<Utc>::from(value).to_rfc3339())),
        Err(_) => Ok(None),
    }
}

fn file_matches(path: &Path, expected_hash: &str, expected_size: u64) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Could not inspect recovery target: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let (size, hash) = scanner::fingerprint_file(path)?;
    Ok(size == expected_size && hash == expected_hash)
}

fn remove_regular_file_best_effort(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            let _ = fs::remove_file(path);
        }
    }
}

fn ensure_managed_restore_root(remote_path: &str) -> Result<(), String> {
    let normalized = rclone::normalize_remote_path(remote_path)?;
    if normalized.starts_with("AtrisBridge/") {
        return Ok(());
    }
    Err(
        "Phase 5 restores are restricted to an AtrisBridge-managed Google Drive workspace path."
            .into(),
    )
}

fn has_valid_md5(kind: Option<&str>, hash: Option<&str>) -> bool {
    matches!(kind, Some(value) if value.eq_ignore_ascii_case("MD5") || value == rclone::CRYPT_CHECKSUM_TYPE)
        && hash.is_some_and(|value| {
            value.len() == 32 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
}

fn download_remote_to_stage(
    app: &AppHandle,
    token: &str,
    remote_file_path: &str,
    destination: &Path,
) -> Result<(), String> {
    rclone::download_google_drive_file_to_stage(app, token, remote_file_path, destination)
}

fn locate_restore_runtime(app: &AppHandle) -> Result<PathBuf, String> {
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
        validate_restore_runtime(resource_path.clone())?;
        return Ok(resource_path);
    }
    #[cfg(debug_assertions)]
    {
        let development_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(executable_name);
        if development_path.is_file() {
            validate_restore_runtime(development_path.clone())?;
            return Ok(development_path);
        }
    }
    Err(format!(
        "AtrisBridge requires its pinned rclone sidecar (v{}). Run `npm run sidecar:prepare` for local development.",
        rclone::REQUIRED_RCLONE_VERSION
    ))
}

fn validate_restore_runtime(executable: PathBuf) -> Result<(), String> {
    let output = clean_restore_command(&executable)
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

fn clean_restore_command(executable: &Path) -> Command {
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

fn load_interrupted_items(connection: &Connection) -> Result<Vec<InterruptedItem>, String> {
    let mut statement = connection
        .prepare(
            "SELECT
                id, workspace_id, relative_path, action, status,
                downloaded_local_hash, downloaded_local_size
             FROM restore_plan_items
             WHERE status IN ('running', 'applying')
             ORDER BY updated_at ASC",
        )
        .map_err(|error| format!("Could not prepare interrupted restore query: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(InterruptedItem {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                relative_path: row.get(2)?,
                action: row.get(3)?,
                status: row.get(4)?,
                downloaded_hash: row.get(5)?,
                downloaded_size: optional_u64(row, 6)?,
            })
        })
        .map_err(|error| format!("Could not query interrupted restore items: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read interrupted restore items: {error}"))
}

fn recover_interrupted_item(
    workspace: &Workspace,
    item: &InterruptedItem,
) -> Result<String, String> {
    let target = resolve_restore_target(workspace, &item.relative_path, false)?;
    let (stage, backup) = artifact_paths(&target, &item.id)?;
    if item.status == "running" {
        remove_regular_file_best_effort(&stage);
        return Ok(
            "Interrupted restore stopped before local apply. Any verified staging file was removed."
                .into(),
        );
    }
    let downloaded_hash = item.downloaded_hash.as_deref().ok_or_else(|| {
        "Restore reached apply state without a persisted downloaded fingerprint.".to_string()
    })?;
    let downloaded_size = item.downloaded_size.ok_or_else(|| {
        "Restore reached apply state without a persisted downloaded size.".to_string()
    })?;
    match item.action.as_str() {
        "create" => {
            if file_matches(&target, downloaded_hash, downloaded_size)? {
                fs::remove_file(&target)
                    .map_err(|error| format!("Could not roll back interrupted create: {error}"))?;
                remove_regular_file_best_effort(&stage);
                Ok("Interrupted create was rolled back to the original absent-local state.".into())
            } else {
                remove_regular_file_best_effort(&stage);
                Ok(
                    "Interrupted create target no longer matches the staged restore. AtrisBridge preserved it for manual inspection."
                        .into(),
                )
            }
        }
        "update" => {
            if backup.is_file() {
                if fs::symlink_metadata(&target).is_err() {
                    fs::rename(&backup, &target).map_err(|error| {
                        format!("Could not restore original file after interrupted update: {error}")
                    })?;
                    remove_regular_file_best_effort(&stage);
                    return Ok(
                        "Interrupted update was rolled back from the recoverable local backup."
                            .into(),
                    );
                }
                if file_matches(&target, downloaded_hash, downloaded_size)? {
                    fs::remove_file(&target).map_err(|error| {
                        format!("Could not remove interrupted restored target: {error}")
                    })?;
                    fs::rename(&backup, &target).map_err(|error| {
                        format!("Could not restore original local file: {error}")
                    })?;
                    remove_regular_file_best_effort(&stage);
                    return Ok(
                        "Interrupted update was rolled back to the original local file.".into(),
                    );
                }
                remove_regular_file_best_effort(&stage);
                return Ok(
                    "Interrupted update target changed after apply. AtrisBridge preserved both the target and .bak recovery file for manual inspection."
                        .into(),
                );
            }
            remove_regular_file_best_effort(&stage);
            Ok(
                "Interrupted update stopped before the original local file was moved. Local content was preserved."
                    .into(),
            )
        }
        _ => Err("Interrupted restore item has an unsupported action.".into()),
    }
}

fn load_plan_with_connection(
    connection: &Connection,
    plan_id: &str,
) -> Result<RestorePlan, String> {
    let metadata = connection
        .query_row(
            "SELECT
                id, workspace_id, provider_id, remote_path, status,
                created_at, local_scan_at, remote_inventory_at,
                restore_count, restore_bytes, blocked_count,
                completed_count, failed_count, completed_at
             FROM restore_plans WHERE id = ?1",
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
                    row.get::<_, Option<String>>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read restore plan: {error}"))?
        .ok_or_else(|| "Restore plan was not found.".to_string())?;
    let total_items: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM restore_plan_items WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not count restore plan items: {error}"))?;
    let mut statement = connection
        .prepare(
            "SELECT id, relative_path, action, status,
                    expected_remote_size, block_reason, last_error
             FROM restore_plan_items
             WHERE plan_id = ?1
             ORDER BY CASE WHEN action = 'blocked' THEN 0 ELSE 1 END, relative_path ASC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare restore plan preview query: {error}"))?;
    let rows = statement
        .query_map(
            params![plan_id, PREVIEW_LIMIT as i64],
            restore_plan_item_from_row,
        )
        .map_err(|error| format!("Could not query restore plan items: {error}"))?;
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read restore plan items: {error}"))?;
    Ok(RestorePlan {
        id: metadata.0,
        workspace_id: metadata.1,
        provider_id: metadata.2,
        remote_path: metadata.3,
        status: metadata.4,
        created_at: metadata.5,
        local_scan_at: metadata.6,
        remote_inventory_at: metadata.7,
        restore_count: from_i64(metadata.8, "restore count")?,
        restore_bytes: from_i64(metadata.9, "restore bytes")?,
        blocked_count: from_i64(metadata.10, "restore blocked count")?,
        completed_count: from_i64(metadata.11, "restore completed count")?,
        failed_count: from_i64(metadata.12, "restore failed count")?,
        completed_at: metadata.13,
        preview_truncated: total_items > PREVIEW_LIMIT as i64,
        items,
    })
}

fn operation_from_row(row: &Row<'_>) -> rusqlite::Result<RestoreOperation> {
    let remote_id: Option<String> = row.get(7)?;
    let remote_size = optional_u64(row, 8)?;
    let checksum_type: Option<String> = row.get(9)?;
    let checksum: Option<String> = row.get(10)?;
    Ok(RestoreOperation {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        relative_path: row.get(2)?,
        action: row.get(3)?,
        expected_local_present: row.get::<_, i64>(4)? != 0,
        expected_local_hash: row.get(5)?,
        expected_local_size: optional_u64(row, 6)?,
        expected_remote_id: remote_id.ok_or(rusqlite::Error::InvalidQuery)?,
        expected_remote_size: remote_size.ok_or(rusqlite::Error::InvalidQuery)?,
        expected_remote_checksum_type: checksum_type.ok_or(rusqlite::Error::InvalidQuery)?,
        expected_remote_checksum: checksum.ok_or(rusqlite::Error::InvalidQuery)?,
    })
}

fn restore_plan_item_from_row(row: &Row<'_>) -> rusqlite::Result<RestorePlanItem> {
    Ok(RestorePlanItem {
        id: row.get(0)?,
        relative_path: row.get(1)?,
        action: row.get(2)?,
        status: row.get(3)?,
        size: optional_u64(row, 4)?,
        block_reason: row.get(5)?,
        last_error: row.get(6)?,
    })
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
    u64::try_from(value).map_err(|_| format!("{label} was negative in the restore journal."))
}

fn truncate_error(value: &str) -> String {
    const LIMIT: usize = 800;
    value.chars().take(LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> FileEvidence {
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
    fn remote_only_file_is_a_safe_create() {
        let mut file = evidence();
        file.local_present = false;
        file.local_hash = None;
        file.local_size = None;
        file.last_synced_hash = None;
        file.last_synced_remote_checksum_type = None;
        file.last_synced_remote_checksum = None;
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Restore("create")
        ));
    }

    #[test]
    fn remote_change_with_unchanged_local_is_a_safe_update() {
        let mut file = evidence();
        file.remote_checksum = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert!(matches!(
            classify(&file, false),
            PlanDecision::Restore("update")
        ));
    }

    #[test]
    fn local_change_blocks_restore_overwrite() {
        let mut file = evidence();
        file.local_hash = Some("local-user-change".into());
        assert!(matches!(classify(&file, false), PlanDecision::Blocked(_)));
    }

    #[test]
    fn unverified_overlap_is_blocked() {
        let mut file = evidence();
        file.last_synced_hash = None;
        file.last_synced_remote_checksum_type = None;
        file.last_synced_remote_checksum = None;
        assert!(matches!(classify(&file, false), PlanDecision::Blocked(_)));
    }

    #[test]
    fn portable_restore_path_rejects_parent_and_windows_reserved_names() {
        assert!(validate_restore_relative_path("src/../secret.txt").is_err());
        assert!(validate_restore_relative_path("CON.txt").is_err());
        assert!(validate_restore_relative_path("src/bad:name.txt").is_err());
        assert!(validate_restore_relative_path("src/main.rs").is_ok());
    }

    #[test]
    fn restore_schema_initialization_is_idempotent() {
        let connection = Connection::open_in_memory().expect("database");
        ensure_restore_schema(&connection).expect("first schema");
        ensure_restore_schema(&connection).expect("second schema");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('restore_plans', 'restore_plan_items')",
                [],
                |row| row.get(0),
            )
            .expect("schema count");
        assert_eq!(count, 2);
    }

    #[test]
    fn case_insensitive_remote_collisions_are_blocked() {
        let mut first = evidence();
        first.relative_path = "Readme.md".into();
        let mut second = evidence();
        second.relative_path = "README.md".into();
        let evidence = vec![first, second];
        let keys = portable_collision_keys(&evidence);
        assert!(keys.contains("readme.md"));
    }
}
