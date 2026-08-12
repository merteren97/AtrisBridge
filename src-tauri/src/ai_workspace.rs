use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    time::Instant,
};

use chrono::{DateTime, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{
    ai_gateway::{self, AiAuditEvent, AiSession},
    scanner,
    storage::find_workspace,
    workspace_coordinator::{
        WorkspaceMutationCoordinator, WorkspaceMutationLease, WorkspaceOperationKind,
    },
};

const MAX_READ_BYTES: u64 = 1024 * 1024;
const MAX_LINE_WINDOW: usize = 500;
const DEFAULT_SEARCH_LIMIT: u32 = 50;
const MAX_SEARCH_LIMIT: u32 = 200;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_SEARCH_EXCERPT_CHARS: usize = 400;
const MAX_SEARCHABLE_FILE_BYTES: u64 = 1024 * 1024;
const BUILTIN_DIRECTORY_EXCLUDES: &[&str] = &[
    ".git",
    ".vs",
    ".idea",
    ".next",
    "node_modules",
    "dist",
    "build",
    "bin",
    "obj",
    "target",
    "coverage",
];
const SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "pfx", "p12", "jks", "keystore", "kdbx"];
const SENSITIVE_FILE_NAMES: &[&str] = &[
    ".npmrc",
    ".pypirc",
    ".netrc",
    "id_rsa",
    "id_ed25519",
    "credentials.json",
    "secrets.json",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AiPathClass {
    Normal,
    Sensitive,
    Denied,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFileStat {
    pub workspace_id: String,
    pub relative_path: String,
    pub size: u64,
    pub modified_at: Option<String>,
    pub blake3: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTextFile {
    pub workspace_id: String,
    pub relative_path: String,
    pub size: u64,
    pub modified_at: Option<String>,
    pub blake3: String,
    pub sensitive: bool,
    pub start_line: u64,
    pub end_line: u64,
    pub total_lines: u64,
    pub truncated: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSearchMatch {
    pub relative_path: String,
    pub line: u64,
    pub column: u64,
    pub excerpt: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSearchResult {
    pub workspace_id: String,
    pub query: String,
    pub matches: Vec<AiSearchMatch>,
    pub truncated: bool,
    pub searched_files: u64,
    pub skipped_files: u64,
}

#[tauri::command]
pub fn ai_file_stat(
    app: AppHandle,
    session_id: String,
    relative_path: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiFileStat, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.read")?;
    let result = (|| {
        let (class, path) = authorize_existing_file_path(&app, &session, &relative_path, false)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        let (size, blake3) = scanner::fingerprint_file(&path)?;
        Ok(AiFileStat {
            workspace_id: session.workspace_id.clone(),
            relative_path: normalize_relative_path(&relative_path)?,
            size,
            modified_at: modified_at(&path)?,
            blake3,
            sensitive: class == AiPathClass::Sensitive,
        })
    })();
    record_tool_result(
        &app,
        &session,
        "workspace.read",
        "workspace.file_stat",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn ai_read_text_file(
    app: AppHandle,
    session_id: String,
    relative_path: String,
    start_line: Option<u64>,
    end_line: Option<u64>,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiTextFile, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.read")?;
    let result = (|| {
        let (class, path) = authorize_existing_file_path(&app, &session, &relative_path, false)?;
        let _lease = acquire_observe(&coordinator, &session)?;
        let (size, blake3) = scanner::fingerprint_file(&path)?;
        if size > MAX_READ_BYTES {
            return Err(format!(
                "AI text reads are limited to {} bytes per file.",
                MAX_READ_BYTES
            ));
        }
        let bytes =
            fs::read(&path).map_err(|error| format!("Could not read workspace file: {error}"))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| "Workspace file is not valid UTF-8 text.".to_string())?;
        let lines = text.lines().collect::<Vec<_>>();
        let total_lines = u64::try_from(lines.len()).unwrap_or(u64::MAX);
        let requested_start = start_line.unwrap_or(1);
        if requested_start == 0 {
            return Err("startLine is 1-based and must be at least 1.".into());
        }
        let requested_end = end_line.unwrap_or_else(|| {
            requested_start.saturating_add(u64::try_from(MAX_LINE_WINDOW - 1).unwrap_or(499))
        });
        if requested_end < requested_start {
            return Err("endLine must be greater than or equal to startLine.".into());
        }
        let requested_count = requested_end
            .saturating_sub(requested_start)
            .saturating_add(1);
        if requested_count > u64::try_from(MAX_LINE_WINDOW).unwrap_or(500) {
            return Err(format!(
                "AI text reads are limited to {MAX_LINE_WINDOW} lines per request."
            ));
        }

        let start_index = usize::try_from(requested_start.saturating_sub(1)).unwrap_or(usize::MAX);
        let end_index = usize::try_from(requested_end)
            .unwrap_or(usize::MAX)
            .min(lines.len());
        let content = if start_index >= lines.len() {
            String::new()
        } else {
            lines[start_index..end_index].join("\n")
        };
        let actual_end = if start_index >= lines.len() {
            requested_start.saturating_sub(1)
        } else {
            u64::try_from(end_index).unwrap_or(u64::MAX)
        };

        Ok(AiTextFile {
            workspace_id: session.workspace_id.clone(),
            relative_path: normalize_relative_path(&relative_path)?,
            size,
            modified_at: modified_at(&path)?,
            blake3,
            sensitive: class == AiPathClass::Sensitive,
            start_line: requested_start,
            end_line: actual_end,
            total_lines,
            truncated: actual_end < total_lines,
            content,
        })
    })();
    record_tool_result(
        &app,
        &session,
        "workspace.read",
        "workspace.read_text_file",
        started,
        &result,
    )?;
    result
}

#[tauri::command]
pub fn ai_search_workspace(
    app: AppHandle,
    session_id: String,
    query: String,
    limit: Option<u32>,
    include_sensitive: bool,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<AiSearchResult, String> {
    let started = Instant::now();
    let session = ai_gateway::authorize_session(&app, &session_id, "workspace.read")?;
    let result = (|| {
        let query = query.trim();
        if query.is_empty() {
            return Err("Search query cannot be empty.".into());
        }
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err(format!(
                "Search query is limited to {MAX_SEARCH_QUERY_CHARS} characters."
            ));
        }
        if include_sensitive {
            ai_gateway::authorize_session(&app, &session.id, "sensitive.read")?;
        }
        let _lease = acquire_observe(&coordinator, &session)?;
        let workspace = find_workspace(&app, &session.workspace_id)?;
        let root = canonical_workspace_root(&workspace.local_path)?;
        let matcher = build_custom_ignore(&root)?;
        let limit = limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT) as usize;
        let mut state = SearchState {
            query_lower: query.to_lowercase(),
            include_sensitive,
            limit,
            matches: Vec::new(),
            searched_files: 0,
            skipped_files: 0,
            truncated: false,
        };
        visit_search_directory(&root, &root, matcher.as_ref(), &mut state)?;
        Ok(AiSearchResult {
            workspace_id: session.workspace_id.clone(),
            query: query.to_string(),
            matches: state.matches,
            truncated: state.truncated,
            searched_files: state.searched_files,
            skipped_files: state.skipped_files,
        })
    })();
    record_tool_result(
        &app,
        &session,
        "workspace.read",
        "workspace.search",
        started,
        &result,
    )?;
    result
}

struct SearchState {
    query_lower: String,
    include_sensitive: bool,
    limit: usize,
    matches: Vec<AiSearchMatch>,
    searched_files: u64,
    skipped_files: u64,
    truncated: bool,
}

fn visit_search_directory(
    root: &Path,
    directory: &Path,
    matcher: Option<&Gitignore>,
    state: &mut SearchState,
) -> Result<(), String> {
    if state.matches.len() >= state.limit {
        state.truncated = true;
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("Could not read workspace directory: {error}"))?;
    for entry in entries {
        if state.matches.len() >= state.limit {
            state.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                state.skipped_files = state.skipped_files.saturating_add(1);
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => {
                state.skipped_files = state.skipped_files.saturating_add(1);
                continue;
            }
        };
        if file_type.is_symlink() {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative_string = relative_to_string(relative);
        let class = classify_relative_path(&relative_string)?;
        if class == AiPathClass::Denied {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        let is_dir = file_type.is_dir();
        if is_builtin_directory_excluded(relative) || is_custom_ignored(&path, is_dir, matcher) {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        if is_dir {
            visit_search_directory(root, &path, matcher, state)?;
            continue;
        }
        if !file_type.is_file() {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        if class == AiPathClass::Sensitive && !state.include_sensitive {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(value) => value,
            Err(_) => {
                state.skipped_files = state.skipped_files.saturating_add(1);
                continue;
            }
        };
        if metadata.len() > MAX_SEARCHABLE_FILE_BYTES {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        let mut file = match File::open(&path) {
            Ok(value) => value,
            Err(_) => {
                state.skipped_files = state.skipped_files.saturating_add(1);
                continue;
            }
        };
        if file.read_to_end(&mut bytes).is_err() {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            state.skipped_files = state.skipped_files.saturating_add(1);
            continue;
        };
        state.searched_files = state.searched_files.saturating_add(1);
        for (line_index, line) in text.lines().enumerate() {
            if state.matches.len() >= state.limit {
                state.truncated = true;
                break;
            }
            let line_lower = line.to_lowercase();
            if let Some(byte_index) = line_lower.find(&state.query_lower) {
                let column = line_lower[..byte_index].chars().count().saturating_add(1);
                state.matches.push(AiSearchMatch {
                    relative_path: relative_string.clone(),
                    line: u64::try_from(line_index.saturating_add(1)).unwrap_or(u64::MAX),
                    column: u64::try_from(column).unwrap_or(u64::MAX),
                    excerpt: truncate_chars(line.trim(), MAX_SEARCH_EXCERPT_CHARS),
                    sensitive: class == AiPathClass::Sensitive,
                });
            }
        }
    }
    Ok(())
}

fn authorize_existing_file_path(
    app: &AppHandle,
    session: &AiSession,
    relative_path: &str,
    write: bool,
) -> Result<(AiPathClass, PathBuf), String> {
    let workspace = find_workspace(app, &session.workspace_id)?;
    let root = canonical_workspace_root(&workspace.local_path)?;
    let class = ensure_ai_path_allowed(&root, relative_path)?;
    if class == AiPathClass::Sensitive {
        ai_gateway::authorize_session(
            app,
            &session.id,
            if write {
                "sensitive.write"
            } else {
                "sensitive.read"
            },
        )?;
    }
    let target = resolve_target_path(&root, relative_path, false)?;
    let metadata = fs::symlink_metadata(&target)
        .map_err(|error| format!("Could not inspect workspace file: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Workspace path is not a regular file.".into());
    }
    Ok((class, target))
}

fn acquire_observe(
    coordinator: &WorkspaceMutationCoordinator,
    session: &AiSession,
) -> Result<WorkspaceMutationLease, String> {
    coordinator
        .acquire(
            &session.workspace_id,
            &format!("ai:{}", session.client_id),
            WorkspaceOperationKind::Observe,
        )
        .map_err(|error| error.to_string())
}

fn record_tool_result<T>(
    app: &AppHandle,
    session: &AiSession,
    capability: &str,
    tool_name: &str,
    started: Instant,
    result: &Result<T, String>,
) -> Result<(), String> {
    ai_gateway::record_audit(
        app,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: Some(capability),
            tool_name,
            outcome: if result.is_ok() { "success" } else { "failed" },
            duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            operation_id: None,
            detail_code: Some(if result.is_ok() {
                "ok"
            } else {
                "operation_failed"
            }),
        },
    )
}

pub(crate) fn canonical_workspace_root(workspace_root: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(workspace_root);
    if !root.is_dir() {
        return Err("Workspace directory no longer exists or is not accessible.".into());
    }
    root.canonicalize()
        .map_err(|error| format!("Could not resolve workspace root: {error}"))
}

pub(crate) fn normalize_relative_path(relative_path: &str) -> Result<String, String> {
    Ok(validate_portable_relative_path(relative_path)?.join("/"))
}

pub(crate) fn validate_portable_relative_path(relative_path: &str) -> Result<Vec<String>, String> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 {
        return Err("Workspace path is empty or exceeds the portable path limit.".into());
    }
    if trimmed.contains('\\') {
        return Err("Workspace paths must use forward slashes.".into());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(
            "Absolute filesystem paths are not accepted by the AI workspace gateway.".into(),
        );
    }
    let mut segments = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err("Workspace path contains traversal or non-normal components.".into());
        };
        let segment = value
            .to_str()
            .ok_or_else(|| "Workspace path must be valid UTF-8.".to_string())?;
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.len() > 255
            || segment.contains(':')
            || segment.contains('\0')
            || segment.ends_with(' ')
            || segment.ends_with('.')
        {
            return Err("Workspace path contains a non-portable segment.".into());
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or(segment)
            .to_ascii_uppercase();
        if is_windows_reserved_name(&stem) {
            return Err("Workspace path contains a Windows-reserved file name.".into());
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        return Err("Workspace path has no usable file name.".into());
    }
    Ok(segments)
}

fn is_windows_reserved_name(value: &str) -> bool {
    matches!(value, "CON" | "PRN" | "AUX" | "NUL")
        || value
            .strip_prefix("COM")
            .and_then(|suffix| suffix.parse::<u8>().ok())
            .map(|number| (1..=9).contains(&number))
            .unwrap_or(false)
        || value
            .strip_prefix("LPT")
            .and_then(|suffix| suffix.parse::<u8>().ok())
            .map(|number| (1..=9).contains(&number))
            .unwrap_or(false)
}

pub(crate) fn classify_relative_path(relative_path: &str) -> Result<AiPathClass, String> {
    let segments = validate_portable_relative_path(relative_path)?;
    if segments.iter().any(|segment| {
        segment.eq_ignore_ascii_case(".git")
            || (segment.starts_with(".atrisbridge-")
                && !segment.eq_ignore_ascii_case(".atrisbridgeignore"))
    }) {
        return Ok(AiPathClass::Denied);
    }
    let name = segments.last().expect("validated path has segment");
    let lower_name = name.to_ascii_lowercase();
    let sensitive_name = lower_name == ".env"
        || lower_name.starts_with(".env.")
        || SENSITIVE_FILE_NAMES
            .iter()
            .any(|candidate| lower_name == candidate.to_ascii_lowercase())
        || lower_name.contains("secret")
        || lower_name.contains("credential")
        || lower_name == ".atrisbridgeignore";
    let sensitive_extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            SENSITIVE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false);
    Ok(if sensitive_name || sensitive_extension {
        AiPathClass::Sensitive
    } else {
        AiPathClass::Normal
    })
}

pub(crate) fn ensure_ai_path_allowed(
    root: &Path,
    relative_path: &str,
) -> Result<AiPathClass, String> {
    let normalized = normalize_relative_path(relative_path)?;
    let class = classify_relative_path(&normalized)?;
    if class == AiPathClass::Denied {
        return Err("Workspace path is blocked by the AtrisBridge AI hard-deny policy.".into());
    }
    let relative = Path::new(&normalized);
    if is_builtin_directory_excluded(relative) {
        return Err("Workspace path is excluded from AI access by built-in project rules.".into());
    }
    let matcher = build_custom_ignore(root)?;
    let candidate = root.join(relative);
    if is_custom_ignored(&candidate, false, matcher.as_ref()) {
        return Err("Workspace path is excluded by .atrisbridgeignore.".into());
    }
    Ok(class)
}

pub(crate) fn resolve_target_path(
    canonical_root: &Path,
    relative_path: &str,
    create_parents: bool,
) -> Result<PathBuf, String> {
    let segments = validate_portable_relative_path(relative_path)?;
    let (file_name, parents) = segments
        .split_last()
        .ok_or_else(|| "Workspace path has no file name.".to_string())?;
    let mut parent = canonical_root.to_path_buf();
    for segment in parents {
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("Workspace path crosses an unsafe local parent.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&next).map_err(|error| {
                    format!("Could not create workspace parent directory: {error}")
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("Workspace parent directory does not exist.".into());
            }
            Err(error) => return Err(format!("Could not inspect workspace parent: {error}")),
        }
        parent = next
            .canonicalize()
            .map_err(|error| format!("Could not resolve workspace parent: {error}"))?;
        if !parent.starts_with(canonical_root) {
            return Err("Workspace path escaped the authorized workspace root.".into());
        }
    }
    let target = parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err("Workspace target is a symbolic link and cannot be accessed by AI.".into());
        }
    }
    Ok(target)
}

pub(crate) fn resolve_future_target_path(
    canonical_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, String> {
    let segments = validate_portable_relative_path(relative_path)?;
    let (file_name, parents) = segments
        .split_last()
        .ok_or_else(|| "Workspace path has no file name.".to_string())?;
    let mut parent = canonical_root.to_path_buf();
    let mut missing_parent = false;
    for segment in parents {
        if missing_parent {
            parent.push(segment);
            continue;
        }
        let next = parent.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("Workspace path crosses an unsafe local parent.".into());
                }
                parent = next
                    .canonicalize()
                    .map_err(|error| format!("Could not resolve workspace parent: {error}"))?;
                if !parent.starts_with(canonical_root) {
                    return Err("Workspace path escaped the authorized workspace root.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing_parent = true;
                parent = next;
            }
            Err(error) => return Err(format!("Could not inspect workspace parent: {error}")),
        }
    }
    Ok(parent.join(file_name))
}

pub(crate) fn regular_file_evidence(path: &Path) -> Result<Option<(u64, String)>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not inspect workspace path: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Workspace path is not a regular file.".into());
    }
    scanner::fingerprint_file(path).map(Some)
}

pub(crate) fn file_matches(
    path: &Path,
    expected_size: u64,
    expected_hash: &str,
) -> Result<bool, String> {
    Ok(regular_file_evidence(path)?
        .map(|(size, hash)| size == expected_size && hash == expected_hash)
        .unwrap_or(false))
}

pub(crate) fn ensure_absent(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(format!("{label} already exists.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not inspect {label}: {error}")),
    }
}

fn build_custom_ignore(root: &Path) -> Result<Option<Gitignore>, String> {
    let ignore_path = root.join(".atrisbridgeignore");
    if !ignore_path.exists() {
        return Ok(None);
    }
    let mut builder = GitignoreBuilder::new(root);
    if let Some(error) = builder.add(&ignore_path) {
        return Err(format!("Could not read .atrisbridgeignore: {error}"));
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| format!("Invalid .atrisbridgeignore rules: {error}"))
}

fn is_custom_ignored(path: &Path, is_dir: bool, matcher: Option<&Gitignore>) -> bool {
    matcher
        .map(|matcher| {
            matcher
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
        })
        .unwrap_or(false)
}

fn is_builtin_directory_excluded(relative: &Path) -> bool {
    relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|component| {
            BUILTIN_DIRECTORY_EXCLUDES
                .iter()
                .any(|candidate| component.eq_ignore_ascii_case(candidate))
        })
}

fn relative_to_string(relative: &Path) -> String {
    relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn modified_at(path: &Path) -> Result<Option<String>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not read workspace file metadata: {error}"))?;
    Ok(metadata
        .modified()
        .ok()
        .map(|value| DateTime::<Utc>::from(value).to_rfc3339()))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_paths_reject_traversal_and_windows_ambiguity() {
        assert!(validate_portable_relative_path("../secret.txt").is_err());
        assert!(validate_portable_relative_path("C:/secret.txt").is_err());
        assert!(validate_portable_relative_path("src\\main.rs").is_err());
        assert!(validate_portable_relative_path("src/CON.txt").is_err());
        assert!(validate_portable_relative_path("src/main.rs").is_ok());
    }

    #[test]
    fn classifies_sensitive_and_hard_denied_paths() {
        assert_eq!(
            classify_relative_path("src/.env.production").expect("env"),
            AiPathClass::Sensitive
        );
        assert_eq!(
            classify_relative_path("certs/client.pfx").expect("pfx"),
            AiPathClass::Sensitive
        );
        assert_eq!(
            classify_relative_path(".git/config").expect("git"),
            AiPathClass::Denied
        );
        assert_eq!(
            classify_relative_path("src/main.rs").expect("source"),
            AiPathClass::Normal
        );
    }
}
