//! The procedure bus (`docs/interop.md` §6): directed, fire-and-forget delivery of asks to
//! a package's home instance.
//!
//! A producer declares `export const req = createProcedure(impl)` — the implementation
//! registers at construction, and receipt is an interop *write* (interop.md §3), so the
//! registration is home-gated in the op layer. Callers import from
//! `smudgy:procedures/<owner>/<pkg>` and `post(args)`; the host stamps the poster's origin
//! as the unforgeable `sender`, so implementations apply their own stranger policy.
//! Delivery rides the action queue like events (JSON payloads, async next-pump dispatch,
//! depth-capped). `.call` — the correlated-reply ask — is deferred (interop.md §14) and
//! will layer on this same bus.
//!
//! **Queue-briefly semantics**: a post that finds no registered implementation for an
//! *addressable* procedure is buffered — bounded per name, oldest dropped — and drained
//! FIFO when the implementation registers. This covers the two real races with no
//! author-visible loss: a caller module's top-level `post` evaluating before the producer's
//! isolate, and a post landing while a reload is rebuilding the engine. Implementations are
//! engine-scoped (their `FunctionId`s die with the isolates); the pending buffer is
//! session-scoped and survives [`MessageBus::reset_engine_state`].

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use super::IsolateId;
use super::script_engine::FunctionId;

/// The message bus handle shared (the same `Rc`) into every isolate's ops, like the store.
pub(crate) type SharedMessageBus = Rc<RefCell<MessageBus>>;

/// One registered receiver: the isolate that registered the handler and its `FunctionId`
/// in that isolate's function registry.
#[derive(Clone)]
pub struct MessageReceiver {
    pub isolate: IsolateId,
    pub function_id: FunctionId,
}

/// A post buffered while its message had no registered receiver, awaiting the drain at
/// registration. `payload` is the JSON text as posted; `sender` is the host-stamped origin.
pub struct PendingPost {
    pub payload: String,
    pub sender: String,
}

/// Most pending posts buffered per message name; the oldest is dropped (with a log) on
/// overflow. Generous for the races the buffer exists to cover (module-evaluation order,
/// a reload window), tight enough that a never-registering receiver can't hoard memory.
pub const PENDING_POST_CAP: usize = 64;

/// Session-global message routing: canonical folded message name (`<producer>#<name>`) →
/// receivers, plus the session-scoped pending buffer.
#[derive(Default)]
pub struct MessageBus {
    receivers: HashMap<String, Vec<MessageReceiver>>,
    pending: HashMap<String, VecDeque<PendingPost>>,
}

impl MessageBus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `canonical`'s implementation and drain its pending posts (FIFO) for the
    /// caller to deliver. A procedure has exactly ONE implementer (interop.md §6):
    /// registration REPLACES any prior receiver, so re-creating a procedure (a dynamic
    /// `createProcedure('name', impl)` in a reconnect handler, say) swaps the
    /// implementation instead of accumulating one delivery per re-creation.
    pub fn subscribe(&mut self, canonical: String, receiver: MessageReceiver) -> Vec<PendingPost> {
        let drained = self
            .pending
            .remove(&canonical)
            .map_or_else(Vec::new, Vec::from);
        self.receivers.insert(canonical, vec![receiver]);
        drained
    }

    /// The receivers currently registered for `canonical`, cloned out so the borrow is
    /// released before the caller queues deliveries.
    #[must_use]
    pub fn receivers(&self, canonical: &str) -> Vec<MessageReceiver> {
        self.receivers
            .get(canonical)
            .map_or_else(Vec::new, Clone::clone)
    }

    /// Buffer a post that found no receiver. Returns `true` when the buffer was full and the
    /// oldest pending post was dropped to make room.
    pub fn push_pending(&mut self, canonical: String, post: PendingPost) -> bool {
        let queue = self.pending.entry(canonical).or_default();
        let dropped = queue.len() >= PENDING_POST_CAP;
        if dropped {
            queue.pop_front();
        }
        queue.push_back(post);
        dropped
    }

    /// Engine teardown: receivers die with their isolates' function registries; pending
    /// posts survive — the queue-briefly semantics that carry a post across a reload window.
    pub fn reset_engine_state(&mut self) {
        self.receivers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(id: usize) -> MessageReceiver {
        MessageReceiver {
            isolate: IsolateId::Main,
            function_id: FunctionId(id),
        }
    }

    #[test]
    fn subscribe_drains_pending_posts_fifo() {
        let mut bus = MessageBus::new();
        bus.push_pending(
            "user#req".into(),
            PendingPost { payload: "1".into(), sender: "user".into() },
        );
        bus.push_pending(
            "user#req".into(),
            PendingPost { payload: "2".into(), sender: "user".into() },
        );
        let drained = bus.subscribe("user#req".into(), receiver(0));
        assert_eq!(
            drained.iter().map(|p| p.payload.as_str()).collect::<Vec<_>>(),
            ["1", "2"]
        );
        // Drained means drained: a re-registration gets nothing — and REPLACES the prior
        // implementation (single implementer, interop.md §6), never accumulating receivers.
        assert!(bus.subscribe("user#req".into(), receiver(1)).is_empty());
        assert_eq!(bus.receivers("user#req").len(), 1);
        assert_eq!(bus.receivers("user#req")[0].function_id, FunctionId(1));
    }

    #[test]
    fn pending_is_bounded_dropping_oldest() {
        let mut bus = MessageBus::new();
        for i in 0..=PENDING_POST_CAP {
            let dropped = bus.push_pending(
                "user#req".into(),
                PendingPost { payload: i.to_string(), sender: "user".into() },
            );
            assert_eq!(dropped, i == PENDING_POST_CAP, "only the overflowing push drops");
        }
        let drained = bus.subscribe("user#req".into(), receiver(0));
        assert_eq!(drained.len(), PENDING_POST_CAP);
        assert_eq!(drained[0].payload, "1", "the oldest post was dropped");
    }

    #[test]
    fn reset_keeps_pending_posts() {
        let mut bus = MessageBus::new();
        bus.subscribe("user#req".into(), receiver(0));
        bus.push_pending(
            "user#req".into(),
            PendingPost { payload: "1".into(), sender: "user".into() },
        );
        bus.reset_engine_state();
        let drained = bus.subscribe("user#req".into(), receiver(1));
        assert_eq!(drained.len(), 1, "pending posts survive an engine rebuild");
    }
}
