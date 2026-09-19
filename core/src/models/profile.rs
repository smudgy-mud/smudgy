// Models related to profile configurations

use crate::get_smudgy_home;
use crate::models::auth::{hex_decode, hex_encode, keyring_service, obfuscate};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{fs, io};
use validator::Validate;

use super::persistence::write_atomic;
use super::state_lock::{self, StateLockGuard};

/// Serializes profile creation/deletion with every model mutation keyed by a profile name.
///
/// The lock is reentrant on one thread so lifecycle operations can call the folder, module,
/// package, and secret cleanup APIs while holding it.
pub(crate) fn lifecycle_guard(server_name: &str) -> Result<StateLockGuard> {
    if server_name.is_empty()
        || server_name.contains(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
    {
        anyhow::bail!("Invalid server name for profile lifecycle lock: {server_name}");
    }
    Ok(state_lock::acquire(&format!("lifecycle:{server_name}")))
}

/// Represents the configuration for a single profile within a server.
/// This struct is serialized to/from `profile.json` within the profile's directory.
#[derive(Serialize, Deserialize, Debug, Validate, Clone, PartialEq, Eq)]
pub struct ProfileConfig {
    pub caption: String,
    pub send_on_connect: String,
}

/// Represents a profile, including its configuration and associated directory path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The unique name of the profile, derived from its directory name.
    pub name: String,
    /// The path to the profile's directory within the server's profiles directory.
    pub path: PathBuf,
    /// The profile's configuration details loaded from `profile.json`.
    pub config: ProfileConfig,
}

/// Result of a profile mutation guarded by an exact configuration snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileCas<T> {
    Applied(T),
    StateChanged,
}

fn validate_profile_name(profile_name: &str) -> Result<()> {
    if profile_name.is_empty()
        || profile_name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
    {
        anyhow::bail!(
            "Invalid profile name: '{}'. Use only alphanumeric, underscore, or hyphen.",
            profile_name
        );
    }
    Ok(())
}

fn profile_dir(server_name: &str, profile_name: &str) -> Result<PathBuf> {
    validate_profile_name(profile_name)?;
    Ok(get_smudgy_home()?
        .join(server_name)
        .join("profiles")
        .join(profile_name))
}

/// Clears deterministic profile credentials before a whole server directory is deleted.
///
/// Profile directories do not need a readable `profile.json` for cleanup: a damaged profile can
/// still own a keyring entry.
pub(crate) fn prepare_server_deletion(server_name: &str) -> Result<()> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let profiles_dir = get_smudgy_home()?.join(server_name).join("profiles");
    let mut active_names = Vec::new();
    match fs::read_dir(&profiles_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .with_context(|| format!("read profile entry in {}", profiles_dir.display()))?;
                if !entry
                    .file_type()
                    .with_context(|| format!("inspect profile path {}", entry.path().display()))?
                    .is_dir()
                {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if validate_profile_name(&name).is_ok() {
                    active_names.push(name);
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read profiles in {}", profiles_dir.display()));
        }
    }
    for profile_name in active_names {
        clear_profile_password(server_name, &profile_name)
            .with_context(|| format!("clear the stored password for profile '{profile_name}'"))?;
    }
    Ok(())
}

/// Helper function to load and deserialize `ProfileConfig` from a file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, read, or if the contents
/// cannot be deserialized into a `ProfileConfig` or fail validation.
fn load_profile_config(path: &PathBuf) -> Result<ProfileConfig> {
    let file_content = fs::read_to_string(path).context(format!(
        "Failed to read profile config file: {}",
        path.display()
    ))?;
    let config: ProfileConfig = serde_json::from_str(&file_content).context(format!(
        "Failed to parse profile config file: {}",
        path.display()
    ))?;
    config.validate().context(format!(
        "Profile config validation failed: {}",
        path.display()
    ))?;
    Ok(config)
}

/// Lists all valid profiles found within a specific server's profile directory.
///
/// A profile is considered valid if it's a directory within the server's `profiles` subfolder
/// and contains a readable and valid `profile.json` file.
///
/// # Errors
///
/// Returns an error if the smudgy home or the server directory cannot be accessed.
/// If the server's `profiles` directory doesn't exist, an empty list is returned.
/// Errors reading individual profile directories or parsing `profile.json` files
/// are logged as warnings, and those profiles are skipped.
pub fn list_profiles(server_name: &str) -> Result<Vec<Profile>> {
    list_profiles_with_mode(server_name, false)
}

/// Lists profiles without accepting a partial inventory.
///
/// Unlike [`list_profiles`], this returns an error if any profile directory entry cannot be
/// inspected, named, or loaded. Use this before editing state that is keyed by the complete set of
/// profiles: silently omitting one profile could otherwise discard its stored settings.
///
/// # Errors
/// Returns an error if any profile cannot be read.
pub fn list_profiles_strict(server_name: &str) -> Result<Vec<Profile>> {
    list_profiles_with_mode(server_name, true)
}

fn list_profiles_with_mode(server_name: &str, strict: bool) -> Result<Vec<Profile>> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let profiles_dir = get_smudgy_home()?.join(server_name).join("profiles");

    let mut profiles = Vec::new();

    match fs::read_dir(&profiles_dir) {
        Ok(entries) => {
            for entry_result in entries {
                let entry = match entry_result {
                    Ok(entry) => entry,
                    Err(error) if strict => {
                        return Err(error).with_context(|| {
                            format!("read profile directory entry for server '{server_name}'")
                        });
                    }
                    Err(error) => {
                        eprintln!(
                            "Warning: Failed to read profile directory entry in server '{server_name}': {error}"
                        );
                        continue;
                    }
                };
                let path = entry.path();
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    if strict {
                        anyhow::bail!(
                            "profile directory name is not valid UTF-8: {}",
                            path.display()
                        );
                    }
                    eprintln!(
                        "Warning: Skipping profile directory with non-UTF8 name in server '{server_name}': {}",
                        path.display()
                    );
                    continue;
                };
                if let Err(error) = validate_profile_name(name) {
                    if strict {
                        return Err(error).with_context(|| {
                            format!("validate profile directory name at {}", path.display())
                        });
                    }
                    eprintln!(
                        "Warning: Skipping invalid profile directory '{name}' in server '{server_name}': {error}"
                    );
                    continue;
                }
                let config_path = path.join("profile.json");
                match load_profile_config(&config_path) {
                    Ok(config) => profiles.push(Profile {
                        name: name.to_string(),
                        path: path.clone(),
                        config,
                    }),
                    Err(error) if strict => {
                        return Err(error).with_context(|| {
                            format!("load profile '{name}' for server '{server_name}'")
                        });
                    }
                    Err(error) => {
                        eprintln!(
                            "Warning: Skipping profile '{name}' in server '{server_name}'. Failed to load config: {error}"
                        );
                    }
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && !strict => {}
        Err(error) => {
            return Err(error).context(format!(
                "Failed to read profiles directory for server '{}' at {}",
                server_name,
                profiles_dir.to_string_lossy()
            ));
        }
    }

    Ok(profiles)
}

/// Creates a new profile directory and configuration file within a server.
///
/// # Errors
///
/// Returns an error if:
/// * The profile name is invalid.
/// * The provided `config` is invalid.
/// * The smudgy home or server directory cannot be accessed.
/// * The server's `profiles` directory doesn't exist.
/// * A profile with the same name already exists within that server.
/// * There are filesystem errors during directory or file creation.
pub fn create_profile(
    server_name: &str,
    profile_name: &str,
    config: ProfileConfig,
) -> Result<Profile> {
    validate_profile_name(profile_name)?;
    let _lifecycle_guard = lifecycle_guard(server_name)?;

    let _package_guard = super::shared_packages::guard(server_name);

    config.validate().context(format!(
        "Invalid configuration for profile '{profile_name}' in server '{server_name}'"
    ))?;

    let server_path = get_smudgy_home()?.join(server_name);
    let profiles_dir = server_path.join("profiles");
    let profile_path = profiles_dir.join(profile_name);

    if !server_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Server directory not found or not a directory: {:?}",
            server_path
        ));
    }
    if !profiles_dir.is_dir() {
        return Err(anyhow::anyhow!(
            "Profiles directory not found within server '{}': {:?}",
            server_name,
            profiles_dir
        ));
    }
    if profile_path.exists() {
        return Err(anyhow::anyhow!(
            "Profile '{}' already exists in server '{}' at {:?}",
            profile_name,
            server_name,
            profile_path
        ));
    }

    // A deleted profile can leave a selected-profile marker or a name-keyed credential behind if
    // its cleanup previously failed. Never let a new profile with the same name inherit them. The
    // profile does not exist yet, so partial cleanup is harmless and creation can be retried.
    super::shared_packages::clear_profile_param_secrets(server_name, profile_name)
        .context("Failed to clear old package secrets for the new profile name")?;
    // An unreachable keyring (no secret service on a minimal Linux desktop, a locked store) must
    // not make profiles impossible to create: nothing can read a stale password from it either.
    // The on-disk fallback is removed regardless, and profile deletion still fails closed.
    if let Err(error) = clear_profile_password(server_name, profile_name) {
        log::warn!(
            "Could not clear an old password for the new profile name {server_name}/{profile_name}: {error:#}"
        );
    }
    super::packages::remove_profile_activation(server_name, profile_name)
        .context("Failed to clear old folder activation for the new profile name")?;
    super::modules::remove_profile_activation(server_name, profile_name)
        .context("Failed to clear old module activation for the new profile name")?;
    super::shared_packages::remove_profile_activation(server_name, profile_name)
        .context("Failed to clear old package activation for the new profile name")?;

    fs::create_dir(&profile_path).context(format!(
        "Failed to create directory for profile '{profile_name}' in server '{server_name}' at {}",
        profile_path.display()
    ))?;

    let config_path = profile_path.join("profile.json");
    let config_json = serde_json::to_string_pretty(&config).context(format!(
        "Failed to serialize config for profile '{profile_name}' in server '{server_name}'"
    ))?;
    write_atomic(&config_path, config_json.as_bytes()).context(format!(
        "Failed to write profile.json for profile '{profile_name}' in server '{server_name}' at {}",
        config_path.display()
    ))?;

    crate::storage::create_profile(&server_path, profile_name)?;

    Ok(Profile {
        name: profile_name.to_string(),
        path: profile_path,
        config,
    })
}

/// Creates a profile only while the exact server snapshot selected by the caller is still current.
///
/// # Errors
/// Returns an error for invalid input or profile-state I/O failures.
pub fn create_profile_if_server_unchanged(
    expected_server: &super::server::Server,
    profile_name: &str,
    config: ProfileConfig,
) -> Result<ProfileCas<Profile>> {
    validate_profile_name(profile_name)?;
    let _lifecycle_guard = lifecycle_guard(&expected_server.name)?;
    if !super::server::server_unchanged_locked(expected_server)? {
        return Ok(ProfileCas::StateChanged);
    }
    create_profile(&expected_server.name, profile_name, config).map(ProfileCas::Applied)
}

/// Loads a specific profile by its name within a given server.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home, server, or profiles directory cannot be accessed.
/// * No directory with the given `profile_name` exists within the server's profiles directory.
/// * The found path is not a directory.
/// * The `profile.json` file is missing, cannot be read, or is invalid.
pub fn load_profile(server_name: &str, profile_name: &str) -> Result<Profile> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let profile_path = profile_dir(server_name, profile_name)?;

    if !profile_path.exists() {
        return Err(anyhow::anyhow!(
            "Profile '{}' not found in server '{}'",
            profile_name,
            server_name
        ))
        .with_context(|| format!("Looked in directory: {}", profile_path.display()));
    }
    if !profile_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Path for profile '{}' in server '{}' exists but is not a directory: {:?}",
            profile_name,
            server_name,
            profile_path
        ));
    }

    let config_path = profile_path.join("profile.json");
    let config = load_profile_config(&config_path).context(format!(
        "Failed to load config for profile '{profile_name}' in server '{server_name}'"
    ))?;

    Ok(Profile {
        name: profile_name.to_string(),
        path: profile_path,
        config,
    })
}

fn load_profile_for_cas_locked(server_name: &str, profile_name: &str) -> Result<Option<Profile>> {
    let path = profile_dir(server_name, profile_name)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            load_profile(server_name, profile_name).map(Some)
        }
        Ok(_) => anyhow::bail!("profile path is not a real directory: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("inspect profile path {}", path.display()))
        }
    }
}

/// Updates the configuration of an existing profile within a server.
///
/// # Errors
///
/// Returns an error if:
/// * The profile with the given `name` cannot be found within the server.
/// * The path found is not a directory.
/// * The `new_config` fails validation.
/// * The `profile.json` file cannot be written.
pub fn update_profile(
    server_name: &str,
    profile_name: &str,
    new_config: ProfileConfig,
) -> Result<Profile> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    new_config.validate().context(format!(
        "Invalid new configuration provided for profile '{profile_name}' in server '{server_name}'"
    ))?;

    let profile_path = profile_dir(server_name, profile_name)?;
    if !profile_path.exists() {
        return Err(anyhow::anyhow!(
            "Profile '{}' not found in server '{}' for update",
            profile_name,
            server_name
        ))
        .with_context(|| format!("Looked for directory: {}", profile_path.display()));
    }
    if !profile_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Path for profile '{}' in server '{}' exists but is not a directory: {:?}",
            profile_name,
            server_name,
            profile_path
        ));
    }

    let config_path = profile_path.join("profile.json");
    let config_json = serde_json::to_string_pretty(&new_config).context(format!(
        "Failed to serialize updated config for profile '{profile_name}' in server '{server_name}'"
    ))?;
    write_atomic(&config_path, config_json.as_bytes()).context(format!(
        "Failed to write updated profile.json for profile '{profile_name}' in server '{server_name}' at {}",
        config_path.display()
    ))?;

    Ok(Profile {
        name: profile_name.to_string(),
        path: profile_path,
        config: new_config,
    })
}

/// Updates a profile only while its complete loaded snapshot is still current.
///
/// # Errors
/// Returns an error for invalid input or profile-state I/O failures.
pub fn update_profile_if_unchanged(
    server_name: &str,
    expected: &Profile,
    new_config: ProfileConfig,
) -> Result<ProfileCas<Profile>> {
    validate_profile_name(&expected.name)?;
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let Some(current) = load_profile_for_cas_locked(server_name, &expected.name)? else {
        return Ok(ProfileCas::StateChanged);
    };
    if current != *expected {
        return Ok(ProfileCas::StateChanged);
    }
    update_profile(server_name, &expected.name, new_config).map(ProfileCas::Applied)
}

/// Deletes a profile from a server and removes all of its associated data.
///
/// Name-keyed state outside the profile directory (its password, package secrets, and folder,
/// module, and package activation memberships) is cleaned up first, then the directory is
/// removed. If the profile does not exist, the function succeeds silently.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home or server directory cannot be accessed.
/// * A file exists with the profile name (instead of a directory).
/// * The directory or its contents cannot be removed due to permissions or other I/O issues.
pub fn delete_profile(server_name: &str, profile_name: &str) -> Result<()> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let _package_guard = super::shared_packages::guard(server_name);
    let profile_path = profile_dir(server_name, profile_name)?;

    match fs::symlink_metadata(&profile_path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => anyhow::bail!(
            "Cannot delete profile '{}' in server '{}': path exists but is not a real directory: {:?}",
            profile_name,
            server_name,
            profile_path
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect profile path {}", profile_path.display()));
        }
    }

    clear_profile_password(server_name, profile_name)
        .context("Failed to safely clear the deleted profile's stored password")?;
    super::shared_packages::clear_profile_param_secrets(server_name, profile_name)
        .context("Failed to safely clear package secrets for deleted profile")?;
    super::packages::remove_profile_activation(server_name, profile_name)
        .context("Failed to remove deleted profile from folder activation")?;
    super::modules::remove_profile_activation(server_name, profile_name)
        .context("Failed to remove deleted profile from module activation")?;
    super::shared_packages::remove_profile_activation(server_name, profile_name)
        .context("Failed to remove deleted profile from package activation")?;

    crate::storage::remove_profile(&get_smudgy_home()?.join(server_name), profile_name)?;

    fs::remove_dir_all(&profile_path).context(format!(
        "Failed to delete directory for profile '{profile_name}' in server '{server_name}' at {}",
        profile_path.display()
    ))
}

/// Deletes only the exact loaded profile snapshot in `expected`.
///
/// # Errors
/// Returns an error for invalid input or deletion/cleanup failures. A missing or changed profile
/// is reported as [`ProfileCas::StateChanged`] without cleanup.
pub fn delete_profile_if_unchanged(
    server_name: &str,
    expected: &Profile,
) -> Result<ProfileCas<()>> {
    validate_profile_name(&expected.name)?;
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let Some(current) = load_profile_for_cas_locked(server_name, &expected.name)? else {
        return Ok(ProfileCas::StateChanged);
    };
    if current != *expected {
        return Ok(ProfileCas::StateChanged);
    }
    delete_profile(server_name, &expected.name)?;
    Ok(ProfileCas::Applied(()))
}

// ===== Auto-login password ($PASSWORD) =====
//
// Auto-login text may embed the literal token `$PASSWORD`. The token is what lives
// in `profile.json`; the real password is kept in the OS keyring (Windows
// Credential Manager / macOS Keychain / Secret Service) keyed by (server, profile),
// with an obfuscated-file fallback, mirroring `models::auth`. The password is
// substituted into the auto-login text only when it is sent to the MUD
// (see `substitute_password`, called from `session::config::load_connect_action`),
// so it never touches `profile.json` and is never logged.

/// The token users embed in a profile's auto-login text to stand in for a stored
/// password.
pub const PASSWORD_TOKEN: &str = "$PASSWORD";

/// Whether `text` contains the [`PASSWORD_TOKEN`].
#[must_use]
pub fn contains_password_token(text: &str) -> bool {
    text.contains(PASSWORD_TOKEN)
}

/// keyring slot for a profile's auto-login password — unique per (server, profile).
/// Profile names are validated to alphanumeric/`_`/`-`, so `/` is a safe delimiter.
fn password_keyring_slot(server_name: &str, profile_name: &str) -> String {
    format!("profile-password:{server_name}/{profile_name}")
}

fn password_keyring_entry(
    server_name: &str,
    profile_name: &str,
) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(
        keyring_service(),
        &password_keyring_slot(server_name, profile_name),
    )
}

/// Obfuscated fallback file for a profile's password, used only when no OS keyring
/// is available. Lives inside the profile directory so it travels with — and is
/// deleted alongside — the profile.
fn password_fallback_path(server_name: &str, profile_name: &str) -> Result<PathBuf> {
    Ok(profile_dir(server_name, profile_name)?.join(".password"))
}

/// Stores the auto-login password for (server, profile) in the OS keyring (with an
/// obfuscated-file fallback when no keyring is available). Never written to
/// `profile.json`, never logged.
///
/// # Errors
///
/// Returns an error if both the keyring write and the fallback-file write fail.
pub fn set_profile_password(server_name: &str, profile_name: &str, password: &str) -> Result<()> {
    validate_profile_name(profile_name)?;
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    load_profile(server_name, profile_name)
        .context("Cannot save a password for a profile that does not exist")?;
    match password_keyring_entry(server_name, profile_name).and_then(|e| e.set_password(password)) {
        Ok(()) => {
            // Don't leave a stale obfuscated copy behind once the keyring holds it.
            if let Ok(path) = password_fallback_path(server_name, profile_name) {
                let _ = fs::remove_file(path);
            }
            Ok(())
        }
        Err(e) => {
            log::warn!(
                "OS keyring unavailable for profile password, falling back to obfuscated file: {e}"
            );
            let path = password_fallback_path(server_name, profile_name)?;
            let encoded = hex_encode(&obfuscate(password.as_bytes()));
            write_atomic(&path, encoded.as_bytes()).context(format!(
                "Failed to write password fallback file {}",
                path.display()
            ))
        }
    }
}

/// Stores a password only for the exact loaded profile snapshot in `expected`.
///
/// # Errors
/// Returns an error if the password cannot be stored.
pub fn set_profile_password_if_unchanged(
    server_name: &str,
    expected: &Profile,
    password: &str,
) -> Result<ProfileCas<()>> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let Some(current) = load_profile_for_cas_locked(server_name, &expected.name)? else {
        return Ok(ProfileCas::StateChanged);
    };
    if current != *expected {
        return Ok(ProfileCas::StateChanged);
    }
    set_profile_password(server_name, &expected.name, password)?;
    Ok(ProfileCas::Applied(()))
}

/// Reads the stored auto-login password for (server, profile), if any. Tries the OS
/// keyring first, then the obfuscated fallback file. Never logs password material.
#[must_use]
pub fn get_profile_password(server_name: &str, profile_name: &str) -> Option<String> {
    validate_profile_name(profile_name).ok()?;
    match password_keyring_entry(server_name, profile_name).and_then(|e| e.get_password()) {
        Ok(password) => Some(password),
        Err(e) => {
            if !matches!(e, keyring::Error::NoEntry) {
                log::warn!("Failed to read profile password from the OS keyring: {e}");
            }
            let path = password_fallback_path(server_name, profile_name).ok()?;
            let content = fs::read_to_string(&path).ok()?;
            let bytes = hex_decode(content.trim())?;
            String::from_utf8(obfuscate(&bytes)).ok()
        }
    }
}

/// Whether an auto-login password is stored for (server, profile).
#[must_use]
pub fn has_profile_password(server_name: &str, profile_name: &str) -> bool {
    get_profile_password(server_name, profile_name).is_some()
}

/// Removes the stored auto-login password for (server, profile) from both the OS
/// keyring and the fallback file. Missing entries are fine.
///
/// # Errors
///
/// Returns an error if an existing keyring entry could not be removed.
pub fn clear_profile_password(server_name: &str, profile_name: &str) -> Result<()> {
    validate_profile_name(profile_name)?;
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let keyring_result = match password_keyring_entry(server_name, profile_name)
        .and_then(|e| e.delete_credential())
    {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to delete profile password from the OS keyring: {e}"
        )),
    };
    if let Ok(path) = password_fallback_path(server_name, profile_name) {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!(
                    "Failed to delete password fallback file {}: {e}",
                    path.display()
                );
            }
        }
    }
    keyring_result
}

/// Clears a password only for the exact loaded profile snapshot in `expected`.
///
/// # Errors
/// Returns an error if an existing keyring entry could not be removed.
pub fn clear_profile_password_if_unchanged(
    server_name: &str,
    expected: &Profile,
) -> Result<ProfileCas<()>> {
    let _lifecycle_guard = lifecycle_guard(server_name)?;
    let Some(current) = load_profile_for_cas_locked(server_name, &expected.name)? else {
        return Ok(ProfileCas::StateChanged);
    };
    if current != *expected {
        return Ok(ProfileCas::StateChanged);
    }
    clear_profile_password(server_name, &expected.name)?;
    Ok(ProfileCas::Applied(()))
}

/// Substitutes [`PASSWORD_TOKEN`] in auto-login `text` with the stored password for
/// (server, profile). Text without the token is returned unchanged; if the token is
/// present but no password is stored, it is replaced with an empty string so the
/// literal token is never sent to the MUD.
#[must_use]
pub fn substitute_password(server_name: &str, profile_name: &str, text: &str) -> String {
    substitute_password_with_redactions(server_name, profile_name, text).0
}

/// Like [`substitute_password`], but also returns the secret substrings that were
/// substituted in, so the caller can redact them from the client's view and the
/// session log when the auto-login text is echoed. The returned vec is empty when
/// the token was absent or no (non-empty) password was stored.
#[must_use]
pub fn substitute_password_with_redactions(
    server_name: &str,
    profile_name: &str,
    text: &str,
) -> (String, Vec<String>) {
    if !text.contains(PASSWORD_TOKEN) {
        return (text.to_string(), Vec::new());
    }
    let password = get_profile_password(server_name, profile_name).unwrap_or_else(|| {
        log::warn!(
            "Auto-login for '{server_name}/{profile_name}' uses {PASSWORD_TOKEN} but no password is stored; sending an empty value"
        );
        String::new()
    });
    let substituted = text.replace(PASSWORD_TOKEN, &password);
    let redactions = if password.is_empty() {
        Vec::new()
    } else {
        vec![password]
    };
    (substituted, redactions)
}

#[cfg(test)]
mod password_tests {
    use super::{
        PASSWORD_TOKEN, contains_password_token, password_keyring_slot, profile_dir,
        substitute_password,
    };

    #[test]
    fn profile_names_cannot_escape_the_profiles_directory() {
        assert!(profile_dir("Srv", "../other").is_err());
        assert!(profile_dir("Srv", "").is_err());
        assert!(profile_dir("Srv", "Gandalf").is_ok());
    }

    #[test]
    fn detects_token() {
        assert!(contains_password_token("connect Gandalf $PASSWORD"));
        assert!(!contains_password_token("connect Gandalf hunter2"));
        assert_eq!(PASSWORD_TOKEN, "$PASSWORD");
    }

    #[test]
    fn substitute_is_noop_without_token() {
        // No token => returned unchanged and no keyring access happens.
        let text = "connect Gandalf hunter2";
        assert_eq!(substitute_password("Srv", "Gandalf", text), text);
    }

    #[test]
    fn keyring_slot_is_unique_per_server_and_profile() {
        assert_eq!(
            password_keyring_slot("Arctic", "Gandalf"),
            "profile-password:Arctic/Gandalf"
        );
        assert_ne!(
            password_keyring_slot("A", "Gandalf"),
            password_keyring_slot("B", "Gandalf")
        );
    }
}
