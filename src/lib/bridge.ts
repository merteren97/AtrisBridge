import { invoke } from "@tauri-apps/api/core";
import type {
  BackupExecutionReport,
  BackupPlan,
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
  return invoke("remove_workspace", { id });
}

export async function setWorkspaceSyncMode(id: string, mode: SyncMode): Promise<Workspace> {
  return invoke<Workspace>("set_workspace_sync_mode", { id, mode });
}

export async function scanWorkspace(id: string): Promise<ScanReport> {
  return invoke<ScanReport>("scan_workspace", { id });
}

export async function initializeIgnoreFile(id: string): Promise<boolean> {
  return invoke<boolean>("initialize_ignore_file", { id });
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
  return invoke<ProviderConnection>("connect_google_drive");
}

export async function disconnectProviderSession(providerId: string): Promise<void> {
  return invoke("disconnect_provider_session", { providerId });
}

export async function forgetProvider(providerId: string): Promise<void> {
  return invoke("forget_provider", { providerId });
}

export async function getWorkspaceRemoteBinding(id: string): Promise<WorkspaceRemoteBinding | null> {
  return invoke<WorkspaceRemoteBinding | null>("workspace_remote_binding", { id });
}

export async function bindWorkspaceRemote(
  id: string,
  providerId: string,
  remotePath: string,
): Promise<WorkspaceRemoteBinding> {
  return invoke<WorkspaceRemoteBinding>("bind_workspace_remote", { id, providerId, remotePath });
}

export async function getWorkspaceEncryptionStatus(id: string): Promise<WorkspaceEncryptionStatus> {
  return invoke<WorkspaceEncryptionStatus>("workspace_encryption_status", { id });
}

export async function enableWorkspaceEncryption(id: string): Promise<EncryptionEnableResult> {
  return invoke<EncryptionEnableResult>("enable_workspace_encryption", { id });
}

export async function exportWorkspaceRecoveryKey(id: string): Promise<string> {
  return invoke<string>("export_workspace_recovery_key", { id });
}

export async function importWorkspaceRecoveryKey(
  id: string,
  recoveryKey: string,
): Promise<WorkspaceEncryptionStatus> {
  return invoke<WorkspaceEncryptionStatus>("import_workspace_recovery_key", { id, recoveryKey });
}

export async function scanRemoteInventory(id: string): Promise<RemoteInventoryReport> {
  return invoke<RemoteInventoryReport>("scan_remote_inventory", { id });
}

export async function getLatestBackupPlan(id: string): Promise<BackupPlan | null> {
  return invoke<BackupPlan | null>("latest_backup_plan", { id });
}

export async function prepareBackupPlan(id: string): Promise<BackupPlan> {
  return invoke<BackupPlan>("prepare_backup_plan", { id });
}

export async function executeBackupPlan(planId: string): Promise<BackupExecutionReport> {
  return invoke<BackupExecutionReport>("execute_backup_plan", { planId });
}

export async function getLatestRestorePlan(id: string): Promise<RestorePlan | null> {
  return invoke<RestorePlan | null>("latest_restore_plan", { id });
}

export async function prepareRestorePlan(id: string): Promise<RestorePlan> {
  return invoke<RestorePlan>("prepare_restore_plan", { id });
}

export async function executeRestorePlan(planId: string): Promise<RestoreExecutionReport> {
  return invoke<RestoreExecutionReport>("execute_restore_plan", { planId });
}

export async function getLatestSyncPlan(id: string): Promise<SyncPlan | null> {
  return invoke<SyncPlan | null>("latest_sync_plan", { id });
}

export async function prepareSyncPlan(id: string): Promise<SyncPlan> {
  return invoke<SyncPlan>("prepare_sync_plan", { id });
}

export async function executeSyncPlan(planId: string): Promise<SyncExecutionReport> {
  return invoke<SyncExecutionReport>("execute_sync_plan", { planId });
}

export async function listSyncRecoveries(id: string): Promise<SyncRecoveryEntry[]> {
  return invoke<SyncRecoveryEntry[]>("list_sync_recoveries", { id });
}

export async function restoreSyncRecovery(recoveryId: string): Promise<SyncRecoveryEntry> {
  return invoke<SyncRecoveryEntry>("restore_sync_recovery", { recoveryId });
}
