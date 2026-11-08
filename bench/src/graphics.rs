//! A stub kitty graphics host for the ingest benches' `ansi_graphics_full`
//! profile: the whole graphics state machine on the socket thread — chunk
//! reassembly, the PNG probe, quota admission and eviction, records,
//! placements attached to lines, parked and settled replies — with decoding
//! reduced to handing back zeroed RGBA of the requested size.
//!
//! The connection decodes off-thread and feeds each completion back between
//! read batches; [`DecodeQueue::complete_all`] does the same after each
//! bench batch, in request order. The pixels are allocated as a real decode
//! would allocate them (one `width * height * 4` buffer per image, moved
//! into the record), but never written.

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, PoisonError};

use smudgy_protocol::graphics::{
    CELL_FIXED_SCALE, DecodeRequest, DecodeTicket, DecodedPixels, GraphicsError, GraphicsHost,
    GraphicsMode, Medium,
};
use smudgy_protocol::responders::pack_dims;
use smudgy_protocol::vt::{VtProcessor, VtSink};

/// A requested decode: its ticket and the pixel size it must produce.
type Requested = (DecodeTicket, u32, u32);

/// The host half: records each decode request and drops its data.
#[derive(Debug)]
struct StubGraphicsHost {
    requested: Arc<Mutex<Vec<Requested>>>,
}

impl GraphicsHost for StubGraphicsHost {
    fn decode(&mut self, ticket: DecodeTicket, request: DecodeRequest) {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ticket, request.width, request.height));
    }

    fn read_media(
        &mut self,
        _medium: Medium,
        _name: &[u8],
        _offset: u32,
        _size: u32,
    ) -> Result<Vec<u8>, GraphicsError> {
        Err(GraphicsError::bad_file())
    }
}

/// The bench's half: the decodes requested since the last completion pass.
#[derive(Debug)]
pub struct DecodeQueue {
    requested: Arc<Mutex<Vec<Requested>>>,
    /// Swapped with the shared list each pass, so neither reallocates.
    taken: Vec<Requested>,
}

impl DecodeQueue {
    /// Finish every decode requested since the last call, in request order,
    /// each with zeroed RGBA of its requested size. Returns how many.
    pub fn complete_all<S: VtSink>(&mut self, processor: &mut VtProcessor<S>) -> usize {
        std::mem::swap(
            &mut self.taken,
            &mut self
                .requested
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let completed = self.taken.len();
        for (ticket, width, height) in self.taken.drain(..) {
            let len = usize::try_from(u64::from(width) * u64::from(height) * 4)
                .expect("admitted images fit in memory");
            processor.graphics_decode_complete(
                ticket,
                Ok(DecodedPixels {
                    width,
                    height,
                    rgba: vec![0_u8; len].into(),
                }),
            );
        }
        completed
    }
}

/// The grid the stub geometry reports, in cells.
pub const STUB_GRID: (u16, u16) = (120, 40);
/// The cell the stub geometry reports, in logical pixels.
pub const STUB_CELL: (u16, u16) = (9, 18);

/// Put `processor` in [`GraphicsMode::Full`] with a fixed grid and cell
/// size ([`STUB_GRID`], [`STUB_CELL`]) and the stub host; the returned queue
/// delivers its decodes.
pub fn enable_full_graphics<S: VtSink>(processor: &mut VtProcessor<S>) -> DecodeQueue {
    let requested = Arc::new(Mutex::new(Vec::new()));
    processor.set_graphics_mode(GraphicsMode::Full);
    processor.set_terminal_geometry(
        Arc::new(AtomicU32::new(pack_dims(STUB_GRID.0, STUB_GRID.1))),
        Arc::new(AtomicU32::new(pack_dims(
            STUB_CELL.0 * CELL_FIXED_SCALE,
            STUB_CELL.1 * CELL_FIXED_SCALE,
        ))),
    );
    processor.set_graphics_host(Box::new(StubGraphicsHost {
        requested: requested.clone(),
    }));
    DecodeQueue {
        requested,
        taken: Vec::new(),
    }
}
