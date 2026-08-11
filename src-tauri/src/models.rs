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
