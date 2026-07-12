use crate::get_smudgy_home;
use crate::models::packages::PackageTree;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io, path::PathBuf};

use super::ScriptLang;

// Helper function for serde to default boolean fields to true.
fn default_true() -> bool {
    true
}

/// Represents the definition of a single hotkey.
///
/// This structure is used as the value in the map representation
/// of the `hotkeys.json` file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)] // Note: Eq might be tricky if we add f32/f64 later
pub struct HotkeyDefinition {
    /// The primary key for the hotkey (e.g., "A", "F1", "Space").
    /// Mapping to `iced::keyboard::key::Physical` happens in the UI layer.
    pub key: String,
    /// A list of modifier keys (e.g., "Shift", "Control", "Alt", "Super").
    /// Mapping to `iced::keyboard::Modifiers` happens in the UI layer.
    #[serde(default)]
    pub modifiers: Vec<String>,
    /// Stores inline script content. If None during load, implies script is file-based.
    /// If None during save, this field is omitted from the JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Optional package path (e.g., "general/movement"). `None` indicates root package.
    #[serde(default)]
    pub package: Option<String>,
    /// The language of the script. Defaults to Plaintext.
    #[serde(default)]
    pub language: ScriptLang,
    /// Whether this specific hotkey is enabled. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl HotkeyDefinition {
    /// Checks if the hotkey is effectively enabled.
    #[must_use]
    pub fn is_effectively_enabled(&self, package_tree: &PackageTree) -> bool {
        if !self.enabled {
            return false;
        }
        match &self.package {
            None => true,
            Some(path_str) => {
                super::packages::is_package_effectively_enabled(path_str, package_tree)
            }
        }
    }

    /// Attempts to retrieve the script content and its language.
    ///
    /// # Errors
    /// Returns an error if the smudgy home directory cannot be determined or a
    /// matching `.ts`/`.js` script file exists but cannot be read.
    pub fn get_script_content(
        &self,
        hotkey_name: &str,
        server_name: &str,
    ) -> Result<Option<(String, ScriptLang)>> {
        if let Some(inline_content) = &self.script {
            // TODO: Determine language of inline scripts. Assume JS for now.
            return Ok(Some((inline_content.clone(), ScriptLang::JS)));
        }

        let base_path = get_smudgy_home()?.join(server_name).join("hotkeys");

        let ts_path = base_path.join(format!("{hotkey_name}.ts"));
        if ts_path.exists() {
            match fs::read_to_string(&ts_path) {
                Ok(content) => return Ok(Some((content, ScriptLang::TS))),
                Err(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("Failed to read script file: {}", ts_path.display())));
                }
            }
        }

        let js_path = base_path.join(format!("{hotkey_name}.js"));
        if js_path.exists() {
            match fs::read_to_string(&js_path) {
                Ok(content) => return Ok(Some((content, ScriptLang::JS))),
                Err(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("Failed to read script file: {}", js_path.display())));
                }
            }
        }
        Ok(None)
    }

    /// Gets the expected filesystem path for a file-based script.
    ///
    /// # Errors
    /// Returns an error if the smudgy home directory cannot be determined.
    pub fn get_expected_script_path(
        &self,
        hotkey_name: &str,
        server_name: &str,
        lang: ScriptLang,
    ) -> Result<PathBuf> {
        let ext = match lang {
            ScriptLang::Plaintext => "txt",
            ScriptLang::JS => "js",
            ScriptLang::TS => "ts",
        };
        Ok(get_smudgy_home()?
            .join(server_name)
            .join("hotkeys") // Use hotkeys subdir
            .join(format!("{hotkey_name}.{ext}")))
    }
}

/// Loads all hotkey definitions from `hotkeys.json` for a given server.
///
/// If `hotkeys.json` does not exist within the server's `hotkeys` directory,
/// returns an empty `HashMap` successfully.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose hotkeys should be loaded.
///
/// # Errors
///
/// Returns an error if the server or hotkeys directory cannot be accessed, or if
/// `hotkeys.json` exists but cannot be read or parsed.
pub fn load_hotkeys(server_name: &str) -> Result<HashMap<String, HotkeyDefinition>> {
    let smudgy_dir = get_smudgy_home()?;
    let hotkeys_path = smudgy_dir
        .join(server_name)
        .join("hotkeys") // Use hotkeys subdir
        .join("hotkeys.json");

    match fs::read_to_string(&hotkeys_path) {
        Ok(content) => {
            let hotkeys: HashMap<String, HotkeyDefinition> = serde_json::from_str(&content)
                .context(format!(
                    "Failed to parse hotkeys.json for server '{server_name}'"
                ))?;
            Ok(hotkeys)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // File not found is okay, just return an empty map
            Ok(HashMap::new())
        }
        Err(e) => {
            // Other read errors are propagated
            Err(e).context(format!(
                "Failed to read hotkeys.json for server '{server_name}'"
            ))
        }
    }
}

/// Saves the hotkey definitions map to `hotkeys.json` for a given server.
///
/// This will overwrite the existing file if it exists. It assumes the
/// parent `hotkeys` directory already exists.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose hotkeys should be saved.
/// * `hotkeys` - The `HashMap<String, HotkeyDefinition>` data structure to save.
///
/// # Errors
///
/// Returns an error if the server or hotkeys directory cannot be accessed, or if
/// `hotkeys.json` cannot be written.
pub fn save_hotkeys(server_name: &str, hotkeys: &HashMap<String, HotkeyDefinition>) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let hotkeys_dir = smudgy_dir.join(server_name).join("hotkeys");

    // Basic check to ensure the hotkeys directory exists.
    if !hotkeys_dir.is_dir() {
        return Err(anyhow::anyhow!(
            "Hotkeys directory not found for server '{}': {:?}",
            server_name,
            hotkeys_dir
        ));
    }

    let hotkeys_path = hotkeys_dir.join("hotkeys.json");

    let json_content = serde_json::to_string_pretty(hotkeys).context(format!(
        "Failed to serialize hotkeys for server '{server_name}'"
    ))?;

    fs::write(&hotkeys_path, json_content).context(format!(
        "Failed to write hotkeys.json for server '{server_name}' at {}",
        hotkeys_path.display()
    ))?;

    Ok(())
}
