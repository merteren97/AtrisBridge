use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    ai_artifact_crypto,
    ai_gateway::{self, AiAuditEvent, AiSession},
    ai_git,
    ai_workspace::{
        self, classify_relative_path, ensure_absent, ensure_ai_path_allowed, file_matches,
        normalize_relative_path, regular_file_evidence, resolve_target_path, AiPathClass,
    },
    database::open_database,
    durable_fs,
    services::workspace as workspace_service,
    storage::find_workspace,
    workspace_coordinator::{
        WorkspaceMutationCoordinator, WorkspaceMutationLease, WorkspaceOperationKind,
    },
};

const MAX_CHANGESET_ITEMS: usize = 100;
const MAX_WRITE_BYTES_PER_FILE: usize = 2 * 1024 * 1024;
const MAX_CHANGESET_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_CHANGESET_LIMIT: u32 = 50;
const MAX_CHANGESET_LIMIT: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiChangeOperation {
    Create,
    Replace,
    Delete,
    Move,
}

impl AiChangeOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Delete => "delete",
            Self::Move => "move",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "create" => Ok(Self::Create),
            "replace" => Ok(Self::Replace),
            "delete" => Ok(Self::Delete),
            "move" => Ok(Self::Move),
            _ => Err("Stored AI changeset operation is invalid.".into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChangeRequest {
    pub operation: AiChangeOperation,
    pub relative_path: String,
    pub destination_path: Option<String>,
    pub expected_before_hash: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChangesetItem {
    pub id: String,
    pub operation: String,
    pub relative_path: String,
    pub destination_path: Option<String>,
    pub before_hash: Option<String>,
    pub before_size: Option<u64>,
    pub after_hash: Option<String>,
    pub after_size: Option<u64>,
    pub sensitive: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChangeset {
    pub id: String,
    pub session_id: String,
    pub client_id: String,
    pub workspace_id: String,
    pub status: String,
    pub failure_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub applied_at: Option<String>,
    pub rolled_back_at: Option<String>,
    pub items: Vec<AiChangesetItem>,
}

#[derive(Debug, Clone)]
struct StoredChangesetItem {
    public: AiChangesetItem,
    payload_path: Option<String>,
    recovery_path: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredChangeset {
    public: AiChangeset,
    items: Vec<StoredChangesetItem>,
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let connection = open_changeset_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_changesets
             WHERE status IN ('applying', 'undoing')
             ORDER BY created_at ASC",
        )
        .map_err(|error| format!("Could not prepare interrupted AI changeset query: {error}"))?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query interrupted AI changesets: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read interrupted AI changeset IDs: {error}"))?;
    drop(statement);
    drop(connection);

    for id in ids {
        let stored = load_stored_changeset(app, &id)?;
        match rollback_files(app, &stored) {
            Ok(()) => {
                mark_changeset_rolled_back(app, &id, Some("startup_recovery"))?;
                let _ = rescan_primary_if_direct(app, &stored);
            }
            Err(error) => {
                mark_changeset_recovery_required(app, &id, "startup_recovery_failed")?;
                eprintln!("AtrisBridge AI changeset {id} requires manual recovery: {error}");
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn prepare_ai_changeset(
    app: AppHandle,
    session_id: String,
    changes: Vec<AiChangeRequest>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiChangeset, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.edit")?;
    let result = prepare_changeset_inner(&app, &session, changes, &coordinator);
    record_changeset_audit(
        &app,
        &session,
        "workspace.edit",
        "workspace.changeset_prepare",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn execute_ai_changeset(
    app: AppHandle,
    session_id: String,
    changeset_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiChangeset, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.edit")?;
    let result = execute_changeset_inner(&app, &session, &changeset_id, &coordinator);
    record_changeset_audit(
        &app,
        &session,
        "workspace.edit",
        "workspace.changeset_execute",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn undo_ai_changeset(
    app: AppHandle,
    session_id: String,
    changeset_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiChangeset, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.edit")?;
    let result = undo_changeset_inner(&app, &session, &changeset_id, &coordinator);
    record_changeset_audit(
        &app,
        &session,
        "workspace.edit",
        "workspace.changeset_undo",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn get_ai_changeset(app: AppHandle, changeset_id: String) -> Result<AiChangeset, String> {
    Ok(load_stored_changeset(&app, &changeset_id)?.public)
}

#[tauri::command]
pub fn list_ai_changesets(
    app: AppHandle,
    workspace_id: String,
    limit: Option<u32>,
) -> Result<Vec<AiChangeset>, String> {
    find_workspace(&app, &workspace_id)?;
    let limit = limit
        .unwrap_or(DEFAULT_CHANGESET_LIMIT)
        .clamp(1, MAX_CHANGESET_LIMIT);
    let connection = open_changeset_database(&app)?;
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_changesets
             WHERE workspace_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare AI changeset query: {error}"))?;
    let ids = statement
        .query_map(params![workspace_id, i64::from(limit)], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("Could not query AI changesets: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI changeset IDs: {error}"))?;
    drop(statement);
    drop(connection);
    ids.into_iter()
        .map(|id| load_stored_changeset(app, &id).map(|stored| stored.public))
        .collect()
}

fn prepare_changeset_inner(
    app: &AppHandle,
    session: &AiSession,
    changes: Vec<AiChangeRequest>,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<AiChangeset, String> {
    if changes.is_empty() || changes.len() > MAX_CHANGESET_ITEMS {
        return Err(format!(
            "AI changesets must contain between 1 and {MAX_CHANGESET_ITEMS} operations."
        ));
    }
    let _lease = acquire_plan(coordinator, session)?;
    let root = ai_git::session_workspace_root(app, session, coordinator)?;
    let id = Uuid::new_v4().to_string();
    let payload_root = changeset_root(app, &id)?.join("payload");
    fs::create_dir_all(&payload_root)
        .map_err(|error| format!("Could not create AI changeset staging directory: {error}"))?;

    let mut staged = Vec::<StoredChangesetItem>::new();
    let mut touched = HashSet::<String>::new();
    let mut total_payload_bytes = 0usize;

    let prepare_result = (|| -> Result<(), String> {
        for change in changes {
            let relative_path = normalize_relative_path(&change.relative_path)?;
            let class = ensure_ai_path_allowed(&root, &relative_path)?;
            authorize_sensitive_write(app, session, class)?;
            if !touched.insert(relative_path.clone()) {
                return Err("AI changeset contains overlapping source paths.".into());
            }

            let item_id = Uuid::new_v4().to_string();
            let source = ai_workspace::resolve_future_target_path(&root, &relative_path)?;
            let current = regular_file_evidence(&source)?;
            let mut destination_path = None;
            let mut payload_path = None;
            let mut before_hash = None;
            let mut before_size = None;
            let mut after_hash = None;
            let mut after_size = None;
            let mut sensitive = class == AiPathClass::Sensitive;

            match change.operation {
                AiChangeOperation::Create => {
                    if change.expected_before_hash.is_some() {
                        return Err("Create operations cannot specify expectedBeforeHash.".into());
                    }
                    let content = change
                        .content
                        .ok_or_else(|| "Create operation requires UTF-8 content.".to_string())?;
                    validate_payload_size(content.as_bytes().len())?;
                    total_payload_bytes = total_payload_bytes.saturating_add(content.len());
                    let staged_path = payload_root.join(format!("{item_id}.payload"));
                    write_payload_artifact(&staged_path, content.as_bytes(), sensitive, &item_id)?;
                    payload_path = Some(staged_path.to_string_lossy().to_string());
                    after_size = Some(u64::try_from(content.len()).unwrap_or(u64::MAX));
                    after_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
                }
                AiChangeOperation::Replace => {
                    let (size, hash) = current.ok_or_else(|| {
                        "Replace operation requires an existing regular file.".to_string()
                    })?;
                    require_expected_hash(change.expected_before_hash.as_deref(), &hash)?;
                    let content = change
                        .content
                        .ok_or_else(|| "Replace operation requires UTF-8 content.".to_string())?;
                    validate_payload_size(content.as_bytes().len())?;
                    total_payload_bytes = total_payload_bytes.saturating_add(content.len());
                    let staged_path = payload_root.join(format!("{item_id}.payload"));
                    write_payload_artifact(&staged_path, content.as_bytes(), sensitive, &item_id)?;
                    payload_path = Some(staged_path.to_string_lossy().to_string());
                    before_size = Some(size);
                    before_hash = Some(hash);
                    after_size = Some(u64::try_from(content.len()).unwrap_or(u64::MAX));
                    after_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
                }
                AiChangeOperation::Delete => {
                    ai_gateway::authorize_session(app, &session.id, "workspace.delete")?;
                    if change.content.is_some() || change.destination_path.is_some() {
                        return Err(
                            "Delete operations do not accept content or destinationPath.".into(),
                        );
                    }
                    let (size, hash) = current.ok_or_else(|| {
                        "Delete operation requires an existing regular file.".to_string()
                    })?;
                    require_expected_hash(change.expected_before_hash.as_deref(), &hash)?;
                    before_size = Some(size);
                    before_hash = Some(hash);
                }
                AiChangeOperation::Move => {
                    if change.content.is_some() {
                        return Err("Move operations do not accept content.".into());
                    }
                    let (size, hash) = current.ok_or_else(|| {
                        "Move operation requires an existing regular file.".to_string()
                    })?;
                    require_expected_hash(change.expected_before_hash.as_deref(), &hash)?;
                    let destination =
                        normalize_relative_path(change.destination_path.as_deref().ok_or_else(
                            || "Move operation requires destinationPath.".to_string(),
                        )?)?;
                    let destination_class = ensure_ai_path_allowed(&root, &destination)?;
                    authorize_sensitive_write(app, session, destination_class)?;
                    if !touched.insert(destination.clone()) {
                        return Err("AI changeset contains overlapping destination paths.".into());
                    }
                    let destination_target =
                        ai_workspace::resolve_future_target_path(&root, &destination)?;
                    sensitive |= destination_class == AiPathClass::Sensitive;
                    ensure_absent(&destination_target, "AI move destination")?;
                    destination_path = Some(destination);
                    before_size = Some(size);
                    before_hash = Some(hash.clone());
                    after_size = Some(size);
                    after_hash = Some(hash);
                }
            }

            if sensitive {
                if let Some(size) = before_size {
                    if size > ai_artifact_crypto::MAX_SENSITIVE_ARTIFACT_BYTES {
                        return Err(format!(
                            "Sensitive AI recovery artifacts are limited to {} bytes.",
                            ai_artifact_crypto::MAX_SENSITIVE_ARTIFACT_BYTES
                        ));
                    }
                }
            }

            if total_payload_bytes > MAX_CHANGESET_PAYLOAD_BYTES {
                return Err(format!(
                    "AI changeset payload exceeds the {} byte limit.",
                    MAX_CHANGESET_PAYLOAD_BYTES
                ));
            }

            staged.push(StoredChangesetItem {
                public: AiChangesetItem {
                    id: item_id,
                    operation: change.operation.as_str().to_string(),
                    relative_path,
                    destination_path,
                    before_hash,
                    before_size,
                    after_hash,
                    after_size,
                    sensitive,
                    status: "pending".into(),
                },
                payload_path,
                recovery_path: None,
            });
        }
        Ok(())
    })();

    if let Err(error) = prepare_result {
        let _ = fs::remove_dir_all(changeset_root(app, &id)?);
        return Err(error);
    }

    let now = Utc::now().to_rfc3339();
    let mut connection = open_changeset_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start AI changeset journal transaction: {error}"))?;
    transaction
        .execute(
            "INSERT INTO ai_changesets (
                id, session_id, client_id, workspace_id, status,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'prepared', ?5, ?5)",
            params![id, session.id, session.client_id, session.workspace_id, now],
        )
        .map_err(|error| format!("Could not journal AI changeset: {error}"))?;
    for item in &staged {
        transaction
            .execute(
                "INSERT INTO ai_changeset_items (
                    id, changeset_id, operation, relative_path, destination_path,
                    before_hash, before_size, after_hash, after_size,
                    sensitive, status, payload_path, recovery_path
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, NULL)",
                params![
                    item.public.id,
                    id,
                    item.public.operation,
                    item.public.relative_path,
                    item.public.destination_path,
                    item.public.before_hash,
                    item.public.before_size.map(to_i64).transpose()?,
                    item.public.after_hash,
                    item.public.after_size.map(to_i64).transpose()?,
                    if item.public.sensitive { 1 } else { 0 },
                    item.payload_path,
                ],
            )
            .map_err(|error| format!("Could not journal AI changeset item: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("Could not commit AI changeset journal: {error}"))?;
    load_stored_changeset(app, &id).map(|stored| stored.public)
}

fn execute_changeset_inner(
    app: &AppHandle,
    session: &AiSession,
    changeset_id: &str,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<AiChangeset, String> {
    let stored = load_stored_changeset(app, changeset_id)?;
    ensure_session_owns_changeset(session, &stored)?;
    if stored.public.status != "prepared" {
        return Err(format!(
            "AI changeset cannot execute from status '{}'.",
            stored.public.status
        ));
    }
    authorize_changeset_capabilities(app, session, &stored)?;
    let _lease = acquire_edit(coordinator, session)?;
    let root = ai_git::session_workspace_root(app, session, coordinator)?;
    preflight_changeset(&root, &stored)?;
    prepare_recovery_snapshots(app, &root, &stored)?;
    set_changeset_status(app, changeset_id, "applying", None)?;

    let mut latest = load_stored_changeset(app, changeset_id)?;
    for item in latest.items.clone() {
        if let Err(error) = apply_item(&root, &item) {
            latest = load_stored_changeset(app, changeset_id)?;
            return match rollback_files(app, &latest) {
                Ok(()) => {
                    mark_changeset_rolled_back(
                        app,
                        changeset_id,
                        Some("apply_failed_rolled_back"),
                    )?;
                    let _ = rescan_primary_if_direct(app, &latest);
                    Err(format!("{error} Applied changes were rolled back safely."))
                }
                Err(rollback_error) => {
                    mark_changeset_recovery_required(
                        app,
                        changeset_id,
                        "apply_and_rollback_failed",
                    )?;
                    Err(format!(
                        "{error} Automatic rollback was not safe: {rollback_error}"
                    ))
                }
            };
        }
        mark_item_status(app, &item.public.id, "applied")?;
    }

    let now = Utc::now().to_rfc3339();
    let connection = open_changeset_database(app)?;
    connection
        .execute(
            "UPDATE ai_changesets
             SET status = 'applied', applied_at = ?1, updated_at = ?1, failure_code = NULL
             WHERE id = ?2 AND status = 'applying'",
            params![now, changeset_id],
        )
        .map_err(|error| format!("Could not finalize AI changeset journal: {error}"))?;
    rescan_primary_if_direct(app, &stored)?;
    load_stored_changeset(app, changeset_id).map(|stored| stored.public)
}

fn undo_changeset_inner(
    app: &AppHandle,
    session: &AiSession,
    changeset_id: &str,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<AiChangeset, String> {
    let stored = load_stored_changeset(app, changeset_id)?;
    ensure_session_owns_changeset(session, &stored)?;
    if stored.public.status != "applied" {
        return Err(format!(
            "Only an applied AI changeset can be undone; current status is '{}'.",
            stored.public.status
        ));
    }
    authorize_changeset_capabilities(app, session, &stored)?;
    let _lease = acquire_edit(coordinator, session)?;
    set_changeset_status(app, changeset_id, "undoing", None)?;
    let latest = load_stored_changeset(app, changeset_id)?;
    match rollback_files(app, &latest) {
        Ok(()) => {
            mark_changeset_rolled_back(app, changeset_id, Some("user_undo"))?;
            rescan_primary_if_direct(app, &stored)?;
            load_stored_changeset(app, changeset_id).map(|stored| stored.public)
        }
        Err(error) => {
            mark_changeset_recovery_required(app, changeset_id, "undo_failed")?;
            Err(format!(
                "AI changeset could not be undone safely: {error} Manual recovery is required."
            ))
        }
    }
}

fn rescan_primary_if_direct(app: &AppHandle, stored: &StoredChangeset) -> Result<(), String> {
    if ai_git::changeset_targets_primary_workspace(app, &stored.public.session_id)? {
        workspace_service::scan(app, &stored.public.workspace_id)?;
    }
    Ok(())
}

fn preflight_changeset(root: &Path, stored: &StoredChangeset) -> Result<(), String> {
    for item in &stored.items {
        let source = if item.public.operation == "create" {
            ai_workspace::resolve_future_target_path(root, &item.public.relative_path)?
        } else {
            resolve_target_path(root, &item.public.relative_path, false)?
        };
        match AiChangeOperation::parse(&item.public.operation)? {
            AiChangeOperation::Create => {
                ensure_absent(&source, "AI create destination")?;
                validate_payload(item)?;
            }
            AiChangeOperation::Replace | AiChangeOperation::Delete => {
                ensure_before_evidence(&source, item)?;
                if item.public.operation == "replace" {
                    validate_payload(item)?;
                }
            }
            AiChangeOperation::Move => {
                ensure_before_evidence(&source, item)?;
                let destination = item
                    .public
                    .destination_path
                    .as_deref()
                    .ok_or_else(|| "Stored move destination is missing.".to_string())?;
                let destination_target =
                    ai_workspace::resolve_future_target_path(root, destination)?;
                ensure_absent(&destination_target, "AI move destination")?;
            }
        }
    }
    Ok(())
}

fn prepare_recovery_snapshots(
    app: &AppHandle,
    root: &Path,
    stored: &StoredChangeset,
) -> Result<(), String> {
    let recovery_root = changeset_root(app, &stored.public.id)?.join("recovery");
    fs::create_dir_all(&recovery_root)
        .map_err(|error| format!("Could not create AI recovery directory: {error}"))?;
    for item in &stored.items {
        if matches!(
            AiChangeOperation::parse(&item.public.operation)?,
            AiChangeOperation::Create
        ) {
            continue;
        }

        if item.recovery_path.is_some() {
            validate_recovery_snapshot(app, stored, item)?;
            continue;
        }

        let source = resolve_target_path(root, &item.public.relative_path, false)?;
        ensure_before_evidence(&source, item)?;
        let recovery = recovery_root.join(format!("{}.bak", item.public.id));
        let before_size = item
            .public
            .before_size
            .ok_or_else(|| "Stored before-size evidence is missing.".to_string())?;
        let before_hash = item
            .public
            .before_hash
            .as_deref()
            .ok_or_else(|| "Stored before-hash evidence is missing.".to_string())?;

        match fs::symlink_metadata(&recovery) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(
                        "Unjournaled AI recovery artifact is not a regular AtrisBridge-owned file."
                            .into(),
                    );
                }
                if !artifact_matches(&recovery, item, "recovery", before_size, before_hash)? {
                    return Err(
                        "Unjournaled AI recovery artifact does not match current workspace evidence."
                            .into(),
                    );
                }
                durable_fs::remove_regular_file(&recovery).map_err(|error| {
                    format!("Could not clean stale AI recovery artifact before retry: {error}")
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect AI recovery snapshot before retry: {error}"
                ));
            }
        }

        let snapshot_result = (|| -> Result<(), String> {
            if item.public.sensitive {
                if before_size > ai_artifact_crypto::MAX_SENSITIVE_ARTIFACT_BYTES {
                    return Err(format!(
                        "Sensitive AI recovery artifacts are limited to {} bytes.",
                        ai_artifact_crypto::MAX_SENSITIVE_ARTIFACT_BYTES
                    ));
                }
                let bytes = fs::read(&source).map_err(|error| {
                    format!("Could not read sensitive AI recovery source: {error}")
                })?;
                ai_artifact_crypto::write_encrypted_artifact(
                    &recovery,
                    &bytes,
                    &artifact_aad(&item.public.id, "recovery"),
                )?;
            } else {
                durable_fs::copy_new_file(&source, &recovery)
                    .map_err(|error| format!("Could not create AI recovery snapshot: {error}"))?;
            }
            if !artifact_matches(&recovery, item, "recovery", before_size, before_hash)? {
                return Err("AI recovery snapshot failed fingerprint verification.".into());
            }
            Ok(())
        })();

        if let Err(error) = snapshot_result {
            let _ = durable_fs::remove_regular_file(&recovery);
            return Err(error);
        }

        let connection = match open_changeset_database(app) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = durable_fs::remove_regular_file(&recovery);
                return Err(error);
            }
        };
        if let Err(error) = connection.execute(
            "UPDATE ai_changeset_items SET recovery_path = ?1 WHERE id = ?2",
            params![recovery.to_string_lossy().to_string(), item.public.id],
        ) {
            let _ = durable_fs::remove_regular_file(&recovery);
            return Err(format!("Could not journal AI recovery snapshot: {error}"));
        }
    }
    Ok(())
}

fn apply_item(root: &Path, item: &StoredChangesetItem) -> Result<(), String> {
    let operation = AiChangeOperation::parse(&item.public.operation)?;
    let source = resolve_target_path(root, &item.public.relative_path, true)?;
    match operation {
        AiChangeOperation::Create => place_payload(&source, item, false),
        AiChangeOperation::Replace => place_payload(&source, item, true),
        AiChangeOperation::Delete => {
            ensure_before_evidence(&source, item)?;
            fs::remove_file(&source)
                .map_err(|error| format!("Could not delete AI changeset target: {error}"))?;
            ensure_absent(&source, "deleted AI changeset target")
        }
        AiChangeOperation::Move => {
            ensure_before_evidence(&source, item)?;
            let destination = item
                .public
                .destination_path
                .as_deref()
                .ok_or_else(|| "Stored move destination is missing.".to_string())?;
            let destination_target = resolve_target_path(root, destination, true)?;
            ensure_absent(&destination_target, "AI move destination")?;
            fs::rename(&source, &destination_target)
                .map_err(|error| format!("Could not move AI changeset target: {error}"))?;
            ensure_absent(&source, "AI move source")?;
            let after_size = item
                .public
                .after_size
                .ok_or_else(|| "Stored move size evidence is missing.".to_string())?;
            let after_hash = item
                .public
                .after_hash
                .as_deref()
                .ok_or_else(|| "Stored move hash evidence is missing.".to_string())?;
            if !file_matches(&destination_target, after_size, after_hash)? {
                return Err(
                    "Moved AI changeset target failed final fingerprint verification.".into(),
                );
            }
            Ok(())
        }
    }
}

fn place_payload(target: &Path, item: &StoredChangesetItem, replace: bool) -> Result<(), String> {
    if replace {
        ensure_before_evidence(target, item)?;
    } else {
        ensure_absent(target, "AI create destination")?;
    }
    let payload = validate_payload(item)?;
    let stage = same_directory_stage_path(target, &item.public.id)?;
    ensure_absent(&stage, "AI staging artifact")?;
    if item.public.sensitive {
        let plaintext = ai_artifact_crypto::read_encrypted_artifact(
            &payload,
            &artifact_aad(&item.public.id, "payload"),
        )?;
        write_owned_file(&stage, &plaintext)?;
    } else {
        durable_fs::copy_new_file(&payload, &stage)
            .map_err(|error| format!("Could not durably stage AI changeset payload: {error}"))?;
    }
    let after_size = item
        .public
        .after_size
        .ok_or_else(|| "Stored after-size evidence is missing.".to_string())?;
    let after_hash = item
        .public
        .after_hash
        .as_deref()
        .ok_or_else(|| "Stored after-hash evidence is missing.".to_string())?;
    if !file_matches(&stage, after_size, after_hash)? {
        let _ = durable_fs::remove_regular_file(&stage);
        return Err("AI staging artifact failed fingerprint verification.".into());
    }
    if replace {
        fs::remove_file(target)
            .map_err(|error| format!("Could not replace existing workspace file: {error}"))?;
    }
    if let Err(error) = fs::rename(&stage, target) {
        let _ = durable_fs::remove_regular_file(&stage);
        return Err(format!("Could not place AI changeset payload: {error}"));
    }
    if !file_matches(target, after_size, after_hash)? {
        return Err("AI changeset target failed final fingerprint verification.".into());
    }
    Ok(())
}

fn rollback_files(app: &AppHandle, stored: &StoredChangeset) -> Result<(), String> {
    let root = ai_git::changeset_workspace_root(
        app,
        &stored.public.workspace_id,
        &stored.public.session_id,
    )?;
    for item in stored.items.iter().rev() {
        rollback_item(app, &root, stored, item)?;
        mark_item_status(app, &item.public.id, "rolled_back")?;
    }
    Ok(())
}

fn rollback_item(
    app: &AppHandle,
    root: &Path,
    stored: &StoredChangeset,
    item: &StoredChangesetItem,
) -> Result<(), String> {
    let operation = AiChangeOperation::parse(&item.public.operation)?;
    let source = resolve_target_path(root, &item.public.relative_path, true)?;
    match operation {
        AiChangeOperation::Create => {
            cleanup_stage_artifact(&source, item)?;
            if regular_file_evidence(&source)?.is_none() {
                return Ok(());
            }
            ensure_after_evidence(&source, item)?;
            fs::remove_file(&source)
                .map_err(|error| format!("Could not roll back AI-created file: {error}"))
        }
        AiChangeOperation::Replace => {
            cleanup_stage_artifact(&source, item)?;
            if matches_before(&source, item)? {
                return Ok(());
            }
            if regular_file_evidence(&source)?.is_none() {
                return restore_recovery_snapshot(app, stored, item, false);
            }
            ensure_after_evidence(&source, item)?;
            restore_recovery_snapshot(app, stored, item, true)
        }
        AiChangeOperation::Delete => {
            if matches_before(&source, item)? {
                return Ok(());
            }
            ensure_absent(&source, "AI delete rollback destination")?;
            restore_recovery_snapshot(app, stored, item, false)
        }
        AiChangeOperation::Move => {
            let destination = item
                .public
                .destination_path
                .as_deref()
                .ok_or_else(|| "Stored move destination is missing.".to_string())?;
            let destination_target = resolve_target_path(root, destination, true)?;
            if matches_before(&source, item)? {
                ensure_absent(&destination_target, "AI move rollback destination")?;
                return Ok(());
            }
            ensure_absent(&source, "AI move rollback source")?;
            ensure_after_evidence(&destination_target, item)?;
            restore_recovery_snapshot(app, stored, item, false)?;
            fs::remove_file(&destination_target).map_err(|error| {
                format!("Could not remove rolled-back AI move destination: {error}")
            })
        }
    }
}

fn cleanup_stage_artifact(target: &Path, item: &StoredChangesetItem) -> Result<(), String> {
    let stage = same_directory_stage_path(target, &item.public.id)?;
    let Some(after_size) = item.public.after_size else {
        return Ok(());
    };
    let Some(after_hash) = item.public.after_hash.as_deref() else {
        return Ok(());
    };
    if file_matches(&stage, after_size, after_hash)? {
        durable_fs::remove_regular_file(&stage)
            .map_err(|error| format!("Could not clean interrupted AI staging artifact: {error}"))?;
    }
    Ok(())
}

fn restore_recovery_snapshot(
    app: &AppHandle,
    stored: &StoredChangeset,
    item: &StoredChangesetItem,
    replace_target: bool,
) -> Result<(), String> {
    let recovery = validate_recovery_snapshot(app, stored, item)?;
    let root = ai_git::changeset_workspace_root(
        app,
        &stored.public.workspace_id,
        &stored.public.session_id,
    )?;
    let target = resolve_target_path(&root, &item.public.relative_path, true)?;
    let stage = same_directory_stage_path(&target, &format!("{}-rollback", item.public.id))?;
    ensure_absent(&stage, "AI rollback staging artifact")?;
    if item.public.sensitive {
        let plaintext = ai_artifact_crypto::read_encrypted_artifact(
            &recovery,
            &artifact_aad(&item.public.id, "recovery"),
        )?;
        write_owned_file(&stage, &plaintext)?;
    } else {
        durable_fs::copy_new_file(&recovery, &stage)
            .map_err(|error| format!("Could not durably stage AI rollback snapshot: {error}"))?;
    }
    let before_size = item
        .public
        .before_size
        .ok_or_else(|| "Stored before-size evidence is missing.".to_string())?;
    let before_hash = item
        .public
        .before_hash
        .as_deref()
        .ok_or_else(|| "Stored before-hash evidence is missing.".to_string())?;
    if !file_matches(&stage, before_size, before_hash)? {
        let _ = durable_fs::remove_regular_file(&stage);
        return Err("AI rollback staging artifact failed fingerprint verification.".into());
    }
    if replace_target {
        ensure_after_evidence(&target, item)?;
        fs::remove_file(&target)
            .map_err(|error| format!("Could not remove AI rollback target: {error}"))?;
    } else {
        ensure_absent(&target, "AI rollback target")?;
    }
    if let Err(error) = fs::rename(&stage, &target) {
        let _ = durable_fs::remove_regular_file(&stage);
        return Err(format!("Could not restore AI rollback target: {error}"));
    }
    if !file_matches(&target, before_size, before_hash)? {
        return Err("Restored AI rollback target failed final fingerprint verification.".into());
    }
    Ok(())
}

fn validate_recovery_snapshot(
    app: &AppHandle,
    stored: &StoredChangeset,
    item: &StoredChangesetItem,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(
        item.recovery_path
            .as_deref()
            .ok_or_else(|| "AI recovery snapshot metadata is missing.".to_string())?,
    );
    let changeset_root = changeset_root(app, &stored.public.id)?;
    let canonical_changeset_root = changeset_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve AI changeset root: {error}"))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("Could not inspect AI recovery snapshot: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("AI recovery snapshot is not a regular AtrisBridge-owned file.".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Could not resolve AI recovery snapshot: {error}"))?;
    if !canonical.starts_with(&canonical_changeset_root) {
        return Err("AI recovery snapshot escaped the AtrisBridge changeset root.".into());
    }
    let before_size = item
        .public
        .before_size
        .ok_or_else(|| "Stored before-size evidence is missing.".to_string())?;
    let before_hash = item
        .public
        .before_hash
        .as_deref()
        .ok_or_else(|| "Stored before-hash evidence is missing.".to_string())?;
    if item.public.sensitive && !ai_artifact_crypto::is_encrypted_artifact(&canonical)? {
        return Err("Sensitive AI recovery snapshot is not encrypted at rest.".into());
    }
    if !artifact_matches(&canonical, item, "recovery", before_size, before_hash)? {
        return Err("AI recovery snapshot no longer matches recorded evidence.".into());
    }
    Ok(canonical)
}

fn validate_payload(item: &StoredChangesetItem) -> Result<PathBuf, String> {
    let path = PathBuf::from(
        item.payload_path
            .as_deref()
            .ok_or_else(|| "AI changeset payload metadata is missing.".to_string())?,
    );
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("Could not inspect AI changeset payload: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("AI changeset payload is not a regular AtrisBridge-owned file.".into());
    }
    let size = item
        .public
        .after_size
        .ok_or_else(|| "Stored after-size evidence is missing.".to_string())?;
    let hash = item
        .public
        .after_hash
        .as_deref()
        .ok_or_else(|| "Stored after-hash evidence is missing.".to_string())?;
    if item.public.sensitive && !ai_artifact_crypto::is_encrypted_artifact(&path)? {
        return Err("Sensitive AI changeset payload is not encrypted at rest.".into());
    }
    if !artifact_matches(&path, item, "payload", size, hash)? {
        return Err("AI changeset payload no longer matches recorded evidence.".into());
    }
    Ok(path)
}

fn ensure_before_evidence(path: &Path, item: &StoredChangesetItem) -> Result<(), String> {
    let expected_size = item
        .public
        .before_size
        .ok_or_else(|| "Stored before-size evidence is missing.".to_string())?;
    let expected_hash = item
        .public
        .before_hash
        .as_deref()
        .ok_or_else(|| "Stored before-hash evidence is missing.".to_string())?;
    if !file_matches(path, expected_size, expected_hash)? {
        return Err(format!(
            "Workspace file '{}' changed after the AI changeset was prepared. The stale changeset was blocked.",
            item.public.relative_path
        ));
    }
    Ok(())
}

fn ensure_after_evidence(path: &Path, item: &StoredChangesetItem) -> Result<(), String> {
    let expected_size = item
        .public
        .after_size
        .ok_or_else(|| "Stored after-size evidence is missing.".to_string())?;
    let expected_hash = item
        .public
        .after_hash
        .as_deref()
        .ok_or_else(|| "Stored after-hash evidence is missing.".to_string())?;
    if !file_matches(path, expected_size, expected_hash)? {
        return Err(format!(
            "Workspace file '{}' changed after the AI changeset applied. Automatic rollback is unsafe.",
            item.public.relative_path
        ));
    }
    Ok(())
}

fn matches_before(path: &Path, item: &StoredChangesetItem) -> Result<bool, String> {
    let Some(size) = item.public.before_size else {
        return Ok(false);
    };
    let Some(hash) = item.public.before_hash.as_deref() else {
        return Ok(false);
    };
    file_matches(path, size, hash)
}

fn authorize_changeset_capabilities(
    app: &AppHandle,
    session: &AiSession,
    stored: &StoredChangeset,
) -> Result<(), String> {
    ai_gateway::authorize_session(app, &session.id, "workspace.edit")?;
    if stored
        .items
        .iter()
        .any(|item| item.public.operation == "delete")
    {
        ai_gateway::authorize_session(app, &session.id, "workspace.delete")?;
    }
    if stored.items.iter().any(|item| item.public.sensitive) {
        ai_gateway::authorize_session(app, &session.id, "sensitive.write")?;
    }
    for item in &stored.items {
        if let Some(destination) = item.public.destination_path.as_deref() {
            if classify_relative_path(destination)? == AiPathClass::Sensitive {
                ai_gateway::authorize_session(app, &session.id, "sensitive.write")?;
            }
        }
    }
    Ok(())
}

fn authorize_sensitive_write(
    app: &AppHandle,
    session: &AiSession,
    class: AiPathClass,
) -> Result<(), String> {
    if class == AiPathClass::Sensitive {
        ai_gateway::authorize_session(app, &session.id, "sensitive.write")?;
    }
    Ok(())
}

fn ensure_session_owns_changeset(
    session: &AiSession,
    stored: &StoredChangeset,
) -> Result<(), String> {
    if stored.public.session_id != session.id
        || stored.public.client_id != session.client_id
        || stored.public.workspace_id != session.workspace_id
    {
        return Err("AI changeset does not belong to this active AI session.".into());
    }
    Ok(())
}

fn require_expected_hash(expected: Option<&str>, actual: &str) -> Result<(), String> {
    let expected = expected.ok_or_else(|| {
        "Replace, delete, and move operations require expectedBeforeHash from a prior AI read/stat."
            .to_string()
    })?;
    if expected != actual {
        return Err("expectedBeforeHash does not match current workspace evidence.".into());
    }
    Ok(())
}

fn validate_payload_size(size: usize) -> Result<(), String> {
    if size > MAX_WRITE_BYTES_PER_FILE {
        return Err(format!(
            "AI writes are limited to {MAX_WRITE_BYTES_PER_FILE} bytes per file."
        ));
    }
    Ok(())
}

fn artifact_aad(item_id: &str, purpose: &str) -> Vec<u8> {
    format!("AtrisBridge AI artifact v1\n{item_id}\n{purpose}").into_bytes()
}

fn write_payload_artifact(
    path: &Path,
    bytes: &[u8],
    sensitive: bool,
    item_id: &str,
) -> Result<(), String> {
    if sensitive {
        ai_artifact_crypto::write_encrypted_artifact(path, bytes, &artifact_aad(item_id, "payload"))
    } else {
        write_owned_file(path, bytes)
    }
}

fn artifact_matches(
    path: &Path,
    item: &StoredChangesetItem,
    purpose: &str,
    expected_size: u64,
    expected_hash: &str,
) -> Result<bool, String> {
    if !item.public.sensitive {
        return file_matches(path, expected_size, expected_hash);
    }
    let plaintext =
        ai_artifact_crypto::read_encrypted_artifact(path, &artifact_aad(&item.public.id, purpose))?;
    Ok(
        u64::try_from(plaintext.len()).unwrap_or(u64::MAX) == expected_size
            && blake3::hash(&plaintext).to_hex().as_str() == expected_hash,
    )
}

fn write_owned_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = File::create(path)
        .map_err(|error| format!("Could not create AI changeset payload: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("Could not write AI changeset payload: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Could not flush AI changeset payload: {error}"))
}

fn same_directory_stage_path(target: &Path, item_id: &str) -> Result<PathBuf, String> {
    let safe = item_id
        .chars()
        .filter(|value| value.is_ascii_hexdigit())
        .collect::<String>();
    if safe.len() < 16 {
        return Err("AI changeset item identifier is invalid.".into());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "AI changeset target has no parent directory.".to_string())?;
    Ok(parent.join(format!(".atrisbridge-ai-{safe}.part")))
}

fn changeset_root(app: &AppHandle, changeset_id: &str) -> Result<PathBuf, String> {
    let safe = changeset_id
        .chars()
        .filter(|value| value.is_ascii_hexdigit())
        .collect::<String>();
    if safe.len() < 16 {
        return Err("AI changeset identifier is invalid.".into());
    }
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))?
        .join("ai-changesets")
        .join(safe);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create AtrisBridge AI changeset directory: {error}"))?;
    Ok(root)
}

fn acquire_plan(
    coordinator: &WorkspaceMutationCoordinator,
    session: &AiSession,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(
            &session.workspace_id,
            &format!("ai:{}", session.client_id),
            WorkspaceOperationKind::Plan,
        )
        .map_err(|error| error.to_string())
}

fn acquire_edit(
    coordinator: &WorkspaceMutationCoordinator,
    session: &AiSession,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(
            &session.workspace_id,
            &format!("ai:{}", session.client_id),
            WorkspaceOperationKind::AiEdit,
        )
        .map_err(|error| error.to_string())
}

fn record_changeset_audit<T>(
    app: &AppHandle,
    session: &AiSession,
    capability: &str,
    tool_name: &str,
    started: Instant,
    result: &Result<T, String>,
) -> Result<(), String> {
    ai_gateway::record_audit(
        app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some(capability),
            tool_name,
            outcome: if result.is_ok() { "success" } else { "failed" },
            duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            operation_id: None,
            detail_code: Some(if result.is_ok() {
                "ok"
            } else {
                "operation_failed"
            }),
        },
    )
}

fn open_changeset_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ai_changesets (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                client_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN (
                    'prepared', 'applying', 'applied', 'undoing', 'rolled_back', 'recovery_required'
                )),
                failure_code TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                applied_at TEXT,
                rolled_back_at TEXT,
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE RESTRICT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_changesets_workspace_created
                ON ai_changesets(workspace_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_ai_changesets_session_created
                ON ai_changesets(session_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS ai_changeset_items (
                id TEXT PRIMARY KEY,
                changeset_id TEXT NOT NULL,
                operation TEXT NOT NULL CHECK(operation IN ('create', 'replace', 'delete', 'move')),
                relative_path TEXT NOT NULL,
                destination_path TEXT,
                before_hash TEXT,
                before_size INTEGER CHECK(before_size IS NULL OR before_size >= 0),
                after_hash TEXT,
                after_size INTEGER CHECK(after_size IS NULL OR after_size >= 0),
                sensitive INTEGER NOT NULL DEFAULT 0 CHECK(sensitive IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('pending', 'applied', 'rolled_back')),
                payload_path TEXT,
                recovery_path TEXT,
                FOREIGN KEY(changeset_id) REFERENCES ai_changesets(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_changeset_items_changeset
                ON ai_changeset_items(changeset_id, id);",
        )
        .map_err(|error| format!("Could not initialize AI changeset metadata: {error}"))
}

fn load_stored_changeset(app: &AppHandle, id: &str) -> Result<StoredChangeset, String> {
    let connection = open_changeset_database(app)?;
    let header = connection
        .query_row(
            "SELECT id, session_id, client_id, workspace_id, status, failure_code,
                    created_at, updated_at, applied_at, rolled_back_at
             FROM ai_changesets WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read AI changeset: {error}"))?
        .ok_or_else(|| "AI changeset was not found.".to_string())?;

    let mut statement = connection
        .prepare(
            "SELECT id, operation, relative_path, destination_path,
                    before_hash, before_size, after_hash, after_size,
                    sensitive, status, payload_path, recovery_path
             FROM ai_changeset_items
             WHERE changeset_id = ?1
             ORDER BY rowid ASC",
        )
        .map_err(|error| format!("Could not prepare AI changeset item query: {error}"))?;
    let items = statement
        .query_map(params![id], stored_item_from_row)
        .map_err(|error| format!("Could not query AI changeset items: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI changeset items: {error}"))?;
    let public_items = items.iter().map(|item| item.public.clone()).collect();
    Ok(StoredChangeset {
        public: AiChangeset {
            id: header.0,
            session_id: header.1,
            client_id: header.2,
            workspace_id: header.3,
            status: header.4,
            failure_code: header.5,
            created_at: header.6,
            updated_at: header.7,
            applied_at: header.8,
            rolled_back_at: header.9,
            items: public_items,
        },
        items,
    })
}

fn stored_item_from_row(row: &Row<'_>) -> rusqlite::Result<StoredChangesetItem> {
    let before_size = row
        .get::<_, Option<i64>>(5)?
        .and_then(|value| u64::try_from(value).ok());
    let after_size = row
        .get::<_, Option<i64>>(7)?
        .and_then(|value| u64::try_from(value).ok());
    Ok(StoredChangesetItem {
        public: AiChangesetItem {
            id: row.get(0)?,
            operation: row.get(1)?,
            relative_path: row.get(2)?,
            destination_path: row.get(3)?,
            before_hash: row.get(4)?,
            before_size,
            after_hash: row.get(6)?,
            after_size,
            sensitive: row.get::<_, i64>(8)? != 0,
            status: row.get(9)?,
        },
        payload_path: row.get(10)?,
        recovery_path: row.get(11)?,
    })
}

fn set_changeset_status(
    app: &AppHandle,
    id: &str,
    status: &str,
    failure_code: Option<&str>,
) -> Result<(), String> {
    let connection = open_changeset_database(app)?;
    connection
        .execute(
            "UPDATE ai_changesets
             SET status = ?1, failure_code = ?2, updated_at = ?3
             WHERE id = ?4",
            params![status, failure_code, Utc::now().to_rfc3339(), id],
        )
        .map_err(|error| format!("Could not update AI changeset status: {error}"))?;
    Ok(())
}

fn mark_changeset_rolled_back(
    app: &AppHandle,
    id: &str,
    failure_code: Option<&str>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let connection = open_changeset_database(app)?;
    connection
        .execute(
            "UPDATE ai_changesets
             SET status = 'rolled_back', failure_code = ?1,
                 rolled_back_at = ?2, updated_at = ?2
             WHERE id = ?3",
            params![failure_code, now, id],
        )
        .map_err(|error| format!("Could not mark AI changeset rolled back: {error}"))?;
    connection
        .execute(
            "UPDATE ai_changeset_items SET status = 'rolled_back' WHERE changeset_id = ?1",
            params![id],
        )
        .map_err(|error| format!("Could not mark AI changeset items rolled back: {error}"))?;
    Ok(())
}

fn mark_changeset_recovery_required(
    app: &AppHandle,
    id: &str,
    failure_code: &str,
) -> Result<(), String> {
    set_changeset_status(app, id, "recovery_required", Some(failure_code))
}

fn mark_item_status(app: &AppHandle, item_id: &str, status: &str) -> Result<(), String> {
    let connection = open_changeset_database(app)?;
    connection
        .execute(
            "UPDATE ai_changeset_items SET status = ?1 WHERE id = ?2",
            params![status, item_id],
        )
        .map_err(|error| format!("Could not update AI changeset item status: {error}"))?;
    Ok(())
}

fn to_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "AI changeset size exceeds SQLite INTEGER range.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changeset_operation_round_trip_is_stable() {
        for operation in [
            AiChangeOperation::Create,
            AiChangeOperation::Replace,
            AiChangeOperation::Delete,
            AiChangeOperation::Move,
        ] {
            assert_eq!(
                AiChangeOperation::parse(operation.as_str()).expect("parse"),
                operation
            );
        }
    }

    #[test]
    fn payload_limits_are_bounded() {
        assert!(validate_payload_size(MAX_WRITE_BYTES_PER_FILE).is_ok());
        assert!(validate_payload_size(MAX_WRITE_BYTES_PER_FILE + 1).is_err());
    }

    #[test]
    fn stale_hash_preconditions_are_required() {
        assert!(require_expected_hash(None, "abc").is_err());
        assert!(require_expected_hash(Some("def"), "abc").is_err());
        assert!(require_expected_hash(Some("abc"), "abc").is_ok());
    }
}
