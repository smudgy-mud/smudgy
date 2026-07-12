// Models related to profile configurations

use crate::get_smudgy_home;
use crate::models::auth::{hex_decode, hex_encode, keyring_service, obfuscate};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{fs, io};
use validator::Validate;

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

/// Helper function to load and deserialize `ProfileConfig` from a file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, read, or if the contents
/// cannot be deserialized into a `ProfileConfig` or fail validation.
fn load_profile_config(path: &PathBuf) -> Result<ProfileConfig> {
    let file_content = fs::read_to_string(path)
        .context(format!("Failed to read profile config file: {}", path.display()))?;
    let config: ProfileConfig = serde_json::from_str(&file_content)
        .context(format!("Failed to parse profile config file: {}", path.display()))?;
    config
        .validate()
        .context(format!("Profile config validation failed: {}", path.display()))?;
    Ok(config)
}

/// Lists all valid profiles found within a specific server's profile directory.
///
/// A profile is considered valid if it's a directory within the server's `profiles` subfolder
/// and contains a readable and valid `profile.json` file.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose profiles should be listed.
///
/// # Errors
///
/// Returns an error if the smudgy home or the server directory cannot be accessed.
/// If the server's `profiles` directory doesn't exist, an empty list is returned.
/// Errors reading individual profile directories or parsing `profile.json` files
/// are logged as warnings, and those profiles are skipped.
pub fn list_profiles(server_name: &str) -> Result<Vec<Profile>> {
    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(server_name);
    let profiles_dir = server_path.join("profiles");

    let mut profiles = Vec::new();

    match fs::read_dir(&profiles_dir) {
        Ok(entries) => {
            for entry_result in entries {
                match entry_result {
                    Ok(entry) => {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                let config_path = path.join("profile.json");
                                match load_profile_config(&config_path) {
                                    Ok(config) => {
                                        profiles.push(Profile {
                                            name: name.to_string(),
                                            path: path.clone(),
                                            config,
                                        });
                                    }
                                    Err(e) => {
                                        // Log warning: Failed to load profile config
                                        eprintln!(
                                            "Warning: Skipping profile '{name}' in server '{server_name}'. Failed to load config: {e}"
                                        );
                                    }
                                }
                            } else {
                                // Log warning: Invalid directory name (not UTF-8)
                                eprintln!(
                                    "Warning: Skipping profile directory with non-UTF8 name in server '{server_name}': {}",
                                    path.display()
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // Log warning: Failed to read profile directory entry
                        eprintln!(
                            "Warning: Failed to read profile directory entry in server '{server_name}': {e}"
                        );
                    }
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // If the profiles dir doesn't exist for this server, return empty list.
            // This is not an error condition.
        }
        Err(e) => {
            // Other errors reading the profiles dir are propagated.
            return Err(e).context(format!(
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
/// # Arguments
///
/// * `server_name` - The name of the server to add the profile to.
/// * `profile_name` - The name for the new profile. Must be a valid directory name.
/// * `config` - The initial `ProfileConfig` for the profile.
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
    // Validate profile name
    if profile_name.is_empty()
        || profile_name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
    {
        return Err(anyhow::anyhow!(
            "Invalid profile name: '{}'. Use only alphanumeric, underscore, or hyphen.",
            profile_name
        ));
    }

    // Validate the provided configuration
    config.validate().context(format!(
        "Invalid configuration for profile '{profile_name}' in server '{server_name}'"
    ))?;

    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(server_name);
    let profiles_dir = server_path.join("profiles");
    let profile_path = profiles_dir.join(profile_name);

    // Pre-flight check: Ensure server and profiles directories exist
    if !server_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Server directory not found or not a directory: {:?}",
            server_path
        ));
    }
    if !profiles_dir.is_dir() {
        // This shouldn't happen if ensure_server_subdirs was called, but check defensively.
        return Err(anyhow::anyhow!(
            "Profiles directory not found within server '{}': {:?}",
            server_name,
            profiles_dir
        ));
    }

    // Check if profile directory already exists
    if profile_path.exists() {
        return Err(anyhow::anyhow!(
            "Profile '{}' already exists in server '{}' at {:?}",
            profile_name,
            server_name,
            profile_path
        ));
    }

    // Create the profile directory
    fs::create_dir(&profile_path).context(format!(
        "Failed to create directory for profile '{profile_name}' in server '{server_name}' at {}",
        profile_path.display()
    ))?;

    // Write the profile.json file
    let config_path = profile_path.join("profile.json");
    let config_json = serde_json::to_string_pretty(&config).context(format!(
        "Failed to serialize config for profile '{profile_name}' in server '{server_name}'"
    ))?;

    fs::write(&config_path, config_json).context(format!(
        "Failed to write profile.json for profile '{profile_name}' in server '{server_name}' at {}",
        config_path.display()
    ))?;

    Ok(Profile {
        name: profile_name.to_string(),
        path: profile_path,
        config,
    })
}

/// Loads a specific profile by its name within a given server.
///
/// # Arguments
///
/// * `server_name` - The name of the server containing the profile.
/// * `profile_name` - The name of the profile to load.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home, server, or profiles directory cannot be accessed.
/// * No directory with the given `profile_name` exists within the server's profiles directory.
/// * The found path is not a directory.
/// * The `profile.json` file is missing, cannot be read, or is invalid.
pub fn load_profile(server_name: &str, profile_name: &str) -> Result<Profile> {
    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(server_name);
    let profiles_dir = server_path.join("profiles");
    let profile_path = profiles_dir.join(profile_name);

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

    // Load the configuration
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

/// Updates the configuration of an existing profile within a server.
///
/// Finds the profile by name, validates the new configuration, and overwrites
/// the existing `profile.json` file.
///
/// # Arguments
///
/// * `server_name` - The name of the server containing the profile.
/// * `profile_name` - The name of the profile to update.
/// * `new_config` - The `ProfileConfig` containing the updated settings.
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
    // Validate the new configuration first
    new_config.validate().context(format!(
        "Invalid new configuration provided for profile '{profile_name}' in server '{server_name}'"
    ))?;

    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(server_name);
    let profiles_dir = server_path.join("profiles");
    let profile_path = profiles_dir.join(profile_name);

    // Ensure the profile directory exists and is a directory
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

    // Construct path to profile.json
    let config_path = profile_path.join("profile.json");

    // Serialize the new config
    let config_json = serde_json::to_string_pretty(&new_config).context(format!(
        "Failed to serialize updated config for profile '{profile_name}' in server '{server_name}'"
    ))?;

    // Write the new config, overwriting the old one
    fs::write(&config_path, config_json).context(format!(
        "Failed to write updated profile.json for profile '{profile_name}' in server '{server_name}' at {}",
        config_path.display()
    ))?;

    // Return the profile representation with the new config
    Ok(Profile {
        name: profile_name.to_string(),
        path: profile_path,
        config: new_config, // Use the validated new_config
    })
}

/// Deletes a profile and all its associated data from a server.
///
/// Finds the profile directory by name within the specified server's `profiles`
/// directory and removes it recursively.
/// If the profile directory does not exist, the function succeeds silently.
///
/// # Arguments
///
/// * `server_name` - The name of the server containing the profile.
/// * `profile_name` - The name of the profile to delete.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home or server directory cannot be accessed.
/// * A file exists with the profile name (instead of a directory).
/// * The directory or its contents cannot be removed due to permissions or other I/O issues.
pub fn delete_profile(server_name: &str, profile_name: &str) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(server_name);
    let profiles_dir = server_path.join("profiles");
    let profile_path = profiles_dir.join(profile_name);

    if profile_path.exists() {
        // Check if it's actually a directory before attempting recursive delete
        if !profile_path.is_dir() {
            return Err(anyhow::anyhow!(
                "Cannot delete profile '{}' in server '{}': Path exists but is not a directory: {:?}",
                profile_name,
                server_name,
                profile_path
            ));
        }

        // Recursively remove the directory
        fs::remove_dir_all(&profile_path).context(format!(
            "Failed to delete directory for profile '{profile_name}' in server '{server_name}' at {}",
            profile_path.display()
        ))?;
    } else {
        // Optionally log that the profile didn't exist? For now, silent success.
        println!(
            "Info: Profile '{profile_name}' not found in server '{server_name}' for deletion."
        );
    }

    // Remove any stored auto-login password. The obfuscated fallback file lived
    // inside the profile directory (already gone above), but the OS keyring entry
    // is outside it and must be cleared explicitly. Best effort.
    if let Err(e) = clear_profile_password(server_name, profile_name) {
        log::warn!(
            "Failed to clear stored password for deleted profile '{server_name}/{profile_name}': {e}"
        );
    }

    Ok(())
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
    Ok(get_smudgy_home()?
        .join(server_name)
        .join("profiles")
        .join(profile_name)
        .join(".password"))
}

/// Stores the auto-login password for (server, profile) in the OS keyring (with an
/// obfuscated-file fallback when no keyring is available). Never written to
/// `profile.json`, never logged.
///
/// # Errors
///
/// Returns an error if both the keyring write and the fallback-file write fail.
pub fn set_profile_password(server_name: &str, profile_name: &str, password: &str) -> Result<()> {
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
            fs::write(&path, encoded).context(format!(
                "Failed to write password fallback file {}",
                path.display()
            ))
        }
    }
}

/// Reads the stored auto-login password for (server, profile), if any. Tries the OS
/// keyring first, then the obfuscated fallback file. Never logs password material.
#[must_use]
pub fn get_profile_password(server_name: &str, profile_name: &str) -> Option<String> {
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
                log::warn!("Failed to delete password fallback file {}: {e}", path.display());
            }
        }
    }
    keyring_result
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
    use super::{PASSWORD_TOKEN, contains_password_token, password_keyring_slot, substitute_password};

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
