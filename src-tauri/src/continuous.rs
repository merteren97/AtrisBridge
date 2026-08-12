use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::{
    commands,
    database::open_database,
    encryption,
    models::{RemoteInventoryReport, ScanReport, SyncMode},
    provider_sessions::ProviderSessionStore,
    provider_storage, restore, scanner,
    storage::{find_workspace, get_journal_summary, record_scan},
    sync,
    transport::rclone,
    workspace_coordinator::{
        WorkspaceLeaseError, WorkspaceMutationCoordinator, WorkspaceOperationKind,
    },
};

const LOCAL_DEBOUNCE: Duration = Duration::from_millis(1800);
const SCHEDULER_TICK: Duration = Duration::from_millis(250);
const POLL_DISCOVERY_TICK: Duration = Duration::from_secs(5);
const DEFAULT_REMOTE_POLL_SECONDS: u64 = 60;
const MIN_REMOTE_POLL_SECONDS: u64 = 30;
const MAX_REMOTE_POLL_SECONDS: u64 = 3600;
const MAX_FAILURE_BACKOFF_MULTIPLIER: u64 = 8;
const COORDINATOR_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuousSyncStatus {
    pub workspace_id: String,
    pub enabled: bool,
    pub runtime_active: bool,
    pub auto_apply_safe: bool,
    pub remote_poll_seconds: u64,
    pub state: String,
    pub last_reason: Option<String>,
    pub last_event_at: Option<String>,
    pub last_cycle_started_at: Option<String>,
    pub last_cycle_completed_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_message: Option<String>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone)]
struct StoredContinuousSettings {
    workspace_id: String,
    enabled: bool,
    auto_apply_safe: bool,
    remote_poll_seconds: u64,
    state: String,
    last_reason: Option<String>,
    last_event_at: Option<String>,
    last_cycle_started_at: Option<String>,
    last_cycle_completed_at: Option<String>,
    last_success_at: Option<String>,
    last_message: Option<String>,
    consecutive_failures: u64,
    last_evidence_signature: Option<String>,
    last_decision: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingCycle {
    due: Instant,
    reason: String,
}

#[derive(Debug, Clone)]
struct CycleRetry {
    reason: String,
    delay: Duration,
}

#[derive(Debug)]
enum SchedulerMessage {
    Dirty {
        workspace_id: String,
        reason: String,
        delay: Duration,
        record_event: bool,
    },
    Finished {
        workspace_id: String,
        retry: Option<CycleRetry>,
    },
    Cancel {
        workspace_id: String,
    },
    WatchError {
        workspace_id: String,
        message: String,
    },
}

pub struct ContinuousSyncManager {
    sender: Sender<SchedulerMessage>,
    receiver: Mutex<Option<Receiver<SchedulerMessage>>>,
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
    scheduler_started: AtomicBool,
}

impl Default for ContinuousSyncManager {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            watchers: Mutex::new(HashMap::new()),
            scheduler_started: AtomicBool::new(false),
        }
    }
}

impl ContinuousSyncManager {
    fn start_scheduler(&self, app: AppHandle) -> Result<(), String> {
        if self.scheduler_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let receiver = self
            .receiver
            .lock()
            .map_err(|_| "Continuous scheduler receiver lock is poisoned.".to_string())?
            .take()
            .ok_or_else(|| "Continuous scheduler receiver is unavailable.".to_string())?;
        let sender = self.sender.clone();
        thread::Builder::new()
            .name("atrisbridge-continuous-scheduler".into())
            .spawn(move || scheduler_loop(app, receiver, sender))
            .map_err(|error| format!("Could not start continuous scheduler: {error}"))?;
        Ok(())
    }

    fn start_workspace(&self, app: &AppHandle, workspace_id: &str) -> Result<(), String> {
        {
            let watchers = self
                .watchers
                .lock()
                .map_err(|_| "Continuous watcher lock is poisoned.".to_string())?;
            if watchers.contains_key(workspace_id) {
                return Ok(());
            }
        }

        let workspace = find_workspace(app, workspace_id)?;
        let root = PathBuf::from(&workspace.local_path);
        if !root.is_dir() {
            return Err("Workspace directory no longer exists or is not accessible.".into());
        }

        let sender = self.sender.clone();
        let callback_workspace_id = workspace_id.to_string();
        let callback_root = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<Event>| match result {
                Ok(event) => {
                    if event_is_relevant(&callback_root, &event) {
                        let _ = sender.send(SchedulerMessage::Dirty {
                            workspace_id: callback_workspace_id.clone(),
                            reason: "local_change".into(),
                            delay: LOCAL_DEBOUNCE,
                            record_event: true,
                        });
                    }
                }
                Err(error) => {
                    let _ = sender.send(SchedulerMessage::WatchError {
                        workspace_id: callback_workspace_id.clone(),
                        message: format!("Filesystem watcher reported an error: {error}"),
                    });
                }
            })
            .map_err(|error| format!("Could not create filesystem watcher: {error}"))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| format!("Could not watch {}: {error}", root.display()))?;
        self.watchers
            .lock()
            .map_err(|_| "Continuous watcher lock is poisoned.".to_string())?
            .insert(workspace_id.to_string(), watcher);
        Ok(())
    }

    pub fn stop_workspace(&self, workspace_id: &str) -> Result<(), String> {
        self.watchers
            .lock()
            .map_err(|_| "Continuous watcher lock is poisoned.".to_string())?
            .remove(workspace_id);
        let _ = self.sender.send(SchedulerMessage::Cancel {
            workspace_id: workspace_id.to_string(),
        });
        Ok(())
    }

    fn schedule(&self, workspace_id: &str, reason: &str, delay: Duration) -> Result<(), String> {
        self.sender
            .send(SchedulerMessage::Dirty {
                workspace_id: workspace_id.to_string(),
                reason: reason.to_string(),
                delay,
                record_event: false,
            })
            .map_err(|_| "Continuous scheduler is unavailable.".to_string())
    }

    fn is_watching(&self, workspace_id: &str) -> bool {
        self.watchers
            .lock()
            .map(|watchers| watchers.contains_key(workspace_id))
            .unwrap_or(false)
    }
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = CASE WHEN enabled = 1 THEN 'idle' ELSE 'disabled' END,
                 last_message = CASE
                    WHEN enabled = 1 AND state IN ('running', 'debouncing')
                    THEN 'AtrisBridge restarted. Interrupted transfers were recovered before watch mode resumed.'
                    ELSE last_message
                 END,
                 updated_at = ?1
             WHERE state IN ('running', 'debouncing')",
            params![Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("Could not normalize continuous sync state: {error}"))?;
    drop(connection);

    let manager = app.state::<ContinuousSyncManager>();
    manager.start_scheduler(app.clone())?;
    for workspace_id in enabled_workspace_ids(app)? {
        match manager.start_workspace(app, &workspace_id) {
            Ok(()) => {
                let _ = manager.schedule(&workspace_id, "startup", Duration::from_secs(3));
            }
            Err(error) => {
                record_runtime_error(
                    app,
                    &workspace_id,
                    &format!("Watch mode could not resume: {error}. Periodic reconciliation remains available."),
                )?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn continuous_sync_status(
    app: AppHandle,
    id: String,
    manager: State<'_, ContinuousSyncManager>,
) -> Result<ContinuousSyncStatus, String> {
    find_workspace(&app, &id)?;
    let stored = ensure_settings_row(&app, &id)?;
    Ok(public_status(stored, manager.is_watching(&id)))
}

#[tauri::command]
pub fn set_continuous_sync_enabled(
    app: AppHandle,
    id: String,
    enabled: bool,
    manager: State<'_, ContinuousSyncManager>,
    sessions: State<'_, ProviderSessionStore>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<ContinuousSyncStatus, String> {
    let _lease = coordinator
        .acquire(
            &id,
            "desktop-watch-control",
            WorkspaceOperationKind::Configure,
        )
        .map_err(|error| error.to_string())?;
    let workspace = find_workspace(&app, &id)?;
    let root = PathBuf::from(&workspace.local_path);
    if !enabled {
        let stored = ensure_settings_row(&app, &id)?;
        if stored.state == "running" {
            return Err(
                "A guarded continuous reconciliation is currently running. Wait for it to finish before pausing watch mode."
                    .into(),
            );
        }
    }
    if enabled {
        if !root.is_dir() {
            return Err("Workspace directory no longer exists or is not accessible.".into());
        }
        let runtime = rclone::status(&app);
        if !runtime.available {
            return Err(runtime
                .message
                .unwrap_or_else(|| "The pinned rclone runtime is not available.".into()));
        }
        let (provider, _) = provider_storage::get_provider_for_workspace(&app, &id)?;
        if sessions.google_drive_token(&provider.id)?.is_none() {
            return Err(
                "A protected Google Drive credential is required before watch mode can start."
                    .into(),
            );
        }
        let encryption_status = encryption::status(&app, &id)?;
        if encryption_status.mode == "content" && !encryption_status.key_available {
            return Err(
                "This encrypted workspace requires its recovery key before watch mode can start."
                    .into(),
            );
        }
    }

    set_enabled_flag(&app, &id, enabled)?;
    if enabled {
        if let Err(error) = manager.start_workspace(&app, &id) {
            set_enabled_flag(&app, &id, false)?;
            return Err(error);
        }
        manager.schedule(&id, "enabled", Duration::from_millis(350))?;
    } else {
        manager.stop_workspace(&id)?;
        set_disabled_state(&app, &id)?;
    }

    let stored = ensure_settings_row(&app, &id)?;
    Ok(public_status(stored, manager.is_watching(&id)))
}

#[tauri::command]
pub fn update_continuous_sync_settings(
    app: AppHandle,
    id: String,
    auto_apply_safe: bool,
    remote_poll_seconds: u64,
    manager: State<'_, ContinuousSyncManager>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<ContinuousSyncStatus, String> {
    let _lease = coordinator
        .acquire(
            &id,
            "desktop-watch-control",
            WorkspaceOperationKind::Configure,
        )
        .map_err(|error| error.to_string())?;
    find_workspace(&app, &id)?;
    validate_poll_seconds(remote_poll_seconds)?;
    ensure_settings_row(&app, &id)?;
    let connection = open_continuous_database(&app)?;
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET auto_apply_safe = ?1, remote_poll_seconds = ?2, updated_at = ?3
             WHERE workspace_id = ?4",
            params![
                if auto_apply_safe { 1 } else { 0 },
                to_i64(remote_poll_seconds, "remote poll interval")?,
                Utc::now().to_rfc3339(),
                id,
            ],
        )
        .map_err(|error| format!("Could not update continuous sync settings: {error}"))?;
    let stored = ensure_settings_row(&app, &id)?;
    if stored.enabled {
        manager.schedule(&id, "settings_changed", Duration::ZERO)?;
    }
    Ok(public_status(stored, manager.is_watching(&id)))
}

#[tauri::command]
pub fn run_continuous_sync_now(
    app: AppHandle,
    id: String,
    manager: State<'_, ContinuousSyncManager>,
) -> Result<ContinuousSyncStatus, String> {
    let stored = ensure_settings_row(&app, &id)?;
    if !stored.enabled {
        return Err(
            "Enable watch mode before requesting an automatic reconciliation cycle.".into(),
        );
    }
    mark_debouncing(&app, &id, "manual", false)?;
    manager.schedule(&id, "manual", Duration::ZERO)?;
    let stored = ensure_settings_row(&app, &id)?;
    Ok(public_status(stored, manager.is_watching(&id)))
}

pub fn is_enabled(app: &AppHandle, workspace_id: &str) -> Result<bool, String> {
    let connection = open_continuous_database(app)?;
    connection
        .query_row(
            "SELECT enabled FROM continuous_sync_settings WHERE workspace_id = ?1",
            params![workspace_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.unwrap_or(0) != 0)
        .map_err(|error| format!("Could not inspect continuous sync state: {error}"))
}

fn scheduler_loop(
    app: AppHandle,
    receiver: Receiver<SchedulerMessage>,
    sender: Sender<SchedulerMessage>,
) {
    let mut pending = HashMap::<String, PendingCycle>::new();
    let mut in_flight = HashSet::<String>::new();
    let mut last_poll_discovery = Instant::now() - POLL_DISCOVERY_TICK;

    loop {
        match receiver.recv_timeout(SCHEDULER_TICK) {
            Ok(message) => match message {
                SchedulerMessage::Dirty {
                    workspace_id,
                    reason,
                    delay,
                    record_event,
                } => {
                    if let Ok(true) = is_enabled(&app, &workspace_id) {
                        let _ = mark_debouncing(&app, &workspace_id, &reason, record_event);
                        let due = Instant::now() + delay;
                        pending
                            .entry(workspace_id)
                            .and_modify(|entry| {
                                if due > entry.due {
                                    entry.due = due;
                                }
                                entry.reason = reason.clone();
                            })
                            .or_insert(PendingCycle { due, reason });
                    }
                }
                SchedulerMessage::Finished {
                    workspace_id,
                    retry,
                } => {
                    in_flight.remove(&workspace_id);
                    if let Some(retry) = retry {
                        if matches!(is_enabled(&app, &workspace_id), Ok(true)) {
                            let due = Instant::now() + retry.delay;
                            pending
                                .entry(workspace_id)
                                .and_modify(|entry| {
                                    if due > entry.due {
                                        entry.due = due;
                                    }
                                })
                                .or_insert(PendingCycle {
                                    due,
                                    reason: retry.reason,
                                });
                        }
                    }
                }
                SchedulerMessage::Cancel { workspace_id } => {
                    pending.remove(&workspace_id);
                }
                SchedulerMessage::WatchError {
                    workspace_id,
                    message,
                } => {
                    let _ = record_runtime_error(&app, &workspace_id, &message);
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_poll_discovery.elapsed() >= POLL_DISCOVERY_TICK {
            last_poll_discovery = Instant::now();
            if let Ok(due_ids) = due_remote_poll_workspaces(&app) {
                for workspace_id in due_ids {
                    if !in_flight.contains(&workspace_id) && !pending.contains_key(&workspace_id) {
                        pending.insert(
                            workspace_id,
                            PendingCycle {
                                due: Instant::now(),
                                reason: "remote_poll".into(),
                            },
                        );
                    }
                }
            }
        }

        let now = Instant::now();
        let ready = pending
            .iter()
            .filter(|(workspace_id, cycle)| cycle.due <= now && !in_flight.contains(*workspace_id))
            .map(|(workspace_id, cycle)| (workspace_id.clone(), cycle.reason.clone()))
            .collect::<Vec<_>>();

        for (workspace_id, reason) in ready {
            pending.remove(&workspace_id);
            if !matches!(is_enabled(&app, &workspace_id), Ok(true)) {
                continue;
            }
            in_flight.insert(workspace_id.clone());
            let cycle_app = app.clone();
            let completion_sender = sender.clone();
            tauri::async_runtime::spawn(async move {
                let retry = run_cycle(cycle_app, workspace_id.clone(), reason).await;
                let _ = completion_sender.send(SchedulerMessage::Finished {
                    workspace_id,
                    retry,
                });
            });
        }
    }
}

async fn run_cycle(app: AppHandle, workspace_id: String, reason: String) -> Option<CycleRetry> {
    let previous = match ensure_settings_row(&app, &workspace_id) {
        Ok(settings) => settings,
        Err(_) => return None,
    };

    let coordinator = app.state::<WorkspaceMutationCoordinator>();
    let _lease = match coordinator.acquire(
        &workspace_id,
        "continuous-watch",
        WorkspaceOperationKind::Continuous,
    ) {
        Ok(lease) => lease,
        Err(WorkspaceLeaseError::Busy(active)) => {
            let message = format!(
                "Workspace is busy with '{}' owned by '{}'. Continuous reconciliation is queued and will retry automatically.",
                active.kind, active.owner
            );
            let _ = record_cycle_deferred(&app, &workspace_id, &reason, &message);
            return Some(CycleRetry {
                reason,
                delay: COORDINATOR_RETRY_DELAY,
            });
        }
        Err(error) => {
            let _ = record_cycle_error(&app, &workspace_id, &reason, &error.to_string());
            return None;
        }
    };

    if let Err(error) = mark_cycle_running(&app, &workspace_id, &reason) {
        let _ = record_cycle_error(&app, &workspace_id, &reason, &error);
        return None;
    }

    let result = run_cycle_inner(&app, &workspace_id, &reason, &previous).await;
    match result {
        Ok(outcome) => {
            let _ = record_cycle_outcome(&app, &workspace_id, &reason, outcome);
        }
        Err(error) => {
            let _ = record_cycle_error(&app, &workspace_id, &reason, &error);
        }
    }
    None
}

struct CycleOutcome {
    state: &'static str,
    decision: &'static str,
    message: String,
    evidence_signature: Option<String>,
    successful: bool,
}

async fn run_cycle_inner(
    app: &AppHandle,
    workspace_id: &str,
    _reason: &str,
    previous: &StoredContinuousSettings,
) -> Result<CycleOutcome, String> {
    if has_running_transfer(app, workspace_id)? {
        return Ok(CycleOutcome {
            state: "idle",
            decision: "deferred",
            message: "Another transfer is already running. Continuous reconciliation was deferred."
                .into(),
            evidence_signature: previous.last_evidence_signature.clone(),
            successful: false,
        });
    }

    strict_refresh_local_inventory(app, workspace_id).await?;
    let sessions = app.state::<ProviderSessionStore>();
    let _remote: RemoteInventoryReport =
        commands::scan_remote_inventory(app.clone(), workspace_id.to_string(), sessions).await?;
    let summary = get_journal_summary(app, workspace_id)?;
    let signature = changed_evidence_signature(app, workspace_id)?;

    if signature.is_none() || summary.changed_files == 0 {
        return Ok(CycleOutcome {
            state: "idle",
            decision: "noop",
            message: "Inventories reconciled; no synchronization changes are pending.".into(),
            evidence_signature: signature,
            successful: true,
        });
    }

    if previous.last_evidence_signature == signature
        && matches!(
            previous.last_decision.as_deref(),
            Some("noop" | "attention")
        )
    {
        let state = if previous.last_decision.as_deref() == Some("attention")
            || previous.last_decision.as_deref() == Some("review")
        {
            "attention"
        } else {
            "idle"
        };
        let decision = match previous.last_decision.as_deref() {
            Some("attention") => "attention",
            Some("review") => "review",
            _ => "noop",
        };
        return Ok(CycleOutcome {
            state,
            decision,
            message: previous.last_message.clone().unwrap_or_else(|| {
                "Evidence is unchanged since the previous continuous cycle.".into()
            }),
            evidence_signature: signature,
            successful: state == "idle",
        });
    }

    let workspace = find_workspace(app, workspace_id)?;
    match workspace.sync_mode {
        SyncMode::Backup => {
            run_backup_cycle(app, workspace_id, previous.auto_apply_safe, signature).await
        }
        SyncMode::Pull => {
            run_restore_cycle(app, workspace_id, previous.auto_apply_safe, signature).await
        }
        SyncMode::TwoWay => {
            run_two_way_cycle(app, workspace_id, previous.auto_apply_safe, signature).await
        }
    }
}

async fn run_backup_cycle(
    app: &AppHandle,
    workspace_id: &str,
    auto_apply_safe: bool,
    signature: Option<String>,
) -> Result<CycleOutcome, String> {
    let sessions = app.state::<ProviderSessionStore>();
    let plan =
        commands::prepare_backup_plan(app.clone(), workspace_id.to_string(), sessions).await?;
    ensure_plan_scan_is_complete(app, workspace_id, &plan.local_scan_at)?;
    match backup_policy(plan.upload_count, plan.blocked_count, auto_apply_safe) {
        AutomaticPolicy::Idle(message) => Ok(CycleOutcome {
            state: "idle",
            decision: "noop",
            message,
            evidence_signature: signature,
            successful: true,
        }),
        AutomaticPolicy::Review(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "review",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Attention(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "attention",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Apply => {
            let sessions = app.state::<ProviderSessionStore>();
            let report = commands::execute_backup_plan(app.clone(), plan.id, sessions).await?;
            if report.failed_count > 0 || report.status != "completed" {
                return Ok(CycleOutcome {
                    state: "attention",
                    decision: "attention",
                    message: format!(
                        "Automatic backup did not converge cleanly: {} completed, {} failed. Pause watch mode and inspect the latest plan.",
                        report.completed_count, report.failed_count
                    ),
                    evidence_signature: signature,
                    successful: false,
                });
            }
            Ok(CycleOutcome {
                state: "idle",
                decision: "applied",
                message: format!(
                    "Automatically uploaded {} safe file change{}.",
                    report.completed_count,
                    if report.completed_count == 1 { "" } else { "s" }
                ),
                evidence_signature: None,
                successful: true,
            })
        }
    }
}

async fn run_restore_cycle(
    app: &AppHandle,
    workspace_id: &str,
    auto_apply_safe: bool,
    signature: Option<String>,
) -> Result<CycleOutcome, String> {
    let sessions = app.state::<ProviderSessionStore>();
    let plan =
        restore::prepare_restore_plan(app.clone(), workspace_id.to_string(), sessions).await?;
    ensure_plan_scan_is_complete(app, workspace_id, &plan.local_scan_at)?;
    match restore_policy(plan.restore_count, plan.blocked_count, auto_apply_safe) {
        AutomaticPolicy::Idle(message) => Ok(CycleOutcome {
            state: "idle",
            decision: "noop",
            message,
            evidence_signature: signature,
            successful: true,
        }),
        AutomaticPolicy::Review(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "review",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Attention(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "attention",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Apply => {
            let sessions = app.state::<ProviderSessionStore>();
            let report = restore::execute_restore_plan(app.clone(), plan.id, sessions).await?;
            if report.failed_count > 0 || report.status != "completed" {
                return Ok(CycleOutcome {
                    state: "attention",
                    decision: "attention",
                    message: format!(
                        "Automatic pull did not converge cleanly: {} completed, {} failed. Pause watch mode and inspect the latest plan.",
                        report.completed_count, report.failed_count
                    ),
                    evidence_signature: signature,
                    successful: false,
                });
            }
            Ok(CycleOutcome {
                state: "idle",
                decision: "applied",
                message: format!(
                    "Automatically restored {} safe file change{}.",
                    report.completed_count,
                    if report.completed_count == 1 { "" } else { "s" }
                ),
                evidence_signature: None,
                successful: true,
            })
        }
    }
}

async fn run_two_way_cycle(
    app: &AppHandle,
    workspace_id: &str,
    auto_apply_safe: bool,
    signature: Option<String>,
) -> Result<CycleOutcome, String> {
    let sessions = app.state::<ProviderSessionStore>();
    let plan = sync::prepare_sync_plan(app.clone(), workspace_id.to_string(), sessions).await?;
    ensure_plan_scan_is_complete(app, workspace_id, &plan.local_scan_at)?;
    match two_way_policy(
        plan.upload_count,
        plan.download_count,
        plan.delete_count,
        plan.conflict_count,
        plan.blocked_count,
        auto_apply_safe,
    ) {
        AutomaticPolicy::Idle(message) => Ok(CycleOutcome {
            state: "idle",
            decision: "noop",
            message,
            evidence_signature: signature,
            successful: true,
        }),
        AutomaticPolicy::Review(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "review",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Attention(message) => Ok(CycleOutcome {
            state: "attention",
            decision: "attention",
            message,
            evidence_signature: signature,
            successful: false,
        }),
        AutomaticPolicy::Apply => {
            let sessions = app.state::<ProviderSessionStore>();
            let report = sync::execute_sync_plan(app.clone(), plan.id, sessions).await?;
            if report.failed_count > 0 || report.status != "completed" {
                return Ok(CycleOutcome {
                    state: "attention",
                    decision: "attention",
                    message: format!(
                        "Automatic two-way sync did not converge cleanly: {} completed, {} failed. Pause watch mode and inspect the latest plan.",
                        report.completed_count, report.failed_count
                    ),
                    evidence_signature: signature,
                    successful: false,
                });
            }
            Ok(CycleOutcome {
                state: "idle",
                decision: "applied",
                message: format!(
                    "Automatically synchronized {} safe file change{}.",
                    report.completed_count,
                    if report.completed_count == 1 { "" } else { "s" }
                ),
                evidence_signature: None,
                successful: true,
            })
        }
    }
}

enum AutomaticPolicy {
    Idle(String),
    Review(String),
    Attention(String),
    Apply,
}

fn backup_policy(upload_count: u64, blocked_count: u64, auto_apply_safe: bool) -> AutomaticPolicy {
    if blocked_count > 0 {
        return AutomaticPolicy::Attention(format!(
            "Backup plan contains {blocked_count} blocked path(s). Pause watch mode and review the latest plan."
        ));
    }
    if upload_count == 0 {
        return AutomaticPolicy::Idle(
            "No backup actions are required for the current evidence.".into(),
        );
    }
    if !auto_apply_safe {
        return AutomaticPolicy::Review(format!(
            "{upload_count} safe upload(s) are ready for review because automatic safe apply is disabled."
        ));
    }
    AutomaticPolicy::Apply
}

fn restore_policy(
    restore_count: u64,
    blocked_count: u64,
    auto_apply_safe: bool,
) -> AutomaticPolicy {
    if blocked_count > 0 {
        return AutomaticPolicy::Attention(format!(
            "Pull plan contains {blocked_count} blocked path(s). Pause watch mode and review the latest plan."
        ));
    }
    if restore_count == 0 {
        return AutomaticPolicy::Idle(
            "No pull actions are required for the current evidence.".into(),
        );
    }
    if !auto_apply_safe {
        return AutomaticPolicy::Review(format!(
            "{restore_count} safe download(s) are ready for review because automatic safe apply is disabled."
        ));
    }
    AutomaticPolicy::Apply
}

fn two_way_policy(
    upload_count: u64,
    download_count: u64,
    delete_count: u64,
    conflict_count: u64,
    blocked_count: u64,
    auto_apply_safe: bool,
) -> AutomaticPolicy {
    if conflict_count > 0 || blocked_count > 0 || delete_count > 0 {
        return AutomaticPolicy::Attention(format!(
            "Two-Way plan requires review: {conflict_count} conflict(s), {blocked_count} blocked path(s), and {delete_count} deletion action(s). Deletions are never auto-applied in Phase 8."
        ));
    }
    let safe_count = upload_count.saturating_add(download_count);
    if safe_count == 0 {
        return AutomaticPolicy::Idle(
            "No two-way actions are required for the current evidence.".into(),
        );
    }
    if !auto_apply_safe {
        return AutomaticPolicy::Review(format!(
            "{safe_count} safe transfer(s) are ready for review because automatic safe apply is disabled."
        ));
    }
    AutomaticPolicy::Apply
}

async fn strict_refresh_local_inventory(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<ScanReport, String> {
    let workspace = find_workspace(app, workspace_id)?;
    let root = PathBuf::from(workspace.local_path);
    let id = workspace_id.to_string();
    let outcome = tauri::async_runtime::spawn_blocking(move || scanner::scan(&id, &root))
        .await
        .map_err(|error| format!("Continuous local scan worker failed: {error}"))??;
    ensure_scan_is_complete(&outcome.report)?;
    record_scan(app, &outcome.report, &outcome.inventory)?;
    Ok(outcome.report)
}

pub(crate) fn ensure_scan_is_complete(report: &ScanReport) -> Result<(), String> {
    if report.warnings.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Local inventory is incomplete ({} warning{}). AtrisBridge will not plan synchronization from uncertain filesystem evidence. Resolve the scan warnings and retry.",
        report.warnings.len(),
        if report.warnings.len() == 1 { "" } else { "s" }
    ))
}

fn ensure_plan_scan_is_complete(
    app: &AppHandle,
    workspace_id: &str,
    scanned_at: &str,
) -> Result<(), String> {
    let connection = open_database(app)?;
    let warnings_json = connection
        .query_row(
            "SELECT warnings_json FROM scan_runs
             WHERE workspace_id = ?1 AND scanned_at = ?2
             ORDER BY id DESC LIMIT 1",
            params![workspace_id, scanned_at],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not inspect planner scan evidence: {error}"))?
        .ok_or_else(|| {
            "The planner local scan journal is missing. Automatic execution was blocked."
                .to_string()
        })?;
    let warnings: Vec<String> = serde_json::from_str(&warnings_json)
        .map_err(|error| format!("Planner scan warning evidence is invalid: {error}"))?;
    if warnings.is_empty() {
        return Ok(());
    }
    Err(format!(
        "The planner local scan completed with {} warning{}. Automatic execution was blocked because local evidence is incomplete.",
        warnings.len(),
        if warnings.len() == 1 { "" } else { "s" }
    ))
}

fn changed_evidence_signature(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<Option<String>, String> {
    let connection = open_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT relative_path, state,
                    local_present, local_hash,
                    remote_present, remote_id, remote_checksum_type, remote_checksum,
                    last_synced_hash, last_synced_remote_checksum_type, last_synced_remote_checksum
             FROM file_entries
             WHERE workspace_id = ?1
               AND state NOT IN ('synced', 'removed_before_sync')
             ORDER BY relative_path ASC",
        )
        .map_err(|error| format!("Could not prepare continuous evidence query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(|error| format!("Could not query continuous evidence: {error}"))?;

    let values = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read continuous evidence: {error}"))?;
    if values.is_empty() {
        return Ok(None);
    }

    let mut hasher = blake3::Hasher::new();
    for value in values {
        for field in [
            Some(value.0),
            Some(value.1),
            Some(value.2.to_string()),
            value.3,
            Some(value.4.to_string()),
            value.5,
            value.6,
            value.7,
            value.8,
            value.9,
            value.10,
        ] {
            if let Some(field) = field {
                hasher.update(field.as_bytes());
            }
            hasher.update(&[0]);
        }
        hasher.update(&[0xff]);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

fn event_is_relevant(root: &Path, event: &Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    if event.paths.is_empty() {
        return true;
    }
    event.paths.iter().any(|path| {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        if relative.as_os_str().is_empty() {
            return true;
        }
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        match scanner::is_path_ignored_for_sync(root, &relative) {
            Ok(ignored) => !ignored,
            Err(_) => true,
        }
    })
}

fn has_running_transfer(app: &AppHandle, workspace_id: &str) -> Result<bool, String> {
    let connection = open_database(app)?;
    for table in ["backup_plans", "restore_plans", "sync_plans"] {
        if !table_exists(&connection, table)? {
            continue;
        }
        let sql =
            format!("SELECT COUNT(*) FROM {table} WHERE workspace_id = ?1 AND status = 'running'");
        let count: i64 = connection
            .query_row(&sql, params![workspace_id], |row| row.get(0))
            .map_err(|error| format!("Could not inspect active transfer state: {error}"))?;
        if count > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|error| format!("Could not inspect continuous sync schema: {error}"))
}

fn open_continuous_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS continuous_sync_settings (
                workspace_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
                auto_apply_safe INTEGER NOT NULL DEFAULT 0 CHECK(auto_apply_safe IN (0,1)),
                remote_poll_seconds INTEGER NOT NULL DEFAULT 60 CHECK(remote_poll_seconds BETWEEN 30 AND 3600),
                state TEXT NOT NULL DEFAULT 'disabled' CHECK(state IN (
                    'disabled', 'idle', 'debouncing', 'running', 'attention', 'error'
                )),
                last_reason TEXT,
                last_event_at TEXT,
                last_cycle_started_at TEXT,
                last_cycle_completed_at TEXT,
                last_success_at TEXT,
                last_message TEXT,
                consecutive_failures INTEGER NOT NULL DEFAULT 0,
                last_evidence_signature TEXT,
                last_decision TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_continuous_sync_enabled
                ON continuous_sync_settings(enabled, state);",
        )
        .map_err(|error| format!("Could not initialize continuous sync metadata: {error}"))
}

fn ensure_settings_row(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<StoredContinuousSettings, String> {
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT OR IGNORE INTO continuous_sync_settings (
                workspace_id, enabled, auto_apply_safe, remote_poll_seconds,
                state, created_at, updated_at
             ) VALUES (?1, 0, 0, ?2, 'disabled', ?3, ?3)",
            params![
                workspace_id,
                to_i64(DEFAULT_REMOTE_POLL_SECONDS, "default poll interval")?,
                now
            ],
        )
        .map_err(|error| format!("Could not initialize workspace watch settings: {error}"))?;
    load_settings(&connection, workspace_id)
}

fn load_settings(
    connection: &Connection,
    workspace_id: &str,
) -> Result<StoredContinuousSettings, String> {
    connection
        .query_row(
            "SELECT workspace_id, enabled, auto_apply_safe, remote_poll_seconds, state,
                    last_reason, last_event_at, last_cycle_started_at, last_cycle_completed_at,
                    last_success_at, last_message, consecutive_failures,
                    last_evidence_signature, last_decision
             FROM continuous_sync_settings WHERE workspace_id = ?1",
            params![workspace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read continuous sync settings: {error}"))?
        .ok_or_else(|| "Continuous sync settings were not found.".to_string())
        .and_then(|row| {
            Ok(StoredContinuousSettings {
                workspace_id: row.0,
                enabled: row.1 != 0,
                auto_apply_safe: row.2 != 0,
                remote_poll_seconds: from_i64(row.3, "remote poll interval")?,
                state: row.4,
                last_reason: row.5,
                last_event_at: row.6,
                last_cycle_started_at: row.7,
                last_cycle_completed_at: row.8,
                last_success_at: row.9,
                last_message: row.10,
                consecutive_failures: from_i64(row.11, "continuous failure count")?,
                last_evidence_signature: row.12,
                last_decision: row.13,
            })
        })
}

fn public_status(stored: StoredContinuousSettings, runtime_active: bool) -> ContinuousSyncStatus {
    ContinuousSyncStatus {
        workspace_id: stored.workspace_id,
        enabled: stored.enabled,
        runtime_active,
        auto_apply_safe: stored.auto_apply_safe,
        remote_poll_seconds: stored.remote_poll_seconds,
        state: stored.state,
        last_reason: stored.last_reason,
        last_event_at: stored.last_event_at,
        last_cycle_started_at: stored.last_cycle_started_at,
        last_cycle_completed_at: stored.last_cycle_completed_at,
        last_success_at: stored.last_success_at,
        last_message: stored.last_message,
        consecutive_failures: stored.consecutive_failures,
    }
}

fn set_enabled_flag(app: &AppHandle, workspace_id: &str, enabled: bool) -> Result<(), String> {
    ensure_settings_row(app, workspace_id)?;
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET enabled = ?1,
                 state = CASE WHEN ?1 = 1 THEN 'idle' ELSE 'disabled' END,
                 last_reason = CASE WHEN ?1 = 1 THEN 'enabled' ELSE 'disabled' END,
                 last_message = CASE
                    WHEN ?1 = 1 THEN 'Watch mode enabled. Waiting for a local change or provider reconciliation.'
                    ELSE 'Watch mode is paused. Manual reviewed plans remain available.'
                 END,
                 consecutive_failures = 0,
                 updated_at = ?2
             WHERE workspace_id = ?3",
            params![if enabled { 1 } else { 0 }, now, workspace_id],
        )
        .map_err(|error| format!("Could not update watch mode: {error}"))?;
    Ok(())
}

fn set_disabled_state(app: &AppHandle, workspace_id: &str) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET enabled = 0, state = 'disabled', updated_at = ?1
             WHERE workspace_id = ?2",
            params![Utc::now().to_rfc3339(), workspace_id],
        )
        .map_err(|error| format!("Could not pause watch mode: {error}"))?;
    Ok(())
}

fn mark_debouncing(
    app: &AppHandle,
    workspace_id: &str,
    reason: &str,
    record_event: bool,
) -> Result<(), String> {
    ensure_settings_row(app, workspace_id)?;
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = CASE WHEN state = 'running' THEN state ELSE 'debouncing' END,
                 last_reason = ?1,
                 last_event_at = CASE WHEN ?2 = 1 THEN ?3 ELSE last_event_at END,
                 last_message = CASE
                    WHEN state = 'running' THEN last_message
                    ELSE 'Change detected. Waiting for the workspace to settle before reconciliation.'
                 END,
                 updated_at = ?3
             WHERE workspace_id = ?4 AND enabled = 1",
            params![reason, if record_event { 1 } else { 0 }, now, workspace_id],
        )
        .map_err(|error| format!("Could not record continuous dirty signal: {error}"))?;
    Ok(())
}

fn mark_cycle_running(app: &AppHandle, workspace_id: &str, reason: &str) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = 'running', last_reason = ?1,
                 last_cycle_started_at = ?2,
                 last_message = 'Refreshing local and remote evidence…',
                 updated_at = ?2
             WHERE workspace_id = ?3 AND enabled = 1",
            params![reason, now, workspace_id],
        )
        .map_err(|error| format!("Could not start continuous cycle journal: {error}"))?;
    Ok(())
}

fn record_cycle_deferred(
    app: &AppHandle,
    workspace_id: &str,
    reason: &str,
    message: &str,
) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = 'debouncing',
                 last_reason = ?1,
                 last_message = ?2,
                 last_decision = 'deferred',
                 updated_at = ?3
             WHERE workspace_id = ?4 AND enabled = 1",
            params![
                reason,
                truncate_message(message),
                Utc::now().to_rfc3339(),
                workspace_id
            ],
        )
        .map_err(|error| format!("Could not record deferred continuous cycle: {error}"))?;
    Ok(())
}

fn record_cycle_outcome(
    app: &AppHandle,
    workspace_id: &str,
    reason: &str,
    outcome: CycleOutcome,
) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = ?1,
                 last_reason = ?2,
                 last_cycle_completed_at = ?3,
                 last_success_at = CASE WHEN ?4 = 1 THEN ?3 ELSE last_success_at END,
                 last_message = ?5,
                 consecutive_failures = 0,
                 last_evidence_signature = ?6,
                 last_decision = ?7,
                 updated_at = ?3
             WHERE workspace_id = ?8 AND enabled = 1",
            params![
                outcome.state,
                reason,
                now,
                if outcome.successful { 1 } else { 0 },
                outcome.message,
                outcome.evidence_signature,
                outcome.decision,
                workspace_id,
            ],
        )
        .map_err(|error| format!("Could not finish continuous cycle journal: {error}"))?;
    Ok(())
}

fn record_cycle_error(
    app: &AppHandle,
    workspace_id: &str,
    reason: &str,
    message: &str,
) -> Result<(), String> {
    let connection = open_continuous_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = 'error', last_reason = ?1,
                 last_cycle_completed_at = ?2,
                 last_message = ?3,
                 consecutive_failures = consecutive_failures + 1,
                 updated_at = ?2
             WHERE workspace_id = ?4 AND enabled = 1",
            params![reason, now, truncate_message(message), workspace_id],
        )
        .map_err(|error| format!("Could not record continuous cycle failure: {error}"))?;
    Ok(())
}

fn record_runtime_error(app: &AppHandle, workspace_id: &str, message: &str) -> Result<(), String> {
    ensure_settings_row(app, workspace_id)?;
    let connection = open_continuous_database(app)?;
    connection
        .execute(
            "UPDATE continuous_sync_settings
             SET state = CASE WHEN enabled = 1 THEN 'error' ELSE state END,
                 last_message = ?1,
                 consecutive_failures = CASE WHEN enabled = 1 THEN consecutive_failures + 1 ELSE consecutive_failures END,
                 updated_at = ?2
             WHERE workspace_id = ?3",
            params![truncate_message(message), Utc::now().to_rfc3339(), workspace_id],
        )
        .map_err(|error| format!("Could not record watcher runtime error: {error}"))?;
    Ok(())
}

fn enabled_workspace_ids(app: &AppHandle) -> Result<Vec<String>, String> {
    let connection = open_continuous_database(app)?;
    let mut statement = connection
        .prepare("SELECT workspace_id FROM continuous_sync_settings WHERE enabled = 1")
        .map_err(|error| format!("Could not prepare enabled watch query: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query enabled watch workspaces: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read enabled watch workspaces: {error}"))
}

fn due_remote_poll_workspaces(app: &AppHandle) -> Result<Vec<String>, String> {
    let connection = open_continuous_database(app)?;
    let mut statement = connection
        .prepare(
            "SELECT workspace_id, remote_poll_seconds, state, last_cycle_completed_at, consecutive_failures
             FROM continuous_sync_settings WHERE enabled = 1",
        )
        .map_err(|error| format!("Could not prepare continuous poll query: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| format!("Could not query continuous poll settings: {error}"))?;

    let now = Utc::now();
    let mut due = Vec::new();
    for row in rows {
        let (workspace_id, poll_seconds, state, last_completed, failures) =
            row.map_err(|error| format!("Could not read continuous poll row: {error}"))?;
        if matches!(
            state.as_str(),
            "running" | "debouncing" | "attention" | "disabled"
        ) {
            continue;
        }
        let poll_seconds = from_i64(poll_seconds, "remote poll interval")?;
        let failures = from_i64(failures, "continuous failure count")?;
        let backoff = if state == "error" {
            1_u64 << failures.min(3)
        } else {
            1
        }
        .min(MAX_FAILURE_BACKOFF_MULTIPLIER);
        let interval = poll_seconds.saturating_mul(backoff);
        let elapsed = last_completed
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| {
                now.signed_duration_since(value.with_timezone(&Utc))
                    .num_seconds()
            })
            .unwrap_or(i64::MAX);
        if elapsed >= i64::try_from(interval).unwrap_or(i64::MAX) {
            due.push(workspace_id);
        }
    }
    Ok(due)
}

fn validate_poll_seconds(value: u64) -> Result<(), String> {
    if !(MIN_REMOTE_POLL_SECONDS..=MAX_REMOTE_POLL_SECONDS).contains(&value) {
        return Err(format!(
            "Remote reconciliation interval must be between {MIN_REMOTE_POLL_SECONDS} and {MAX_REMOTE_POLL_SECONDS} seconds."
        ));
    }
    Ok(())
}

fn truncate_message(message: &str) -> String {
    const LIMIT: usize = 900;
    if message.len() <= LIMIT {
        return message.to_string();
    }
    let mut end = LIMIT;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} exceeds SQLite INTEGER range."))
}

fn from_i64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Stored {label} is invalid."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_way_never_auto_applies_deletions_or_conflicts() {
        assert!(matches!(
            two_way_policy(1, 1, 1, 0, 0, true),
            AutomaticPolicy::Attention(_)
        ));
        assert!(matches!(
            two_way_policy(1, 1, 0, 1, 0, true),
            AutomaticPolicy::Attention(_)
        ));
        assert!(matches!(
            two_way_policy(1, 1, 0, 0, 1, true),
            AutomaticPolicy::Attention(_)
        ));
    }

    #[test]
    fn safe_two_way_transfers_require_auto_apply_opt_in() {
        assert!(matches!(
            two_way_policy(2, 3, 0, 0, 0, false),
            AutomaticPolicy::Review(_)
        ));
        assert!(matches!(
            two_way_policy(2, 3, 0, 0, 0, true),
            AutomaticPolicy::Apply
        ));
    }

    #[test]
    fn backup_and_restore_blocked_paths_never_auto_apply() {
        assert!(matches!(
            backup_policy(2, 1, true),
            AutomaticPolicy::Attention(_)
        ));
        assert!(matches!(
            restore_policy(2, 1, true),
            AutomaticPolicy::Attention(_)
        ));
    }

    #[test]
    fn remote_poll_interval_is_bounded() {
        assert!(validate_poll_seconds(29).is_err());
        assert!(validate_poll_seconds(30).is_ok());
        assert!(validate_poll_seconds(60).is_ok());
        assert!(validate_poll_seconds(3600).is_ok());
        assert!(validate_poll_seconds(3601).is_err());
    }
}
