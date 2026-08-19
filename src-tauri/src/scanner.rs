use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path},
    time::Instant,
};

use chrono::{DateTime, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::models::{ScanFile, ScanReport};

const PREVIEW_LIMIT: usize = 250;
const HASH_BUFFER_SIZE: usize = 64 * 1024;
const BUILTIN_CASE_INSENSITIVE_DIRECTORY_IGNORES: &[&str] = &[".git", ".vs", ".idea", ".next"];
const BUILTIN_GENERATED_DIRECTORY_IGNORES: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "bin",
    "obj",
    "target",
    "coverage",
];
const BUILTIN_SECRET_EXTENSIONS: &[&str] = &["pem", "key", "pfx", "p12"];
const BUILTIN_RESERVED_FILE_NAMES: &[&str] = &[".atrisbridge-key-check"];

pub struct ScanOutcome {
    pub report: ScanReport,
    pub inventory: Vec<ScanFile>,
}

struct ScanState {
    file_count: u64,
    directory_count: u64,
    total_bytes: u64,
    skipped_entries: u64,
    files: Vec<ScanFile>,
    warnings: Vec<String>,
}

pub fn scan(workspace_id: &str, root: &Path) -> Result<ScanOutcome, String> {
    if !root.is_dir() {
        return Err("Workspace directory no longer exists or is not accessible.".into());
    }

    let started = Instant::now();
    let matcher = build_custom_ignore(root)?;
    let mut state = ScanState {
        file_count: 0,
        directory_count: 0,
        total_bytes: 0,
        skipped_entries: 0,
        files: Vec::new(),
        warnings: Vec::new(),
    };

    visit_directory(root, root, matcher.as_ref(), &mut state)?;
    state
        .files
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let preview_truncated = state.files.len() > PREVIEW_LIMIT;
    let preview = state.files.iter().take(PREVIEW_LIMIT).cloned().collect();
    let scanned_at = Utc::now().to_rfc3339();

    Ok(ScanOutcome {
        report: ScanReport {
            workspace_id: workspace_id.to_owned(),
            scanned_at,
            duration_ms: started.elapsed().as_millis(),
            file_count: state.file_count,
            directory_count: state.directory_count,
            total_bytes: state.total_bytes,
            skipped_entries: state.skipped_entries,
            preview_truncated,
            files: preview,
            warnings: state.warnings,
        },
        inventory: state.files,
    })
}

pub fn fingerprint_file(path: &Path) -> Result<(u64, String), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not read upload candidate metadata: {error}"))?;
    if !metadata.is_file() {
        return Err("Upload candidate is no longer a regular file.".into());
    }
    let digest = hash_file(path)
        .map_err(|error| format!("Could not fingerprint upload candidate: {error}"))?;
    Ok((metadata.len(), digest))
}

pub fn is_path_ignored_for_sync(root: &Path, relative_path: &str) -> Result<bool, String> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Sync candidate contains an unsafe relative path.".into());
    }
    if is_builtin_ignored(relative, false) {
        return Ok(true);
    }

    let matcher = build_custom_ignore(root)?;
    Ok(is_custom_ignored(
        &root.join(relative),
        false,
        matcher.as_ref(),
    ))
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

fn visit_directory(
    root: &Path,
    directory: &Path,
    matcher: Option<&Gitignore>,
    state: &mut ScanState,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("Could not read {}: {error}", directory.display()))?;

    for entry_result in entries {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(error) => {
                push_warning(state, format!("Could not read directory entry: {error}"));
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                push_warning(
                    state,
                    format!("Could not inspect {}: {error}", path.display()),
                );
                continue;
            }
        };

        if file_type.is_symlink() {
            state.skipped_entries += 1;
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path);
        let is_dir = file_type.is_dir();
        if is_builtin_ignored(relative, is_dir) || is_custom_ignored(&path, is_dir, matcher) {
            state.skipped_entries += 1;
            continue;
        }

        if is_dir {
            state.directory_count += 1;
            if let Err(error) = visit_directory(root, &path, matcher, state) {
                push_warning(state, error);
            }
            continue;
        }

        if !file_type.is_file() {
            state.skipped_entries += 1;
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                push_warning(
                    state,
                    format!("Could not read metadata for {}: {error}", path.display()),
                );
                continue;
            }
        };

        let digest = match hash_file(&path) {
            Ok(digest) => digest,
            Err(error) => {
                push_warning(state, format!("Could not hash {}: {error}", path.display()));
                continue;
            }
        };

        state.file_count += 1;
        state.total_bytes = state.total_bytes.saturating_add(metadata.len());

        let modified_at = metadata.modified().ok().map(|value| {
            let datetime: DateTime<Utc> = value.into();
            datetime.to_rfc3339()
        });
        state.files.push(ScanFile {
            relative_path: normalized_relative_path(relative),
            size: metadata.len(),
            modified_at,
            blake3: digest,
        });
    }

    Ok(())
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];

    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher.finalize().to_hex().to_string())
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

fn is_builtin_directory_ignored(component: &str) -> bool {
    BUILTIN_CASE_INSENSITIVE_DIRECTORY_IGNORES
        .iter()
        .any(|ignored| component.eq_ignore_ascii_case(ignored))
        || BUILTIN_GENERATED_DIRECTORY_IGNORES
            .iter()
            .any(|ignored| component == *ignored)
}

fn has_builtin_ignored_directory(relative: &Path, is_dir: bool) -> bool {
    let mut components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if !is_dir {
        components.pop();
    }
    components.into_iter().any(is_builtin_directory_ignored)
}

fn is_builtin_ignored(relative: &Path, is_dir: bool) -> bool {
    if !is_dir {
        if let Some(name) = relative.file_name().and_then(|value| value.to_str()) {
            if BUILTIN_RESERVED_FILE_NAMES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
            {
                return true;
            }
        }
    }
    if has_builtin_ignored_directory(relative, is_dir) {
        return true;
    }

    if is_dir {
        return false;
    }

    let Some(name) = relative.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }

    if name.starts_with(".atrisbridge-") && (name.ends_with(".part") || name.ends_with(".bak")) {
        return true;
    }

    relative
        .extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            BUILTIN_SECRET_EXTENSIONS
                .iter()
                .any(|ignored| extension.eq_ignore_ascii_case(ignored))
        })
        .unwrap_or(false)
}

fn normalized_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn push_warning(state: &mut ScanState, warning: String) {
    if state.warnings.len() < 20 {
        state.warnings.push(warning);
    }
}

pub fn default_ignore_file() -> &'static str {
    r#"# AtrisBridge project ignore rules
# Gitignore syntax is supported. Built-in safety rules are applied even without this file.

# Generated output
node_modules/
dist/
build/
bin/
obj/
target/
coverage/

# IDE state
.vs/
.idea/

# Local secrets
.env
.env.*
*.pem
*.key
*.pfx
*.p12

# Add project-specific generated or sensitive paths below.
"#
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use uuid::Uuid;

    use super::*;

    #[test]
    fn normalizes_path_components_for_journal_keys() {
        let path = PathBuf::from("src").join("feature").join("file.rs");
        assert_eq!(normalized_relative_path(&path), "src/feature/file.rs");
    }

    #[test]
    fn generated_directory_filters_do_not_hide_domain_directories() {
        assert!(is_builtin_ignored(Path::new("target/debug/app.exe"), false));
        assert!(is_builtin_ignored(Path::new("src/bin/app.dll"), false));
        assert!(!is_builtin_ignored(Path::new("target"), false));
        assert!(!is_builtin_ignored(
            Path::new("ViewModel/Target/TargetDetailWindow.xaml.cs"),
            false
        ));
        assert!(!is_builtin_ignored(
            Path::new("Services/Build/BuildService.cs"),
            false
        ));
    }

    #[test]
    fn blocks_known_secret_extensions() {
        assert!(is_builtin_ignored(
            Path::new("certificates/client.pfx"),
            false
        ));
        assert!(is_builtin_ignored(Path::new(".env.local"), false));
        assert!(is_builtin_ignored(
            Path::new("src/.atrisbridge-sync-abcdef0123456789.part"),
            false
        ));
        assert!(is_builtin_ignored(
            Path::new("src/.atrisbridge-abcdef0123456789.bak"),
            false
        ));
        assert!(!is_builtin_ignored(Path::new("src/main.rs"), false));
    }

    #[test]
    fn restore_candidates_reuse_builtin_and_custom_ignore_rules() {
        let root =
            std::env::temp_dir().join(format!("atrisbridge-ignore-policy-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("temp root");
        fs::write(root.join(".atrisbridgeignore"), "private/\n*.generated\n").expect("ignore file");

        assert!(is_path_ignored_for_sync(&root, ".env.production").expect("built-in env"));
        assert!(
            is_path_ignored_for_sync(&root, "node_modules/package/index.js")
                .expect("built-in directory")
        );
        assert!(is_path_ignored_for_sync(&root, "private/customer.txt").expect("custom directory"));
        assert!(is_path_ignored_for_sync(&root, "cache/data.generated").expect("custom extension"));
        assert!(!is_path_ignored_for_sync(&root, "src/main.rs").expect("allowed source"));
        assert!(
            !is_path_ignored_for_sync(&root, "ViewModel/Target/TargetDetailWindow.xaml.cs")
                .expect("domain target directory")
        );

        fs::remove_dir_all(root).expect("cleanup temp root");
    }
}
