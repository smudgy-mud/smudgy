use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use tempfile::Builder;

/// Replaces `path` without ever exposing a partially written destination.
///
/// The temporary file lives beside the destination so persisting it stays on
/// one filesystem. Its contents are synced before the atomic replacement; on
/// Unix, the parent directory is synced afterward so the replacement survives
/// a crash once this function returns successfully.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomic_with(path, |file| file.write_all(contents))
}

fn write_atomic_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };

    let mut temporary = Builder::new()
        .prefix(".smudgy-write-")
        .tempfile_in(parent)?;
    write(temporary.as_file_mut())?;
    if let Some(permissions) = existing_permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|err| err.error)?;
    sync_parent(parent)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_new_file_without_leaving_a_temporary_sibling() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("settings.json");

        write_atomic(&path, br#"{"theme":"dark"}"#).expect("atomic write");

        assert_eq!(fs::read(&path).expect("saved file"), br#"{"theme":"dark"}"#);
        assert_eq!(
            fs::read_dir(dir.path()).expect("directory entries").count(),
            1
        );
    }

    #[test]
    fn replaces_an_existing_file_and_preserves_its_permissions() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("settings.json");
        fs::write(&path, b"old").expect("old file");
        let permissions = fs::metadata(&path).expect("old metadata").permissions();

        write_atomic(&path, b"new").expect("atomic replacement");

        assert_eq!(fs::read(&path).expect("saved file"), b"new");
        assert_eq!(
            fs::metadata(&path).expect("new metadata").permissions(),
            permissions
        );
        assert_eq!(
            fs::read_dir(dir.path()).expect("directory entries").count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_a_non_default_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("settings.json");
        fs::write(&path, b"old").expect("old file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("set non-default mode");

        write_atomic(&path, b"new").expect("atomic replacement");

        let mode = fs::metadata(&path)
            .expect("new metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn writer_failure_preserves_the_old_file_and_cleans_up_the_temporary_file() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("settings.json");
        fs::write(&path, b"old").expect("old file");

        let error = write_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        })
        .expect_err("the injected failure must be returned");

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).expect("old file"), b"old");
        assert_eq!(
            fs::read_dir(dir.path()).expect("directory entries").count(),
            1
        );
    }

    #[test]
    fn replacement_failure_preserves_the_destination_and_cleans_up_the_temporary_file() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("settings.json");
        fs::create_dir(&path).expect("destination directory");

        write_atomic(&path, b"new").expect_err("a file cannot replace a directory");

        assert!(path.is_dir());
        assert_eq!(
            fs::read_dir(dir.path()).expect("directory entries").count(),
            1
        );
    }
}
