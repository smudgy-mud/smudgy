//! Ordered, low-volume commands from session runtimes to the UI daemon.
//!
//! Session event streams remain the transport for observations and buffered
//! terminal output. This bus is the single ordering boundary for imperative UI
//! mutations issued by scripts, including mutations that target another
//! session. Every producer sends directly from the runtime where the script op
//! ran; a foreign command must never hop through the target runtime first.

use std::{
    hash::{Hash, Hasher},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use futures::{Stream, channel::mpsc};

use super::{
    SessionId,
    runtime::pane::{PaneDef, PaneKey, PanePlacement, SplitDirection, TabPosition},
};

/// One command in the UI daemon's canonical receive order.
#[derive(Clone, Debug)]
pub struct UiCommandEnvelope {
    /// Runtime whose script issued the command, even when the mutation targets
    /// another session.
    pub origin: SessionId,
    /// Monotonic within `origin` and preserved across script-engine reloads.
    /// The receiver's channel order is authoritative; this stamp makes any
    /// producer-side overtaking visible in tests and diagnostics.
    pub origin_seq: u64,
    pub command: UiCommand,
}

#[derive(Clone, Debug)]
pub enum UiCommand {
    Pane(PaneCommand),
}

/// Pane-layout mutations applied by the UI daemon.
///
/// Pane output deliberately does not travel here. It stays on the owning
/// session's backpressured event stream, where `PaneOpened` still orders pane
/// display-state materialization before the first `AppendTo`.
#[derive(Clone, Debug)]
pub enum PaneCommand {
    Open {
        session_id: SessionId,
        def: PaneDef,
        placement: PanePlacement,
    },
    /// Remove the pane from the layout in bus order. The owning session event
    /// retires its display state only after prior buffered output is flushed.
    Close {
        session_id: SessionId,
        key: PaneKey,
    },
    Resize {
        session_id: SessionId,
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    Relocate {
        session_id: SessionId,
        key: PaneKey,
        reference: PaneKey,
        direction: SplitDirection,
        size_px: Option<f32>,
    },
    GroupWith {
        session_id: SessionId,
        key: PaneKey,
        reference_session: SessionId,
        reference: PaneKey,
        position: TabPosition,
        selected: bool,
    },
    Select {
        session_id: SessionId,
        key: PaneKey,
    },
    TearOut {
        session_id: SessionId,
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    Swap {
        session_id: SessionId,
        key: PaneKey,
        other_session: SessionId,
        other_key: PaneKey,
    },
}

/// Cloneable producer half shared by every session runtime in one UI process.
#[derive(Clone, Debug)]
pub struct UiCommandBus {
    id: Arc<()>,
    tx: mpsc::UnboundedSender<UiCommandEnvelope>,
}

impl Hash for UiCommandBus {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.id).hash(state);
    }
}

impl UiCommandBus {
    /// Enqueue without blocking a script runtime on the UI thread.
    fn send(&self, envelope: UiCommandEnvelope) -> bool {
        self.tx.unbounded_send(envelope).is_ok()
    }
}

/// Runtime-local producer identity. All isolates in a session share this
/// value, and the runtime preserves it across engine reloads.
#[derive(Clone, Debug)]
pub(crate) struct UiCommandProducer {
    origin: SessionId,
    next_seq: Arc<AtomicU64>,
    bus: UiCommandBus,
}

impl UiCommandProducer {
    pub(crate) fn new(origin: SessionId, bus: UiCommandBus) -> Self {
        Self {
            origin,
            next_seq: Arc::new(AtomicU64::new(0)),
            bus,
        }
    }

    pub(crate) fn send(&self, command: UiCommand) -> bool {
        let origin_seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let sent = self.bus.send(UiCommandEnvelope {
            origin: self.origin,
            origin_seq,
            command,
        });
        if !sent {
            log::warn!(
                "Dropping UI command from {}: UI bus has shut down",
                self.origin
            );
        }
        sent
    }
}

/// The daemon's single consumer. Clones share one underlying receiver; the
/// stable id keeps iced's subscription recipe alive across application updates.
#[derive(Clone)]
pub struct UiCommandReceiver {
    id: Arc<()>,
    rx: Arc<Mutex<mpsc::UnboundedReceiver<UiCommandEnvelope>>>,
}

impl std::fmt::Debug for UiCommandReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiCommandReceiver").finish_non_exhaustive()
    }
}

impl Hash for UiCommandReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.id).hash(state);
    }
}

impl Stream for UiCommandReceiver {
    type Item = UiCommandEnvelope;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut rx = self.rx.lock().unwrap();
        Pin::new(&mut *rx).poll_next(cx)
    }
}

/// Build one process-wide command path. The UI owns the receiver and gives a
/// clone of the producer to each session it spawns.
#[must_use]
pub fn channel() -> (UiCommandBus, UiCommandReceiver) {
    let (tx, rx) = mpsc::unbounded();
    let id = Arc::new(());
    (
        UiCommandBus {
            id: Arc::clone(&id),
            tx,
        },
        UiCommandReceiver {
            id,
            rx: Arc::new(Mutex::new(rx)),
        },
    )
}
