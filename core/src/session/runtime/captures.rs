//! Immutable capture values shared by automation dispatch and its three consumers.
//!
//! Incoming matches retain their source line and byte ranges. Parsed commands and
//! interop deliveries retain their existing owned values; they need not be substrings.

use std::{ops::Range, sync::Arc};

use crate::session::styled_line::StyledLine;

use super::trigger::MatchCapture;

/// Queued captures keep the selected pattern's values alive across later line edits
/// and automation replacement. This handle never contains isolate-bound V8 values.
#[derive(Clone, Debug)]
pub enum CapturePayload {
    Owned(Arc<Vec<MatchCapture>>),
    Ranged(Arc<RangedCaptures>),
}

impl CapturePayload {
    pub fn view(&self) -> CaptureView<'_> {
        match self {
            Self::Owned(values) => CaptureView::Owned(values),
            Self::Ranged(values) => CaptureView::Ranged(values),
        }
    }

    #[cfg(test)]
    pub fn get(&self, index: usize) -> Option<CaptureRef<'_>> {
        self.view().get(index)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.view().len()
    }

    #[cfg(test)]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = CaptureRef<'_>> {
        self.view().iter()
    }
}

impl From<Arc<Vec<MatchCapture>>> for CapturePayload {
    fn from(values: Arc<Vec<MatchCapture>>) -> Self {
        Self::Owned(values)
    }
}

/// Names are compiled once and remain attached to a queued match's exact pattern.
#[derive(Debug)]
struct CaptureSchema {
    names: Box<[Option<Box<str>>]>,
}

/// Most literal, prefix, and color-only matches have only slot zero. Store it
/// inline; larger capture sets use one exact-size allocation without inflating
/// every queued match with a multi-group inline buffer.
#[derive(Debug)]
enum CaptureRanges {
    Whole(Range<usize>),
    Groups(Box<[Option<Range<usize>>]>),
}

impl CaptureRanges {
    fn len(&self) -> usize {
        match self {
            Self::Whole(_) => 1,
            Self::Groups(groups) => groups.len(),
        }
    }

    // Outer None means an absent group; inner None means it did not participate.
    #[allow(clippy::option_option)]
    fn get(&self, index: usize) -> Option<Option<Range<usize>>> {
        match self {
            Self::Whole(range) => (index == 0).then(|| Some(range.clone())),
            Self::Groups(groups) => groups.get(index).cloned(),
        }
    }

    fn from_locations(locations: &regex::CaptureLocations) -> Self {
        if locations.len() == 1 {
            let (start, end) = locations.get(0).expect("whole match participated");
            Self::Whole(start..end)
        } else {
            Self::Groups(
                (0..locations.len())
                    .map(|index| locations.get(index).map(|(start, end)| start..end))
                    .collect(),
            )
        }
    }
}

#[derive(Debug)]
pub struct RangedCaptures {
    line: Arc<StyledLine>,
    raw: bool,
    /// No schema allocation is needed for a pattern with only its whole match.
    schema: Option<Arc<CaptureSchema>>,
    ranges: CaptureRanges,
}

/// A borrowed reader lets events keep their existing owned action payloads while
/// automations use ranges. Consumers do not need to materialize Rust strings.
#[derive(Clone, Copy)]
pub enum CaptureView<'a> {
    Owned(&'a [MatchCapture]),
    Ranged(&'a RangedCaptures),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureRef<'a> {
    pub name: Option<&'a str>,
    pub value: &'a str,
}

impl<'a> CaptureView<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Owned(values) => values.len(),
            Self::Ranged(values) => values.ranges.len(),
        }
    }

    pub fn get(self, index: usize) -> Option<CaptureRef<'a>> {
        match self {
            Self::Owned(values) => values.get(index).map(|capture| CaptureRef {
                name: capture.name.as_deref(),
                value: &capture.value,
            }),
            Self::Ranged(values) => {
                let range = values.ranges.get(index)?;
                let text = if values.raw {
                    values
                        .line
                        .raw()
                        .expect("raw captures retain their raw source")
                } else {
                    &values.line.text
                };
                Some(CaptureRef {
                    name: values
                        .schema
                        .as_ref()
                        .and_then(|schema| schema.names[index].as_deref()),
                    value: range.as_ref().map_or("", |range| &text[range.clone()]),
                })
            }
        }
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = CaptureRef<'a>> {
        (0..self.len()).map(move |index| self.get(index).expect("in-bounds capture"))
    }
}

impl<'a> From<&'a Arc<Vec<MatchCapture>>> for CaptureView<'a> {
    fn from(values: &'a Arc<Vec<MatchCapture>>) -> Self {
        Self::Owned(values)
    }
}

/// A regex and the immutable schema initialized on its first capture-bearing hit.
/// Keeping them together prevents a replacement from reusing an older schema.
#[derive(Debug)]
pub struct CapturePattern {
    regex: regex::Regex,
    schema: std::cell::OnceCell<Arc<CaptureSchema>>,
    locations: std::cell::RefCell<Option<Box<regex::CaptureLocations>>>,
}

impl CapturePattern {
    pub fn new(regex: regex::Regex) -> Self {
        Self {
            regex,
            schema: std::cell::OnceCell::new(),
            locations: std::cell::RefCell::new(None),
        }
    }

    pub fn capture_line(
        &self,
        line: &Arc<StyledLine>,
        raw: bool,
        start: Option<usize>,
        literal_range: Option<Range<usize>>,
    ) -> CapturePayload {
        if let Some(range) = literal_range {
            debug_assert_eq!(self.regex.captures_len(), 1);
            return CapturePayload::Ranged(Arc::new(RangedCaptures {
                line: line.clone(),
                raw,
                schema: None,
                ranges: CaptureRanges::Whole(range),
            }));
        }

        let text = if raw {
            line.raw().expect("selected raw source")
        } else {
            &line.text
        };
        // Without parenthesized groups the ordinary match already contains all
        // capture data. This also covers anchored prefixes and color-only regexes.
        if self.regex.captures_len() == 1 {
            let whole = self
                .regex
                .find_at(text, start.unwrap_or(0))
                .expect("a selected trigger match must still capture");
            assert!(
                start.is_none_or(|start| whole.start() == start),
                "selected occurrence changed"
            );
            return CapturePayload::Ranged(Arc::new(RangedCaptures {
                line: line.clone(),
                raw,
                schema: None,
                ranges: CaptureRanges::Whole(whole.range()),
            }));
        }
        let mut scratch = self.locations.borrow_mut();
        let locations = scratch.get_or_insert_with(|| Box::new(self.regex.capture_locations()));
        let whole = self
            .regex
            .captures_read_at(locations, text, start.unwrap_or(0))
            .expect("a selected trigger match must still capture");
        assert!(
            start.is_none_or(|start| whole.start() == start),
            "selected occurrence changed"
        );
        let schema = (locations.len() > 1).then(|| {
            self.schema
                .get_or_init(|| {
                    Arc::new(CaptureSchema {
                        names: self
                            .regex
                            .capture_names()
                            .map(|name| name.map(Into::into))
                            .collect(),
                    })
                })
                .clone()
        });
        CapturePayload::Ranged(Arc::new(RangedCaptures {
            line: line.clone(),
            raw,
            schema,
            ranges: CaptureRanges::from_locations(locations),
        }))
    }
}

impl std::ops::Deref for CapturePattern {
    type Target = regex::Regex;
    fn deref(&self) -> &Self::Target {
        &self.regex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_matches_share_long_sources_without_capture_metadata() {
        let text = format!("{}needle", "é".repeat(500_000));
        let line = Arc::new(StyledLine::new(&text, Vec::new()));
        let pattern = CapturePattern::new(regex::Regex::new("needle").unwrap());
        for literal in [None, Some(1_000_000..1_000_006)] {
            let captures = pattern.capture_line(&line, false, None, literal);
            assert_eq!(captures.get(0).unwrap().value, "needle");
            let CapturePayload::Ranged(ranged) = captures else {
                panic!("incoming match must borrow its source");
            };
            assert!(Arc::ptr_eq(&ranged.line, &line));
            assert!(ranged.schema.is_none());
        }
        assert!(pattern.schema.get().is_none());
        assert!(pattern.locations.borrow().is_none());
        assert!(std::mem::size_of::<CaptureRanges>() <= std::mem::size_of::<Vec<usize>>());
    }

    #[test]
    fn ranges_keep_utf8_source_and_selected_schema_alive() {
        let line = Arc::new(StyledLine::new("éé value", Vec::new()));
        let schema = Arc::new(CaptureSchema {
            names: vec![None, Some("word".into()), Some("optional".into()), None].into(),
        });
        let captures = CapturePayload::Ranged(Arc::new(RangedCaptures {
            line: line.clone(),
            raw: false,
            schema: Some(schema.clone()),
            ranges: CaptureRanges::Groups(vec![Some(0..10), Some(0..4), None, Some(4..4)].into()),
        }));
        drop(line);
        drop(schema);
        assert_eq!(
            captures.get(1),
            Some(CaptureRef {
                name: Some("word"),
                value: "éé"
            })
        );
        assert_eq!(
            captures.get(2),
            Some(CaptureRef {
                name: Some("optional"),
                value: ""
            })
        );
        assert_eq!(captures.get(3).unwrap().value, "");
        assert_eq!(captures.get(4), None);
        assert_eq!(captures.iter().count(), 4);
    }

    #[test]
    fn successive_matches_keep_optional_groups_and_old_schema() {
        let pattern = CapturePattern::new(regex::Regex::new(r"(?<word>é+)(?<tail>x)?").unwrap());
        let first_line = Arc::new(StyledLine::new("ééx", Vec::new()));
        let retained = Arc::downgrade(&first_line);
        let first = pattern.capture_line(&first_line, false, None, None);
        let second = pattern.capture_line(
            &Arc::new(StyledLine::new("é", Vec::new())),
            false,
            None,
            None,
        );
        drop(first_line);
        drop(pattern);
        assert_eq!(first.get(1).unwrap().value, "éé");
        assert_eq!(first.get(2).unwrap().value, "x");
        assert_eq!(
            second.get(2),
            Some(CaptureRef {
                name: Some("tail"),
                value: ""
            })
        );
        assert!(second.get(3).is_none());
        assert!(retained.upgrade().is_some());
        drop(first);
        assert!(
            retained.upgrade().is_none(),
            "scratch and schemas must not retain the input"
        );
        assert!(std::mem::size_of::<CapturePayload>() <= 2 * std::mem::size_of::<usize>());
    }

    #[test]
    fn raw_offsets_and_capture_at_keep_the_original_haystack() {
        let raw = "\x1b[31méé\x1b[0m";
        let line = Arc::new(StyledLine::new_with_raw(
            "éé",
            Vec::new(),
            Some(raw.as_bytes()),
        ));
        let pattern =
            CapturePattern::new(regex::Regex::new(r"(?<escape>\x1b\[\d+m)(?<word>é+)").unwrap());
        let captures = pattern.capture_line(&line, true, None, None);
        assert_eq!(captures.get(1).unwrap().value, "\x1b[31m");
        assert_eq!(captures.get(2).unwrap().value, "éé");

        let pattern =
            CapturePattern::new(regex::Regex::new(r"(?<anchored>^word)|(?<word>word)").unwrap());
        let line = Arc::new(StyledLine::new("xword word", Vec::new()));
        for start in [1, 6] {
            let captures = pattern.capture_line(&line, false, Some(start), None);
            assert_eq!(
                captures.get(1).unwrap().value,
                "",
                "capture_at must not slice away the anchor context"
            );
            assert_eq!(captures.get(2).unwrap().value, "word");
        }
    }
}
