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
