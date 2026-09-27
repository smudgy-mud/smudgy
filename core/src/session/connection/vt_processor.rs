//! Native dispatch adapter for the portable VT processor.
//!
//! The sink is a concrete type, so no trait object or virtual call is added
//! to the native per-byte ingest path.

use tokio::sync::mpsc::UnboundedSender;

use crate::session::runtime::RuntimeAction;
use smudgy_protocol::vt::{VtEvent, VtSink};

#[cfg(feature = "bench-api")]
pub use smudgy_protocol::sgr::process as sgr_process;
pub use smudgy_protocol::vt::{AnsiColor, Color, parse_ansi_fragment, parse_link_tooltip_text};

pub type VtProcessor = smudgy_protocol::vt::VtProcessor<NativeVtSink>;

#[derive(Debug)]
pub struct NativeVtSink(UnboundedSender<RuntimeAction>);

impl From<UnboundedSender<RuntimeAction>> for NativeVtSink {
    fn from(sender: UnboundedSender<RuntimeAction>) -> Self {
        Self(sender)
    }
}

impl VtSink for NativeVtSink {
    #[inline]
    fn emit(&mut self, event: VtEvent) {
        let action = match event {
            VtEvent::HandleIncomingLine(line) => RuntimeAction::HandleIncomingLine(line),
            VtEvent::HandleIncomingFragmentedLine {
                line,
                completion_fragment,
            } => RuntimeAction::HandleIncomingFragmentedLine {
                line,
                completion_fragment,
            },
            VtEvent::HandleIncomingPartialLine(line) => {
                RuntimeAction::HandleIncomingPartialLine(line)
            }
            VtEvent::PromptBoundary => RuntimeAction::PromptBoundary,
            VtEvent::RetractIncomingPartialLine => RuntimeAction::RetractIncomingPartialLine,
            VtEvent::RequestRepaint => RuntimeAction::RequestRepaint,
            VtEvent::IncomingPacketProcessed {
                connection_generation,
                has_displayable_text,
            } => RuntimeAction::IncomingPacketProcessed {
                connection_generation,
                has_displayable_text,
            },
        };
        let _ = self.0.send(action);
    }
}
