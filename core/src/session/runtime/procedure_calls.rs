//! Bounded procedure request/reply bookkeeping. Waiting calls are not Deno ops:
//! their promises live in the caller's heap and are settled by ordinary actions.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc::UnboundedSender, watch};

use super::message_bus::MessageReceiver;
use super::trigger::MatchCapture;
use super::{RuntimeAction, SessionId};

pub const MAX_TIMEOUT_MS: u32 = 60_000;
pub const MAX_CALL_HOPS: u32 = 64;
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_PER_ISOLATE: usize = 256;
const MAX_PER_SESSION: usize = 1024;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

/// Native failures stay structured until the op boundary formats a JS exception.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub(crate) struct ProcedureCallFailure {
    pub code: &'static str,
    pub message: &'static str,
}

impl ProcedureCallFailure {
    pub const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

/// Numeric reply protocol, mirrored by the trusted wrapper in `js/smudgy.ts`.
#[repr(u32)]
pub(crate) enum ProcedureReplyStatus {
    Value = 0,
    Void = 1,
    Error = 2,
}

impl ProcedureReplyStatus {
    pub fn decode(status: u32) -> Self {
        match status {
            value if value == Self::Value as u32 => Self::Value,
            value if value == Self::Void as u32 => Self::Void,
            // Preserve the op's existing handling of unknown statuses as errors.
            _ => Self::Error,
        }
    }
}

/// Charges actual queued storage until its last owner drops it, including
/// after timeout/reload. Shared across threads and engine generations.
#[derive(Clone, Debug, Default)]
pub(crate) struct PayloadMemory(Arc<AtomicUsize>);

#[derive(Debug)]
pub(crate) struct PayloadReservation {
    memory: PayloadMemory,
    bytes: usize,
}

impl Drop for PayloadReservation {
    fn drop(&mut self) {
        self.memory.0.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl PayloadMemory {
    pub fn reserve(&self, bytes: usize) -> Result<PayloadReservation, ProcedureCallFailure> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|sum| *sum <= MAX_PENDING_BYTES)
            })
            .map_err(|_| {
                ProcedureCallFailure::new("Capacity", "queued procedure payload budget exceeded")
            })?;
        Ok(PayloadReservation {
            memory: self.clone(),
            bytes,
        })
    }
}

/// An exact caller heap plus a session-local, never-reused request number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcedureCallKey {
    pub(crate) session: SessionId,
    pub(crate) instance: u64,
    pub(crate) id: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct CallAncestry {
    pub(crate) hops: u32,
    pub(crate) deadline: Instant,
}

#[derive(Debug)]
pub struct ProcedureRequest {
    pub(crate) key: ProcedureCallKey,
    pub(crate) target: SessionId,
    pub(crate) canonical: Arc<str>,
    pub(crate) producer: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) payload: Arc<str>,
    pub(crate) origin: Arc<str>,
    pub(crate) caller: crate::session::registry::SessionSnapshot,
    pub(crate) ancestry: CallAncestry,
    pub(crate) reply_tx: UnboundedSender<RuntimeAction>,
    pub(crate) memory: PayloadMemory,
    pub(crate) allocation: PayloadReservation,
}

#[derive(Clone, Debug)]
pub enum ProcedureOutcome {
    Value(Arc<str>),
    Void,
    Error {
        code: &'static str,
        message: String,
    },
    /// An implementation or serialization error encoded by the trusted JS wrapper.
    EncodedError(Arc<str>),
}

impl ProcedureOutcome {
    pub(crate) fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
        }
    }

    fn capture(&self) -> (&'static str, String) {
        match self {
            Self::Value(json) => ("value", json.to_string()),
            Self::Void => ("void", String::new()),
            Self::Error { code, message } => (
                "error",
                serde_json::json!({
                    "code": code, "message": message, "name": "ProcedureCallError",
                })
                .to_string(),
            ),
            Self::EncodedError(json) => ("error", json.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct ProcedureReply {
    pub(crate) key: ProcedureCallKey,
    pub(crate) outcome: ProcedureOutcome,
    pub(crate) tx: UnboundedSender<RuntimeAction>,
    _allocation: Option<PayloadReservation>,
}

impl ProcedureRequest {
    fn retained_bytes(&self) -> usize {
        self.allocation.bytes
    }
    pub(crate) fn reply(&self, outcome: ProcedureOutcome) -> Arc<ProcedureReply> {
        let bytes = match &outcome {
            ProcedureOutcome::Value(json) | ProcedureOutcome::EncodedError(json) => json.len(),
            ProcedureOutcome::Error { message, .. } => message.len() + 128,
            ProcedureOutcome::Void => 0,
        };
        let (outcome, allocation) = match self.memory.reserve(bytes) {
            Ok(allocation) => (outcome, Some(allocation)),
            Err(_) => (
                ProcedureOutcome::error("Capacity", "queued procedure response budget exceeded"),
                None,
            ),
        };
        Arc::new(ProcedureReply {
            key: self.key,
            outcome,
            tx: self.reply_tx.clone(),
            _allocation: allocation,
        })
    }

    pub(crate) fn fail(&self, code: &'static str, message: &'static str) {
        let _ = self.reply_tx.send(RuntimeAction::ProcedureReply(
            self.reply(ProcedureOutcome::error(code, message)),
        ));
    }
}

struct PendingCall {
    request: Arc<ProcedureRequest>,
    receiver: MessageReceiver,
}

struct Invocation {
    request: Arc<ProcedureRequest>,
    instance: u64,
    reply_prepared: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DeadlineKind {
    Call,
    Invocation,
}

#[derive(Default)]
struct Budget {
    counts: HashMap<u64, usize>,
    bytes: usize,
}

impl Budget {
    fn admit(
        &mut self,
        instance: u64,
        bytes: usize,
        count: usize,
    ) -> Result<(), ProcedureCallFailure> {
        if count >= MAX_PER_SESSION
            || self.counts.get(&instance).copied().unwrap_or(0) >= MAX_PER_ISOLATE
            || self.bytes + bytes > MAX_PENDING_BYTES
        {
            return Err(ProcedureCallFailure::new(
                "Capacity",
                "procedure call budget exceeded",
            ));
        }
        *self.counts.entry(instance).or_default() += 1;
        self.bytes += bytes;
        Ok(())
    }

    fn release(&mut self, instance: u64, bytes: usize) {
        let count = self.counts.get_mut(&instance).expect("admitted instance");
        *count -= 1;
        if *count == 0 {
            self.counts.remove(&instance);
        }
        self.bytes -= bytes;
    }
}

/// Session-local tracking; queued request/reply data may outlive these entries.
///
/// - Each pending caller and incoming invocation owns one deadline and one
///   directional budget charge. `incoming_keys` indexes the same invocations by
///   the caller's key so cancellation can find the local ticket.
/// - Preparing a reply invalidates its parent ticket, but retains the invocation.
///   Dispatch validates and removes it before forwarding. Caller settlement is a
///   separate step that removes the pending call and queues its JS callback.
/// - Removal releases logical counts/bytes. Actual queued storage remains charged
///   by `PayloadReservation` until the last request/reply owner drops it.
/// - Reset clears tracking but preserves `next_id` and `memory`: tickets are never
///   reused, and old queued data remains charged across engine generations.
pub(crate) struct ProcedureCalls {
    pub ready: bool,
    next_id: u32,
    settlers: HashMap<u64, MessageReceiver>,
    pending: HashMap<u32, PendingCall>,
    invocations: HashMap<u32, Invocation>,
    incoming_keys: HashMap<ProcedureCallKey, u32>,
    deadlines: BTreeSet<(Instant, DeadlineKind, u32)>,
    deadline_tx: Option<watch::Sender<Option<Instant>>>,
    outgoing_budget: Budget,
    incoming_budget: Budget,
    pub memory: PayloadMemory,
}

impl Default for ProcedureCalls {
    fn default() -> Self {
        Self {
            ready: false,
            next_id: 1,
            settlers: HashMap::new(),
            pending: HashMap::new(),
            invocations: HashMap::new(),
            incoming_keys: HashMap::new(),
            deadlines: BTreeSet::new(),
            deadline_tx: None,
            outgoing_budget: Budget::default(),
            incoming_budget: Budget::default(),
            memory: PayloadMemory::default(),
        }
    }
}

impl ProcedureCalls {
    pub fn enable(&mut self, runtime_tx: UnboundedSender<RuntimeAction>) {
        if self.deadline_tx.is_none() {
            let (tx, rx) = watch::channel(self.next_deadline());
            self.deadline_tx = Some(tx);
            tokio::spawn(deadline_worker(rx, runtime_tx));
        }
        self.ready = true;
    }

    fn publish_deadline(&self) {
        if let Some(tx) = &self.deadline_tx {
            let deadline = self.next_deadline();
            tx.send_if_modified(|current| {
                if *current == deadline {
                    return false;
                }
                *current = deadline;
                true
            });
        }
    }

    pub fn register_settler(&mut self, instance: u64, receiver: MessageReceiver) {
        self.settlers.insert(instance, receiver);
    }

    pub fn allocate_id(&mut self) -> Result<u32, ProcedureCallFailure> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(ProcedureCallFailure::new(
            "Capacity",
            "procedure request IDs exhausted",
        ))?;
        Ok(id)
    }

    pub fn ancestry(
        &self,
        instance: u64,
        parent: u32,
        delivery_depth: u32,
        timeout_ms: u32,
    ) -> Result<CallAncestry, ProcedureCallFailure> {
        if !self.ready {
            return Err(ProcedureCallFailure::new(
                "NotReady",
                "procedures cannot be called before RuntimeReady",
            ));
        }
        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            return Err(ProcedureCallFailure::new(
                "InvalidTimeout",
                "timeoutMs must be between 1 and 60000",
            ));
        }
        let now = Instant::now();
        let deadline = now + Duration::from_millis(u64::from(timeout_ms));
        if parent == 0 {
            if delivery_depth >= MAX_CALL_HOPS {
                return Err(ProcedureCallFailure::new(
                    "HopLimit",
                    "procedure call hop limit reached",
                ));
            }
            return Ok(CallAncestry {
                hops: delivery_depth + 1,
                deadline,
            });
        }
        let invocation = self
            .invocations
            .get(&parent)
            .filter(|v| v.instance == instance && !v.reply_prepared)
            .ok_or(ProcedureCallFailure::new(
                "Expired",
                "the parent procedure invocation has finished",
            ))?;
        let ancestor = invocation.request.ancestry;
        if ancestor.deadline <= now {
            return Err(ProcedureCallFailure::new(
                "Timeout",
                "the parent procedure deadline expired",
            ));
        }
        let hops = ancestor.hops.max(delivery_depth);
        if hops >= MAX_CALL_HOPS {
            return Err(ProcedureCallFailure::new(
                "HopLimit",
                "procedure call hop limit reached",
            ));
        }
        Ok(CallAncestry {
            hops: hops + 1,
            deadline: deadline.min(ancestor.deadline),
        })
    }

    pub fn insert_call(
        &mut self,
        request: Arc<ProcedureRequest>,
    ) -> Result<(), ProcedureCallFailure> {
        let receiver =
            self.settlers
                .get(&request.key.instance)
                .cloned()
                .ok_or(ProcedureCallFailure::new(
                    "Closed",
                    "the calling isolate is no longer live",
                ))?;
        self.outgoing_budget.admit(
            request.key.instance,
            request.retained_bytes(),
            self.pending.len(),
        )?;
        self.deadlines.insert((
            request.ancestry.deadline,
            DeadlineKind::Call,
            request.key.id,
        ));
        self.pending
            .insert(request.key.id, PendingCall { request, receiver });
        self.publish_deadline();
        Ok(())
    }

    pub fn start_invocation(
        &mut self,
        request: Arc<ProcedureRequest>,
        instance: u64,
    ) -> Result<u32, ProcedureCallFailure> {
        if !self.ready {
            return Err(ProcedureCallFailure::new(
                "NotReady",
                "the target runtime is loading",
            ));
        }
        if request.ancestry.deadline <= Instant::now() {
            return Err(ProcedureCallFailure::new(
                "Timeout",
                "procedure deadline expired before delivery",
            ));
        }
        if self.incoming_keys.contains_key(&request.key) {
            return Err(ProcedureCallFailure::new(
                "Duplicate",
                "procedure was already delivered",
            ));
        }
        let id = self.allocate_id()?;
        self.incoming_budget
            .admit(instance, request.retained_bytes(), self.invocations.len())?;
        self.deadlines
            .insert((request.ancestry.deadline, DeadlineKind::Invocation, id));
        self.incoming_keys.insert(request.key, id);
        self.invocations.insert(
            id,
            Invocation {
                request,
                instance,
                reply_prepared: false,
            },
        );
        self.publish_deadline();
        Ok(id)
    }

    fn remove_invocation(&mut self, id: u32) -> Option<Invocation> {
        let v = self.invocations.remove(&id)?;
        self.deadlines
            .remove(&(v.request.ancestry.deadline, DeadlineKind::Invocation, id));
        self.incoming_keys.remove(&v.request.key);
        self.incoming_budget
            .release(v.instance, v.request.retained_bytes());
        self.publish_deadline();
        Some(v)
    }

    fn remove_call(&mut self, id: u32) -> Option<PendingCall> {
        let call = self.pending.remove(&id)?;
        self.deadlines
            .remove(&(call.request.ancestry.deadline, DeadlineKind::Call, id));
        self.outgoing_budget
            .release(call.request.key.instance, call.request.retained_bytes());
        self.publish_deadline();
        Some(call)
    }

    pub fn prepare_reply(
        &mut self,
        id: u32,
        instance: u64,
        outcome: ProcedureOutcome,
    ) -> Option<Arc<ProcedureReply>> {
        let v = self.invocations.get_mut(&id)?;
        if v.instance != instance || v.reply_prepared {
            return None;
        }
        // A reload can discard the queued forwarding action. Keep the request
        // tracked until dispatch so reset/timeout can still reject the caller.
        v.reply_prepared = true;
        let outcome = if v.request.ancestry.deadline <= Instant::now() {
            ProcedureOutcome::error("Timeout", "procedure deadline expired")
        } else {
            outcome
        };
        Some(v.request.reply(outcome))
    }

    /// Consume a prepared invocation once; dispatch sends the associated reply.
    pub fn accept_reply_for_forwarding(&mut self, id: u32, instance: u64) -> bool {
        if !self
            .invocations
            .get(&id)
            .is_some_and(|v| v.instance == instance && v.reply_prepared)
        {
            return false;
        }
        self.remove_invocation(id);
        true
    }

    pub fn cancel(&mut self, key: ProcedureCallKey) {
        if let Some(id) = self.incoming_keys.get(&key).copied() {
            self.remove_invocation(id);
        }
    }

    pub fn complete(&mut self, reply: &ProcedureReply) -> Option<RuntimeAction> {
        if self.pending.get(&reply.key.id)?.request.key != reply.key {
            return None;
        }
        let call = self.remove_call(reply.key.id)?;
        let outcome = if call.request.ancestry.deadline <= Instant::now() {
            ProcedureOutcome::error("Timeout", "procedure deadline expired")
        } else {
            reply.outcome.clone()
        };
        Some(settlement(call, &outcome))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.first().map(|v| v.0)
    }

    /// Called only when the host timer queues an expiration action. Ordinary
    /// trigger dispatch does not read the clock or poll the procedure broker.
    pub fn expire(&mut self) -> Vec<RuntimeAction> {
        let Some(deadline) = self.next_deadline() else {
            return Vec::new();
        };
        let now = Instant::now();
        if deadline > now {
            return Vec::new();
        }
        let mut actions = Vec::new();
        while let Some(&(deadline, kind, id)) = self.deadlines.first() {
            if deadline > now {
                break;
            }
            self.deadlines.pop_first();
            match kind {
                DeadlineKind::Call => {
                    if let Some(call) = self.remove_call(id) {
                        cancel_target(&call.request);
                        actions.push(settlement(
                            call,
                            &ProcedureOutcome::error("Timeout", "procedure deadline expired"),
                        ));
                    }
                }
                DeadlineKind::Invocation => {
                    if let Some(v) = self.remove_invocation(id) {
                        v.request.fail("Timeout", "procedure deadline expired");
                    }
                }
            }
        }
        actions
    }

    pub fn reset(&mut self) {
        self.ready = false;
        // Closing the publisher stops this engine generation's host timer task.
        self.deadline_tx = None;
        for (_, call) in self.pending.drain() {
            cancel_target(&call.request);
        }
        for (_, v) in self.invocations.drain() {
            v.request.fail(
                "Reloaded",
                "the target procedure engine stopped or reloaded",
            );
        }
        self.settlers.clear();
        self.incoming_keys.clear();
        self.deadlines.clear();
        self.outgoing_budget = Budget::default();
        self.incoming_budget = Budget::default();
    }
}

/// One host timer per live engine, parked without a timer when the tree is empty.
/// This is independent of Deno's pending-op accounting and carries no V8 state.
async fn deadline_worker(
    mut deadlines: watch::Receiver<Option<Instant>>,
    runtime_tx: UnboundedSender<RuntimeAction>,
) {
    loop {
        let deadline = *deadlines.borrow_and_update();
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                changed = deadlines.changed() => {
                    if changed.is_err() { break; }
                    continue;
                }
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    if runtime_tx.send(RuntimeAction::ExpireProcedureCalls).is_err() { break; }
                }
            }
        }
        // After firing, wait for dispatch to update the tree. Never queue the
        // same overdue deadline repeatedly while earlier external input drains.
        if deadlines.changed().await.is_err() {
            break;
        }
    }
}

fn cancel_target(request: &ProcedureRequest) {
    if let Some(runtime) = crate::session::registry::get_runtime(request.target) {
        let _ = runtime
            .tx
            .send(RuntimeAction::CancelProcedureCall(request.key));
    }
}

fn settlement(call: PendingCall, outcome: &ProcedureOutcome) -> RuntimeAction {
    let (kind, payload) = outcome.capture();
    RuntimeAction::CallJavascriptFunction {
        isolate: call.receiver.isolate,
        id: call.receiver.function_id,
        matches: Arc::new(vec![
            MatchCapture {
                name: Some("id".into()),
                value: call.request.key.id.to_string(),
            },
            MatchCapture {
                name: Some("kind".into()),
                value: kind.to_string(),
            },
            MatchCapture {
                name: Some("payload".into()),
                value: payload,
            },
        ]),
        depth: 0,
        is_captured: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::runtime::{IsolateId, script_engine::FunctionId};

    fn broker() -> ProcedureCalls {
        let mut calls = ProcedureCalls {
            ready: true,
            ..ProcedureCalls::default()
        };
        calls.register_settler(
            1,
            MessageReceiver {
                isolate: IsolateId::Main,
                function_id: FunctionId(0),
            },
        );
        calls
    }

    fn request(calls: &mut ProcedureCalls, payload: &str) -> Arc<ProcedureRequest> {
        let id = calls.allocate_id().unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(ProcedureRequest {
            key: ProcedureCallKey {
                session: SessionId::from(10100),
                instance: 1,
                id,
            },
            target: SessionId::from(10101),
            canonical: Arc::from("user#test"),
            producer: Arc::from("user"),
            name: Arc::from("test"),
            payload: Arc::from(payload),
            origin: Arc::from("user"),
            caller: crate::session::registry::SessionSnapshot {
                id: SessionId::from(10100),
                profile_name: Arc::new("Test".into()),
                profile_subtext: Arc::new(String::new()),
                connected: false,
            },
            ancestry: CallAncestry {
                hops: 1,
                deadline: Instant::now() + Duration::from_secs(10),
            },
            reply_tx: tx,
            allocation: calls.memory.reserve(payload.len()).unwrap(),
            memory: calls.memory.clone(),
        })
    }

    #[test]
    fn stale_and_duplicate_replies_cannot_settle_a_live_call() {
        let mut calls = broker();
        let request = request(&mut calls, "{}");
        calls.insert_call(Arc::clone(&request)).unwrap();
        let mut stale = request.reply(ProcedureOutcome::Void);
        Arc::get_mut(&mut stale).unwrap().key.instance = 2;
        assert!(calls.complete(&stale).is_none());
        assert_eq!(calls.pending.len(), 1);
        let reply = request.reply(ProcedureOutcome::Value(Arc::from("42")));
        assert!(calls.complete(&reply).is_some());
        assert!(calls.complete(&reply).is_none());
        assert!(calls.pending.is_empty() && calls.deadlines.is_empty());
        assert_eq!(calls.outgoing_budget.bytes, 0);
    }

    #[test]
    fn only_the_invoked_heap_can_reply_and_parent_tickets_expire() {
        let mut calls = broker();
        let request = request(&mut calls, "{}");
        let ticket = calls.start_invocation(request, 2).unwrap();
        assert!(
            calls
                .prepare_reply(ticket, 3, ProcedureOutcome::Void)
                .is_none()
        );
        assert_eq!(calls.ancestry(2, ticket, 0, 10000).unwrap().hops, 2);
        assert_eq!(calls.ancestry(2, ticket, 10, 10000).unwrap().hops, 11);
        assert!(calls.ancestry(3, ticket, 0, 10000).is_err());
        assert!(!calls.accept_reply_for_forwarding(ticket, 2));
        assert!(
            calls
                .prepare_reply(ticket, 2, ProcedureOutcome::Void)
                .is_some()
        );
        assert!(calls.ancestry(2, ticket, 0, 10000).is_err());
        assert_eq!(calls.invocations.len(), 1);
        assert!(
            calls
                .prepare_reply(ticket, 2, ProcedureOutcome::Void)
                .is_none()
        );
        assert!(!calls.accept_reply_for_forwarding(ticket, 3));
        assert!(calls.accept_reply_for_forwarding(ticket, 2));
        assert!(!calls.accept_reply_for_forwarding(ticket, 2));
        assert!(calls.invocations.is_empty() && calls.deadlines.is_empty());
        assert_eq!(calls.incoming_budget.bytes, 0);
    }

    #[test]
    fn root_calls_preserve_delivery_depth_and_enforce_the_hop_limit() {
        let calls = broker();
        assert_eq!(calls.ancestry(1, 0, 0, 10000).unwrap().hops, 1);
        assert_eq!(calls.ancestry(1, 0, 63, 10000).unwrap().hops, 64);
        for depth in [64, u32::MAX] {
            assert_eq!(
                calls.ancestry(1, 0, depth, 10000).unwrap_err(),
                ProcedureCallFailure::new("HopLimit", "procedure call hop limit reached")
            );
        }
    }

    #[test]
    fn reset_rejects_prepared_replies_and_discards_stale_forwarding() {
        let mut calls = broker();
        let mut request = request(&mut calls, "{}");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::get_mut(&mut request).unwrap().reply_tx = tx;
        let ticket = calls.start_invocation(request, 2).unwrap();
        let prepared = calls
            .prepare_reply(ticket, 2, ProcedureOutcome::Void)
            .unwrap();
        calls.reset();
        let RuntimeAction::ProcedureReply(reply) = rx.try_recv().unwrap() else {
            panic!("expected a reload rejection");
        };
        assert!(matches!(
            reply.outcome,
            ProcedureOutcome::Error {
                code: "Reloaded",
                ..
            }
        ));
        assert!(!calls.accept_reply_for_forwarding(ticket, 2));
        assert_eq!(prepared.key, reply.key);
        assert!(rx.try_recv().is_err());
        assert!(calls.invocations.is_empty() && calls.deadlines.is_empty());
        assert_eq!(calls.incoming_budget.bytes, 0);
    }

    #[test]
    fn queued_memory_stays_charged_across_timeout_and_reload() {
        let mut calls = broker();
        let request = request(&mut calls, "queued payload");
        calls.insert_call(Arc::clone(&request)).unwrap();
        let reply = request.reply(ProcedureOutcome::Value(Arc::from("result")));
        calls.reset();
        assert_eq!(
            calls.memory.0.load(Ordering::Relaxed),
            "queued payload".len() + "result".len()
        );
        assert!(calls.memory.reserve(MAX_PENDING_BYTES).is_err());
        drop(request);
        assert_eq!(calls.memory.0.load(Ordering::Relaxed), "result".len());
        drop(reply);
        assert_eq!(calls.memory.0.load(Ordering::Relaxed), 0);
        assert!(calls.memory.reserve(MAX_PENDING_BYTES).is_ok());
        assert_eq!(calls.memory.0.load(Ordering::Relaxed), 0);
        assert!(calls.allocate_id().unwrap() > 1);
    }

    #[test]
    fn expired_calls_release_deadlines_and_slots() {
        let mut calls = broker();
        let mut request = request(&mut calls, "{}");
        Arc::get_mut(&mut request).unwrap().ancestry.deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        calls.insert_call(request).unwrap();
        assert_eq!(calls.expire().len(), 1);
        assert!(calls.pending.is_empty() && calls.deadlines.is_empty());
        assert_eq!(calls.outgoing_budget.bytes, 0);
        assert_eq!(calls.memory.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn host_timer_reschedules_disarms_and_stops_on_reset() {
        let mut calls = broker();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        calls.enable(tx);
        let later = request(&mut calls, "{}");
        calls.insert_call(Arc::clone(&later)).unwrap();
        tokio::task::yield_now().await;
        let mut earlier = request(&mut calls, "{}");
        Arc::get_mut(&mut earlier).unwrap().ancestry.deadline =
            Instant::now() + Duration::from_millis(5);
        calls.insert_call(earlier).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap(),
            Some(RuntimeAction::ExpireProcedureCalls)
        ));
        assert_eq!(calls.expire().len(), 1);
        assert!(
            calls
                .complete(&later.reply(ProcedureOutcome::Void))
                .is_some()
        );
        assert!(calls.next_deadline().is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err()
        );
        calls.reset();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .is_none()
        );
    }
}
