//! Embedded localization for Smudgy's first-party client text.
//!
//! English is the authoritative fallback. The active locale is process-wide so
//! core-generated client feedback and every window use one consistent language.

use std::fmt;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use serde::{Deserialize, Serialize};
use unic_langid::LanguageIdentifier;

const EN_US_SOURCE: &str = include_str!("../locales/en-US/main.ftl");
const ZH_TW_SOURCE: &str = include_str!("../locales/zh-TW/main.ftl");

/// The language choice persisted in `settings.json`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalePreference {
    /// Follow the desktop language, falling back to English when unsupported.
    #[default]
    System,
    English,
    TraditionalChinese,
}

impl LocalePreference {
    /// Every available choice in picker order.
    pub const ALL: [Self; 3] = [Self::System, Self::TraditionalChinese, Self::English];

    /// Stable BCP-47-like configuration value for diagnostics and CLI use.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en-US",
            Self::TraditionalChinese => "zh-TW",
        }
    }
}

impl fmt::Display for LocalePreference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = match self {
            Self::System => "locale-system",
            Self::English => "locale-english",
            Self::TraditionalChinese => "locale-traditional-chinese",
        };
        formatter.write_str(&translate(key))
    }
}

/// The concrete catalog currently used for rendering.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Locale {
    #[default]
    English = 0,
    TraditionalChinese = 1,
}

impl Locale {
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::English => "en-US",
            Self::TraditionalChinese => "zh-TW",
        }
    }
}

static ACTIVE_LOCALE: AtomicU8 = AtomicU8::new(Locale::English as u8);

static EN_US: LazyLock<FluentBundle<FluentResource>> =
    LazyLock::new(|| make_bundle("en-US", EN_US_SOURCE));
static ZH_TW: LazyLock<FluentBundle<FluentResource>> =
    LazyLock::new(|| make_bundle("zh-TW", ZH_TW_SOURCE));

fn make_bundle(tag: &str, source: &str) -> FluentBundle<FluentResource> {
    let language: LanguageIdentifier = tag.parse().expect("built-in locale tag is valid");
    let resource = FluentResource::try_new(source.to_string()).unwrap_or_else(|(_, errors)| {
        panic!("invalid built-in Fluent resource for {tag}: {errors:?}")
    });
    let mut bundle = FluentBundle::new_concurrent(vec![language]);
    // Smudgy renders terminal-style UI text where invisible bidi isolation
    // marks would leak into copy/paste and cursor offsets. The supported
    // catalogs are both left-to-right, so interpolation can stay literal.
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("duplicate Fluent messages in {tag}: {errors:?}"));
    bundle
}

/// Activate a persisted preference before the first application window renders.
///
/// `SMUDGY_LOCALE` is an optional launch override. For `System`, the host's
/// ordered UI language preferences are consulted. On Unix, this considers
/// `LANGUAGE` before the POSIX `LC_*` variables; Windows and Apple platforms
/// use their native locale APIs.
pub fn activate(preference: LocalePreference) -> Locale {
    let locale = std::env::var("SMUDGY_LOCALE")
        .ok()
        .and_then(|value| locale_from_tag(&value))
        .unwrap_or_else(|| match preference {
            LocalePreference::System => detect_system_locale(),
            LocalePreference::English => Locale::English,
            LocalePreference::TraditionalChinese => Locale::TraditionalChinese,
        });
    ACTIVE_LOCALE.store(locale as u8, Ordering::Release);
    locale
}

/// The resolved concrete locale currently in use.
#[must_use]
pub fn current_locale() -> Locale {
    match ACTIVE_LOCALE.load(Ordering::Acquire) {
        1 => Locale::TraditionalChinese,
        _ => Locale::English,
    }
}

/// Resolve supported desktop language variables from the current process.
#[must_use]
pub fn detect_system_locale() -> Locale {
    sys_locale::get_locales()
        .find_map(|tag| locale_from_tag(&tag))
        .unwrap_or(Locale::English)
}

/// Resolve a supported locale tag. Unsupported tags return `None`, allowing a
/// later item in a desktop fallback list to be considered.
#[must_use]
pub fn locale_from_tag(tag: &str) -> Option<Locale> {
    let normalized = tag
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-")
        .to_ascii_lowercase();

    if normalized == "en" || normalized.starts_with("en-") {
        return Some(Locale::English);
    }
    if matches!(normalized.as_str(), "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant")
        || normalized.starts_with("zh-hant-")
    {
        return Some(Locale::TraditionalChinese);
    }
    None
}

/// Translate a message without variables, with English fallback.
#[must_use]
pub fn translate(id: &str) -> String {
    translate_with(id, &[])
}

/// Translate a message with string variables, with English fallback.
#[must_use]
pub fn translate_with(id: &str, values: &[(&str, String)]) -> String {
    let mut args = FluentArgs::new();
    for (name, value) in values {
        args.set(*name, value.as_str());
    }

    let primary = match current_locale() {
        Locale::English => &*EN_US,
        Locale::TraditionalChinese => &*ZH_TW,
    };
    format_message(primary, id, &args)
        .or_else(|| format_message(&EN_US, id, &args))
        .unwrap_or_else(|| format!("⟦{id}⟧"))
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

/// Build a translated message while keeping call sites compact and variable
/// names explicit in the catalog.
#[macro_export]
macro_rules! t {
    ($id:literal $(,)?) => {
        $crate::translate($id)
    };
    ($id:literal, $($name:literal => $value:expr),+ $(,)?) => {{
        let values = [$(($name, ($value).to_string())),+];
        $crate::translate_with($id, &values)
    }};
}

#[cfg(test)]
fn catalog_ids(source: &str) -> std::collections::BTreeSet<&str> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if line.starts_with(char::is_whitespace) || line.starts_with('#') {
                return None;
            }
            line.split_once(" = ").map(|(id, _)| id)
        })
        .collect()
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
            && let Some((id, value)) = line.split_once(" = ")
        {
            current = Some(id.to_string());
            messages.insert(id.to_string(), value.to_string());
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
            let variables: BTreeSet<String> = variable
                .captures_iter(&value)
                .map(|capture| capture[1].to_string())
                .collect();
            (id, variables)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCALE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn locale_tags_distinguish_traditional_and_simplified_chinese() {
        assert_eq!(
            locale_from_tag("zh_TW.UTF-8"),
            Some(Locale::TraditionalChinese)
        );
        assert_eq!(
            locale_from_tag("zh-Hant-HK"),
            Some(Locale::TraditionalChinese)
        );
        assert_eq!(locale_from_tag("en_US.UTF-8"), Some(Locale::English));
        assert_eq!(locale_from_tag("zh_CN.UTF-8"), None);
        assert_eq!(locale_from_tag("C.UTF-8"), None);
    }

    #[test]
    fn traditional_chinese_catalog_matches_english_keys() {
        assert_eq!(catalog_ids(EN_US_SOURCE), catalog_ids(ZH_TW_SOURCE));
    }

    #[test]
    fn traditional_chinese_catalog_matches_english_variables() {
        assert_eq!(
            catalog_variables(EN_US_SOURCE),
            catalog_variables(ZH_TW_SOURCE)
        );
    }

    #[test]
    fn interpolation_and_locale_switching_work() {
        let _guard = LOCALE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        activate(LocalePreference::English);
        assert_eq!(t!("welcome-name", "name" => "Mira"), "Welcome, Mira.");
        activate(LocalePreference::TraditionalChinese);
        assert_eq!(t!("welcome-name", "name" => "Mira"), "歡迎，Mira。");
    }

    #[test]
    fn unknown_message_is_visible_instead_of_silently_blank() {
        let _guard = LOCALE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        activate(LocalePreference::English);
        assert_eq!(translate("not-a-real-message"), "⟦not-a-real-message⟧");
    }
}
