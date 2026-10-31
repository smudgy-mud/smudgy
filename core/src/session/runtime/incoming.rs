//! Expand transport batches at the external-input boundary, never inside a line's
//! action expansion. The receiver owns the unread tail so reload carries it too.

use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError};

use super::RuntimeAction;
use crate::session::styled_line::StyledLine;

pub(super) struct IncomingActions {
    receiver: UnboundedReceiver<RuntimeAction>,
    lines: std::vec::IntoIter<Arc<StyledLine>>,
}

impl From<UnboundedReceiver<RuntimeAction>> for IncomingActions {
    fn from(receiver: UnboundedReceiver<RuntimeAction>) -> Self {
        Self {
            receiver,
            lines: Vec::new().into_iter(),
        }
    }
}

impl IncomingActions {
    pub(super) fn close(&mut self) {
        self.receiver.close();
    }

    #[inline]
    pub(super) fn try_recv(&mut self) -> Result<RuntimeAction, TryRecvError> {
        loop {
            if let Some(line) = self.lines.next() {
                return Ok(RuntimeAction::HandleIncomingLine(line));
            }
            match self.receiver.try_recv()? {
                RuntimeAction::HandleIncomingLines(lines) => self.lines = lines.into_iter(),
                action => return Ok(action),
            }
        }
    }

    pub(super) async fn recv(&mut self) -> Option<RuntimeAction> {
        loop {
            if let Some(line) = self.lines.next() {
                return Some(RuntimeAction::HandleIncomingLine(line));
            }
            match self.receiver.recv().await? {
                RuntimeAction::HandleIncomingLines(lines) => self.lines = lines.into_iter(),
                action => return Some(action),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::from_output_str(text))
    }

    fn text(action: RuntimeAction) -> String {
        let RuntimeAction::HandleIncomingLine(line) = action else {
            panic!("expected an individual line");
        };
        line.text.clone()
    }

    #[tokio::test]
    async fn batch_tail_precedes_later_input_across_receiver_move_and_close() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut receiver = IncomingActions::from(rx);
        tx.send(RuntimeAction::HandleIncomingLines(vec![
            line("one"),
            line("two"),
        ]))
        .unwrap();
        tx.send(RuntimeAction::PromptBoundary).unwrap();
        assert_eq!(text(receiver.recv().await.unwrap()), "one");

        // Reload moves the receiver; shutdown closes it before draining queued work.
        let mut replacement = receiver;
        replacement.close();
        assert_eq!(text(replacement.try_recv().unwrap()), "two");
        assert!(matches!(
            replacement.recv().await,
            Some(RuntimeAction::PromptBoundary)
        ));
        assert!(replacement.recv().await.is_none());
    }

    #[tokio::test]
    async fn empty_batches_do_not_hide_following_input_or_disconnect() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut receiver = IncomingActions::from(rx);
        tx.send(RuntimeAction::HandleIncomingLines(Vec::new()))
            .unwrap();
        tx.send(RuntimeAction::HandleIncomingLine(line("last")))
            .unwrap();
        drop(tx);
        assert_eq!(text(receiver.try_recv().unwrap()), "last");
        assert!(receiver.recv().await.is_none());
    }
}
