pub(crate) const WORKSPACE_TEXT_MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const WORKSPACE_TEXT_MAX_LINES: usize = 2_000;
pub(crate) const WORKSPACE_TEXT_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub(crate) const WORKSPACE_SEARCH_DEFAULT_RESULTS: u32 = 100;
pub(crate) const WORKSPACE_SEARCH_MAX_RESULTS: u32 = 500;
pub(crate) const WORKSPACE_SEARCH_MAX_QUERY_CHARS: usize = 512;
pub(crate) const WORKSPACE_SEARCH_MAX_EXCERPT_CHARS: usize = 700;
pub(crate) const WORKSPACE_SEARCH_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) const CHANGESET_MAX_ITEMS: usize = 100;
pub(crate) const CHANGESET_MAX_WRITE_BYTES_PER_FILE: usize = 2 * 1024 * 1024;
pub(crate) const CHANGESET_MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const CHANGESET_DEFAULT_LIST_RESULTS: u32 = 50;
pub(crate) const CHANGESET_MAX_LIST_RESULTS: u32 = 200;

pub(crate) const COMMAND_STDOUT_MAX_BYTES: usize = 1024 * 1024;
pub(crate) const COMMAND_STDERR_MAX_BYTES: usize = 1024 * 1024;

pub(crate) const GIT_OUTPUT_MAX_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const GIT_ERROR_MAX_BYTES: usize = 64 * 1024;
pub(crate) const GIT_MAX_PATHS: usize = 200;
pub(crate) const GIT_MAX_DIFF_PATHS: usize = 2_000;
pub(crate) const GIT_MAX_LOG_ENTRIES: u32 = 500;
pub(crate) const GIT_MAX_COMMIT_MESSAGE_CHARS: usize = 4_096;

pub(crate) const MCP_DEFAULT_LIST_RESULTS: u32 = 50;
pub(crate) const MCP_MAX_LIST_RESULTS: u32 = 200;
