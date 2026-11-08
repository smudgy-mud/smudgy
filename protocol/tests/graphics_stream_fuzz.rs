//! A seeded fuzz of the kitty graphics protocol through its public API.
//!
//! Each seed generates a byte stream mixing plain and Unicode text (Unicode
//! placeholders with combining marks and the SGR colours that address
//! them), line ends, SGR, OSC 8 links, random CSI (terminal queries
//! included), other escape strings, garbage bytes and graphics APCs with
//! random actions, keys, values and payloads — valid-looking and not:
//! chunked uploads that finish, interleave or never finish, huge sizes,
//! wrong lengths, zlib, PNG headers, local-media names, deletes of every
//! target, relative and virtual placements, feature queries, animations
//! (frames, a frame composed onto another, playback control, a frame
//! deleted). The stream is cut into random feeds, between which the harness
//! ends batches, commits prompts, toggles raw capture, changes the geometry,
//! switches the graphics mode and completes decodes — at once, later, out
//! of order, with errors, with the wrong size, twice, or, as a decoder that
//! loses the request, with the failure its completion guard reports.
//!
//! What must hold: nothing panics; every reply is well formed (single
//! keys, each once, an image named first, a status kitty's clients parse)
//! and no command is answered twice; a drain leaves nothing ready behind;
//! every emitted line keeps its objects within the inline-object cap and on
//! its text's character boundaries; raw text never carries a graphics APC;
//! and once every decode has landed, a terminal query is answered at once
//! (nothing stays parked). A second harness drives [`GraphicsState`] directly with
//! a small quota, where its accounting is visible: the quota and the image
//! cap are never exceeded, decodes in flight are exactly those not yet
//! completed, and addressable images are live and sized as declared.
//!
//! `GRAPHICS_FUZZ_SEEDS` (default 2000) and `GRAPHICS_FUZZ_FIRST_SEED`
//! (default 0) select the seeds, for longer runs (`--release` helps, but
//! turns the processor's `debug_assert`s off) and for replaying a failure,
//! whose seed the panic names. A failing processor seed shrinks to a short
//! list of steps with the ignored `minimize_a_failing_seed`.
//!
//! Two bugs this fuzz found (a batch ending in a character whose last
//! UTF-8 byte has a C1 value split it in the raw capture, and vtparse
//! swallowed an `ESC` that cut a UTF-8 sequence short) are fixed, and
//! pinned by the tests at the end with the bytes that found them.

use std::collections::VecDeque;
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use smudgy_protocol::graphics::{
    CELL_FIXED_SCALE, DecodeRequest, DecodeTicket, DecodedPixels, GraphicsEnv, GraphicsError,
    GraphicsHost, GraphicsMode, GraphicsState, ImageData, MAX_IMAGES, MAX_PLACEMENT_CELLS,
    MAX_PLACEMENT_ROWS, Medium, Placement, ReplyQueue, encode_payload, parse_apc,
};
use smudgy_protocol::responders::pack_dims;
use smudgy_protocol::vt::{VtEvent, VtProcessor, VtSink};
use smudgy_session_model::StyledLine;
use smudgy_session_model::inline_content::MAX_INLINE_OBJECTS;
use vtparse::VTParser;

// ---------------------------------------------------------------- PRNG --

/// xorshift64*: small, deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Spread small seeds apart; xorshift's state must not be zero.
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn index(&mut self, n: usize) -> usize {
        usize::try_from(self.below(n as u64)).expect("below a usize")
    }

    fn range(&mut self, low: u32, high: u32) -> u32 {
        low + u32::try_from(self.below(u64::from(high - low) + 1)).expect("within u32")
    }

    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[7]
    }

    fn word(&mut self) -> u32 {
        let [a, b, c, d, ..] = self.next().to_be_bytes();
        u32::from_be_bytes([a, b, c, d])
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }
}

// ----------------------------------------------------------- generation --

/// How a graphics APC ends on the wire.
#[derive(Clone, Copy, Debug)]
enum Term {
    /// `ESC \`.
    St,
    /// UTF-8 encoded C1 ST.
    C1St,
    /// A raw 0x9C byte.
    RawC1St,
    /// BEL, which does not end an APC.
    Bel,
    /// CAN, which aborts it.
    Can,
    /// Nothing: the next bytes run on inside the string.
    None,
}

/// One graphics APC: its body (`G` control `;` payload) and terminator.
struct Apc {
    body: Vec<u8>,
    term: Term,
}

fn term(rng: &mut Rng) -> Term {
    match rng.below(100) {
        0..=91 => Term::St,
        92..=93 => Term::C1St,
        94 => Term::RawC1St,
        95 => Term::Bel,
        96..=97 => Term::Can,
        _ => Term::None,
    }
}

fn push_apc(out: &mut Vec<u8>, apc: &Apc) {
    out.extend_from_slice(b"\x1b_");
    out.extend_from_slice(&apc.body);
    match apc.term {
        Term::St => out.extend_from_slice(b"\x1b\\"),
        Term::C1St => out.extend_from_slice("\u{9c}".as_bytes()),
        Term::RawC1St => out.push(0x9c),
        Term::Bel => out.push(0x07),
        Term::Can => out.push(0x18),
        Term::None => {}
    }
}

/// A small id from a space small enough for commands to meet each other.
fn id(rng: &mut Rng) -> u32 {
    match rng.below(20) {
        0 => 0,
        1 => u32::MAX,
        2 => rng.word(),
        _ => rng.range(1, 8),
    }
}

/// A pixel dimension: mostly small, sometimes at or past the limits.
fn dim(rng: &mut Rng) -> u32 {
    match rng.below(40) {
        0 => 0,
        1 => 10_000,
        2 => 10_001,
        3 => 4096,
        4 => u32::MAX,
        5 => rng.range(17, 300),
        _ => rng.range(1, 16),
    }
}

fn signed(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => i32::MIN.to_string(),
        1 => i32::MAX.to_string(),
        2 => "-1073741825".to_owned(),
        _ => (i64::from(rng.range(0, 20)) - 10).to_string(),
    }
}

fn small(rng: &mut Rng) -> String {
    match rng.below(40) {
        0 => "0".to_owned(),
        1 => u32::MAX.to_string(),
        2 => "4294967296".to_owned(),
        3 => "-3".to_owned(),
        4 => "x".to_owned(),
        5 => String::new(),
        _ => rng.range(1, 40).to_string(),
    }
}

/// Control data: `key=value` items joined by commas, sometimes with an
/// empty item, a duplicate, an unknown key or a key with no value.
fn control(rng: &mut Rng, items: &[(char, String)]) -> String {
    let mut parts: Vec<String> = items.iter().map(|(k, v)| format!("{k}={v}")).collect();
    if rng.chance(1, 20) {
        parts.push(String::new());
    }
    if rng.chance(1, 25) {
        let key = *rng.pick(&['b', 'g', 'j', 'k', 'l', 'n', 'u', 'A', 'B', 'D', 'E']);
        parts.push(format!("{key}={}", small(rng)));
    }
    if rng.chance(1, 40) {
        parts.push("a".to_owned());
    }
    if rng.chance(1, 40) && !parts.is_empty() {
        let dup = parts[rng.index(parts.len())].clone();
        parts.push(dup);
    }
    if parts.len() > 1 && rng.chance(1, 10) {
        let at = rng.index(parts.len());
        parts.swap(at, 0);
    }
    parts.join(",")
}

/// Keys any placing command may carry.
fn placement_keys(rng: &mut Rng, items: &mut Vec<(char, String)>) {
    for (key, odds) in [
        ('p', 3),
        ('c', 5),
        ('r', 5),
        ('x', 8),
        ('y', 8),
        ('w', 8),
        ('h', 8),
        ('X', 10),
        ('Y', 10),
        ('C', 6),
        ('q', 6),
    ] {
        if rng.chance(1, odds) {
            let value = match key {
                'p' => id(rng).to_string(),
                'C' => rng.range(0, 2).to_string(),
                'q' => rng.range(0, 3).to_string(),
                _ => small(rng),
            };
            items.push((key, value));
        }
    }
    if rng.chance(1, 5) {
        items.push(('z', signed(rng)));
    }
}

fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    header
}

fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("in-memory zlib");
    encoder.finish().expect("in-memory zlib")
}

/// Base64 that is not quite: random alphabet characters, padding in odd
/// places, characters outside the alphabet.
fn garbage_base64(rng: &mut Rng) -> Vec<u8> {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=-_ !*\n";
    let len = rng.index(80);
    (0..len).map(|_| *rng.pick(ALPHABET)).collect()
}

/// A local-media name: ones the fake filesystem serves, and not.
fn media_name(rng: &mut Rng) -> Vec<u8> {
    let names: [&[u8]; 9] = [
        b"/tmp/tty-graphics-protocol-fuzz.rgb",
        b"/dev/shm/fuzz-png",
        b"fuzz-shm",
        b"relative.png",
        b"/etc/passwd",
        b"/proc/self/environ",
        b"",
        b"/tmp/\xff\xfe.png",
        b"/a/very/../odd/./path",
    ];
    if rng.chance(1, 10) {
        let len = rng.index(40);
        rng.bytes(len)
    } else {
        rng.pick(&names).to_vec()
    }
}

/// A transmit, query or frame: direct (maybe chunked) or local media.
#[allow(clippy::too_many_lines)] // One generator: its choices read best in one place.
fn transmit(rng: &mut Rng) -> Vec<Apc> {
    let action = *rng.pick(&["t", "T", "T", "q", "", "f"]);
    let format = *rng.pick(&[24_u32, 24, 32, 32, 100, 100, 0, 16]);
    let (width, height) = (dim(rng), dim(rng));
    let mut items: Vec<(char, String)> = Vec::new();
    if !action.is_empty() {
        items.push(('a', action.to_owned()));
    }
    if format != 0 || rng.chance(1, 2) {
        items.push(('f', format.to_string()));
    }
    if rng.chance(9, 10) {
        items.push(('s', width.to_string()));
        items.push(('v', height.to_string()));
    }
    match rng.below(10) {
        0 => items.push(('I', id(rng).to_string())),
        1 => {}
        _ => items.push(('i', id(rng).to_string())),
    }
    placement_keys(rng, &mut items);
    if action == "T" && rng.chance(1, 6) {
        items.push(('U', "1".to_owned()));
    }
    if action == "f" {
        for key in ['r', 'c', 'z'] {
            if rng.chance(1, 3) {
                items.push((key, small(rng)));
            }
        }
    }

    // Local media: the payload is a name.
    if rng.chance(1, 8) {
        items.push(('t', (*rng.pick(&["f", "t", "s", "x"])).to_owned()));
        if rng.chance(1, 3) {
            items.push(('S', small(rng)));
        }
        if rng.chance(1, 3) {
            items.push(('O', small(rng)));
        }
        if rng.chance(1, 6) {
            items.push(('m', "1".to_owned()));
        }
        let name = media_name(rng);
        let body = format!("G{};{}", control(rng, &items), encode_payload(&name));
        return vec![Apc {
            body: body.into_bytes(),
            term: term(rng),
        }];
    }
    if rng.chance(1, 30) {
        items.push(('t', "d".to_owned()));
    }

    // The pixels, as declared or not.
    let bytes_per_pixel = if format == 24 { 3 } else { 4 };
    let exact = (u64::from(width.min(64)) * u64::from(height.min(64)) * bytes_per_pixel)
        .min(16 * 1024) as usize;
    // Packed pixels are one repeated byte: nothing decodes them, and a
    // byte at a time from the generator is slow in a debug build.
    let fill = |len: usize, rng: &mut Rng| vec![rng.byte(); len];
    let mut pixels = match rng.below(10) {
        0..=5 if format == 100 => {
            let mut png = png_header(width, height);
            let tail = rng.index(200);
            png.extend(rng.bytes(tail));
            png
        }
        0..=5 => fill(exact, rng),
        6 => {
            let delta = rng.index(8);
            if rng.chance(1, 2) {
                fill(exact + delta, rng)
            } else {
                fill(exact.saturating_sub(delta + 1), rng)
            }
        }
        7 => Vec::new(),
        _ => {
            let len = rng.index(600);
            rng.bytes(len)
        }
    };
    if rng.chance(1, 6) {
        items.push(('o', "z".to_owned()));
        if rng.chance(4, 5) {
            pixels = zlib(&pixels);
        }
    }
    let payload = if rng.chance(1, 10) {
        garbage_base64(rng)
    } else {
        encode_payload(&pixels).into_bytes()
    };

    // One command, or chunks of it.
    if payload.len() <= 4096 && rng.chance(2, 3) {
        if rng.chance(1, 20) {
            items.push(('m', "0".to_owned()));
        }
        let mut body = format!("G{};", control(rng, &items)).into_bytes();
        body.extend_from_slice(&payload);
        return vec![Apc {
            body,
            term: term(rng),
        }];
    }
    let mut apcs = Vec::new();
    let mut rest = &payload[..];
    let mut first = true;
    loop {
        let size = if rng.chance(9, 10) {
            4 * rng.range(1, 1024) as usize
        } else {
            rng.range(1, 5000) as usize
        };
        let (chunk, tail) = rest.split_at(size.min(rest.len()));
        rest = tail;
        let last = rest.is_empty();
        let mut keys = if first {
            items.clone()
        } else {
            let mut keys = Vec::new();
            if rng.chance(1, 8) {
                keys.push(('q', rng.range(0, 3).to_string()));
            }
            if action == "f" || rng.chance(1, 20) {
                keys.push(('a', action.to_owned()));
            }
            if rng.chance(1, 30) {
                keys.push(('i', id(rng).to_string()));
            }
            keys
        };
        keys.push(('m', if last { "0" } else { "1" }.to_owned()));
        let mut body = format!("G{};", control(rng, &keys)).into_bytes();
        body.extend_from_slice(chunk);
        apcs.push(Apc {
            body,
            term: term(rng),
        });
        first = false;
        // Some uploads never finish.
        if last || rng.chance(1, 25) {
            break;
        }
    }
    apcs
}

/// `a=p`: by id or number, plain, virtual (`U=1`) or relative (`P`, `Q`,
/// `H`, `V`).
fn put(rng: &mut Rng) -> Apc {
    let mut items = vec![('a', "p".to_owned())];
    if rng.chance(1, 6) {
        items.push(('I', id(rng).to_string()));
    } else {
        items.push(('i', id(rng).to_string()));
    }
    placement_keys(rng, &mut items);
    if rng.chance(1, 3) {
        items.push(('U', (*rng.pick(&["1", "1", "0", "2"])).to_owned()));
    }
    if rng.chance(1, 3) {
        items.push(('P', id(rng).to_string()));
        if rng.chance(3, 4) {
            items.push(('Q', id(rng).to_string()));
        }
        if rng.chance(1, 2) {
            items.push(('H', signed(rng)));
        }
        if rng.chance(1, 2) {
            items.push(('V', signed(rng)));
        }
    }
    Apc {
        body: format!("G{}", control(rng, &items)).into_bytes(),
        term: term(rng),
    }
}

/// `a=d` with every target, upper and lower case.
fn delete(rng: &mut Rng) -> Apc {
    let mut items = vec![('a', "d".to_owned())];
    if rng.chance(9, 10) {
        let target = *rng.pick(&[
            "a", "A", "i", "I", "n", "N", "c", "C", "p", "P", "q", "Q", "r", "R", "x", "X", "y",
            "Y", "z", "Z", "f", "F", "b", "",
        ]);
        items.push(('d', target.to_owned()));
    }
    for (key, odds) in [
        ('i', 2),
        ('I', 6),
        ('p', 4),
        ('x', 4),
        ('y', 4),
        ('z', 6),
        ('q', 6),
    ] {
        if rng.chance(1, odds) {
            let value = match key {
                'i' | 'I' | 'p' => id(rng).to_string(),
                'z' => signed(rng),
                'q' => rng.range(0, 3).to_string(),
                _ => small(rng),
            };
            items.push((key, value));
        }
    }
    Apc {
        body: format!("G{}", control(rng, &items)).into_bytes(),
        term: term(rng),
    }
}

/// `a=q,e=…`: what this terminal supports of an action.
fn feature_query(rng: &mut Rng) -> Apc {
    let mut items = vec![('a', "q".to_owned())];
    if rng.chance(5, 6) {
        items.push(('i', id(rng).to_string()));
    }
    items.push((
        'e',
        (*rng.pick(&["t", "T", "q", "p", "d", "f", "a", "c", "x", ""])).to_owned(),
    ));
    for key in [
        's', 'v', 'f', 't', 'o', 'U', 'C', 'P', 'Q', 'z', 'r', 'X', 'c',
    ] {
        if rng.chance(1, 5) {
            let value = match key {
                't' => (*rng.pick(&["d", "f", "t", "s"])).to_owned(),
                'o' => "z".to_owned(),
                'f' => (*rng.pick(&["24", "32", "100"])).to_owned(),
                'z' => signed(rng),
                _ => small(rng),
            };
            items.push((key, value));
        }
    }
    Apc {
        body: format!("G{}", control(rng, &items)).into_bytes(),
        term: term(rng),
    }
}

/// `a=a` and `a=c`, and random control data of any shape.
fn other_command(rng: &mut Rng) -> Apc {
    let body = match rng.below(3) {
        0 => {
            let mut items = vec![('a', (*rng.pick(&["a", "c"])).to_owned())];
            for key in [
                'i', 'r', 'c', 's', 'v', 'z', 'x', 'y', 'w', 'h', 'X', 'Y', 'C',
            ] {
                if rng.chance(1, 3) {
                    items.push((key, small(rng)));
                }
            }
            format!("G{}", control(rng, &items)).into_bytes()
        }
        1 => {
            let count = rng.index(8);
            let mut items = Vec::new();
            for _ in 0..count {
                let key = char::from(b'A' + rng.byte() % 58);
                items.push((key, small(rng)));
            }
            let mut body = format!("G{}", control(rng, &items)).into_bytes();
            if rng.chance(1, 2) {
                body.push(b';');
                body.extend(garbage_base64(rng));
            }
            body
        }
        _ => {
            // A continuation chunk with no upload, or raw bytes after G.
            if rng.chance(1, 2) {
                format!("Gm={};{}", rng.range(0, 1), encode_payload(&rng.bytes(12))).into_bytes()
            } else {
                // A SUB or C1 byte among them can end the string early and
                // leave the rest to Ground, a high byte last among those
                // cutting a UTF-8 sequence short before the terminator.
                let len = rng.index(30);
                let mut body = b"G".to_vec();
                body.extend(
                    rng.bytes(len)
                        .into_iter()
                        .filter(|&b| b != 0x1b && b != 0x18),
                );
                body
            }
        }
    };
    Apc {
        body,
        term: term(rng),
    }
}

/// A virtual placement a scenario made, for placeholder text to show.
#[derive(Clone, Copy)]
struct Virtual {
    image: u32,
    placement: u32,
    columns: u32,
    rows: u32,
}

/// A coherent sequence, so that the deeper states are reached: a valid
/// image, a placement of it (plain, `C=1` or under the text), relative
/// placements hanging from that, a virtual placement, and now and then a
/// delete aimed at what was made.
fn scenario(rng: &mut Rng) -> (Vec<Apc>, Option<Virtual>) {
    let image = rng.range(1, 8);
    let (width, height) = (rng.range(1, 24), rng.range(1, 24));
    let format = *rng.pick(&[24_u32, 32]);
    let bytes_per_pixel = if format == 24 { 3 } else { 4 };
    let pixels = vec![0x5a_u8; (width * height * bytes_per_pixel) as usize];
    let mut apcs = Vec::new();
    let mut apc = |body: String, rng: &mut Rng| {
        apcs.push(Apc {
            body: body.into_bytes(),
            term: if rng.chance(19, 20) {
                Term::St
            } else {
                term(rng)
            },
        });
    };
    let quiet = *rng.pick(&["", "", ",q=1", ",q=2"]);
    let action = *rng.pick(&["t", "T"]);
    apc(
        format!(
            "Ga={action},i={image},f={format},s={width},v={height}{quiet};{}",
            encode_payload(&pixels)
        ),
        rng,
    );
    let parent = rng.range(1, 4);
    let style = *rng.pick(&["", "", ",C=1", ",z=-1", ",z=5", ",c=3,r=2"]);
    apc(format!("Ga=p,i={image},p={parent}{style}"), rng);
    for child in 0..rng.range(0, 3) {
        let of = if rng.chance(3, 4) {
            image
        } else {
            rng.range(1, 8)
        };
        apc(
            format!(
                "Ga=p,i={of},p={},P={image},Q={parent},H={},V={}",
                5 + child,
                signed(rng),
                signed(rng)
            ),
            rng,
        );
    }
    let mut made = None;
    if rng.chance(1, 2) {
        let placement = if rng.chance(1, 2) { 0 } else { rng.range(1, 7) };
        let (columns, rows) = (rng.range(1, 6), rng.range(1, 3));
        let id = if placement == 0 {
            String::new()
        } else {
            format!(",p={placement}")
        };
        apc(format!("Ga=p,U=1,i={image}{id},c={columns},r={rows}"), rng);
        made = Some(Virtual {
            image,
            placement,
            columns,
            rows,
        });
        if rng.chance(1, 3) {
            apc(
                format!("Ga=p,i={image},p=9,P={image},Q={placement},H=1,V=0"),
                rng,
            );
        }
    }
    // An animation: two or three frames of the image's size, each composed
    // on the first (`c=1`) with a gap now and then, a frame composed onto
    // another, playback started, and now and then the current frame set
    // or a frame deleted. Queued behind the image's decode, these run as
    // the harness settles it.
    if rng.chance(1, 2) {
        for _ in 0..rng.range(2, 3) {
            let keys = *rng.pick(&["", ",z=40", ",z=-1", ",X=1", ",r=1"]);
            apc(
                format!(
                    "Ga=f,i={image},f={format},s={width},v={height},c=1{keys}{quiet};{}",
                    encode_payload(&pixels)
                ),
                rng,
            );
        }
        let rect = if rng.chance(1, 2) {
            String::new()
        } else {
            format!(",w={},h={},X=0,Y=0,x=0,y=0", width.div_ceil(2), height)
        };
        apc(format!("Ga=c,i={image},r=1,c=2{rect}{quiet}"), rng);
        apc(format!("Ga=a,i={image},s=3,v={}", rng.range(0, 3)), rng);
        if rng.chance(1, 3) {
            apc(format!("Ga=a,i={image},c=2"), rng);
        }
        if rng.chance(1, 3) {
            apc(format!("Ga=d,d=f,i={image},r=2"), rng);
        }
    }
    if rng.chance(1, 4) {
        let target = *rng.pick(&["i", "I", "a", "A", "p", "c", "z", "Z", "r", "x", "y"]);
        apc(
            format!("Ga=d,d={target},i={image},p={parent},x=1,y=1,z=-1"),
            rng,
        );
    }
    (apcs, made)
}

/// One graphics command (several APCs when chunked or a scenario), and a
/// virtual placement it made, if any.
fn graphics(rng: &mut Rng) -> (Vec<Apc>, Option<Virtual>) {
    let apcs = match rng.below(24) {
        0..=8 => transmit(rng),
        9..=11 => vec![put(rng)],
        12..=14 => vec![delete(rng)],
        15 => vec![feature_query(rng)],
        16..=18 => vec![other_command(rng)],
        _ => return scenario(rng),
    };
    (apcs, None)
}

/// Placeholder cells showing a virtual placement: its rows, each its
/// columns, numbered with diacritics, in the colours that name it.
fn show_virtual(rng: &mut Rng, made: Virtual, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("\x1b[38;5;{}m", made.image).as_bytes());
    if made.placement != 0 {
        out.extend_from_slice(format!("\x1b[58;5;{}m", made.placement).as_bytes());
    }
    for row in 0..made.rows {
        let mut buf = [0; 4];
        for column in 0..made.columns {
            out.extend_from_slice("\u{10EEEE}".as_bytes());
            out.extend_from_slice(DIACRITICS[row as usize].encode_utf8(&mut buf).as_bytes());
            // Kitty infers a missing column from the cell before.
            if column == 0 || rng.chance(1, 2) {
                out.extend_from_slice(DIACRITICS[column as usize].encode_utf8(&mut buf).as_bytes());
            }
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\x1b[39;59m");
}

/// The diacritics kitty's placeholder cells number rows and columns with
/// (the first few of its table), and other combining marks.
const DIACRITICS: &[char] = &[
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}',
];
const OTHER_MARKS: &[char] = &['\u{0300}', '\u{0301}', '\u{0308}', '\u{0327}', '\u{20DD}'];

fn sgr(rng: &mut Rng) -> String {
    let params = match rng.below(14) {
        0 => String::new(),
        1 => "0".to_owned(),
        2 => format!("38;5;{}", rng.range(0, 255)),
        3 => format!(
            "38;2;{};{};{}",
            rng.range(0, 3),
            rng.range(0, 3),
            rng.range(0, 8)
        ),
        4 => format!("58;5;{}", rng.range(0, 9)),
        5 => format!(
            "58:2::{}:{}:{}",
            rng.range(0, 1),
            rng.range(0, 1),
            rng.range(0, 9)
        ),
        6 => format!("58;2;0;0;{}", rng.range(0, 9)),
        7 => "59".to_owned(),
        8 => format!("{}", rng.range(30, 37)),
        9 => "4:3".to_owned(),
        10 => format!("38:5:{}", rng.range(0, 9)),
        11 => "1;4;7".to_owned(),
        12 => format!("48;5;{}", rng.range(0, 255)),
        _ => "38;5".to_owned(),
    };
    format!("\x1b[{params}m")
}

/// Text that addresses virtual placements: colours naming an image (and a
/// placement), then placeholder cells with row/column marks.
fn placeholders(rng: &mut Rng, out: &mut Vec<u8>) {
    let image = rng.range(1, 8);
    match rng.below(4) {
        0 => out.extend_from_slice(format!("\x1b[38;2;0;0;{image}m").as_bytes()),
        _ => out.extend_from_slice(format!("\x1b[38;5;{image}m").as_bytes()),
    }
    if rng.chance(1, 2) {
        out.extend_from_slice(format!("\x1b[58;5;{}m", rng.range(0, 8)).as_bytes());
    }
    let cells = rng.range(1, 12);
    for _ in 0..cells {
        out.extend_from_slice("\u{10EEEE}".as_bytes());
        let marks = rng.index(4);
        for _ in 0..marks {
            let mark = if rng.chance(4, 5) {
                *rng.pick(DIACRITICS)
            } else {
                *rng.pick(OTHER_MARKS)
            };
            let mut buf = [0; 4];
            out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
        }
    }
    if rng.chance(1, 2) {
        out.extend_from_slice(b"\x1b[0m");
    }
}

fn text(rng: &mut Rng, out: &mut Vec<u8>) {
    const WORDS: &[&str] = &[
        "You", "see", "a", "dragon", "here.", "hp:", "123", "|", "[", "]", " ", "  ", "\t",
    ];
    const UNICODE: &[&str] = &[
        "café",
        "日本語",
        "🎉",
        "naïve",
        "e\u{301}",
        "\u{200b}",
        "\u{202e}x",
        "\u{fffd}",
        "\u{10EEEE}",
        "ẞ",
        "\u{feff}",
    ];
    let words = rng.range(1, 8);
    for _ in 0..words {
        if rng.chance(1, 4) {
            out.extend_from_slice(rng.pick(UNICODE).as_bytes());
        } else {
            out.extend_from_slice(rng.pick(WORDS).as_bytes());
        }
        if rng.chance(1, 2) {
            out.push(b' ');
        }
    }
}

fn osc8(rng: &mut Rng, out: &mut Vec<u8>) {
    let st: &[u8] = if rng.chance(1, 3) { b"\x07" } else { b"\x1b\\" };
    match rng.below(5) {
        0 => out.extend_from_slice(b"\x1b]8;;"),
        1 => out.extend_from_slice(b"\x1b]8;id=x;https://example.com/a?b=c"),
        2 => out.extend_from_slice(b"\x1b]8;;send:look\x1b[31m"),
        3 => out.extend_from_slice(b"\x1b]8;;javascript:alert(1)"),
        _ => out.extend_from_slice(b"\x1b]8;;https://example.com/\x1b_Ga=d\x1b\\"),
    }
    out.extend_from_slice(st);
}

fn csi(rng: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    if rng.chance(1, 8) {
        // The queries the graphics protocol's clients send.
        out.extend_from_slice(rng.pick(&[&b"c"[..], b"0c", b"14t", b"16t", b"18t", b">c", b"?c"]));
        return;
    }
    if rng.chance(1, 5) {
        out.push(*rng.pick(b"?>=<"));
    }
    let params = rng.index(5);
    for index in 0..params {
        if index > 0 {
            out.push(if rng.chance(1, 6) { b':' } else { b';' });
        }
        out.extend_from_slice(small(rng).as_bytes());
    }
    if rng.chance(1, 10) {
        out.push(*rng.pick(b" !\"#$%&'"));
    }
    out.push(*rng.pick(b"cmtnhlABCDEFGHJKPSTXZ@`dfqrsu~"));
}

fn other_string(rng: &mut Rng, out: &mut Vec<u8>) {
    let lead: &[u8] = rng.pick(&[
        &b"\x1b_X"[..],
        b"\x1b_",
        b"\x1bP1$q",
        b"\x1b]0;",
        b"\x1bX",
        b"\x1b^",
    ]);
    out.extend_from_slice(lead);
    let len = rng.index(40);
    out.extend(
        rng.bytes(len)
            .into_iter()
            .map(|b| b & 0x7f)
            .filter(|&b| b >= 0x20 && b != 0x7f),
    );
    out.extend_from_slice(if rng.chance(1, 5) { b"\x07" } else { b"\x1b\\" });
}

/// Random bytes; how many of them could begin a control sequence (`ESC`,
/// or a CSI introducer, raw or as the second byte of its UTF-8 encoding)
/// and so lead to a terminal query's answer.
fn garbage(rng: &mut Rng, out: &mut Vec<u8>) -> usize {
    const SPECIAL: &[u8] = &[
        0x1b, 0x9b, 0x9c, 0x9d, 0x9f, 0x18, 0x1a, 0x07, 0x7f, 0xff, 0xc2, 0x00, 0x08,
    ];
    // High bytes land before an `ESC` and at the end, where the next
    // token or a batch end can cut a UTF-8 sequence short.
    let len = rng.range(1, 12);
    let mut introducers = 0;
    for _ in 0..len {
        let byte = if rng.chance(1, 2) {
            *rng.pick(SPECIAL)
        } else {
            rng.byte()
        };
        introducers += usize::from(byte == 0x1b || byte == 0x9b);
        out.push(byte);
    }
    introducers
}

/// What a stream's commands may be answered with, at most: the bounds
/// `check_replies` counts against.
#[derive(Debug, Default)]
struct Budget {
    /// Every graphics APC, chunks included.
    apcs: usize,
    /// Those that name an image (`i` or `I`) and so may be answered: at
    /// most one reply each, however a chunked upload ends.
    replying_apcs: usize,
    /// Control sequences that could be a terminal query: every CSI sent as
    /// one, and every byte in garbage or inside an APC's body that could
    /// begin one.
    queries: usize,
}

impl Budget {
    fn apc(&mut self, apc: &Apc) {
        self.apcs += 1;
        self.replying_apcs += usize::from(may_reply(&apc.body));
        self.queries += memchr::memchr_iter(0x9b, &apc.body).count();
    }
}

/// Whether an APC's control data names an image (`i` or `I`, not zero) and
/// so may be answered. Generous: a value the parser refuses counts too.
fn may_reply(body: &[u8]) -> bool {
    let control = body.strip_prefix(b"G").unwrap_or(body);
    let control = control.split(|&b| b == b';').next().unwrap_or(control);
    control.split(|&b| b == b',').any(|item| {
        matches!(item, [b'i' | b'I', b'=', value @ ..] if value.iter().any(u8::is_ascii_digit) && value.iter().any(|&b| b != b'0'))
    })
}

/// A seed's stream, and what its commands may be answered with.
fn stream(rng: &mut Rng) -> (Vec<u8>, Budget) {
    let target = 128 + rng.index(5000);
    let mut out = Vec::with_capacity(target + 4096);
    let mut budget = Budget::default();
    // Chunks of an upload not yet written, released between other tokens.
    let mut pending: VecDeque<Apc> = VecDeque::new();
    while out.len() < target || !pending.is_empty() {
        if let Some(apc) = pending.pop_front() {
            push_apc(&mut out, &apc);
            budget.apc(&apc);
            // Usually the next chunk follows at once.
            if !pending.is_empty() && rng.chance(4, 5) {
                continue;
            }
        }
        match rng.below(100) {
            0..=21 => text(rng, &mut out),
            22..=27 => placeholders(rng, &mut out),
            28..=39 => out.extend_from_slice(rng.pick(&[&b"\r\n"[..], b"\n", b"\r", b"\n\n"])),
            40..=47 => out.extend_from_slice(sgr(rng).as_bytes()),
            48..=50 => osc8(rng, &mut out),
            51..=56 => {
                csi(rng, &mut out);
                budget.queries += 1;
            }
            57..=88 => {
                let (apcs, made) = graphics(rng);
                if pending.is_empty() {
                    pending.extend(apcs);
                    if let Some(made) = made {
                        // The whole scenario, then text showing it.
                        while let Some(apc) = pending.pop_front() {
                            push_apc(&mut out, &apc);
                            budget.apc(&apc);
                        }
                        show_virtual(rng, made, &mut out);
                    }
                } else {
                    // Another command in the middle of an upload.
                    for apc in apcs.into_iter().take(1) {
                        push_apc(&mut out, &apc);
                        budget.apc(&apc);
                    }
                }
            }
            89..=93 => other_string(rng, &mut out),
            _ => budget.queries += garbage(rng, &mut out),
        }
    }
    (out, budget)
}

// --------------------------------------------------------------- hosts --

/// A decode the host was asked for: its ticket and the size its pixels
/// must have — the data's, or for a frame or a compose the canvas it lands
/// on, whatever rectangle its data covers.
type Requested = (DecodeTicket, u32, u32);

#[derive(Debug, Default)]
struct HostLog {
    requested: Vec<Requested>,
    decodes: usize,
    media_reads: usize,
}

/// Records decodes for the harness to complete; serves a fake filesystem.
#[derive(Debug)]
struct FuzzHost(Arc<Mutex<HostLog>>);

impl GraphicsHost for FuzzHost {
    fn decode(&mut self, ticket: DecodeTicket, request: DecodeRequest) {
        let mut log = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        log.decodes += 1;
        let (width, height) = request
            .compose
            .as_ref()
            .map_or((request.width, request.height), |compose| compose.canvas);
        log.requested.push((ticket, width, height));
    }

    fn read_media(
        &mut self,
        _medium: Medium,
        name: &[u8],
        offset: u32,
        size: u32,
    ) -> Result<Vec<u8>, GraphicsError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .media_reads += 1;
        match name {
            b"/tmp/tty-graphics-protocol-fuzz.rgb" if size <= 64 * 1024 => {
                let len = if size == 0 { 48 } else { size as usize };
                Ok(vec![u8::try_from(offset % 256).unwrap_or(0); len])
            }
            b"/dev/shm/fuzz-png" | b"fuzz-shm" => {
                let mut png = png_header(4, 4);
                png.resize(64, 0);
                Ok(png)
            }
            _ => Err(GraphicsError::bad_file()),
        }
    }
}

/// Pixels of the size asked for (or, to test the check, not).
fn pixels(width: u32, height: u32) -> DecodedPixels {
    let len =
        usize::try_from(u64::from(width) * u64::from(height) * 4).expect("admitted sizes fit");
    DecodedPixels {
        width,
        height,
        rgba: vec![0_u8; len].into(),
    }
}

/// Pixels of another size than asked for, which must be refused: one
/// column more, or one fewer at the width the generator caps at.
fn wrong_size(width: u32, height: u32) -> DecodedPixels {
    let width = if width < 64 { width + 1 } else { width - 1 };
    pixels(width, height)
}

/// The failure a decoder that loses the request is reported with: what
/// core's completion guard sends when the callback is dropped unrun.
fn dropped_decode() -> GraphicsError {
    GraphicsError::invalid("decoder dropped the request")
}

/// How the harness settles a decode; `None` when the decoder loses it.
fn completion(
    rng: &mut Rng,
    width: u32,
    height: u32,
) -> Option<Result<DecodedPixels, GraphicsError>> {
    Some(match rng.below(100) {
        0..=79 => Ok(pixels(width, height)),
        80..=89 => Err(GraphicsError::invalid("fuzz decode failed")),
        90..=94 => Ok(wrong_size(width, height)),
        _ => return None,
    })
}

// ------------------------------------------------- processor harness --

#[derive(Debug, Default)]
struct Collect(Vec<VtEvent>);

impl VtSink for Collect {
    fn emit(&mut self, event: VtEvent) {
        self.0.push(event);
    }
}

/// How a decode is settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pixels,
    Failed,
    /// Pixels of another size than asked, which the processor must refuse.
    WrongSize,
    /// The decoder lost the request; its completion guard reports that.
    Dropped,
}

/// One step of a run. The random driver produces these and the harness
/// applies them, so a failing run is a list of steps that can be replayed
/// and shrunk (see `minimize_a_failing_seed`).
#[derive(Clone, Debug)]
enum Op {
    /// Bytes through the connection's byte loop; with `bulk`, plain runs
    /// in Ground take the bulk path.
    Feed(Vec<u8>, bool),
    EndBatch,
    Prompt,
    Drain,
    Mode(GraphicsMode),
    Raw(bool),
    Geometry {
        cell: u32,
        grid: u32,
    },
    /// Settle the `n`th decode requested, counting from zero.
    Settle(usize, Outcome),
    /// Settle the `n`th decode a second time: a host bug to shrug off.
    Again(usize),
    /// With every decode settled — the ones the decoder lost by their
    /// guard's failure — DA1 is answered at once.
    ExpectDa1,
}

/// How a run's processor starts.
#[derive(Clone, Copy, Debug)]
struct Setup {
    mode: GraphicsMode,
    raw: bool,
    host: bool,
}

#[derive(Debug, Default)]
struct Stats {
    seeds: u64,
    bytes: usize,
    feeds: usize,
    lines: usize,
    objects: usize,
    anchored: usize,
    placeholder_runs: usize,
    relative_children: usize,
    decodes: usize,
    media_reads: usize,
    reply_bytes: usize,
    oks: usize,
    errors: usize,
    dropped: usize,
    graphics_replies: usize,
    answers: usize,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.seeds += other.seeds;
        self.bytes += other.bytes;
        self.feeds += other.feeds;
        self.lines += other.lines;
        self.objects += other.objects;
        self.anchored += other.anchored;
        self.placeholder_runs += other.placeholder_runs;
        self.relative_children += other.relative_children;
        self.decodes += other.decodes;
        self.media_reads += other.media_reads;
        self.reply_bytes += other.reply_bytes;
        self.oks += other.oks;
        self.errors += other.errors;
        self.dropped += other.dropped;
        self.graphics_replies += other.graphics_replies;
        self.answers += other.answers;
    }
}

struct Harness<'t> {
    parser: VTParser,
    processor: VtProcessor<Collect>,
    host: Arc<Mutex<HostLog>>,
    /// Every decode requested so far, in request order, and whether it was
    /// settled.
    requested: Vec<Requested>,
    settled: Vec<bool>,
    raw_wanted: Arc<AtomicBool>,
    window: Arc<AtomicU32>,
    cell: Arc<AtomicU32>,
    replies: Vec<u8>,
    stats: Stats,
    /// The steps applied so far, the failing one last.
    trace: &'t mut Vec<Op>,
}

impl<'t> Harness<'t> {
    fn new(setup: Setup, trace: &'t mut Vec<Op>) -> Self {
        let host = Arc::new(Mutex::new(HostLog::default()));
        let mut processor = VtProcessor::new(Collect::default());
        let raw_wanted = Arc::new(AtomicBool::new(setup.raw));
        processor.set_raw_wanted_flag(raw_wanted.clone());
        processor.set_graphics_mode(setup.mode);
        let window = Arc::new(AtomicU32::new(pack_dims(80, 24)));
        let cell = Arc::new(AtomicU32::new(0));
        processor.set_terminal_geometry(window.clone(), cell.clone());
        if setup.host {
            processor.set_graphics_host(Box::new(FuzzHost(host.clone())));
        }
        Self {
            parser: VTParser::new(),
            processor,
            host,
            requested: Vec::new(),
            settled: Vec::new(),
            raw_wanted,
            window,
            cell,
            replies: Vec::new(),
            stats: Stats::default(),
            trace,
        }
    }

    fn apply(&mut self, op: Op) {
        self.trace.push(op.clone());
        match op {
            Op::Feed(bytes, bulk) => self.feed(&bytes, bulk),
            Op::EndBatch => self.processor.notify_end_of_buffer(),
            Op::Prompt => self.processor.commit_prompt(),
            Op::Drain => self.drain(),
            Op::Mode(mode) => self.processor.set_graphics_mode(mode),
            Op::Raw(wanted) => self.raw_wanted.store(wanted, Ordering::Relaxed),
            Op::Geometry { cell, grid } => {
                self.cell.store(cell, Ordering::Relaxed);
                self.window.store(grid, Ordering::Relaxed);
            }
            Op::Settle(n, outcome) => {
                self.sync_requested();
                if let Some(&(ticket, width, height)) = self.requested.get(n)
                    && !self.settled[n]
                {
                    self.settled[n] = true;
                    let result = match outcome {
                        Outcome::Pixels => Ok(pixels(width, height)),
                        Outcome::Failed => Err(GraphicsError::invalid("fuzz decode failed")),
                        Outcome::WrongSize => Ok(wrong_size(width, height)),
                        Outcome::Dropped => Err(dropped_decode()),
                    };
                    self.processor.graphics_decode_complete(ticket, result);
                }
            }
            Op::Again(n) => {
                if let Some(&(ticket, ..)) = self.requested.get(n)
                    && self.settled[n]
                {
                    self.processor
                        .graphics_decode_complete(ticket, Err(GraphicsError::invalid("again")));
                }
            }
            Op::ExpectDa1 => {
                // A frame command queued behind a decode issues its own decode
                // when that one lands, so settle until nothing new is requested.
                loop {
                    self.sync_requested();
                    let pending: Vec<usize> = (0..self.requested.len())
                        .filter(|&n| !self.settled[n])
                        .collect();
                    if pending.is_empty() {
                        break;
                    }
                    for n in pending {
                        let (ticket, width, height) = self.requested[n];
                        self.settled[n] = true;
                        self.processor
                            .graphics_decode_complete(ticket, Ok(pixels(width, height)));
                    }
                }
                self.feed(b"\x1b[c", false);
                self.drain();
                assert!(
                    self.replies.ends_with(b"\x1b[?62;c"),
                    "with every decode settled, DA1 is answered at once: {:?}",
                    String::from_utf8_lossy(&self.replies[self.replies.len().saturating_sub(80)..])
                );
            }
        }
        self.check_events();
    }

    fn sync_requested(&mut self) {
        let mut log = self.host.lock().unwrap_or_else(PoisonError::into_inner);
        self.settled
            .resize(self.settled.len() + log.requested.len(), false);
        self.requested.append(&mut log.requested);
    }

    /// The connection's byte loop: with `bulk`, plain runs while the parser
    /// idles in Ground (raw capture first); everything else a byte at a time.
    fn feed(&mut self, bytes: &[u8], bulk: bool) {
        self.stats.feeds += 1;
        self.stats.bytes += bytes.len();
        let mut at = 0;
        while at < bytes.len() {
            if bulk && self.parser.is_ground() {
                let run = plain_run(&bytes[at..]);
                if !run.is_empty() {
                    self.processor.push_raw_incoming_run(run);
                    self.processor.print_run(run);
                    at += run.len();
                    continue;
                }
            }
            let byte = bytes[at];
            if byte != b'\n' && byte != b'\r' {
                self.processor.push_raw_incoming_byte(byte);
            }
            self.parser.parse_byte(byte, &mut self.processor);
            at += 1;
        }
    }

    fn drain(&mut self) {
        let before = self.replies.len();
        self.processor.drain_replies(&mut self.replies);
        assert!(
            !self.processor.has_replies(),
            "a drain left a ready reply behind"
        );
        let (graphics, answers) = check_replies(&self.replies[before..]);
        self.stats.graphics_replies += graphics;
        self.stats.answers += answers;
    }

    fn check_events(&mut self) {
        for event in std::mem::take(&mut self.processor.sink_mut().0) {
            match &event {
                VtEvent::HandleIncomingLine(line) | VtEvent::HandleIncomingPartialLine(line) => {
                    self.check_line(line);
                }
                VtEvent::HandleIncomingFragmentedLine {
                    line,
                    completion_fragment,
                } => {
                    self.check_line(line);
                    self.check_line(completion_fragment);
                }
                _ => {}
            }
        }
    }

    fn check_line(&mut self, line: &StyledLine) {
        self.stats.lines += 1;
        if let Some(raw) = line.raw() {
            assert!(
                !raw.contains("\x1b_G"),
                "a graphics APC reached raw text: {raw:?}"
            );
        }
        let Some(objects) = &line.objects else {
            return;
        };
        assert!(
            objects.len() <= MAX_INLINE_OBJECTS,
            "{} objects on one line",
            objects.len()
        );
        for object in objects.iter() {
            let range = &object.range;
            assert!(
                range.start <= range.end
                    && range.end <= line.text.len()
                    && line.text.is_char_boundary(range.start)
                    && line.text.is_char_boundary(range.end),
                "object range {range:?} outside its line {:?}",
                line.text
            );
            self.stats.objects += 1;
            if let Ok(placement) = object.resource.clone().downcast::<Placement>() {
                assert!(placement.geometry.columns <= MAX_PLACEMENT_CELLS);
                assert!(placement.geometry.rows <= MAX_PLACEMENT_ROWS);
                self.stats.anchored += usize::from(placement.anchored);
                self.stats.placeholder_runs += usize::from(placement.placeholder);
                self.stats.relative_children += placement.child_placements().len();
            }
        }
        assert!(
            objects
                .windows(2)
                .all(|pair| pair[0].range.start <= pair[1].range.start),
            "objects out of line order on {:?}",
            line.text
        );
    }

    fn stats(mut self) -> Stats {
        let log = self.host.lock().unwrap_or_else(PoisonError::into_inner);
        self.stats.decodes = log.decodes;
        self.stats.media_reads = log.media_reads;
        drop(log);
        self.stats.reply_bytes = self.replies.len();
        self.stats.oks = count(&self.replies, b";OK\x1b\\");
        self.stats.errors = count(&self.replies, b";E");
        self.stats.seeds = 1;
        self.stats
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// The leading run the parser would only print, as the connection's bulk
/// path finds it: no C0 control or DEL, valid UTF-8, no encoded C1.
fn plain_run(bytes: &[u8]) -> &str {
    let end = bytes
        .iter()
        .position(|&b| b < 0x20 || b == 0x7f)
        .unwrap_or(bytes.len());
    let valid = match std::str::from_utf8(&bytes[..end]) {
        Ok(text) => text,
        Err(err) => std::str::from_utf8(&bytes[..err.valid_up_to()]).expect("valid prefix"),
    };
    let c1 = valid
        .char_indices()
        .find(|(_, c)| ('\u{80}'..='\u{9f}').contains(c))
        .map_or(valid.len(), |(at, _)| at);
    &valid[..c1]
}

/// Every reply is a graphics response (`ESC _ G` keys `;` status `ESC \`,
/// see [`check_graphics_reply`]) or a terminal-query answer: DA1's
/// `ESC [ ? 62 ; c`, or a size report `ESC [ 4|6|8 ; n ; n t`. How many of
/// each.
fn check_replies(bytes: &[u8]) -> (usize, usize) {
    let (mut graphics, mut answers) = (0, 0);
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if let Some(body) = rest.strip_prefix(b"\x1b_G") {
            let end = body
                .windows(2)
                .position(|w| w == b"\x1b\\")
                .unwrap_or_else(|| panic!("unterminated graphics reply {rest:?}"));
            check_graphics_reply(&body[..end]);
            graphics += 1;
            at += 3 + end + 2;
        } else if let Some(body) = rest.strip_prefix(b"\x1b[") {
            let end = body
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .unwrap_or_else(|| panic!("unterminated query answer {rest:?}"));
            let answer = String::from_utf8_lossy(&body[..=end]);
            assert!(
                answer == "?62;c" || is_size_report(&answer),
                "unexpected answer {answer:?}"
            );
            answers += 1;
            at += 2 + end + 1;
        } else {
            panic!("unexpected reply bytes {rest:?}");
        }
    }
    (graphics, answers)
}

/// `4;h;w t`, `6;h;w t` or `8;r;c t`: numbers all the way.
fn is_size_report(answer: &str) -> bool {
    let Some(report) = answer.strip_suffix('t') else {
        return false;
    };
    let fields: Vec<&str> = report.split(';').collect();
    matches!(fields[..], ["4" | "6" | "8", a, b] if a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok())
}

/// A graphics response's body, as kitty's clients parse it: printable
/// ASCII; `key=value` items with single-letter keys, each at most once, the
/// image (`i` or `I`) named first, numeric where the key is one of kitty's
/// numeric keys; then `;` and `OK` or an `E…` code, a colon and a message.
fn check_graphics_reply(body: &[u8]) {
    let text = String::from_utf8_lossy(body);
    assert!(
        body.iter().all(|b| (0x20..0x7f).contains(b)),
        "unprintable graphics reply {text:?}"
    );
    let (keys, status) = text
        .split_once(';')
        .unwrap_or_else(|| panic!("no status in graphics reply {text:?}"));
    let mut seen: Vec<&str> = Vec::new();
    for (index, item) in keys.split(',').enumerate() {
        let (key, value) = item
            .split_once('=')
            .unwrap_or_else(|| panic!("key without a value in graphics reply {text:?}"));
        assert!(
            key.len() == 1 && key.bytes().all(|b| b.is_ascii_alphabetic()),
            "key {key:?} in graphics reply {text:?}"
        );
        assert!(
            !value.is_empty()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "value {value:?} in graphics reply {text:?}"
        );
        assert!(
            !seen.contains(&key),
            "key {key:?} twice in graphics reply {text:?}"
        );
        seen.push(key);
        if index == 0 {
            assert!(
                key == "i" || key == "I",
                "graphics reply {text:?} names no image first"
            );
        }
        if matches!(key, "i" | "I" | "p" | "r") {
            assert!(
                value.parse::<u32>().is_ok_and(|n| n != 0),
                "{key}={value} in graphics reply {text:?}"
            );
        }
    }
    let well_formed_error = |status: &str| {
        status.split_once(':').is_some_and(|(code, message)| {
            code.len() > 1
                && code.starts_with('E')
                && code.bytes().all(|b| b.is_ascii_uppercase())
                && !message.is_empty()
        })
    };
    assert!(
        status == "OK" || well_formed_error(status),
        "status {status:?} in graphics reply {text:?}"
    );
}

/// A feed's length: often tiny (splitting every sequence), often a typical
/// read, sometimes large.
fn feed_len(rng: &mut Rng) -> usize {
    match rng.below(4) {
        0 => rng.range(1, 8) as usize,
        1 | 2 => rng.range(9, 512) as usize,
        _ => rng.range(513, 8192) as usize,
    }
}

fn random_geometry(rng: &mut Rng) -> Op {
    let cell = if rng.chance(1, 5) {
        0
    } else {
        let scale = u32::from(CELL_FIXED_SCALE);
        let width = u16::try_from(rng.range(scale, 40 * scale)).expect("fits");
        let height = u16::try_from(rng.range(scale, 60 * scale)).expect("fits");
        pack_dims(width, height)
    };
    let grid = pack_dims(
        u16::try_from(rng.range(0, 400)).expect("fits"),
        u16::try_from(rng.range(0, 200)).expect("fits"),
    );
    Op::Geometry { cell, grid }
}

/// The setup a seed's run starts from.
fn setup(seed: u64) -> Setup {
    let mut rng = Rng::new(seed ^ 0x5E70);
    Setup {
        mode: if rng.chance(7, 8) {
            GraphicsMode::Full
        } else {
            *rng.pick(&[GraphicsMode::DisplayOnly, GraphicsMode::Off])
        },
        raw: rng.chance(1, 2),
        host: rng.chance(19, 20),
    }
}

/// Settle some of the decodes requested and not yet settled: in order or
/// not, well or badly, now, later or never (`dropped`).
fn settle_some(harness: &mut Harness<'_>, rng: &mut Rng, dropped: &mut Vec<usize>) {
    harness.sync_requested();
    let mut open: Vec<usize> = (0..harness.requested.len())
        .filter(|&n| !harness.settled[n] && !dropped.contains(&n))
        .collect();
    if open.len() > 1 && rng.chance(1, 3) {
        for index in (1..open.len()).rev() {
            let other = rng.index(index + 1);
            open.swap(index, other);
        }
    }
    for n in open {
        match rng.below(100) {
            0..=24 => {}
            25..=84 => harness.apply(Op::Settle(n, Outcome::Pixels)),
            85..=92 => harness.apply(Op::Settle(n, Outcome::Failed)),
            93..=97 => harness.apply(Op::Settle(n, Outcome::WrongSize)),
            _ => dropped.push(n),
        }
    }
    if rng.chance(1, 50) {
        let settled: Vec<usize> = (0..harness.settled.len())
            .filter(|&n| harness.settled[n])
            .collect();
        if !settled.is_empty() {
            harness.apply(Op::Again(*rng.pick(&settled)));
        }
    }
}

/// One seed's run, recording its steps in `trace`.
fn run_processor_seed(seed: u64, trace: &mut Vec<Op>) -> Stats {
    let setup = setup(seed);
    let mut rng = Rng::new(seed);
    let (bytes, budget) = stream(&mut rng);
    let mut harness = Harness::new(setup, trace);
    let mut dropped = Vec::new();
    harness.apply(random_geometry(&mut rng));
    let mut at = 0;
    while at < bytes.len() {
        let end = (at + feed_len(&mut rng)).min(bytes.len());
        harness.apply(Op::Feed(bytes[at..end].to_vec(), rng.chance(3, 4)));
        at = end;
        // Between feeds: batch ends (after which decodes land and replies
        // are written, as in the connection's `select!`), prompts, drains,
        // and now and then a decode landing inside a batch, a mode switch,
        // raw capture flipping, a new geometry.
        if rng.chance(1, 2) {
            harness.apply(Op::EndBatch);
            settle_some(&mut harness, &mut rng, &mut dropped);
            harness.apply(Op::Drain);
        } else if rng.chance(1, 16) {
            settle_some(&mut harness, &mut rng, &mut dropped);
        }
        if rng.chance(1, 10) {
            harness.apply(Op::Prompt);
        }
        if rng.chance(1, 4) {
            harness.apply(Op::Drain);
        }
        if rng.chance(1, 60) {
            let mode = *rng.pick(&[
                GraphicsMode::Full,
                GraphicsMode::DisplayOnly,
                GraphicsMode::Off,
            ]);
            harness.apply(Op::Mode(mode));
        }
        if rng.chance(1, 10) {
            harness.apply(Op::Raw(rng.chance(1, 2)));
        }
        if rng.chance(1, 30) {
            harness.apply(random_geometry(&mut rng));
        }
    }
    // The end: abort whatever escape is open, settle every decode — the
    // ones the decoder lost with their guard's failure, as core's
    // completion guard reports them — and check that nothing stays parked.
    harness.apply(Op::Feed(b"\x18".to_vec(), false));
    harness.apply(Op::EndBatch);
    harness.apply(Op::Mode(GraphicsMode::Full));
    harness.sync_requested();
    for n in 0..harness.requested.len() {
        if !harness.settled[n] {
            let outcome = if dropped.contains(&n) {
                Outcome::Dropped
            } else {
                Outcome::Pixels
            };
            harness.apply(Op::Settle(n, outcome));
        }
    }
    harness.apply(Op::Drain);
    harness.apply(Op::ExpectDa1);
    harness.apply(Op::Prompt);
    let replies = String::from_utf8_lossy(&harness.replies).into_owned();
    let mut stats = harness.stats();
    stats.dropped = dropped.len();
    // One reply per command that could be answered at most (`ExpectDa1`
    // asks one query of its own), none past a few hundred bytes.
    assert!(
        stats.graphics_replies <= budget.replying_apcs,
        "{} graphics replies for {budget:?}: {replies:?}\nSTREAM {}",
        stats.graphics_replies,
        bytes.escape_ascii()
    );
    assert!(
        stats.answers <= budget.queries + 1,
        "{} query answers for {budget:?}: {replies:?}",
        stats.answers
    );
    assert!(
        stats.reply_bytes <= (budget.apcs + budget.queries + 2) * 512,
        "{} reply bytes for {budget:?}",
        stats.reply_bytes
    );
    stats
}

/// Replay `ops` on a fresh processor started as `setup`.
fn replay(setup: Setup, ops: &[Op]) {
    let mut trace = Vec::new();
    let mut harness = Harness::new(setup, &mut trace);
    for op in ops {
        harness.apply(op.clone());
    }
}

fn seeds() -> std::ops::Range<u64> {
    let var = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let first = var("GRAPHICS_FUZZ_FIRST_SEED", 0);
    first..first + var("GRAPHICS_FUZZ_SEEDS", 2000)
}

/// Run `seed` through `run`, naming the seed if it panics.
fn run_named<T>(what: &str, seed: u64, run: impl FnOnce(u64) -> T) -> T {
    match catch_unwind(AssertUnwindSafe(|| run(seed))) {
        Ok(value) => value,
        Err(panic) => {
            eprintln!(
                "{what}: seed {seed} failed; replay with GRAPHICS_FUZZ_FIRST_SEED={seed} \
                 GRAPHICS_FUZZ_SEEDS=1, shrink with GRAPHICS_FUZZ_MINIMIZE={seed} \
                 (the ignored minimize_a_failing_seed test)"
            );
            std::panic::resume_unwind(panic);
        }
    }
}

#[test]
fn random_streams_keep_the_processor_sound() {
    let started = Instant::now();
    let mut total = Stats::default();
    for seed in seeds() {
        let stats = run_named("processor", seed, |seed| {
            run_processor_seed(seed, &mut Vec::new())
        });
        total.add(&stats);
    }
    eprintln!("processor fuzz: {total:?} in {:.2?}", started.elapsed());
    // The generator reaches what it is for.
    if total.seeds >= 500 {
        assert!(total.objects > 0 && total.anchored > 0, "{total:?}");
        assert!(total.placeholder_runs > 0, "{total:?}");
        assert!(
            total.oks > 0 && total.errors > 0 && total.decodes > 0,
            "{total:?}"
        );
        assert!(total.media_reads > 0, "{total:?}");
        assert!(
            total.graphics_replies > 0 && total.answers > 0 && total.dropped > 0,
            "{total:?}"
        );
    }
}

// ------------------------------------------------------ minimizing --

/// The first line of a panic's message, up to its first colon: what a
/// shrunk run must still fail with.
fn panic_kind(panic: &(dyn std::any::Any + Send)) -> String {
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    message
        .lines()
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_owned()
}

fn fails_with(setup: Setup, ops: &[Op], kind: &str) -> bool {
    catch_unwind(AssertUnwindSafe(|| replay(setup, ops)))
        .is_err_and(|panic| panic_kind(panic.as_ref()) == kind)
}

/// Shrink a failing seed's steps to a small list that fails the same way:
/// drop runs of steps, then bytes within each feed, until nothing more can
/// go. `GRAPHICS_FUZZ_MINIMIZE=<seed> cargo test -p smudgy_protocol --test
/// graphics_stream_fuzz -- --ignored --nocapture minimize`.
#[test]
#[ignore = "a tool: run with GRAPHICS_FUZZ_MINIMIZE=<seed>"]
fn minimize_a_failing_seed() {
    let Some(seed) = std::env::var("GRAPHICS_FUZZ_MINIMIZE")
        .ok()
        .and_then(|seed| seed.parse::<u64>().ok())
    else {
        eprintln!("set GRAPHICS_FUZZ_MINIMIZE=<seed>");
        return;
    };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut ops = Vec::new();
    let failure = catch_unwind(AssertUnwindSafe(|| run_processor_seed(seed, &mut ops)));
    let Err(panic) = failure else {
        std::panic::set_hook(hook);
        eprintln!("seed {seed} does not fail");
        return;
    };
    let kind = panic_kind(panic.as_ref());
    let setup = setup(seed);
    assert!(
        fails_with(setup, &ops, &kind),
        "the trace replays the failure"
    );
    loop {
        let before = (ops.len(), feed_bytes(&ops));
        // Runs of steps, halving.
        let mut size = ops.len() / 2;
        while size > 0 {
            let mut at = 0;
            while at < ops.len() {
                let mut candidate = ops.clone();
                candidate.drain(at..(at + size).min(ops.len()));
                if fails_with(setup, &candidate, &kind) {
                    ops = candidate;
                } else {
                    at += size;
                }
            }
            size /= 2;
        }
        // Bytes within feeds, halving; and the per-byte path for bulk.
        for index in 0..ops.len() {
            if let Op::Feed(_, true) = &ops[index] {
                let mut candidate = ops.clone();
                if let Op::Feed(_, bulk) = &mut candidate[index] {
                    *bulk = false;
                }
                if fails_with(setup, &candidate, &kind) {
                    ops = candidate;
                }
            }
            let Op::Feed(bytes, _) = &ops[index] else {
                continue;
            };
            let mut size = bytes.len() / 2;
            while size > 0 {
                let mut at = 0;
                loop {
                    let Op::Feed(bytes, _) = &ops[index] else {
                        unreachable!()
                    };
                    if at >= bytes.len() {
                        break;
                    }
                    let mut candidate = ops.clone();
                    if let Op::Feed(bytes, _) = &mut candidate[index] {
                        let end = (at + size).min(bytes.len());
                        bytes.drain(at..end);
                    }
                    if fails_with(setup, &candidate, &kind) {
                        ops = candidate;
                    } else {
                        at += size;
                    }
                }
                size /= 2;
            }
        }
        if (ops.len(), feed_bytes(&ops)) == before {
            break;
        }
    }
    std::panic::set_hook(hook);
    eprintln!("seed {seed} fails with {kind:?}; minimal steps from {setup:?}:");
    for op in &ops {
        match op {
            Op::Feed(bytes, bulk) => {
                eprintln!("  Feed(b\"{}\", {bulk})", bytes.escape_ascii());
            }
            other => eprintln!("  {other:?}"),
        }
    }
}

fn feed_bytes(ops: &[Op]) -> usize {
    ops.iter()
        .map(|op| match op {
            Op::Feed(bytes, _) => bytes.len(),
            _ => 0,
        })
        .sum()
}

// ------------------------------------------------------ state harness --

#[derive(Debug, Default)]
struct StateStats {
    commands: usize,
    decodes: usize,
    peak_used: u64,
    peak_images: usize,
    placements: usize,
}

#[allow(clippy::too_many_lines)] // One run: setup, steps, checks, the end.
fn run_state_seed(seed: u64) -> StateStats {
    let mut rng = Rng::new(seed ^ 0x5741_7445);
    let quota = *rng.pick(&[0_u64, 1024, 16 * 1024, 256 * 1024, 4 << 20]);
    let mut table = GraphicsState::with_quota(quota);
    let log = Arc::new(Mutex::new(HostLog::default()));
    let mut host = FuzzHost(log.clone());
    let mut replies = ReplyQueue::default();
    let mut scrollback: VecDeque<Arc<Placement>> = VecDeque::new();
    let mut held: Vec<Requested> = Vec::new();
    // Decodes the decoder lost: in flight until their guards report them.
    let mut dropped: Vec<Requested> = Vec::new();
    let (mut issued, mut completed) = (0_usize, 0_usize);
    let mut line_seq = 0_u64;
    let mut stats = StateStats::default();
    let mut out = Vec::new();

    let steps = 10 + rng.index(60);
    for _ in 0..steps {
        let (apcs, _) = graphics(&mut rng);
        for apc in apcs {
            let mut env = GraphicsEnv {
                host: &mut host,
                replies: &mut replies,
                cell: if rng.chance(1, 4) {
                    (0, 0)
                } else {
                    (9 * CELL_FIXED_SCALE, 18 * CELL_FIXED_SCALE)
                },
                replies_enabled: rng.chance(9, 10),
                objects_free: rng.index(MAX_INLINE_OBJECTS + 1),
                line_seq,
                column: u16::try_from(rng.index(200)).expect("fits"),
                grid: (80, 24),
            };
            if rng.chance(1, 40) {
                let cut = rng.index(apc.body.len() + 1);
                table.truncated_command(&mut env, &apc.body[..cut]);
            } else if let Some(Ok((control, payload))) = parse_apc(&apc.body)
                && let Some(placement) = table.command(&mut env, control, payload)
            {
                assert!(placement.geometry.columns <= MAX_PLACEMENT_CELLS);
                assert!(placement.geometry.rows <= MAX_PLACEMENT_ROWS);
                stats.placements += 1;
                scrollback.push_back(placement);
            }
            stats.commands += 1;
        }
        // Lines scroll away, and with them their placements.
        while scrollback.len() > 40 || (!scrollback.is_empty() && rng.chance(1, 8)) {
            scrollback.pop_front();
        }
        if rng.chance(1, 3) {
            line_seq += rng.below(30);
        }
        // Decodes land, some late, some badly, some never.
        let mut requested =
            std::mem::take(&mut log.lock().unwrap_or_else(PoisonError::into_inner).requested);
        issued += requested.len();
        held.append(&mut requested);
        let mut kept = Vec::new();
        for (ticket, width, height) in std::mem::take(&mut held) {
            if rng.chance(1, 3) {
                kept.push((ticket, width, height));
            } else if let Some(result) = completion(&mut rng, width, height) {
                table.decode_complete(&mut host, &mut replies, ticket, result);
                completed += 1;
            } else {
                dropped.push((ticket, width, height));
            }
        }
        held = kept;
        // A landing decode dispatches the next frame command queued on its
        // image, which asks the host for a decode of its own: count those too.
        let mut dispatched =
            std::mem::take(&mut log.lock().unwrap_or_else(PoisonError::into_inner).requested);
        issued += dispatched.len();
        held.append(&mut dispatched);
        replies.drain_into(&mut out);
        assert!(!replies.has_ready());

        // The accounting the state exposes.
        assert!(
            table.used_bytes() <= quota,
            "{} > quota {quota}",
            table.used_bytes()
        );
        assert!(table.image_count() <= MAX_IMAGES);
        assert_eq!(table.decodes_in_flight(), issued - completed);
        let mut uids = Vec::new();
        let mut addressable = 0;
        for id in (0..=8).chain([u32::MAX]) {
            if let Some(record) = table.image(id) {
                assert!(
                    !record.is_evicted(),
                    "image {id} is addressable but evicted"
                );
                assert!(
                    matches!(record.data(), ImageData::Pending | ImageData::Ready(_)),
                    "image {id} is addressable with {:?}",
                    record.data()
                );
                assert_eq!(
                    record.decoded_len(),
                    u64::from(record.width) * u64::from(record.height) * 4
                );
                if !uids.contains(&record.uid) {
                    uids.push(record.uid);
                    addressable += record.decoded_len();
                }
            }
        }
        assert!(addressable <= table.used_bytes());
        stats.peak_used = stats.peak_used.max(table.used_bytes());
        stats.peak_images = stats.peak_images.max(table.image_count());
    }
    check_replies(&out);
    // Every decode the host kept lands, the ones the decoder lost are
    // reported by their guards, and nothing is left parked.
    while !held.is_empty() || !dropped.is_empty() {
        for (ticket, width, height) in std::mem::take(&mut held) {
            table.decode_complete(&mut host, &mut replies, ticket, Ok(pixels(width, height)));
            completed += 1;
        }
        for (ticket, ..) in std::mem::take(&mut dropped) {
            table.decode_complete(&mut host, &mut replies, ticket, Err(dropped_decode()));
            completed += 1;
        }
        // Each landing may dispatch a queued frame command's decode.
        let mut dispatched =
            std::mem::take(&mut log.lock().unwrap_or_else(PoisonError::into_inner).requested);
        issued += dispatched.len();
        held.append(&mut dispatched);
    }
    replies.drain_into(&mut out);
    check_replies(&out);
    assert_eq!(issued, completed);
    assert!(
        !replies.is_waiting(),
        "a reply is parked with no decode in flight"
    );
    assert_eq!(table.decodes_in_flight(), 0);
    stats.decodes = issued;
    stats
}

#[test]
fn random_commands_keep_the_graphics_state_accounting_sound() {
    let started = Instant::now();
    let mut total = StateStats::default();
    for seed in seeds() {
        let stats = run_named("state", seed, run_state_seed);
        total.commands += stats.commands;
        total.decodes += stats.decodes;
        total.placements += stats.placements;
        total.peak_used = total.peak_used.max(stats.peak_used);
        total.peak_images = total.peak_images.max(stats.peak_images);
    }
    eprintln!("state fuzz: {total:?} in {:.2?}", started.elapsed());
}

// ------------------------------------------------------ spec examples --

/// The specification's documented commands, with real payloads where it
/// shows placeholders, through the processor harness with decodes settled
/// after every feed: a sanity seed for the harness, and the replies the
/// specification documents.
#[test]
fn the_specifications_examples_run_clean() {
    let b64 = |bytes: &[u8]| encode_payload(bytes);
    let rgb = vec![0x40_u8; 10 * 20 * 3];
    let rgba = vec![0x40_u8; 100 * 30 * 4];
    let chunked = b64(&rgba);
    let (one, rest) = chunked.split_at(4096);
    let (two, three) = rest.split_at(4096);
    let mut png = png_header(10, 20);
    png.extend_from_slice(&[0; 40]);
    let shm = b64(b"/some-shared-memory-name");
    let examples: Vec<String> = vec![
        format!("\x1b_Gf=24,s=10,v=20;{}\x1b\\", b64(&rgb)),
        format!("\x1b_Gf=16,s=10,v=20;{}\x1b\\", b64(&rgb)),
        format!("\x1b_Gf=100;{}\x1b\\", b64(&png)),
        format!("\x1b_Gf=24,s=10,v=20,o=z;{}\x1b\\", b64(&zlib(&rgb))),
        format!("\x1b_Gf=100,t=f;{}\x1b\\", b64(b"/path/to/file.png")),
        format!("\x1b_Gs=10,v=2,t=s,o=z;{shm}\x1b\\"),
        format!("\x1b_Gs=10,v=2,t=s,S=80,O=10;{shm}\x1b\\"),
        format!("\x1b_Gs=100,v=30,m=1;{one}\x1b\\\x1b_Gm=1;{two}\x1b\\\x1b_Gm=0;{three}\x1b\\"),
        format!("\x1b_Gi=31,s=10,v=2,t=s;{shm}\x1b\\"),
        "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c".to_owned(),
        "\x1b_Ga=q,i=31,e=T,U=1,C=1\x1b\\".to_owned(),
        "\x1b_Ga=q,i=31,e=f,r=1,X=1\x1b\\".to_owned(),
        "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b_Ga=q,i=32,e=T,U=1\x1b\\\x1b_Ga=q,i=33,e=f\x1b\\\x1b[c"
            .to_owned(),
        format!("\x1b_Ga=T,i=10,f=24,s=10,v=20;{}\x1b\\text\r\n", b64(&rgb)),
        format!(
            "\x1b_Ga=T,i=5,f=24,s=10,v=20,q=2;{}\x1b\\\x1b_Ga=p,U=1,i=5,c=3,r=2\x1b\\\
             \x1b[38;5;5m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\u{10EEEE}\u{0305}\u{030E}\r\n\
             \u{10EEEE}\u{030D}\u{0305}\u{10EEEE}\u{030D}\u{030D}\u{10EEEE}\u{030D}\u{030E}\x1b[39m\r\n",
            b64(&rgb)
        ),
        "\x1b_Ga=p,i=10,p=2,P=10,Q=1,H=2,V=-1\x1b\\\x1b_Ga=p,i=10,p=1\x1b\\\x1b_Ga=p,i=10,p=3,P=10,Q=1\x1b\\\r\n"
            .to_owned(),
        "\x1b_Ga=d\x1b\\".to_owned(),
        "\x1b_Ga=d,d=i,i=10\x1b\\".to_owned(),
        "\x1b_Ga=d,d=i,i=10,p=7\x1b\\".to_owned(),
        "\x1b_Ga=d,d=Z,z=-1\x1b\\".to_owned(),
        "\x1b_Ga=d,d=p,x=3,y=4\x1b\\".to_owned(),
        "\x1b_Ga=a,i=3,c=7\x1b\\".to_owned(),
        "\x1b_Ga=a,i=7,r=3,z=48\x1b\\".to_owned(),
        "\x1b_Ga=c,i=1,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3\x1b\\".to_owned(),
    ];
    let mut trace = Vec::new();
    let mut harness = Harness::new(
        Setup {
            mode: GraphicsMode::Full,
            raw: true,
            host: true,
        },
        &mut trace,
    );
    let mut answers = Vec::new();
    for example in &examples {
        harness.replies.clear();
        harness.apply(Op::Feed(example.as_bytes().to_vec(), true));
        harness.apply(Op::EndBatch);
        harness.sync_requested();
        for n in 0..harness.requested.len() {
            harness.apply(Op::Settle(n, Outcome::Pixels));
        }
        harness.apply(Op::Drain);
        answers.push(String::from_utf8_lossy(&harness.replies).into_owned());
    }
    // The replies the specification documents.
    assert_eq!(answers[9], "\x1b_Gi=31;OK\x1b\\\x1b[?62;c");
    assert_eq!(answers[10], "\x1b_Gi=31,e=T,C=1,U=1;OK\x1b\\");
    // Frame support echoes the supported keys the query named, as kitty does.
    assert_eq!(answers[11], "\x1b_Gi=31,e=f,X=1,r=1;OK\x1b\\");
    assert!(answers[12].starts_with("\x1b_Gi=31;OK\x1b\\\x1b_Gi=32,e=T,U=1;OK\x1b\\"));
    assert!(answers[12].ends_with("\x1b[?62;c"));
    assert_eq!(answers[13], "\x1b_Gi=10;OK\x1b\\");
    // Local media the fake filesystem does not serve.
    assert_eq!(
        answers[8],
        "\x1b_Gi=31;EBADF:Failed to read image file\x1b\\"
    );
    assert!(
        harness.stats.placeholder_runs > 0,
        "the placeholder example drew runs: {:?}",
        harness.stats
    );
    assert!(harness.stats.relative_children > 0, "{:?}", harness.stats);
}

/// Found by this fuzz: an `ESC` arriving while a UTF-8 sequence was
/// incomplete was swallowed with it (vtparse printed one U+FFFD and dropped
/// the `ESC`), so the escape sequence it began was printed as text. For a
/// graphics APC that put the command and its payload on screen, and in the
/// raw capture, since no APC was ever seen; a CSI fared the same
/// (`\xe3\x1b[31m` printed `[31m`). The parser (the patched vtparse in
/// `patches/`) now follows Unicode's "maximal subpart" practice: the U+FFFD
/// stands for the truncated sequence, and the `ESC` begins its own.
#[test]
fn an_esc_cutting_utf8_short_still_begins_its_sequence() {
    let mut processor: VtProcessor<Collect> = VtProcessor::new(Collect::default());
    let mut parser = VTParser::new();
    for &byte in b"\xe3\x1b_Ga=d\x1b\\x\r\n" {
        if byte != b'\n' && byte != b'\r' {
            processor.push_raw_incoming_byte(byte);
        }
        parser.parse_byte(byte, &mut processor);
    }
    let lines: Vec<&StyledLine> = processor
        .sink_mut()
        .0
        .iter()
        .filter_map(|event| match event {
            VtEvent::HandleIncomingLine(line) => Some(&**line),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "\u{fffd}x");
    assert_eq!(lines[0].raw(), Some("\u{fffd}x"));
}

/// Found by this fuzz: a read batch (or prompt) ending in a character whose
/// UTF-8 encoding ends in 0x90, 0x98, 0x9B, 0x9D, 0x9E or 0x9F — `Ð`
/// (`C3 90`), Cyrillic `А` (`D0 90`) and `П` (`D0 9F`), `ẞ` (`E1 BA 9E`),
/// `語` (`E8 AA 9E`) — had that last byte held back from the partial line's
/// raw text as a lone C1 sequence introducer (`held_raw_start` in
/// `vt.rs`), splitting the character. When the character came through the
/// bulk path the capture was still marked verified, so `raw_text` took the
/// split bytes through `from_utf8_unchecked`: the `debug_assert` panicked
/// in a debug build, and a release build made an invalid `&str` (undefined
/// behaviour). Only a trailing `ESC` is held back now. Graphics play no
/// part: mode `Off`, no host; raw capture on (a raw-pattern trigger) is
/// enough.
#[test]
fn a_batch_ending_in_a_character_ending_in_a_c1_value_keeps_it_whole() {
    for text in ["\u{d0}", "\u{410}", "\u{41f}", "\u{1e9e}", "\u{8a9e}"] {
        let mut processor: VtProcessor<Collect> = VtProcessor::new(Collect::default());
        // The connection's bulk path for a run of validated text.
        processor.push_raw_incoming_run(text);
        processor.print_run(text);
        processor.notify_end_of_buffer();
        let raw: Vec<String> = processor
            .sink_mut()
            .0
            .iter()
            .filter_map(|event| match event {
                VtEvent::HandleIncomingPartialLine(line) => line.raw().map(str::to_owned),
                _ => None,
            })
            .collect();
        assert_eq!(raw, [text]);
    }
}
