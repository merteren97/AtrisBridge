use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    ai_gateway::{self, AiAuditEvent, AiSession},
    ai_output::TailPreservingBuffer,
    ai_workspace::{
        canonical_workspace_root, classify_relative_path, ensure_ai_path_allowed,
        normalize_relative_path, AiPathClass,
    },
    database::open_database,
    storage::find_workspace,
    workspace_coordinator::{
        WorkspaceMutationCoordinator, WorkspaceMutationLease, WorkspaceOperationKind,
    },
};

const MAX_GIT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_GIT_ERROR_BYTES: usize = 64 * 1024;
const MAX_GIT_PATHS: usize = 200;
const MAX_DIFF_PATHS: usize = 2_000;
const MAX_LOG_LIMIT: u32 = 100;
const MAX_COMMIT_MESSAGE_CHARS: usize = 4_096;
const LOCAL_GIT_TIMEOUT: Duration = Duration::from_secs(45);
const REMOTE_GIT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct StoredWorktree {
    session_id: String,
    workspace_id: String,
    branch_name: String,
    path: String,
    base_commit: String,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitWorktree {
    pub session_id: String,
    pub workspace_id: String,
    pub branch_name: String,
    pub base_commit: String,
    pub head_commit: Option<String>,
    pub status: String,
    pub dirty: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitStatusEntry {
    pub path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
    pub sensitive: bool,
    pub owned_by_session: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitStatus {
    pub workspace_id: String,
    pub branch: Option<String>,
    pub head_commit: String,
    pub ahead: u64,
    pub behind: u64,
    pub dirty: bool,
    pub entries: Vec<AiGitStatusEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitDiff {
    pub workspace_id: String,
    pub staged: bool,
    pub relative_path: Option<String>,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitCommitEntry {
    pub commit: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitBranch {
    pub name: String,
    pub commit: String,
    pub upstream: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitMutationResult {
    pub workspace_id: String,
    pub branch: Option<String>,
    pub head_commit: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGitPushResult {
    pub workspace_id: String,
    pub remote: String,
    pub branch: String,
    pub head_commit: String,
}

struct CapturedPipe {
    bytes: Vec<u8>,
    truncated: bool,
}

struct GitOutput {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let connection = open_git_database(app)?;
    connection
        .execute(
            "UPDATE ai_git_worktrees
             SET status = 'retained', updated_at = ?1
             WHERE status = 'ready'
               AND session_id IN (
                   SELECT id FROM ai_sessions WHERE status <> 'active'
               )",
            params![Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("Could not retain interrupted AI worktrees: {error}"))?;
    drop(connection);

    for record in load_worktree_records(app, None)? {
        match record.status.as_str() {
            "ready" | "retained" if !Path::new(&record.path).is_dir() => {
                set_worktree_status(app, &record.session_id, "recovery_required")?;
            }
            "recovery_required" if recoverable_provisioned_worktree(app, &record) => {
                set_worktree_status(app, &record.session_id, "retained")?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn recoverable_provisioned_worktree(app: &AppHandle, record: &StoredWorktree) -> bool {
    let Ok(root) = PathBuf::from(&record.path).canonicalize() else {
        return false;
    };
    if ensure_under_worktree_storage(app, &root).is_err() || ensure_repo_root(&root).is_err() {
        return false;
    }
    if current_branch(&root).ok().flatten().as_deref() != Some(record.branch_name.as_str()) {
        return false;
    }
    if head_commit(&root).ok().as_deref() != Some(record.base_commit.as_str()) {
        return false;
    }
    git_stdout(
        &root,
        git_safe_read_args(["status", "--porcelain=v1", "--untracked-files=all"]),
        MAX_GIT_OUTPUT_BYTES,
    )
    .map(|status| status.trim().is_empty())
    .unwrap_or(false)
}

#[tauri::command]
pub fn provision_ai_worktree(
    app: AppHandle,
    session_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitWorktree, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = ensure_isolated_worktree(&app, &session, &coordinator).and_then(|root| {
        load_worktree_record(&app, &session.id)?
            .ok_or_else(|| "AI worktree metadata is missing after provisioning.".to_string())
            .and_then(|record| public_worktree(&root, &record))
    });
    record_git_result(
        &app,
        &session,
        "git.local",
        "git.worktree_provision",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn list_ai_worktrees(
    app: AppHandle,
    workspace_id: String,
) -> Result<Vec<AiGitWorktree>, String> {
    find_workspace(&app, &workspace_id)?;
    load_worktree_records(&app, Some(&workspace_id))?
        .into_iter()
        .map(|record| {
            let root = PathBuf::from(&record.path);
            public_worktree(&root, &record)
        })
        .collect()
}

#[tauri::command]
pub fn discard_ai_worktree(
    app: AppHandle,
    session_id: String,
    force: bool,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitWorktree, String> {
    let record = load_worktree_record(&app, &session_id)?
        .ok_or_else(|| "AI worktree was not found.".to_string())?;
    if record.status == "removed" {
        return public_worktree(Path::new(&record.path), &record);
    }
    let workspace = find_workspace(&app, &record.workspace_id)?;
    let source_root = canonical_workspace_root(&workspace.local_path)?;
    ensure_repo_root(&source_root)?;
    let _lease = acquire_git(&coordinator, &record.workspace_id, "desktop")?;
    let worktree_path = PathBuf::from(&record.path);
    if worktree_path.is_dir() {
        let dirty = !git_stdout(
            &worktree_path,
            git_safe_read_args(["status", "--porcelain=v1", "--untracked-files=all"]),
            MAX_GIT_OUTPUT_BYTES,
        )?
        .trim()
        .is_empty();
        if dirty && !force {
            return Err(
                "AI worktree has uncommitted changes. Review it first or explicitly force discard."
                    .into(),
            );
        }
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend(["worktree".into(), "remove".into()]);
        if force {
            args.push("--force".into());
        }
        args.push(worktree_path.to_string_lossy().to_string());
        run_git_checked(&source_root, args, LOCAL_GIT_TIMEOUT, "remove AI worktree")?;
    } else {
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend(["worktree".into(), "prune".into()]);
        run_git_checked(&source_root, args, LOCAL_GIT_TIMEOUT, "prune AI worktrees")?;
    }
    clear_stage_ownership(&app, &session_id)?;
    set_worktree_status(&app, &session_id, "removed")?;
    let record = load_worktree_record(&app, &session_id)?
        .ok_or_else(|| "AI worktree metadata disappeared while discarding.".to_string())?;
    public_worktree(Path::new(&record.path), &record)
}

#[tauri::command]
pub fn ai_git_status(
    app: AppHandle,
    session_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitStatus, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        status_inner(&app, &session, &root)
    })();
    record_git_result(&app, &session, "git.local", "git.status", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_diff(
    app: AppHandle,
    session_id: String,
    staged: bool,
    relative_path: Option<String>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitDiff, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    ai_gateway::authorize_session(&app, &session_id, "workspace.read")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        let normalized = relative_path
            .as_deref()
            .map(normalize_relative_path)
            .transpose()?;
        let paths = if let Some(path) = normalized.as_deref() {
            authorize_git_read_path(&app, &session, &root, path)?;
            vec![path.to_string()]
        } else {
            visible_diff_paths(&app, &session, &root, staged)?
        };
        if paths.is_empty() {
            return Ok(AiGitDiff {
                workspace_id: session.workspace_id.clone(),
                staged,
                relative_path: normalized,
                diff: String::new(),
                truncated: false,
            });
        }
        let mut args = git_safe_read_args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
            "--unified=3",
        ]);
        if staged {
            args.push("--cached".into());
        }
        args.push("--".into());
        args.extend(paths);
        let output = run_git_checked(&root, args, LOCAL_GIT_TIMEOUT, "read Git diff")?;
        Ok(AiGitDiff {
            workspace_id: session.workspace_id.clone(),
            staged,
            relative_path: normalized,
            diff: String::from_utf8_lossy(&output.stdout).into_owned(),
            truncated: output.stdout_truncated,
        })
    })();
    record_git_result(&app, &session, "git.local", "git.diff", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_log(
    app: AppHandle,
    session_id: String,
    limit: Option<u32>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<Vec<AiGitCommitEntry>, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        let limit = limit.unwrap_or(25).clamp(1, MAX_LOG_LIMIT);
        let args = git_safe_read_args([
            "log",
            &format!("-{limit}"),
            "--date=iso-strict",
            "--pretty=format:%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e",
        ]);
        let text = git_stdout(&root, args, MAX_GIT_OUTPUT_BYTES)?;
        Ok(parse_log(&text))
    })();
    record_git_result(&app, &session, "git.local", "git.log", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_branches(
    app: AppHandle,
    session_id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<Vec<AiGitBranch>, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        let text = git_stdout(
            &root,
            git_safe_read_args([
                "for-each-ref",
                "refs/heads",
                "--format=%(refname:short)%00%(objectname)%00%(upstream:short)%00%(HEAD)",
            ]),
            MAX_GIT_OUTPUT_BYTES,
        )?;
        Ok(parse_branches(&text))
    })();
    record_git_result(
        &app,
        &session,
        "git.local",
        "git.branches",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn ai_git_stage(
    app: AppHandle,
    session_id: String,
    paths: Vec<String>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitMutationResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        ensure_filter_execution_policy(&app, &session, &root)?;
        let paths = authorize_git_write_paths(&app, &session, &root, paths)?;
        ensure_stage_claim_is_safe(&app, &session, &root, &paths)?;
        let mut args = git_safe_mutation_prefix(&app)?;
        args.push("add".into());
        args.push("--".into());
        args.extend(paths.iter().cloned());
        run_git_checked(&root, args, LOCAL_GIT_TIMEOUT, "stage Git paths")?;
        claim_staged_paths(&app, &session.id, &root, &paths)?;
        mutation_result(&session, &root)
    })();
    record_git_result(&app, &session, "git.local", "git.stage", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_unstage(
    app: AppHandle,
    session_id: String,
    paths: Vec<String>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitMutationResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        let paths = authorize_git_write_paths(&app, &session, &root, paths)?;
        ensure_paths_owned_by_session(&app, &session.id, &root, &paths)?;
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend(["reset".into(), "-q".into(), "HEAD".into(), "--".into()]);
        args.extend(paths.iter().cloned());
        run_git_checked(&root, args, LOCAL_GIT_TIMEOUT, "unstage Git paths")?;
        release_staged_paths(&app, &session.id, &paths)?;
        mutation_result(&session, &root)
    })();
    record_git_result(&app, &session, "git.local", "git.unstage", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_commit(
    app: AppHandle,
    session_id: String,
    message: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitMutationResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        validate_commit_message(&message)?;
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        ensure_commit_stage_ownership(&app, &session, &root)?;
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend([
            "-c".into(),
            "commit.gpgSign=false".into(),
            "commit".into(),
            "--no-verify".into(),
            "-m".into(),
            message,
        ]);
        run_git_checked(&root, args, LOCAL_GIT_TIMEOUT, "create Git commit")?;
        clear_stage_ownership(&app, &session.id)?;
        mutation_result(&session, &root)
    })();
    record_git_result(&app, &session, "git.local", "git.commit", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_create_branch(
    app: AppHandle,
    session_id: String,
    branch_name: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitMutationResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        ensure_worktree_clean(&root)?;
        ensure_filter_execution_policy(&app, &session, &root)?;
        validate_branch_name(&root, &branch_name)?;
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend(["switch".into(), "-c".into(), branch_name.clone()]);
        run_git_checked(&root, args, LOCAL_GIT_TIMEOUT, "create Git branch")?;
        if session.mode == "isolated_worktree" {
            update_worktree_branch(&app, &session.id, &branch_name)?;
        }
        mutation_result(&session, &root)
    })();
    record_git_result(
        &app,
        &session,
        "git.local",
        "git.branch_create",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn ai_git_revert(
    app: AppHandle,
    session_id: String,
    commit: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitMutationResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    ai_gateway::authorize_session(&app, &session_id, "workspace.edit")?;
    let result = (|| {
        validate_commit_id(&commit)?;
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        ensure_worktree_clean(&root)?;
        ensure_filter_execution_policy(&app, &session, &root)?;
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend([
            "-c".into(),
            "commit.gpgSign=false".into(),
            "revert".into(),
            "--no-edit".into(),
            commit,
        ]);
        let output = run_git(&root, args, LOCAL_GIT_TIMEOUT)?;
        if !output.success {
            let mut abort = git_safe_mutation_prefix(&app)?;
            abort.extend(["revert".into(), "--abort".into()]);
            let _ = run_git(&root, abort, LOCAL_GIT_TIMEOUT);
            return Err(local_git_error(
                "Git revert failed and was aborted",
                &output,
            ));
        }
        mutation_result(&session, &root)
    })();
    record_git_result(&app, &session, "git.local", "git.revert", started, &result)?;
    result
}

#[tauri::command]
pub fn ai_git_push(
    app: AppHandle,
    session_id: String,
    remote: String,
    branch: Option<String>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiGitPushResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "git.remote")?;
    ai_gateway::authorize_session(&app, &session_id, "git.local")?;
    let result = (|| {
        validate_remote_name(&remote)?;
        let root = session_workspace_root(&app, &session, &coordinator)?;
        let _lease = acquire_git(&coordinator, &session.workspace_id, &session.client_id)?;
        validate_remote_transport(&root, &remote)?;
        validate_remote_execution_config(&root, &remote)?;
        let branch = match branch {
            Some(value) => {
                validate_branch_name(&root, &value)?;
                value
            }
            None => current_branch(&root)?.ok_or_else(|| {
                "Cannot push from a detached Git HEAD without an explicit branch.".to_string()
            })?,
        };
        let mut args = git_safe_mutation_prefix(&app)?;
        args.extend([
            "push".into(),
            "--porcelain".into(),
            remote.clone(),
            format!("HEAD:refs/heads/{branch}"),
        ]);
        let output = run_git(&root, args, REMOTE_GIT_TIMEOUT)?;
        if !output.success {
            return Err(format!(
                "Git push to remote '{}' failed without exposing remote credentials or transport output.",
                remote
            ));
        }
        Ok(AiGitPushResult {
            workspace_id: session.workspace_id.clone(),
            remote,
            branch,
            head_commit: head_commit(&root)?,
        })
    })();
    record_git_result(&app, &session, "git.remote", "git.push", started, &result)?;
    result
}

pub(crate) fn session_workspace_root(
    app: &AppHandle,
    session: &AiSession,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<PathBuf, String> {
    if session.mode == "direct" {
        let workspace = find_workspace(app, &session.workspace_id)?;
        return canonical_workspace_root(&workspace.local_path);
    }
    if session.mode != "isolated_worktree" {
        return Err("AI session mode is invalid.".into());
    }
    ensure_isolated_worktree(app, session, coordinator)
}

pub(crate) fn changeset_workspace_root(
    app: &AppHandle,
    workspace_id: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    if let Some(record) = load_worktree_record(app, session_id)? {
        if record.workspace_id != workspace_id {
            return Err("AI worktree workspace ownership is inconsistent.".into());
        }
        if record.status == "removed" {
            return Err("AI worktree was removed before changeset recovery completed.".into());
        }
        let root = PathBuf::from(record.path)
            .canonicalize()
            .map_err(|error| format!("Could not resolve retained AI worktree: {error}"))?;
        ensure_under_worktree_storage(app, &root)?;
        return Ok(root);
    }
    let workspace = find_workspace(app, workspace_id)?;
    canonical_workspace_root(&workspace.local_path)
}

pub(crate) fn changeset_targets_primary_workspace(
    app: &AppHandle,
    session_id: &str,
) -> Result<bool, String> {
    Ok(load_worktree_record(app, session_id)?.is_none())
}

fn ensure_isolated_worktree(
    app: &AppHandle,
    session: &AiSession,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<PathBuf, String> {
    ai_gateway::authorize_session(app, &session.id, "git.local")?;
    if let Some(record) = load_worktree_record(app, &session.id)? {
        if record.workspace_id != session.workspace_id {
            return Err("AI worktree ownership does not match the active session.".into());
        }
        if record.status == "recovery_required" && recoverable_provisioned_worktree(app, &record) {
            set_worktree_status(app, &session.id, "ready")?;
        } else if record.status != "ready" {
            return Err(format!(
                "AI worktree is not usable from status '{}'.",
                record.status
            ));
        }
        let root = PathBuf::from(&record.path)
            .canonicalize()
            .map_err(|error| format!("Could not resolve AI worktree: {error}"))?;
        ensure_under_worktree_storage(app, &root)?;
        ensure_repo_root(&root)?;
        return Ok(root);
    }

    let workspace = find_workspace(app, &session.workspace_id)?;
    let source_root = canonical_workspace_root(&workspace.local_path)?;
    ensure_repo_root(&source_root)?;
    let _lease = acquire_git(coordinator, &session.workspace_id, &session.client_id)?;
    ensure_worktree_clean(&source_root)?;
    ensure_filter_execution_policy(app, session, &source_root)?;

    let base_commit = head_commit(&source_root)?;
    let branch_name = isolated_branch_name(&session.workspace_id, &session.id)?;
    validate_branch_name(&source_root, &branch_name)?;
    let worktree_path = worktree_path(app, &session.workspace_id, &session.id)?;
    if worktree_path.exists() {
        return Err("Reserved AI worktree directory already exists without metadata.".into());
    }

    let now = Utc::now().to_rfc3339();
    let connection = open_git_database(app)?;
    connection
        .execute(
            "INSERT INTO ai_git_worktrees (
                session_id, workspace_id, branch_name, path, base_commit,
                status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'recovery_required', ?6, ?6)",
            params![
                session.id,
                session.workspace_id,
                branch_name,
                worktree_path.to_string_lossy().to_string(),
                base_commit,
                now,
            ],
        )
        .map_err(|error| format!("Could not pre-journal AI worktree provisioning: {error}"))?;
    drop(connection);

    let parent = worktree_path
        .parent()
        .ok_or_else(|| "AI worktree path has no parent directory.".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create AI worktree parent directory: {error}"))?;

    let mut args = git_safe_mutation_prefix(app)?;
    args.extend([
        "worktree".into(),
        "add".into(),
        "-b".into(),
        branch_name.clone(),
        worktree_path.to_string_lossy().to_string(),
        base_commit.clone(),
    ]);
    run_git_checked(
        &source_root,
        args,
        LOCAL_GIT_TIMEOUT,
        "create isolated AI worktree",
    )?;

    let canonical = worktree_path
        .canonicalize()
        .map_err(|error| format!("Could not resolve created AI worktree: {error}"))?;
    ensure_under_worktree_storage(app, &canonical)?;
    ensure_repo_root(&canonical)?;
    if current_branch(&canonical)?.as_deref() != Some(branch_name.as_str()) {
        return Err("Created AI worktree checked out an unexpected branch.".into());
    }
    if head_commit(&canonical)? != base_commit {
        return Err("Created AI worktree HEAD does not match its journaled base commit.".into());
    }
    ensure_worktree_clean(&canonical)?;

    let connection = open_git_database(app)?;
    let updated = connection
        .execute(
            "UPDATE ai_git_worktrees
             SET path = ?1, status = 'ready', updated_at = ?2
             WHERE session_id = ?3 AND status = 'recovery_required'",
            params![
                canonical.to_string_lossy().to_string(),
                Utc::now().to_rfc3339(),
                session.id,
            ],
        )
        .map_err(|error| format!("Could not finalize AI worktree journal: {error}"))?;
    if updated != 1 {
        return Err("AI worktree journal changed while provisioning.".into());
    }
    Ok(canonical)
}

fn status_inner(app: &AppHandle, session: &AiSession, root: &Path) -> Result<AiGitStatus, String> {
    ensure_repo_root(root)?;
    let text = git_stdout(
        root,
        git_safe_read_args([
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=all",
        ]),
        MAX_GIT_OUTPUT_BYTES,
    )?;
    let owned = load_stage_ownership(app, &session.id)?;
    let can_read_sensitive = session
        .capabilities
        .iter()
        .any(|capability| capability == "sensitive.read");
    let mut branch = None;
    let mut ahead = 0u64;
    let mut behind = 0u64;
    let mut entries = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            let parsed = parse_branch_header(header);
            branch = parsed.0;
            ahead = parsed.1;
            behind = parsed.2;
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let index_status = line[0..1].to_string();
        let worktree_status = line[1..2].to_string();
        let raw_path = line[3..].trim().to_string();
        let candidate = raw_path
            .rsplit_once(" -> ")
            .map(|(_, destination)| destination.to_string())
            .unwrap_or(raw_path);
        let normalized = normalize_relative_path(candidate.trim_matches('"')).ok();
        let class = normalized
            .as_deref()
            .and_then(|path| classify_relative_path(path).ok())
            .unwrap_or(AiPathClass::Denied);
        let sensitive = class == AiPathClass::Sensitive;
        let path = if class == AiPathClass::Denied || (sensitive && !can_read_sensitive) {
            None
        } else {
            normalized
        };
        let owned_by_session = path
            .as_ref()
            .map(|path| owned.contains_key(path))
            .unwrap_or(false);
        entries.push(AiGitStatusEntry {
            path,
            index_status,
            worktree_status,
            sensitive,
            owned_by_session,
        });
    }
    Ok(AiGitStatus {
        workspace_id: session.workspace_id.clone(),
        branch,
        head_commit: head_commit(root)?,
        ahead,
        behind,
        dirty: !entries.is_empty(),
        entries,
    })
}

fn visible_diff_paths(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
    staged: bool,
) -> Result<Vec<String>, String> {
    let mut args = git_safe_read_args([
        "diff",
        "--name-only",
        "-z",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
    ]);
    if staged {
        args.push("--cached".into());
    }
    let output = run_git_checked(root, args, LOCAL_GIT_TIMEOUT, "enumerate Git diff paths")?;
    if output.stdout_truncated {
        return Err("Git diff path list exceeded the AtrisBridge safety bound.".into());
    }
    let mut visible = Vec::new();
    for raw in output
        .stdout
        .split(|value| *value == 0)
        .filter(|value| !value.is_empty())
    {
        if visible.len() >= MAX_DIFF_PATHS {
            return Err("Git diff contains too many paths for a bounded AI request.".into());
        }
        let path = String::from_utf8(raw.to_vec())
            .map_err(|_| "Git diff path is not valid UTF-8.".to_string())?;
        let normalized = normalize_relative_path(&path)?;
        let class = match ensure_ai_path_allowed(root, &normalized) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if class == AiPathClass::Sensitive
            && !session
                .capabilities
                .iter()
                .any(|capability| capability == "sensitive.read")
        {
            continue;
        }
        visible.push(normalized);
    }
    if visible.iter().any(|path| {
        classify_relative_path(path)
            .map(|class| class == AiPathClass::Sensitive)
            .unwrap_or(false)
    }) {
        ai_gateway::authorize_session(app, &session.id, "sensitive.read")?;
    }
    Ok(visible)
}

fn authorize_git_read_path(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
    path: &str,
) -> Result<(), String> {
    let class = ensure_ai_path_allowed(root, path)?;
    if class == AiPathClass::Sensitive {
        ai_gateway::authorize_session(app, &session.id, "sensitive.read")?;
    }
    Ok(())
}

fn authorize_git_write_paths(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.is_empty() || paths.len() > MAX_GIT_PATHS {
        return Err(format!(
            "Git path operations must contain between 1 and {MAX_GIT_PATHS} paths."
        ));
    }
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let path = normalize_relative_path(&path)?;
        if !seen.insert(path.clone()) {
            continue;
        }
        let class = ensure_ai_path_allowed(root, &path)?;
        ensure_explicit_git_file(root, &path)?;
        if class == AiPathClass::Sensitive {
            ai_gateway::authorize_session(app, &session.id, "sensitive.write")?;
        }
        normalized.push(path);
    }
    Ok(normalized)
}

fn ensure_explicit_git_file(root: &Path, relative_path: &str) -> Result<(), String> {
    let candidate = root.join(relative_path);
    match fs::symlink_metadata(&candidate) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Git path '{relative_path}' must identify one explicit regular file. Directory and symlink staging is not allowed."
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let output = run_git(
                root,
                git_safe_read_args(["ls-files", "--error-unmatch", "--", relative_path]),
                LOCAL_GIT_TIMEOUT,
            )?;
            if output.success {
                Ok(())
            } else {
                Err(format!(
                    "Git path '{relative_path}' does not identify an existing regular file or a tracked deleted file."
                ))
            }
        }
        Err(error) => Err(format!(
            "Could not inspect Git path '{relative_path}': {error}"
        )),
    }
}

fn ensure_stage_claim_is_safe(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
    paths: &[String],
) -> Result<(), String> {
    let staged = staged_paths(root)?;
    let owned = load_stage_ownership(app, &session.id)?;
    for path in paths {
        if !staged.contains(path) {
            continue;
        }
        let Some(expected) = owned.get(path) else {
            return Err(format!(
                "Git path '{path}' was already staged outside this AI session and cannot be claimed."
            ));
        };
        verify_stage_evidence(root, path, expected)?;
    }
    Ok(())
}

fn ensure_commit_stage_ownership(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
) -> Result<(), String> {
    let staged = staged_paths(root)?;
    if staged.is_empty() {
        return Err("There are no staged changes to commit.".into());
    }
    let owned = load_stage_ownership(app, &session.id)?;
    for path in &staged {
        let Some(expected) = owned.get(path) else {
            return Err(format!(
                "Git path '{path}' was staged outside this AI session. AtrisBridge will not include it in an AI commit."
            ));
        };
        verify_stage_evidence(root, path, expected)?;
        let class = ensure_ai_path_allowed(root, path)?;
        if class == AiPathClass::Sensitive {
            ai_gateway::authorize_session(app, &session.id, "sensitive.write")?;
        }
    }
    Ok(())
}

fn staged_paths(root: &Path) -> Result<HashSet<String>, String> {
    let output = run_git_checked(
        root,
        git_safe_read_args([
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ]),
        LOCAL_GIT_TIMEOUT,
        "enumerate staged Git paths",
    )?;
    if output.stdout_truncated {
        return Err("Staged Git path list exceeded the AtrisBridge safety bound.".into());
    }
    output
        .stdout
        .split(|value| *value == 0)
        .filter(|value| !value.is_empty())
        .map(|raw| {
            let path = String::from_utf8(raw.to_vec())
                .map_err(|_| "Staged Git path is not valid UTF-8.".to_string())?;
            normalize_relative_path(&path)
        })
        .collect()
}

fn stage_index_evidence(root: &Path, relative_path: &str) -> Result<String, String> {
    let output = run_git_checked(
        root,
        git_safe_read_args(["ls-files", "--stage", "-z", "--", relative_path]),
        LOCAL_GIT_TIMEOUT,
        "inspect staged Git index entry",
    )?;
    if output.stdout_truncated {
        return Err("Git index evidence exceeded the AtrisBridge safety bound.".into());
    }
    let entries = output
        .stdout
        .split(|value| *value == 0)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if entries.len() > 1 {
        return Err(format!(
            "Git index for '{relative_path}' contains multiple conflict stages and cannot be owned by an AI session."
        ));
    }
    if let Some(raw) = entries.first() {
        let record = String::from_utf8(raw.to_vec())
            .map_err(|_| "Git index entry is not valid UTF-8.".to_string())?;
        let (metadata, path) = record
            .split_once('\t')
            .ok_or_else(|| "Git index entry has an invalid format.".to_string())?;
        if normalize_relative_path(path)? != relative_path {
            return Err("Git index evidence resolved to an unexpected path.".into());
        }
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[2] != "0" || !is_regular_git_mode(fields[0]) {
            return Err("AI staging only supports stage-0 regular-file Git index entries.".into());
        }
        validate_object_id(fields[1])?;
        return Ok(format!("index:{}:{}", fields[0], fields[1]));
    }

    let head = run_git_checked(
        root,
        git_safe_read_args(["ls-tree", "-z", "HEAD", "--", relative_path]),
        LOCAL_GIT_TIMEOUT,
        "inspect deleted Git index entry",
    )?;
    if head.stdout_truncated {
        return Err("Git HEAD evidence exceeded the AtrisBridge safety bound.".into());
    }
    let entries = head
        .stdout
        .split(|value| *value == 0)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if entries.len() != 1 {
        return Err(format!(
            "Git path '{relative_path}' has no unambiguous stage-0 index evidence."
        ));
    }
    let record = String::from_utf8(entries[0].to_vec())
        .map_err(|_| "Git HEAD entry is not valid UTF-8.".to_string())?;
    let (metadata, path) = record
        .split_once('\t')
        .ok_or_else(|| "Git HEAD entry has an invalid format.".to_string())?;
    if normalize_relative_path(path)? != relative_path {
        return Err("Git HEAD evidence resolved to an unexpected path.".into());
    }
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 || fields[1] != "blob" || !is_regular_git_mode(fields[0]) {
        return Err("AI staged deletions only support regular files.".into());
    }
    validate_object_id(fields[2])?;
    Ok(format!("deleted:{}:{}", fields[0], fields[2]))
}

fn verify_stage_evidence(root: &Path, relative_path: &str, expected: &str) -> Result<(), String> {
    let current = stage_index_evidence(root, relative_path)?;
    if current != expected {
        return Err(format!(
            "Git index entry for '{relative_path}' changed after this AI session staged it. Review the index before continuing."
        ));
    }
    Ok(())
}

fn is_regular_git_mode(mode: &str) -> bool {
    matches!(mode, "100644" | "100755")
}

fn validate_object_id(value: &str) -> Result<(), String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Git object ID has an invalid format.".into());
    }
    Ok(())
}

fn ensure_paths_owned_by_session(
    app: &AppHandle,
    session_id: &str,
    root: &Path,
    paths: &[String],
) -> Result<(), String> {
    let owned = load_stage_ownership(app, session_id)?;
    for path in paths {
        let Some(expected) = owned.get(path) else {
            return Err(format!(
                "Git path '{path}' is not staged under this AI session and cannot be unstaged by it."
            ));
        };
        verify_stage_evidence(root, path, expected)?;
    }
    Ok(())
}

fn claim_staged_paths(
    app: &AppHandle,
    session_id: &str,
    root: &Path,
    paths: &[String],
) -> Result<(), String> {
    let staged = staged_paths(root)?;
    let mut evidence = Vec::new();
    for path in paths {
        if staged.contains(path) {
            evidence.push((path.clone(), stage_index_evidence(root, path)?));
        }
    }
    if evidence.is_empty() {
        return Err("None of the requested Git paths produced a staged change.".into());
    }

    let mut connection = open_git_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start Git stage ownership transaction: {error}"))?;
    let now = Utc::now().to_rfc3339();
    for (path, index_evidence) in evidence {
        transaction
            .execute(
                "INSERT INTO ai_git_stage_ownership (
                    session_id, relative_path, index_evidence, created_at
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id, relative_path) DO UPDATE SET
                    index_evidence = excluded.index_evidence,
                    created_at = excluded.created_at",
                params![session_id, path, index_evidence, now],
            )
            .map_err(|error| format!("Could not claim AI-staged Git path: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("Could not commit Git stage ownership: {error}"))
}

fn release_staged_paths(app: &AppHandle, session_id: &str, paths: &[String]) -> Result<(), String> {
    let connection = open_git_database(app)?;
    for path in paths {
        connection
            .execute(
                "DELETE FROM ai_git_stage_ownership
                 WHERE session_id = ?1 AND relative_path = ?2",
                params![session_id, path],
            )
            .map_err(|error| format!("Could not release AI-staged Git path: {error}"))?;
    }
    Ok(())
}

fn clear_stage_ownership(app: &AppHandle, session_id: &str) -> Result<(), String> {
    let connection = open_git_database(app)?;
    connection
        .execute(
            "DELETE FROM ai_git_stage_ownership WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|error| format!("Could not clear AI Git stage ownership: {error}"))?;
    Ok(())
}

fn load_stage_ownership(
    app: &AppHandle,
    session_id: &str,
) -> Result<HashMap<String, String>, String> {
    let connection = open_git_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT relative_path, index_evidence
             FROM ai_git_stage_ownership WHERE session_id = ?1",
        )
        .map_err(|error| format!("Could not prepare Git stage ownership query: {error}"))?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("Could not query Git stage ownership: {error}"))?;
    let owned = rows
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(|error| format!("Could not read Git stage ownership: {error}"))?;
    Ok(owned)
}

fn ensure_filter_execution_policy(
    app: &AppHandle,
    session: &AiSession,
    root: &Path,
) -> Result<(), String> {
    let output = run_git(
        root,
        git_safe_read_args([
            "config",
            "--get-regexp",
            "^filter\\..*\\.(clean|smudge|process)$",
        ]),
        LOCAL_GIT_TIMEOUT,
    )?;
    if output.success || !output.stdout.is_empty() {
        let text = String::from_utf8_lossy(&output.stdout);
        if !text.trim().is_empty() {
            ai_gateway::authorize_session(app, &session.id, "command.execute").map_err(|_| {
                "This repository configures executable Git clean/smudge/process filters. Isolated checkout or staging requires command.execute permission for this AI session."
                    .to_string()
            })?;
        }
    }
    Ok(())
}

fn validate_remote_execution_config(root: &Path, remote: &str) -> Result<(), String> {
    let local_helper = run_git(
        root,
        git_safe_read_args(["config", "--local", "--get-all", "credential.helper"]),
        LOCAL_GIT_TIMEOUT,
    )?;
    if local_helper.success && !local_helper.stdout.is_empty() {
        return Err(
            "Repository-local credential helpers are blocked for AI Git remote operations. Use a trusted user/OS credential helper instead."
                .into(),
        );
    }
    for key in [
        "core.sshCommand".to_string(),
        format!("remote.{remote}.receivepack"),
        format!("remote.{remote}.uploadpack"),
    ] {
        let output = run_git(
            root,
            git_safe_read_args(["config", "--local", "--get", &key]),
            LOCAL_GIT_TIMEOUT,
        )?;
        if output.success && !output.stdout.is_empty() {
            return Err(format!(
                "Repository-local Git execution setting '{key}' is blocked for AI remote operations."
            ));
        }
    }
    Ok(())
}

fn validate_remote_transport(root: &Path, remote: &str) -> Result<(), String> {
    let url = git_stdout(
        root,
        git_safe_read_args(["remote", "get-url", "--push", remote]),
        MAX_GIT_ERROR_BYTES,
    )?;
    let url = url.trim();
    let allowed = url.starts_with("https://")
        || url.starts_with("ssh://")
        || (url.contains('@')
            && url.contains(':')
            && !url.contains("::")
            && !url.starts_with('-')
            && !url.contains("\\"));
    if !allowed {
        return Err(
            "AI Git push only allows HTTPS, SSH, or standard SCP-style SSH remotes. Local, ext::, file, helper, and custom transports are blocked."
                .into(),
        );
    }
    Ok(())
}

fn mutation_result(session: &AiSession, root: &Path) -> Result<AiGitMutationResult, String> {
    Ok(AiGitMutationResult {
        workspace_id: session.workspace_id.clone(),
        branch: current_branch(root)?,
        head_commit: head_commit(root)?,
    })
}

fn public_worktree(root: &Path, record: &StoredWorktree) -> Result<AiGitWorktree, String> {
    let usable = root.is_dir() && record.status != "removed";
    let (head_commit, dirty) = if usable {
        (
            head_commit(root).ok(),
            !git_stdout(
                root,
                git_safe_read_args(["status", "--porcelain=v1", "--untracked-files=all"]),
                MAX_GIT_OUTPUT_BYTES,
            )
            .unwrap_or_default()
            .trim()
            .is_empty(),
        )
    } else {
        (None, false)
    };
    Ok(AiGitWorktree {
        session_id: record.session_id.clone(),
        workspace_id: record.workspace_id.clone(),
        branch_name: record.branch_name.clone(),
        base_commit: record.base_commit.clone(),
        head_commit,
        status: record.status.clone(),
        dirty,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    })
}

fn isolated_branch_name(workspace_id: &str, session_id: &str) -> Result<String, String> {
    Ok(format!(
        "atris/ai/{}/{}",
        safe_identifier(workspace_id)?,
        safe_identifier(session_id)?
    ))
}

fn safe_identifier(value: &str) -> Result<String, String> {
    let uuid = Uuid::parse_str(value)
        .map_err(|_| "AI workspace/session identifier is not a UUID.".to_string())?;
    Ok(uuid.simple().to_string()[..12].to_string())
}

fn worktree_path(app: &AppHandle, workspace_id: &str, session_id: &str) -> Result<PathBuf, String> {
    Ok(worktree_storage_root(app)?
        .join(safe_identifier(workspace_id)?)
        .join(safe_identifier(session_id)?))
}

fn worktree_storage_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("ai-worktrees"))
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))
}

fn ensure_under_worktree_storage(app: &AppHandle, root: &Path) -> Result<(), String> {
    let storage = worktree_storage_root(app)?;
    fs::create_dir_all(&storage)
        .map_err(|error| format!("Could not create AI worktree storage directory: {error}"))?;
    let canonical_storage = storage
        .canonicalize()
        .map_err(|error| format!("Could not resolve AI worktree storage directory: {error}"))?;
    if !root.starts_with(&canonical_storage) {
        return Err("AI worktree escaped the AtrisBridge-owned worktree storage root.".into());
    }
    Ok(())
}

fn ensure_repo_root(root: &Path) -> Result<(), String> {
    let top = git_stdout(
        root,
        git_safe_read_args(["rev-parse", "--show-toplevel"]),
        MAX_GIT_ERROR_BYTES,
    )?;
    let top = PathBuf::from(top.trim())
        .canonicalize()
        .map_err(|error| format!("Could not resolve Git repository root: {error}"))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    if top != root {
        return Err(
            "AI Git isolation requires the AtrisBridge workspace directory itself to be the Git repository root."
                .into(),
        );
    }
    Ok(())
}

fn ensure_worktree_clean(root: &Path) -> Result<(), String> {
    let status = git_stdout(
        root,
        git_safe_read_args(["status", "--porcelain=v1", "--untracked-files=all"]),
        MAX_GIT_OUTPUT_BYTES,
    )?;
    if !status.trim().is_empty() {
        return Err(
            "Git working tree must be clean before this operation. Commit/stash existing developer changes or use an appropriate direct workflow."
                .into(),
        );
    }
    Ok(())
}

fn head_commit(root: &Path) -> Result<String, String> {
    git_stdout(
        root,
        git_safe_read_args(["rev-parse", "--verify", "HEAD"]),
        MAX_GIT_ERROR_BYTES,
    )
    .map(|value| value.trim().to_string())
}

fn current_branch(root: &Path) -> Result<Option<String>, String> {
    let output = run_git(
        root,
        git_safe_read_args(["symbolic-ref", "--quiet", "--short", "HEAD"]),
        LOCAL_GIT_TIMEOUT,
    )?;
    if !output.success {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn validate_branch_name(root: &Path, branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.len() > 240 || branch.starts_with('-') || branch.contains('\0') {
        return Err("Git branch name is invalid or exceeds the safety bound.".into());
    }
    let output = run_git(
        root,
        git_safe_read_args(["check-ref-format", "--branch", branch]),
        LOCAL_GIT_TIMEOUT,
    )?;
    if !output.success {
        return Err("Git branch name is not a valid branch reference.".into());
    }
    Ok(())
}

fn validate_remote_name(remote: &str) -> Result<(), String> {
    if remote.is_empty()
        || remote.len() > 128
        || remote.starts_with('-')
        || !remote
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"._-".contains(&value))
    {
        return Err("Git remote name contains unsupported characters.".into());
    }
    Ok(())
}

fn validate_commit_id(commit: &str) -> Result<(), String> {
    if !(7..=64).contains(&commit.len()) || !commit.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err("Git revert only accepts an explicit hexadecimal commit ID.".into());
    }
    Ok(())
}

fn validate_commit_message(message: &str) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_COMMIT_MESSAGE_CHARS
        || trimmed.contains('\0')
    {
        return Err("Git commit message is empty or exceeds the safety bound.".into());
    }
    Ok(())
}

fn parse_branch_header(header: &str) -> (Option<String>, u64, u64) {
    let branch = header
        .split("...")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with("HEAD "))
        .map(ToString::to_string);
    let mut ahead = 0;
    let mut behind = 0;
    if let Some(start) = header.find('[') {
        if let Some(end) = header[start..].find(']') {
            for part in header[start + 1..start + end].split(',') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("ahead ") {
                    ahead = value.parse().unwrap_or(0);
                }
                if let Some(value) = part.strip_prefix("behind ") {
                    behind = value.parse().unwrap_or(0);
                }
            }
        }
    }
    (branch, ahead, behind)
}

fn parse_log(text: &str) -> Vec<AiGitCommitEntry> {
    text.split('\x1e')
        .filter_map(|record| {
            let record = record.trim();
            if record.is_empty() {
                return None;
            }
            let fields = record.split('\x1f').collect::<Vec<_>>();
            if fields.len() != 6 {
                return None;
            }
            Some(AiGitCommitEntry {
                commit: fields[0].to_string(),
                parents: fields[1]
                    .split_whitespace()
                    .map(ToString::to_string)
                    .collect(),
                author_name: fields[2].to_string(),
                author_email: fields[3].to_string(),
                authored_at: fields[4].to_string(),
                subject: fields[5].to_string(),
            })
        })
        .collect()
}

fn parse_branches(text: &str) -> Vec<AiGitBranch> {
    text.lines()
        .filter_map(|line| {
            let fields = line.split('\0').collect::<Vec<_>>();
            if fields.len() != 4 {
                return None;
            }
            Some(AiGitBranch {
                name: fields[0].to_string(),
                commit: fields[1].to_string(),
                upstream: (!fields[2].is_empty()).then(|| fields[2].to_string()),
                current: fields[3].trim() == "*",
            })
        })
        .collect()
}

fn git_safe_read_args<const N: usize>(args: [&str; N]) -> Vec<String> {
    let mut safe = vec!["-c".into(), "core.fsmonitor=false".into()];
    safe.extend(args.into_iter().map(ToString::to_string));
    safe
}

fn git_safe_mutation_prefix(app: &AppHandle) -> Result<Vec<String>, String> {
    let hooks = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))?
        .join("git-empty-hooks");
    fs::create_dir_all(&hooks)
        .map_err(|error| format!("Could not create isolated Git hooks directory: {error}"))?;
    Ok(vec![
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        format!("core.hooksPath={}", hooks.to_string_lossy()),
    ])
}

fn git_stdout(root: &Path, args: Vec<String>, max: usize) -> Result<String, String> {
    let output = run_git(root, args, LOCAL_GIT_TIMEOUT)?;
    if !output.success {
        return Err(local_git_error("Git command failed", &output));
    }
    if output.stdout_truncated {
        return Err("Git output exceeded the AtrisBridge safety bound.".into());
    }
    if output.stdout.len() > max {
        return Err("Git output exceeded the requested safety bound.".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_git_checked(
    root: &Path,
    args: Vec<String>,
    timeout: Duration,
    action: &str,
) -> Result<GitOutput, String> {
    let output = run_git(root, args, timeout)?;
    if !output.success {
        return Err(local_git_error(&format!("Could not {action}"), &output));
    }
    Ok(output)
}

fn run_git(root: &Path, args: Vec<String>, timeout: Duration) -> Result<GitOutput, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_PROXY_COMMAND")
        .env_remove("GIT_EXEC_PATH")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT");
    configure_no_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start Git: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not capture Git stdout.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not capture Git stderr.".to_string())?;
    let stdout_thread = capture_pipe(stdout, MAX_GIT_OUTPUT_BYTES);
    let stderr_thread = capture_pipe(stderr, MAX_GIT_ERROR_BYTES);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not observe Git process: {error}"))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(
                "Git operation exceeded the AtrisBridge timeout and was terminated.".into(),
            );
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| "Git stdout capture thread failed.".to_string())?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "Git stderr capture thread failed.".to_string())?;
    Ok(GitOutput {
        success: status.success(),
        code: status.code(),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

fn capture_pipe<R: Read + Send + 'static>(
    mut reader: R,
    max: usize,
) -> thread::JoinHandle<CapturedPipe> {
    thread::spawn(move || {
        let mut capture = TailPreservingBuffer::new(max);
        let mut buffer = [0u8; 8 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => capture.push(&buffer[..read]),
                Err(_) => {
                    capture.mark_truncated();
                    break;
                }
            }
        }
        let (bytes, truncated) = capture.finish();
        CapturedPipe { bytes, truncated }
    })
}

fn truncate_error_detail(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 2 {
        return value
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }
    let tail = value
        .chars()
        .skip(count.saturating_sub(max_chars - 2))
        .collect::<String>();
    format!("… {tail}")
}

fn local_git_error(action: &str, output: &GitOutput) -> String {
    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    detail = truncate_error_detail(&detail, 1_000);
    if output.stderr_truncated || output.stdout_truncated {
        detail.push_str(" [output truncated; tail preserved]");
    }
    if detail.is_empty() {
        format!("{action} (Git exit code {:?}).", output.code)
    } else {
        format!("{action}: {detail}")
    }
}

#[cfg(windows)]
fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn configure_no_window(_command: &mut Command) {}

fn acquire_git(
    coordinator: &WorkspaceMutationCoordinator,
    workspace_id: &str,
    owner: &str,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(
            workspace_id,
            &format!("ai:{owner}"),
            WorkspaceOperationKind::Git,
        )
        .map_err(|error| error.to_string())
}

fn acquire_observe(
    coordinator: &WorkspaceMutationCoordinator,
    session: &AiSession,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(
            &session.workspace_id,
            &format!("ai:{}", session.client_id),
            WorkspaceOperationKind::Observe,
        )
        .map_err(|error| error.to_string())
}

fn record_git_result<T>(
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

fn open_git_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ai_git_worktrees (
                session_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                branch_name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                base_commit TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN (
                    'ready', 'retained', 'removed', 'recovery_required'
                )),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE RESTRICT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_git_worktrees_workspace_status
                ON ai_git_worktrees(workspace_id, status, created_at DESC);

            CREATE TABLE IF NOT EXISTS ai_git_stage_ownership (
                session_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                index_evidence TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY(session_id, relative_path),
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE CASCADE
            );",
        )
        .map_err(|error| format!("Could not initialize AI Git metadata: {error}"))?;

    let mut statement = connection
        .prepare("PRAGMA table_info(ai_git_stage_ownership)")
        .map_err(|error| format!("Could not inspect AI Git stage schema: {error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("Could not query AI Git stage schema: {error}"))?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(|error| format!("Could not read AI Git stage schema: {error}"))?;
    drop(statement);
    if !columns.contains("index_evidence") {
        connection
            .execute(
                "ALTER TABLE ai_git_stage_ownership
                 ADD COLUMN index_evidence TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|error| format!("Could not migrate AI Git stage evidence: {error}"))?;
        connection
            .execute("DELETE FROM ai_git_stage_ownership", [])
            .map_err(|error| format!("Could not clear legacy AI Git stage ownership: {error}"))?;
    }
    Ok(())
}

fn load_worktree_record(
    app: &AppHandle,
    session_id: &str,
) -> Result<Option<StoredWorktree>, String> {
    let connection = open_git_database(app)?;
    connection
        .query_row(
            "SELECT session_id, workspace_id, branch_name, path, base_commit,
                    status, created_at, updated_at
             FROM ai_git_worktrees WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok(StoredWorktree {
                    session_id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    branch_name: row.get(2)?,
                    path: row.get(3)?,
                    base_commit: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Could not read AI worktree metadata: {error}"))
}

fn load_worktree_records(
    app: &AppHandle,
    workspace_id: Option<&str>,
) -> Result<Vec<StoredWorktree>, String> {
    let connection = open_git_database(app)?;
    let sql = if workspace_id.is_some() {
        "SELECT session_id, workspace_id, branch_name, path, base_commit,
                status, created_at, updated_at
         FROM ai_git_worktrees WHERE workspace_id = ?1
         ORDER BY created_at DESC"
    } else {
        "SELECT session_id, workspace_id, branch_name, path, base_commit,
                status, created_at, updated_at
         FROM ai_git_worktrees WHERE ?1 IS NULL
         ORDER BY created_at DESC"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("Could not prepare AI worktree query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id], |row| {
            Ok(StoredWorktree {
                session_id: row.get(0)?,
                workspace_id: row.get(1)?,
                branch_name: row.get(2)?,
                path: row.get(3)?,
                base_commit: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|error| format!("Could not query AI worktrees: {error}"))?;
    let records = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI worktrees: {error}"))?;
    Ok(records)
}

fn set_worktree_status(app: &AppHandle, session_id: &str, status: &str) -> Result<(), String> {
    let connection = open_git_database(app)?;
    connection
        .execute(
            "UPDATE ai_git_worktrees SET status = ?1, updated_at = ?2 WHERE session_id = ?3",
            params![status, Utc::now().to_rfc3339(), session_id],
        )
        .map_err(|error| format!("Could not update AI worktree status: {error}"))?;
    Ok(())
}

fn update_worktree_branch(
    app: &AppHandle,
    session_id: &str,
    branch_name: &str,
) -> Result<(), String> {
    let connection = open_git_database(app)?;
    connection
        .execute(
            "UPDATE ai_git_worktrees
             SET branch_name = ?1, updated_at = ?2
             WHERE session_id = ?3 AND status = 'ready'",
            params![branch_name, Utc::now().to_rfc3339(), session_id],
        )
        .map_err(|error| format!("Could not update AI worktree branch metadata: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_reject_option_injection() {
        assert!(validate_remote_name("origin").is_ok());
        assert!(validate_remote_name("upstream-2").is_ok());
        assert!(validate_remote_name("--upload-pack=evil").is_err());
        assert!(validate_remote_name("origin/other").is_err());
    }

    #[test]
    fn revert_requires_explicit_hex_commit() {
        assert!(validate_commit_id("abcdef1").is_ok());
        assert!(validate_commit_id("HEAD~1").is_err());
        assert!(validate_commit_id("--no-edit").is_err());
    }

    #[test]
    fn branch_header_extracts_ahead_and_behind() {
        let parsed = parse_branch_header("main...origin/main [ahead 2, behind 3]");
        assert_eq!(parsed.0.as_deref(), Some("main"));
        assert_eq!(parsed.1, 2);
        assert_eq!(parsed.2, 3);
    }

    #[test]
    fn log_parser_is_bounded_to_well_formed_records() {
        let parsed =
            parse_log("abc\x1fparent\x1fA\x1fa@example.com\x1f2026-01-01T00:00:00Z\x1fSubject\x1e");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].commit, "abc");
        assert_eq!(parsed[0].parents, vec!["parent"]);
    }

    #[test]
    fn git_output_budgets_fit_remote_relay_headroom() {
        assert_eq!(MAX_GIT_OUTPUT_BYTES, 2 * 1024 * 1024);
        assert_eq!(MAX_GIT_ERROR_BYTES, 64 * 1024);
    }

    #[test]
    fn git_error_detail_preserves_the_tail() {
        let value = format!("{}TAIL", "x".repeat(2_000));
        let truncated = truncate_error_detail(&value, 100);
        assert!(truncated.starts_with("… "));
        assert!(truncated.ends_with("TAIL"));
        assert!(truncated.chars().count() <= 100);
    }
}
