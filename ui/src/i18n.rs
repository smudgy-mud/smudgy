//! Application-owned locale policy and active translator.
//!
//! `smudgy_i18n` owns immutable catalogs and formatting. This module owns the
//! selected preference, platform detection, launch override, and the active
//! translator used by Smudgy's windows.

use std::fmt;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::{LazyLock, RwLock};

use smudgy_i18n::{FluentArgs, Translator};

const SYSTEM_PREFERENCE: &str = "system";

static ACTIVE_TRANSLATOR: LazyLock<RwLock<Translator>> =
    LazyLock::new(|| RwLock::new(Translator::default()));
static STATIC_TRANSLATIONS: LazyLock<
    Mutex<HashMap<(&'static str, &'static str), &'static str>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// One application locale picker item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocaleChoice {
    preference: &'static str,
    display_name: &'static str,
    system: bool,
}

impl LocaleChoice {
    const SYSTEM: Self = Self {
        preference: SYSTEM_PREFERENCE,
        display_name: "",
        system: true,
    };

    pub(crate) const fn preference(self) -> &'static str {
        self.preference
    }
}

impl fmt::Display for LocaleChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.system {
            formatter.write_str(&translate("locale-system"))
        } else {
            formatter.write_str(self.display_name)
        }
    }
}

/// Data-driven picker choices: System followed by every catalog manifest
/// entry. A new catalog appears here without adding an enum variant.
pub(crate) fn locale_choices() -> Vec<LocaleChoice> {
    std::iter::once(LocaleChoice::SYSTEM)
        .chain(
            smudgy_i18n::available_catalogs().map(|catalog| LocaleChoice {
                preference: catalog.tag,
                display_name: catalog.display_name,
                system: false,
            }),
        )
        .collect()
}

/// Convert a persisted preference into its picker item. Unsupported or empty
/// legacy values display as System while the resolver still falls back safely.
pub(crate) fn locale_choice(preference: &str) -> LocaleChoice {
    if preference.eq_ignore_ascii_case(SYSTEM_PREFERENCE) || preference.trim().is_empty() {
        return LocaleChoice::SYSTEM;
    }

    let Some(tag) = Translator::for_tag(preference).map(Translator::tag) else {
        return LocaleChoice::SYSTEM;
    };
    locale_choices()
        .into_iter()
        .find(|choice| choice.preference == tag)
        .unwrap_or(LocaleChoice::SYSTEM)
}

/// Resolve and activate an application preference.
///
/// `SMUDGY_LOCALE` is a launch-only override. A System preference consults
/// `sys-locale`, which uses native APIs on Windows and Apple platforms and
/// the ordered locale environment on Unix.
pub(crate) fn activate(preference: &str) -> &'static str {
    let override_tag = std::env::var("SMUDGY_LOCALE").ok();
    let translator = resolve(
        preference,
        override_tag.as_deref(),
        sys_locale::get_locales(),
    );
    let tag = translator.tag();
    *ACTIVE_TRANSLATOR
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = translator;
    tag
}

fn resolve(
    preference: &str,
    override_tag: Option<&str>,
    system_tags: impl IntoIterator<Item = String>,
) -> Translator {
    if let Some(translator) = override_tag.and_then(Translator::for_tag) {
        return translator;
    }
    if preference.eq_ignore_ascii_case(SYSTEM_PREFERENCE) || preference.trim().is_empty() {
        return system_tags
            .into_iter()
            .find_map(|tag| Translator::for_tag(&tag))
            .unwrap_or_default();
    }
    Translator::for_tag(preference).unwrap_or_default()
}

pub(crate) fn with_translator<T>(format: impl FnOnce(Translator) -> T) -> T {
    let translator = *ACTIVE_TRANSLATOR
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    format(translator)
}

pub(crate) fn translate(id: &str) -> String {
    with_translator(|translator| translator.translate(id))
}

pub(crate) fn translate_with(id: &str, args: &FluentArgs<'_>) -> String {
    with_translator(|translator| translator.translate_with(id, args))
}

/// Translate a literal for Iced APIs that retain a borrowed label or
/// placeholder. Each `(catalog, message id)` allocation is cached once for the
/// process lifetime; the selected locale itself remains application-owned.
pub(crate) fn translate_static(id: &'static str) -> &'static str {
    with_translator(|translator| {
        let key = (translator.tag(), id);
        let mut cache = STATIC_TRANSLATIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = cache.get(&key) {
            return *value;
        }
        let value = Box::leak(translator.translate(id).into_boxed_str());
        cache.insert(key, value);
        value
    })
}

macro_rules! t {
    ($id:literal $(,)?) => {
        $crate::i18n::with_translator(|translator| smudgy_i18n::t!(translator, $id))
    };
    ($id:literal, $($name:literal => $value:expr),+ $(,)?) => {
        $crate::i18n::with_translator(|translator| {
            smudgy_i18n::t!(translator, $id, $($name => $value),+)
        })
    };
}
pub(crate) use t;

// Placeholders and other Iced APIs retain a borrowed label. Keep this adapter
// in the application layer so the catalog crate stays stateless.
macro_rules! ts {
    ($id:literal $(,)?) => {
        $crate::i18n::translate_static($id)
    };
}
pub(crate) use ts;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_picker_is_derived_from_catalog_manifest() {
        let choices = locale_choices();
        assert_eq!(choices.len(), smudgy_i18n::available_catalogs().len() + 1);
        assert_eq!(choices[0], LocaleChoice::SYSTEM);
        assert_eq!(locale_choice("zh-Hant-TW").preference(), "zh-TW");
    }

    #[test]
    fn resolution_prefers_override_then_platform_order_then_english() {
        assert_eq!(
            resolve("system", Some("zh-TW"), ["en-US".to_string()]).tag(),
            "zh-TW"
        );
        assert_eq!(
            resolve("system", None, ["zh-HK".to_string(), "en-GB".to_string()]).tag(),
            "en-US"
        );
        assert_eq!(
            resolve("not-a-locale", None, std::iter::empty()).tag(),
            "en-US"
        );
    }
}
