//! The pattern-field syntax highlighter (`matching-logic.md` §9): real styled
//! runs inside an editable [`iced::widget::text_editor`], with a true caret —
//! never a styled overlay that can drift out of sync with the text.
//!
//! The scanners here are display-only. They deliberately do not re-derive
//! match semantics — compiling and matching stay in
//! `smudgy_core::models::matchers`, and Send text expansion in the runtime's
//! template expander — they only mark where the accent runs go, spelling the
//! same grammar so a run's color tells the truth about what will happen. The
//! pieces of the reference grammar (an identifier, a bare reference's dotted
//! tail, a braced tail's segments) are the runtime's own, imported from
//! `smudgy_core::models::state_exposure`, so the two cannot drift.

use std::ops::Range;

use iced::advanced::text::highlighter::Highlighter;
use smudgy_core::models::state_exposure::{ReferenceSegments, dotted_tail_len, identifier_len};

/// One highlighted run. The editors map these to theme colors at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A `{hole}` span — the capture accent.
    Hole,
    /// A bare `*` wildcard — the accent at reduced strength.
    Wildcard,
    /// A `/…/` regex island inside a Simple pattern.
    Island,
    /// A `(?<name>` group opener in a regex source.
    GroupOpen,
    /// A `\e` or `\x1b` escape in a regex source, or the `$$` escape (a
    /// literal `$`) in a send-text body.
    Escape,
    /// A reference the automation provides (send-text body): a capture, or a
    /// state value at or below an exposed path.
    KnownRef,
    /// A `$ref` nothing captures (send-text body).
    UnknownRef,
    /// A state reference (`$name.path`, `${name.path}`) the exposures do not
    /// cover (send-text body); the editor names it under the field.
    UnexposedRef,
}

/// One exposed state path a send-text body can reference, ASCII-folded as the
/// store folds: the name the action spells and the path segments beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedPath {
    name: String,
    segments: Vec<String>,
}

impl ExposedPath {
    /// Folds `name` and `segments`, so callers pass the author's spelling.
    pub fn new(name: &str, segments: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self {
            name: name.to_ascii_lowercase(),
            segments: segments
                .into_iter()
                .map(|segment| segment.as_ref().to_ascii_lowercase())
                .collect(),
        }
    }

    /// Whether the folded `segments` of a reference under this path's name sit
    /// at or below it.
    fn covers(&self, segments: &[String]) -> bool {
        self.segments.len() <= segments.len()
            && self
                .segments
                .iter()
                .zip(segments)
                .all(|(exposed, segment)| exposed == segment)
    }
}

/// Which grammar a field holds, as the highlighter's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldSyntax {
    /// A Simple pattern: `{holes}`, `*`, `/islands/`.
    Pattern,
    /// A regex source: group openers and escapes get the accent.
    Regex,
    /// A body sent as written, with no expansion (a hotkey exposing no state):
    /// nothing is marked. The field keeps the highlighter's widget state, so a
    /// first exposure switches it to [`FieldSyntax::SendText`] without a
    /// remount.
    Plain,
    /// The send-text action body; `known` is every reference the matcher
    /// provides, rendered (`$name`, `$1`, `$0`), and `exposed` every state path
    /// the automation reads.
    SendText {
        known: Vec<String>,
        exposed: Vec<ExposedPath>,
    },
}

/// A per-line scanner for one [`FieldSyntax`]. Stateless across lines: each
/// line is highlighted from scratch, which is exactly right for fields one
/// line tall.
pub struct PatternHighlighter {
    syntax: FieldSyntax,
    current_line: usize,
}

impl Highlighter for PatternHighlighter {
    type Settings = FieldSyntax;
    type Highlight = Token;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Token)>;

    fn new(settings: &Self::Settings) -> Self {
        Self {
            syntax: settings.clone(),
            current_line: 0,
        }
    }

    fn update(&mut self, new_settings: &Self::Settings) {
        self.syntax = new_settings.clone();
        self.current_line = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.current_line = self.current_line.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        self.current_line += 1;
        let spans = match &self.syntax {
            FieldSyntax::Pattern => scan_pattern(line),
            FieldSyntax::Regex => scan_regex(line),
            FieldSyntax::Plain => Vec::new(),
            FieldSyntax::SendText { known, exposed } => scan_send_text(line, known, exposed),
        };
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

/// Marks `{holes}`, `*` wildcards, and `/…/` islands in a Simple pattern.
/// Mirrors the compiler's tokenization shape: an unclosed brace or an unpaired
/// slash is literal text; a backslash inside an island escapes its closing
/// delimiter.
pub fn scan_pattern(line: &str) -> Vec<(Range<usize>, Token)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => match line[i..].find('}') {
                Some(rel) => {
                    spans.push((i..i + rel + 1, Token::Hole));
                    i += rel + 1;
                }
                None => i += 1,
            },
            b'*' => {
                spans.push((i..i + 1, Token::Wildcard));
                i += 1;
            }
            b'/' => {
                let mut j = i + 1;
                let mut close = None;
                while j < bytes.len() {
                    match bytes[j] {
                        b'\\' => j += 2,
                        b'/' => {
                            close = Some(j);
                            break;
                        }
                        _ => j += 1,
                    }
                }
                match close {
                    Some(j) => {
                        spans.push((i..j + 1, Token::Island));
                        i = j + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    spans
}

/// Marks `(?<name>` group openers and `\e` / `\x1b` escapes in a regex
/// source. Lookbehind-style `(?<=` / `(?<!` openers are not group names and
/// stay unhighlighted.
pub fn scan_regex(line: &str) -> Vec<(Range<usize>, Token)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let name_start = if line[i..].starts_with("(?<") {
                Some(i + 3)
            } else if line[i..].starts_with("(?P<") {
                Some(i + 4)
            } else {
                None
            };
            if let Some(start) = name_start
                && !matches!(bytes.get(start), Some(b'=' | b'!'))
                && let Some(rel) = line[start..].find('>')
            {
                spans.push((i..start + rel + 1, Token::GroupOpen));
                i = start + rel + 1;
                continue;
            }
            i += 1;
        } else if bytes[i] == b'\\' {
            if line[i..].starts_with("\\x1b") || line[i..].starts_with("\\x1B") {
                spans.push((i..i + 4, Token::Escape));
                i += 4;
            } else if line[i..].starts_with("\\e") {
                spans.push((i..i + 2, Token::Escape));
                i += 2;
            } else {
                // Any other escape: skip both bytes so `\\e` stays literal.
                i += 2;
            }
        } else {
            i += 1;
        }
    }
    spans
}

/// Marks the references of a send-text body, spelling the runtime's template
/// grammar: `$$` is the escape for a literal `$`; `$N` is the single-digit
/// capture; `$name` is a capture, or, when `name` is an exposed state name, a
/// state reference that takes the dotted tail after it
/// (`$gmcp.Char.Vitals.hp`; a trailing `.` stays literal); `${…}` is the
/// braced form of any of them, where a state path may also spell bracket keys
/// (`${gmcp["Some-Pkg"].Msg}`). A lone `$`, and `${` with no closing brace,
/// are literal.
///
/// A reference is known when the matcher captures it, or when it names an
/// exposed value at or below an exposed path (names and paths compare folded,
/// as the store folds). A bare `$name` that is both a capture and an exposed
/// name means the capture, the runtime's rule. A state reference the
/// exposures do not cover is [`Token::UnexposedRef`], so the editor can say
/// which path to expose.
pub fn scan_send_text(
    line: &str,
    known: &[String],
    exposed: &[ExposedPath],
) -> Vec<(Range<usize>, Token)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'$') => {
                spans.push((i..i + 2, Token::Escape));
                i += 2;
            }
            Some(b'{') => match line[i + 2..].find('}') {
                Some(rel) => {
                    let end = i + 2 + rel + 1;
                    let inner = &line[i + 2..i + 2 + rel];
                    spans.push((i..end, classify_braced(inner, known, exposed)));
                    i = end;
                }
                // No closing brace: the `$` is literal, and so is what follows it.
                None => i += 1,
            },
            Some(b'0'..=b'9') => {
                spans.push((i..i + 2, capture_token(&line[i..i + 2], known)));
                i += 2;
            }
            Some(c) if *c == b'_' || c.is_ascii_alphabetic() => {
                let name_end = i + 1 + identifier_len(&line[i + 1..]);
                let name = &line[i + 1..name_end];
                if is_exposed_name(name, exposed) {
                    // An exposed name takes the dotted tail as part of the
                    // reference; without one, a same-named capture wins.
                    let end = name_end + dotted_tail_len(&line[name_end..]);
                    let token = if end == name_end && is_known(&line[i..name_end], known) {
                        Token::KnownRef
                    } else {
                        let segments = line[name_end..end].split('.').skip(1);
                        classify_state(name, segments, exposed)
                    };
                    spans.push((i..end, token));
                    i = end;
                } else {
                    spans.push((i..name_end, capture_token(&line[i..name_end], known)));
                    i = name_end;
                }
            }
            // A lone `$`: literal.
            _ => i += 1,
        }
    }
    spans
}

/// The state references of a body's lines that the exposures do not cover, each
/// once in order of first appearance: what the editor names under the field.
/// Empty for every syntax but [`FieldSyntax::SendText`].
pub fn unexposed_references<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    syntax: &FieldSyntax,
) -> Vec<String> {
    let FieldSyntax::SendText { known, exposed } = syntax else {
        return Vec::new();
    };
    let mut references: Vec<String> = Vec::new();
    for line in lines {
        for (range, token) in scan_send_text(line, known, exposed) {
            let reference = &line[range];
            if token == Token::UnexposedRef && !references.iter().any(|seen| seen == reference) {
                references.push(reference.to_string());
            }
        }
    }
    references
}

fn is_known(reference: &str, known: &[String]) -> bool {
    known.iter().any(|k| k == reference)
}

/// The token of a capture reference (`$1`, `$name`, or a braced spelling
/// resolved to one).
fn capture_token(reference: &str, known: &[String]) -> Token {
    if is_known(reference, known) {
        Token::KnownRef
    } else {
        Token::UnknownRef
    }
}

fn is_exposed_name(name: &str, exposed: &[ExposedPath]) -> bool {
    !name.is_empty()
        && exposed
            .iter()
            .any(|path| path.name.eq_ignore_ascii_case(name))
}

/// The token of a state reference `name` + `segments`: known when an exposed
/// path under that name covers it.
fn classify_state<'a>(
    name: &str,
    segments: impl IntoIterator<Item = &'a str>,
    exposed: &[ExposedPath],
) -> Token {
    let folded: Vec<String> = segments.into_iter().map(str::to_ascii_lowercase).collect();
    let covered = exposed
        .iter()
        .any(|path| path.name.eq_ignore_ascii_case(name) && path.covers(&folded));
    if covered {
        Token::KnownRef
    } else {
        Token::UnexposedRef
    }
}

/// The token of a braced reference's inner text: digits are a capture index
/// however they are spelled (`${01}` is `$1`); an exposed name followed by a
/// path, or standing alone with no same-named capture, is a state reference;
/// anything else is the capture named by the whole text. A path under a name
/// nothing exposes is state-shaped, so the editor names it.
fn classify_braced(inner: &str, known: &[String], exposed: &[ExposedPath]) -> Token {
    if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
        return match inner.parse::<usize>() {
            Ok(index) => capture_token(&format!("${index}"), known),
            Err(_) => Token::UnknownRef,
        };
    }
    let (name, tail) = inner.split_at(identifier_len(inner));
    let capture = format!("${inner}");
    if is_exposed_name(name, exposed) && (!tail.is_empty() || !is_known(&capture, known)) {
        return match ReferenceSegments::new(tail).collect::<Result<Vec<&str>, _>>() {
            Ok(segments) => classify_state(name, segments, exposed),
            Err(_) => Token::UnexposedRef,
        };
    }
    if is_known(&capture, known) {
        Token::KnownRef
    } else if !name.is_empty() && !tail.is_empty() {
        Token::UnexposedRef
    } else {
        Token::UnknownRef
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(spans: &[(Range<usize>, Token)]) -> Vec<(Range<usize>, Token)> {
        spans.to_vec()
    }

    /// The scan as `(run text, token)` pairs, so tables read as the author sees them.
    fn marked(line: &str, known: &[&str], exposed: &[ExposedPath]) -> Vec<(String, Token)> {
        let known: Vec<String> = known.iter().map(ToString::to_string).collect();
        scan_send_text(line, &known, exposed)
            .into_iter()
            .map(|(range, token)| (line[range].to_string(), token))
            .collect()
    }

    fn run(text: &str, token: Token) -> (String, Token) {
        (text.to_string(), token)
    }

    fn exposed(entries: &[(&str, &[&str])]) -> Vec<ExposedPath> {
        entries
            .iter()
            .map(|(name, segments)| ExposedPath::new(name, segments.iter().copied()))
            .collect()
    }

    #[test]
    fn pattern_scanner_marks_holes_wildcards_and_islands() {
        let spans = scan_pattern("greet {person} * /\\d+/ {x");
        assert_eq!(
            kinds(&spans),
            vec![
                (6..14, Token::Hole),
                (15..16, Token::Wildcard),
                (17..22, Token::Island),
            ],
            "the unclosed brace at the end is literal text"
        );
    }

    #[test]
    fn island_escapes_shield_the_closing_slash() {
        let spans = scan_pattern("/a\\/b/");
        assert_eq!(spans, vec![(0..6, Token::Island)]);
    }

    #[test]
    fn regex_scanner_marks_group_openers_and_escapes() {
        let spans = scan_regex(r"\e\[31m(?<hp>\d+)(?:x)(?<=y)");
        assert_eq!(
            spans,
            vec![(0..2, Token::Escape), (7..13, Token::GroupOpen)],
            "non-capturing groups and lookbehind stay plain"
        );
        assert_eq!(
            scan_regex(r"\\e"),
            vec![],
            "a literal backslash-e is not the escape"
        );
        assert_eq!(scan_regex(r"\x1b"), vec![(0..4, Token::Escape)]);
    }

    #[test]
    fn send_text_scanner_classifies_references() {
        let known = vec!["$person".to_string(), "$1".to_string()];
        let spans = scan_send_text("say $person hits $targt for $1", &known, &[]);
        assert_eq!(
            spans,
            vec![
                (4..11, Token::KnownRef),
                (17..23, Token::UnknownRef),
                (28..30, Token::KnownRef),
            ]
        );
    }

    #[test]
    fn send_text_scanner_spells_the_runtime_escapes_digits_and_braces() {
        let known = ["$1", "$0", "$person"];
        // `$$` is the escape and hides what follows it; `$N` takes one digit;
        // a lone `$`, and `${` without its close, are literal.
        assert_eq!(
            marked("$$1 $10 $ ${x $$person", &known, &[]),
            vec![
                run("$$", Token::Escape),
                run("$1", Token::KnownRef),
                run("$$", Token::Escape),
            ]
        );
        assert_eq!(marked("costs 5$", &known, &[]), vec![]);
        // Braced captures resolve as their bare spelling would, digits by index.
        assert_eq!(
            marked("${1} ${01} ${2} ${} ${person} ${nobody}", &known, &[]),
            vec![
                run("${1}", Token::KnownRef),
                run("${01}", Token::KnownRef),
                run("${2}", Token::UnknownRef),
                run("${}", Token::UnknownRef),
                run("${person}", Token::KnownRef),
                run("${nobody}", Token::UnknownRef),
            ]
        );
        // Without exposures a dotted tail is literal text after the capture.
        assert_eq!(
            marked("$person.hp", &known, &[]),
            vec![run("$person", Token::KnownRef)]
        );
        // Text around references may be anything, including non-ASCII.
        assert_eq!(
            marked("powiedz $person → $1!", &known, &[]),
            vec![run("$person", Token::KnownRef), run("$1", Token::KnownRef)]
        );
    }

    #[test]
    fn send_text_scanner_resolves_state_references_against_the_exposures() {
        let known = ["$target", "$0"];
        let exposed = exposed(&[
            ("gmcp", &["Char", "Vitals"]),
            ("stats", &[]),
            ("target", &["hp"]),
        ]);
        let cases: [(&str, Token); 14] = [
            // Folded, at or below the exposed path.
            ("$gmcp.char.VITALS.hp", Token::KnownRef),
            ("$GMCP.Char.Vitals", Token::KnownRef),
            // Above, beside, or the bare root when the root is not exposed.
            ("$gmcp.Char", Token::UnexposedRef),
            ("$gmcp.Room.Info", Token::UnexposedRef),
            ("$gmcp", Token::UnexposedRef),
            // An exposed root covers the name and everything beneath it.
            ("$stats", Token::KnownRef),
            ("$stats.hp.max", Token::KnownRef),
            // A name that is also a capture: bare means the capture.
            ("$target", Token::KnownRef),
            ("$target.hp", Token::KnownRef),
            ("$target.mp", Token::UnexposedRef),
            // Braced forms, with bracket keys.
            ("${gmcp.Char.Vitals.hp}", Token::KnownRef),
            ("${gmcp[\"Char\"].vitals['hp']}", Token::KnownRef),
            ("${gmcp.Room.Info}", Token::UnexposedRef),
            ("${stats}", Token::KnownRef),
        ];
        for (reference, token) in cases {
            assert_eq!(
                marked(reference, &known, &exposed),
                vec![run(reference, token)],
                "{reference}"
            );
        }
        // A trailing dot is literal; the run stops before it.
        assert_eq!(
            marked("$gmcp.Char.Vitals. next", &known, &exposed),
            vec![run("$gmcp.Char.Vitals", Token::KnownRef)]
        );
        // A dotted tail under a name nothing exposes is literal text after the
        // capture, but its braced form is state-shaped and named as such.
        assert_eq!(
            marked("$foo.bar ${foo.bar}", &known, &exposed),
            vec![
                run("$foo", Token::UnknownRef),
                run("${foo.bar}", Token::UnexposedRef),
            ]
        );
        // Malformed braced tails render nothing at runtime, so they are unexposed.
        assert_eq!(
            marked(
                "${gmcp[Char]} ${gmcp.Char.Vitals.} ${gmcp.Char..hp}",
                &known,
                &exposed
            ),
            vec![
                run("${gmcp[Char]}", Token::UnexposedRef),
                run("${gmcp.Char.Vitals.}", Token::UnexposedRef),
                run("${gmcp.Char..hp}", Token::UnexposedRef),
            ]
        );
        // The escape hides a state reference too, and a bare capture is unchanged.
        assert_eq!(
            marked("$$gmcp.Char.Vitals.hp $0 $9x ${9x}", &known, &exposed),
            vec![
                run("$$", Token::Escape),
                run("$0", Token::KnownRef),
                run("$9", Token::UnknownRef),
                run("${9x}", Token::UnknownRef),
            ]
        );
    }

    #[test]
    fn unexposed_references_list_each_uncovered_state_reference_once() {
        let syntax = FieldSyntax::SendText {
            known: vec!["$target".to_string()],
            exposed: exposed(&[("gmcp", &["Char", "Vitals"])]),
        };
        let lines = [
            "say $gmcp.Room.Info $gmcp.Char.Vitals.hp",
            "say $gmcp.Room.Info again ${gmcp.Room.Info.name} $nobody",
        ];
        assert_eq!(
            unexposed_references(lines, &syntax),
            vec!["$gmcp.Room.Info", "${gmcp.Room.Info.name}"]
        );
        assert!(unexposed_references(lines, &FieldSyntax::Plain).is_empty());
        assert!(unexposed_references(lines, &FieldSyntax::Pattern).is_empty());
    }

    #[test]
    fn plain_syntax_marks_nothing() {
        let mut highlighter = PatternHighlighter::new(&FieldSyntax::Plain);
        assert_eq!(
            highlighter
                .highlight_line("say $x ${y} $$")
                .collect::<Vec<_>>(),
            vec![]
        );
        highlighter.update(&FieldSyntax::SendText {
            known: Vec::new(),
            exposed: Vec::new(),
        });
        assert_eq!(
            highlighter.highlight_line("say $x").collect::<Vec<_>>(),
            vec![(4..6, Token::UnknownRef)]
        );
    }
}
