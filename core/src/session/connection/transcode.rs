//! Charset transcoding between the wire and the client's UTF-8 pipeline
//! (`docs/telnet.md` Phase 2).
//!
//! Everything downstream of the telnet layer — vtparse, `StyledLine`, triggers, scripts —
//! assumes UTF-8. This stage converts a non-UTF-8 server's application bytes to UTF-8 on the
//! way in (after telnet stripping: VT escapes are ASCII in every supported encoding, and the
//! parser has already un-escaped `IAC IAC`, so bytes arrive here exactly as the encoding
//! produced them) and encodes outbound command text on the way out (before telnet framing,
//! then doubles any `0xFF` the encoding produced — Latin-1 `ÿ` is a real `IAC` hazard that
//! UTF-8 output can never produce).
//!
//! The active encoding comes from the per-server setting, overridden mid-connection by a
//! CHARSET negotiation (RFC 2066) at the exact stream position of the `ACCEPTED` reply. The
//! decoder is streaming — a multibyte character split across TCP reads decodes correctly —
//! which is precisely why this wraps `encoding_rs` instead of a hand-rolled table.
//!
//! **UTF-8 connections (the overwhelming default) never enter this module's convert paths:**
//! [`Transcode::is_passthrough`] is a single load, and the caller feeds bytes onward
//! untouched, preserving the ingest fast path byte for byte.

use encoding_rs::{Decoder, Encoding, UTF_8};

/// A whole outbound command cannot be represented in the active server
/// encoding. No bytes from that command are returned to the socket layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeError {
    encoding: &'static str,
    character: char,
}

impl EncodeError {
    #[must_use]
    pub const fn character(self) -> char {
        self.character
    }
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "outbound text cannot be represented in {}; command was not sent",
            self.encoding
        )
    }
}

impl std::error::Error for EncodeError {}

/// Per-connection transcoding state, owned by the connect task like the telnet parser.
pub struct Transcode {
    encoding: &'static Encoding,
    /// `None` on the UTF-8 pass-through (no decoder is ever constructed for it).
    decoder: Option<Decoder>,
    /// Reused inbound UTF-8 output.
    in_buf: String,
    /// Reused outbound scratch (encoded, pre-doubling) and final (IAC-doubled) buffers.
    scratch: Vec<u8>,
    out_buf: Vec<u8>,
}

impl std::fmt::Debug for Transcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcode")
            .field("encoding", &self.encoding.name())
            .finish_non_exhaustive()
    }
}

impl Default for Transcode {
    /// The UTF-8 pass-through — what every connection without a configured or negotiated
    /// charset runs.
    fn default() -> Self {
        Self::new(UTF_8)
    }
}

impl Transcode {
    #[must_use]
    pub fn new(encoding: &'static Encoding) -> Self {
        let convert = encoding != UTF_8;
        Self {
            encoding,
            decoder: convert.then(|| encoding.new_decoder()),
            in_buf: String::new(),
            scratch: Vec::new(),
            out_buf: Vec::new(),
        }
    }

    /// Whether this connection is plain UTF-8 — the caller skips both convert stages
    /// entirely, so the default pays one branch and nothing else.
    #[must_use]
    pub fn is_passthrough(&self) -> bool {
        self.decoder.is_none()
    }

    #[must_use]
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// Switch encodings mid-connection (a CHARSET `ACCEPTED`). Fresh coders: the switch
    /// happens at a subnegotiation boundary, so no partial sequence is legitimately in
    /// flight; any carried decoder state belonged to the old encoding anyway.
    pub fn switch_to(&mut self, encoding: &'static Encoding) {
        *self = Self::new(encoding);
    }

    /// Decode one run of inbound application bytes to UTF-8, carrying partial multibyte
    /// sequences to the next call. Invalid sequences become U+FFFD, mirroring the lossy
    /// posture the UTF-8 path has at line-bake time.
    ///
    /// # Panics
    ///
    /// Panics on the pass-through — the caller must branch on [`Self::is_passthrough`]
    /// first (this keeps the borrow of the internal buffer out of the hot path entirely).
    pub fn decode(&mut self, data: &[u8]) -> &str {
        let decoder = self
            .decoder
            .as_mut()
            .expect("decode is only called on converting connections");
        self.in_buf.clear();
        let capacity = decoder
            .max_utf8_buffer_length(data.len())
            .unwrap_or(data.len().saturating_mul(3) + 16);
        self.in_buf.reserve(capacity);
        // With capacity reserved per max_utf8_buffer_length, a single call consumes the
        // whole input; the loop is belt-and-suspenders for the Option overflow fallback.
        let mut src = data;
        loop {
            let (result, read, _replaced) = decoder.decode_to_string(src, &mut self.in_buf, false);
            src = &src[read..];
            match result {
                encoding_rs::CoderResult::InputEmpty => break,
                encoding_rs::CoderResult::OutputFull => {
                    self.in_buf.reserve(src.len().saturating_mul(3) + 16);
                }
            }
        }
        &self.in_buf
    }

    /// Encode one complete outbound text write into the active encoding and
    /// double any `0xFF` byte so it survives the telnet layer as a literal.
    ///
    /// Encoding is atomic at the command boundary: an unmappable character
    /// returns [`EncodeError`] and exposes none of the representable prefix.
    /// A fresh encoder with `last = true` makes each command self-contained
    /// even for stateful encodings such as ISO-2022-JP.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when any character is not representable. No
    /// encoded prefix is retained or exposed in that case.
    ///
    /// # Panics
    ///
    /// Panics on the pass-through — the caller must branch on [`Self::is_passthrough`]
    /// first (UTF-8 output cannot contain `0xFF`, so it needs neither stage).
    pub fn encode_outbound(&mut self, text: &str) -> Result<&[u8], EncodeError> {
        assert!(
            !self.is_passthrough(),
            "encode_outbound is only called on converting connections"
        );
        let mut encoder = self.encoding.new_encoder();
        self.scratch.clear();
        let capacity = encoder
            .max_buffer_length_from_utf8_without_replacement(text.len())
            .unwrap_or(text.len().saturating_mul(2) + 16);
        self.scratch.reserve(capacity.max(16));

        let mut src = text;
        loop {
            let (result, read) =
                encoder.encode_from_utf8_to_vec_without_replacement(src, &mut self.scratch, true);
            src = &src[read..];
            match result {
                encoding_rs::EncoderResult::InputEmpty => break,
                encoding_rs::EncoderResult::OutputFull => {
                    self.scratch.reserve(self.scratch.capacity().max(64));
                }
                encoding_rs::EncoderResult::Unmappable(character) => {
                    self.scratch.clear();
                    self.out_buf.clear();
                    return Err(EncodeError {
                        encoding: self.encoding.name(),
                        character,
                    });
                }
            }
        }

        // Double 0xFF bytes (IAC) so the telnet layer delivers them as literals.
        self.out_buf.clear();
        super::telnet::double_iac_into(&self.scratch, &mut self.out_buf);
        Ok(&self.out_buf)
    }

    /// Flush the decoder at end of stream: a partial multibyte sequence still buffered
    /// when the socket closes surfaces as U+FFFD instead of vanishing. Returns the flushed
    /// text (usually empty); a no-op on the pass-through.
    pub fn finish(&mut self) -> &str {
        self.in_buf.clear();
        if let Some(decoder) = self.decoder.as_mut() {
            self.in_buf.reserve(8);
            let (_, _, _) = decoder.decode_to_string(&[], &mut self.in_buf, true);
            // The decoder is spent after a `last = true` call; a fresh one keeps any
            // late use of this connection's state well-defined.
            self.decoder = Some(self.encoding.new_decoder());
        }
        &self.in_buf
    }
}

#[cfg(test)]
mod tests {
    use super::Transcode;
    use encoding_rs::{BIG5, UTF_8, WINDOWS_1252};

    #[test]
    fn utf8_is_a_pure_passthrough() {
        let t = Transcode::new(UTF_8);
        assert!(t.is_passthrough());
    }

    #[test]
    fn latin1_bytes_decode_to_utf8() {
        let mut t = Transcode::new(WINDOWS_1252);
        assert!(!t.is_passthrough());
        assert_eq!(t.decode(&[0xE9, b'!', 0xFF]), "\u{e9}!\u{ff}"); // é ! ÿ
    }

    #[test]
    fn a_multibyte_character_split_across_reads_decodes_once() {
        // Big5 "你" = 0xA7 0x41; feed the two bytes in separate calls — the decoder
        // must carry the partial sequence, yielding nothing then the whole character.
        let mut t = Transcode::new(BIG5);
        assert_eq!(t.decode(&[0xA7]), "");
        assert_eq!(t.decode(&[0x41]), "\u{4f60}");
    }

    #[test]
    fn invalid_sequences_decode_lossily() {
        let mut t = Transcode::new(BIG5);
        // A Big5 lead byte followed by an impossible trail decodes with replacement,
        // never panics, and resyncs for what follows.
        let out = t.decode(&[0xA7, 0x00, b'o', b'k']).to_string();
        assert!(out.contains('\u{fffd}'), "lossy replacement expected: {out:?}");
        assert!(out.ends_with("ok"));
    }

    #[test]
    fn outbound_latin1_doubles_the_ff_byte() {
        // 'ÿ' encodes to 0xFF in windows-1252 — exactly the IAC byte — and must go out
        // doubled so the server's telnet layer reads a literal.
        let mut t = Transcode::new(WINDOWS_1252);
        assert_eq!(t.encode_outbound("say \u{ff}").unwrap(), b"say \xFF\xFF");
    }

    #[test]
    fn outbound_unmappable_rejects_the_whole_command_and_resets_cleanly() {
        let mut t = Transcode::new(WINDOWS_1252);
        let error = t
            .encode_outbound("go \u{2192} east")
            .expect_err("an unmappable command must be rejected");
        assert_eq!(error.character(), '\u{2192}');
        assert!(
            t.scratch.is_empty(),
            "representable prefix must be discarded"
        );
        assert!(t.out_buf.is_empty(), "no bytes may reach the socket");
        assert_eq!(
            t.encode_outbound("go east").unwrap(),
            b"go east",
            "the rejected command must not corrupt the next encoder"
        );
    }

    #[test]
    fn finish_flushes_a_pending_partial_sequence_as_replacement() {
        let mut t = Transcode::new(BIG5);
        assert_eq!(t.decode(&[0xA7]), "", "lead byte pends inside the decoder");
        assert_eq!(t.finish(), "\u{fffd}", "the flush surfaces it as U+FFFD");
        // The decoder is fresh afterward: a full character still decodes.
        assert_eq!(t.decode(&[0xA7, 0x41]), "\u{4f60}");
        // A clean stream flushes to nothing.
        assert_eq!(t.finish(), "");
    }

    #[test]
    fn switch_to_resets_coders() {
        let mut t = Transcode::new(BIG5);
        assert_eq!(t.decode(&[0xA7]), "", "half a character pending");
        t.switch_to(WINDOWS_1252);
        // The pending Big5 lead byte is gone; Latin-1 decodes cleanly.
        assert_eq!(t.decode(&[0xE9]), "\u{e9}");
    }
}
