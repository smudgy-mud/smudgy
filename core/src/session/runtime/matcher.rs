//! Tiered multi-pattern matcher for trigger/alias pattern sets.
//!
//! Replaces `regex::RegexSet`, which degrades to ~1ms/line once a profile
//! carries thousands of unanchored patterns (the union lazy DFA thrashes its
//! cache and falls back to the `PikeVM`). Patterns are classified once at build
//! time and routed to the cheapest engine that can match them:
//!
//! - **Pure literals** (including regex-escaped ones — the dominant case for
//!   item-name substitutions) go to a single Aho-Corasick automaton: one
//!   O(line) pass regardless of pattern count.
//! - **Everything else** goes to [`regex_filtered::Regexes`], a port of RE2's
//!   `FilteredRE2`: an Aho-Corasick prefilter over each pattern's required
//!   literal atoms, with the full regex run only for candidate patterns.
//! - Patterns `regex-filtered` cannot handle (none known in practice) fall
//!   back to individually-compiled regexes checked on every line.
//!
//! On the `bench/` corpus (6,305 literals over a 16MB log) this is several
//! thousand times faster than `RegexSet`. See `bench/benches/trigger_matching.rs`,
//! which mirrors this tiering, for the comparison numbers.

use aho_corasick::{AhoCorasick, MatchKind};
use anyhow::{Context, Result};
use regex::Regex;
use regex_syntax::hir::HirKind;

/// A compiled set of patterns answering one question per line: *which
/// patterns match anywhere in this haystack?*
///
/// Indices returned by [`Self::matched_indices`] are positions in the
/// original pattern list passed to [`Self::build`], ascending and
/// deduplicated — the same contract as `RegexSet::matches`.
pub struct PatternSet {
    /// Original pattern strings, for diagnostics.
    patterns: Vec<String>,
    /// Patterns that are pure literals, matched in one Aho-Corasick pass.
    literals: AhoCorasick,
    /// Aho-Corasick pattern id → index in `patterns`.
    literal_indices: Vec<usize>,
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

        let mut literal_strings = Vec::new();
        let mut literal_indices = Vec::new();
        let mut filter_candidates = Vec::new();
        for (idx, pattern) in patterns.iter().enumerate() {
            if let Some(literal) = as_literal(pattern) {
                literal_strings.push(literal);
                literal_indices.push(idx);
            } else {
                filter_candidates.push(idx);
            }
        }

        let literals = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&literal_strings)
            .context("failed to build literal pattern matcher")?;

        // `regex-filtered` parses with its own parser; in the unlikely event
        // it rejects a pattern the regex crate accepts, demote that pattern
        // to an individually-checked regex rather than failing the set.
        let mut unfiltered = Vec::new();
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

        Ok(Self {
            patterns,
            literals,
            literal_indices,
            filtered,
            filtered_indices,
            unfiltered,
        })
    }

    /// Returns the indices of every pattern matching anywhere in `haystack`,
    /// ascending and deduplicated.
    #[must_use]
    pub fn matched_indices(&self, haystack: &str) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .literals
            .find_overlapping_iter(haystack)
            .map(|m| self.literal_indices[m.pattern().as_usize()])
            .chain(
                self.filtered
                    .matching(haystack)
                    .map(|(id, _)| self.filtered_indices[id]),
            )
            .chain(
                self.unfiltered
                    .iter()
                    .filter(|(_, regex)| regex.is_match(haystack))
                    .map(|(idx, _)| *idx),
            )
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The original pattern strings, indexed as in [`Self::matched_indices`].
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

impl std::fmt::Debug for PatternSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatternSet")
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
    fn case_insensitive_patterns_route_to_regex_tier() {
        let set = PatternSet::build(["(?i)dargaroth"]).unwrap();
        assert_eq!(set.matched_indices("DARGAROTH snarls."), vec![0]);
        assert_eq!(set.matched_indices("dargaroth snarls."), vec![0]);
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
