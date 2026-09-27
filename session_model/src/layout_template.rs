//! Identity of a stored layout template, independent of its storage backend.

/// A named server layout or the server's last-session snapshot. Both enter the
/// same apply flow; only the source of the template differs by host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    Named(String),
    LastSession,
}

impl std::fmt::Display for TemplateSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => write!(formatter, "layout '{name}'"),
            Self::LastSession => write!(formatter, "the last session"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TemplateSource;

    #[test]
    fn labels_both_template_sources() {
        assert_eq!(
            TemplateSource::Named("combat".into()).to_string(),
            "layout 'combat'"
        );
        assert_eq!(TemplateSource::LastSession.to_string(), "the last session");
    }
}
