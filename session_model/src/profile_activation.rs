//! Profile-scoped activation for server-owned automation roots on both hosts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The profiles in which an automation root starts automatically.
///
/// `All` also includes profiles that the user creates later. `Selected` does not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProfileActivation {
    /// Start in every current and future profile.
    #[default]
    All,
    /// Do not start in any profile.
    None,
    /// Start only in the named profiles.
    Selected { profiles: BTreeSet<String> },
}

impl ProfileActivation {
    /// Converts the legacy server-wide enabled flag into a profile scope.
    #[must_use]
    pub const fn from_legacy(enabled: bool) -> Self {
        if enabled { Self::All } else { Self::None }
    }

    /// Whether this scope enables `profile_name`.
    #[must_use]
    pub fn is_enabled_for(&self, profile_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Selected { profiles } => profiles.contains(profile_name),
        }
    }

    /// Keeps only known profile names and reduces an empty set to `None`.
    /// A selection covering every *current* profile stays `Selected`: only an
    /// explicit `All` also activates profiles created in the future.
    #[must_use]
    pub fn canonicalize(mut self, known_profiles: &BTreeSet<String>) -> Self {
        let Self::Selected { profiles } = &mut self else {
            return self;
        };
        profiles.retain(|profile| known_profiles.contains(profile));
        if profiles.is_empty() {
            Self::None
        } else {
            self
        }
    }

    /// Returns the current named selection. `All` expands to every known profile.
    #[must_use]
    pub fn selected_profiles(&self, known_profiles: &BTreeSet<String>) -> BTreeSet<String> {
        match self {
            Self::All => known_profiles.clone(),
            Self::None => BTreeSet::new(),
            Self::Selected { profiles } => profiles.intersection(known_profiles).cloned().collect(),
        }
    }

    /// Changes one profile and returns a canonical complete scope.
    #[must_use]
    pub fn with_profile(
        &self,
        profile_name: &str,
        enabled: bool,
        known_profiles: &BTreeSet<String>,
    ) -> Self {
        // A no-op must not change the meaning of `All`: selecting an already
        // enabled profile should keep future profiles enabled as well.
        if self.is_enabled_for(profile_name) == enabled {
            return self.clone();
        }
        let mut profiles = self.selected_profiles(known_profiles);
        if enabled {
            profiles.insert(profile_name.to_string());
        } else {
            profiles.remove(profile_name);
        }
        Self::Selected { profiles }.canonicalize(known_profiles)
    }

    /// Removes one deleted profile without interpreting any other profile name.
    ///
    /// This intentionally does not call [`Self::canonicalize`]. Profile discovery can be
    /// temporarily incomplete when another profile's file is unreadable. Canonicalizing against
    /// that lossy inventory could erase the unreadable profile or turn a selected set into `All`.
    #[must_use]
    pub fn without_profile(mut self, profile_name: &str) -> Self {
        let Self::Selected { profiles } = &mut self else {
            return self;
        };
        profiles.remove(profile_name);
        if profiles.is_empty() {
            Self::None
        } else {
            self
        }
    }

    /// The fail-closed value written for clients that do not know about profile scopes.
    #[must_use]
    pub const fn legacy_enabled(&self) -> bool {
        matches!(self, Self::All)
    }
}

/// Resolves an additive activation field against its legacy bool mirror.
#[must_use]
pub fn resolve_activation(
    activation: Option<&ProfileActivation>,
    legacy_enabled: bool,
) -> ProfileActivation {
    activation
        .cloned()
        .unwrap_or_else(|| ProfileActivation::from_legacy(legacy_enabled))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> BTreeSet<String> {
        ["Alt", "Main"].into_iter().map(str::to_string).collect()
    }

    #[test]
    fn all_and_none_apply_to_every_name() {
        assert!(ProfileActivation::All.is_enabled_for("future"));
        assert!(!ProfileActivation::None.is_enabled_for("Main"));
    }

    #[test]
    fn selected_sets_are_canonical() {
        let selected = ProfileActivation::Selected {
            profiles: ["Main", "removed"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
        .canonicalize(&known());
        assert_eq!(
            selected,
            ProfileActivation::Selected {
                profiles: ["Main"].into_iter().map(str::to_string).collect()
            }
        );

        let current_only = ProfileActivation::Selected { profiles: known() }.canonicalize(&known());
        assert_eq!(
            current_only,
            ProfileActivation::Selected { profiles: known() }
        );
        assert!(!current_only.is_enabled_for("Future"));
    }

    #[test]
    fn removing_one_profile_from_all_does_not_include_future_profiles() {
        let activation = ProfileActivation::All.with_profile("Alt", false, &known());
        assert!(activation.is_enabled_for("Main"));
        assert!(!activation.is_enabled_for("Alt"));
        assert!(!activation.is_enabled_for("Future"));
        assert!(!activation.legacy_enabled());
    }

    #[test]
    fn idempotent_toggle_keeps_future_profile_policy() {
        assert_eq!(
            ProfileActivation::All.with_profile("Main", true, &known()),
            ProfileActivation::All
        );
        assert_eq!(
            ProfileActivation::None.with_profile("Main", false, &known()),
            ProfileActivation::None
        );
    }

    #[test]
    fn serde_is_sorted_and_tagged() {
        let activation = ProfileActivation::Selected {
            profiles: ["Main", "Alt"].into_iter().map(str::to_string).collect(),
        };
        assert_eq!(
            serde_json::to_value(activation).unwrap(),
            serde_json::json!({ "mode": "selected", "profiles": ["Alt", "Main"] })
        );
    }

    #[test]
    fn deleting_one_profile_does_not_canonicalize_against_an_inventory() {
        let activation = ProfileActivation::Selected {
            profiles: ["Main", "temporarily-unreadable"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        assert_eq!(activation.clone().without_profile("Alt"), activation);
        assert_eq!(
            activation.without_profile("Main"),
            ProfileActivation::Selected {
                profiles: ["temporarily-unreadable"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            }
        );
    }
}
