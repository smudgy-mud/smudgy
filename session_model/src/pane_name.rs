//! Portable pane-name identity policy shared by runtime and saved layouts.

/// Reserved name for the fused output/input pane in every namespace.
pub const MAIN_PANE_NAME: &str = "main";

/// Maximum number of non-main panes per session.
pub const NON_MAIN_PANE_CAP: usize = 16;

/// Case-fold a pane name to its stable identity form.
#[must_use]
#[inline]
pub fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Whether a name resolves to the reserved main pane.
#[must_use]
#[inline]
pub fn is_main_pane_name(name: &str) -> bool {
    fold(name) == MAIN_PANE_NAME
}

/// Validate the name rules enforced when a pane is created or restored.
///
/// # Errors
///
/// Returns the violated rule in user-presentable terms.
#[inline]
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is empty".to_string());
    }
    if name.chars().count() > 64 {
        return Err("name is longer than 64 characters".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("name contains control characters".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_validation_are_portable() {
        assert_eq!(fold("MaIn"), MAIN_PANE_NAME);
        assert!(is_main_pane_name("MAIN"));
        assert!(validate_name("room/map").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("line\nbreak").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
    }
}
