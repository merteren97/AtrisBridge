use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

/// Copies a regular file into a destination that must not already exist and
/// flushes the destination through the same writable handle that performed the
/// copy. This avoids Windows `ERROR_ACCESS_DENIED` failures caused by reopening
/// a copied file read-only and then calling `sync_all()` on that handle.
pub fn copy_new_file(source: &Path, destination: &Path) -> io::Result<u64> {
    let mut source_file = File::open(source)?;
    let source_permissions = source_file.metadata()?.permissions();
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;

    let result = (|| -> io::Result<u64> {
        let copied = io::copy(&mut source_file, &mut destination_file)?;
        destination_file.flush()?;
        destination_file.sync_all()?;
        Ok(copied)
    })();

    drop(destination_file);

    if let Err(error) = result {
        let _ = remove_regular_file(destination);
        return Err(error);
    }

    if let Err(error) = fs::set_permissions(destination, source_permissions) {
        let _ = remove_regular_file(destination);
        return Err(error);
    }

    result
}

/// Removes an AtrisBridge-owned regular file without following symlinks.
/// Windows read-only attributes are cleared only for that exact file so stale
/// recovery artifacts can be cleaned safely after an interrupted write.
pub fn remove_regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to remove a non-regular file",
        ));
    }

    #[cfg(windows)]
    if metadata.permissions().readonly() {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }

    fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};
    use uuid::Uuid;

    fn test_root() -> std::path::PathBuf {
        env::temp_dir().join(format!("atrisbridge-durable-fs-{}", Uuid::new_v4()))
    }

    #[test]
    fn durable_copy_round_trip_uses_a_writable_destination_handle() {
        let root = test_root();
        fs::create_dir_all(&root).expect("create test root");
        let source = root.join("source.txt");
        let destination = root.join("destination.txt");
        fs::write(&source, b"AtrisBridge durable copy").expect("write source");

        let copied = copy_new_file(&source, &destination).expect("durable copy");
        assert_eq!(copied, 24);
        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"AtrisBridge durable copy"
        );

        fs::remove_dir_all(&root).expect("cleanup test root");
    }

    #[test]
    fn durable_copy_never_overwrites_an_existing_destination() {
        let root = test_root();
        fs::create_dir_all(&root).expect("create test root");
        let source = root.join("source.txt");
        let destination = root.join("destination.txt");
        fs::write(&source, b"new").expect("write source");
        fs::write(&destination, b"existing").expect("write destination");

        assert!(copy_new_file(&source, &destination).is_err());
        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"existing"
        );

        fs::remove_dir_all(&root).expect("cleanup test root");
    }

    #[test]
    fn cleanup_rejects_symlinks_or_non_files() {
        let root = test_root();
        fs::create_dir_all(&root).expect("create test root");
        assert!(remove_regular_file(&root).is_err());
        fs::remove_dir_all(&root).expect("cleanup test root");
    }
}
