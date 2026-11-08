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
    /// [`WireProfile::AnsiLight`] styling plus a chunked kitty-graphics
    /// upload (`ESC _ G … ESC \`, 4 KiB base64 chunks) at the head of every
    /// [`GRAPHICS_EVERY`]th line. Stresses the APC path: every payload byte
    /// takes the per-byte route, and the line count is unchanged, so the
    /// profile isolates the cost of image traffic riding a text stream.
    AnsiGraphics,
    /// [`WireProfile::AnsiGraphics`] with uploads a displaying connection
    /// stores and places: each image starts with a PNG header declaring
    /// [`GRAPHICS_FULL_DIMENSION`] square pixels, carries a fresh image id,
    /// and asks for its `OK` reply. Same lines, sizes, chunking and seed as
    /// `AnsiGraphics` (the bytes differ only in the header and control
    /// keys), so the two compare the parse-and-drop path with the whole
    /// state machine: records, placements, quota eviction and replies.
    AnsiGraphicsFull,
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
        WireProfile::AnsiGraphics => dress_ansi_graphics(lines, Uploads::Dropped),
        WireProfile::AnsiGraphicsFull => dress_ansi_graphics(lines, Uploads::Stored),
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
const SEED_ANSI_GRAPHICS: u64 = 0x5EED_0000_0000_0004;

/// Lines per graphics upload in [`WireProfile::AnsiGraphics`].
pub const GRAPHICS_EVERY: usize = 50;
/// Decoded bytes per upload: a modest PNG's worth, which base64 and the
/// 4 KiB chunk rule turn into 16 chunks.
pub const GRAPHICS_IMAGE_BYTES: usize = 48 * 1024;
/// The protocol's chunk ceiling for a base64 payload.
pub const GRAPHICS_CHUNK_PAYLOAD: usize = 4096;
/// The width and height the [`WireProfile::AnsiGraphicsFull`] PNG headers
/// declare: 64 KiB of RGBA once decoded, a plausible size for a 48 KiB PNG,
/// so the default 64 MiB quota holds 1024 of them and a corpus with more
/// uploads keeps evicting.
pub const GRAPHICS_FULL_DIMENSION: u32 = 128;
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

/// What the uploads of a graphics profile are for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uploads {
    /// [`WireProfile::AnsiGraphics`]: one id, replies suppressed, no PNG
    /// header; a displaying connection refuses every upload at its probe.
    Dropped,
    /// [`WireProfile::AnsiGraphicsFull`]: a PNG header, a fresh id each,
    /// replies requested.
    Stored,
}

fn dress_ansi_graphics(lines: &[String], uploads_are: Uploads) -> Vec<u8> {
    let mut rng = SplitMix64::new(SEED_ANSI_GRAPHICS);
    let uploads = lines.len() / GRAPHICS_EVERY;
    let mut out =
        Vec::with_capacity(lines.iter().map(|l| l.len() + 24).sum::<usize>() + uploads * 72 * 1024);
    let mut upload = 0_u32;
    for (i, line) in lines.iter().enumerate() {
        if i % GRAPHICS_EVERY == GRAPHICS_EVERY - 1 {
            upload += 1;
            push_graphics_upload(&mut out, &mut rng, uploads_are, upload);
        }
        push_light_line(&mut out, line, &mut rng, false);
        if i % 20 == 19 {
            push_prompt_ga(&mut out, &mut rng);
        }
    }
    out
}

/// Standard base64 with padding, inlined so the bench support layer adds no
/// dependency for a few dozen bytes of code.
fn base64_encode(bytes: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let b0 = group[0];
        let b1 = group.get(1).copied().unwrap_or(0);
        let b2 = group.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[usize::from(b0 >> 2)]);
        out.push(ALPHABET[usize::from(((b0 & 0b11) << 4) | (b1 >> 4))]);
        out.push(if group.len() > 1 {
            ALPHABET[usize::from(((b1 & 0b1111) << 2) | (b2 >> 6))]
        } else {
            b'='
        });
        out.push(if group.len() > 2 {
            ALPHABET[usize::from(b2 & 0b11_1111)]
        } else {
            b'='
        });
    }
    out
}

/// The first 24 bytes of a PNG file declaring `width` x `height` RGBA
/// pixels: the signature and the start of the IHDR chunk, which is all the
/// protocol's probe reads. The rest of the chunk (and its CRC) is left to
/// the random bytes after it; nothing in the bench decodes them.
fn png_header(width: u32, height: u32) -> [u8; 24] {
    let mut header = [0_u8; 24];
    header[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    header[8..12].copy_from_slice(&13_u32.to_be_bytes());
    header[12..16].copy_from_slice(b"IHDR");
    header[16..20].copy_from_slice(&width.to_be_bytes());
    header[20..24].copy_from_slice(&height.to_be_bytes());
    header
}

/// One chunked `a=T,f=100` upload of [`GRAPHICS_IMAGE_BYTES`] pseudo-random
/// bytes, split per the protocol: 4 KiB base64 chunks, `m=1` on all but the
/// last. The bytes are not a real PNG — nothing decodes them, and the bench
/// measures the wire path and the state machine, not the codec. A stored
/// upload (`upload`, from 1, is its image id) starts with a PNG header so a
/// displaying connection admits it.
fn push_graphics_upload(
    out: &mut Vec<u8>,
    rng: &mut SplitMix64,
    uploads_are: Uploads,
    upload: u32,
) {
    let mut image = Vec::with_capacity(GRAPHICS_IMAGE_BYTES);
    while image.len() < GRAPHICS_IMAGE_BYTES {
        image.extend_from_slice(&rng.next_u64().to_le_bytes());
    }
    image.truncate(GRAPHICS_IMAGE_BYTES);
    if uploads_are == Uploads::Stored {
        image[..24].copy_from_slice(&png_header(
            GRAPHICS_FULL_DIMENSION,
            GRAPHICS_FULL_DIMENSION,
        ));
    }
    let encoded = base64_encode(&image);
    let chunks: Vec<&[u8]> = encoded.chunks(GRAPHICS_CHUNK_PAYLOAD).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let last = index + 1 == chunks.len();
        out.extend_from_slice(b"\x1b_G");
        if index == 0 {
            match uploads_are {
                Uploads::Dropped => out.extend_from_slice(b"a=T,f=100,i=1,q=2,"),
                Uploads::Stored => {
                    out.extend_from_slice(format!("a=T,f=100,i={upload},").as_bytes());
                }
            }
        }
        out.extend_from_slice(if last { b"m=0;" } else { b"m=1;" });
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
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
        apc: usize,
    }

    /// Walks the stream enforcing the framing invariants the parser relies
    /// on: no bare ESC (every ESC begins a complete `ESC [ params m` SGR, or
    /// an `ESC _ G … ESC \` graphics chunk within the protocol's payload
    /// ceiling, terminating before end-of-stream), IAC is always followed by
    /// a complete telnet unit, `IAC IAC` doubles are counted as balanced
    /// pairs, and subnegotiations always reach their `IAC SE`.
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
                ESC if bytes.get(i + 1) == Some(&b'_') => {
                    assert_eq!(
                        bytes.get(i + 2),
                        Some(&b'G'),
                        "only graphics APCs are emitted"
                    );
                    let mut j = i + 3;
                    while bytes.get(j) != Some(&ESC) {
                        assert!(j < bytes.len(), "APC never reached ESC \\");
                        j += 1;
                    }
                    assert_eq!(bytes.get(j + 1), Some(&b'\\'), "APC must end with ESC \\");
                    let body = &bytes[i + 3..j];
                    let payload = body
                        .iter()
                        .position(|&b| b == b';')
                        .map_or(0, |at| body.len() - at - 1);
                    assert!(
                        payload <= GRAPHICS_CHUNK_PAYLOAD,
                        "chunk payload {payload} exceeds the protocol ceiling"
                    );
                    counts.apc += 1;
                    i = j + 2;
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
    fn ansi_graphics_is_valid_and_shaped_right() {
        let lines = sample_lines();
        let bytes = dress_lines(&lines, WireProfile::AnsiGraphics);
        let counts = scan(&bytes);
        let uploads = lines.len() / GRAPHICS_EVERY;
        let chunks_per_upload =
            (GRAPHICS_IMAGE_BYTES.div_ceil(3) * 4).div_ceil(GRAPHICS_CHUNK_PAYLOAD);
        assert_eq!(uploads, 2, "the sample corpus carries two uploads");
        assert_eq!(
            counts.apc,
            uploads * chunks_per_upload,
            "every upload is fully chunked"
        );
        assert_eq!(counts.ga, lines.len() / 20, "prompts as in AnsiLight");
        assert_eq!(counts.literal_iac, 0);
        assert_eq!(counts.negotiations, 0);
        assert!(counts.sgr >= 2 * (lines.len() - 1), "sgr = {}", counts.sgr);
        // Uploads ride existing lines: no extra line terminators.
        let crlfs = bytes.windows(2).filter(|w| w == b"\r\n").count();
        let plain = dress_lines(&lines, WireProfile::AnsiLight);
        let plain_crlfs = plain.windows(2).filter(|w| w == b"\r\n").count();
        assert_eq!(crlfs, plain_crlfs);
        assert_eq!(base64_encode(b"Man"), b"TWFu");
        assert_eq!(base64_encode(b"Ma"), b"TWE=");
        assert_eq!(base64_encode(b"M"), b"TQ==");
    }

    #[test]
    fn ansi_graphics_full_differs_only_in_headers_and_keys() {
        let lines = sample_lines();
        let full = dress_lines(&lines, WireProfile::AnsiGraphicsFull);
        let dropped = dress_lines(&lines, WireProfile::AnsiGraphics);
        let counts = scan(&full);
        assert_eq!(counts.apc, scan(&dropped).apc, "the same chunking");
        assert_eq!(counts.ga, lines.len() / 20);
        // A fresh id per upload, replies requested, a PNG header leading
        // each image.
        let header = base64_encode(&png_header(
            GRAPHICS_FULL_DIMENSION,
            GRAPHICS_FULL_DIMENSION,
        ));
        for (upload, first) in [
            (1, "\x1b_Ga=T,f=100,i=1,m=1;"),
            (2, "\x1b_Ga=T,f=100,i=2,m=1;"),
        ] {
            let at = full
                .windows(first.len())
                .position(|w| w == first.as_bytes())
                .unwrap_or_else(|| panic!("upload {upload} starts with {first:?}"));
            let payload = &full[at + first.len()..];
            assert_eq!(&payload[..header.len()], header.as_slice());
        }
        assert!(!full.windows(3).any(|w| w == b"q=2"));
        // Everything else is the same stream: removing the APCs leaves the
        // same text.
        let strip = |bytes: &[u8]| {
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == ESC && bytes.get(i + 1) == Some(&b'_') {
                    while !(bytes[i] == ESC && bytes.get(i + 1) == Some(&b'\\')) {
                        i += 1;
                    }
                    i += 2;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            out
        };
        assert_eq!(strip(&full), strip(&dropped));
    }

    #[test]
    fn output_is_deterministic_across_calls() {
        let lines = sample_lines();
        for profile in [
            WireProfile::AnsiLight,
            WireProfile::AnsiHeavy,
            WireProfile::IacDense,
            WireProfile::AnsiGraphics,
            WireProfile::AnsiGraphicsFull,
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
