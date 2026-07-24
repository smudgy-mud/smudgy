//! Account and cloud-session credential storage.
//!
//! Two kinds of state live here:
//!
//! - [`AccountInfo`] — **non-secret** persisted account state, stored as plain JSON in
//!   `<smudgy_home>/account.json`.
//! - The cloud **session token** (format `smudgy_sess_<64hex>`), which is a secret. It is
//!   stored in the OS keyring (Windows Credential Manager / macOS Keychain / Secret
//!   Service) and only falls back to an obfuscated file when no keyring is available.
//!   The keyring *slot* (user) defaults to [`KEYRING_USER`] but can be pointed at a
//!   different slot via [`set_keyring_user`] (the `--keyring-user` launch flag), so a
//!   second instance run against an isolated `--data-dir` can hold a different account's
//!   token in the keyring without colliding with the default install.
//!
//! Token material is never logged, not even at trace level.

use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use smudgy_cloud::Uuid;
use std::sync::OnceLock;
use std::{fs, io, path::Path};

use super::persistence::write_atomic;

/// File name for the non-secret account state, relative to the smudgy home directory.
const ACCOUNT_FILE: &str = "account.json";

/// File name for the obfuscated session-token fallback file, relative to the smudgy
/// home directory.
const FALLBACK_TOKEN_FILE: &str = ".cloud-session";

/// Keyring service name for release builds (shared by release candidates).
/// Dev/pre-release builds use a SEPARATE service ([`keyring_service`]), so a dev
/// client's *entire* keyring — the cloud session token AND every package secret — is
/// isolated from a release client's, mirroring the dev-aware home dir. A release candidate
/// shares the release service (it behaves like a release). Isolating at the service (not
/// per-slot) keeps every smudgy keyring entry consistent.
const KEYRING_SERVICE: &str = "smudgy";
const KEYRING_SERVICE_DEV: &str = "smudgy-dev";

/// The keyring service in effect for this build (dev-aware, mirroring the home-dir split in
/// [`crate::get_smudgy_home`]). Used for the session token AND package secrets, so the whole
/// keyring is isolated per build channel. Only a [`Dev`](crate::models::settings::BuildChannel::Dev)
/// build is isolated; releases and release candidates share the `smudgy` service.
#[must_use]
pub fn keyring_service() -> &'static str {
    if crate::models::settings::is_dev_build() {
        KEYRING_SERVICE_DEV
    } else {
        KEYRING_SERVICE
    }
}

/// Default keyring user/slot name for the cloud session token. Overridable per process via
/// [`set_keyring_user`]. The slot is build-agnostic — isolation is by service.
const KEYRING_USER: &str = "cloud-session";

/// Process-wide override for the keyring user/slot (see [`set_keyring_user`]).
static KEYRING_USER_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Points session-token storage at the keyring slot `user` instead of the
/// default [`KEYRING_USER`] for the lifetime of the process.
///
/// This backs the `--keyring-user` launch flag. Paired with `--data-dir`, it
/// lets a second instance keep a *different account's* token in the OS keyring
/// without overwriting the default install's slot. Set once at startup, before
/// any token access; a second call is ignored (the first override wins).
pub fn set_keyring_user(user: impl Into<String>) {
    if KEYRING_USER_OVERRIDE.set(user.into()).is_err() {
        warn!("keyring user override already set; ignoring later override");
    }
}

/// The keyring user/slot in effect: the [`set_keyring_user`] override if one was set at
/// startup, otherwise the default [`KEYRING_USER`].
fn keyring_user() -> &'static str {
    KEYRING_USER_OVERRIDE.get().map_or(KEYRING_USER, String::as_str)
}

/// Repeating XOR key for the fallback token file. See [`obfuscate`] — this is
/// obfuscation, not encryption.
const OBFUSCATION_KEY: &[u8] = b"smudgy-local-session-obfuscation";

/// Non-secret persisted account state.
///
/// Stored as plain JSON in `<smudgy_home>/account.json`. Secrets (the session token)
/// are stored separately — see [`save_session_token`].
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct AccountInfo {
    /// The account's server-side user id, if known.
    pub user_id: Option<Uuid>,
    /// The email address the account was registered with.
    pub email: String,
    /// The account's nickname, which is its globally-unique handle, if chosen.
    pub nickname: Option<String>,
    /// Whether the account's email address has been verified.
    pub email_verified: bool,
    /// Whether the server requires the user to pick a nickname.
    pub needs_nickname: bool,
}

/// Loads the persisted account state from `<smudgy_home>/account.json`.
///
/// Returns `None` when the file is missing, unreadable, or corrupt; failures other
/// than a missing file are logged as warnings.
#[must_use]
pub fn load_account() -> Option<AccountInfo> {
    match get_smudgy_home() {
        Ok(dir) => load_account_in(&dir),
        Err(e) => {
            warn!("Failed to locate smudgy home directory while loading account info: {e:#}");
            None
        }
    }
}

/// Saves the persisted account state to `<smudgy_home>/account.json`.
///
/// # Errors
///
/// Returns an error if the smudgy home directory cannot be determined, the account
/// state cannot be serialized, or the file cannot be written.
pub fn save_account(account: &AccountInfo) -> Result<()> {
    save_account_in(&get_smudgy_home()?, account)
}

/// Deletes `<smudgy_home>/account.json`. A missing file is not an error.
///
/// # Errors
///
/// Returns an error if the smudgy home directory cannot be determined or the file
/// exists but cannot be deleted (e.g. permission issues).
pub fn clear_account() -> Result<()> {
    clear_account_in(&get_smudgy_home()?)
}

/// Loads account state from `account.json` inside `dir`. See [`load_account`].
fn load_account_in(dir: &Path) -> Option<AccountInfo> {
    let path = dir.join(ACCOUNT_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(account) => Some(account),
            Err(e) => {
                warn!("Failed to parse {}, ignoring it: {e}", path.display());
                None
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!("Failed to read {}, ignoring it: {e}", path.display());
            None
        }
    }
}

/// Saves account state to `account.json` inside `dir`. See [`save_account`].
fn save_account_in(dir: &Path, account: &AccountInfo) -> Result<()> {
    let path = dir.join(ACCOUNT_FILE);
    let json = serde_json::to_string_pretty(account).context("Failed to serialize account info")?;
    write_atomic(&path, json.as_bytes()).context(format!("Failed to write {}", path.display()))
}

/// Deletes `account.json` inside `dir`. See [`clear_account`].
fn clear_account_in(dir: &Path) -> Result<()> {
    let path = dir.join(ACCOUNT_FILE);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context(format!("Failed to delete {}", path.display())),
    }
}

/// Returns the keyring entry used for the cloud session token, using the
/// [`keyring_user`] slot in effect for this process.
fn keyring_entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(keyring_service(), keyring_user())
}

/// Stores the cloud session token.
///
/// The token is written to the OS keyring; if the keyring is unavailable (e.g. no
/// credential store on this system), it is written to an obfuscated fallback file
/// instead (see [`obfuscate`] — obfuscation, not encryption). When the keyring write
/// succeeds, any stale fallback file is removed so an old token cannot linger there.
///
/// Token material is never logged.
///
/// # Errors
///
/// Returns an error if both the keyring write and the fallback-file write fail.
pub fn save_session_token(token: &str) -> Result<()> {
    match keyring_entry().and_then(|entry| entry.set_password(token)) {
        Ok(()) => {
            // Best effort: don't leave an outdated token in the fallback file.
            if let Err(e) = get_smudgy_home().and_then(|dir| remove_token_file(&dir)) {
                warn!("Failed to remove stale session token fallback file: {e:#}");
            }
            Ok(())
        }
        Err(e) => {
            warn!(
                "OS keyring unavailable for session token storage, falling back to obfuscated file: {e}"
            );
            save_token_to_file(&get_smudgy_home()?, token)
        }
    }
}

/// Loads the cloud session token, if one is stored.
///
/// Tries the OS keyring first, then the obfuscated fallback file. Returns `None`
/// when no token is stored anywhere. Token material is never logged.
#[must_use]
pub fn load_session_token() -> Option<String> {
    match keyring_entry().and_then(|entry| entry.get_password()) {
        Ok(token) => Some(token),
        Err(e) => {
            if !matches!(e, keyring::Error::NoEntry) {
                warn!("Failed to read session token from the OS keyring: {e}");
            }
            let dir = get_smudgy_home().ok()?;
            load_token_from_file(&dir)
        }
    }
}

/// Removes the cloud session token from **both** the OS keyring and the fallback
/// file. Missing entries are fine.
///
/// # Errors
///
/// Returns an error if an existing keyring entry or fallback file could not be
/// removed; both removals are attempted regardless.
pub fn clear_session_token() -> Result<()> {
    let keyring_result = match keyring_entry().and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to delete session token from the OS keyring: {e}"
        )),
    };

    let file_result = remove_token_file(&get_smudgy_home()?);

    keyring_result?;
    file_result
}

/// XOR-obfuscates `bytes` with the repeating [`OBFUSCATION_KEY`].
///
/// **This is obfuscation, NOT encryption.** It only keeps the token from being
/// trivially readable in a text editor; anyone with this source code (or basic
/// cryptanalysis) can recover it. It exists solely as a last-resort fallback for
/// systems without an OS credential store. XOR is symmetric, so the same function
/// both obfuscates and deobfuscates.
pub(crate) fn obfuscate(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .zip(OBFUSCATION_KEY.iter().cycle())
        .map(|(byte, key)| byte ^ key)
        .collect()
}

/// Hex-encodes `bytes` as lowercase ASCII.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX_CHARS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_CHARS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Decodes a lowercase/uppercase hex string. Returns `None` on malformed input.
pub(crate) fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    fn hex_val(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }

    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((hex_val(pair[0])? << 4) | hex_val(pair[1])?);
    }
    Some(out)
}

/// Writes the obfuscated, hex-encoded token to `.cloud-session` inside `dir`.
///
/// Fallback path only — see [`save_session_token`] and [`obfuscate`].
fn save_token_to_file(dir: &Path, token: &str) -> Result<()> {
    let path = dir.join(FALLBACK_TOKEN_FILE);
    let encoded = hex_encode(&obfuscate(token.as_bytes()));
    write_atomic(&path, encoded.as_bytes()).context(format!("Failed to write {}", path.display()))
}

/// Reads the token back from `.cloud-session` inside `dir`, if present and intact.
///
/// Returns `None` on a missing, unreadable, or malformed file. Warnings logged here
/// never include token material.
fn load_token_from_file(dir: &Path) -> Option<String> {
    let path = dir.join(FALLBACK_TOKEN_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("Failed to read session token fallback file: {e}");
            return None;
        }
    };
    let Some(bytes) = hex_decode(content.trim()) else {
        warn!("Session token fallback file is malformed, ignoring it");
        return None;
    };
    if let Ok(token) = String::from_utf8(obfuscate(&bytes)) {
        Some(token)
    } else {
        warn!("Session token fallback file did not decode cleanly, ignoring it");
        None
    }
}

/// Deletes `.cloud-session` inside `dir`. A missing file is not an error.
fn remove_token_file(dir: &Path) -> Result<()> {
    let path = dir.join(FALLBACK_TOKEN_FILE);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context(format!("Failed to delete {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique temp directory per test so parallel tests don't collide.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smudgy-auth-test-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    fn sample_token() -> String {
        format!("smudgy_sess_{}", "ab12cd34".repeat(8))
    }

    #[test]
    fn obfuscation_round_trips() {
        let token = sample_token();
        let obfuscated = obfuscate(token.as_bytes());
        assert_ne!(obfuscated, token.as_bytes());
        assert_eq!(obfuscate(&obfuscated), token.as_bytes());
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(hex_decode(&hex_encode(&bytes)), Some(bytes));
        assert_eq!(hex_decode(""), Some(Vec::new()));
        assert!(hex_decode("abc").is_none()); // odd length
        assert!(hex_decode("zz").is_none()); // non-hex
    }

    #[test]
    fn token_file_round_trips_and_is_not_plaintext() {
        let dir = temp_dir("token");
        let token = sample_token();

        save_token_to_file(&dir, &token).expect("save should succeed");

        let raw = fs::read_to_string(dir.join(FALLBACK_TOKEN_FILE)).expect("file should exist");
        assert!(
            !raw.contains(&token) && !raw.contains("smudgy_sess"),
            "fallback file must not contain the raw token"
        );

        assert_eq!(load_token_from_file(&dir).as_deref(), Some(token.as_str()));

        remove_token_file(&dir).expect("remove should succeed");
        assert_eq!(load_token_from_file(&dir), None);
        remove_token_file(&dir).expect("removing a missing file is fine");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_token_file_loads_as_none() {
        let dir = temp_dir("token-corrupt");
        fs::write(dir.join(FALLBACK_TOKEN_FILE), "not hex at all!").expect("write");
        assert_eq!(load_token_from_file(&dir), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn account_round_trips() {
        let dir = temp_dir("account");
        let account = AccountInfo {
            user_id: Some(Uuid::new_v4()),
            email: "user@example.com".to_string(),
            nickname: Some("Walter".to_string()),
            email_verified: true,
            needs_nickname: false,
        };

        save_account_in(&dir, &account).expect("save should succeed");
        assert_eq!(load_account_in(&dir), Some(account));

        clear_account_in(&dir).expect("clear should succeed");
        assert_eq!(load_account_in(&dir), None);
        clear_account_in(&dir).expect("clearing a missing file is fine");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_account_file_loads_as_none() {
        let dir = temp_dir("account-corrupt");
        fs::write(dir.join(ACCOUNT_FILE), "{ this is not json").expect("write");
        assert_eq!(load_account_in(&dir), None);
        fs::remove_dir_all(&dir).ok();
    }
}
