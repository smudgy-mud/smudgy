//! MCCP inbound decompression (`docs/telnet.md` Phase 3 + 5) — the stage
//! between the socket and the telnet parser once a compression-start marker arrives.
//!
//! Compression wraps the *telnet stream*: after `IAC SB MCCP2 IAC SE` (or the MCCPX
//! `BEGIN_ENCODING` marker), every byte the server sends — negotiation, subnegotiations,
//! application text alike — is one compressed stream. The telnet parser halts at the marker
//! ([`TelnetParser::receive`](super::telnet::TelnetParser) returns the consumed count), the
//! connection flips this stage on, and from then on socket bytes are inflated in bounded
//! chunks that feed the same parser, whose state carries over seamlessly.
//!
//! Two codecs: zlib deflate (MCCP2, and MCCPX `deflate`) and zstd (MCCPX `zstd`). The output
//! buffer is caller-owned and capped per step at [`INFLATE_CHUNK`], which bounds memory
//! against compression bombs by construction: a hostile ratio costs time (paced by the
//! caller's yield cadence), never unbounded allocation. A corrupt stream is unrecoverable by
//! nature — there is no way to re-find a plaintext boundary — so [`Inflow::step`]'s error is
//! disconnect-grade.

use std::io;

use flate2::{Decompress, FlushDecompress, Status};
use zstd::stream::raw::{Decoder as ZstdDecoder, InBuffer, Operation, OutBuffer};

/// The most decompressed output produced per [`Inflow::step`] call. Also the natural
/// granularity for the caller's pacing (commit + yield cadence under a high-ratio burst).
pub const INFLATE_CHUNK: usize = 64 * 1024;

/// A negotiated compression codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// zlib deflate (MCCP2; MCCPX `deflate`).
    Deflate,
    /// Zstandard (MCCPX `zstd`).
    Zstd,
}

/// The inbound byte-stream transform ahead of the telnet parser.
pub enum Inflow {
    /// No compression negotiated (or the stream ended): bytes pass to the parser verbatim.
    Plain,
    /// A zlib deflate stream is active.
    Inflate(Box<Decompress>),
    /// A zstd stream is active.
    Zstd(Box<ZstdDecoder<'static>>),
}

impl std::fmt::Debug for Inflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Plain => "Inflow::Plain",
            Self::Inflate(_) => "Inflow::Inflate",
            Self::Zstd(_) => "Inflow::Zstd",
        })
    }
}

/// One [`Inflow::step`]'s outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateStep {
    /// `consumed` input bytes were read; the stream continues. The output buffer may hold
    /// up to [`INFLATE_CHUNK`] decompressed bytes (possibly zero while the codec buffers).
    Progress { consumed: usize },
    /// The stream ended cleanly after `consumed` input bytes; whatever input follows is plain
    /// telnet again, and the server may renegotiate later.
    End { consumed: usize },
}

impl Inflow {
    #[must_use]
    pub const fn is_plain(&self) -> bool {
        matches!(self, Self::Plain)
    }

    /// Begin a compression stream with `codec` (compression starts with the byte after the
    /// marker).
    ///
    /// # Errors
    ///
    /// If the zstd decoder fails to initialize (allocation).
    pub fn begin(&mut self, codec: Codec) -> io::Result<()> {
        *self = match codec {
            Codec::Deflate => Self::Inflate(Box::new(Decompress::new(true))),
            Codec::Zstd => Self::Zstd(Box::new(ZstdDecoder::new()?)),
        };
        Ok(())
    }

    /// Revert to the pass-through (an orderly stream end).
    pub fn end(&mut self) {
        *self = Self::Plain;
    }

    /// Inflate one bounded chunk: reads some of `input`, replaces `out`'s contents with up
    /// to [`INFLATE_CHUNK`] decompressed bytes. `Progress { consumed: 0 }` with an empty
    /// `out` means more input is needed.
    ///
    /// # Errors
    ///
    /// A corrupt/desynced stream. Unrecoverable — the caller must tear the connection down.
    ///
    /// # Panics
    ///
    /// Panics on [`Inflow::Plain`] — the caller routes plain bytes straight to the parser.
    pub fn step(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<InflateStep> {
        match self {
            Self::Plain => unreachable!("step is only called while a compression stream is active"),
            Self::Inflate(z) => {
                out.clear();
                // `reserve_exact`, not `reserve`: `decompress_vec` fills up to `capacity()`,
                // so an over-allocating `reserve` would let one step exceed INFLATE_CHUNK —
                // breaking both the bomb bound and the caller's pacing. Exact keeps it true.
                out.reserve_exact(INFLATE_CHUNK - out.len());
                let before = z.total_in();
                let status = z
                    .decompress_vec(input, out, FlushDecompress::None)
                    .map_err(io::Error::other)?;
                let consumed = usize::try_from(z.total_in() - before).unwrap_or(usize::MAX);
                match status {
                    Status::StreamEnd => Ok(InflateStep::End { consumed }),
                    Status::Ok | Status::BufError => Ok(InflateStep::Progress { consumed }),
                }
            }
            Self::Zstd(dec) => {
                // Decode straight into the reused buffer's spare capacity — no zero-fill.
                // zstd's `WriteBuf for Vec<u8>` writes through the Vec's raw pointer and sets
                // its length via `filled_until` (on the `OutBuffer` drop), so `out` ends the
                // call sized exactly to the produced bytes — no memset, no copy. `reserve_exact`
                // keeps one step's output bounded by INFLATE_CHUNK (the bomb guard); the caller
                // drives repeated steps (empty input once the read is consumed) to flush a frame
                // whose output exceeds one window.
                out.clear();
                out.reserve_exact(INFLATE_CHUNK);
                let mut in_buf = InBuffer::around(input);
                let mut out_buf = OutBuffer::around(&mut *out);
                let hint = dec.run(&mut in_buf, &mut out_buf)?;
                let consumed = in_buf.pos();
                let produced = out_buf.pos();
                // Drop the OutBuffer so its `WriteBuf::filled_until` syncs `out.len()` to
                // `produced` before the caller reads `out`.
                drop(out_buf);
                debug_assert_eq!(out.len(), produced);
                // `run` returns 0 at a frame boundary. The MCCPX draft terminates a stream
                // with a single finished frame (`ZSTD_e_end`), so a frame boundary is the
                // stream end — revert to plain telnet.
                if hint == 0 {
                    Ok(InflateStep::End { consumed })
                } else {
                    Ok(InflateStep::Progress { consumed })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Codec, INFLATE_CHUNK, InflateStep, Inflow};
    use flate2::{Compress, Compression, FlushCompress};

    /// zlib-compress `data` as one finished stream.
    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut z = Compress::new(Compression::default(), true);
        let mut out = Vec::with_capacity(data.len() + 64);
        z.compress_vec(data, &mut out, FlushCompress::Finish)
            .expect("compress");
        out
    }

    /// Drive `inflow` over `input`, returning (all decompressed bytes, input consumed,
    /// whether the stream ended).
    fn drain(inflow: &mut Inflow, mut input: &[u8]) -> (Vec<u8>, usize, bool) {
        let mut out = Vec::new();
        let mut all = Vec::new();
        let mut total = 0;
        loop {
            let step = inflow.step(input, &mut out).expect("valid stream");
            let (consumed, ended) = match step {
                InflateStep::Progress { consumed } => (consumed, false),
                InflateStep::End { consumed } => (consumed, true),
            };
            all.extend_from_slice(&out);
            total += consumed;
            input = &input[consumed..];
            if ended {
                return (all, total, true);
            }
            if consumed == 0 && out.is_empty() {
                return (all, total, false);
            }
        }
    }

    #[test]
    fn a_finished_stream_round_trips_and_reports_end() {
        let plain = b"You see a fearsome dragon here.\r\n".repeat(50);
        let wire = deflate(&plain);
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Deflate).unwrap();
        let (out, consumed, ended) = drain(&mut inflow, &wire);
        assert_eq!(out, plain);
        assert!(ended, "Z_FINISH must surface as End");
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn stream_end_mid_buffer_leaves_the_tail_unconsumed() {
        // A finished stream followed by plain bytes in one buffer: End's consumed count
        // stops exactly at the stream boundary so the caller feeds the tail as plain.
        let wire = [deflate(b"compressed part"), b"PLAIN TAIL".to_vec()].concat();
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Deflate).unwrap();
        let (out, consumed, ended) = drain(&mut inflow, &wire);
        assert_eq!(out, b"compressed part");
        assert!(ended);
        assert_eq!(&wire[consumed..], b"PLAIN TAIL");
    }

    #[test]
    fn input_split_at_every_boundary_still_decodes() {
        let plain = b"line of text\r\n".repeat(20);
        let wire = deflate(&plain);
        for split in 1..wire.len() {
            let mut inflow = Inflow::Plain;
            inflow.begin(Codec::Deflate).unwrap();
            let (mut out, consumed_a, ended_a) = drain(&mut inflow, &wire[..split]);
            assert!(!ended_a || consumed_a == split);
            let (out_b, _, ended_b) = drain(&mut inflow, &wire[consumed_a..]);
            out.extend_from_slice(&out_b);
            assert!(ended_b || ended_a, "split {split}: stream must finish");
            assert_eq!(out, plain, "split {split}");
        }
    }

    #[test]
    fn output_per_step_is_bounded_against_bombs() {
        // Highly compressible input (a bomb's shape): no single step may exceed the chunk
        // cap, so memory stays bounded no matter the ratio.
        let plain = vec![b'x'; INFLATE_CHUNK * 8];
        let wire = deflate(&plain);
        assert!(
            wire.len() < plain.len() / 100,
            "the corpus must actually be bomb-shaped"
        );
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Deflate).unwrap();
        let mut out = Vec::new();
        let mut input = &wire[..];
        let mut total = 0;
        loop {
            let step = inflow.step(input, &mut out).expect("valid");
            assert!(out.len() <= INFLATE_CHUNK, "step output exceeded the cap");
            total += out.len();
            let (consumed, ended) = match step {
                InflateStep::Progress { consumed } => (consumed, false),
                InflateStep::End { consumed } => (consumed, true),
            };
            input = &input[consumed..];
            if ended {
                break;
            }
        }
        assert_eq!(total, plain.len());
    }

    #[test]
    fn garbage_is_a_hard_error() {
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Deflate).unwrap();
        let mut out = Vec::new();
        assert!(inflow.step(b"\xFF\xFEnot zlib at all", &mut out).is_err());
    }

    /// zstd-compress `data` as one finished frame.
    fn zstd_frame(data: &[u8]) -> Vec<u8> {
        zstd::bulk::compress(data, 3).expect("zstd compress")
    }

    #[test]
    fn a_zstd_frame_round_trips_and_reports_end() {
        let plain = b"A zstd-compressed room description.\r\n".repeat(50);
        let wire = zstd_frame(&plain);
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Zstd).unwrap();
        let (out, consumed, ended) = drain(&mut inflow, &wire);
        assert_eq!(out, plain);
        assert!(ended, "the frame boundary must surface as End");
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn zstd_stream_end_mid_buffer_leaves_the_tail_unconsumed() {
        let wire = [zstd_frame(b"compressed part"), b"PLAIN TAIL".to_vec()].concat();
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Zstd).unwrap();
        let (out, consumed, ended) = drain(&mut inflow, &wire);
        assert_eq!(out, b"compressed part");
        assert!(ended);
        assert_eq!(&wire[consumed..], b"PLAIN TAIL");
    }

    #[test]
    fn zstd_split_across_reads_decodes() {
        let plain = b"zstd line\r\n".repeat(30);
        let wire = zstd_frame(&plain);
        let split = wire.len() / 2;
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Zstd).unwrap();
        let (mut out, consumed_a, _) = drain(&mut inflow, &wire[..split]);
        let (out_b, _, ended) = drain(&mut inflow, &wire[consumed_a..]);
        out.extend_from_slice(&out_b);
        assert!(ended);
        assert_eq!(out, plain);
    }

    #[test]
    fn zstd_garbage_is_a_hard_error() {
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Zstd).unwrap();
        let mut out = Vec::new();
        assert!(
            inflow
                .step(b"\x00\x01not a zstd frame at all", &mut out)
                .is_err()
        );
    }

    /// A zstd frame whose output far exceeds one INFLATE_CHUNK window: the whole frame must
    /// decode (the `drain` helper steps past the input emptying, mirroring the connect task's
    /// drain loop, to flush the decoder's internally buffered output and reach the End).
    #[test]
    fn zstd_frame_larger_than_one_window_fully_drains() {
        let plain = b"repeated content line that compresses well\r\n".repeat(4000);
        assert!(plain.len() > INFLATE_CHUNK * 2, "must exceed one window");
        let wire = zstd_frame(&plain);
        let mut inflow = Inflow::Plain;
        inflow.begin(Codec::Zstd).unwrap();
        let (out, _consumed, ended) = drain(&mut inflow, &wire);
        assert!(ended, "the frame must reach End");
        assert_eq!(out, plain, "every byte of a multi-window frame decodes");
    }
}
