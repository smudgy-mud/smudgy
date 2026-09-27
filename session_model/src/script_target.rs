//! Explicit package runtime compatibility, independent of the JS engine.

use serde::{Deserialize, Serialize};

/// Legacy manifests are native-only. Web and cross-host support are opt-ins;
/// this is a compatibility declaration, not a security permission.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptTarget {
    #[default]
    Native,
    Web,
    Both,
}

impl ScriptTarget {
    #[must_use]
    pub const fn supports_native(self) -> bool {
        matches!(self, Self::Native | Self::Both)
    }

    #[must_use]
    pub const fn supports_web(self) -> bool {
        matches!(self, Self::Web | Self::Both)
    }

    #[must_use]
    pub const fn is_native(&self) -> bool {
        matches!(self, Self::Native)
    }
}

impl std::fmt::Display for ScriptTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Native => "Native",
            Self::Web => "Web",
            Self::Both => "Both",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_default_and_explicit_targets() {
        #[derive(Deserialize)]
        struct Manifest {
            #[serde(default)]
            target: ScriptTarget,
        }
        assert_eq!(
            serde_json::from_str::<Manifest>("{}").unwrap().target,
            ScriptTarget::Native
        );
        assert_eq!(
            serde_json::from_str::<Manifest>(r#"{"target":"web"}"#)
                .unwrap()
                .target,
            ScriptTarget::Web
        );
        assert!(ScriptTarget::Both.supports_native() && ScriptTarget::Both.supports_web());
        assert!(!ScriptTarget::Web.supports_native());
    }
}
