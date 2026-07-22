use crate::get_smudgy_home;
use crate::models::packages::PackageTree;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io, path::PathBuf};

use super::ScriptLang;

/// Helper function for serde to default boolean fields to true.
fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

/// Represents the definition of a single alias.
///
/// This structure is used as the value in the map representation
/// of the `aliases.json` file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasDefinition {
    /// The regex pattern to match against user input.
    pub pattern: String,
    /// Stores inline script content. If None during load, implies script is file-based.
    /// If None during save, this field is omitted from the JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Optional package path (e.g., "combat/defense"). `None` indicates root package.
    #[serde(default)]
    pub package: Option<String>,
    /// Whether this specific alias is enabled. Defaults to true.
    /// Note: The alias is only *effectively* enabled if its `enabled` field is true
    /// AND all its parent packages are also enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Relative evaluation priority. Higher values run first; defaults to zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,
    /// Whether matching continues to later aliases after this one runs. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub fallthrough: bool,
    /// The language of the script. Defaults to Plaintext.
    #[serde(default)]
    pub language: ScriptLang,
}

impl AliasDefinition {
    /// Checks if the alias is effectively enabled.
    ///
    /// An alias is effectively enabled if its own `enabled` flag is true AND
    /// its package (if any) is effectively enabled according to the provided
    /// `PackageTree`.
    ///
    /// # Arguments
    ///
    /// * `package_tree` - The loaded `PackageTree` for the server.
    ///
    /// # Returns
    ///
    /// `true` if the alias should be considered active, `false` otherwise.
    #[must_use]
    pub fn is_effectively_enabled(&self, package_tree: &PackageTree) -> bool {
        if !self.enabled {
            return false; // Explicitly disabled
        }

        match &self.package {
            None => true, // Enabled and in root package
            Some(path_str) => {
                // Enabled, check if package path is also enabled
                super::packages::is_package_effectively_enabled(path_str, package_tree)
            }
        }
    }

    /// Attempts to retrieve the script content and its language.
    ///
    /// Checks for inline script first. If none, looks for corresponding
    /// .ts and then .js files in the server's aliases directory.
    ///
    /// # Arguments
    /// * `alias_name` - The name of the alias (needed to find the file).
    /// * `server_name` - The name of the server (needed for the base path).
    ///
    /// # Returns
    /// `Ok(Some((content, language)))` if a script is found.
    /// `Ok(None)` if no inline script and no corresponding file exists.
    /// `Err(...)` if there's an error accessing files or directories.
    ///
    /// # Errors
    /// Returns an error if the smudgy home directory cannot be determined or a
    /// matching `.ts`/`.js` script file exists but cannot be read.
    pub fn get_script_content(
        &self,
        alias_name: &str,
        server_name: &str,
    ) -> Result<Option<(String, ScriptLang)>> {
        // 1. Check for inline script
        if let Some(inline_content) = &self.script {
            // TODO: Determine language of inline scripts. Assume JS for now.
            // This might require adding a `lang` field to AliasDefinition
            // if inline TS is desired, or a convention (e.g., comments).
            return Ok(Some((inline_content.clone(), ScriptLang::JS)));
        }

        // 2. No inline script, look for files
        let base_path = get_smudgy_home()?.join(server_name).join("aliases"); // Base aliases directory

        // Check for .ts file
        let ts_path = base_path.join(format!("{alias_name}.ts"));
        if ts_path.exists() {
            match fs::read_to_string(&ts_path) {
                Ok(content) => return Ok(Some((content, ScriptLang::TS))),
                Err(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("Failed to read script file: {}", ts_path.display())));
                }
            }
        }

        // Check for .js file
        let js_path = base_path.join(format!("{alias_name}.js"));
        if js_path.exists() {
            match fs::read_to_string(&js_path) {
                Ok(content) => return Ok(Some((content, ScriptLang::JS))),
                Err(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("Failed to read script file: {}", js_path.display())));
                }
            }
        }

        // 3. No inline script and no file found
        Ok(None)
    }

    /// Gets the expected filesystem path for a file-based script.
    ///
    /// Does not check if the file actually exists.
    ///
    /// # Arguments
    /// * `alias_name` - The name of the alias.
    /// * `server_name` - The name of the server.
    /// * `lang` - The desired script language (`.js` or `.ts`).
    ///
    /// # Errors
    /// Returns an error if the smudgy home directory cannot be determined.
    pub fn get_expected_script_path(
        &self,
        alias_name: &str,
        server_name: &str,
        lang: ScriptLang,
    ) -> Result<PathBuf> {
        let ext = match lang {
            ScriptLang::JS => "js",
            ScriptLang::TS => "ts",
            ScriptLang::Plaintext => "txt",
        };
        Ok(get_smudgy_home()?
            .join(server_name)
            .join("aliases")
            .join(format!("{alias_name}.{ext}")))
    }
}

/// Loads all alias definitions from `aliases.json` for a given server.
///
/// If `aliases.json` does not exist, returns an empty map successfully.
///
/// # Errors
/// Returns an error if the smudgy home directory cannot be determined, or if
/// `aliases.json` exists but cannot be read or parsed.
pub fn load_aliases(server_name: &str) -> Result<HashMap<String, AliasDefinition>> {
    let smudgy_dir = get_smudgy_home()?;
    let aliases_path = smudgy_dir
        .join(server_name)
        .join("aliases")
        .join("aliases.json");

    match fs::read_to_string(&aliases_path) {
        Ok(content) => {
            let aliases: HashMap<String, AliasDefinition> = serde_json::from_str(&content)
                .context(format!(
                    "Failed to parse aliases.json for server '{server_name}'"
                ))?;
            Ok(aliases)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(e).context(format!(
            "Failed to read aliases.json for server '{server_name}'"
        )),
    }
}

/// Saves the alias definitions map to `aliases.json` for a given server.
///
/// # Errors
/// Returns an error if the smudgy home directory cannot be determined, the
/// server's `aliases` directory does not exist, or `aliases.json` cannot be
/// serialized or written.
pub fn save_aliases(server_name: &str, aliases: &HashMap<String, AliasDefinition>) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let aliases_dir = smudgy_dir.join(server_name).join("aliases");

    if !aliases_dir.is_dir() {
        return Err(anyhow::anyhow!(
            "Aliases directory not found for server '{}': {:?}",
            server_name,
            aliases_dir
        ));
    }

    let aliases_path = aliases_dir.join("aliases.json");

    let json_content = serde_json::to_string_pretty(aliases).context(format!(
        "Failed to serialize aliases for server '{server_name}'"
    ))?;

    fs::write(&aliases_path, json_content).context(format!(
        "Failed to write aliases.json for server '{server_name}' at {}",
        aliases_path.display()
    ))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::AliasDefinition;

    #[test]
    fn matching_options_are_backward_compatible_and_sparse() {
        let old: AliasDefinition = serde_json::from_str(r#"{"pattern":"^look$"}"#).unwrap();
        assert_eq!(old.priority, 0);
        assert!(old.fallthrough);

        let default_json = serde_json::to_string(&old).unwrap();
        assert!(!default_json.contains("priority"));
        assert!(!default_json.contains("fallthrough"));

        let configured: AliasDefinition =
            serde_json::from_str(r#"{"pattern":"^look$","priority":7,"fallthrough":false}"#)
                .unwrap();
        let configured_json = serde_json::to_string(&configured).unwrap();
        assert!(configured_json.contains("\"priority\":7"));
        assert!(configured_json.contains("\"fallthrough\":false"));
    }
}
