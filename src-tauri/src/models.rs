use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub local_path: String,
    pub sync_mode: SyncMode,
    pub created_at: String,
    pub last_scan_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Backup,
    Pull,
    TwoWay,
}

impl SyncMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Pull => "pull",
            Self::TwoWay => "two_way",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "backup" => Some(Self::Backup),
            "pull" => Some(Self::Pull),
            "two_way" => Some(Self::TwoWay),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanFile {
    pub relative_path: String,
    pub size: u64,
    pub modified_at: Option<String>,
    pub blake3: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub workspace_id: String,
    pub scanned_at: String,
    pub duration_ms: u128,
    pub file_count: u64,
    pub directory_count: u64,
    pub total_bytes: u64,
    pub skipped_entries: u64,
    pub preview_truncated: bool,
    pub files: Vec<ScanFile>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSummary {
    pub workspace_id: String,
    pub tracked_files: u64,
    pub present_files: u64,
    pub present_bytes: u64,
    pub changed_files: u64,
    pub tombstones: u64,
    pub conflicts: u64,
    pub pending_operations: u64,
    pub last_scan_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneStatus {
    pub available: bool,
    pub version: Option<String>,
    pub required_version: String,
    pub source: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub provider_type: String,
    pub display_name: String,
    pub account_label: Option<String>,
    pub created_at: String,
    pub last_verified_at: Option<String>,
    pub session_active: bool,
    pub credential_persisted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRemoteBinding {
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
    pub created_at: String,
    pub last_inventory_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEncryptionStatus {
    pub workspace_id: String,
    pub mode: String,
    pub key_available: bool,
    pub filename_encrypted: bool,
    pub remote_namespace: Option<String>,
    pub enabled_at: Option<String>,
    pub verified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionEnableResult {
    pub status: WorkspaceEncryptionStatus,
    pub recovery_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInventoryReport {
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
    pub scanned_at: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteFileObservation {
    pub relative_path: String,
    pub remote_id: Option<String>,
    pub size: u64,
    pub modified_at: Option<String>,
    pub checksum_type: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPlanItem {
    pub id: String,
    pub relative_path: String,
    pub action: String,
    pub status: String,
    pub size: Option<u64>,
    pub block_reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPlan {
    pub id: String,
    pub workspace_id: String,
    pub provider_id: String,
    pub remote_path: String,
    pub status: String,
    pub created_at: String,
    pub local_scan_at: String,
    pub remote_inventory_at: String,
    pub upload_count: u64,
    pub upload_bytes: u64,
    pub blocked_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub completed_at: Option<String>,
    pub preview_truncated: bool,
    pub items: Vec<BackupPlanItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupExecutionReport {
    pub plan_id: String,
    pub status: String,
    pub completed_count: u64,
    pub failed_count: u64,
    pub uploaded_bytes: u64,
    pub finished_at: String,
}
