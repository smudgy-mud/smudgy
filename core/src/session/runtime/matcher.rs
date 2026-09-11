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
//!
//! Above all of it sits [`ScanCache`], which answers a line the set has already seen
//! from the last answer instead of scanning it again. MUD output repeats heavily
//! enough for that to be the largest single saving here.

use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};
use anyhow::{Context, Result};
use regex::Regex;
use regex_syntax::hir::HirKind;

/// One matching pattern; pure literals also retain their earliest byte span.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Answers for lines this set has already matched. `None` for a set too small to be
    /// worth caching; see [`MIN_CACHED_PATTERNS`]. The `Option` is outside the cell so a
    /// set without one pays a branch per line rather than a borrow.
    cache: Option<std::cell::RefCell<ScanCache>>,
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
        let pattern_count = patterns.len();

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
            cache: (pattern_count >= MIN_CACHED_PATTERNS)
                .then(|| std::cell::RefCell::new(ScanCache::new())),
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
        // A line this set has already matched is answered from that answer. What follows is
        // a pure function of `self` and `haystack`, and `self` is immutable once built — a
        // change to the trigger population rebuilds the whole `PatternSet`, and the cache
        // with it — so the previous answer is still the right one.
        let store_under = match &self.cache {
            Some(cache) => match cache.borrow_mut().probe(haystack, out) {
                Probe::Hit => return,
                Probe::Miss(hash) => Some(hash),
                Probe::Skipped => None,
            },
            None => None,
        };
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

        if let Some(hash) = store_under
            && let Some(cache) = &self.cache
        {
            cache.borrow_mut().store(hash, haystack, out);
        }
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

/// Buckets in [`ScanCache`], each holding [`WAYS`] answers.
///
/// 2-way over 2048 buckets remembers 4096 lines and, on a real several-hour session log,
/// answers about two thirds of lines from cache — within a point of a true LRU of the same
/// size, for none of its bookkeeping. Raising it has little left to win: the same log gains
/// under three points from four times the size.
const BUCKETS: usize = 2048;

/// Answers kept per bucket. Two is what makes this match an LRU: one is a direct-mapped
/// table, which loses four to six points to conflicts on both corpora measured.
const WAYS: usize = 2;

/// Below this many patterns a set is not cached at all. The scan a hit skips is roughly
/// proportional to the pattern population, so on a tiny set — a session with two triggers,
/// or a prompt tier with none — the probe would be a large fraction of what it saves, and
/// the table would be most of the set's memory.
const MIN_CACHED_PATTERNS: usize = 8;

/// A line longer than this is matched but not remembered. The entry would pin its bytes
/// for as long as the set lives, and a line this long is not the repeated kind.
const MAX_CACHED_LINE: usize = 4096;

/// Lines between reassessments of whether the cache is paying for itself.
const WINDOW: u32 = 8192;

/// Hit rate below which the cache backs off to sampling one line in `2^n`.
///
/// Break-even is lower than this: a probe is a hash of the line plus a bucket compare,
/// against a scan of every automaton and candidate regex — roughly one in twenty of it for
/// a large trigger set. It is set well above break-even because the saving shrinks with the
/// pattern population while the probe does not, so a set near [`MIN_CACHED_PATTERNS`] needs
/// a much better hit rate than a large one to come out ahead, and this one threshold serves
/// both.
const BACK_OFF_BELOW: f32 = 0.15;

/// Hit rate at which it goes back to probing every line. Above `BACK_OFF_BELOW` so a
/// workload sitting near the threshold does not oscillate.
const RESUME_ABOVE: f32 = 0.30;

/// The most a back-off can thin probing: one line in 64.
const MAX_PROBE_MASK: u32 = 63;

/// What a probe decided.
enum Probe {
    /// `out` holds the answer.
    Hit,
    /// Not remembered. Scan, then `store` under this hash.
    Miss(u64),
    /// Not looked at, because the cache is sampling. Scan, and store nothing.
    Skipped,
}

/// One remembered answer. Its buffers are reused when the entry is replaced, so a set in
/// steady state does not allocate to remember a line.
#[derive(Default)]
struct Entry {
    hash: u64,
    /// The line this answer belongs to. A hash equal by accident is not enough: two lines
    /// that collided would take each other's triggers, which is a wrong answer with no
    /// symptom. The compare is over bytes already in cache from hashing them.
    line: String,
    matches: Vec<PatternMatch>,
    /// An entry is empty until first filled; an empty `line` is a legitimate key.
    filled: bool,
}

/// The answers this set has already given, keyed by the line that produced them.
///
/// MUD output repeats: a real several-hour session is 28% distinct lines, and two thirds of
/// its lines repeat one of the last few thousand. Matching is the largest cost in the
/// engine and it is a pure function of the line, so a repeat need not pay it twice.
///
/// The cache measures whether that is true of the session it is actually in. A workload of
/// mostly-unique lines — a file transfer, a long unique-text dump — would otherwise pay a
/// hash per line for nothing, so when the hit rate falls the cache thins its probing to a
/// sample, which still notices if the traffic becomes repetitive again.
struct ScanCache {
    entries: Box<[Entry]>,
    /// Lines seen since the window opened.
    seen: u32,
    probes: u32,
    hits: u32,
    /// A line is probed when `seen & probe_mask == 0`; zero probes every line.
    probe_mask: u32,
}

impl ScanCache {
    fn new() -> Self {
        Self {
            entries: (0..BUCKETS * WAYS).map(|_| Entry::default()).collect(),
            seen: 0,
            probes: 0,
            hits: 0,
            probe_mask: 0,
        }
    }

    /// Look `haystack` up, filling `out` and returning [`Probe::Hit`] if it is remembered.
    fn probe(&mut self, haystack: &str, out: &mut Vec<PatternMatch>) -> Probe {
        self.seen += 1;
        if self.seen >= WINDOW {
            self.reassess();
        }
        if self.seen & self.probe_mask != 0 || haystack.len() > MAX_CACHED_LINE {
            return Probe::Skipped;
        }
        self.probes += 1;
        let hash = twox_hash::XxHash3_64::oneshot(haystack.as_bytes());
        let bucket = Self::bucket(hash);
        for way in 0..WAYS {
            let entry = &self.entries[bucket + way];
            if entry.filled && entry.hash == hash && entry.line == haystack {
                out.extend_from_slice(&entry.matches);
                self.hits += 1;
                if way != 0 {
                    // Answering from the second way promotes it, so the bucket keeps the
                    // two lines most recently asked for rather than the two most recently
                    // stored.
                    self.entries.swap(bucket, bucket + way);
                }
                return Probe::Hit;
            }
        }
        Probe::Miss(hash)
    }

    /// Remember `matches` as the answer for `haystack`.
    fn store(&mut self, hash: u64, haystack: &str, matches: &[PatternMatch]) {
        let bucket = Self::bucket(hash);
        // The incoming answer takes the first way, and what was there moves to the second.
        // Swapping rather than overwriting keeps both entries' buffers, which is what makes
        // a steady-state store allocate nothing.
        self.entries.swap(bucket, bucket + WAYS - 1);
        let entry = &mut self.entries[bucket];
        entry.hash = hash;
        entry.line.clear();
        entry.line.push_str(haystack);
        entry.matches.clear();
        entry.matches.extend_from_slice(matches);
        entry.filled = true;
    }

    fn bucket(hash: u64) -> usize {
        // The low bits of an XXH3 hash are as well distributed as any other, and the table
        // is a power of two.
        (hash as usize & (BUCKETS - 1)) * WAYS
    }

    /// Decide whether the last window of lines was worth probing, and open a new one.
    fn reassess(&mut self) {
        if self.probes > 0 {
            let rate = self.hits as f32 / self.probes as f32;
            if rate >= RESUME_ABOVE {
                self.probe_mask = 0;
            } else if rate < BACK_OFF_BELOW {
                self.probe_mask = ((self.probe_mask << 1) | 1).min(MAX_PROBE_MASK);
            }
        }
        self.seen = 0;
        self.probes = 0;
        self.hits = 0;
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

    /// A set big enough to be cached, so the cached and uncached paths can be compared.
    fn cached_set() -> PatternSet {
        PatternSet::build([
            "ring",
            "shiny",
            "Dargaroth",
            "café",
            "gold",
            "sword",
            "tower",
            "forest",
            r"^You (\w+) the (\w+)",
            r"hits you for (\d+)",
            "",
        ])
        .unwrap()
    }

    #[test]
    fn a_repeated_line_is_answered_the_same_way() {
        let set = cached_set();
        assert!(set.cache.is_some(), "this set should be cached");
        let lines = [
            "a shiny ring of gold",
            "You draw the sword",
            "nothing of note here",
            "it hits you for 12",
            "",
            "a shiny ring of gold",
        ];
        // Every line twice: the second visit is a cache hit and must be indistinguishable.
        for line in lines {
            let first = set.matches(line);
            let second = set.matches(line);
            assert_eq!(first, second, "cached answer differs for {line:?}");
            // And a third time, now that it is certainly stored.
            assert_eq!(
                first,
                set.matches(line),
                "cached answer drifted for {line:?}"
            );
        }
    }

    #[test]
    fn a_cached_answer_belongs_to_its_own_line() {
        // Two lines in the same bucket must not take each other's answer. Rather than hunt
        // for a colliding pair, drive enough distinct lines through one set that every
        // bucket is contested, and check each answer against a set that cannot cache.
        let cached = cached_set();
        let uncached = PatternSet::build(["ring", "shiny"]).unwrap();
        assert!(uncached.cache.is_none(), "small sets are not cached");
        for i in 0..(BUCKETS * WAYS * 2) {
            let line = format!("line {i} with a shiny ring and gold");
            let answer = cached.matched_indices(&line);
            assert_eq!(
                answer,
                cached.matched_indices(&line),
                "second visit to {line:?} disagreed"
            );
            assert!(answer.contains(&0) && answer.contains(&1) && answer.contains(&4));
        }
        // The lines that fell out are recomputed correctly, not answered from a neighbour.
        assert_eq!(
            cached.matched_indices("line 0 with a shiny ring and gold"),
            vec![0, 1, 4, 10]
        );
    }

    #[test]
    fn probing_thins_out_when_the_cache_stops_paying_and_recovers() {
        let set = cached_set();
        // A window of lines that never repeat: the cache is asked, never answers, and
        // should stop asking on every line.
        for i in 0..(WINDOW + 1) {
            drop(set.matches(&format!("unique line {i} in a forest")));
        }
        let thinned = set.cache.as_ref().unwrap().borrow().probe_mask;
        assert!(
            thinned > 0,
            "probing should have thinned, mask is {thinned}"
        );

        // Now a repetitive window. The sampled probes still store, so the hit rate climbs
        // and full probing resumes.
        for _ in 0..(WINDOW * (thinned + 1) + 1) {
            drop(set.matches("a shiny ring of gold"));
        }
        assert_eq!(
            set.cache.as_ref().unwrap().borrow().probe_mask,
            0,
            "probing should have resumed on repetitive traffic"
        );
    }

    #[test]
    fn a_line_too_long_to_remember_still_matches() {
        let set = cached_set();
        let long = format!("{} shiny ring", "x".repeat(MAX_CACHED_LINE));
        assert!(long.len() > MAX_CACHED_LINE);
        assert_eq!(set.matched_indices(&long), vec![0, 1, 10]);
        assert_eq!(set.matched_indices(&long), vec![0, 1, 10]);
        // Nothing that long was kept.
        assert!(
            set.cache
                .as_ref()
                .unwrap()
                .borrow()
                .entries
                .iter()
                .all(|e| e.line.len() <= MAX_CACHED_LINE)
        );
    }

    #[test]
    fn a_hash_collision_misses_rather_than_answering_the_wrong_line() {
        // A 64-bit hash collides eventually, and a cache that keyed on the hash alone would
        // hand one line another line's triggers — a wrong answer with no symptom. Rather
        // than hunt for a real XXH3 collision, store an answer under the hash of a
        // different line, which is the exact state a collision produces: same bucket, same
        // stored hash, different text.
        let mut c = ScanCache::new();
        let theirs = twox_hash::XxHash3_64::oneshot(b"the other line");
        c.store(
            theirs,
            "this line",
            &[PatternMatch {
                index: 3,
                literal: None,
            }],
        );

        let mut out = Vec::new();
        match c.probe("the other line", &mut out) {
            Probe::Miss(hash) => assert_eq!(hash, theirs, "the collision was not constructed"),
            Probe::Hit => panic!("a colliding line took another line's answer"),
            Probe::Skipped => panic!("probing should be on"),
        }
        assert!(out.is_empty(), "a miss must leave the answer to the scan");
    }

    #[test]
    fn cache_isolated_store_then_probe() {
        let mut c = ScanCache::new();
        let mut out = Vec::new();
        match c.probe("hello world", &mut out) {
            Probe::Miss(h) => {
                c.store(
                    h,
                    "hello world",
                    &[PatternMatch {
                        index: 7,
                        literal: None,
                    }],
                );
            }
            Probe::Hit => panic!("hit on an empty cache"),
            Probe::Skipped => panic!("skipped with mask 0"),
        }
        out.clear();
        match c.probe("hello world", &mut out) {
            Probe::Hit => assert_eq!(out.len(), 1),
            Probe::Miss(_) => panic!("stored answer was not found"),
            Probe::Skipped => panic!("skipped with mask 0"),
        }
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
