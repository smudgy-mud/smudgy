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
//! - `DEL` (`0x7F`), which Ground ignores rather than prints;
//! - any byte that is not part of a valid UTF-8 sequence — the parser's own decoder turns those
//!   into replacement characters by its rules, which this path must not second-guess;
//! - a UTF-8-encoded C1 control (`U+0080..=U+009F`, on the wire `C2 80..=C2 9F`) — the parser
//!   treats a decoded C1 as the control it names (`C2 9B` *is* CSI), so it must see those bytes.
//!
//! `IAC` never reaches this layer (the telnet preprocessor consumes it), and being `0xFF` it is
//! outside every accepted class anyway.
//!
//! # How the scan works
//!
//! The control test is a SWAR scan over 8-byte words (the classic "has a byte below n" and "has
//! a byte equal to n" bit tricks). Borrow propagation can flag bytes *above* a genuine hit, but
//! the lowest flagged byte is always exact, which is all a prefix search needs. No
//! target-specific SIMD: this scan sits under a memcpy-class copy and a per-line commit that both
//! dwarf it, and one portable code path serves the x86-64, aarch64, and Linux builds alike.
//!
//! The scan also reports whether the run is pure ASCII. ASCII needs no UTF-8 validation at all;
//! a run with high bytes is validated with `simdutf8` (SIMD where the target has it), cut back
//! to its valid prefix, and then cut before any encoded C1 control. Text the transcoder already
//! decoded ([`Utf8Trust::Verified`]) skips validation but not the C1 cut: a Latin-1 server's
//! `0x85` decodes to `U+0085`, which is still NEL.

use memchr::memchr_iter;

/// Whether the caller already knows the bytes are valid UTF-8.
///
/// `Verified` is a promise, not a hint: [`printable_run`] converts a verified run without a
/// validation pass. The only source of verified bytes is transcoder output, which is `&str` by
/// construction, and the byte loop keeps its cursor on a character boundary whenever the parser
/// is in Ground (the parser leaves its UTF-8 state exactly at the end of a code point).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utf8Trust {
    /// Bytes straight off the wire: validated before any bulk print.
    Unverified,
    /// Text the transcoder produced: valid by construction, so only the C1 cut applies.
    Verified,
}

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
    let mut words = data.chunks_exact(8);
    for chunk in &mut words {
        let word = u64::from_le_bytes(chunk.try_into().expect("chunks_exact yields 8 bytes"));
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
    for &byte in words.remainder() {
        if byte < 0x20 || byte == 0x7F {
            break;
        }
        seen |= u64::from(byte);
        consumed += 1;
    }
    (consumed, seen & HIGH_BITS == 0)
}

/// The leading run of `data` the VT parser would print verbatim from Ground. Empty when the
/// first byte needs the state machine: a control, DEL, invalid UTF-8, or an encoded C1.
#[must_use]
pub fn printable_run(data: &[u8], trust: Utf8Trust) -> &str {
    let (len, ascii) = control_free_prefix(data);
    let run = &data[..len];
    if ascii {
        debug_assert!(run.is_ascii());
        // SAFETY: every byte is below 0x80, and ASCII is valid UTF-8.
        return unsafe { std::str::from_utf8_unchecked(run) };
    }
    let run = match trust {
        Utf8Trust::Verified => run,
        Utf8Trust::Unverified => match simdutf8::compat::from_utf8(run) {
            Ok(_) => run,
            Err(error) => &run[..error.valid_up_to()],
        },
    };
    let run = before_encoded_c1(run);
    debug_assert!(std::str::from_utf8(run).is_ok());
    // SAFETY: `run` is valid UTF-8 — validated just above, or promised by `Verified` (see the
    // enum) — and every cut lands on a character boundary: a control byte is ASCII, and both
    // `valid_up_to` and the C1 cut stop before a lead byte.
    unsafe { std::str::from_utf8_unchecked(run) }
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
    use super::{Utf8Trust, control_free_prefix, printable_run};

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
        assert_eq!(printable_run(text.as_bytes(), Utf8Trust::Unverified), text);
        assert_eq!(printable_run(text.as_bytes(), Utf8Trust::Verified), text);
    }

    #[test]
    fn run_stops_before_invalid_and_truncated_sequences() {
        assert_eq!(printable_run(b"ab\xff\xfecd", Utf8Trust::Unverified), "ab");
        assert_eq!(printable_run(b"\xffcd", Utf8Trust::Unverified), "");
        // Overlong, surrogate, and out-of-range leads are all invalid.
        assert_eq!(printable_run(b"x\xc0\x80", Utf8Trust::Unverified), "x");
        assert_eq!(printable_run(b"x\xed\xa0\x80", Utf8Trust::Unverified), "x");
        assert_eq!(printable_run(b"x\xf5", Utf8Trust::Unverified), "x");
        // A sequence cut by the read boundary is not printed early.
        let cut = &"ok 你".as_bytes()[..5];
        assert_eq!(printable_run(cut, Utf8Trust::Unverified), "ok ");
        // A lone continuation byte.
        assert_eq!(printable_run(b"x\x80y", Utf8Trust::Unverified), "x");
    }

    #[test]
    fn run_stops_before_an_encoded_c1_control_regardless_of_trust() {
        let text = "before\u{9b}31mafter";
        for trust in [Utf8Trust::Unverified, Utf8Trust::Verified] {
            assert_eq!(printable_run(text.as_bytes(), trust), "before", "{trust:?}");
        }
        assert_eq!(printable_run("\u{85}x".as_bytes(), Utf8Trust::Verified), "");
        // U+00A0..=U+00FF share the C2/C3 leads but are printable.
        let text = "\u{a0}\u{bf}\u{c0}\u{ff}";
        assert_eq!(printable_run(text.as_bytes(), Utf8Trust::Unverified), text);
    }

    #[test]
    fn run_ends_at_the_first_control() {
        assert_eq!(
            printable_run(b"text\x1b[31m", Utf8Trust::Unverified),
            "text"
        );
        assert_eq!(printable_run(b"\x1b[31mtext", Utf8Trust::Unverified), "");
        assert_eq!(printable_run(b"a\x7fb", Utf8Trust::Unverified), "a");
        assert_eq!(printable_run(b"", Utf8Trust::Unverified), "");
    }
}
