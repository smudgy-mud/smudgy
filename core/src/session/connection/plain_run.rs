//! The Ground-state bulk path of the inbound byte loop.
//!
//! # Why
//!
//! Almost every inbound byte is printable text that the VT parser, idling in its Ground state,
//! forwards to `VTActor::print` one character at a time: a state-table lookup, a virtual call,
//! and a one-`char` `String::push` per byte. Escape sequences and controls are rare and short.
//! This module finds the leading run of bytes the parser would only ever print, so the loop can
//! hand the whole run over as one `push_str` (and one raw-capture `extend_from_slice`) and drop
//! back to the state machine only for the bytes that need it.
//!
//! # What qualifies
//!
//! A run ends at the first byte that can drive the state machine or that Ground would not print
//! verbatim:
//!
//! - any C0 control (`< 0x20`), which covers `ESC`, `CR`, `LF`, `BEL`, `TAB`, …;
//! - `DEL` (`0x7F`), whose handling belongs to the parser;
//! - any byte that is not part of a valid UTF-8 sequence — the parser's own decoder turns those
//!   into replacement characters by its rules, which this path must not second-guess;
//! - a UTF-8-encoded C1 control (`U+0080..=U+009F`, on the wire `C2 80..=C2 9F`) — the parser
//!   treats a decoded C1 as the control it names (`C2 9B` *is* CSI), so it must see those bytes.
//!
//! Telnet commands have already been removed. An escaped literal `IAC` can still arrive as
//! `0xFF`; it is invalid UTF-8 and remains on the parser path.
//!
//! # How the scan works
//!
//! The control test is a SWAR scan over 8-byte words (the classic "has a byte below n" and "has
//! a byte equal to n" bit tricks). Borrow propagation can flag bytes *above* a genuine hit, but
//! the lowest flagged byte is always exact, which is all a prefix search needs. No
//! target-specific SIMD is needed for this control scan.
//!
//! The scan also reports whether the run is pure ASCII. ASCII needs no UTF-8 validation at all;
//! a run with high bytes is validated with `simdutf8` (SIMD where the target has it), cut back
//! to its valid prefix, and then cut before any encoded C1 control. Text the transcoder already
//! decoded ([`PlainRuns::from_text`]) skips validation but not the C1 cut: a Latin-1 server's
//! `0x85` decodes to `U+0085`, which is still NEL.
//!
//! Control and validation boundaries are retained across parser fallbacks. In particular,
//! consuming one invalid byte or encoded C1 must not rescan the remaining buffer. Control
//! scanning advances once through each region; UTF-8 validation advances through each valid
//! prefix (the compat validator stops at the first erroneous SIMD block).

use memchr::memchr_iter;

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH_BITS: u64 = 0x8080_8080_8080_8080;

/// A mask whose lowest set bit marks the first byte of `word` that is a C0 control or DEL.
/// Higher set bits may be borrow artifacts; only the lowest is exact.
#[inline]
fn control_mask(word: u64) -> u64 {
    // Bytes below 0x20: subtracting 0x20 from every byte borrows exactly in those, and the
    // `!word` term keeps a high-bit byte (>= 0x80) from ever passing.
    let below_space = word.wrapping_sub(ONES * 0x20) & !word & HIGH_BITS;
    // Bytes equal to 0x7F: zero after the xor, then the "has a zero byte" trick.
    let xored = word ^ (ONES * 0x7F);
    let del = xored.wrapping_sub(ONES) & !xored & HIGH_BITS;
    below_space | del
}

/// The length of the leading run of `data` holding no C0 control and no DEL, and whether that
/// run is pure ASCII.
#[must_use]
pub fn control_free_prefix(data: &[u8]) -> (usize, bool) {
    let mut seen = 0u64;
    let mut consumed = 0;
    let (words, remainder) = data.as_chunks::<8>();
    for chunk in words {
        let word = u64::from_le_bytes(*chunk);
        let mask = control_mask(word);
        if mask != 0 {
            let index = (mask.trailing_zeros() / 8) as usize;
            // Only the bytes before the hit belong to the run (index 0 keeps nothing).
            seen |= word & ((1u64 << (index * 8)) - 1);
            return (consumed + index, seen & HIGH_BITS == 0);
        }
        seen |= word;
        consumed += 8;
    }
    for &byte in remainder {
        if byte < 0x20 || byte == 0x7F {
            break;
        }
        seen |= u64::from(byte);
        consumed += 1;
    }
    (consumed, seen & HIGH_BITS == 0)
}

/// Finds printable runs at the byte loop's cursor, retaining work across parser fallbacks.
/// Call [`Self::at`] with advancing offsets for linear scanning. Other offsets are safe too,
/// but may require scanning a region again. Only use returned runs while the parser is Ground.
pub struct PlainRuns<'a> {
    data: &'a [u8],
    verified: bool,
    control_start: usize,
    control_end: usize,
    ascii: bool,
    valid_start: usize,
    valid_end: usize,
    #[cfg(test)]
    control_scans: usize,
    #[cfg(test)]
    validation_bytes: usize,
}

impl<'a> PlainRuns<'a> {
    /// Bytes from the wire, validated before being returned as text.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            verified: false,
            control_start: 0,
            control_end: 0,
            ascii: false,
            valid_start: 0,
            valid_end: 0,
            #[cfg(test)]
            control_scans: 0,
            #[cfg(test)]
            validation_bytes: 0,
        }
    }

    /// Decoded text. The type enforces validity; callers cannot mark arbitrary bytes trusted.
    #[must_use]
    pub fn from_text(text: &'a str) -> Self {
        Self {
            verified: true,
            ..Self::new(text.as_bytes())
        }
    }

    /// The original input, including bytes that must still pass through the VT parser.
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        self.data
    }

    /// The printable prefix starting at `offset`, or empty for a control, invalid byte,
    /// continuation byte, encoded C1, or offset outside the input.
    pub fn at(&mut self, offset: usize) -> &'a str {
        let Some(&byte) = self.data.get(offset) else {
            return "";
        };
        // Besides avoiding an empty scan at common controls, this checks character boundaries
        // even for callers that probe the middle of trusted UTF-8 or a cached valid region.
        if byte < 0x20 || byte == 0x7F || (byte >= 0x80 && !(0xC2..=0xF4).contains(&byte)) {
            return "";
        }
        if offset < self.control_start || offset >= self.control_end {
            let (len, ascii) = control_free_prefix(&self.data[offset..]);
            self.control_start = offset;
            self.control_end = offset + len;
            self.ascii = ascii;
            self.valid_start = offset;
            self.valid_end = offset;
            #[cfg(test)]
            {
                self.control_scans += 1;
            }
        }
        let end = if self.ascii || self.verified {
            self.control_end
        } else {
            if offset < self.valid_start || offset >= self.valid_end {
                let candidate = &self.data[offset..self.control_end];
                self.valid_start = offset;
                self.valid_end = offset
                    + match simdutf8::compat::from_utf8(candidate) {
                        Ok(_) => candidate.len(),
                        Err(error) => error.valid_up_to(),
                    };
                #[cfg(test)]
                {
                    self.validation_bytes += candidate.len();
                }
            }
            self.valid_end
        };
        let run = &self.data[offset..end];
        let run = if self.ascii {
            run
        } else {
            before_encoded_c1(run)
        };
        debug_assert!(std::str::from_utf8(run).is_ok());
        // SAFETY: ASCII, a cached validated prefix, or original &str bytes. The start was
        // checked above; controls, valid_up_to, and encoded-C1 cuts all end on a boundary.
        unsafe { std::str::from_utf8_unchecked(run) }
    }
}

/// `run` cut before the first UTF-8-encoded C1 control (`C2 80..=C2 9F`). In valid UTF-8 a
/// `C2` byte is always a lead byte, so a hit is never a continuation byte in disguise.
fn before_encoded_c1(run: &[u8]) -> &[u8] {
    for index in memchr_iter(0xC2, run) {
        if matches!(run.get(index + 1), Some(0x80..=0x9F)) {
            return &run[..index];
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::{PlainRuns, control_free_prefix};

    fn printable_run(data: &[u8]) -> &str {
        PlainRuns::new(data).at(0)
    }

    /// Every byte the scan must stop at.
    fn stop_bytes() -> impl Iterator<Item = u8> {
        (0x00..0x20).chain(std::iter::once(0x7F))
    }

    #[test]
    fn prefix_stops_at_every_control_in_every_position() {
        // Lengths straddle the 8-byte word boundary in both the word and the remainder loops.
        for len in 0..40 {
            for stop in stop_bytes() {
                for at in 0..len {
                    let mut data = vec![b'x'; len];
                    data[at] = stop;
                    assert_eq!(
                        control_free_prefix(&data),
                        (at, true),
                        "len {len}, stop {stop:#04x} at {at}"
                    );
                }
            }
            let clean = vec![b'x'; len];
            assert_eq!(control_free_prefix(&clean), (len, true), "clean len {len}");
        }
    }

    #[test]
    fn prefix_passes_every_printable_and_high_byte() {
        for byte in (0x20..0x7F).chain(0x80..=0xFF) {
            let data = [byte; 17];
            let (len, ascii) = control_free_prefix(&data);
            assert_eq!(len, 17, "byte {byte:#04x}");
            assert_eq!(ascii, byte < 0x80, "byte {byte:#04x}");
        }
    }

    #[test]
    fn ascii_flag_only_counts_bytes_inside_the_run() {
        // A high byte after the stop must not taint the run's ASCII verdict.
        assert_eq!(control_free_prefix(b"ascii\x1b\xc3\xa9"), (5, true));
        // A high byte before the stop does.
        assert_eq!(control_free_prefix(b"caf\xc3\xa9\n"), (5, false));
        // Straddling words: the high byte in the first word, the stop in the second.
        assert_eq!(control_free_prefix(b"caf\xc3\xa9 and more\n"), (14, false));
    }

    #[test]
    fn run_is_the_whole_valid_text() {
        let text = "héllo wörld ✓ 你好 🙂 and ascii";
        assert_eq!(printable_run(text.as_bytes()), text);
        assert_eq!(PlainRuns::from_text(text).at(0), text);
    }

    #[test]
    fn run_stops_before_invalid_and_truncated_sequences() {
        assert_eq!(printable_run(b"ab\xff\xfecd"), "ab");
        assert_eq!(printable_run(b"\xffcd"), "");
        // Overlong, surrogate, and out-of-range leads are all invalid.
        assert_eq!(printable_run(b"x\xc0\x80"), "x");
        assert_eq!(printable_run(b"x\xed\xa0\x80"), "x");
        assert_eq!(printable_run(b"x\xf5"), "x");
        // A sequence cut by the read boundary is not printed early.
        let cut = &"ok 你".as_bytes()[..5];
        assert_eq!(printable_run(cut), "ok ");
        // A lone continuation byte.
        assert_eq!(printable_run(b"x\x80y"), "x");
    }

    #[test]
    fn run_stops_before_an_encoded_c1_control_regardless_of_trust() {
        let text = "before\u{9b}31mafter";
        assert_eq!(printable_run(text.as_bytes()), "before");
        assert_eq!(PlainRuns::from_text(text).at(0), "before");
        assert_eq!(PlainRuns::from_text("\u{85}x").at(0), "");
        // U+00A0..=U+00FF share the C2/C3 leads but are printable.
        let text = "\u{a0}\u{bf}\u{c0}\u{ff}";
        assert_eq!(printable_run(text.as_bytes()), text);
    }

    #[test]
    fn run_ends_at_the_first_control() {
        assert_eq!(printable_run(b"text\x1b[31m"), "text");
        assert_eq!(printable_run(b"\x1b[31mtext"), "");
        assert_eq!(printable_run(b"a\x7fb"), "a");
        assert_eq!(printable_run(b""), "");
    }

    #[test]
    fn fallback_retains_scan_boundaries_as_input_grows() {
        for len in [1024, 4096, 16_384, 65_536] {
            // Both malformed data interspersed with ASCII and valid encoded controls used
            // to restart the full control scan on almost every byte/code point.
            for seed in [&b"x\xfe"[..], &b"\xc2\x85"[..], &b"x\xc2\x85"[..]] {
                let data = seed.repeat(len / seed.len());
                let mut runs = PlainRuns::new(&data);
                let mut offset = 0;
                while offset < data.len() {
                    offset += runs.at(offset).len().max(1);
                }
                assert_eq!(runs.control_scans, 1, "{len} bytes, seed {seed:?}");
                if std::str::from_utf8(&data).is_ok() {
                    // C1 fallbacks must reuse successful validation of the remaining text.
                    assert_eq!(runs.validation_bytes, data.len());
                }
            }
        }
    }

    #[test]
    fn trusted_and_cached_text_reject_offsets_inside_codepoints() {
        let text = "é你🙂 plain\u{85}text";
        for mut runs in [PlainRuns::new(text.as_bytes()), PlainRuns::from_text(text)] {
            // Warm the caches, then probe offsets out of order as well as at every byte.
            assert_eq!(runs.at(0), "é你🙂 plain");
            for offset in (0..=text.len() + 1).rev() {
                let result = runs.at(offset);
                assert!(std::str::from_utf8(result.as_bytes()).is_ok());
                if !text.is_char_boundary(offset) || offset >= text.len() {
                    assert!(result.is_empty(), "offset {offset}");
                }
            }
            assert_eq!(runs.at(usize::MAX), "");
        }
    }
}
