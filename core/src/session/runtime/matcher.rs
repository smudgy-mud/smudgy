//! Tiered multi-pattern matcher for trigger/alias pattern sets.
//!
//! Replaces `regex::RegexSet`, which degrades to ~1ms/line once a profile
//! carries thousands of unanchored patterns (the union lazy DFA thrashes its
//! cache and falls back to the `PikeVM`). Patterns are classified once at build
//! time and routed to the cheapest engine that can match them:
//!
//! - **Pure literals** (including regex-escaped ones — the dominant case for
//!   item-name substitutions) are found by name: a hit is the match.
//! - **Everything else** goes to [`regex_filtered::Regexes`], a port of RE2's
//!   `FilteredRE2`: a prefilter over each pattern's required literal atoms, with
//!   the full regex run only for candidate patterns.
//!
//! Both populations are found in **one** Aho-Corasick pass. The literals and the
//! filtered set's atoms go into a single automaton whose pattern ids put the
//! literals first: a hit below `literal_count` is a literal, and the rest are atom
//! ids handed to `regex-filtered` to resolve into candidates. Scanning each line
//! once for both is worth more than either tier's own automaton, because a sweep
//! costs what the line is long whatever the pattern count.
//! - Patterns `regex-filtered` cannot handle (none known in practice) fall
//!   back to individually-compiled regexes checked on every line.
//!
//! On the `bench/` corpus (6,305 literals over a 16MB log) this is several
//! thousand times faster than `RegexSet`. See `bench/benches/trigger_matching.rs`,
//! which mirrors this tiering, for the comparison numbers.

use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};
use anyhow::{Context, Result};
use regex::Regex;
use regex_syntax::hir::HirKind;

/// One matching pattern; pure literals also retain their earliest byte span.
#[derive(Debug, PartialEq, Eq)]
pub struct PatternMatch {
    pub index: usize,
    pub literal: Option<Range<usize>>,
}

/// A compiled set of patterns answering one question per line: *which
/// patterns match anywhere in this haystack?*
///
/// Pattern indices in [`Self::matches`] are positions in the
/// original pattern list passed to [`Self::build`], ascending and
/// deduplicated — the same contract as `RegexSet::matches`.
pub struct PatternSet {
    /// Original pattern strings, for diagnostics.
    patterns: Vec<String>,
    /// Empty regexes match every subject without inspecting its bytes. Keep
    /// their original indices out of the text engines so a color-only trigger
    /// can proceed directly to its O(spans) style scan.
    always_match_indices: Vec<usize>,
    /// One automaton over the pure literals followed by the filtered set's required
    /// atoms. Empty when the set has neither.
    scan: AhoCorasick,
    /// How many of `scan`'s patterns are literals: ids below this are literal patterns,
    /// and ids at or above it are `regex-filtered` atom ids offset by it.
    literal_count: usize,
    /// The literal strings, in `scan`'s pattern-id order. `scan` is ASCII
    /// case-insensitive because that is what an atom prefilter requires, so a literal
    /// hit is confirmed against these bytes before it counts as a match.
    literal_strings: Vec<Box<str>>,
    /// `scan` pattern id → index in `patterns`.
    literal_indices: Vec<usize>,
    /// The atom ids found on the current line, reused between lines.
    atom_scratch: std::cell::RefCell<Vec<usize>>,
    /// Non-literal patterns, prefiltered by required literal atoms.
    filtered: regex_filtered::Regexes,
    /// `regex-filtered` pattern id → index in `patterns`.
    filtered_indices: Vec<usize>,
    /// Patterns `regex-filtered` rejected; always checked individually.
    unfiltered: Vec<(usize, Regex)>,
}

impl PatternSet {
    /// Builds an empty set that matches nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self::build(std::iter::empty::<&str>()).expect("an empty PatternSet always builds")
    }

    /// Classifies and compiles `patterns` into the tiered matcher.
    ///
    /// # Errors
    ///
    /// Returns an error if a pattern is not valid regex syntax or an engine
    /// rejects the compiled set (e.g. a size limit).
    pub fn build<I, S>(patterns: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let patterns: Vec<String> = patterns
            .into_iter()
            .map(|p| p.as_ref().to_owned())
            .collect();

        let mut always_match_indices = Vec::new();
        let mut literal_strings = Vec::new();
        let mut literal_indices = Vec::new();
        let mut filter_candidates = Vec::new();
        let mut unfiltered = Vec::new();
        for (idx, pattern) in patterns.iter().enumerate() {
            // Empty regexes are used by color-only triggers. Neither an empty
            // Aho-Corasick automaton nor an empty regex-filtered set is free to
            // run over a long haystack, so keep this O(1) candidate separate
            // from every text engine. The trigger's style qualifier then
            // performs its O(spans) scan.
            if pattern.is_empty() {
                always_match_indices.push(idx);
                continue;
            }
            if let Some(literal) = as_literal(pattern) {
                literal_strings.push(literal);
                literal_indices.push(idx);
            } else {
                filter_candidates.push(idx);
            }
        }

        // `regex-filtered` parses with its own parser; in the unlikely event
        // it rejects a pattern the regex crate accepts, demote that pattern
        // to an individually-checked regex rather than failing the set.
        let (filtered, filtered_indices) = loop {
            let mut builder = Some(regex_filtered::Builder::new_atom_len(2));
            let mut rejected = None;
            for (pos, &idx) in filter_candidates.iter().enumerate() {
                if let Ok(b) = builder.take().unwrap().push(&patterns[idx]) {
                    builder = Some(b);
                } else {
                    rejected = Some(pos);
                    break;
                }
            }
            if let Some(pos) = rejected {
                let idx = filter_candidates.remove(pos);
                let regex = Regex::new(&patterns[idx])
                    .with_context(|| format!("invalid pattern: {}", patterns[idx]))?;
                unfiltered.push((idx, regex));
            } else {
                break (
                    builder
                        .take()
                        .unwrap()
                        .build()
                        .context("failed to build prefiltered regex set")?,
                    filter_candidates,
                );
            }
        };

        // One automaton for both populations, literals first so a hit's id says which it
        // is. `MatchKind::Standard` is what `find_overlapping_iter` requires, and the
        // ASCII case-insensitivity is what `regex-filtered` builds its own prefilter with
        // — an atom rules a pattern in, and the regex behind it decides the case. A
        // literal has no regex behind it, so `matches_into` confirms its bytes instead.
        let literal_count = literal_strings.len();
        let scan = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .ascii_case_insensitive(true)
            .prefilter(true)
            .build(
                literal_strings
                    .iter()
                    .map(String::as_str)
                    .chain(filtered.atoms().iter().map(String::as_str)),
            )
            .context("failed to build the pattern scanner")?;

        Ok(Self {
            patterns,
            always_match_indices,
            scan,
            literal_count,
            literal_strings: literal_strings
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
            literal_indices,
            filtered,
            filtered_indices,
            unfiltered,
            atom_scratch: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// Return each matching pattern once, in original pattern order. Repeated
    /// literal occurrences retain the earliest start, matching `Regex::captures`.
    #[cfg(test)]
    #[must_use]
    pub fn matches(&self, haystack: &str) -> Vec<PatternMatch> {
        let mut out = Vec::new();
        self.matches_into(haystack, &mut out);
        out
    }

    /// [`Self::matches`] into a vector that the caller owns, so a per-line caller reuses one
    /// allocation from line to line. The function clears `out` first. On return, `out` holds
    /// exactly what `matches` returns.
    pub fn matches_into(&self, haystack: &str, out: &mut Vec<PatternMatch>) {
        out.clear();
        out.extend(self.always_match_indices.iter().map(|&index| PatternMatch {
            index,
            literal: None,
        }));
        // The one pass. Literal hits are matches, once their case is confirmed; atom hits
        // are candidates, resolved below by the set they belong to.
        let mut atoms = self.atom_scratch.borrow_mut();
        atoms.clear();
        for hit in self.scan.find_overlapping_iter(haystack) {
            let id = hit.pattern().as_usize();
            if let Some(atom) = id.checked_sub(self.literal_count) {
                atoms.push(atom);
            } else if haystack.as_bytes()[hit.start()..hit.end()]
                == *self.literal_strings[id].as_bytes()
            {
                out.push(PatternMatch {
                    index: self.literal_indices[id],
                    literal: Some(hit.start()..hit.end()),
                });
            }
        }
        if !self.filtered_indices.is_empty() {
            out.extend(
                self.filtered
                    .matching_of(haystack, atoms.drain(..))
                    .map(|(id, _)| PatternMatch {
                        index: self.filtered_indices[id],
                        literal: None,
                    }),
            );
        }
        drop(atoms);
        out.extend(
            self.unfiltered
                .iter()
                .filter(|(_, regex)| regex.is_match(haystack))
                .map(|&(index, _)| PatternMatch {
                    index,
                    literal: None,
                }),
        );
        out.sort_unstable_by_key(|hit| {
            (hit.index, hit.literal.as_ref().map_or(0, |span| span.start))
        });
        out.dedup_by_key(|hit| hit.index);
    }

    #[cfg(test)]
    fn matched_indices(&self, haystack: &str) -> Vec<usize> {
        self.matches(haystack)
            .into_iter()
            .map(|hit| hit.index)
            .collect()
    }

    /// The original pattern strings, indexed as in [`Self::matches`].
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

impl std::fmt::Debug for PatternSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatternSet")
            .field("always", &self.always_match_indices.len())
            .field("literals", &self.literal_indices.len())
            .field("filtered", &self.filtered_indices.len())
            .field("unfiltered", &self.unfiltered.len())
            .finish_non_exhaustive()
    }
}

/// If `pattern` matches exactly one literal string (e.g. `A shiny ring`, or
/// any regex-escaped text), returns that string.
fn as_literal(pattern: &str) -> Option<String> {
    let hir = regex_syntax::parse(pattern).ok()?;
    if let HirKind::Literal(literal) = hir.kind() {
        std::str::from_utf8(&literal.0).ok().map(ToOwned::to_owned)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_metacharacters_classify_as_literals() {
        assert_eq!(
            as_literal(&regex::escape("a (cool) ring +1")).as_deref(),
            Some("a (cool) ring +1")
        );
        assert_eq!(as_literal(r"^You (\w+)"), None);
        assert_eq!(as_literal("(?i)dargaroth"), None);
    }

    #[test]
    fn matches_literals_and_regexes_with_regexset_contract() {
        let set = PatternSet::build([
            regex::escape("A shiny ring").as_str(),
            r"^(\w+) tells you '(.+)'",
            "Dargaroth",
        ])
        .unwrap();

        assert_eq!(
            set.matched_indices("You see A shiny ring here. Dargaroth grins."),
            vec![0, 2]
        );
        assert_eq!(set.matched_indices("Bob tells you 'flee!'"), vec![1]);
        assert!(set.matched_indices("nothing of note").is_empty());
    }

    #[test]
    fn repeated_hits_are_deduplicated_and_sorted() {
        let set = PatternSet::build(["ring", "shiny", r"shiny (\w+)"]).unwrap();
        assert_eq!(
            set.matched_indices("a shiny ring and a shiny ring"),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn overlapping_literals_all_match() {
        let set = PatternSet::build(["a shiny ring", "shiny ring of power", "ring"]).unwrap();
        assert_eq!(
            set.matched_indices("you wear a shiny ring of power"),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn empty_regex_bypasses_every_text_engine() {
        let set = PatternSet::build([""]).unwrap();
        assert_eq!(set.always_match_indices, [0]);
        assert!(set.literal_indices.is_empty());
        assert!(set.filtered_indices.is_empty());
        assert!(set.unfiltered.is_empty());
        assert_eq!(set.matched_indices(&"x".repeat(100_000)), vec![0]);
    }

    #[test]
    fn mixed_always_literal_and_regex_matches_keep_original_index_order() {
        let set = PatternSet::build([
            "",
            "needle",
            r"n\w+",
            "",
            "needle",
            r"^needle$",
            r"n\w+",
            r"(?:)",
        ])
        .unwrap();
        assert_eq!(set.always_match_indices, [0, 3]);

        // Repeated matches of one pattern collapse to one index, while
        // duplicate patterns remain distinct original entries across tiers.
        assert_eq!(
            set.matched_indices("needle needle"),
            vec![0, 1, 2, 3, 4, 6, 7]
        );
        assert_eq!(set.matched_indices("needle"), (0..=7).collect::<Vec<_>>());
        assert_eq!(set.matched_indices("other"), vec![0, 3, 7]);
        assert_eq!(set.matched_indices(""), vec![0, 3, 7]);
    }

    #[test]
    fn literals_stay_case_sensitive_in_the_shared_scanner() {
        // The one automaton is ASCII case-insensitive, because that is what the atom
        // prefilter it also serves requires. A literal has no regex behind it to settle
        // the case, so `matches_into` confirms the bytes; without that, every literal
        // trigger would silently become case-insensitive.
        let set = PatternSet::build(["Dargaroth", "café", r"^A glowing (\w+)"]).unwrap();
        assert_eq!(set.matched_indices("Dargaroth grins."), vec![0]);
        assert!(set.matched_indices("dargaroth grins.").is_empty());
        assert!(set.matched_indices("DARGAROTH grins.").is_empty());
        // Non-ASCII is untouched by ASCII case folding, and the span stays on a
        // character boundary.
        assert_eq!(
            set.matches("a café here"),
            vec![PatternMatch {
                index: 1,
                literal: Some(2..7)
            }]
        );
        assert!(set.matched_indices("a CAFÉ here").is_empty());
        // The regex tier is unaffected: its atoms rule patterns in, and the regex decides.
        assert_eq!(set.matched_indices("A glowing rune"), vec![2]);
        assert!(set.matched_indices("a glowing rune").is_empty());
    }

    #[test]
    fn case_insensitive_patterns_route_to_regex_tier() {
        let set = PatternSet::build(["(?i)dargaroth"]).unwrap();
        assert_eq!(set.matched_indices("DARGAROTH snarls."), vec![0]);
        assert_eq!(set.matched_indices("dargaroth snarls."), vec![0]);
    }

    #[test]
    fn literal_spans_preserve_duplicates_overlaps_and_utf8_offsets() {
        let set = PatternSet::build(["aba", "ba", "aba", "(?<word>aba)", "^aba"]).unwrap();
        assert_eq!(
            set.matches("éababa"),
            vec![
                PatternMatch {
                    index: 0,
                    literal: Some(2..5)
                },
                PatternMatch {
                    index: 1,
                    literal: Some(3..5)
                },
                PatternMatch {
                    index: 2,
                    literal: Some(2..5)
                },
                PatternMatch {
                    index: 3,
                    literal: None
                },
            ]
        );
    }

    #[test]
    fn empty_set_matches_nothing() {
        let set = PatternSet::empty();
        assert!(set.matched_indices("anything at all").is_empty());
    }

    #[test]
    fn empty_pattern_matches_every_line() {
        let set = PatternSet::build([""]).unwrap();
        assert_eq!(set.matched_indices("anything"), vec![0]);
    }
}
