//! Compares multi-pattern matching strategies for the trigger/substitution
//! hot path, using a synthetic 16MB+ session log and 6,350 synthetic item names as
//! unanchored literal patterns (the "thousands of substitutions" workload).
//!
//! Strategies:
//! - `regex_set`: what `core/src/session/runtime/trigger.rs` does today —
//!   `RegexSet::matches` to find which patterns hit, then a per-pattern
//!   `Regex::captures` re-run for each hit.
//! - `regex_filtered`: the `regex-filtered` crate (a port of RE2's
//!   `FilteredRE2`) — Aho-Corasick prefilter over literal atoms, full regex
//!   confirmation only for candidates, then the same `captures` re-run.
//! - `aho_corasick_overlapping`: pure Aho-Corasick reporting every pattern
//!   present in the line (trigger semantics: "which triggers fire"). No
//!   captures pass — a literal tier would synthesize `$0` from the match span.
//! - `aho_corasick_leftmost`: leftmost-longest non-overlapping matches
//!   (substitution/highlight semantics: spans to rewrite).
//! - `tiered`: mirrors `core/src/session/runtime/matcher.rs` (`PatternSet`) —
//!   literals to Aho-Corasick, the rest to regex-filtered, results merged into
//!   ascending deduplicated indices, then the same per-match `captures` re-run
//!   `Trigger::run` performs. This is the engine smudgy actually ships.
//!
//! The `mixed` group adds ~100 representative non-literal regex triggers on
//! top of the literals, for the two engines that support them.
//!
//! The `regex_set_current` baseline is so slow (~40 KiB/s at this pattern
//! count) that it scans only the first 20k lines; throughput is normalized
//! to bytes scanned, so its numbers remain comparable to the full-corpus
//! engines.
//!
//! Corpora + the shared `REGEX_TRIGGERS` set live in the crate lib (`src/lib.rs`).
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` truncates the log corpus (faster runs),
//! `SMUDGY_BENCH_SKIP_SANITY=1` skips the cross-engine agreement check.

use std::{collections::HashSet, hint::black_box};

use aho_corasick::{AhoCorasick, MatchKind};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use regex::{Regex, RegexSet, RegexSetBuilder};
use smudgy_bench::{REGEX_TRIGGERS, load_item_names, load_log_lines};

/// Mirrors `MAX_REGEX_SIZE` in `core/src/session/runtime/trigger.rs`.
const MAX_REGEX_SIZE: usize = 512 * 1024 * 1024;

fn build_regex_set(patterns: &[String]) -> RegexSet {
    RegexSetBuilder::new(patterns)
        .size_limit(MAX_REGEX_SIZE)
        .build()
        .expect("RegexSet build")
}

fn build_regexes(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|p| Regex::new(p).expect("pattern compiles"))
        .collect()
}

fn build_filtered(patterns: &[String]) -> regex_filtered::Regexes {
    let mut builder = regex_filtered::Builder::new_atom_len(2);
    for p in patterns {
        builder = builder.push(p).expect("pattern parses");
    }
    builder.build().expect("regex-filtered build")
}

/// Mirrors `as_literal` in `core/src/session/runtime/matcher.rs`.
fn as_literal(pattern: &str) -> Option<String> {
    let hir = regex_syntax::parse(pattern).ok()?;
    if let regex_syntax::hir::HirKind::Literal(literal) = hir.kind() {
        std::str::from_utf8(&literal.0).ok().map(ToOwned::to_owned)
    } else {
        None
    }
}

/// Mirrors `PatternSet` in `core/src/session/runtime/matcher.rs` (without the
/// unfiltered fallback bucket, which is empty for these corpora).
struct Tiered {
    literals: AhoCorasick,
    literal_indices: Vec<usize>,
    filtered: regex_filtered::Regexes,
    filtered_indices: Vec<usize>,
}

fn build_tiered(patterns: &[String]) -> Tiered {
    let mut literal_strings = Vec::new();
    let mut literal_indices = Vec::new();
    let mut filter_patterns = Vec::new();
    let mut filtered_indices = Vec::new();
    for (idx, pattern) in patterns.iter().enumerate() {
        if let Some(literal) = as_literal(pattern) {
            literal_strings.push(literal);
            literal_indices.push(idx);
        } else {
            filter_patterns.push(pattern.clone());
            filtered_indices.push(idx);
        }
    }
    Tiered {
        literals: AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&literal_strings)
            .expect("aho-corasick build"),
        literal_indices,
        filtered: build_filtered(&filter_patterns),
        filtered_indices,
    }
}

impl Tiered {
    fn matched_indices(&self, haystack: &str) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .literals
            .find_overlapping_iter(haystack)
            .map(|m| self.literal_indices[m.pattern().as_usize()])
            .chain(
                self.filtered
                    .matching(haystack)
                    .map(|(id, _)| self.filtered_indices[id]),
            )
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The shipped flow: `PatternSet::matched_indices`, then the per-match
/// `captures` re-run `Trigger::run` performs.
fn scan_tiered(tiered: &Tiered, regexes: &[Regex], lines: &[String]) -> u64 {
    let mut hits = 0;
    for line in lines {
        for idx in tiered.matched_indices(line) {
            black_box(regexes[idx].captures(line));
            hits += 1;
        }
    }
    hits
}

/// Current trigger.rs flow: `RegexSet::matches`, then a `captures` re-run per
/// matched pattern (trigger.rs:403 + trigger.rs:657).
fn scan_regex_set(set: &RegexSet, regexes: &[Regex], lines: &[String]) -> u64 {
    let mut hits = 0;
    for line in lines {
        for idx in set.matches(line) {
            black_box(regexes[idx].captures(line));
            hits += 1;
        }
    }
    hits
}

/// regex-filtered flow: prefiltered + confirmed match, then the same
/// `captures` re-run a trigger would do for its capture variables.
fn scan_filtered(filtered: &regex_filtered::Regexes, regexes: &[Regex], lines: &[String]) -> u64 {
    let mut hits = 0;
    for line in lines {
        for (idx, _re) in filtered.matching(line) {
            black_box(regexes[idx].captures(line));
            hits += 1;
        }
    }
    hits
}

/// Trigger semantics on a literal tier: the distinct set of patterns present
/// anywhere in the line. `$0` comes from the match itself — no captures pass.
fn scan_ac_overlapping(ac: &AhoCorasick, seen: &mut HashSet<usize>, lines: &[String]) -> u64 {
    let mut hits = 0;
    for line in lines {
        seen.clear();
        for m in ac.find_overlapping_iter(line) {
            if seen.insert(m.pattern().as_usize()) {
                black_box((m.start(), m.end()));
                hits += 1;
            }
        }
    }
    hits
}

/// Substitution semantics on a literal tier: leftmost-longest non-overlapping
/// spans, i.e. the spans a rewrite/highlight pass would operate on.
fn scan_ac_leftmost(ac: &AhoCorasick, lines: &[String]) -> u64 {
    let mut hits = 0;
    for line in lines {
        for m in ac.find_iter(line) {
            black_box((m.pattern().as_usize(), m.start(), m.end()));
            hits += 1;
        }
    }
    hits
}

/// Verify the three "which patterns matched" engines agree before trusting
/// the numbers. Runs on a slice of the corpus to keep startup quick.
fn sanity_check(
    set: &RegexSet,
    filtered: &regex_filtered::Regexes,
    ac: &AhoCorasick,
    tiered: &Tiered,
    lines: &[String],
) {
    let mut total = 0_u64;
    let mut lines_with_hits = 0_u64;
    for line in lines {
        let from_set: HashSet<usize> = set.matches(line).iter().collect();
        let from_filtered: HashSet<usize> = filtered.matching(line).map(|(idx, _)| idx).collect();
        let from_ac: HashSet<usize> = ac
            .find_overlapping_iter(line)
            .map(|m| m.pattern().as_usize())
            .collect();
        let from_tiered: HashSet<usize> = tiered.matched_indices(line).into_iter().collect();
        assert_eq!(
            from_set, from_filtered,
            "regex-filtered disagrees on: {line}"
        );
        assert_eq!(from_set, from_ac, "aho-corasick disagrees on: {line}");
        assert_eq!(from_set, from_tiered, "tiered disagrees on: {line}");
        total += from_set.len() as u64;
        lines_with_hits += u64::from(!from_set.is_empty());
    }
    eprintln!(
        "sanity: engines agree on {} lines ({lines_with_hits} lines with hits, {total} pattern hits)",
        lines.len()
    );
}

#[allow(clippy::too_many_lines)]
fn trigger_matching(c: &mut Criterion) {
    let lines = load_log_lines();
    let names = load_item_names();
    let corpus_bytes: u64 = lines.iter().map(|l| l.len() as u64).sum();
    eprintln!(
        "corpus: {} lines / {corpus_bytes} bytes, {} literal patterns, {} regex triggers",
        lines.len(),
        names.len(),
        REGEX_TRIGGERS.len()
    );

    let literal_patterns: Vec<String> = names.iter().map(|n| regex::escape(n)).collect();
    let mixed_patterns: Vec<String> = literal_patterns
        .iter()
        .cloned()
        .chain(REGEX_TRIGGERS.iter().map(|&p| p.to_owned()))
        .collect();

    eprintln!("building matchers...");
    let lit_set = build_regex_set(&literal_patterns);
    let lit_regexes = build_regexes(&literal_patterns);
    let lit_filtered = build_filtered(&literal_patterns);
    let ac_overlapping = AhoCorasick::builder()
        .match_kind(MatchKind::Standard)
        .build(&names)
        .expect("aho-corasick build");
    let ac_leftmost = AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&names)
        .expect("aho-corasick build");

    let lit_tiered = build_tiered(&literal_patterns);

    let mixed_set = build_regex_set(&mixed_patterns);
    let mixed_regexes = build_regexes(&mixed_patterns);
    let mixed_filtered = build_filtered(&mixed_patterns);
    let mixed_tiered = build_tiered(&mixed_patterns);

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        let slice = &lines[..lines.len().min(20_000)];
        sanity_check(&lit_set, &lit_filtered, &ac_overlapping, &lit_tiered, slice);
    }

    // The RegexSet baseline runs at ~40 KiB/s with this many patterns — a
    // full-corpus pass takes minutes, so it gets a capped slice. Throughput
    // is normalized to bytes scanned, so the numbers remain comparable.
    let baseline_lines = &lines[..lines.len().min(20_000)];
    let baseline_bytes: u64 = baseline_lines.iter().map(|l| l.len() as u64).sum();
    if baseline_lines.len() < lines.len() {
        eprintln!(
            "note: regex_set_current benches scan only the first {} lines; other engines scan all {}",
            baseline_lines.len(),
            lines.len()
        );
    }

    let mut group = c.benchmark_group("scan_literals");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(baseline_bytes));
    group.bench_function("regex_set_current", |b| {
        b.iter(|| scan_regex_set(&lit_set, &lit_regexes, baseline_lines));
    });
    group.throughput(Throughput::Bytes(corpus_bytes));
    group.bench_function("regex_filtered", |b| {
        b.iter(|| scan_filtered(&lit_filtered, &lit_regexes, &lines));
    });
    group.bench_function("aho_corasick_overlapping", |b| {
        let mut seen = HashSet::new();
        b.iter(|| scan_ac_overlapping(&ac_overlapping, &mut seen, &lines));
    });
    group.bench_function("aho_corasick_leftmost", |b| {
        b.iter(|| scan_ac_leftmost(&ac_leftmost, &lines));
    });
    group.bench_function("tiered", |b| {
        b.iter(|| scan_tiered(&lit_tiered, &lit_regexes, &lines));
    });
    group.finish();

    let mut group = c.benchmark_group("scan_mixed");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(baseline_bytes));
    group.bench_function("regex_set_current", |b| {
        b.iter(|| scan_regex_set(&mixed_set, &mixed_regexes, baseline_lines));
    });
    group.throughput(Throughput::Bytes(corpus_bytes));
    group.bench_function("regex_filtered", |b| {
        b.iter(|| scan_filtered(&mixed_filtered, &mixed_regexes, &lines));
    });
    group.bench_function("tiered", |b| {
        b.iter(|| scan_tiered(&mixed_tiered, &mixed_regexes, &lines));
    });
    group.finish();

    // Rebuild cost matters too: trigger.rs rebuilds the full set on every
    // trigger add/update/enable.
    let mut group = c.benchmark_group("build");
    group.sample_size(10);
    group.bench_function("regex_set", |b| {
        b.iter(|| black_box(build_regex_set(&mixed_patterns)));
    });
    group.bench_function("regex_filtered", |b| {
        b.iter(|| black_box(build_filtered(&mixed_patterns)));
    });
    group.bench_function("aho_corasick", |b| {
        b.iter(|| {
            black_box(
                AhoCorasick::builder()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(&names)
                    .expect("aho-corasick build"),
            )
        });
    });
    group.bench_function("tiered", |b| {
        b.iter(|| black_box(build_tiered(&mixed_patterns)));
    });
    group.finish();
}

criterion_group!(benches, trigger_matching);
criterion_main!(benches);
