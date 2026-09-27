//! Invalidates asynchronous password reads when a pane's connection intent changes.
//!
//! The worker remains alive while disconnected, so an `IndexedDB` read for a
//! previous reconnect must not reopen its socket after Disconnect or after a
//! newer reconnect request. This is window-side control flow, not session work.

#[derive(Debug, Default, Clone, Copy)]
pub struct CredentialGate {
    revision: u64,
}

impl CredentialGate {
    pub fn advance(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.revision
    }

    #[must_use]
    pub fn accepts(&self, revision: u64) -> bool {
        self.revision == revision
    }
}

#[cfg(test)]
mod tests {
    use super::CredentialGate;

    #[test]
    fn disconnect_invalidates_a_pending_password_read() {
        let mut gate = CredentialGate::default();
        let reconnect = gate.advance();
        gate.advance(); // Disconnect.
        assert!(!gate.accepts(reconnect));
    }

    #[test]
    fn only_the_newest_reconnect_can_consume_its_password() {
        let mut gate = CredentialGate::default();
        let first = gate.advance();
        let second = gate.advance();
        assert!(!gate.accepts(first));
        assert!(gate.accepts(second));
    }
}
