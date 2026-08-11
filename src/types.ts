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
