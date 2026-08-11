import { invoke } from "@tauri-apps/api/core";
import type {
  BackupExecutionReport,
  BackupPlan,
  ContinuousSyncStatus,
  EncryptionEnableResult,
  JournalSummary,
  ProviderConnection,
  RcloneStatus,
  RemoteInventoryReport,
  RestoreExecutionReport,
  RestorePlan,
  ScanReport,
  SyncExecutionReport,
  SyncMode,
  SyncPlan,
  SyncRecoveryEntry,
  Workspace,
  WorkspaceEncryptionStatus,
  WorkspaceRemoteBinding,
} from "../types";

export async function listWorkspaces(): Promise<Workspace[]> {
  return invoke<Workspace[]>("list_workspaces");
}

export async function addWorkspace(name: string, path: string): Promise<Workspace> {
  return invoke<Workspace>("add_workspace", { name, path });
}

export async function removeWorkspace(id: string): Promise<void> {
  return invoke("guarded_remove_workspace", { id });
}

export async function setWorkspaceSyncMode(id: string, mode: SyncMode): Promise<Workspace> {
  return invoke<Workspace>("guarded_set_workspace_sync_mode", { id, mode });
}

export async function scanWorkspace(id: string): Promise<ScanReport> {
  return invoke<ScanReport>("guarded_scan_workspace", { id });
}

export async function initializeIgnoreFile(id: string): Promise<boolean> {
  return invoke<boolean>("guarded_initialize_ignore_file", { id });
}

export async function getJournalSummary(id: string): Promise<JournalSummary> {
  return invoke<JournalSummary>("journal_summary", { id });
}

export async function listJournalSummaries(): Promise<JournalSummary[]> {
  return invoke<JournalSummary[]>("journal_summaries");
}

export async function getRcloneStatus(): Promise<RcloneStatus> {
  return invoke<RcloneStatus>("rclone_runtime_status");
}

export async function listProviderConnections(): Promise<ProviderConnection[]> {
  return invoke<ProviderConnection[]>("provider_connections");
}

export async function connectGoogleDrive(): Promise<ProviderConnection> {
  return invoke<ProviderConnection>("guarded_connect_google_drive");
}

export async function disconnectProviderSession(providerId: string): Promise<void> {
  return invoke("guarded_disconnect_provider_session", { providerId });
}

export async function forgetProvider(providerId: string): Promise<void> {
  return invoke("guarded_forget_provider", { providerId });
}

export async function getWorkspaceRemoteBinding(id: string): Promise<WorkspaceRemoteBinding | null> {
  return invoke<WorkspaceRemoteBinding | null>("workspace_remote_binding", { id });
}

export async function bindWorkspaceRemote(
  id: string,
  providerId: string,
  remotePath: string,
): Promise<WorkspaceRemoteBinding> {
  return invoke<WorkspaceRemoteBinding>("guarded_bind_workspace_remote", { id, providerId, remotePath });
}

export async function getWorkspaceEncryptionStatus(id: string): Promise<WorkspaceEncryptionStatus> {
  return invoke<WorkspaceEncryptionStatus>("workspace_encryption_status", { id });
}

export async function enableWorkspaceEncryption(id: string): Promise<EncryptionEnableResult> {
  return invoke<EncryptionEnableResult>("guarded_enable_workspace_encryption", { id });
}

export async function exportWorkspaceRecoveryKey(id: string): Promise<string> {
  return invoke<string>("export_workspace_recovery_key", { id });
}

export async function importWorkspaceRecoveryKey(
  id: string,
  recoveryKey: string,
): Promise<WorkspaceEncryptionStatus> {
  return invoke<WorkspaceEncryptionStatus>("guarded_import_workspace_recovery_key", { id, recoveryKey });
}

export async function scanRemoteInventory(id: string): Promise<RemoteInventoryReport> {
  return invoke<RemoteInventoryReport>("guarded_scan_remote_inventory", { id });
}

export async function getLatestBackupPlan(id: string): Promise<BackupPlan | null> {
  return invoke<BackupPlan | null>("latest_backup_plan", { id });
}

export async function prepareBackupPlan(id: string): Promise<BackupPlan> {
  return invoke<BackupPlan>("guarded_prepare_backup_plan", { id });
}

export async function executeBackupPlan(planId: string): Promise<BackupExecutionReport> {
  return invoke<BackupExecutionReport>("guarded_execute_backup_plan", { planId });
}

export async function getLatestRestorePlan(id: string): Promise<RestorePlan | null> {
  return invoke<RestorePlan | null>("latest_restore_plan", { id });
}

export async function prepareRestorePlan(id: string): Promise<RestorePlan> {
  return invoke<RestorePlan>("guarded_prepare_restore_plan", { id });
}

export async function executeRestorePlan(planId: string): Promise<RestoreExecutionReport> {
  return invoke<RestoreExecutionReport>("guarded_execute_restore_plan", { planId });
}

export async function getLatestSyncPlan(id: string): Promise<SyncPlan | null> {
  return invoke<SyncPlan | null>("latest_sync_plan", { id });
}

export async function prepareSyncPlan(id: string): Promise<SyncPlan> {
  return invoke<SyncPlan>("guarded_prepare_sync_plan", { id });
}

export async function executeSyncPlan(planId: string): Promise<SyncExecutionReport> {
  return invoke<SyncExecutionReport>("guarded_execute_sync_plan", { planId });
}

export async function listSyncRecoveries(id: string): Promise<SyncRecoveryEntry[]> {
  return invoke<SyncRecoveryEntry[]>("list_sync_recoveries", { id });
}

export async function restoreSyncRecovery(recoveryId: string): Promise<SyncRecoveryEntry> {
  return invoke<SyncRecoveryEntry>("guarded_restore_sync_recovery", { recoveryId });
}

export async function getContinuousSyncStatus(id: string): Promise<ContinuousSyncStatus> {
  return invoke<ContinuousSyncStatus>("continuous_sync_status", { id });
}

export async function setContinuousSyncEnabled(
  id: string,
  enabled: boolean,
): Promise<ContinuousSyncStatus> {
  return invoke<ContinuousSyncStatus>("set_continuous_sync_enabled", { id, enabled });
}

export async function updateContinuousSyncSettings(
  id: string,
  autoApplySafe: boolean,
  remotePollSeconds: number,
): Promise<ContinuousSyncStatus> {
  return invoke<ContinuousSyncStatus>("update_continuous_sync_settings", {
    id,
    autoApplySafe,
    remotePollSeconds,
  });
}

export async function runContinuousSyncNow(id: string): Promise<ContinuousSyncStatus> {
  return invoke<ContinuousSyncStatus>("run_continuous_sync_now", { id });
}
