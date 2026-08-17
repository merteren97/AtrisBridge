use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use chrono::Utc;
use rusqlite::params;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::{
    ai_workspace::canonical_workspace_root,
    database::open_database,
    storage::find_workspace,
    workspace_coordinator::{WorkspaceMutationCoordinator, WorkspaceOperationKind},
};

const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const MAX_GIT_TEXT_BYTES: usize = 512 * 1024;

#[derive(Debug)]
struct CleanupCandidate {
    session_id: String,
    workspace_id: String,
    branch_name: String,
    path: String,
    base_commit: String,
}

pub fn setup(app: &AppHandle) {
    let app = app.clone();
    let coordinator = app.state::<WorkspaceMutationCoordinator>().inner().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let cleanup_app = app.clone();
            let cleanup_coordinator = coordinator.clone();
            match tauri::async_runtime::spawn_blocking(move || {
                cleanup_once(&cleanup_app, &cleanup_coordinator)
            })
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("AtrisBridge AI worktree cleanup failed: {error}"),
                Err(error) => eprintln!("AtrisBridge AI worktree cleanup worker failed: {error}"),
            }
            tokio::time::sleep(CLEANUP_INTERVAL).await;
        }
    });
}

fn cleanup_once(
    app: &AppHandle,
    coordinator: &WorkspaceMutationCoordinator,
) -> Result<usize, String> {
    let candidates = load_candidates(app)?;
    let mut cleaned = 0usize;
    for candidate in candidates {
        match cleanup_candidate(app, coordinator, &candidate) {
            Ok(true) => cleaned += 1,
            Ok(false) => {}
            Err(error) => eprintln!(
                "AtrisBridge retained AI worktree {} during cleanup: {}",
                candidate.session_id, error
            ),
        }
    }
    Ok(cleaned)
}

fn load_candidates(app: &AppHandle) -> Result<Vec<CleanupCandidate>, String> {
    let connection = open_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT w.session_id, w.workspace_id, w.branch_name, w.path, w.base_commit
             FROM ai_git_worktrees w
             INNER JOIN ai_sessions s ON s.id = w.session_id
             WHERE s.status <> 'active'
               AND w.status IN ('ready', 'retained', 'recovery_required')
             ORDER BY w.created_at ASC",
        )
        .map_err(|error| format!("Could not prepare stale AI worktree query: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(CleanupCandidate {
                session_id: row.get(0)?,
                workspace_id: row.get(1)?,
                branch_name: row.get(2)?,
                path: row.get(3)?,
                base_commit: row.get(4)?,
            })
        })
        .map_err(|error| format!("Could not query stale AI worktrees: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read stale AI worktrees: {error}"))
}

fn cleanup_candidate(
    app: &AppHandle,
    coordinator: &WorkspaceMutationCoordinator,
    candidate: &CleanupCandidate,
) -> Result<bool, String> {
    let expected_branch = generated_branch_name(&candidate.workspace_id, &candidate.session_id)?;
    if candidate.branch_name != expected_branch {
        return Ok(false);
    }

    let workspace = find_workspace(app, &candidate.workspace_id)?;
    let source_root = canonical_workspace_root(&workspace.local_path)?;
    ensure_repository_root(&source_root)?;

    let _lease = match coordinator.acquire(
        &candidate.workspace_id,
        "ai-lifecycle-cleanup",
        WorkspaceOperationKind::Git,
    ) {
        Ok(lease) => lease,
        Err(_) => return Ok(false),
    };

    let expected_path = expected_worktree_path(app, &candidate.workspace_id, &candidate.session_id)?;
    let recorded_path = PathBuf::from(&candidate.path);
    if recorded_path.exists() {
        let canonical_recorded = recorded_path
            .canonicalize()
            .map_err(|error| format!("Could not resolve stale AI worktree path: {error}"))?;
        if canonical_recorded != expected_path {
            return Ok(false);
        }
        if current_branch(&canonical_recorded)?.as_deref() != Some(expected_branch.as_str()) {
            return Ok(false);
        }
        if !git_status_clean(&canonical_recorded)? {
            return Ok(false);
        }
        if git_head(&canonical_recorded)?.as_deref() != Some(candidate.base_commit.as_str()) {
            return Ok(false);
        }
        remove_worktree(app, &source_root, &canonical_recorded)?;
    } else {
        let recorded_matches_expected = recorded_path == expected_path;
        if !recorded_matches_expected {
            return Ok(false);
        }
        prune_worktrees(app, &source_root)?;
    }

    if branch_checked_out(&source_root, &expected_branch)? {
        return Ok(false);
    }

    match branch_tip(&source_root, &expected_branch)? {
        Some(tip) if tip == candidate.base_commit => {
            delete_branch_ref(app, &source_root, &expected_branch, &candidate.base_commit)?;
        }
        Some(_) => return Ok(false),
        None => {}
    }

    mark_removed(app, &candidate.session_id)?;
    Ok(true)
}

fn generated_branch_name(workspace_id: &str, session_id: &str) -> Result<String, String> {
    Ok(format!(
        "atris/ai/{}/{}",
        short_uuid(workspace_id)?,
        short_uuid(session_id)?
    ))
}

fn short_uuid(value: &str) -> Result<String, String> {
    let uuid = Uuid::parse_str(value)
        .map_err(|_| "AI workspace/session identifier is not a UUID.".to_string())?;
    Ok(uuid.simple().to_string()[..12].to_string())
}

fn expected_worktree_path(
    app: &AppHandle,
    workspace_id: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    let storage = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))?
        .join("ai-worktrees");
    fs::create_dir_all(&storage)
        .map_err(|error| format!("Could not create AI worktree storage directory: {error}"))?;
    let storage = storage
        .canonicalize()
        .map_err(|error| format!("Could not resolve AI worktree storage directory: {error}"))?;
    Ok(storage
        .join(short_uuid(workspace_id)?)
        .join(short_uuid(session_id)?))
}

fn ensure_repository_root(root: &Path) -> Result<(), String> {
    let top = git_text(root, &["-c", "core.fsmonitor=false", "rev-parse", "--show-toplevel"])?
        .ok_or_else(|| "Workspace is no longer a Git repository.".to_string())?;
    let top = PathBuf::from(top.trim())
        .canonicalize()
        .map_err(|error| format!("Could not resolve Git repository root: {error}"))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))?;
    if top != root {
        return Err("AI worktree cleanup requires the workspace itself to remain the Git repository root.".into());
    }
    Ok(())
}

fn current_branch(root: &Path) -> Result<Option<String>, String> {
    git_text(
        root,
        &[
            "-c",
            "core.fsmonitor=false",
            "symbolic-ref",
            "--quiet",
            "--short",
            "HEAD",
        ],
    )
    .map(|value| value.map(|text| text.trim().to_string()))
}

fn git_status_clean(root: &Path) -> Result<bool, String> {
    let status = git_text(
        root,
        &[
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
    )?
    .ok_or_else(|| "Could not inspect stale AI worktree status.".to_string())?;
    Ok(status.trim().is_empty())
}

fn git_head(root: &Path) -> Result<Option<String>, String> {
    git_text(
        root,
        &[
            "-c",
            "core.fsmonitor=false",
            "rev-parse",
            "--verify",
            "HEAD",
        ],
    )
    .map(|value| value.map(|text| text.trim().to_string()))
}

fn branch_tip(root: &Path, branch: &str) -> Result<Option<String>, String> {
    let reference = format!("refs/heads/{branch}");
    git_text(
        root,
        &[
            "-c",
            "core.fsmonitor=false",
            "rev-parse",
            "--verify",
            "--quiet",
            &reference,
        ],
    )
    .map(|value| value.map(|text| text.trim().to_string()))
}

fn branch_checked_out(root: &Path, branch: &str) -> Result<bool, String> {
    let output = git_text(
        root,
        &[
            "-c",
            "core.fsmonitor=false",
            "worktree",
            "list",
            "--porcelain",
        ],
    )?
    .ok_or_else(|| "Could not inspect registered Git worktrees.".to_string())?;
    let expected = format!("branch refs/heads/{branch}");
    Ok(output.lines().any(|line| line.trim() == expected))
}

fn remove_worktree(app: &AppHandle, source_root: &Path, worktree: &Path) -> Result<(), String> {
    let hooks = empty_hooks_path(app)?;
    let worktree = worktree.to_string_lossy().to_string();
    git_mutation(
        source_root,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            &format!("core.hooksPath={}", hooks.to_string_lossy()),
            "worktree",
            "remove",
            &worktree,
        ],
        "remove stale AI worktree",
    )
}

fn prune_worktrees(app: &AppHandle, source_root: &Path) -> Result<(), String> {
    let hooks = empty_hooks_path(app)?;
    git_mutation(
        source_root,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            &format!("core.hooksPath={}", hooks.to_string_lossy()),
            "worktree",
            "prune",
        ],
        "prune stale AI worktrees",
    )
}

fn delete_branch_ref(
    app: &AppHandle,
    source_root: &Path,
    branch: &str,
    expected_tip: &str,
) -> Result<(), String> {
    let hooks = empty_hooks_path(app)?;
    let reference = format!("refs/heads/{branch}");
    git_mutation(
        source_root,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            &format!("core.hooksPath={}", hooks.to_string_lossy()),
            "update-ref",
            "-d",
            &reference,
            expected_tip,
        ],
        "delete stale AI branch ref",
    )
}

fn empty_hooks_path(app: &AppHandle) -> Result<PathBuf, String> {
    let hooks = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve AtrisBridge app-data directory: {error}"))?
        .join("git-empty-hooks");
    fs::create_dir_all(&hooks)
        .map_err(|error| format!("Could not create isolated Git hooks directory: {error}"))?;
    Ok(hooks)
}

fn git_text(root: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let output = git_command(root, args)
        .output()
        .map_err(|error| format!("Could not start Git during AI lifecycle cleanup: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    if output.stdout.len() > MAX_GIT_TEXT_BYTES {
        return Err("Git cleanup output exceeded the AtrisBridge safety bound.".into());
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn git_mutation(root: &Path, args: &[&str], action: &str) -> Result<(), String> {
    let status = git_command(root, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("Could not start Git while trying to {action}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Could not {action}; Git rejected the guarded cleanup operation."))
    }
}

fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
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
    command
}

fn mark_removed(app: &AppHandle, session_id: &str) -> Result<(), String> {
    let mut connection = open_database(app)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start AI lifecycle cleanup transaction: {error}"))?;
    transaction
        .execute(
            "DELETE FROM ai_git_stage_ownership WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|error| format!("Could not clear stale AI Git stage ownership: {error}"))?;
    transaction
        .execute(
            "UPDATE ai_git_worktrees
             SET status = 'removed', updated_at = ?1
             WHERE session_id = ?2 AND status <> 'removed'",
            params![Utc::now().to_rfc3339(), session_id],
        )
        .map_err(|error| format!("Could not mark stale AI worktree removed: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit AI lifecycle cleanup transaction: {error}"))
}

#[cfg(windows)]
fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn configure_no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_branch_is_scoped_to_workspace_and_session() {
        assert_eq!(
            generated_branch_name(
                "d7db14a8-d612-4f2e-9a6d-1234567890ab",
                "06c04de3-bd33-4d20-8c91-1234567890ab"
            )
            .expect("branch"),
            "atris/ai/d7db14a8d612/06c04de3bd33"
        );
    }

    #[test]
    fn invalid_identifiers_are_never_cleanup_candidates() {
        assert!(generated_branch_name("workspace", "session").is_err());
    }
}
