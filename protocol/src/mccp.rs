//! Shared bounded MCCP2 zlib step. Hosts own stream activation and pacing.

use std::io;

use flate2::{Decompress, FlushDecompress, Status};

/// Maximum inflated bytes produced by one step, including under hostile ratios.
pub const INFLATE_CHUNK: usize = 64 * 1024;

/// Inflate into a reused buffer, returning (compressed bytes consumed, stream ended).
/// A host must keep stepping until it needs new input or reaches the end marker.
///
/// # Errors
/// Returns a decompression error for corrupt or unsupported zlib input.
pub fn inflate_step(
    z: &mut Decompress,
    input: &[u8],
    out: &mut Vec<u8>,
) -> io::Result<(usize, bool)> {
    out.clear();
    // decompress_vec writes to capacity; exact reservation is the memory bound.
    out.reserve_exact(INFLATE_CHUNK - out.len());
    let before = z.total_in();
    let status = z
        .decompress_vec(input, out, FlushDecompress::None)
        .map_err(io::Error::other)?;
    let consumed = usize::try_from(z.total_in() - before).unwrap_or(usize::MAX);
    Ok((consumed, status == Status::StreamEnd))
}
