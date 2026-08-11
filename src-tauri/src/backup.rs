use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    database::open_database,
    models::{BackupExecutionReport, BackupPlan, BackupPlanItem, RemoteFileObservation},
};

const PREVIEW_LIMIT: usize = 120;

#[derive(Debug, Clone)]
pub struct BackupExecutionContext {
    pub plan_id: String,
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
}

#[derive(Debug, Clone)]
pub struct BackupOperation {
    pub id: String,
    pub workspace_id: String,
    pub relative_path: String,
    pub action: String,
    pub local_hash: String,
    pub local_size: u64,
    pub expected_remote_present: bool,
    pub expected_remote_id: Option<String>,
    pub expected_remote_checksum_type: Option<String>,
    pub expected_remote_checksum: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CurrentFileEvidence {
    pub local_present: bool,
    pub local_hash: Option<String>,
    pub local_size: Option<u64>,
    pub remote_present: bool,
    pub remote_id: Option<String>,
    pub remote_checksum_type: Option<String>,
    pub remote_checksum: Option<String>,
}

#[derive(Debug)]
struct FileEvidence {
    relative_path: String,
    local_present: bool,
    local_size: Option<u64>,
    local_hash: Option<String>,
    remote_present: bool,
    remote_id: Option<String>,
    remote_checksum_type: Option<String>,
    remote_checksum: Option<String>,
    last_synced_hash: Option<String>,
    last_synced_remote_checksum_type: Option<String>,
    last_synced_remote_checksum: Option<String>,
}

enum PlanDecision {
    Skip,
    Upload(&'static str),
    Blocked(String),
}

pub fn create_plan(app: &AppHandle, workspace_id: &str) -> Result<BackupPlan, String> {
    let mut connection = open_database(app)?;
    create_plan_with_connection(&mut connection, workspace_id)
}

pub fn latest_plan(app: &AppHandle, workspace_id: &str) -> Result<Option<BackupPlan>, String> {
    let connection = open_database(app)?;
    let plan_id = connection
        .query_row(
            "SELECT id FROM backup_plans WHERE workspace_id = ?1 ORDER BY created_at DESC LIMIT 1",
            params![workspace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not read latest backup plan: {error}"))?;

    plan_id
        .map(|id| load_plan_with_connection(&connection, &id))
        .transpose()
}

pub fn execution_context(app: &AppHandle, plan_id: &str) -> Result<BackupExecutionContext, String> {
    let connection = open_database(app)?;
    let context = connection
        .query_row(
            "SELECT
                p.workspace_id, p.provider_id, p.remote_path, p.status,
                w.sync_mode, b.provider_id, b.remote_path
             FROM backup_plans p
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
        .map_err(|error| format!("Could not read backup execution context: {error}"))?
        .ok_or_else(|| "Backup plan was not found.".to_string())?;

    if context.3 != "ready" {
        return Err(format!(
            "Backup plan is {} and cannot be started again. Prepare a fresh plan.",
            context.3
        ));
    }
    if context.4 != "backup" {
        return Err("Only backup-mode workspaces can execute Phase 4 uploads.".into());
    }
    if context.5.as_deref() != Some(context.1.as_str())
        || context.6.as_deref() != Some(context.2.as_str())
    {
        return Err(
            "Workspace cloud binding changed after this plan was prepared. Prepare a fresh plan."
                .into(),
        );
    }

    Ok(BackupExecutionContext {
        plan_id: plan_id.to_string(),
        workspace_id: context.0,
        provider_id: context.1,
        remote_path: context.2,
    })
}

pub fn begin_execution(app: &AppHandle, plan_id: &str) -> Result<Vec<BackupOperation>, String> {
    let mut connection = open_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start backup execution transaction: {error}"))?;

    let changed = transaction
        .execute(
            "UPDATE backup_plans SET status = 'running', last_error = NULL
             WHERE id = ?1 AND status = 'ready'",
            params![plan_id],
        )
        .map_err(|error| format!("Could not start backup plan: {error}"))?;
    if changed == 0 {
        return Err("Backup plan is no longer ready. Prepare a fresh plan.".into());
    }

    let operations = {
        let mut statement = transaction
            .prepare(
                "SELECT
                    id, workspace_id, relative_path, action,
                    local_hash, local_size, expected_remote_present,
                    expected_remote_id, expected_remote_checksum_type, expected_remote_checksum
                 FROM backup_plan_items
                 WHERE plan_id = ?1 AND status = 'ready' AND action IN ('create', 'update')
                 ORDER BY relative_path ASC",
            )
            .map_err(|error| format!("Could not prepare backup operation query: {error}"))?;
        let rows = statement
            .query_map(params![plan_id], operation_from_row)
            .map_err(|error| format!("Could not query backup operations: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("Could not read backup operations: {error}"))?
    };

    transaction
        .commit()
        .map_err(|error| format!("Could not commit backup execution start: {error}"))?;
    Ok(operations)
}

pub fn mark_operation_running(app: &AppHandle, operation_id: &str) -> Result<(), String> {
    let connection = open_database(app)?;
    let now = Utc::now().to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE backup_plan_items
             SET status = 'running', updated_at = ?1, last_error = NULL
             WHERE id = ?2 AND status = 'ready'",
            params![now, operation_id],
        )
        .map_err(|error| format!("Could not start backup item: {error}"))?;
    if changed == 0 {
        return Err("Backup item is no longer ready.".into());
    }
    Ok(())
}

pub fn current_file_evidence(
    app: &AppHandle,
    workspace_id: &str,
    relative_path: &str,
) -> Result<CurrentFileEvidence, String> {
    let connection = open_database(app)?;
    connection
        .query_row(
            "SELECT
                local_present, local_hash, local_size,
                remote_present, remote_id, remote_checksum_type, remote_checksum
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
                    remote_checksum_type: row.get(5)?,
                    remote_checksum: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read current file evidence: {error}"))?
        .ok_or_else(|| "File is no longer present in the AtrisBridge journal.".to_string())
}

pub fn complete_operation(
    app: &AppHandle,
    operation: &BackupOperation,
    observation: &RemoteFileObservation,
) -> Result<(), String> {
    let remote_id = observation
        .remote_id
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a remote file ID after upload.".to_string())?;
    let checksum_type = observation
        .checksum_type
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a checksum type after upload.".to_string())?;
    let checksum = observation
        .checksum
        .as_deref()
        .ok_or_else(|| "Google Drive did not return a checksum after upload.".to_string())?;
    if observation.size != operation.local_size {
        return Err("Remote file size did not match the planned local file after upload.".into());
    }

    let mut connection = open_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start backup completion transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();

    let file_changed = transaction
        .execute(
            "UPDATE file_entries
             SET remote_present = 1,
                 remote_id = ?1,
                 remote_size = ?2,
                 remote_modified_at = ?3,
                 remote_checksum_type = ?4,
                 remote_checksum = ?5,
                 last_synced_hash = ?6,
                 last_synced_remote_checksum_type = ?4,
                 last_synced_remote_checksum = ?5,
                 last_synced_at = ?7,
                 last_remote_seen_at = ?7,
                 state = 'synced',
                 tombstone = 0
             WHERE workspace_id = ?8
               AND relative_path = ?9
               AND local_present = 1
               AND local_hash = ?6",
            params![
                remote_id,
                to_i64(observation.size, "remote file size")?,
                observation.modified_at,
                checksum_type,
                checksum,
                operation.local_hash,
                now,
                operation.workspace_id,
                operation.relative_path,
            ],
        )
        .map_err(|error| format!("Could not establish synchronized baseline: {error}"))?;
    if file_changed == 0 {
        return Err(
            "Local journal changed while the upload was completing; baseline was not accepted."
                .into(),
        );
    }

    let item_changed = transaction
        .execute(
            "UPDATE backup_plan_items
             SET status = 'completed',
                 completed_remote_id = ?1,
                 completed_remote_checksum_type = ?2,
                 completed_remote_checksum = ?3,
                 updated_at = ?4,
                 last_error = NULL
             WHERE id = ?5 AND status = 'running'",
            params![remote_id, checksum_type, checksum, now, operation.id],
        )
        .map_err(|error| format!("Could not complete backup item journal: {error}"))?;
    if item_changed == 0 {
        return Err("Backup item journal changed before completion.".into());
    }

    transaction
        .commit()
        .map_err(|error| format!("Could not commit backup completion: {error}"))
}

pub fn fail_operation(app: &AppHandle, operation_id: &str, error: &str) -> Result<(), String> {
    let connection = open_database(app)?;
    let now = Utc::now().to_rfc3339();
    let message = truncate_error(error);
    connection
        .execute(
            "UPDATE backup_plan_items
             SET status = 'failed', last_error = ?1, updated_at = ?2
             WHERE id = ?3 AND status IN ('ready', 'running')",
            params![message, now, operation_id],
        )
        .map_err(|db_error| format!("Could not record failed backup item: {db_error}"))?;
    Ok(())
}

pub fn finalize_plan(app: &AppHandle, plan_id: &str) -> Result<BackupExecutionReport, String> {
    let mut connection = open_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start backup finalization: {error}"))?;

    let (completed, failed, remaining, uploaded_bytes): (i64, i64, i64, i64) = transaction
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN ('ready', 'running') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'completed' THEN local_size ELSE 0 END), 0)
             FROM backup_plan_items
             WHERE plan_id = ?1 AND action IN ('create', 'update')",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| format!("Could not summarize backup execution: {error}"))?;

    if remaining != 0 {
        return Err("Backup plan still contains unfinished operations.".into());
    }

    let blocked: i64 = transaction
        .query_row(
            "SELECT blocked_count FROM backup_plans WHERE id = ?1",
            params![plan_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not read backup block count: {error}"))?
        .ok_or_else(|| "Backup plan was not found during finalization.".to_string())?;

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
            "UPDATE backup_plans
             SET status = ?1,
                 completed_count = ?2,
                 failed_count = ?3,
                 completed_at = ?4,
                 last_error = CASE WHEN ?3 > 0 THEN 'One or more upload items failed.' ELSE NULL END
             WHERE id = ?5 AND status = 'running'",
            params![status, completed, failed, finished_at, plan_id],
        )
        .map_err(|error| format!("Could not finalize backup plan: {error}"))?;

    transaction
        .commit()
        .map_err(|error| format!("Could not commit backup finalization: {error}"))?;

    Ok(BackupExecutionReport {
        plan_id: plan_id.to_string(),
        status: status.to_string(),
        completed_count: from_i64(completed, "completed backup count")?,
        failed_count: from_i64(failed, "failed backup count")?,
        uploaded_bytes: from_i64(uploaded_bytes, "uploaded byte count")?,
        finished_at,
    })
}

fn create_plan_with_connection(
    connection: &mut Connection,
    workspace_id: &str,
) -> Result<BackupPlan, String> {
    let metadata = connection
        .query_row(
            "SELECT
                w.sync_mode, w.last_scan_at,
                b.provider_id, b.remote_path, b.last_inventory_at
             FROM workspaces w
             LEFT JOIN workspace_remote_bindings b ON b.workspace_id = w.id
             WHERE w.id = ?1",
            params![workspace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read backup planning metadata: {error}"))?
        .ok_or_else(|| "Workspace was not found.".to_string())?;

    if metadata.0 != "backup" {
        return Err("Only backup-mode workspaces can prepare Phase 4 upload plans.".into());
    }
    let local_scan_at = metadata
        .1
        .ok_or_else(|| "Run a fresh local scan before preparing a backup plan.".to_string())?;
    let provider_id = metadata.2.ok_or_else(|| {
        "Bind this workspace to Google Drive before preparing a backup plan.".to_string()
    })?;
    let remote_path = metadata
        .3
        .ok_or_else(|| "Workspace remote path is missing.".to_string())?;
    let remote_inventory_at = metadata.4.ok_or_else(|| {
        "Read a fresh remote inventory before preparing a backup plan.".to_string()
    })?;

    let running: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM backup_plans WHERE workspace_id = ?1 AND status = 'running'",
            params![workspace_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect active backup plans: {error}"))?;
    if running > 0 {
        return Err("A backup execution is already running for this workspace.".into());
    }

    let evidence = load_file_evidence(connection, workspace_id)?;
    let plan_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut upload_count = 0_u64;
    let mut upload_bytes = 0_u64;
    let mut blocked_count = 0_u64;

    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start backup planning transaction: {error}"))?;
    transaction
        .execute(
            "UPDATE backup_plan_items
             SET status = 'cancelled', updated_at = ?1
             WHERE plan_id IN (
                SELECT id FROM backup_plans WHERE workspace_id = ?2 AND status = 'ready'
             ) AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous backup items: {error}"))?;
    transaction
        .execute(
            "UPDATE backup_plans
             SET status = 'cancelled', completed_at = ?1
             WHERE workspace_id = ?2 AND status = 'ready'",
            params![now, workspace_id],
        )
        .map_err(|error| format!("Could not retire previous backup plan: {error}"))?;

    transaction
        .execute(
            "INSERT INTO backup_plans (
                id, workspace_id, provider_id, remote_path, status,
                created_at, local_scan_at, remote_inventory_at,
                upload_count, upload_bytes, blocked_count
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
        .map_err(|error| format!("Could not create backup plan: {error}"))?;

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO backup_plan_items (
                    id, plan_id, workspace_id, relative_path, action, status,
                    local_hash, local_size, expected_remote_present,
                    expected_remote_id, expected_remote_checksum_type, expected_remote_checksum,
                    block_reason, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            )
            .map_err(|error| format!("Could not prepare backup plan item insert: {error}"))?;

        for file in evidence {
            match classify(&file) {
                PlanDecision::Skip => {}
                PlanDecision::Upload(action) => {
                    let local_hash = file.local_hash.as_deref().ok_or_else(|| {
                        format!("Local fingerprint disappeared for {}.", file.relative_path)
                    })?;
                    let local_size = file.local_size.ok_or_else(|| {
                        format!("Local size disappeared for {}.", file.relative_path)
                    })?;
                    upload_count = upload_count.checked_add(1).ok_or_else(|| {
                        "Backup plan file count exceeded supported range.".to_string()
                    })?;
                    upload_bytes = upload_bytes.checked_add(local_size).ok_or_else(|| {
                        "Backup plan byte count exceeded supported range.".to_string()
                    })?;
                    statement
                        .execute(params![
                            Uuid::new_v4().to_string(),
                            plan_id,
                            workspace_id,
                            file.relative_path,
                            action,
                            "ready",
                            local_hash,
                            to_i64(local_size, "planned file size")?,
                            if file.remote_present { 1 } else { 0 },
                            file.remote_id,
                            file.remote_checksum_type,
                            file.remote_checksum,
                            Option::<String>::None,
                            now,
                        ])
                        .map_err(|error| format!("Could not add upload to backup plan: {error}"))?;
                }
                PlanDecision::Blocked(reason) => {
                    blocked_count = blocked_count.checked_add(1).ok_or_else(|| {
                        "Backup block count exceeded supported range.".to_string()
                    })?;
                    statement
                        .execute(params![
                            Uuid::new_v4().to_string(),
                            plan_id,
                            workspace_id,
                            file.relative_path,
                            "blocked",
                            "blocked",
                            file.local_hash,
                            file.local_size
                                .map(|size| to_i64(size, "blocked file size"))
                                .transpose()?,
                            if file.remote_present { 1 } else { 0 },
                            file.remote_id,
                            file.remote_checksum_type,
                            file.remote_checksum,
                            reason,
                            now,
                        ])
                        .map_err(|error| {
                            format!("Could not add safety block to backup plan: {error}")
                        })?;
                }
            }
        }
    }

    let final_status = if upload_count > 0 {
        "ready"
    } else if blocked_count > 0 {
        "partial"
    } else {
        "completed"
    };
    let completed_at = if upload_count == 0 {
        Some(now.as_str())
    } else {
        None
    };
    transaction
        .execute(
            "UPDATE backup_plans
             SET status = ?1, upload_count = ?2, upload_bytes = ?3,
                 blocked_count = ?4, completed_at = ?5
             WHERE id = ?6",
            params![
                final_status,
                to_i64(upload_count, "planned upload count")?,
                to_i64(upload_bytes, "planned upload bytes")?,
                to_i64(blocked_count, "blocked file count")?,
                completed_at,
                plan_id,
            ],
        )
        .map_err(|error| format!("Could not finalize backup plan summary: {error}"))?;

    transaction
        .commit()
        .map_err(|error| format!("Could not commit backup plan: {error}"))?;
    load_plan_with_connection(connection, &plan_id)
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
                remote_present, remote_id, remote_checksum_type, remote_checksum,
                last_synced_hash, last_synced_remote_checksum_type, last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1
             ORDER BY relative_path ASC",
        )
        .map_err(|error| format!("Could not prepare backup evidence query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id], |row| {
            Ok(FileEvidence {
                relative_path: row.get(0)?,
                local_present: row.get::<_, i64>(1)? != 0,
                local_size: optional_u64(row, 2)?,
                local_hash: row.get(3)?,
                remote_present: row.get::<_, i64>(4)? != 0,
                remote_id: row.get(5)?,
                remote_checksum_type: row.get(6)?,
                remote_checksum: row.get(7)?,
                last_synced_hash: row.get(8)?,
                last_synced_remote_checksum_type: row.get(9)?,
                last_synced_remote_checksum: row.get(10)?,
            })
        })
        .map_err(|error| format!("Could not query backup evidence: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read backup evidence: {error}"))
}

fn classify(file: &FileEvidence) -> PlanDecision {
    if file.relative_path.contains('\\') {
        return PlanDecision::Blocked(
            "Backslash characters in file names are not mapped to cloud paths in Phase 4.".into(),
        );
    }

    if !file.local_present {
        if file.remote_present && file.last_synced_hash.is_some() {
            return PlanDecision::Blocked(
                "Local file was deleted. Phase 4 never deletes or changes the remote copy.".into(),
            );
        }
        if file.remote_present {
            return PlanDecision::Blocked(
                "Remote-only file is preserved; backup mode does not download or replace it."
                    .into(),
            );
        }
        return PlanDecision::Skip;
    }

    let Some(local_hash) = file.local_hash.as_deref() else {
        return PlanDecision::Blocked("Local fingerprint is missing; rescan is required.".into());
    };
    if file.local_size.is_none() {
        return PlanDecision::Blocked("Local file size is missing; rescan is required.".into());
    }

    let Some(last_synced_hash) = file.last_synced_hash.as_deref() else {
        if file.remote_present {
            return PlanDecision::Blocked(
                "A remote file already exists at this path without an AtrisBridge baseline.".into(),
            );
        }
        return PlanDecision::Upload("create");
    };

    let local_matches_baseline = local_hash == last_synced_hash;
    let remote_matches_baseline = remote_matches_baseline(file);
    match (local_matches_baseline, remote_matches_baseline) {
        (true, true) => PlanDecision::Skip,
        (false, true) if file.remote_id.is_some() => PlanDecision::Upload("update"),
        (false, true) => PlanDecision::Blocked(
            "Remote object identity is missing; AtrisBridge will not update it without complete evidence."
                .into(),
        ),
        (true, false) => PlanDecision::Blocked(
            "Remote file changed or disappeared since the last synchronized baseline.".into(),
        ),
        (false, false) => PlanDecision::Blocked(
            "Local and remote evidence both changed since the last synchronized baseline.".into(),
        ),
    }
}

fn remote_matches_baseline(file: &FileEvidence) -> bool {
    if !file.remote_present {
        return false;
    }
    match (
        file.remote_checksum_type.as_deref(),
        file.remote_checksum.as_deref(),
        file.last_synced_remote_checksum_type.as_deref(),
        file.last_synced_remote_checksum.as_deref(),
    ) {
        (Some(current_type), Some(current), Some(baseline_type), Some(baseline)) => {
            current_type == baseline_type && current == baseline
        }
        _ => false,
    }
}

fn load_plan_with_connection(connection: &Connection, plan_id: &str) -> Result<BackupPlan, String> {
    let plan = connection
        .query_row(
            "SELECT
                id, workspace_id, provider_id, remote_path, status,
                created_at, local_scan_at, remote_inventory_at,
                upload_count, upload_bytes, blocked_count,
                completed_count, failed_count, completed_at
             FROM backup_plans
             WHERE id = ?1",
            params![plan_id],
            |row| {
                Ok(BackupPlan {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    provider_id: row.get(2)?,
                    remote_path: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                    local_scan_at: row.get(6)?,
                    remote_inventory_at: row.get(7)?,
                    upload_count: required_u64(row, 8)?,
                    upload_bytes: required_u64(row, 9)?,
                    blocked_count: required_u64(row, 10)?,
                    completed_count: required_u64(row, 11)?,
                    failed_count: required_u64(row, 12)?,
                    completed_at: row.get(13)?,
                    preview_truncated: false,
                    items: Vec::new(),
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read backup plan: {error}"))?
        .ok_or_else(|| "Backup plan was not found.".to_string())?;

    let mut statement = connection
        .prepare(
            "SELECT id, relative_path, action, status, local_size, block_reason, last_error
             FROM backup_plan_items
             WHERE plan_id = ?1
             ORDER BY CASE action WHEN 'blocked' THEN 0 ELSE 1 END, relative_path ASC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare backup plan preview: {error}"))?;
    let rows = statement
        .query_map(params![plan_id, (PREVIEW_LIMIT + 1) as i64], |row| {
            Ok(BackupPlanItem {
                id: row.get(0)?,
                relative_path: row.get(1)?,
                action: row.get(2)?,
                status: row.get(3)?,
                size: optional_u64(row, 4)?,
                block_reason: row.get(5)?,
                last_error: row.get(6)?,
            })
        })
        .map_err(|error| format!("Could not query backup plan preview: {error}"))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read backup plan preview: {error}"))?;
    let preview_truncated = items.len() > PREVIEW_LIMIT;
    if preview_truncated {
        items.truncate(PREVIEW_LIMIT);
    }

    Ok(BackupPlan {
        preview_truncated,
        items,
        ..plan
    })
}

fn operation_from_row(row: &Row<'_>) -> rusqlite::Result<BackupOperation> {
    Ok(BackupOperation {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        relative_path: row.get(2)?,
        action: row.get(3)?,
        local_hash: row
            .get::<_, Option<String>>(4)?
            .ok_or(rusqlite::Error::InvalidQuery)?,
        local_size: required_u64(row, 5)?,
        expected_remote_present: row.get::<_, i64>(6)? != 0,
        expected_remote_id: row.get(7)?,
        expected_remote_checksum_type: row.get(8)?,
        expected_remote_checksum: row.get(9)?,
    })
}

fn optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
        })
        .transpose()
}

fn required_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn from_i64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Stored {label} is invalid."))
}

fn truncate_error(error: &str) -> String {
    error.chars().take(1000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    fn connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("foreign keys");
        database::migrate_schema(&mut connection).expect("schema");
        connection
    }

    fn seed_context(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO workspaces (
                    id, name, local_path, sync_mode, created_at, last_scan_at
                 ) VALUES (
                    'workspace-1', 'Project', '/tmp/project', 'backup',
                    '2026-08-11T07:00:00Z', '2026-08-11T07:10:00Z'
                 )",
                [],
            )
            .expect("workspace");
        connection
            .execute(
                "INSERT INTO provider_connections (
                    id, provider_type, display_name, created_at, last_verified_at
                 ) VALUES (
                    'provider-1', 'google_drive', 'Google Drive',
                    '2026-08-11T07:00:00Z', '2026-08-11T07:00:00Z'
                 )",
                [],
            )
            .expect("provider");
        connection
            .execute(
                "INSERT INTO workspace_remote_bindings (
                    workspace_id, provider_id, remote_path, created_at, last_inventory_at
                 ) VALUES (
                    'workspace-1', 'provider-1', 'AtrisBridge/Project',
                    '2026-08-11T07:00:00Z', '2026-08-11T07:11:00Z'
                 )",
                [],
            )
            .expect("binding");
    }

    fn insert_file(
        connection: &Connection,
        path: &str,
        local_hash: &str,
        remote_present: bool,
        remote_checksum: Option<&str>,
        baseline_hash: Option<&str>,
        baseline_checksum: Option<&str>,
    ) {
        connection
            .execute(
                "INSERT INTO file_entries (
                    workspace_id, relative_path, local_present, local_size, local_hash,
                    remote_present, remote_id, remote_size, remote_checksum_type, remote_checksum,
                    last_synced_hash, last_synced_remote_checksum_type, last_synced_remote_checksum,
                    state, first_seen_at, last_seen_at
                 ) VALUES (
                    'workspace-1', ?1, 1, 12, ?2,
                    ?3, CASE WHEN ?3 = 1 THEN 'remote-id' ELSE NULL END,
                    CASE WHEN ?3 = 1 THEN 12 ELSE NULL END,
                    CASE WHEN ?4 IS NOT NULL THEN 'MD5' ELSE NULL END, ?4,
                    ?5, CASE WHEN ?6 IS NOT NULL THEN 'MD5' ELSE NULL END, ?6,
                    'local_only', '2026-08-11T07:00:00Z', '2026-08-11T07:10:00Z'
                 )",
                params![
                    path,
                    local_hash,
                    if remote_present { 1 } else { 0 },
                    remote_checksum,
                    baseline_hash,
                    baseline_checksum,
                ],
            )
            .expect("file");
    }

    #[test]
    fn new_local_file_becomes_create_upload() {
        let mut connection = connection();
        seed_context(&connection);
        insert_file(
            &connection,
            "src/main.rs",
            "local-a",
            false,
            None,
            None,
            None,
        );

        let plan = create_plan_with_connection(&mut connection, "workspace-1").expect("plan");

        assert_eq!(plan.status, "ready");
        assert_eq!(plan.upload_count, 1);
        assert_eq!(plan.blocked_count, 0);
        assert_eq!(plan.items[0].action, "create");
    }

    #[test]
    fn unverified_remote_overlap_is_blocked() {
        let mut connection = connection();
        seed_context(&connection);
        insert_file(
            &connection,
            "src/main.rs",
            "local-a",
            true,
            Some("remote-a"),
            None,
            None,
        );

        let plan = create_plan_with_connection(&mut connection, "workspace-1").expect("plan");

        assert_eq!(plan.upload_count, 0);
        assert_eq!(plan.blocked_count, 1);
        assert_eq!(plan.items[0].action, "blocked");
    }

    #[test]
    fn local_change_with_unchanged_remote_becomes_update() {
        let mut connection = connection();
        seed_context(&connection);
        insert_file(
            &connection,
            "src/main.rs",
            "local-new",
            true,
            Some("remote-baseline"),
            Some("local-old"),
            Some("remote-baseline"),
        );

        let plan = create_plan_with_connection(&mut connection, "workspace-1").expect("plan");

        assert_eq!(plan.upload_count, 1);
        assert_eq!(plan.items[0].action, "update");
    }

    #[test]
    fn changed_remote_is_never_overwritten_by_planner() {
        let mut connection = connection();
        seed_context(&connection);
        insert_file(
            &connection,
            "src/main.rs",
            "local-new",
            true,
            Some("remote-new"),
            Some("local-old"),
            Some("remote-old"),
        );

        let plan = create_plan_with_connection(&mut connection, "workspace-1").expect("plan");

        assert_eq!(plan.upload_count, 0);
        assert_eq!(plan.blocked_count, 1);
        assert!(plan.items[0]
            .block_reason
            .as_deref()
            .unwrap_or_default()
            .contains("both changed"));
    }

    #[test]
    fn missing_remote_identity_blocks_update_even_when_checksum_matches() {
        let mut connection = connection();
        seed_context(&connection);
        insert_file(
            &connection,
            "src/main.rs",
            "local-new",
            true,
            Some("remote-baseline"),
            Some("local-old"),
            Some("remote-baseline"),
        );
        connection
            .execute(
                "UPDATE file_entries SET remote_id = NULL
                 WHERE workspace_id = 'workspace-1' AND relative_path = 'src/main.rs'",
                [],
            )
            .expect("remove remote id");

        let plan = create_plan_with_connection(&mut connection, "workspace-1").expect("plan");

        assert_eq!(plan.upload_count, 0);
        assert_eq!(plan.blocked_count, 1);
        assert!(plan.items[0]
            .block_reason
            .as_deref()
            .unwrap_or_default()
            .contains("identity is missing"));
    }
}
