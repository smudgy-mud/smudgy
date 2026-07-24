//! Deterministic wire-bytes generator: re-dresses the plain-text log corpora
//! into raw MUD socket bytes (ANSI SGR styling, CRLF framing, telnet IAC
//! traffic) so ingest benches measure the parse path against realistic input.
//!
//! The committed logs are display text — zero ESC and zero 0xFF bytes — which
//! makes them useless for benchmarking the ANSI/telnet layers directly.
//! Rather than committing a second multi-megabyte corpus, [`dress_lines`]
//! synthesizes the wire form on the fly. Every profile is seeded with a fixed
//! constant through an inline splitmix64, so two calls produce byte-identical
//! output and criterion numbers stay comparable across runs and machines.
//!
//! All emitted framing is *valid*: SGR sequences are `ESC [ params m`, IAC is
//! always followed by a complete telnet unit, literal 0xFF bytes appear only
//! as doubled `IAC IAC`, and subnegotiations are properly `IAC SB … IAC SE`
//! bracketed. The module tests scan the output to enforce this.

/// Telnet Interpret-As-Command lead byte; doubled (`IAC IAC`) for a literal
/// 0xFF inside line text.
pub const IAC: u8 = 255;
/// Telnet Go-Ahead: the classic MUD prompt terminator.
pub const GA: u8 = 249;
const SE: u8 = 240;
const SB: u8 = 250;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;

const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_EOR: u8 = 25;
const OPT_NAWS: u8 = 31;
const OPT_LINEMODE: u8 = 34;
const OPT_GMCP: u8 = 201;

const ESC: u8 = 0x1b;

/// How heavily the plain-text corpus gets dressed with ANSI + telnet traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireProfile {
    /// ~1 SGR color run per line (16-color), CRLF endings, and a short prompt
    /// followed by `IAC GA` roughly every 20 lines. The typical mostly-plain
    /// MUD stream.
    AnsiLight,
    /// 6–10 SGR runs per line mixing 16-color, 256-color, and truecolor with
    /// bold toggles and resets; prompts + `IAC GA` every ~10 lines. The
    /// heavily-decorated stream that stresses the SGR state machine.
    AnsiHeavy,
    /// [`WireProfile::AnsiLight`] styling plus dense telnet traffic: an
    /// opening negotiation burst, `IAC GA` after every line, occasional
    /// escaped-literal `IAC IAC` inside line text, and periodic short
    /// subnegotiations. Stresses the IAC layer rather than SGR parsing.
    IacDense,
}

/// Dresses `lines` (plain display text) into a single raw wire-byte stream
/// according to `profile`. Deterministic: seeded with a per-profile constant,
/// so repeated calls yield byte-identical output.
#[must_use]
pub fn dress_lines(lines: &[String], profile: WireProfile) -> Vec<u8> {
    match profile {
        WireProfile::AnsiLight => dress_ansi_light(lines),
        WireProfile::AnsiHeavy => dress_ansi_heavy(lines),
        WireProfile::IacDense => dress_iac_dense(lines),
    }
}

/// Splits `bytes` into `chunk_len`-sized slices (the final slice may be
/// shorter), simulating fixed-size socket reads — e.g. 16 KiB — so ingest
/// benches can feed the parser the way the connection loop does.
#[must_use]
pub fn chunk(bytes: &[u8], chunk_len: usize) -> Vec<&[u8]> {
    assert!(chunk_len > 0, "chunk_len must be non-zero");
    bytes.chunks(chunk_len).collect()
}

use crate::SplitMix64;

const SEED_ANSI_LIGHT: u64 = 0x5EED_0000_0000_0001;
const SEED_ANSI_HEAVY: u64 = 0x5EED_0000_0000_0002;
const SEED_IAC_DENSE: u64 = 0x5EED_0000_0000_0003;

/// Byte ranges of the whitespace-separated words of `line`. The corpora are
/// UTF-8 and the separators are ASCII space/tab, which never occur inside a
/// multi-byte sequence, so every range boundary is a valid char boundary —
/// safe to splice escape sequences at.
fn word_ranges(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b' ' || b == b'\t' {
            if let Some(s) = start.take() {
                ranges.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        ranges.push((s, bytes.len()));
    }
    ranges
}

/// Appends `ESC [ <params> m`.
fn push_sgr(out: &mut Vec<u8>, params: &str) {
    out.push(ESC);
    out.push(b'[');
    out.extend_from_slice(params.as_bytes());
    out.push(b'm');
}

/// Appends a short status prompt followed by `IAC GA` (no trailing CRLF —
/// GA is the terminator, matching how MUD servers actually send prompts).
fn push_prompt_ga(out: &mut Vec<u8>, rng: &mut SplitMix64) {
    let hp = 50 + rng.below(950);
    let mana = 10 + rng.below(490);
    let mv = 20 + rng.below(280);
    out.extend_from_slice(format!("<{hp}hp {mana}m {mv}mv> ").as_bytes());
    out.push(IAC);
    out.push(GA);
}

/// What gets spliced into a line at a byte offset. Discriminant order is the
/// tie-break when two inserts share an offset: a literal `IAC IAC` lands
/// before the SGR open, and the SGR close before either at the same offset is
/// impossible (close > open by construction).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Insert {
    LiteralIac,
    SgrOpen,
    SgrClose,
}

/// Dresses one line in the `AnsiLight` shape — one 16-color SGR run over a
/// random word span — optionally splicing a doubled `IAC IAC` (an escaped
/// literal 0xFF) at a word boundary, then terminates it with CRLF.
fn push_light_line(out: &mut Vec<u8>, line: &str, rng: &mut SplitMix64, literal_iac: bool) {
    let bytes = line.as_bytes();
    let words = word_ranges(line);
    if words.is_empty() {
        if literal_iac {
            out.push(IAC);
            out.push(IAC);
        }
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
        return;
    }

    let (open_at, close_at) = words[rng.below(words.len())];
    // 31..=37: the 16-color foregrounds minus black-on-black.
    let color = 31 + rng.below(7);
    let mut inserts = vec![(open_at, Insert::SgrOpen), (close_at, Insert::SgrClose)];
    if literal_iac {
        inserts.push((words[rng.below(words.len())].0, Insert::LiteralIac));
    }
    inserts.sort_unstable();

    let mut cursor = 0;
    for (offset, kind) in inserts {
        out.extend_from_slice(&bytes[cursor..offset]);
        cursor = offset;
        match kind {
            Insert::LiteralIac => {
                out.push(IAC);
                out.push(IAC);
            }
            Insert::SgrOpen => push_sgr(out, &color.to_string()),
            Insert::SgrClose => push_sgr(out, "0"),
        }
    }
    out.extend_from_slice(&bytes[cursor..]);
    out.extend_from_slice(b"\r\n");
}

fn dress_ansi_light(lines: &[String]) -> Vec<u8> {
    let mut rng = SplitMix64::new(SEED_ANSI_LIGHT);
    // ~ +16 bytes/line of dressing; a coarse reserve that avoids most regrowth.
    let mut out = Vec::with_capacity(lines.iter().map(|l| l.len() + 24).sum());
    for (i, line) in lines.iter().enumerate() {
        push_light_line(&mut out, line, &mut rng, false);
        if i % 20 == 19 {
            push_prompt_ga(&mut out, &mut rng);
        }
    }
    out
}

/// One randomly-drawn SGR parameter string in the `AnsiHeavy` mix: 16-color,
/// 256-color, truecolor, bold on/off, or a reset-then-recolor.
fn heavy_sgr_params(rng: &mut SplitMix64) -> String {
    match rng.below(6) {
        0 => (31 + rng.below(7)).to_string(),
        1 => format!("38;5;{}", rng.below(256)),
        2 => format!(
            "38;2;{};{};{}",
            rng.below(256),
            rng.below(256),
            rng.below(256)
        ),
        3 => "1".to_string(),
        4 => "22".to_string(),
        _ => format!("0;{}", 31 + rng.below(7)),
    }
}

/// Dresses one line in the `AnsiHeavy` shape: the words are grouped into 6–10
/// chunks, each opened with a random SGR (some chunks also close with an
/// explicit reset), and the whole line ends with a reset before CRLF.
fn push_heavy_line(out: &mut Vec<u8>, line: &str, rng: &mut SplitMix64) {
    let bytes = line.as_bytes();
    let words = word_ranges(line);
    if words.is_empty() {
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
        return;
    }

    let runs = (6 + rng.below(5)).min(words.len());
    let per_run = words.len().div_ceil(runs);
    let mut cursor = 0;
    for chunk in words.chunks(per_run) {
        let start = chunk[0].0;
        let end = chunk[chunk.len() - 1].1;
        // Inter-chunk whitespace stays unstyled, exactly as it appeared.
        out.extend_from_slice(&bytes[cursor..start]);
        push_sgr(out, &heavy_sgr_params(rng));
        out.extend_from_slice(&bytes[start..end]);
        // A third of the runs reset immediately; the rest let the next SGR
        // override, which is how real servers smear attributes across spans.
        if rng.below(3) == 0 {
            push_sgr(out, "0");
        }
        cursor = end;
    }
    out.extend_from_slice(&bytes[cursor..]);
    push_sgr(out, "0");
    out.extend_from_slice(b"\r\n");
}

fn dress_ansi_heavy(lines: &[String]) -> Vec<u8> {
    let mut rng = SplitMix64::new(SEED_ANSI_HEAVY);
    // 6-10 runs at up to ~19 bytes of escape each: reserve generously.
    let mut out = Vec::with_capacity(lines.iter().map(|l| l.len() + 160).sum());
    for (i, line) in lines.iter().enumerate() {
        push_heavy_line(&mut out, line, &mut rng);
        if i % 10 == 9 {
            push_prompt_ga(&mut out, &mut rng);
        }
    }
    out
}

/// Appends a short GMCP subnegotiation: `IAC SB GMCP <ascii payload> IAC SE`.
/// The payload is pure ASCII (never 0xFF), so no doubling is needed inside
/// the frame.
fn push_subnegotiation(out: &mut Vec<u8>, rng: &mut SplitMix64) {
    out.push(IAC);
    out.push(SB);
    out.push(OPT_GMCP);
    let hp = rng.below(1000);
    let maxhp = 1000 + rng.below(1000);
    out.extend_from_slice(format!("Char.Vitals {{\"hp\":{hp},\"maxhp\":{maxhp}}}").as_bytes());
    out.push(IAC);
    out.push(SE);
}

/// Every ~8th line carries an escaped literal 0xFF (`IAC IAC`) in its text.
const LITERAL_IAC_ODDS: usize = 8;
/// A short subnegotiation is interleaved every this-many lines.
const SUBNEG_EVERY: usize = 25;

fn dress_iac_dense(lines: &[String]) -> Vec<u8> {
    let mut rng = SplitMix64::new(SEED_IAC_DENSE);
    let mut out = Vec::with_capacity(lines.iter().map(|l| l.len() + 32).sum());

    // Opening negotiation burst, as a server sends on connect.
    for &(command, option) in &[
        (WILL, OPT_SGA),
        (WILL, OPT_EOR),
        (DO, OPT_NAWS),
        (WILL, OPT_GMCP),
        (DO, OPT_TTYPE),
        (WONT, OPT_ECHO),
        (DONT, OPT_LINEMODE),
    ] {
        out.push(IAC);
        out.push(command);
        out.push(option);
    }

    for (i, line) in lines.iter().enumerate() {
        let literal_iac = rng.below(LITERAL_IAC_ODDS) == 0;
        push_light_line(&mut out, line, &mut rng, literal_iac);
        out.push(IAC);
        out.push(GA);
        if i % SUBNEG_EVERY == SUBNEG_EVERY - 1 {
            push_subnegotiation(&mut out, &mut rng);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counters extracted by [`scan`]; the per-profile tests assert the
    /// stream's shape through them.
    #[derive(Debug, Default)]
    struct Scan {
        sgr: usize,
        ga: usize,
        literal_iac: usize,
        negotiations: usize,
        subnegotiations: usize,
    }

    /// Walks the stream enforcing the framing invariants the parser relies
    /// on: no bare ESC (every ESC begins a complete `ESC [ params m` SGR that
    /// terminates before end-of-stream), IAC is always followed by a complete
    /// telnet unit, `IAC IAC` doubles are counted as balanced pairs, and
    /// subnegotiations always reach their `IAC SE`.
    fn scan(bytes: &[u8]) -> Scan {
        let mut counts = Scan::default();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                IAC => {
                    let command = *bytes.get(i + 1).expect("IAC must not end the stream");
                    match command {
                        IAC => {
                            counts.literal_iac += 1;
                            i += 2;
                        }
                        GA => {
                            counts.ga += 1;
                            i += 2;
                        }
                        WILL | WONT | DO | DONT => {
                            assert!(i + 2 < bytes.len(), "negotiation missing its option byte");
                            counts.negotiations += 1;
                            i += 3;
                        }
                        SB => {
                            assert!(i + 2 < bytes.len(), "subnegotiation missing its option");
                            let mut j = i + 3;
                            loop {
                                assert!(j < bytes.len(), "subnegotiation never reached IAC SE");
                                if bytes[j] == IAC {
                                    let next = *bytes
                                        .get(j + 1)
                                        .expect("IAC inside subnegotiation must be followed");
                                    if next == SE {
                                        break;
                                    }
                                    assert_eq!(
                                        next, IAC,
                                        "only IAC IAC or IAC SE may follow IAC inside SB"
                                    );
                                    j += 2;
                                } else {
                                    j += 1;
                                }
                            }
                            counts.subnegotiations += 1;
                            i = j + 2;
                        }
                        other => panic!("IAC followed by unexpected command byte {other}"),
                    }
                }
                ESC => {
                    assert_eq!(
                        bytes.get(i + 1),
                        Some(&b'['),
                        "ESC must open a CSI sequence"
                    );
                    let mut j = i + 2;
                    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                        j += 1;
                    }
                    assert_eq!(
                        bytes.get(j),
                        Some(&b'm'),
                        "SGR must terminate with 'm' before end of stream"
                    );
                    counts.sgr += 1;
                    i = j + 1;
                }
                _ => i += 1,
            }
        }
        counts
    }

    /// A small mixed corpus: ordinary prose, an empty line, and multi-byte
    /// UTF-8 (the splice points must respect char boundaries).
    fn sample_lines() -> Vec<String> {
        let mut lines: Vec<String> = (0..100)
            .map(|i| format!("You hit the training dummy number {i} very hard."))
            .collect();
        lines[13] = String::new();
        lines[42] = "Le café — déjà vu, naïveté, größer, 日本語 text".to_string();
        lines[77] = "   leading and trailing whitespace   ".to_string();
        lines
    }

    #[test]
    fn ansi_light_is_valid_and_shaped_right() {
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::AnsiLight);
        let counts = scan(&bytes);
        assert_eq!(counts.ga, lines.len() / 20, "one prompt+GA per 20 lines");
        assert_eq!(counts.literal_iac, 0);
        assert_eq!(counts.negotiations, 0);
        assert_eq!(counts.subnegotiations, 0);
        // One open + one close per non-empty line.
        assert!(counts.sgr >= 2 * (lines.len() - 1), "sgr = {}", counts.sgr);
    }

    #[test]
    fn ansi_heavy_is_valid_and_shaped_right() {
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::AnsiHeavy);
        let counts = scan(&bytes);
        assert_eq!(counts.ga, lines.len() / 10, "one prompt+GA per 10 lines");
        assert_eq!(counts.literal_iac, 0);
        // Every styled line carries several runs plus its trailing reset (a
        // short line can cap runs at its word count, hence >= 6, not >= 7).
        assert!(counts.sgr >= 6 * (lines.len() - 1), "sgr = {}", counts.sgr);
    }

    #[test]
    fn iac_dense_is_valid_and_shaped_right() {
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::IacDense);
        let counts = scan(&bytes);
        assert_eq!(counts.ga, lines.len(), "IAC GA after every line");
        assert_eq!(counts.negotiations, 7, "the opening burst");
        assert_eq!(counts.subnegotiations, lines.len() / SUBNEG_EVERY);
        assert!(
            counts.literal_iac > 0,
            "the seed must produce at least one escaped literal IAC"
        );
    }

    #[test]
    fn output_is_deterministic_across_calls() {
        let lines = sample_lines();
        for profile in [
            WireProfile::AnsiLight,
            WireProfile::AnsiHeavy,
            WireProfile::IacDense,
        ] {
            let first = dress_lines(&lines, profile);
            let second = dress_lines(&lines, profile);
            assert_eq!(first, second, "{profile:?} must be byte-identical");
        }
    }

    #[test]
    fn stripped_text_survives_dressing() {
        // Removing every escape/telnet byte must give back the original text:
        // proof the dresser only *adds* framing and never corrupts content.
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::AnsiLight);
        let mut stripped = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                IAC => i += 2, // GA in this profile; commands validated by scan()
                ESC => {
                    i += 2;
                    while bytes[i] != b'm' {
                        i += 1;
                    }
                    i += 1;
                }
                b => {
                    stripped.push(b);
                    i += 1;
                }
            }
        }
        let text = String::from_utf8(stripped).expect("stripped stream is valid UTF-8");
        for line in &lines {
            assert!(text.contains(line.as_str()), "line lost: {line:?}");
        }
    }

    #[test]
    fn chunk_reassembles_exactly() {
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::AnsiHeavy);
        let chunks = chunk(&bytes, 16 * 1024);
        assert!(!chunks.is_empty());
        assert!(
            chunks[..chunks.len() - 1]
                .iter()
                .all(|c| c.len() == 16 * 1024)
        );
        let reassembled: Vec<u8> = chunks.concat();
        assert_eq!(reassembled, bytes);
    }
}
