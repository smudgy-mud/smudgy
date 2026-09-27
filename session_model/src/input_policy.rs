//! Outgoing command syntax shared by the native and browser runtimes.

use serde::{Deserialize, Serialize};

/// Native's default command separator. Empty disables separator splitting.
pub const DEFAULT_COMMAND_SEPARATOR: &str = ";";
/// Native's default verbatim-send prefix. Empty disables the prefix.
pub const DEFAULT_RAW_LINE_PREFIX: &str = "\\\\";
/// Fixed-width mask avoids revealing a saved password's length in an echoed command.
pub const REDACTION_MASK: &str = "********";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSyntax {
    pub separator: String,
    pub raw_prefix: String,
}

impl Default for CommandSyntax {
    fn default() -> Self {
        Self {
            separator: DEFAULT_COMMAND_SEPARATOR.into(),
            raw_prefix: DEFAULT_RAW_LINE_PREFIX.into(),
        }
    }
}

impl CommandSyntax {
    pub fn validate(self) -> Result<Self, &'static str> {
        (self.separator.chars().count() <= 4)
            .then_some(self)
            .ok_or("command separator exceeds four characters")
    }
}

/// Split an outgoing chunk on newlines and, when configured, a command
/// separator. Empty pieces are retained: pressing Enter on a blank command
/// and adjacent separators each still send a line.
#[inline]
#[must_use]
pub fn split_commands<'a>(text: &'a str, separator: &str) -> Vec<&'a str> {
    if separator.is_empty() {
        text.split('\n').collect()
    } else {
        text.split('\n')
            .flat_map(|chunk| chunk.split(separator))
            .collect()
    }
}

/// Replace every non-empty secret in a command's visible copy. The wire copy
/// remains unchanged; callers must not pass this result to a transport.
#[must_use]
pub fn redact(text: &str, redactions: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in redactions {
        if !secret.is_empty() {
            out = out.replace(secret.as_str(), REDACTION_MASK);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_empty_commands_and_multi_character_separators() {
        assert_eq!(
            split_commands("north;;south\n", ";;"),
            ["north", "south", ""]
        );
        assert_eq!(split_commands("north;;south\n", ""), ["north;;south", ""]);
        assert_eq!(split_commands("", ";"), [""]);
    }

    #[test]
    fn masks_each_nonempty_secret_without_exposing_length() {
        assert_eq!(
            redact("login name secret", &["secret".into()]),
            "login name ********"
        );
        assert_eq!(redact("look", &[String::new()]), "look");
    }
}
