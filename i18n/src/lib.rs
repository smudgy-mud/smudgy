//! Catalog loading and Fluent formatting for Smudgy first-party client text.
//!
//! This crate deliberately has no selected-locale preference, system-locale
//! detection, or process-global active translator. Applications own that
//! policy and keep a [`Translator`] value; this library only supplies the
//! embedded catalog registry, locale matching, English fallback, and
//! type-preserving Fluent formatting.

use std::sync::LazyLock;

pub use fluent_bundle::FluentArgs;
use fluent_bundle::FluentResource;
use fluent_bundle::concurrent::FluentBundle;
use unic_langid::LanguageIdentifier;

mod manifest;

pub(crate) struct CatalogSource {
    tag: &'static str,
    aliases: &'static [&'static str],
    display_name: &'static str,
    source: &'static str,
}

/// Public metadata for one embedded catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogInfo {
    /// Canonical BCP-47 locale tag persisted by the application.
    pub tag: &'static str,
    /// Native language name shown in locale pickers.
    pub display_name: &'static str,
}

struct Catalog {
    info: CatalogInfo,
    bundle: FluentBundle<FluentResource>,
}

static CATALOGS: LazyLock<Vec<Catalog>> = LazyLock::new(|| {
    manifest::CATALOGS
        .iter()
        .map(|source| Catalog {
            info: CatalogInfo {
                tag: source.tag,
                display_name: source.display_name,
            },
            bundle: make_bundle(source.tag, source.source),
        })
        .collect()
});

static FALLBACK_INDEX: LazyLock<usize> = LazyLock::new(|| {
    manifest::CATALOGS
        .iter()
        .position(|source| source.tag == manifest::FALLBACK_TAG)
        .expect("the catalog manifest must include its fallback locale")
});

fn make_bundle(tag: &str, source: &str) -> FluentBundle<FluentResource> {
    let language: LanguageIdentifier = tag.parse().expect("built-in locale tag is valid");
    let resource = FluentResource::try_new(source.to_string()).unwrap_or_else(|(_, errors)| {
        panic!("invalid built-in Fluent resource for {tag}: {errors:?}")
    });
    let mut bundle = FluentBundle::new_concurrent(vec![language]);
    // Smudgy renders terminal-style UI text where invisible bidi isolation
    // marks would leak into copy/paste and cursor offsets. Every catalog in
    // the manifest is currently left-to-right.
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("duplicate Fluent messages in {tag}: {errors:?}"));
    bundle
}

/// Iterate over the data-driven catalog manifest in picker order.
///
/// Adding a language requires one manifest entry and its Fluent file; no
/// locale enum or selection `match` needs to change.
#[must_use]
pub fn available_catalogs() -> impl ExactSizeIterator<Item = CatalogInfo> {
    manifest::CATALOGS.iter().map(|source| CatalogInfo {
        tag: source.tag,
        display_name: source.display_name,
    })
}

/// A concrete catalog choice with automatic English fallback.
///
/// The value is cheap to copy: immutable catalog bundles live in the
/// process-wide registry, while the application owns which `Translator` is
/// currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translator {
    primary_index: usize,
}

impl Translator {
    /// Match one locale tag against catalog tags and manifest aliases.
    ///
    /// A language-only alias (for example `en`) accepts all of that
    /// language's regions. Script/region-bearing aliases are exact, which
    /// lets the manifest avoid silently treating `zh-HK` as `zh-TW`.
    #[must_use]
    pub fn for_tag(tag: &str) -> Option<Self> {
        let requested = parse_locale(tag)?;
        manifest::CATALOGS
            .iter()
            .position(|source| {
                locale_matches(&requested, source.tag)
                    || source
                        .aliases
                        .iter()
                        .any(|alias| locale_matches(&requested, alias))
            })
            .map(|primary_index| Self { primary_index })
    }

    /// Select the first supported tag in an ordered preference list.
    #[must_use]
    pub fn negotiate<'a>(tags: impl IntoIterator<Item = &'a str>) -> Self {
        tags.into_iter().find_map(Self::for_tag).unwrap_or_default()
    }

    /// Canonical tag of the selected primary catalog.
    #[must_use]
    pub fn tag(self) -> &'static str {
        CATALOGS[self.primary_index].info.tag
    }

    /// Translate a message without variables.
    #[must_use]
    pub fn translate(self, id: &str) -> String {
        self.translate_with(id, &FluentArgs::new())
    }

    /// Translate a message while preserving native Fluent argument types.
    ///
    /// If the selected catalog does not yet contain the message, English is
    /// used. This permits contributors to land incomplete catalogs
    /// incrementally without blank UI strings.
    #[must_use]
    pub fn translate_with(self, id: &str, args: &FluentArgs<'_>) -> String {
        let primary = &CATALOGS[self.primary_index].bundle;
        format_message(primary, id, args)
            .or_else(|| {
                (self.primary_index != *FALLBACK_INDEX)
                    .then(|| format_message(&CATALOGS[*FALLBACK_INDEX].bundle, id, args))
                    .flatten()
            })
            .unwrap_or_else(|| format!("⟦{id}⟧"))
    }
}

impl Default for Translator {
    fn default() -> Self {
        Self {
            primary_index: *FALLBACK_INDEX,
        }
    }
}

fn parse_locale(tag: &str) -> Option<LanguageIdentifier> {
    tag.trim()
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-")
        .parse()
        .ok()
}

fn locale_matches(requested: &LanguageIdentifier, candidate: &str) -> bool {
    let Some(candidate) = parse_locale(candidate) else {
        return false;
    };
    if candidate.script.is_none() && candidate.region.is_none() {
        requested.language == candidate.language
    } else {
        requested == &candidate
    }
}

fn format_message(
    bundle: &FluentBundle<FluentResource>,
    id: &str,
    args: &FluentArgs<'_>,
) -> Option<String> {
    let message = bundle.get_message(id)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    let rendered = bundle.format_pattern(pattern, Some(args), &mut errors);
    errors.is_empty().then(|| rendered.into_owned())
}

/// Build a translated message without erasing Fluent value types.
///
/// The first argument is an application-owned [`Translator`]. Values are
/// inserted directly into [`FluentArgs`], so numbers remain numbers for
/// plural/select rules instead of becoming strings.
#[macro_export]
macro_rules! t {
    ($translator:expr, $id:literal $(,)?) => {
        ($translator).translate($id)
    };
    ($translator:expr, $id:literal, $($name:literal => $value:expr),+ $(,)?) => {{
        let mut args = $crate::FluentArgs::new();
        $(args.set($name, $value);)+
        ($translator).translate_with($id, &args)
    }};
}

#[cfg(test)]
fn catalog_variables(
    source: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    use regex::Regex;
    use std::collections::{BTreeMap, BTreeSet};

    let variable = Regex::new(r"\{\s*\$([A-Za-z][A-Za-z0-9_-]*)").unwrap();
    let mut messages: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;

    for line in source.lines() {
        if !line.starts_with(char::is_whitespace)
            && !line.starts_with('#')
            && let Some((id, value)) = line.split_once('=')
        {
            let id = id.trim().to_string();
            current = Some(id.clone());
            messages.insert(id, value.trim_start().to_string());
            continue;
        }
        if line.starts_with(char::is_whitespace)
            && let Some(id) = &current
            && let Some(value) = messages.get_mut(id)
        {
            value.push('\n');
            value.push_str(line);
        }
    }

    messages
        .into_iter()
        .map(|(id, value)| {
            let variables = variable
                .captures_iter(&value)
                .map(|capture| capture[1].to_string())
                .collect::<BTreeSet<_>>();
            (id, variables)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_catalogs_parse_and_have_unique_tags() {
        let mut tags = std::collections::BTreeSet::new();
        for (index, catalog) in CATALOGS.iter().enumerate() {
            assert!(tags.insert(catalog.info.tag), "duplicate catalog tag");
            assert_eq!(catalog.info, available_catalogs().nth(index).unwrap());
        }
    }

    #[test]
    fn catalog_matching_is_data_driven_and_region_safe() {
        assert_eq!(Translator::for_tag("zh_TW.UTF-8").unwrap().tag(), "zh-TW");
        assert_eq!(Translator::for_tag("zh-Hant-TW").unwrap().tag(), "zh-TW");
        assert_eq!(Translator::for_tag("en_GB.UTF-8").unwrap().tag(), "en-US");
        assert!(Translator::for_tag("zh-HK").is_none());
        assert!(Translator::for_tag("zh-MO").is_none());
        assert!(Translator::for_tag("zh-CN").is_none());
        assert!(Translator::for_tag("C.UTF-8").is_none());
    }

    #[test]
    fn secondary_catalogs_may_be_incremental_but_matching_keys_keep_variables() {
        let fallback = catalog_variables(manifest::CATALOGS[*FALLBACK_INDEX].source);
        for source in manifest::CATALOGS
            .iter()
            .filter(|source| source.tag != manifest::FALLBACK_TAG)
        {
            for (id, variables) in catalog_variables(source.source) {
                assert_eq!(
                    fallback.get(&id),
                    Some(&variables),
                    "{id} in {} must match the fallback variables",
                    source.tag
                );
            }
        }
    }

    #[test]
    fn missing_primary_message_falls_back_to_english() {
        let partial = make_bundle("pl-PL", "language = Język");
        let args = FluentArgs::new();
        let rendered = format_message(&partial, "action-save", &args)
            .or_else(|| format_message(&CATALOGS[*FALLBACK_INDEX].bundle, "action-save", &args));
        assert_eq!(rendered.as_deref(), Some("Save"));
    }

    #[test]
    fn macro_preserves_numeric_fluent_values() {
        let translator = Translator::default();
        assert_eq!(
            t!(translator, "message-count", "count" => 1_i64),
            "One message"
        );
        assert_eq!(
            t!(translator, "message-count", "count" => 3_i64),
            "3 messages"
        );
    }

    #[test]
    fn unknown_message_is_visible_instead_of_silently_blank() {
        assert_eq!(
            Translator::default().translate("not-a-real-message"),
            "⟦not-a-real-message⟧"
        );
    }
}
