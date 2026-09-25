//! Small, host-independent Connect policies. Persistence remains host-owned.

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
}
