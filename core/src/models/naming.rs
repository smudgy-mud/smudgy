//! Centralized validation for user-facing names that become filesystem path
//! components (and, for packages, `smudgy://owner/name` import specifiers).
//!
//! There are two tiers, because the consumers differ:
//!
//! - [`validate_name`], [`validate_folder_path`], and [`validate_module_subpath`]
//!   are **permissive** (a denylist): anything is allowed except characters that
//!   are illegal or unsafe in a cross-platform filename. This lets folders,
//!   scripts, aliases, triggers, hotkeys, and modules use spaces and friendly
//!   punctuation — these names only ever land on disk.
//! - [`validate_package_name`] stays **slug-like** (letters, digits, `-`, `_`):
//!   package names are published, globally-addressable coordinates embedded in
//!   `smudgy://owner/name` import URLs, so they avoid spaces and other characters
//!   that would need percent-encoding.
//!
//! Uniqueness should always be checked with [`names_conflict`], which folds case
//! to match the case-insensitive filesystems these names land on (Windows,
//! macOS): without it, `Combat` and `combat` pass an in-app check but collide on
//! disk.

/// Maximum length, in characters, of a single name or path segment. Filesystem
/// components are capped at 255 bytes; we stay well under that for sane UX and to
/// leave room for a `.ts`/`.js` extension on the on-disk file.
pub const MAX_NAME_LEN: usize = 64;

/// Characters that are illegal in a file or directory name on at least one major
/// platform (Windows is the strictest). These names map directly onto paths, so
/// we reject them everywhere.
const ILLEGAL_NAME_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Windows reserved device names, matched case-insensitively and ignoring any
/// extension. Creating a file whose stem is one of these fails on Windows.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a single user-facing name (a folder segment, or a
/// script/alias/trigger/hotkey name). Permissive: letters, digits, spaces, and
/// punctuation are all allowed; only what is illegal or unsafe as a filename is
/// rejected.
///
/// # Errors
///
/// Returns a human-readable message describing why `name` is unacceptable.
pub fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "Name cannot be longer than {MAX_NAME_LEN} characters"
        ));
    }
    for c in name.chars() {
        if ILLEGAL_NAME_CHARS.contains(&c) {
            return Err(format!("Name cannot contain '{c}'"));
        }
        if c.is_control() {
            return Err("Name cannot contain control characters".to_string());
        }
        // A single space is fine; tabs, newlines, and other whitespace are not.
        if c.is_whitespace() && c != ' ' {
            return Err("Name cannot contain tabs or line breaks".to_string());
        }
    }
    // Windows strips trailing dots/spaces, and a leading dot collides with the
    // `.`/`..` directory entries (and creates a hidden file on Unix).
    if name.starts_with('.') || name.ends_with('.') {
        return Err("Name cannot start or end with a '.'".to_string());
    }
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(format!("'{name}' is a reserved name"));
    }
    Ok(())
}

/// Validates a `/`-separated folder path, segment by segment, with
/// [`validate_name`].
///
/// # Errors
///
/// Returns a human-readable message if the path is empty or any segment is
/// invalid.
pub fn validate_folder_path(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    for segment in path.split('/') {
        if segment.trim().is_empty() {
            return Err("Folder path cannot contain empty segments".to_string());
        }
        validate_name(segment)?;
    }
    Ok(())
}

/// Validates a module file subpath (e.g. `lib/util.ts`): `/`-separated, each
/// segment a valid [`validate_name`], and no `.`/`..` traversal segments that
/// could escape the `modules/` root.
///
/// # Errors
///
/// Returns a human-readable message if the subpath is empty, escapes its root,
/// or contains an invalid segment.
pub fn validate_module_subpath(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Module name cannot be empty".to_string());
    }
    if path.starts_with('/') {
        return Err("Module name cannot start with '/'".to_string());
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err("Module path cannot contain empty segments".to_string());
        }
        if segment == "." || segment == ".." {
            return Err("Module path cannot contain '.' or '..' segments".to_string());
        }
        validate_name(segment)?;
    }
    Ok(())
}

/// Validates a package name. Stricter than [`validate_name`]: package names are
/// published, globally-addressable coordinates embedded in `smudgy://owner/name`
/// import URLs, so they stay slug-like — letters, digits, `-`, and `_`, with no
/// spaces, dots, or other punctuation.
///
/// # Errors
///
/// Returns a human-readable message if the name is empty, too long, or contains
/// a disallowed character.
pub fn validate_package_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Package name cannot be empty".to_string());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "Package name cannot be longer than {MAX_NAME_LEN} characters"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Package names may only contain letters, digits, '-' and '_'".to_string());
    }
    Ok(())
}

/// Whether two names collide for uniqueness purposes. Folds case because the
/// filesystems these names land on (Windows, macOS) are case-insensitive, so
/// `Combat` and `combat` would map to the same file.
#[must_use]
pub fn names_conflict(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_spaces_and_punctuation() {
        for name in ["Combat (PvP)", "healer's macros", "My Triggers!", "v1.2 notes"] {
            assert!(validate_name(name).is_ok(), "should allow {name:?}");
        }
    }

    #[test]
    fn rejects_illegal_filename_chars() {
        for name in ["a/b", "a\\b", "a:b", "a|b", "a?b", "a*b", "a\"b", "a<b", "a>b"] {
            assert!(validate_name(name).is_err(), "should reject {name:?}");
        }
    }

    #[test]
    fn rejects_empty_control_and_dot_edges() {
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a\tb").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("trailing.").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
    }

    #[test]
    fn rejects_reserved_device_names() {
        assert!(validate_name("CON").is_err());
        assert!(validate_name("nul").is_err());
        assert!(validate_name("COM1").is_err());
        assert!(validate_name("nul.ts").is_err());
        // Not reserved: device name as a substring is fine.
        assert!(validate_name("console").is_ok());
    }

    #[test]
    fn rejects_overlong_names() {
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn module_subpath_allows_nested_paths_but_blocks_traversal() {
        assert!(validate_module_subpath("lib/util.ts").is_ok());
        assert!(validate_module_subpath("my scripts/util.ts").is_ok());
        assert!(validate_module_subpath("../escape.ts").is_err());
        assert!(validate_module_subpath("lib/../escape.ts").is_err());
        assert!(validate_module_subpath("/abs.ts").is_err());
        assert!(validate_module_subpath("a//b.ts").is_err());
    }

    #[test]
    fn package_names_stay_slug_like() {
        assert!(validate_package_name("my-combat_pack").is_ok());
        assert!(validate_package_name("with space").is_err());
        assert!(validate_package_name("with.dot").is_err());
        assert!(validate_package_name("with/slash").is_err());
    }

    #[test]
    fn names_conflict_is_case_insensitive() {
        assert!(names_conflict("Combat", "combat"));
        assert!(names_conflict("  Combat  ", "combat"));
        assert!(!names_conflict("combat", "combat-2"));
    }
}
