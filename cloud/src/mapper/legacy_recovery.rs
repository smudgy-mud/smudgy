//! Retire pre-format-3 cloud queues without decoding or replaying their edits.
//!
//! The old viewer directory is never a replay source. New writes live in its
//! `format-3` child, so failure or interruption here cannot send an old edit.

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use super::pending::{durable_rename, sync_directory};

/// A persistent recovery notice for the currently authenticated account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCloudRecovery {
    pub folder: PathBuf,
    /// Originals remain outside the replay queue if archiving could not finish.
    pub incomplete: bool,
}

pub(super) fn archive(
    viewer_root: &Path,
    commits: &Path,
    cache_root: Option<&Path>,
    namespace: &str,
    viewer: Uuid,
) -> Option<LegacyCloudRecovery> {
    let folder = viewer_root.join("recovery").join("pre-format-3");
    match archive_inner(viewer_root, &folder, commits, cache_root, namespace, viewer) {
        Ok(false) => None,
        Ok(true) => Some(LegacyCloudRecovery {
            folder,
            incomplete: false,
        }),
        Err(error) => {
            log::warn!(
                "Legacy cloud recovery in {} is incomplete; originals will not be replayed: {error}",
                folder.display()
            );
            Some(LegacyCloudRecovery {
                folder,
                incomplete: true,
            })
        }
    }
}

fn archive_inner(
    viewer_root: &Path,
    folder: &Path,
    commits: &Path,
    cache_root: Option<&Path>,
    namespace: &str,
    viewer: Uuid,
) -> io::Result<bool> {
    let originals = files_in(viewer_root)?;
    if originals.is_empty() {
        return Ok(folder.join("README.txt").is_file());
    }
    create_directory(folder)?;
    // Copy supporting data before retiring even one journal record. No typed
    // mutation deserialization: obsolete fields and corrupt bytes survive.
    if let Some(cache_root) = cache_root {
        for version in ["v2", "v3", "v4", ""] {
            let source = cache_root.join(version).join(viewer.to_string());
            let target = folder.join("cache").join(if version.is_empty() {
                "unversioned"
            } else {
                version
            });
            copy_tree(&source, &target)?;
        }
    }
    for path in files_in(commits)? {
        if path.extension().is_some_and(|ext| ext == "commit") {
            let bytes = fs::read(&path)?;
            // Select by actor and origin, never by an operation's old schema.
            // Malformed markers stay in place for the normal journal checker.
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if value["members"].as_array().is_some_and(|members| {
                members.iter().any(|member| {
                    member["viewer_id"].as_str() == Some(viewer.to_string().as_str())
                        && member["server_namespace"].as_str() == Some(namespace)
                        && member["map_format"].is_null()
                })
            }) {
                write_once(
                    &folder.join("commits").join(path.file_name().unwrap()),
                    &bytes,
                )?;
            }
        }
    }
    write_once(&folder.join("README.txt"), b"Smudgy cloud upgrade recovery\n\nThese older queued edits were NOT replayed against the upgraded cloud service.\nChanges already saved on the old service are in the migrated cloud snapshot.\nEdits that had not reached the old service are preserved here for manual recovery.\n\njournal/ contains original queue bytes (including dependent edits and deletion markers).\ncache/ contains this account's older cached maps, when available.\ncommits/ contains batch receipts from the local journal, when available.\nThese files can contain Private map content. They are not an automatic import format.\nKeep this folder until you have recovered any work you need.\n\nNew edits use a separate queue and sync normally. If archiving is incomplete,\nremaining originals are in this folder's grandparent directory and will not be sent.\n")?;
    let journal = folder.join("journal");
    create_directory(&journal)?;
    for source in originals {
        let target = journal.join(source.file_name().unwrap());
        // A restart or an older client can leave an existing recovery file.
        // Never overwrite a differing original.
        write_once(&target, &fs::read(&source)?)?;
        fs::remove_file(&source)?;
        sync_directory(viewer_root)?;
    }
    Ok(true)
}

fn files_in(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::other("recovery source contains a symbolic link"));
        }
        if kind.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn create_directory(path: &Path) -> io::Result<()> {
    if !path.try_exists()? {
        if let Some(parent) = path.parent() {
            create_directory(parent)?;
        }
        fs::create_dir(path).or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn write_once(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.try_exists()? {
        return if fs::read(path)? == bytes {
            Ok(())
        } else {
            Err(io::Error::other(
                "recovery snapshot already contains different bytes",
            ))
        };
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing recovery parent"))?;
    create_directory(parent)?;
    let temporary = parent.join(format!("{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    durable_rename(&temporary, path)
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::other("recovery cache contains a symbolic link"));
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        } else if kind.is_file() {
            write_once(&target.join(entry.file_name()), &fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_copy_failure_preserves_every_original_until_retry() {
        let root = std::env::temp_dir().join(format!("smudgy-recovery-{}", Uuid::new_v4()));
        let viewer = Uuid::new_v4();
        let original = root.join("viewer");
        let cache = root.join("cache");
        let folder = original.join("recovery/pre-format-3");
        fs::create_dir_all(&original).unwrap();
        fs::write(original.join("a.json"), b"first").unwrap();
        fs::write(original.join("b.json"), b"dependent").unwrap();
        let source = cache.join("v2").join(viewer.to_string());
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("map.json"), b"private map").unwrap();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("cache"), b"blocks cache copy").unwrap();
        let run = || {
            archive(
                &original,
                &root.join("commits"),
                Some(&cache),
                "origin",
                viewer,
            )
            .unwrap()
        };
        assert!(run().incomplete);
        assert_eq!(fs::read(original.join("a.json")).unwrap(), b"first");
        assert_eq!(fs::read(original.join("b.json")).unwrap(), b"dependent");
        fs::remove_file(folder.join("cache")).unwrap();
        assert!(!run().incomplete);
        assert_eq!(
            fs::read(folder.join("cache/v2/map.json")).unwrap(),
            b"private map"
        );
        assert!(!run().incomplete, "completed recovery is idempotent");
        // A downgraded client must not overwrite a previous recovery artifact.
        fs::write(original.join("a.json"), b"different draft").unwrap();
        assert!(run().incomplete);
        assert_eq!(
            fs::read(original.join("a.json")).unwrap(),
            b"different draft"
        );
        assert_eq!(fs::read(folder.join("journal/a.json")).unwrap(), b"first");
        fs::remove_dir_all(root).unwrap();
    }
}
