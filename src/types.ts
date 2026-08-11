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
}

export interface WorkspaceRemoteBinding {
  workspaceId: string;
  providerId: string;
  remotePath: string;
  createdAt: string;
  lastInventoryAt: string | null;
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
