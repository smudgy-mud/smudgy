use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::HashMap, fs, io, path::PathBuf};

use super::{ScriptLang, persistence::write_atomic, state_exposure::StateExposure};

// Helper function for serde to default boolean fields to true.
fn default_true() -> bool {
    true
}

/// Represents the definition of a single trigger.
///
/// This structure is used as the value in the map representation
/// of the `triggers.json` file.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerDefinition {
    /// Regex patterns to match against incoming server text.
    pub patterns: Option<Vec<String>>,
    /// Raw/literal strings to match.
    pub raw_patterns: Option<Vec<String>>,
    /// Regex patterns that must *not* match for the trigger to fire.
    pub anti_patterns: Option<Vec<String>>,
    /// Stores inline script content. If None during load, implies script is file-based.
    /// If None during save, this field is omitted from the JSON.
    pub script: Option<String>,
    /// Optional package path (e.g., "combat/healing"). `None` indicates root package.
    pub package: Option<String>,
    /// The language of the script. Defaults to Plaintext.
    pub language: ScriptLang,
    /// Whether this specific trigger is enabled. Defaults to true.
    pub enabled: bool,
    /// Whether this trigger should fire on prompts. Defaults to false.
    pub prompt: bool,
    /// Relative evaluation priority. Higher values run first; defaults to zero.
    pub priority: i32,
    /// Whether matching continues to later triggers after this one runs. Defaults to true.
    pub fallthrough: bool,
    /// Authoring intent behind the three pattern vectors, as the editor's row
    /// list in author order. The runtime reads this sidecar only when it exactly
    /// reproduces the stored vectors. It then uses metadata such as color
    /// filters. The vectors are derived from it on every save
    /// ([`super::matchers::trigger_patterns`]). Absent means every row is a
    /// hand-written regex shown verbatim — the pre-sidecar behavior.
    pub matchers: Option<Vec<super::matchers::TriggerMatcherSource>>,
    /// The session-store roots this trigger reads (`$name.path` in Send text, `name.path`
    /// in JavaScript). Empty for the common case, which serializes nothing and pays nothing
    /// at fire time. Carried by hand through both serde halves below.
    pub state: Vec<StateExposure>,
    // TODO: Add other trigger-specific fields like sound file, highlighting, etc.
}

impl Default for TriggerDefinition {
    fn default() -> Self {
        Self {
            patterns: None,
            raw_patterns: None,
            anti_patterns: None,
            script: None,
            package: None,
            language: ScriptLang::default(),
            enabled: true,
            prompt: false,
            priority: 0,
            fallthrough: true,
            matchers: None,
            state: Vec::new(),
        }
    }
}

// Custom Serialize implementation
impl Serialize for TriggerDefinition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(None)?;

        // Handle patterns field
        match &self.patterns {
            Some(vec) if !vec.is_empty() && vec.len() == 1 => {
                map.serialize_entry("pattern", &vec[0])?;
            }
            Some(vec) if !vec.is_empty() => {
                map.serialize_entry("patterns", vec)?;
            }
            _ => {}
        }

        // Handle raw_patterns field
        match &self.raw_patterns {
            Some(vec) if !vec.is_empty() && vec.len() == 1 => {
                map.serialize_entry("raw_pattern", &vec[0])?;
            }
            Some(vec) if !vec.is_empty() => {
                map.serialize_entry("raw_patterns", vec)?;
            }
            _ => {}
        }

        // Handle anti_patterns field
        match &self.anti_patterns {
            Some(vec) if !vec.is_empty() && vec.len() == 1 => {
                map.serialize_entry("anti_pattern", &vec[0])?;
            }
            Some(vec) if !vec.is_empty() => {
                map.serialize_entry("anti_patterns", vec)?;
            }
            _ => {}
        }

        // Other fields
        if self.script.is_some() {
            map.serialize_entry("script", &self.script)?;
        }
        if self.package.is_some() {
            map.serialize_entry("package", &self.package)?;
        }
        if self.language != ScriptLang::default() {
            map.serialize_entry("language", &self.language)?;
        }
        if !self.enabled {
            map.serialize_entry("enabled", &self.enabled)?;
        }
        if self.prompt {
            map.serialize_entry("prompt", &self.prompt)?;
        }
        if self.priority != 0 {
            map.serialize_entry("priority", &self.priority)?;
        }
        if !self.fallthrough {
            map.serialize_entry("fallthrough", &self.fallthrough)?;
        }
        match &self.matchers {
            Some(matchers) if !matchers.is_empty() => {
                map.serialize_entry("matchers", matchers)?;
            }
            _ => {}
        }
        if !self.state.is_empty() {
            map.serialize_entry("state", &self.state)?;
        }

        map.end()
    }
}

// Custom Deserialize implementation
impl<'de> Deserialize<'de> for TriggerDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TriggerHelper {
            #[serde(default)]
            pattern: Option<String>,
            #[serde(default)]
            patterns: Option<Vec<String>>,
            #[serde(default)]
            raw_pattern: Option<String>,
            #[serde(default)]
            raw_patterns: Option<Vec<String>>,
            #[serde(default)]
            anti_pattern: Option<String>,
            #[serde(default)]
            anti_patterns: Option<Vec<String>>,
            #[serde(default)]
            script: Option<String>,
            #[serde(default)]
            package: Option<String>,
            #[serde(default)]
            language: ScriptLang,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            prompt: bool,
            #[serde(default)]
            priority: i32,
            #[serde(default = "default_true")]
            fallthrough: bool,
            #[serde(default)]
            matchers: Option<Vec<crate::models::matchers::TriggerMatcherSource>>,
            #[serde(default)]
            state: Vec<StateExposure>,
        }

        let helper = TriggerHelper::deserialize(deserializer)?;

        // Combine pattern/patterns fields
        let patterns = match (helper.pattern, helper.patterns) {
            (None, None) => None,
            (Some(p), None) => Some(vec![p]),
            (None, Some(ps)) => {
                if ps.is_empty() {
                    None
                } else {
                    Some(ps)
                }
            }
            (Some(p), Some(mut ps)) => {
                ps.insert(0, p);
                Some(ps)
            }
        };

        // Combine raw_pattern/raw_patterns fields
        let raw_patterns = match (helper.raw_pattern, helper.raw_patterns) {
            (None, None) => None,
            (Some(p), None) => Some(vec![p]),
            (None, Some(ps)) => {
                if ps.is_empty() {
                    None
                } else {
                    Some(ps)
                }
            }
            (Some(p), Some(mut ps)) => {
                ps.insert(0, p);
                Some(ps)
            }
        };

        // Combine anti_pattern/anti_patterns fields
        let anti_patterns = match (helper.anti_pattern, helper.anti_patterns) {
            (None, None) => None,
            (Some(p), None) => Some(vec![p]),
            (None, Some(ps)) => {
                if ps.is_empty() {
                    None
                } else {
                    Some(ps)
                }
            }
            (Some(p), Some(mut ps)) => {
                ps.insert(0, p);
                Some(ps)
            }
        };

        Ok(TriggerDefinition {
            patterns,
            raw_patterns,
            anti_patterns,
            script: helper.script,
            package: helper.package,
            language: helper.language,
            enabled: helper.enabled,
            prompt: helper.prompt,
            priority: helper.priority,
            fallthrough: helper.fallthrough,
            // An empty list carries no authoring intent; normalize to absent.
            matchers: helper.matchers.filter(|matchers| !matchers.is_empty()),
            state: helper.state,
        })
    }
}

impl TriggerDefinition {
    /// Attempts to retrieve the script content and its language.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The smudgy home directory cannot be determined.
    /// - Reading the script file (e.g., `trigger_name.ts` or `trigger_name.js`) fails.
    pub fn get_script_content(
        &self,
        trigger_name: &str,
        server_name: &str,
    ) -> Result<Option<(String, ScriptLang)>> {
        if let Some(inline_content) = &self.script {
            // TODO: Determine language of inline scripts. Assume JS for now.
            return Ok(Some((inline_content.clone(), ScriptLang::JS)));
        }

        let base_path = get_smudgy_home()?.join(server_name).join("triggers");

        let ts_path = base_path.join(format!("{trigger_name}.ts"));
        if ts_path.exists() {
            match fs::read_to_string(&ts_path) {
                Ok(content) => return Ok(Some((content, ScriptLang::TS))),
                Err(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("Failed to read script file: {}", ts_path.display())));
                }
            }
        }

        let js_path = base_path.join(format!("{trigger_name}.js"));
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
    ///
    /// Returns an error if the smudgy home directory cannot be determined.
    pub fn get_expected_script_path(
        &self,
        trigger_name: &str,
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
            .join("triggers") // Use triggers subdir
            .join(format!("{trigger_name}.{ext}")))
    }

    /// Checks if the trigger definition has any pattern specified.
    #[must_use]
    pub fn has_patterns(&self) -> bool {
        self.patterns.is_some() || self.raw_patterns.is_some() || self.anti_patterns.is_some()
    }
}

/// Loads all trigger definitions from `triggers.json` for a given server.
///
/// If `triggers.json` does not exist within the server's `triggers` directory,
/// returns an empty `HashMap` successfully.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose triggers should be loaded.
///
/// # Errors
///
/// Returns an error if the server or triggers directory cannot be accessed, or if
/// `triggers.json` exists but cannot be read or parsed.
pub fn load_triggers(server_name: &str) -> Result<HashMap<String, TriggerDefinition>> {
    let smudgy_dir = get_smudgy_home()?;
    let triggers_path = smudgy_dir
        .join(server_name)
        .join("triggers") // Use triggers subdir
        .join("triggers.json");

    match fs::read_to_string(&triggers_path) {
        Ok(content) => {
            let triggers: HashMap<String, TriggerDefinition> = serde_json::from_str(&content)
                .context(format!(
                    "Failed to parse triggers.json for server '{server_name}'"
                ))?;
            Ok(triggers)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // File not found is okay, just return an empty map
            Ok(HashMap::new())
        }
        Err(e) => {
            // Other read errors are propagated
            Err(e).context(format!(
                "Failed to read triggers.json for server '{server_name}'"
            ))
        }
    }
}

/// Saves the trigger definitions map to `triggers.json` for a given server.
///
/// This will overwrite the existing file if it exists. It assumes the
/// parent `triggers` directory already exists.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose triggers should be saved.
/// * `triggers` - The `HashMap<String, TriggerDefinition>` data structure to save.
///
/// # Errors
///
/// Returns an error if the server or triggers directory cannot be accessed, or if
/// `triggers.json` cannot be written.
pub fn save_triggers<S: ::std::hash::BuildHasher>(
    server_name: &str,
    triggers: &HashMap<String, TriggerDefinition, S>,
) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let triggers_dir = smudgy_dir.join(server_name).join("triggers");

    // Basic check to ensure the triggers directory exists.
    if !triggers_dir.is_dir() {
        return Err(anyhow::anyhow!(
            "Triggers directory not found for server '{}': {:?}",
            server_name,
            triggers_dir
        ));
    }

    let triggers_path = triggers_dir.join("triggers.json");

    let json_content = serde_json::to_string_pretty(triggers).context(format!(
        "Failed to serialize triggers for server '{server_name}'"
    ))?;

    write_atomic(&triggers_path, json_content.as_bytes()).context(format!(
        "Failed to write triggers.json for server '{server_name}' at {}",
        triggers_path.display()
    ))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_pattern_serialization() {
        let trigger = TriggerDefinition {
            patterns: Some(vec!["test_pattern".to_string()]),
            raw_patterns: Some(vec!["raw_test".to_string()]),
            anti_patterns: Some(vec!["anti_test".to_string()]),
            enabled: true,
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&trigger).expect("Failed to serialize");

        // Should serialize single elements as "pattern", not "patterns"
        assert!(json.contains("\"pattern\": \"test_pattern\""));
        assert!(json.contains("\"raw_pattern\": \"raw_test\""));
        assert!(json.contains("\"anti_pattern\": \"anti_test\""));
        assert!(!json.contains("\"patterns\""));
        assert!(!json.contains("\"raw_patterns\""));
        assert!(!json.contains("\"anti_patterns\""));
    }

    #[test]
    fn test_multiple_patterns_serialization() {
        let trigger = TriggerDefinition {
            patterns: Some(vec!["pattern1".to_string(), "pattern2".to_string()]),
            raw_patterns: Some(vec!["raw1".to_string(), "raw2".to_string()]),
            anti_patterns: Some(vec!["anti1".to_string(), "anti2".to_string()]),
            enabled: true,
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&trigger).expect("Failed to serialize");

        // Should serialize multiple elements as "patterns", not "pattern"
        assert!(json.contains("\"patterns\": ["));
        assert!(json.contains("\"raw_patterns\": ["));
        assert!(json.contains("\"anti_patterns\": ["));
        assert!(!json.contains("\"pattern\": "));
        assert!(!json.contains("\"raw_pattern\": "));
        assert!(!json.contains("\"anti_pattern\": "));
    }

    #[test]
    fn test_deserialization_single_pattern() {
        let json = r#"{
            "pattern": "test_pattern",
            "raw_pattern": "raw_test",
            "anti_pattern": "anti_test"
        }"#;

        let trigger: TriggerDefinition = serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(trigger.patterns, Some(vec!["test_pattern".to_string()]));
        assert_eq!(trigger.raw_patterns, Some(vec!["raw_test".to_string()]));
        assert_eq!(trigger.anti_patterns, Some(vec!["anti_test".to_string()]));
        assert_eq!(trigger.priority, 0);
        assert!(trigger.fallthrough);
    }

    #[test]
    fn test_deserialization_multiple_patterns() {
        let json = r#"{
            "patterns": ["pattern1", "pattern2"],
            "raw_patterns": ["raw1", "raw2"],
            "anti_patterns": ["anti1", "anti2"]
        }"#;

        let trigger: TriggerDefinition = serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(
            trigger.patterns,
            Some(vec!["pattern1".to_string(), "pattern2".to_string()])
        );
        assert_eq!(
            trigger.raw_patterns,
            Some(vec!["raw1".to_string(), "raw2".to_string()])
        );
        assert_eq!(
            trigger.anti_patterns,
            Some(vec!["anti1".to_string(), "anti2".to_string()])
        );
    }

    #[test]
    fn test_deserialization_mixed_formats() {
        // Test that both singular and plural forms can be present and are combined
        let json = r#"{
            "pattern": "single",
            "patterns": ["multi1", "multi2"],
            "raw_pattern": "raw_single",
            "anti_patterns": ["anti1", "anti2"]
        }"#;

        let trigger: TriggerDefinition = serde_json::from_str(json).expect("Failed to deserialize");

        // Single pattern should be prepended to patterns array
        assert_eq!(
            trigger.patterns,
            Some(vec![
                "single".to_string(),
                "multi1".to_string(),
                "multi2".to_string()
            ])
        );
        assert_eq!(trigger.raw_patterns, Some(vec!["raw_single".to_string()]));
        assert_eq!(
            trigger.anti_patterns,
            Some(vec!["anti1".to_string(), "anti2".to_string()])
        );
    }

    #[test]
    fn test_empty_patterns_serialization() {
        let trigger = TriggerDefinition {
            patterns: Some(vec![]),
            enabled: true,
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&trigger).expect("Failed to serialize");

        // Empty vectors should not be serialized
        assert!(!json.contains("pattern"));
    }

    #[test]
    fn matchers_sidecar_round_trips_and_stays_sparse() {
        use crate::models::matchers::{MatcherRole, MatcherSyntax, TriggerMatcherSource};

        let trigger = TriggerDefinition {
            patterns: Some(vec![r"^You\s+are\s+(?<state>.*?)\.$".to_string()]),
            matchers: Some(vec![TriggerMatcherSource {
                role: MatcherRole::Match,
                syntax: MatcherSyntax::Pattern,
                source: "You are {state}.".to_string(),
                anchor_start: true,
                anchor_end: true,
                color: None,
            }]),
            ..Default::default()
        };
        let json = serde_json::to_string(&trigger).unwrap();
        assert!(json.contains("\"matchers\""));
        let back: TriggerDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, trigger);

        // No sidecar (or an empty one) serializes nothing, and pre-sidecar
        // files load with `None` — the verbatim-regex degradation.
        let plain = serde_json::to_string(&TriggerDefinition::default()).unwrap();
        assert!(!plain.contains("matchers"));
        let old: TriggerDefinition = serde_json::from_str(r#"{"pattern":"^x$"}"#).unwrap();
        assert!(old.matchers.is_none());
        let empty: TriggerDefinition =
            serde_json::from_str(r#"{"pattern":"^x$","matchers":[]}"#).unwrap();
        assert!(empty.matchers.is_none());
    }

    #[test]
    fn state_exposures_ride_both_hand_written_serde_halves() {
        use crate::models::state_exposure::StateExposure;

        // A capture-only trigger serializes exactly what it did before the field existed,
        // and an older file (no `state` key) loads with an empty list.
        let capture_only = TriggerDefinition {
            patterns: Some(vec![r"^(?<who>\w+) says".to_string()]),
            script: Some("say $who".to_string()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&capture_only).unwrap(),
            r#"{"pattern":"^(?<who>\\w+) says","script":"say $who"}"#
        );
        let old: TriggerDefinition =
            serde_json::from_str(r#"{"pattern":"^x$","script":"y"}"#).unwrap();
        assert!(old.state.is_empty());
        let empty: TriggerDefinition =
            serde_json::from_str(r#"{"pattern":"^x$","state":[]}"#).unwrap();
        assert!(empty.state.is_empty());
        assert!(!serde_json::to_string(&empty).unwrap().contains("state"));

        let exposing = TriggerDefinition {
            state: vec![
                StateExposure {
                    producer: "gmcp".to_string(),
                    handle: None,
                    name_override: None,
                    paths: vec!["Char.Vitals".to_string()],
                },
                StateExposure {
                    producer: "smudgy://kapusniak/arctic-prompt".to_string(),
                    handle: Some("prompt".to_string()),
                    name_override: None,
                    paths: vec![String::new()],
                },
            ],
            ..capture_only.clone()
        };
        let json = serde_json::to_string(&exposing).unwrap();
        assert!(json.ends_with(
            r#""state":[{"producer":"gmcp","paths":["Char.Vitals"]},{"producer":"smudgy://kapusniak/arctic-prompt","handle":"prompt","paths":[""]}]}"#
        ), "{json}");
        let back: TriggerDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, exposing);
    }

    #[test]
    fn prompt_survives_serialization_roundtrip() {
        let trigger = TriggerDefinition {
            patterns: Some(vec!["^hp".to_string()]),
            prompt: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&trigger).expect("Failed to serialize");
        assert!(json.contains("\"prompt\":true"));

        let deserialized: TriggerDefinition =
            serde_json::from_str(&json).expect("Failed to deserialize");
        assert!(deserialized.prompt);

        // The sparse-serialization contract: a default (false) prompt is omitted.
        let default_json = serde_json::to_string(&TriggerDefinition::default()).unwrap();
        assert!(!default_json.contains("prompt"));
    }

    #[test]
    fn test_roundtrip_serialization() {
        let original = TriggerDefinition {
            patterns: Some(vec!["test".to_string()]),
            raw_patterns: Some(vec!["raw1".to_string(), "raw2".to_string()]),
            script: Some("console.log('test');".to_string()),
            package: Some("test/package".to_string()),
            language: ScriptLang::JS,
            enabled: false,
            priority: 42,
            fallthrough: false,
            ..Default::default()
        };

        let json = serde_json::to_string(&original).expect("Failed to serialize");
        let deserialized: TriggerDefinition =
            serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(original, deserialized);
        assert!(json.contains("\"priority\":42"));
        assert!(json.contains("\"fallthrough\":false"));
    }
}
