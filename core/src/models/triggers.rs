use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::PathBuf,
};

use super::{ScriptLang, persistence::write_atomic, state_exposure::StateExposure};

// Helper function for serde to default boolean fields to true.
fn default_true() -> bool {
    true
}

/// The deepest an inner trigger can sit below a top-level trigger.
pub const MAX_INNER_DEPTH: usize = 8;

/// How many lines after its outer trigger's line an inner trigger may still match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineReach {
    /// No lines after the outer trigger's line.
    #[default]
    None,
    /// This many lines after the outer trigger's line.
    Lines(u32),
    /// No line limit: the watch ends only by `stopWatching()`, a restart, or a reset.
    Unlimited,
}

impl LineReach {
    /// The count form: `None` reads as `0`, `Unlimited` as `None`.
    #[must_use]
    pub fn lines(self) -> Option<u32> {
        match self {
            Self::None => Some(0),
            Self::Lines(count) => Some(count),
            Self::Unlimited => None,
        }
    }

    /// The reach for a count: `0` is `None`.
    #[must_use]
    pub fn from_lines(count: u32) -> Self {
        if count == 0 {
            Self::None
        } else {
            Self::Lines(count)
        }
    }

    /// Whether an input `after` lines past the outer trigger's line is within reach.
    #[must_use]
    pub fn covers(self, after: u64) -> bool {
        match self {
            Self::None => false,
            Self::Lines(count) => after <= u64::from(count),
            Self::Unlimited => true,
        }
    }

    /// The looser of two reaches.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unlimited, _) | (_, Self::Unlimited) => Self::Unlimited,
            (Self::None, other) | (other, Self::None) => other,
            (Self::Lines(a), Self::Lines(b)) => Self::Lines(a.max(b)),
        }
    }
}

impl Serialize for LineReach {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => serializer.serialize_u32(0),
            Self::Lines(count) => serializer.serialize_u32(*count),
            Self::Unlimited => serializer.serialize_str("unlimited"),
        }
    }
}

impl<'de> Deserialize<'de> for LineReach {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Count(u32),
            Word(String),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Count(count) => Ok(Self::from_lines(count)),
            Wire::Word(word) if word == "unlimited" => Ok(Self::Unlimited),
            Wire::Word(word) => Err(serde::de::Error::custom(format!(
                "within_lines must be a line count or \"unlimited\", not {word:?}"
            ))),
        }
    }
}

/// What an inner trigger does when its outer trigger fires again while it is still watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Overlap {
    /// Watch again from the new line; the earlier firing is forgotten.
    #[default]
    Restart,
    /// Watch each firing separately.
    Each,
}

/// What an inner trigger matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InnerInput {
    /// The line itself.
    #[default]
    Line,
    /// Each matched value of the outer trigger, one at a time, on the outer's own line.
    OuterValues,
}

/// How far, and how, an inner trigger watches after its outer trigger fires. Every field is
/// at its default for a top-level trigger, where the whole struct is inert.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InnerReach {
    pub within_lines: LineReach,
    /// Whether the trigger may fire on the outer trigger's own line. Default true.
    pub same_line: bool,
    /// Stop watching at the next prompt.
    pub until_prompt: bool,
    /// Fire at most once each time the outer trigger fires.
    pub once: bool,
    pub overlap: Overlap,
    pub input: InnerInput,
}

impl InnerReach {
    /// The reach of a top-level trigger, and of an inner trigger that watches nothing beyond
    /// the outer's own line.
    pub const DEFAULT: Self = Self {
        within_lines: LineReach::None,
        same_line: true,
        until_prompt: false,
        once: false,
        overlap: Overlap::Restart,
        input: InnerInput::Line,
    };

    /// Whether every field is at its default, so nothing needs saving.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// Whether the trigger can match anything at all: no lines after the outer's line with
    /// that line excluded can never fire.
    #[must_use]
    pub fn can_fire(&self) -> bool {
        self.same_line || self.within_lines != LineReach::None
    }
}

impl Default for InnerReach {
    fn default() -> Self {
        Self::DEFAULT
    }
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
    /// How this trigger watches after its outer trigger fires. Inert (all defaults) for a
    /// top-level trigger. Saved sparsely: only the non-default fields appear.
    pub reach: InnerReach,
    /// The triggers inside this one, by name. Saved nested under this entry so an older
    /// client, which does not know the key, loads none of them.
    pub inner: Option<BTreeMap<String, TriggerDefinition>>,
    /// The name of the trigger this one is inside. Runtime-only: the file expresses it by
    /// nesting, so it is never read from or written to JSON. The loader sets it while
    /// flattening the tree into registrations.
    pub outer: Option<String>,
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
            reach: InnerReach::DEFAULT,
            inner: None,
            outer: None,
        }
    }
}

// Custom Serialize implementation
impl Serialize for TriggerDefinition {
    #[allow(clippy::too_many_lines)]
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
        if self.reach.within_lines != LineReach::None {
            map.serialize_entry("within_lines", &self.reach.within_lines)?;
        }
        if !self.reach.same_line {
            map.serialize_entry("same_line", &false)?;
        }
        if self.reach.until_prompt {
            map.serialize_entry("until_prompt", &true)?;
        }
        if self.reach.once {
            map.serialize_entry("once", &true)?;
        }
        if self.reach.overlap != Overlap::Restart {
            map.serialize_entry("overlap", &self.reach.overlap)?;
        }
        if self.reach.input != InnerInput::Line {
            map.serialize_entry("input", &self.reach.input)?;
        }
        match &self.inner {
            Some(inner) if !inner.is_empty() => {
                map.serialize_entry("inner", inner)?;
            }
            _ => {}
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
        // The saved flags are independent switches, not an encodable state machine.
        #[allow(clippy::struct_excessive_bools)]
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
            #[serde(default)]
            within_lines: LineReach,
            #[serde(default = "default_true")]
            same_line: bool,
            #[serde(default)]
            until_prompt: bool,
            #[serde(default)]
            once: bool,
            #[serde(default)]
            overlap: Overlap,
            #[serde(default)]
            input: InnerInput,
            #[serde(default)]
            inner: Option<BTreeMap<String, TriggerDefinition>>,
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
            reach: InnerReach {
                within_lines: helper.within_lines,
                same_line: helper.same_line,
                until_prompt: helper.until_prompt,
                once: helper.once,
                overlap: helper.overlap,
                input: helper.input,
            },
            // An empty map carries nothing; normalize to absent.
            inner: helper.inner.filter(|inner| !inner.is_empty()),
            outer: None,
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

    #[test]
    fn inner_triggers_nest_under_their_outer_and_stay_sparse() {
        let mut inner = BTreeMap::new();
        inner.insert(
            "item line".to_string(),
            TriggerDefinition {
                patterns: Some(vec!["^ (?<item>.+)$".to_string()]),
                script: Some("note".to_string()),
                reach: InnerReach {
                    within_lines: LineReach::Lines(40),
                    until_prompt: true,
                    ..InnerReach::DEFAULT
                },
                ..Default::default()
            },
        );
        inner.insert(
            "forever".to_string(),
            TriggerDefinition {
                patterns: Some(vec!["^x$".to_string()]),
                reach: InnerReach {
                    within_lines: LineReach::Unlimited,
                    same_line: false,
                    once: true,
                    overlap: Overlap::Each,
                    input: InnerInput::OuterValues,
                    ..InnerReach::DEFAULT
                },
                ..Default::default()
            },
        );
        let outer = TriggerDefinition {
            patterns: Some(vec!["^You are carrying:$".to_string()]),
            package: Some("inventory".to_string()),
            inner: Some(inner),
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&outer).unwrap();
        assert!(json.contains("\"inner\": {"));
        assert!(json.contains("\"within_lines\": 40"));
        assert!(json.contains("\"until_prompt\": true"));
        assert!(json.contains("\"within_lines\": \"unlimited\""));
        assert!(json.contains("\"same_line\": false"));
        assert!(json.contains("\"once\": true"));
        assert!(json.contains("\"overlap\": \"each\""));
        assert!(json.contains("\"input\": \"outerValues\""));
        // The outer itself is at every default: nothing about reach appears at its level.
        let top_level_lines: Vec<&str> = json
            .lines()
            .take_while(|l| !l.contains("\"inner\""))
            .collect();
        assert!(top_level_lines.iter().all(|l| !l.contains("within_lines")));
        assert!(
            !json.contains("\"outer\""),
            "placement is expressed by nesting, never stored"
        );

        let back: TriggerDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, outer);
        assert_eq!(back.inner.as_ref().unwrap().len(), 2);
        assert!(back.outer.is_none());
    }

    #[test]
    fn a_file_without_inner_triggers_loads_flat_and_an_empty_inner_map_is_absent() {
        let flat: TriggerDefinition = serde_json::from_str(r#"{"pattern": "x"}"#).unwrap();
        assert!(flat.inner.is_none());
        assert!(flat.reach.is_default());
        let emptied: TriggerDefinition =
            serde_json::from_str(r#"{"pattern": "x", "inner": {}}"#).unwrap();
        assert!(emptied.inner.is_none());
        let bad = serde_json::from_str::<TriggerDefinition>(
            r#"{"pattern": "x", "within_lines": "lots"}"#,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn reach_helpers() {
        assert_eq!(LineReach::from_lines(0), LineReach::None);
        assert!(LineReach::Lines(3).covers(3));
        assert!(!LineReach::Lines(3).covers(4));
        assert!(!LineReach::None.covers(1));
        assert!(LineReach::Unlimited.covers(u64::MAX));
        assert_eq!(
            LineReach::Lines(2).max(LineReach::Lines(5)),
            LineReach::Lines(5)
        );
        assert_eq!(
            LineReach::Lines(2).max(LineReach::Unlimited),
            LineReach::Unlimited
        );
        assert!(
            !InnerReach {
                same_line: false,
                ..InnerReach::DEFAULT
            }
            .can_fire()
        );
        assert!(
            InnerReach {
                same_line: false,
                within_lines: LineReach::Lines(1),
                ..InnerReach::DEFAULT
            }
            .can_fire()
        );
    }
}
