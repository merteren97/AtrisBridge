use rusqlite::{params, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::{
    ai_changeset::{self, AiChangeRequest},
    ai_command, ai_gateway,
    ai_gateway::{AiSession, AiSessionMode},
    ai_git, ai_task,
    ai_task::{AiTaskManager, AiTaskRecord, AiTaskResult},
    ai_workspace,
    database::open_database,
    mcp_core, services,
    workspace_coordinator::WorkspaceMutationCoordinator,
};

const DEFAULT_LIST_LIMIT: u32 = 50;
const MAX_LIST_LIMIT: u32 = 200;
const DEFAULT_SESSION_TTL_MINUTES: u64 = 60;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpWorkspace {
    id: String,
    name: String,
    sync_mode: crate::models::SyncMode,
    last_scan_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTaskSnapshot {
    pub task: AiTaskRecord,
    pub result: Option<AiTaskResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionIdArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionOpenArgs {
    workspace_id: String,
    mode: AiSessionMode,
    requested_capabilities: Vec<String>,
    ttl_minutes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionPathArgs {
    session_id: String,
    relative_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadTextArgs {
    session_id: String,
    relative_path: String,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchArgs {
    session_id: String,
    query: String,
    limit: Option<u32>,
    #[serde(default)]
    include_sensitive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareChangesetArgs {
    session_id: String,
    changes: Vec<AiChangeRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangesetIdArgs {
    session_id: String,
    changeset_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionLimitArgs {
    session_id: String,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeDiscardArgs {
    session_id: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitDiffArgs {
    session_id: String,
    #[serde(default)]
    staged: bool,
    relative_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitPathsArgs {
    session_id: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitCommitArgs {
    session_id: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitBranchArgs {
    session_id: String,
    branch_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitRevertArgs {
    session_id: String,
    commit: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitPushArgs {
    session_id: String,
    remote: String,
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandRunArgs {
    session_id: String,
    profile_id: String,
}

pub fn manifest_value() -> Result<Value, String> {
    serde_json::to_value(mcp_core::manifest())
        .map_err(|error| format!("Could not encode AtrisBridge MCP manifest: {error}"))
}

pub fn dispatch_tool(
    app: &AppHandle,
    principal: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value, String> {
    match tool_name {
        "workspace_list" => {
            require_empty_object(arguments)?;
            let workspaces = services::workspace::list(app)?
                .into_iter()
                .map(|workspace| McpWorkspace {
                    id: workspace.id,
                    name: workspace.name,
                    sync_mode: workspace.sync_mode,
                    last_scan_at: workspace.last_scan_at,
                })
                .collect::<Vec<_>>();
            encode(workspaces)
        }
        "session_open" => {
            let args: SessionOpenArgs = decode(arguments)?;
            // Session approval is intentionally not inferred from the MCP caller. Persistent
            // workspace/client permission rules remain the authority for non-interactive local
            // transports; default-ask capabilities fail closed until the user approves them in
            // AtrisBridge.
            encode(ai_gateway::open_ai_session(
                app.clone(),
                args.workspace_id,
                principal.to_string(),
                args.mode,
                args.requested_capabilities,
                Vec::new(),
                args.ttl_minutes.unwrap_or(DEFAULT_SESSION_TTL_MINUTES),
            )?)
        }
        "session_status" => {
            let args: SessionIdArgs = decode(arguments)?;
            encode(owned_session(app, principal, &args.session_id)?)
        }
        "session_close" => {
            let args: SessionIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_gateway::close_ai_session(app.clone(), args.session_id)?)
        }
        "workspace_stat" => {
            let args: SessionPathArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_workspace::ai_file_stat(
                app.clone(),
                args.session_id,
                args.relative_path,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "workspace_read_text" => {
            let args: ReadTextArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_workspace::ai_read_text_file(
                app.clone(),
                args.session_id,
                args.relative_path,
                args.start_line,
                args.end_line,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "workspace_search" => {
            let args: SearchArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_workspace::ai_search_workspace(
                app.clone(),
                args.session_id,
                args.query,
                args.limit,
                args.include_sensitive,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "changeset_prepare" => {
            let args: PrepareChangesetArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_changeset::prepare_ai_changeset(
                app.clone(),
                args.session_id,
                args.changes,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "changeset_apply" => {
            let args: ChangesetIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            ensure_changeset_session(app, &args.session_id, &args.changeset_id)?;
            encode(ai_changeset::execute_ai_changeset(
                app.clone(),
                args.session_id,
                args.changeset_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "changeset_undo" => {
            let args: ChangesetIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            ensure_changeset_session(app, &args.session_id, &args.changeset_id)?;
            encode(ai_changeset::undo_ai_changeset(
                app.clone(),
                args.session_id,
                args.changeset_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "changeset_get" => {
            let args: ChangesetIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            let changeset = ai_changeset::get_ai_changeset(app.clone(), args.changeset_id)?;
            if changeset.session_id != args.session_id {
                return Err("AI changeset is not owned by the current workspace session.".into());
            }
            encode(changeset)
        }
        "changeset_list" => {
            let args: SessionLimitArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(list_session_changesets(
                app,
                &args.session_id,
                args.limit.unwrap_or(DEFAULT_LIST_LIMIT),
            )?)
        }
        "worktree_provision" => {
            let args: SessionIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::provision_ai_worktree(
                app.clone(),
                args.session_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "worktree_list" => {
            let args: SessionIdArgs = decode(arguments)?;
            let session = owned_session(app, principal, &args.session_id)?;
            let worktrees = ai_git::list_ai_worktrees(app.clone(), session.workspace_id)?
                .into_iter()
                .filter(|worktree| worktree.session_id == args.session_id)
                .collect::<Vec<_>>();
            encode(worktrees)
        }
        "worktree_discard" => {
            let args: WorktreeDiscardArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::discard_ai_worktree(
                app.clone(),
                args.session_id,
                args.force,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_status" => {
            let args: SessionIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_status(
                app.clone(),
                args.session_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_diff" => {
            let args: GitDiffArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_diff(
                app.clone(),
                args.session_id,
                args.staged,
                args.relative_path,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_log" => {
            let args: SessionLimitArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_log(
                app.clone(),
                args.session_id,
                args.limit,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_branches" => {
            let args: SessionIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_branches(
                app.clone(),
                args.session_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_stage" => {
            let args: GitPathsArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_stage(
                app.clone(),
                args.session_id,
                args.paths,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_unstage" => {
            let args: GitPathsArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_unstage(
                app.clone(),
                args.session_id,
                args.paths,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_commit" => {
            let args: GitCommitArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_commit(
                app.clone(),
                args.session_id,
                args.message,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_create_branch" => {
            let args: GitBranchArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_create_branch(
                app.clone(),
                args.session_id,
                args.branch_name,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_revert" => {
            let args: GitRevertArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_revert(
                app.clone(),
                args.session_id,
                args.commit,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "git_push" => {
            let args: GitPushArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_git::ai_git_push(
                app.clone(),
                args.session_id,
                args.remote,
                args.branch,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "command_profiles" => {
            let args: SessionIdArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_command::list_ai_command_profiles(
                app.clone(),
                args.session_id,
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        "command_run" => {
            let args: CommandRunArgs = decode(arguments)?;
            owned_session(app, principal, &args.session_id)?;
            encode(ai_task::start_ai_command_task(
                app.clone(),
                args.session_id,
                args.profile_id,
                app.state::<AiTaskManager>(),
                app.state::<WorkspaceMutationCoordinator>(),
            )?)
        }
        _ => Err(format!("Unknown AtrisBridge MCP tool '{tool_name}'.")),
    }
}

pub fn task_snapshot(
    app: &AppHandle,
    principal: &str,
    task_id: &str,
) -> Result<McpTaskSnapshot, String> {
    let (session_id, owner) = task_owner(app, task_id)?
        .ok_or_else(|| "AI task was not found for the authenticated client.".to_string())?;
    if owner != principal {
        return Err("AI task is not owned by the authenticated MCP client.".into());
    }
    owned_session(app, principal, &session_id)?;
    let task = ai_task::get_ai_task(app.clone(), session_id.clone(), task_id.to_string())?;
    let result = if is_terminal_task_status(&task.status) {
        Some(ai_task::get_ai_task_result(
            app.clone(),
            session_id,
            task_id.to_string(),
        )?)
    } else {
        None
    };
    Ok(McpTaskSnapshot { task, result })
}

pub fn cancel_task(
    app: &AppHandle,
    principal: &str,
    task_id: &str,
) -> Result<AiTaskRecord, String> {
    let (session_id, owner) = task_owner(app, task_id)?
        .ok_or_else(|| "AI task was not found for the authenticated client.".to_string())?;
    if owner != principal {
        return Err("AI task is not owned by the authenticated MCP client.".into());
    }
    owned_session(app, principal, &session_id)?;
    ai_task::cancel_ai_task(
        app.clone(),
        session_id,
        task_id.to_string(),
        app.state::<AiTaskManager>(),
    )
}

fn owned_session(app: &AppHandle, principal: &str, session_id: &str) -> Result<AiSession, String> {
    let connection = open_database(app)?;
    let owner = connection
        .query_row(
            "SELECT client_id FROM ai_sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not verify AI session ownership: {error}"))?
        .ok_or_else(|| "AI session was not found.".to_string())?;
    if owner != principal {
        return Err("AI session is not owned by the authenticated MCP client.".into());
    }
    ai_gateway::list_ai_sessions(app.clone(), None)?
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| "AI session disappeared while verifying ownership.".to_string())
}

fn ensure_changeset_session(
    app: &AppHandle,
    session_id: &str,
    changeset_id: &str,
) -> Result<(), String> {
    let connection = open_database(app)?;
    let owner = connection
        .query_row(
            "SELECT session_id FROM ai_changesets WHERE id = ?1",
            params![changeset_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not verify AI changeset ownership: {error}"))?
        .ok_or_else(|| "AI changeset was not found.".to_string())?;
    if owner != session_id {
        return Err("AI changeset is not owned by the current workspace session.".into());
    }
    Ok(())
}

fn list_session_changesets(
    app: &AppHandle,
    session_id: &str,
    limit: u32,
) -> Result<Vec<ai_changeset::AiChangeset>, String> {
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    let connection = open_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT id FROM ai_changesets
             WHERE session_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare MCP changeset query: {error}"))?;
    let ids = statement
        .query_map(params![session_id, i64::from(limit)], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("Could not query MCP changesets: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read MCP changeset IDs: {error}"))?;
    drop(statement);
    drop(connection);
    ids.into_iter()
        .map(|id| ai_changeset::get_ai_changeset(app.clone(), id))
        .collect()
}

fn task_owner(app: &AppHandle, task_id: &str) -> Result<Option<(String, String)>, String> {
    let connection = open_database(app)?;
    connection
        .query_row(
            "SELECT session_id, client_id FROM ai_tasks WHERE id = ?1",
            params![task_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("Could not verify AI task ownership: {error}"))
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("Invalid AtrisBridge MCP tool arguments: {error}"))
}

fn encode<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value)
        .map_err(|error| format!("Could not encode AtrisBridge MCP tool result: {error}"))
}

fn require_empty_object(value: Value) -> Result<(), String> {
    match value {
        Value::Object(object) if object.is_empty() => Ok(()),
        _ => Err("This AtrisBridge MCP tool does not accept arguments.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_task_statuses_are_explicit() {
        for status in ["completed", "failed", "cancelled", "interrupted"] {
            assert!(is_terminal_task_status(status));
        }
        for status in ["queued", "running"] {
            assert!(!is_terminal_task_status(status));
        }
    }

    #[test]
    fn empty_object_validation_rejects_hidden_arguments() {
        assert!(require_empty_object(json!({})).is_ok());
        assert!(require_empty_object(json!({"clientId": "spoof"})).is_err());
    }
}
