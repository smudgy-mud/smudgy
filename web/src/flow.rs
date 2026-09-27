//! Bounded presentation flow between one session worker and its window.
//!
//! Network, Telnet, and automation work finishes synchronously in the worker.
//! Only snapshots sent to iced are coalesced. A worker sends at most one frame
//! until the window confirms that it has applied it.

#[derive(Debug, Default)]
pub struct PresentationFlow {
    dirty: bool,
    scheduled: bool,
    in_flight: Option<u64>,
    next_sequence: u64,
}

impl PresentationFlow {
    /// Mark worker state changed. Returns true when the host should arm a timer.
    pub fn changed(&mut self) -> bool {
        self.dirty = true;
        self.schedule_if_ready()
    }

    /// Called when the presentation timer fires. Returns the sequence to send.
    pub fn timer_fired(&mut self) -> Option<u64> {
        self.scheduled = false;
        (self.dirty && self.in_flight.is_none()).then_some(self.next_sequence)
    }

    /// Commit a sequence only after `postMessage` succeeds.
    pub fn sent(&mut self, sequence: u64) {
        debug_assert_eq!(sequence, self.next_sequence);
        debug_assert!(self.in_flight.is_none());
        self.in_flight = Some(sequence);
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.dirty = false;
    }

    /// Accept one matching acknowledgment and schedule any accumulated work.
    pub fn acknowledge(&mut self, sequence: u64) -> bool {
        if self.in_flight != Some(sequence) {
            return false;
        }
        self.in_flight = None;
        self.schedule_if_ready()
    }

    fn schedule_if_ready(&mut self) -> bool {
        if !self.dirty || self.scheduled || self.in_flight.is_some() {
            return false;
        }
        self.scheduled = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::PresentationFlow;

    #[test]
    fn coalesces_changes_until_applied_and_ignores_stale_acks() {
        let mut flow = PresentationFlow::default();
        assert!(flow.changed());
        assert!(!flow.changed());
        assert_eq!(flow.timer_fired(), Some(0));
        flow.sent(0);
        assert!(!flow.changed());
        assert!(!flow.changed());
        assert!(!flow.acknowledge(1));
        assert!(flow.acknowledge(0));
        assert!(!flow.acknowledge(0));
        assert_eq!(flow.timer_fired(), Some(1));
        flow.sent(1);
        assert!(!flow.acknowledge(0));
        assert!(!flow.acknowledge(1));
    }
}
