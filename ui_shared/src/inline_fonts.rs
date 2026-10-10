//! Sparse font choices for selectable terminal text.
use crate::terminal_buffer::{RenderedOffsets, SpanMetadata as Link};
use iced::{
    Pixels, font,
    widget::text::{LineHeight, Span},
};
use smudgy_session_model::inline_content::{InlineFont, InlineFontStyle};
use std::{
    collections::HashSet,
    rc::Rc,
    sync::{LazyLock, Mutex},
};

/// The syntactic contract for a `fontFace` name. Script-side validation stops
/// here: a Span that is built but never shown must not claim a family slot.
pub fn validate(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("Span fontFace must be a font family name of 1..128 bytes");
    }
    Ok(())
}

/// iced font handles need static family names. Bound both count and name length,
/// including across isolate reloads, so script-authored names cannot leak memory.
/// Interning happens only here, when a line carrying the face is rendered.
pub fn family(name: &str) -> Result<font::Family, &'static str> {
    match name {
        "monospace" => return Ok(font::Family::Monospace),
        "sans-serif" => return Ok(font::Family::SansSerif),
        "serif" => return Ok(font::Family::Serif),
        _ => {}
    }
    validate(name)?;
    static NAMES: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Mutex::default);
    let mut names = NAMES.lock().unwrap();
    if let Some(name) = names.get(name) {
        return Ok(font::Family::Name(name));
    }
    if names.len() == 128 {
        return Err("at most 128 distinct Span font families may be used per application run");
    }
    let name = Box::leak(name.to_owned().into_boxed_str());
    names.insert(name);
    Ok(font::Family::Name(name))
}

pub(crate) fn apply(
    spans: &Rc<Vec<Span<'static, Link>>>,
    offsets: &RenderedOffsets,
    fonts: &[InlineFont],
    base_size: f32,
    line_height: f32,
) -> Rc<Vec<Span<'static, Link>>> {
    let regions: Vec<_> = fonts
        .iter()
        .filter_map(|font| {
            let start = offsets.source_to_rendered(font.range.start);
            let end = offsets.source_to_rendered(font.range.end);
            (start < end).then_some((start..end, &font.options))
        })
        .collect();
    let boundaries = regions
        .iter()
        .flat_map(|(range, _)| [range.start, range.end]);
    let mut result = Vec::with_capacity(spans.len() + regions.len() * 2);
    let mut region_index = 0;
    crate::span_cuts::split_at(spans, boundaries, |piece, mut part| {
        while region_index < regions.len() && regions[region_index].0.end <= piece.start {
            region_index += 1;
        }
        if let Some((_, options)) = regions
            .get(region_index)
            .filter(|(range, _)| range.start <= piece.start)
        {
            let mut font = part.font.unwrap_or(crate::prefs::current().font);
            if let Some(face) = &options.font_face {
                font.family = family(face).unwrap_or(font.family);
            }
            if let Some(weight) = options.font_weight {
                font.weight = match weight {
                    100 => font::Weight::Thin,
                    200 => font::Weight::ExtraLight,
                    300 => font::Weight::Light,
                    500 => font::Weight::Medium,
                    600 => font::Weight::Semibold,
                    700 => font::Weight::Bold,
                    800 => font::Weight::ExtraBold,
                    900 => font::Weight::Black,
                    _ => font::Weight::Normal,
                };
            }
            if let Some(style) = options.font_style {
                font.style = match style {
                    InlineFontStyle::Normal => font::Style::Normal,
                    InlineFontStyle::Italic => font::Style::Italic,
                    InlineFontStyle::Oblique => font::Style::Oblique,
                };
            }
            part.font = Some(font);
            if let Some(size) = options.font_size {
                part.size = Some(Pixels(f32::from(size)));
                part.line_height = Some(LineHeight::Absolute(Pixels(
                    line_height * f32::from(size) / base_size,
                )));
            }
        }
        result.push(part);
    });
    Rc::new(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_checks_the_name_without_claiming_a_family_slot() {
        assert!(validate("Iosevka Term").is_ok());
        assert!(validate("").is_err());
        assert!(validate("bad\u{7}name").is_err());
        assert!(validate(&"x".repeat(129)).is_err());
        // Every syntactically valid name a script could construct stays free
        // until a rendered line interns it; the budget is for shown faces.
        for n in 0..1000 {
            assert!(validate(&format!("never shown {n}")).is_ok());
        }
        assert!(matches!(
            family("Iosevka Term"),
            Ok(font::Family::Name("Iosevka Term"))
        ));
    }
}
