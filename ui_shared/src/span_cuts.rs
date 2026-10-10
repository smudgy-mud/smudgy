//! One way to cut rendered spans at byte offsets. Fonts, effects and inline
//! objects all partition the same span list; sharing the walk keeps their piece
//! boundaries, and their handling of an offset inside a character, identical.
use crate::terminal_buffer::SpanMetadata as Link;
use iced::widget::text::Span;
use std::ops::Range;

/// Each span with its index and the rendered byte range it occupies, in order.
pub(crate) fn ranges<'a>(
    spans: &'a [Span<'static, Link>],
) -> impl Iterator<Item = (usize, Range<usize>, &'a Span<'static, Link>)> + 'a {
    let mut position = 0;
    spans.iter().enumerate().map(move |(index, span)| {
        let start = position;
        position += span.text.len();
        (index, start..position, span)
    })
}

/// Calls `piece` once per fragment, in order, with the rendered byte range it
/// covers: every span is cut at each boundary that falls inside it. A boundary
/// inside a character is ignored rather than splitting it, so no consumer can
/// slice a UTF-8 sequence midway.
pub(crate) fn split_at(
    spans: &[Span<'static, Link>],
    boundaries: impl IntoIterator<Item = usize>,
    mut piece: impl FnMut(Range<usize>, Span<'static, Link>),
) {
    let mut boundaries: Vec<usize> = boundaries.into_iter().collect();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut next = 0;
    for (_, range, span) in ranges(spans) {
        while next < boundaries.len() && boundaries[next] <= range.start {
            next += 1;
        }
        let cuts = boundaries[next..]
            .iter()
            .copied()
            .take_while(|boundary| *boundary < range.end)
            .chain(std::iter::once(range.end));
        let mut start = range.start;
        for stop in cuts {
            if !span.text.is_char_boundary(stop - range.start) {
                continue;
            }
            let mut part = span.clone();
            if start != range.start || stop != range.end {
                part.text = span.text[start - range.start..stop - range.start]
                    .to_owned()
                    .into();
            }
            piece(start..stop, part);
            start = stop;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(spans: &[Span<'static, Link>], boundaries: &[usize]) -> Vec<(Range<usize>, String)> {
        let mut out = Vec::new();
        split_at(spans, boundaries.iter().copied(), |range, part| {
            out.push((range, part.text.to_string()));
        });
        out
    }

    #[test]
    fn cuts_fall_inside_spans_and_skip_mid_character_offsets() {
        let spans = vec![Span::new("abé"), Span::new(""), Span::new("cd")];
        // 3 is inside "é" (bytes 2..4); 0, 4 and 6 are span edges.
        assert_eq!(
            texts(&spans, &[0, 1, 3, 4, 5, 6, 9]),
            vec![
                (0..1, "a".into()),
                (1..4, "bé".into()),
                (4..4, String::new()),
                (4..5, "c".into()),
                (5..6, "d".into()),
            ]
        );
        assert_eq!(texts(&spans, &[]).len(), 3);
    }

    #[test]
    fn ranges_tile_the_rendered_text() {
        let spans = vec![Span::new("ab"), Span::new("cde")];
        let ranges: Vec<_> = ranges(&spans).map(|(i, r, _)| (i, r)).collect();
        assert_eq!(ranges, vec![(0, 0..2), (1, 2..5)]);
    }
}
