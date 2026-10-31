//! Native dispatch adapter for the portable VT processor.
//!
//! The sink is a concrete type, so no trait object or virtual call is added
//! to the native per-byte ingest path.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::session::runtime::RuntimeAction;
use crate::session::styled_line::StyledLine;
use smudgy_protocol::vt::{VtEvent, VtSink};

#[cfg(feature = "bench-api")]
pub use smudgy_protocol::sgr::process as sgr_process;
pub use smudgy_protocol::vt::{AnsiColor, Color, parse_ansi_fragment, parse_link_tooltip_text};

pub type VtProcessor = smudgy_protocol::vt::VtProcessor<NativeVtSink>;

#[derive(Debug)]
pub struct NativeVtSink {
    sender: UnboundedSender<RuntimeAction>,
    lines: Vec<Arc<StyledLine>>,
    bytes: usize,
    batched: bool,
}

const MAX_BATCH_LINES: usize = 64;
const MAX_BATCH_BYTES: usize = 64 * 1024;

impl From<UnboundedSender<RuntimeAction>> for NativeVtSink {
    fn from(sender: UnboundedSender<RuntimeAction>) -> Self {
        Self {
            sender,
            lines: Vec::new(),
            bytes: 0,
            batched: false,
        }
    }
}

impl NativeVtSink {
    /// The connection flushes after each available parser feed, before waiting
    /// for more bytes or writing replies. Other VT consumers retain immediate
    /// delivery unless they explicitly opt into that flushing contract.
    pub(crate) fn batched(sender: UnboundedSender<RuntimeAction>) -> Self {
        Self {
            lines: Vec::with_capacity(MAX_BATCH_LINES),
            batched: true,
            ..Self::from(sender)
        }
    }

    pub(crate) fn flush_lines(&mut self) {
        self.bytes = 0;
        match self.lines.len() {
            0 => {}
            1 => {
                let line = self.lines.pop().expect("one pending line");
                let _ = self.sender.send(RuntimeAction::HandleIncomingLine(line));
            }
            _ => {
                let _ = self
                    .sender
                    .send(RuntimeAction::HandleIncomingLines(std::mem::replace(
                        &mut self.lines,
                        Vec::with_capacity(MAX_BATCH_LINES),
                    )));
            }
        }
    }
}

impl VtSink for NativeVtSink {
    #[inline]
    fn emit(&mut self, event: VtEvent) {
        if self.batched {
            if let VtEvent::HandleIncomingLine(line) = event {
                self.bytes = self.bytes.saturating_add(line.text.len());
                self.lines.push(line);
                if self.lines.len() >= MAX_BATCH_LINES || self.bytes >= MAX_BATCH_BYTES {
                    self.flush_lines();
                }
                return;
            }
            // Prompts, fragments, retractions and repaint markers retain their
            // exact position relative to complete lines.
            self.flush_lines();
        }
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
        let _ = self.sender.send(action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::from_output_str(text))
    }

    #[test]
    fn prompt_and_partial_events_flush_complete_lines_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = NativeVtSink::batched(tx);
        sink.emit(VtEvent::HandleIncomingLine(line("one")));
        sink.emit(VtEvent::HandleIncomingLine(line("two")));
        sink.emit(VtEvent::HandleIncomingPartialLine(line("HP> ")));
        sink.emit(VtEvent::PromptBoundary);
        let RuntimeAction::HandleIncomingLines(lines) = rx.try_recv().unwrap() else {
            panic!("expected coalesced complete lines");
        };
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(RuntimeAction::HandleIncomingPartialLine(_))
        ));
        assert!(matches!(rx.try_recv(), Ok(RuntimeAction::PromptBoundary)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn batches_are_bounded_and_a_short_tail_flushes_without_more_input() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = NativeVtSink::batched(tx);
        for _ in 0..MAX_BATCH_LINES {
            sink.emit(VtEvent::HandleIncomingLine(line("x")));
        }
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::HandleIncomingLines(lines)) if lines.len() == MAX_BATCH_LINES)
        );
        sink.emit(VtEvent::HandleIncomingLine(line("tail")));
        sink.flush_lines();
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::HandleIncomingLine(line)) if line.text == "tail")
        );
        sink.emit(VtEvent::HandleIncomingLine(line(
            &"x".repeat(MAX_BATCH_BYTES),
        )));
        assert!(matches!(
            rx.try_recv(),
            Ok(RuntimeAction::HandleIncomingLine(_))
        ));
        assert!(rx.try_recv().is_err());
    }
}
