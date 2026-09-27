use super::CatalogSource;

pub(super) const FALLBACK_TAG: &str = "en-US";

/// Embedded catalog manifest. Picker order is declaration order.
///
/// `aliases` are locale identifiers accepted for the catalog. A language-only
/// alias matches every region of that language; script/region aliases match
/// exactly.
pub(super) const CATALOGS: &[CatalogSource] = &[
    CatalogSource {
        tag: "en-US",
        aliases: &["en"],
        display_name: "English (United States)",
        source: include_str!("../locales/en-US/main.ftl"),
    },
    #[cfg(feature = "all-locales")]
    CatalogSource {
        tag: "pl-PL",
        aliases: &["pl"],
        display_name: "Polski",
        source: include_str!("../locales/pl-PL/main.ftl"),
    },
    #[cfg(feature = "all-locales")]
    CatalogSource {
        tag: "uk-UA",
        aliases: &["uk"],
        display_name: "Українська",
        source: include_str!("../locales/uk-UA/main.ftl"),
    },
    #[cfg(feature = "all-locales")]
    CatalogSource {
        tag: "zh-TW",
        aliases: &["zh-Hant-TW"],
        display_name: "繁體中文（臺灣）",
        source: include_str!("../locales/zh-TW/main.ftl"),
    },
];
