//! Portable preference value types shared by every Smudgy host.
//!
//! Persistence and UI reducers remain host-owned. These enums live here so
//! native and browser settings cannot silently assign different wire values or
//! rendering semantics to the same user choice.

use serde::{Deserialize, Serialize};

/// How an SGR bold attribute is presented in terminal output.
#[derive(Serialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalBoldMode {
    /// Increase the selected terminal font's weight without changing color.
    Bold,
    /// Use the bright ANSI palette without changing font weight.
    Bright,
    /// Increase font weight and use the bright ANSI palette.
    #[default]
    BoldAndBright,
}

impl TerminalBoldMode {
    pub const ALL: [Self; 3] = [Self::Bold, Self::Bright, Self::BoldAndBright];

    #[must_use]
    pub const fn uses_bold_weight(self) -> bool {
        matches!(self, Self::Bold | Self::BoldAndBright)
    }

    #[must_use]
    pub const fn uses_bright_palette(self) -> bool {
        matches!(self, Self::Bright | Self::BoldAndBright)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalBoldModeName {
    Bold,
    Bright,
    BoldAndBright,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TerminalBoldModeCompat {
    Name(TerminalBoldModeName),
    LegacyBool(bool),
}

impl From<TerminalBoldModeCompat> for TerminalBoldMode {
    fn from(value: TerminalBoldModeCompat) -> Self {
        match value {
            TerminalBoldModeCompat::Name(TerminalBoldModeName::Bold) => Self::Bold,
            TerminalBoldModeCompat::Name(TerminalBoldModeName::Bright) => Self::Bright,
            TerminalBoldModeCompat::Name(TerminalBoldModeName::BoldAndBright) => {
                Self::BoldAndBright
            }
            TerminalBoldModeCompat::LegacyBool(true) => Self::BoldAndBright,
            TerminalBoldModeCompat::LegacyBool(false) => Self::Bold,
        }
    }
}

impl<'de> Deserialize<'de> for TerminalBoldMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        TerminalBoldModeCompat::deserialize(deserializer).map(Into::into)
    }
}

/// What the session command input does with text after submission.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandInputBehavior {
    /// Select the sent text, then clear it when the input loses focus.
    SelectAllClearOnBlur,
    /// Select the sent text and retain it until it is replaced.
    #[default]
    SelectAll,
    /// Clear the input immediately on send.
    Clear,
}

impl CommandInputBehavior {
    pub const ALL: [Self; 3] = [Self::SelectAllClearOnBlur, Self::SelectAll, Self::Clear];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SelectAllClearOnBlur => "Select all on send, clear when unfocused",
            Self::SelectAll => "Select all on send",
            Self::Clear => "Clear on send",
        }
    }
}

impl std::fmt::Display for CommandInputBehavior {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_mode_keeps_the_legacy_boolean_migration() {
        assert_eq!(
            serde_json::from_str::<TerminalBoldMode>("true").unwrap(),
            TerminalBoldMode::BoldAndBright
        );
        assert_eq!(
            serde_json::from_str::<TerminalBoldMode>("false").unwrap(),
            TerminalBoldMode::Bold
        );
        assert_eq!(
            serde_json::from_str::<TerminalBoldMode>("\"bright\"").unwrap(),
            TerminalBoldMode::Bright
        );
    }
}
