import { invoke } from "@tauri-apps/api/core";
import type {
  BackupExecutionReport,
  BackupPlan,
  JournalSummary,
  ProviderConnection,
  RcloneStatus,
  RemoteInventoryReport,
  ScanReport,
  Workspace,
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
