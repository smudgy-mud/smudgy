//! Reading a stored workspace template back, never fatally.
//!
//! The curated-store reader (named layouts): a template file that cannot
//! be used — malformed JSON, or a schema version this build does not speak
//! — is **set aside** (renamed beside the original) rather than deleted,
//! and the caller receives a safe empty template. The per-server
//! last-session snapshots deliberately do NOT read through here: autosave
//! continuously rewrites those files, so their reader
//! (`last_session::read`) is side-effect-free instead.

use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

use super::dto;

/// The version field alone, parseable even when the rest of the document
/// belongs to a schema this build has never heard of.
#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

/// Load a workspace template from `path`:
///
/// - missing file → empty workspace, silently (the first-run case);
/// - unreadable file → empty workspace, warned, file left in place;
/// - malformed JSON → empty workspace, file set aside as `<name>.invalid`;
/// - any schema version but [`dto::SCHEMA_VERSION`] → empty workspace, file
///   set aside as `<name>.v<version>` so a build that speaks it can still
///   find the bytes;
/// - a parsed file is sanitized entry-by-entry ([`dto::Workspace::sanitized`]),
///   so partial damage costs the damaged entries only.
///
/// The unreadable original is never destroyed as part of the fallback.
#[must_use]
pub fn load_from(path: &Path) -> dto::Workspace {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return dto::Workspace::default();
        }
        Err(err) => {
            log::warn!(
                "[workspace] failed to read {}: {err}; starting with an empty workspace",
                path.display()
            );
            return dto::Workspace::default();
        }
    };

    let probe: VersionProbe = match serde_json::from_str(&raw) {
        Ok(probe) => probe,
        Err(err) => {
            log::warn!(
                "[workspace] {} is not a workspace file ({err}); setting it aside",
                path.display()
            );
            set_aside(path, "invalid");
            return dto::Workspace::default();
        }
    };
    if probe.version != dto::SCHEMA_VERSION {
        log::warn!(
            "[workspace] {} is schema version {} (this build speaks {}); setting it aside",
            path.display(),
            probe.version,
            dto::SCHEMA_VERSION
        );
        set_aside(path, &format!("v{}", probe.version));
        return dto::Workspace::default();
    }

    match serde_json::from_str::<dto::Workspace>(&raw) {
        Ok(workspace) => workspace.sanitized(),
        Err(err) => {
            log::warn!(
                "[workspace] failed to parse {}: {err}; setting it aside",
                path.display()
            );
            set_aside(path, "invalid");
            dto::Workspace::default()
        }
    }
}

/// Preserve an unusable workspace file by renaming it to `<name>.<suffix>`
/// beside the original, so the next autosave cannot overwrite the only copy
/// of whatever it held. An older set-aside file with the same suffix is
/// replaced (the freshest evidence wins); when even the rename fails the
/// file is left exactly where it is.
fn set_aside(path: &Path, suffix: &str) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let target = path.with_file_name(format!("{name}.{suffix}"));
    match fs::remove_file(&target) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            log::warn!(
                "[workspace] could not clear {}: {err}; leaving {} in place",
                target.display(),
                path.display()
            );
            return;
        }
    }
    match fs::rename(path, &target) {
        Ok(()) => log::info!(
            "[workspace] preserved the unusable workspace file as {}",
            target.display()
        ),
        Err(err) => log::warn!(
            "[workspace] could not set {} aside: {err}; leaving it in place",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE_FILE_NAME: &str = "combat.json";

    const GOLDEN: &str = include_str!("../../../session_model/testdata/workspace_v1.json");

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary directory")
    }

    #[test]
    fn missing_file_loads_the_empty_workspace() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        assert_eq!(load_from(&path), dto::Workspace::default());
        // Nothing was created or set aside.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn valid_file_loads_sanitized() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, GOLDEN).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.sessions.len(), 2);
        assert_eq!(loaded.windows.len(), 2);
        // A good file is left exactly where it was.
        assert!(path.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn malformed_json_falls_back_empty_and_preserves_the_bytes() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, b"{ definitely not json").unwrap();

        assert_eq!(load_from(&path), dto::Workspace::default());

        let aside = dir.path().join(format!("{TEMPLATE_FILE_NAME}.invalid"));
        assert!(!path.exists(), "the unusable file moved aside");
        assert_eq!(fs::read(&aside).unwrap(), b"{ definitely not json");
    }

    #[test]
    fn truncated_file_falls_back_empty_and_preserves_the_bytes() {
        // A crash mid-write of a non-atomic writer, or disk damage: valid
        // prefix, broken tail.
        let truncated = &GOLDEN[..GOLDEN.len() / 2];
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, truncated).unwrap();

        assert_eq!(load_from(&path), dto::Workspace::default());
        let aside = dir.path().join(format!("{TEMPLATE_FILE_NAME}.invalid"));
        assert_eq!(fs::read_to_string(&aside).unwrap(), truncated);
    }

    #[test]
    fn unknown_future_version_is_set_aside_unread() {
        // A plausible v9 file: same top-level shape, content this build
        // cannot interpret. The version probe must divert it before the full
        // parse ever sees it.
        let future = r#"{"version": 9, "sessions": 42, "windows": {"reshaped": true}}"#;
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, future).unwrap();

        assert_eq!(load_from(&path), dto::Workspace::default());

        let aside = dir.path().join(format!("{TEMPLATE_FILE_NAME}.v9"));
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&aside).unwrap(), future);
    }

    #[test]
    fn version_zero_is_not_this_schema() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, r#"{"version": 0}"#).unwrap();
        assert_eq!(load_from(&path), dto::Workspace::default());
        assert!(dir.path().join(format!("{TEMPLATE_FILE_NAME}.v0")).exists());
    }

    #[test]
    fn a_missing_version_field_is_malformed() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, r#"{"sessions": [], "windows": []}"#).unwrap();
        assert_eq!(load_from(&path), dto::Workspace::default());
        assert!(
            dir.path()
                .join(format!("{TEMPLATE_FILE_NAME}.invalid"))
                .exists()
        );
    }

    #[test]
    fn a_fresh_set_aside_replaces_an_older_one() {
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        let aside = dir.path().join(format!("{TEMPLATE_FILE_NAME}.invalid"));
        fs::write(&aside, b"older evidence").unwrap();
        fs::write(&path, b"newer evidence, also broken").unwrap();

        assert_eq!(load_from(&path), dto::Workspace::default());
        assert_eq!(fs::read(&aside).unwrap(), b"newer evidence, also broken");
    }

    #[test]
    fn semantically_damaged_but_parseable_files_degrade_in_place() {
        // Parseable JSON with a bad record loses the record, keeps the rest,
        // and is NOT set aside — the sanitized model simply becomes the next
        // snapshot.
        let damaged = r#"{
            "version": 1,
            "sessions": [
                {"id": 1, "server": "Arctic", "profile": "imm", "connect": true},
                {"id": 2, "server": "", "profile": "broken", "connect": false}
            ],
            "windows": [{
                "id": 1,
                "geometry": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0, "scale": 1.0},
                "clusters": [
                    {"weight": 1.0, "root": {"group": {"tabs": [{"slot": 1, "id": "main"}], "selected": 0}}},
                    {"weight": 1.0, "root": {"group": {"tabs": [{"slot": 2, "id": "main"}], "selected": 0}}}
                ]
            }]
        }"#;
        let dir = dir();
        let path = dir.path().join(TEMPLATE_FILE_NAME);
        fs::write(&path, damaged).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.windows.len(), 1);
        assert_eq!(loaded.windows[0].clusters.len(), 1);
        assert!(path.exists(), "a parseable file stays in place");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
