//! Portable policy for a command that could not reach the transport.
//!
//! Hosts still own their connection machinery and presentation. This module
//! keeps the decision itself identical: user intent wins, an in-flight dial is
//! never duplicated, and automatic recovery never re-sends the lost command.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionIntent {
    Online,
    Offline,
}

impl ConnectionIntent {
    #[must_use]
    pub const fn from_bool(online: bool) -> Self {
        if online { Self::Online } else { Self::Offline }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectState {
    /// A reconnect request already exists; this failure joins its outcome.
    Pending,
    /// This submission has already spent its one automatic reconnect.
    Spent,
    /// A connection attempt exists independently of this send failure.
    Connecting,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailureAction {
    JoinReconnect,
    ReportNotConnected,
    ReportConnecting,
    Reconnect,
}

/// Decide what a failed send should do. The line itself is always restored by
/// the host and is never sent again automatically.
#[must_use]
pub const fn decide(
    intent: ConnectionIntent,
    reconnect_enabled: bool,
    state: ReconnectState,
) -> SendFailureAction {
    if matches!(state, ReconnectState::Pending) {
        SendFailureAction::JoinReconnect
    } else if matches!(intent, ConnectionIntent::Offline)
        || !reconnect_enabled
        || matches!(state, ReconnectState::Spent)
    {
        SendFailureAction::ReportNotConnected
    } else if matches!(state, ReconnectState::Connecting) {
        SendFailureAction::ReportConnecting
    } else {
        SendFailureAction::Reconnect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_offline_intent_always_wins() {
        assert_eq!(
            decide(ConnectionIntent::Offline, true, ReconnectState::Idle),
            SendFailureAction::ReportNotConnected
        );
    }

    #[test]
    fn an_existing_attempt_is_never_duplicated() {
        assert_eq!(
            decide(ConnectionIntent::Online, true, ReconnectState::Pending),
            SendFailureAction::JoinReconnect
        );
        assert_eq!(
            decide(ConnectionIntent::Online, true, ReconnectState::Connecting),
            SendFailureAction::ReportConnecting
        );
    }

    #[test]
    fn only_an_idle_intended_session_auto_reconnects() {
        assert_eq!(
            decide(ConnectionIntent::Online, true, ReconnectState::Idle),
            SendFailureAction::Reconnect
        );
        assert_eq!(
            decide(ConnectionIntent::Online, false, ReconnectState::Idle),
            SendFailureAction::ReportNotConnected
        );
    }
}
