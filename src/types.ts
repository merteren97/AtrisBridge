export type SyncMode = "backup" | "pull" | "two_way";

export interface Workspace {
  id: string;
  name: string;
  localPath: string;
  syncMode: SyncMode;
  createdAt: string;
  lastScanAt: string | null;
}

export interface ScanFile {
  relativePath: string;
  size: number;
  modifiedAt: string | null;
  blake3: string;
}

export interface ScanReport {
  workspaceId: string;
  scannedAt: string;
  durationMs: number;
  fileCount: number;
  directoryCount: number;
  totalBytes: number;
  skippedEntries: number;
  previewTruncated: boolean;
  files: ScanFile[];
  warnings: string[];
}

export interface JournalSummary {
  workspaceId: string;
  trackedFiles: number;
  presentFiles: number;
  presentBytes: number;
  changedFiles: number;
  tombstones: number;
  conflicts: number;
  pendingOperations: number;
  lastScanAt: string | null;
}

export interface RcloneStatus {
  available: boolean;
  version: string | null;
  requiredVersion: string;
  source: string | null;
  message: string | null;
}

export interface ProviderConnection {
  id: string;
  providerType: "google_drive";
  displayName: string;
  accountLabel: string | null;
  createdAt: string;
  lastVerifiedAt: string | null;
  sessionActive: boolean;
  credentialPersisted: boolean;
}

export interface WorkspaceRemoteBinding {
  workspaceId: string;
  providerId: string;
  remotePath: string;
  createdAt: string;
  lastInventoryAt: string | null;
}

export interface WorkspaceEncryptionStatus {
  workspaceId: string;
  mode: "disabled" | "content";
  keyAvailable: boolean;
  filenameEncrypted: boolean;
  remoteNamespace: string | null;
  enabledAt: string | null;
  verifiedAt: string | null;
}

export interface EncryptionEnableResult {
  status: WorkspaceEncryptionStatus;
  recoveryKey: string;
}

export interface RemoteInventoryReport {
  workspaceId: string;
  providerId: string;
  remotePath: string;
  scannedAt: string;
  fileCount: number;
  totalBytes: number;
}

export type TransferPlanStatus =
  | "ready"
  | "running"
  | "completed"
  | "partial"
  | "failed"
  | "cancelled";

export type TransferPlanItemAction = "create" | "update" | "blocked";
export type BackupPlanStatus = TransferPlanStatus;
export type BackupPlanItemAction = TransferPlanItemAction;
export type BackupPlanItemStatus =
  | "ready"
  | "running"
  | "completed"
  | "failed"
  | "blocked"
  | "cancelled";

export interface BackupPlanItem {
  id: string;
  relativePath: string;
  action: BackupPlanItemAction;
  status: BackupPlanItemStatus;
  size: number | null;
  blockReason: string | null;
  lastError: string | null;
}

export interface BackupPlan {
  id: string;
  workspaceId: string;
  providerId: string;
  remotePath: string;
  status: BackupPlanStatus;
  createdAt: string;
  localScanAt: string;
  remoteInventoryAt: string;
  uploadCount: number;
  uploadBytes: number;
  blockedCount: number;
  completedCount: number;
  failedCount: number;
  completedAt: string | null;
  previewTruncated: boolean;
  items: BackupPlanItem[];
}

export interface BackupExecutionReport {
  planId: string;
  status: BackupPlanStatus;
  completedCount: number;
  failedCount: number;
  uploadedBytes: number;
  finishedAt: string;
}

export type RestorePlanStatus = TransferPlanStatus;
export type RestorePlanItemAction = TransferPlanItemAction;
export type RestorePlanItemStatus =
  | "ready"
  | "running"
  | "applying"
  | "completed"
  | "failed"
  | "blocked"
  | "cancelled";

export interface RestorePlanItem {
  id: string;
  relativePath: string;
  action: RestorePlanItemAction;
  status: RestorePlanItemStatus;
  size: number | null;
  blockReason: string | null;
  lastError: string | null;
}

export interface RestorePlan {
  id: string;
  workspaceId: string;
  providerId: string;
  remotePath: string;
  status: RestorePlanStatus;
  createdAt: string;
  localScanAt: string;
  remoteInventoryAt: string;
  restoreCount: number;
  restoreBytes: number;
  blockedCount: number;
  completedCount: number;
  failedCount: number;
  completedAt: string | null;
  previewTruncated: boolean;
  items: RestorePlanItem[];
}

export interface RestoreExecutionReport {
  planId: string;
  status: RestorePlanStatus;
  completedCount: number;
  failedCount: number;
  restoredBytes: number;
  finishedAt: string;
}

export type SyncPlanStatus = TransferPlanStatus;
export type SyncPlanItemAction =
  | "upload_create"
  | "upload_update"
  | "download_create"
  | "download_update"
  | "remote_trash"
  | "local_delete"
  | "acknowledge_delete"
  | "conflict"
  | "blocked";
export type SyncPlanItemStatus =
  | "ready"
  | "running"
  | "applying"
  | "completed"
  | "failed"
  | "conflict"
  | "blocked"
  | "cancelled";

export interface SyncPlanItem {
  id: string;
  relativePath: string;
  action: SyncPlanItemAction;
  status: SyncPlanItemStatus;
  size: number | null;
  reason: string | null;
  lastError: string | null;
}

export interface SyncPlan {
  id: string;
  workspaceId: string;
  providerId: string;
  remotePath: string;
  status: SyncPlanStatus;
  createdAt: string;
  localScanAt: string;
  remoteInventoryAt: string;
  uploadCount: number;
  downloadCount: number;
  deleteCount: number;
  conflictCount: number;
  blockedCount: number;
  transferBytes: number;
  completedCount: number;
  failedCount: number;
  completedAt: string | null;
  previewTruncated: boolean;
  items: SyncPlanItem[];
}

export interface SyncExecutionReport {
  planId: string;
  status: SyncPlanStatus;
  completedCount: number;
  failedCount: number;
  transferredBytes: number;
  finishedAt: string;
}

export interface SyncRecoveryEntry {
  id: string;
  workspaceId: string;
  relativePath: string;
  size: number;
  createdAt: string;
  restoredAt: string | null;
}

export type ContinuousSyncState =
  | "disabled"
  | "idle"
  | "debouncing"
  | "running"
  | "attention"
  | "error";

export interface ContinuousSyncStatus {
  workspaceId: string;
  enabled: boolean;
  runtimeActive: boolean;
  autoApplySafe: boolean;
  remotePollSeconds: number;
  state: ContinuousSyncState;
  lastReason: string | null;
  lastEventAt: string | null;
  lastCycleStartedAt: string | null;
  lastCycleCompletedAt: string | null;
  lastSuccessAt: string | null;
  lastMessage: string | null;
  consecutiveFailures: number;
}
