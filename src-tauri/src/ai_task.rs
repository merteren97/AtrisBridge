use std::{
    collections::HashMap,
    fs,
    io::ErrorKind,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    ai_artifact_crypto, ai_command,
    ai_gateway::{self, AiAuditEvent, AiSession},
    database::open_database,
    workspace_coordinator::WorkspaceMutationCoordinator,
};

const DEFAULT_TASK_LIMIT: u32 = 50;
const MAX_TASK_LIMIT: u32 = 200;
const TASK_EXPIRY_MARGIN_SECONDS: i64 = 60;
const TASK_QUEUE_WAIT_SECONDS: i64 = 2 * 60;
const TASK_RESULT_RETENTION_DAYS: i64 = 7;

#[derive(Clone, Default)]
pub struct AiTaskManager {
    cancellations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTaskRecord {
    pub id: String,
    pub session_id: String,
    pub client_id: String,
    pub workspace_id: String,
    pub kind: String,
    pub profile_id: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancel_requested: bool,
    pub result_available: bool,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTaskResult {
    pub task: AiTaskRecord,
    pub command: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskArtifact {
    command: Option<Value>,
    error: Option<String>,
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let connection = open_task_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE ai_tasks
             SET status = 'interrupted', completed_at = ?1, error_code = 'desktop_restart'
             WHERE status IN ('queued', 'running')",
            params![now],
        )
        .map_err(|error| format!("Could not interrupt stale AI tasks: {error}"))?;
    fs::create_dir_all(task_result_root(app)?)
        .map_err(|error| format!("Could not initialize AI task result storage: {error}"))?;
    cleanup_terminal_task_artifacts(app, &connection)?;
    cleanup_expired_task_results(app, &connection)?;
    Ok(())
}

#[tauri::command]
pub fn start_ai_command_task(
    app: AppHandle,
    session_id: String,
    profile_id: String,
    manager: State<'_, AiTaskManager>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiTaskRecord, String> {
    let session = authorize_task_session(&app, &session_id)?;
    ai_command::validate_profile_id(&profile_id)?;
    ensure_session_covers_task(&session, ai_command::MAX_TIMEOUT_SECONDS)?;

    let task_id = format!("abt_{}", Uuid::new_v4());
    let created_at = Utc::now().to_rfc3339();
    let connection = open_task_database(&app)?;
    cleanup_expired_task_results(&app, &connection)?;
    connection
        .execute(
            "INSERT INTO ai_tasks (
                id, session_id, client_id, workspace_id, kind, profile_id,
                status, created_at, cancel_requested, result_available
             ) VALUES (?1, ?2, ?3, ?4, 'command', ?5, 'queued', ?6, 0, 0)",
            params![
                task_id,
                session.id,
                session.client_id,
                session.workspace_id,
                profile_id,
                created_at
            ],
        )
        .map_err(|error| format!("Could not create AI command task: {error}"))?;

    let cancel = Arc::new(AtomicBool::new(false));
    if let Err(error) = manager.insert_cancellation(&task_id, cancel.clone()) {
        let _ = delete_unstarted_task(&connection, &task_id);
        return Err(error);
    }
    if let Err(error) = ai_gateway::record_audit(
        &app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some("command.execute"),
            tool_name: "task.command.start",
            outcome: "success",
            duration_ms: None,
            operation_id: Some(&task_id),
            detail_code: Some(&profile_id),
        },
    ) {
        let _ = manager.remove_cancellation(&task_id);
        let _ = delete_unstarted_task(&connection, &task_id);
        return Err(error);
    }

    let worker_manager = manager.inner().clone();
    let worker_coordinator = coordinator.inner().clone();
    let worker_app = app.clone();
    let worker_task_id = task_id.clone();
    let worker_session_id = session_id.clone();
    let worker_profile_id = profile_id.clone();
    if let Err(error) = thread::Builder::new()
        .name(format!("atrisbridge-ai-task-{}", short_task_id(&task_id)))
        .spawn(move || {
            run_command_task_worker(
                worker_app,
                worker_task_id,
                worker_session_id,
                worker_profile_id,
                worker_manager,
                worker_coordinator,
                cancel,
            );
        })
    {
        manager.remove_cancellation(&task_id)?;
        connection
            .execute(
                "UPDATE ai_tasks
                 SET status = 'failed', completed_at = ?1, error_code = 'worker_spawn_failed'
                 WHERE id = ?2 AND status = 'queued'",
                params![Utc::now().to_rfc3339(), task_id],
            )
            .map_err(|db_error| format!("Could not mark failed AI task: {db_error}"))?;
        let _ = ai_gateway::record_audit(
            &app,
            AiAuditEvent {
                session_id: Some(&session.id),
                client_id: &session.client_id,
                workspace_id: &session.workspace_id,
                capability: Some("command.execute"),
                tool_name: "task.command.complete",
                outcome: "failed",
                duration_ms: None,
                operation_id: Some(&task_id),
                detail_code: Some("worker_spawn_failed"),
            },
        );
        return Err(format!("Could not start AI command task worker: {error}"));
    }

    load_task_for_session(&connection, &session.id, &task_id)?
        .ok_or_else(|| "AI task disappeared after creation.".to_string())
}

#[tauri::command]
pub fn get_ai_task(
    app: AppHandle,
    session_id: String,
    task_id: String,
) -> Result<AiTaskRecord, String> {
    let session = authorize_task_session(&app, &session_id)?;
    let connection = open_task_database(&app)?;
    load_task_for_session(&connection, &session.id, &task_id)?
        .ok_or_else(|| "AI task was not found for this session.".to_string())
}

#[tauri::command]
pub fn list_ai_tasks(
    app: AppHandle,
    session_id: String,
    limit: Option<u32>,
) -> Result<Vec<AiTaskRecord>, String> {
    let session = authorize_task_session(&app, &session_id)?;
    let limit = limit.unwrap_or(DEFAULT_TASK_LIMIT).clamp(1, MAX_TASK_LIMIT);
    let connection = open_task_database(&app)?;
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_tasks
             WHERE session_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare AI task query: {error}"))?;
    let rows = statement
        .query_map(params![session.id, i64::from(limit)], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("Could not query AI tasks: {error}"))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI task IDs: {error}"))?;
    ids.into_iter()
        .map(|task_id| {
            load_task_for_session(&connection, &session.id, &task_id)?
                .ok_or_else(|| "AI task disappeared while listing session tasks.".to_string())
        })
        .collect()
}

#[tauri::command]
pub fn get_ai_task_result(
    app: AppHandle,
    session_id: String,
    task_id: String,
) -> Result<AiTaskResult, String> {
    let session = authorize_task_session(&app, &session_id)?;
    let connection = open_task_database(&app)?;
    cleanup_expired_task_results(&app, &connection)?;
    let task = load_task_for_session(&connection, &session.id, &task_id)?
        .ok_or_else(|| "AI task was not found for this session.".to_string())?;
    if !is_terminal_status(&task.status) {
        return Err("AI task has not reached a terminal state.".into());
    }
    if !task.result_available {
        return Ok(AiTaskResult {
            task,
            command: None,
            error: None,
        });
    }
    let artifact = read_task_artifact(&app, &task.id)?;
    Ok(AiTaskResult {
        task,
        command: artifact.command,
        error: artifact.error,
    })
}

#[tauri::command]
pub fn cancel_ai_task(
    app: AppHandle,
    session_id: String,
    task_id: String,
    manager: State<'_, AiTaskManager>,
) -> Result<AiTaskRecord, String> {
    let session = authorize_task_session(&app, &session_id)?;
    let connection = open_task_database(&app)?;
    let task = load_task_for_session(&connection, &session.id, &task_id)?
        .ok_or_else(|| "AI task was not found for this session.".to_string())?;
    if is_terminal_status(&task.status) {
        return Ok(task);
    }
    let cancel = manager
        .cancellation(&task.id)?
        .ok_or_else(|| "AI task is not owned by the current desktop process.".to_string())?;
    cancel.store(true, Ordering::SeqCst);
    connection
        .execute(
            "UPDATE ai_tasks SET cancel_requested = 1
             WHERE id = ?1 AND session_id = ?2 AND status IN ('queued', 'running')",
            params![task.id, session.id],
        )
        .map_err(|error| format!("Could not request AI task cancellation: {error}"))?;
    ai_gateway::record_audit(
        &app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some("command.execute"),
            tool_name: "task.cancel",
            outcome: "success",
            duration_ms: None,
            operation_id: Some(&task.id),
            detail_code: Some("cancel_requested"),
        },
    )?;
    load_task_for_session(&connection, &session.id, &task.id)?
        .ok_or_else(|| "AI task disappeared after cancellation request.".to_string())
}

impl AiTaskManager {
    fn insert_cancellation(&self, task_id: &str, signal: Arc<AtomicBool>) -> Result<(), String> {
        self.cancellations
            .lock()
            .map_err(|_| "AI task cancellation registry is unavailable.".to_string())?
            .insert(task_id.to_string(), signal);
        Ok(())
    }

    fn cancellation(&self, task_id: &str) -> Result<Option<Arc<AtomicBool>>, String> {
        Ok(self
            .cancellations
            .lock()
            .map_err(|_| "AI task cancellation registry is unavailable.".to_string())?
            .get(task_id)
            .cloned())
    }

    fn remove_cancellation(&self, task_id: &str) -> Result<(), String> {
        self.cancellations
            .lock()
            .map_err(|_| "AI task cancellation registry is unavailable.".to_string())?
            .remove(task_id);
        Ok(())
    }
}

fn run_command_task_worker(
    app: AppHandle,
    task_id: String,
    session_id: String,
    profile_id: String,
    manager: AiTaskManager,
    coordinator: WorkspaceMutationCoordinator,
    cancel: Arc<AtomicBool>,
) {
    let terminal = (|| -> Result<(), String> {
        let connection = open_task_database(&app)?;
        if cancel.load(Ordering::SeqCst) {
            finish_without_result(
                &connection,
                &task_id,
                "cancelled",
                Some("cancelled_before_start"),
            )?;
            return Ok(());
        }
        let changed = connection
            .execute(
                "UPDATE ai_tasks SET status = 'running', started_at = ?1
                 WHERE id = ?2 AND status = 'queued'",
                params![Utc::now().to_rfc3339(), task_id],
            )
            .map_err(|error| format!("Could not start AI task metadata: {error}"))?;
        if changed != 1 {
            return Err("AI task was no longer queued when its worker started.".into());
        }

        let result = ai_command::run_ai_command_cancellable(
            app.clone(),
            session_id.clone(),
            profile_id,
            coordinator,
            cancel.as_ref(),
        );
        match result {
            Ok(command) => {
                let artifact = TaskArtifact {
                    command: Some(
                        serde_json::to_value(&command)
                            .map_err(|error| format!("Could not encode AI task result: {error}"))?,
                    ),
                    error: None,
                };
                write_task_artifact(&app, &task_id, &artifact)?;
                let status = if command.cancelled {
                    "cancelled"
                } else {
                    "completed"
                };
                finish_with_result(&connection, &task_id, status, None)?;
            }
            Err(error) => {
                let cancelled = cancel.load(Ordering::SeqCst);
                let artifact = TaskArtifact {
                    command: None,
                    error: Some(error),
                };
                write_task_artifact(&app, &task_id, &artifact)?;
                finish_with_result(
                    &connection,
                    &task_id,
                    if cancelled { "cancelled" } else { "failed" },
                    Some(if cancelled {
                        "cancelled"
                    } else {
                        "command_failed"
                    }),
                )?;
            }
        }
        Ok(())
    })();

    if terminal.is_err() {
        let _ = remove_task_artifact(&app, &task_id);
        if let Ok(connection) = open_task_database(&app) {
            let _ = finish_without_result(
                &connection,
                &task_id,
                if cancel.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "failed"
                },
                Some("task_runtime_failed"),
            );
        }
    }
    let _ = manager.remove_cancellation(&task_id);

    if let Ok(connection) = open_task_database(&app) {
        if let Ok(Some(task)) = load_task(&connection, &task_id) {
            let outcome = match task.status.as_str() {
                "completed" => "success",
                "cancelled" => "cancelled",
                _ => "failed",
            };
            let _ = ai_gateway::record_audit(
                &app,
                AiAuditEvent {
                    session_id: Some(&task.session_id),
                    client_id: &task.client_id,
                    workspace_id: &task.workspace_id,
                    capability: Some("command.execute"),
                    tool_name: "task.command.complete",
                    outcome,
                    duration_ms: None,
                    operation_id: Some(&task.id),
                    detail_code: Some(&task.status),
                },
            );
        }
    }
}

fn authorize_task_session(app: &AppHandle, session_id: &str) -> Result<AiSession, String> {
    let session = ai_gateway::authorize_session(app, session_id, "command.execute")?;
    ai_gateway::authorize_session(app, session_id, "git.local")?;
    if session.mode != "isolated_worktree" {
        return Err("AI command tasks require an isolated-worktree session.".into());
    }
    Ok(session)
}

fn ensure_session_covers_task(session: &AiSession, timeout_seconds: u64) -> Result<(), String> {
    let expires_at = DateTime::parse_from_rfc3339(&session.expires_at)
        .map_err(|_| "AI session expiry timestamp is invalid.".to_string())?
        .with_timezone(&Utc);
    let required_until = Utc::now()
        + ChronoDuration::seconds(
            i64::try_from(timeout_seconds)
                .unwrap_or(i64::MAX / 2)
                .saturating_add(TASK_QUEUE_WAIT_SECONDS)
                .saturating_add(TASK_EXPIRY_MARGIN_SECONDS),
        );
    if expires_at <= required_until {
        return Err(
            "AI session expires too soon for this command task. Open a longer-lived session before starting the task."
                .into(),
        );
    }
    Ok(())
}

fn open_task_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ai_tasks (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                client_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                kind TEXT NOT NULL CHECK(kind IN ('command')),
                profile_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN (
                    'queued', 'running', 'completed', 'failed', 'cancelled', 'interrupted'
                )),
                created_at TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT,
                cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0, 1)),
                result_available INTEGER NOT NULL DEFAULT 0 CHECK(result_available IN (0, 1)),
                error_code TEXT,
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE CASCADE,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_tasks_session_created
                ON ai_tasks(session_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_ai_tasks_workspace_status
                ON ai_tasks(workspace_id, status, created_at DESC);",
        )
        .map_err(|error| format!("Could not initialize AI task metadata: {error}"))
}

fn load_task_for_session(
    connection: &Connection,
    session_id: &str,
    task_id: &str,
) -> Result<Option<AiTaskRecord>, String> {
    let task = load_task(connection, task_id)?;
    Ok(task.filter(|task| task.session_id == session_id))
}

fn load_task(connection: &Connection, task_id: &str) -> Result<Option<AiTaskRecord>, String> {
    connection
        .query_row(
            "SELECT id, session_id, client_id, workspace_id, kind, profile_id, status,
                    created_at, started_at, completed_at, cancel_requested,
                    result_available, error_code
             FROM ai_tasks WHERE id = ?1",
            params![task_id],
            |row| {
                Ok(AiTaskRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    client_id: row.get(2)?,
                    workspace_id: row.get(3)?,
                    kind: row.get(4)?,
                    profile_id: row.get(5)?,
                    status: row.get(6)?,
                    created_at: row.get(7)?,
                    started_at: row.get(8)?,
                    completed_at: row.get(9)?,
                    cancel_requested: row.get::<_, i64>(10)? != 0,
                    result_available: row.get::<_, i64>(11)? != 0,
                    error_code: row.get(12)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read AI task: {error}"))
}

fn finish_with_result(
    connection: &Connection,
    task_id: &str,
    status: &str,
    error_code: Option<&str>,
) -> Result<(), String> {
    connection
        .execute(
            "UPDATE ai_tasks
             SET status = ?1, completed_at = ?2, result_available = 1, error_code = ?3
             WHERE id = ?4 AND status IN ('queued', 'running')",
            params![status, Utc::now().to_rfc3339(), error_code, task_id],
        )
        .map_err(|error| format!("Could not finish AI task: {error}"))?;
    Ok(())
}

fn finish_without_result(
    connection: &Connection,
    task_id: &str,
    status: &str,
    error_code: Option<&str>,
) -> Result<(), String> {
    connection
        .execute(
            "UPDATE ai_tasks
             SET status = ?1, completed_at = ?2, result_available = 0, error_code = ?3
             WHERE id = ?4 AND status IN ('queued', 'running')",
            params![status, Utc::now().to_rfc3339(), error_code, task_id],
        )
        .map_err(|error| format!("Could not finish AI task metadata: {error}"))?;
    Ok(())
}

fn delete_unstarted_task(connection: &Connection, task_id: &str) -> Result<(), String> {
    connection
        .execute(
            "DELETE FROM ai_tasks WHERE id = ?1 AND status = 'queued'",
            params![task_id],
        )
        .map_err(|error| format!("Could not discard unstarted AI task: {error}"))?;
    Ok(())
}

fn remove_task_artifact(app: &AppHandle, task_id: &str) -> Result<(), String> {
    let path = task_artifact_path(app, task_id)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not remove AI task result artifact: {error}")),
    }
}

fn cleanup_terminal_task_artifacts(app: &AppHandle, connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_tasks
             WHERE status IN ('failed', 'cancelled', 'interrupted')
               AND result_available = 0",
        )
        .map_err(|error| format!("Could not prepare AI task artifact cleanup: {error}"))?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query AI task artifact cleanup: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI task artifact cleanup IDs: {error}"))?;
    drop(statement);
    for task_id in ids {
        if let Err(error) = remove_task_artifact(app, &task_id) {
            eprintln!("AtrisBridge could not clean stale AI task artifact {task_id}: {error}");
        }
    }
    Ok(())
}

fn cleanup_expired_task_results(app: &AppHandle, connection: &Connection) -> Result<(), String> {
    let cutoff = (Utc::now() - ChronoDuration::days(TASK_RESULT_RETENTION_DAYS)).to_rfc3339();
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_tasks
             WHERE result_available = 1
               AND completed_at IS NOT NULL
               AND completed_at < ?1",
        )
        .map_err(|error| format!("Could not prepare AI task result retention query: {error}"))?;
    let ids = statement
        .query_map(params![cutoff], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query expired AI task results: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read expired AI task result IDs: {error}"))?;
    drop(statement);
    for task_id in ids {
        match remove_task_artifact(app, &task_id) {
            Ok(()) => {
                connection
                    .execute(
                        "UPDATE ai_tasks SET result_available = 0 WHERE id = ?1",
                        params![task_id],
                    )
                    .map_err(|error| {
                        format!("Could not expire AI task result metadata: {error}")
                    })?;
            }
            Err(error) => {
                eprintln!("AtrisBridge could not expire AI task result {task_id}: {error}");
            }
        }
    }
    Ok(())
}

fn task_result_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("ai-tasks").join("results"))
        .map_err(|error| format!("Could not resolve AI task result directory: {error}"))
}

fn task_artifact_path(app: &AppHandle, task_id: &str) -> Result<PathBuf, String> {
    validate_task_id(task_id)?;
    Ok(task_result_root(app)?.join(format!("{task_id}.enc")))
}

fn task_artifact_aad(task_id: &str) -> Vec<u8> {
    format!("atrisbridge:ai-task-result:{task_id}").into_bytes()
}

fn write_task_artifact(
    app: &AppHandle,
    task_id: &str,
    artifact: &TaskArtifact,
) -> Result<(), String> {
    let root = task_result_root(app)?;
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create AI task result directory: {error}"))?;
    let bytes = serde_json::to_vec(artifact)
        .map_err(|error| format!("Could not encode AI task artifact: {error}"))?;
    let path = task_artifact_path(app, task_id)?;
    if let Err(error) =
        ai_artifact_crypto::write_encrypted_artifact(&path, &bytes, &task_artifact_aad(task_id))
    {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(())
}

fn read_task_artifact(app: &AppHandle, task_id: &str) -> Result<TaskArtifact, String> {
    let bytes = ai_artifact_crypto::read_encrypted_artifact(
        &task_artifact_path(app, task_id)?,
        &task_artifact_aad(task_id),
    )?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not decode AI task result artifact: {error}"))
}

fn validate_task_id(task_id: &str) -> Result<(), String> {
    let Some(value) = task_id.strip_prefix("abt_") else {
        return Err("AI task identifier is invalid.".into());
    };
    if value.len() != 36
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err("AI task identifier is invalid.".into());
    }
    Ok(())
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn short_task_id(task_id: &str) -> &str {
    task_id
        .strip_prefix("abt_")
        .and_then(|value| value.get(..8))
        .unwrap_or("task")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_are_path_safe() {
        let id = format!("abt_{}", Uuid::new_v4());
        assert!(validate_task_id(&id).is_ok());
        assert!(validate_task_id("../../result").is_err());
        assert!(validate_task_id("abt_not-a-uuid").is_err());
    }

    #[test]
    fn terminal_statuses_are_explicit() {
        assert!(!is_terminal_status("queued"));
        assert!(!is_terminal_status("running"));
        assert!(is_terminal_status("completed"));
        assert!(is_terminal_status("failed"));
        assert!(is_terminal_status("cancelled"));
        assert!(is_terminal_status("interrupted"));
    }

    #[test]
    fn task_artifact_aad_binds_task_identity() {
        assert_ne!(task_artifact_aad("abt_a"), task_artifact_aad("abt_b"));
    }
}
