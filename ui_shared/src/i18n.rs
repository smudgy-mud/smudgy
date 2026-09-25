//! Stateless English fallback used by portable widgets.

#[must_use]
pub fn translate(id: &str) -> String {
    match id {
        "link-tooltip-loading" => "Loading…",
        _ => id,
    }
    .to_owned()
}

#[must_use]
pub fn translate_static(id: &'static str) -> &'static str {
    match id {
        "terminal-search-placeholder" => "Search scrollback",
        _ => id,
    }
}

#[allow(unused_macros)]
macro_rules! ts {
    ($id:literal $(,)?) => {
        $crate::i18n::translate_static($id)
    };
}
#[allow(unused_imports)]
pub(crate) use ts;
