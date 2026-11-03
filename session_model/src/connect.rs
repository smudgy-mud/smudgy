//! Small, host-independent Connect policies. Persistence remains host-owned.

use std::collections::BTreeSet;

/// Shared quick-connect profile identity. Both hosts persist it as an
/// ordinary, editable profile, while server-wide automation remains shared.
pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// Server and profile names also become native directory names, so both hosts
/// enforce the same portable alphabet.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Prefer the last/current selection when it still exists; otherwise use the
/// first item in the host's ordering. Returns no selection for an empty list.
#[must_use]
pub fn preferred_index<'a>(
    names: impl IntoIterator<Item = &'a str>,
    preferred: Option<&str>,
) -> Option<usize> {
    let mut first = None;
    for (index, name) in names.into_iter().enumerate() {
        first.get_or_insert(index);
        if Some(name) == preferred {
            return Some(index);
        }
    }
    first
}

/// The server/profile pairs that currently have an open session, connected
/// or offline. Connect refuses to delete a server or profile in this set: the
/// live session still runs on its directory, automations, map, and
/// credentials, so deleting them would pull state out from under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenSessions(BTreeSet<(String, String)>);

impl OpenSessions {
    #[must_use]
    pub fn from_pairs<S: Into<String>, P: Into<String>>(
        pairs: impl IntoIterator<Item = (S, P)>,
    ) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(server, profile)| (server.into(), profile.into()))
                .collect(),
        )
    }

    /// Whether any session is open on `server`, under any profile.
    #[must_use]
    pub fn has_server(&self, server: &str) -> bool {
        self.0.iter().any(|(open, _)| open == server)
    }

    /// Whether a session is open on exactly this server and profile.
    #[must_use]
    pub fn has_profile(&self, server: &str, profile: &str) -> bool {
        self.0
            .iter()
            .any(|(open_server, open_profile)| open_server == server && open_profile == profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_policy_and_selection() {
        assert!(valid_name("MUD_42-α"));
        assert!(!valid_name(""));
        assert!(!valid_name("../mud"));
        let names = ["first", "second"];
        assert_eq!(preferred_index(names, Some("second")), Some(1));
        assert_eq!(preferred_index(names, Some("removed")), Some(0));
        assert_eq!(preferred_index([], None), None);
    }

    #[test]
    fn open_sessions_match_exact_server_and_profile() {
        let open = OpenSessions::from_pairs([("Aardwolf", "Mage"), ("Aardwolf", "Thief")]);
        assert!(open.has_server("Aardwolf"));
        assert!(!open.has_server("aardwolf"));
        assert!(!open.has_server("Discworld"));
        assert!(open.has_profile("Aardwolf", "Mage"));
        assert!(!open.has_profile("Aardwolf", "Cleric"));
        assert!(!open.has_profile("Discworld", "Mage"));
        assert!(!OpenSessions::default().has_server("Aardwolf"));
    }
}
