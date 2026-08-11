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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Backup,
    Pull,
    TwoWay,
}

#[derive(Debug, Serialize)]
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
