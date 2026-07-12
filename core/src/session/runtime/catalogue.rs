//! The host-side **runtime catalogue** (`docs/interop.md` §10): the queryable
//! registry of everything interop-shaped in the session, and the data layer the automations
//! window's store tab renders. Three tiers, cheapest first:
//!
//! 1. **Index (declared presence).** Handle declarations per package, registered from the
//!    same static extraction that powers stub synthesis when the engine builds, and
//!    confirmed at runtime when the producer's constructor actually runs (which also covers
//!    dynamically-created handles); plus *observed-but-undeclared* entries — ad-hoc store
//!    keys catalogued at first write and event/message names catalogued at first
//!    emission/post, provenance marked "undeclared". Presence never depends on emission.
//! 2. **Samples (observed values).** A bounded ring of recent payloads per event/message
//!    (timestamps, host-stamped sender origins, oversized payloads truncated); the live
//!    store tree *is* state's sample, shared into the snapshot as its `Node` root. Shapes
//!    are inferred by an all-history accumulator per entry ([`ShapeAcc`] — a recursive JSON
//!    type walk, fields marked optional when they vary) — ground truth that works
//!    identically for JS packages and for data matching nobody's declaration.
//! 3. **Declared shapes (optional enrichment).** The erased type-alias name and, when the
//!    payload type is declared in the producer's entry module, that declaration's source —
//!    advisory display metadata carried from static extraction, never the source of
//!    consumer types.
//!
//! **Entry budget.** Entries are never evicted (session history), but entry *count* is the
//! one unbounded dimension a hostile or buggy producer can grow, so each producer carries a
//! cap ([`MAX_ENTRIES_PER_PRODUCER`]): at the cap, new **undeclared** entries are refused
//! with a one-time teaching diagnostic (the catalogue is informational — refusal is safe),
//! while declared entries — bounded by code size — are always admitted and existing entries
//! keep recording. Dynamically-created handles surface as undeclared entries and are
//! exactly the unbounded minting channel the cap exists for. The same discipline bounds
//! each entry's *size*: the all-history shape accumulator is budgeted
//! ([`MAX_SHAPE_BUDGET_PER_ENTRY`]) — payloads that mint keys dynamically (a group table
//! keyed by member name, a hostile feed of fresh random keys) would otherwise grow one
//! entry's accumulator, and every snapshot's render of it, without bound. At exhaustion
//! new positions render as an elision marker (`...`) while already-tracked positions keep
//! merging.
//!
//! **Deferred sample parsing.** The ring stores the raw (already truncated) payload text;
//! parsing for shape inference happens only while a store tab is subscribed. Subscriber
//! presence is read live at each record through the broadcast handle the runtime attaches
//! ([`RuntimeCatalogue::attach_subscriber_probe`]), so a tab that subscribes mid-turn is
//! honored from that sample on — never deferred to the next drain point
//! ([`RuntimeCatalogue::set_subscribed`] pushes the flag for probe-less harnesses). A
//! subscribed session merges each sample into the entry's [`ShapeAcc`] as it is recorded;
//! an unsubscribed session records raw text only, and the ring backlog catch-up-parses when
//! a snapshot is next built. The inferred shape is therefore **over all history** — an
//! accumulator cannot un-merge ring-evicted samples, and occurrence counts already work
//! this way — with one carve-out: samples both recorded while unsubscribed *and* ring-
//! evicted before the next snapshot never reach the accumulator.
//!
//! Everything here is host-side: browsing the catalogue never touches V8. Entries and
//! samples are session-scoped (they survive engine rebuilds, like the store tree); the
//! declared/confirmed flags are engine-scoped and rebuilt by the next engine's registration
//! pass ([`RuntimeCatalogue::reset_engine_state`]).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use smudgy_cloud::Node;

use super::store::SessionStore;

/// The catalogue handle shared (the same `Rc`) into every isolate's ops and the runtime's
/// broadcast point.
pub(crate) type SharedCatalogue = Rc<RefCell<RuntimeCatalogue>>;

/// Which interop primitive a catalogue entry describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogueKind {
    State,
    Event,
    Procedure,
}

impl CatalogueKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Event => "event",
            Self::Procedure => "procedure",
        }
    }

    /// Index into a per-producer [`ProducerEntries::by_kind`] array; [`KINDS`] is the
    /// inverse. Ordered like the enum so snapshot enumeration keeps the historical
    /// `(producer, kind, name)` sort order.
    const fn index(self) -> usize {
        match self {
            Self::State => 0,
            Self::Event => 1,
            Self::Procedure => 2,
        }
    }
}

/// [`CatalogueKind::index`]'s inverse, in enumeration order.
const KINDS: [CatalogueKind; 3] = [
    CatalogueKind::State,
    CatalogueKind::Event,
    CatalogueKind::Procedure,
];

/// Ring size per entry: enough recent history to see a shape and a cadence, small enough
/// that a chatty event costs nothing to retain.
pub const SAMPLE_RING_CAP: usize = 20;

/// Payload size cap per retained sample. An oversized payload is kept truncated for display
/// and excluded from shape inference (a truncated JSON text no longer parses).
pub const SAMPLE_PAYLOAD_CAP: usize = 4096;

/// Catalogue entries admitted per producer before new **undeclared** entries are refused
/// (declared entries — bounded by code size — are always admitted; nothing is ever
/// evicted). Generous by an order of magnitude over real producers, which catalogue a
/// handful of handles plus their ad-hoc store keys; only unbounded dynamic minting — the
/// one growth channel the store budgets don't already bound — approaches it.
pub const MAX_ENTRIES_PER_PRODUCER: usize = 512;

/// Budget for one entry's all-history shape accumulator, charged as the accumulator
/// grows: a new object field costs its key length plus [`SHAPE_NODE_COST`], a new
/// array-element accumulator [`SHAPE_NODE_COST`]. Already-tracked positions merge for
/// free, so real payload shapes — tens of fields with short names — sit orders of
/// magnitude below the budget; only payloads that mint keys dynamically (a group table
/// keyed by member name, a hostile feed of fresh random keys per line) approach it. The
/// budget bounds both the accumulator's memory and the rendered shape each snapshot
/// serializes from it; at exhaustion new positions are elided and the render marks the
/// elision (`...`).
pub const MAX_SHAPE_BUDGET_PER_ENTRY: usize = 8 * 1024;

/// Flat budget charge per retained accumulator node (an object field's accumulator, an
/// array-element accumulator), alongside the field's key bytes — approximates per-node
/// overhead so [`MAX_SHAPE_BUDGET_PER_ENTRY`] tracks real memory, not just key text.
const SHAPE_NODE_COST: usize = 16;

/// One observed emission/post. Immutable once recorded; the ring and every snapshot share
/// one allocation per sample (`Arc`), so snapshot assembly is refcount bumps, not clones.
#[derive(Clone, Debug)]
pub struct CatalogueSample {
    /// Wall-clock capture time, milliseconds since the Unix epoch.
    pub at_epoch_ms: u64,
    /// The host-stamped origin: the producer itself for an event, the poster for a message.
    pub sender: Arc<str>,
    /// The raw payload text, truncated to [`SAMPLE_PAYLOAD_CAP`] — display form and (when
    /// not truncated) the deferred-parse input for shape inference.
    pub display: String,
    pub truncated: bool,
}

/// One catalogued interop entry (a state key, an event, or a message).
#[derive(Debug)]
struct CatalogueEntry {
    /// First-seen casing (declaration first, else first observation).
    name: Arc<str>,
    /// Statically declared by the producer's entry module (tier 1; engine-scoped).
    declared: bool,
    /// The producer's constructor ran this engine run — also how dynamically-created
    /// handles surface (tier 1; engine-scoped).
    runtime_confirmed: bool,
    /// The exported erased type alias, when the producer declares one (tier 3).
    type_alias: Option<Arc<str>>,
    /// The payload type's declaration source, when found in the entry module (tier 3).
    declared_shape: Option<Arc<str>>,
    /// Recent payloads, newest last (tier 2; events/messages only — state's sample is the
    /// store tree itself).
    samples: VecDeque<Arc<CatalogueSample>>,
    /// Total observed occurrences (the ring is bounded; this is not).
    occurrences: u64,
    /// All-history shape accumulator (see the module docs for the exact "all history"
    /// carve-out under deferred parsing).
    shape: ShapeAcc,
    /// Remaining accumulator budget ([`MAX_SHAPE_BUDGET_PER_ENTRY`]); every merge draws
    /// new nodes from it, so the accumulator — and the shape rendered from it per
    /// snapshot — stays bounded no matter how many distinct keys the payloads mint.
    shape_budget: usize,
    /// Merge watermark: occurrences already folded into `shape` (parsed, or skipped as
    /// truncated/unparsable). The gap to `occurrences` is the unparsed ring backlog.
    shape_merged: u64,
}

impl CatalogueEntry {
    fn new(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            declared: false,
            runtime_confirmed: false,
            type_alias: None,
            declared_shape: None,
            samples: VecDeque::new(),
            occurrences: 0,
            shape: ShapeAcc::default(),
            shape_budget: MAX_SHAPE_BUDGET_PER_ENTRY,
            shape_merged: 0,
        }
    }

    /// Fold the unmerged ring backlog into the shape accumulator (deferred parsing's
    /// catch-up: samples recorded while no subscriber existed). Samples that were both
    /// recorded unsubscribed and already ring-evicted are gone for good — the documented
    /// all-history carve-out.
    fn catch_up_shape(&mut self) {
        let unmerged = self.occurrences.saturating_sub(self.shape_merged);
        if unmerged == 0 {
            return;
        }
        let take = usize::try_from(unmerged)
            .unwrap_or(usize::MAX)
            .min(self.samples.len());
        for sample in self.samples.iter().skip(self.samples.len() - take) {
            if !sample.truncated
                && let Ok(value) = serde_json::from_str::<Value>(&sample.display)
            {
                self.shape.add(&value, &mut self.shape_budget);
            }
        }
        self.shape_merged = self.occurrences;
    }
}

/// One producer's committed store subtree in a snapshot: the tree's `Node` root, shared
/// with the committed store (an `Arc`-interior bump, never a deep copy) so the inspector
/// walks exactly the store's structure lazily — and `Arc::ptr_eq` against a previous
/// snapshot's node identifies unchanged subtrees across generations.
#[derive(Clone, Debug)]
pub struct ProducerView {
    /// `"user"` / `"smudgy://owner/name"`.
    pub producer: String,
    pub tree: Node,
    pub entries: u64,
    pub bytes: u64,
}

/// One catalogue entry in a snapshot, shaped for rendering. String fields are shared with
/// the catalogue's own entries (`Arc`), so building a view allocates nothing per field.
#[derive(Clone, Debug)]
pub struct CatalogueEntryView {
    pub producer: Arc<str>,
    pub kind: CatalogueKind,
    pub name: Arc<str>,
    pub declared: bool,
    pub runtime_confirmed: bool,
    pub type_alias: Option<Arc<str>>,
    pub declared_shape: Option<Arc<str>>,
    /// Rendered from the entry's all-history accumulator; `None` with no parsable samples.
    pub inferred_shape: Option<String>,
    pub occurrences: u64,
    /// Newest last.
    pub samples: Vec<Arc<CatalogueSample>>,
}

/// A point-in-time view of everything interop-shaped in the session, served to the UI.
#[derive(Clone, Debug, Default)]
pub struct CatalogueSnapshot {
    pub producers: Vec<ProducerView>,
    pub entries: Vec<CatalogueEntryView>,
}

/// The catalogue broadcast: a coalesced full snapshot whenever anything interop-shaped
/// changed (sent only while a window is subscribed, on the [`CatalogueCadence`] the
/// runtime's drain point drives).
#[derive(Clone, Debug)]
pub enum CatalogueEvent {
    Snapshot(Arc<CatalogueSnapshot>),
}

/// One producer's catalogue partition: its entries nested by kind then folded name, plus
/// the entry-budget bookkeeping. The nesting (rather than one flat
/// `(producer, kind, name)` tuple key) is what lets every lookup probe with **borrowed**
/// strings (`Arc<str>: Borrow<str>`) — the hot sample/observe paths find an existing
/// entry with zero key allocation; `Arc` keys are minted only when an entry is inserted.
#[derive(Debug, Default)]
struct ProducerEntries {
    /// Entries admitted — the budget denominator ([`MAX_ENTRIES_PER_PRODUCER`]).
    admitted: usize,
    /// Whether this producer's refusal diagnostic already went out (one teaching notice).
    refusal_warned: bool,
    /// Entries per kind, keyed by folded name; indexed by [`CatalogueKind::index`].
    by_kind: [BTreeMap<Arc<str>, CatalogueEntry>; KINDS.len()],
}

#[derive(Default)]
pub struct RuntimeCatalogue {
    /// `BTreeMap` so snapshots enumerate in a stable producer-grouped order with no sort
    /// pass; nested per-producer maps keep hit-path lookups allocation-free (see
    /// [`ProducerEntries`]).
    entries: BTreeMap<Arc<str>, ProducerEntries>,
    /// Refusal diagnostics awaiting echo — drained by the runtime's drain point, which owns
    /// the session's echo channel (ops that mint entries indirectly, and host emitters with
    /// no `OpState` at all, share this one surfacing path).
    pending_notices: Vec<String>,
    /// Whether a store tab is subscribed — pushed by the runtime at each drain point.
    /// Authoritative only while no [`Self::attach_subscriber_probe`] handle is attached
    /// (unit tests and benches); the probe reads live subscriber presence per record.
    subscribed: bool,
    /// The catalogue broadcast handle, attached by the runtime at construction so the
    /// record path reads subscriber presence where it changes (the sender's receiver
    /// count — one atomic load) instead of a flag that lags by up to a full expansion.
    subscriber_probe: Option<tokio::sync::broadcast::Sender<CatalogueEvent>>,
    /// Whether anything changed since the last consumed snapshot (store-tree changes are
    /// marked by the runtime's flush hook).
    dirty: bool,
}

impl RuntimeCatalogue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit-or-find the entry at `(producer, kind, folded)`: the existing entry, a freshly
    /// inserted one (marking the catalogue dirty), or `None` when the producer is at its
    /// entry budget and the entry is not a declaration — the refusal queues the one-time
    /// teaching notice. Never evicts. The hit path costs a producer refcount bump and
    /// borrowed-`str` map probes — key `Arc`s are minted only on insert.
    fn admit(
        &mut self,
        producer: &Arc<str>,
        kind: CatalogueKind,
        folded: &str,
        display_name: &str,
        declared: bool,
    ) -> Option<&mut CatalogueEntry> {
        let slot = self.entries.entry(Arc::clone(producer)).or_default();
        if !slot.by_kind[kind.index()].contains_key(folded) {
            if !declared && slot.admitted >= MAX_ENTRIES_PER_PRODUCER {
                if !slot.refusal_warned {
                    slot.refusal_warned = true;
                    self.pending_notices.push(format!(
                        "[interop] {producer}: the session catalogue is full for this \
                         producer ({MAX_ENTRIES_PER_PRODUCER} entries), so new undeclared \
                         names are no longer catalogued (declared handles are always \
                         admitted, and existing entries keep recording). Hitting the cap \
                         usually means state keys, events, or messages are being minted \
                         dynamically without bound; publish under a fixed set of names."
                    ));
                }
                return None;
            }
            slot.admitted += 1;
            self.dirty = true;
            slot.by_kind[kind.index()].insert(Arc::from(folded), CatalogueEntry::new(display_name));
        }
        slot.by_kind[kind.index()].get_mut(folded)
    }

    /// Tier 1 (+3): register one statically-extracted declaration. Called per handle when
    /// the engine builds; re-registration after a reload refreshes the declared metadata.
    /// Declarations are bounded by code size, so they bypass the entry budget.
    ///
    /// # Panics
    ///
    /// Never: declared admission bypasses the entry budget, the one refusal condition.
    pub fn declare(
        &mut self,
        producer: &str,
        kind: CatalogueKind,
        name: &str,
        type_alias: Option<&str>,
        declared_shape: Option<&str>,
    ) {
        let producer: Arc<str> = Arc::from(producer);
        let entry = self
            .admit(&producer, kind, &folded(name), name, true)
            .expect("declared entries are always admitted");
        entry.declared = true;
        entry.name = Arc::from(name);
        entry.type_alias = type_alias.map(Arc::from);
        entry.declared_shape = declared_shape.map(Arc::from);
        self.dirty = true;
    }

    /// Tier 1: the producer's constructor ran (`state()`/`event()`/`message()`), which is
    /// also how dynamically-created handles surface as runtime-confirmed entries — and why
    /// this path is budget-refusable: dynamic construction is unbounded minting.
    pub fn confirm_runtime(&mut self, producer: &Arc<str>, kind: CatalogueKind, name: &str) {
        if let Some(entry) = self.admit(producer, kind, &folded(name), name, false) {
            entry.runtime_confirmed = true;
            self.dirty = true;
        }
    }

    /// Tier 1: an ad-hoc store key observed at a write with no matching declaration —
    /// catalogued at first write with provenance "undeclared" (the entry exists; `declared`
    /// stays false). Only an actual insert dirties the catalogue; the per-write hit path
    /// (every `set` observes its root key) allocates nothing.
    pub fn observe_state_key(&mut self, producer: &Arc<str>, name: &str) {
        let _ = self.admit(producer, CatalogueKind::State, &folded(name), name, false);
    }

    /// Tier 2, interned-identity path: record one observed emission whose catalogue key
    /// strings were resolved at handle construction (`emit` — the per-line hot path). Costs
    /// refcount bumps plus the sample's own display copy; while unsubscribed the payload is
    /// never parsed.
    pub fn sample_interned(
        &mut self,
        producer: &Arc<str>,
        kind: CatalogueKind,
        name: &Arc<str>,
        name_folded: &Arc<str>,
        sender: &Arc<str>,
        payload_json: &str,
    ) {
        self.record_sample(producer, kind, name, name_folded, Arc::clone(sender), payload_json);
    }

    /// Whether shape merging should happen for a sample recorded right now: the attached
    /// probe's live receiver count when the runtime wired one (subscriber presence is read
    /// where it changes, so no sample recorded while a tab is open is ever treated as
    /// unsubscribed), else the drain-pushed flag ([`Self::set_subscribed`]).
    fn subscribed_now(&self) -> bool {
        self.subscriber_probe
            .as_ref()
            .map_or(self.subscribed, |tx| tx.receiver_count() > 0)
    }

    /// Tier 2, dynamic-name path: record one observed post/emission whose name arrives per
    /// call (`procedurePost`, the platform producers). Folds the name per call (borrowed
    /// when already lowercase); otherwise identical to [`Self::sample_interned`].
    pub fn sample_dynamic(
        &mut self,
        producer: &Arc<str>,
        kind: CatalogueKind,
        name: &str,
        sender: &str,
        payload_json: &str,
    ) {
        self.record_sample(producer, kind, name, &folded(name), Arc::from(sender), payload_json);
    }

    /// The one sample choke point (`docs/interop.md` §10): a bounded insert, recorded
    /// whether or not anyone subscribes — presence and history never depend on listeners.
    /// While subscribed (probed live, so a mid-turn subscribe counts from this sample on),
    /// the sample is parsed once here and merged into the entry's all-history shape (after
    /// catching up any unsubscribed backlog, so the watermark never skips unparsed
    /// samples); while unsubscribed, only the raw text is kept.
    fn record_sample(
        &mut self,
        producer: &Arc<str>,
        kind: CatalogueKind,
        name: &str,
        folded: &str,
        sender: Arc<str>,
        payload_json: &str,
    ) {
        let subscribed = self.subscribed_now();
        let Some(entry) = self.admit(producer, kind, folded, name, false) else {
            return;
        };
        if subscribed {
            entry.catch_up_shape();
        }
        entry.occurrences += 1;
        let truncated = payload_json.len() > SAMPLE_PAYLOAD_CAP;
        if subscribed {
            if !truncated
                && let Ok(value) = serde_json::from_str::<Value>(payload_json)
            {
                entry.shape.add(&value, &mut entry.shape_budget);
            }
            entry.shape_merged = entry.occurrences;
        }
        let display = if truncated {
            let mut end = SAMPLE_PAYLOAD_CAP;
            while end > 0 && !payload_json.is_char_boundary(end) {
                end -= 1;
            }
            payload_json[..end].to_string()
        } else {
            payload_json.to_string()
        };
        let sample = Arc::new(CatalogueSample {
            at_epoch_ms: epoch_ms(),
            sender,
            display,
            truncated,
        });
        if entry.samples.len() >= SAMPLE_RING_CAP {
            entry.samples.pop_front();
        }
        entry.samples.push_back(sample);
        self.dirty = true;
    }

    /// Note a change that only the snapshot can show (the runtime calls this when a store
    /// flush committed writes — the tree is served by the snapshot, not tracked here).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether a new snapshot is worth building — a peek; [`Self::take_dirty`] consumes.
    /// The cadence decision reads this without consuming, so a send deferred inside the
    /// broadcast window leaves the flag standing for the trailing-edge drain.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Consume the dirty flag (the send path, immediately before building the snapshot).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Attach the catalogue broadcast handle as the live subscriber probe: the record path
    /// reads the sender's receiver count (one atomic load) per sample, so a store tab that
    /// subscribes mid-turn is honored immediately — a drain-pushed flag alone would lag by
    /// up to a full expansion and lose a >ring burst recorded in the gap to the all-history
    /// carve-out. The runtime attaches this once at construction; the handle survives
    /// engine rebuilds with the catalogue.
    pub fn attach_subscriber_probe(&mut self, tx: tokio::sync::broadcast::Sender<CatalogueEvent>) {
        self.subscriber_probe = Some(tx);
    }

    /// Push the subscriber state observed at the runtime's drain point. While unsubscribed,
    /// samples are recorded raw and never parsed (deferred parsing); the backlog
    /// catch-up-parses at the next snapshot. With a probe attached
    /// ([`Self::attach_subscriber_probe`]) the record path reads the live count instead;
    /// this flag then only serves probe-less harnesses.
    pub fn set_subscribed(&mut self, subscribed: bool) {
        self.subscribed = subscribed;
    }

    /// Drain the entry-budget refusal notices queued since the last drain (the runtime
    /// echoes them to the session — one teaching diagnostic per producer).
    pub fn take_refusal_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_notices)
    }

    /// Engine teardown: declared/confirmed are per-engine facts (the next engine's
    /// registration pass rebuilds them); entries and samples are session history and stay.
    pub fn reset_engine_state(&mut self) {
        for slot in self.entries.values_mut() {
            for map in &mut slot.by_kind {
                for entry in map.values_mut() {
                    entry.declared = false;
                    entry.runtime_confirmed = false;
                }
            }
        }
        self.dirty = true;
    }

    /// Build the UI-facing snapshot: the committed store trees (shared `Node` roots, not
    /// copies) plus every catalogue entry with its all-history inferred shape. Takes `&mut`
    /// because this is where deferred parsing settles: each entry's unmerged ring backlog
    /// is folded into its accumulator before rendering.
    #[must_use]
    pub fn snapshot(&mut self, store: &SessionStore) -> CatalogueSnapshot {
        let producers = store
            .snapshot_producers()
            .into_iter()
            .map(|(producer, tree, usage)| ProducerView {
                producer: producer.to_string(),
                tree,
                entries: usage.entries,
                bytes: usage.bytes,
            })
            .collect();
        let mut entries = Vec::new();
        for (producer, slot) in &mut self.entries {
            for (kind, map) in KINDS.iter().zip(&mut slot.by_kind) {
                for entry in map.values_mut() {
                    entry.catch_up_shape();
                    entries.push(CatalogueEntryView {
                        producer: Arc::clone(producer),
                        kind: *kind,
                        name: Arc::clone(&entry.name),
                        declared: entry.declared,
                        runtime_confirmed: entry.runtime_confirmed,
                        type_alias: entry.type_alias.clone(),
                        declared_shape: entry.declared_shape.clone(),
                        inferred_shape: if entry.shape.is_empty() {
                            None
                        } else {
                            Some(entry.shape.render())
                        },
                        occurrences: entry.occurrences,
                        samples: entry.samples.iter().cloned().collect(),
                    });
                }
            }
        }
        CatalogueSnapshot { producers, entries }
    }
}

/// The uniform ASCII case fold for names that arrive per call rather than interned —
/// borrowed through when already lowercase (the common case), so the hit paths that probe
/// the entry maps with it allocate nothing.
fn folded(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

fn epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------------------
// Broadcast cadence: leading-edge send + trailing coalesce.
// ---------------------------------------------------------------------------

/// The catalogue broadcast window (~30 Hz). A plain throttle would delay the dominant
/// sparse-update case while the terminal already shows the value, and a wait-for-quiet
/// debounce starves the display through a sustained burst — so the cadence is
/// **leading-edge + trailing coalesce**: the first dirty drain after a quiet spell sends
/// immediately (zero added latency for the common case); dirty drains inside the window
/// defer, and the runtime arms a one-shot timer at the window's edge so a burst's final
/// state lands within it instead of waiting for the 500 ms safety tick.
pub const CATALOGUE_SEND_WINDOW: std::time::Duration = std::time::Duration::from_millis(33);

/// What the drain point should do about the catalogue broadcast right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CadenceDecision {
    /// Build and send a snapshot now (and call [`CatalogueCadence::sent`]).
    SendNow,
    /// Dirty state must wait out the window: leave the dirty flag standing and arm a
    /// one-shot wake at the deadline (the trailing edge).
    Defer(tokio::time::Instant),
    /// Nothing to send (clean, or nobody subscribed) — disarm any trailing wake.
    Idle,
}

/// The broadcast cadence state ([`CATALOGUE_SEND_WINDOW`]): pure decision logic, kept out
/// of the runtime's run loop so it is unit-testable. The runtime feeds it each drain and
/// owns the actual timer arm.
#[derive(Debug, Default)]
pub struct CatalogueCadence {
    /// When the last snapshot went out; `None` before the first send.
    last_send: Option<tokio::time::Instant>,
}

impl CatalogueCadence {
    /// Decide at a drain point. `new_subscriber` bypasses the window — a freshly opened
    /// tab needs its first snapshot immediately, dirty or not; otherwise dirty state sends
    /// on the leading edge (≥ window since the last send) and defers inside the window.
    #[must_use]
    pub fn on_drain(
        &self,
        dirty: bool,
        subscribed: bool,
        new_subscriber: bool,
        now: tokio::time::Instant,
    ) -> CadenceDecision {
        if !subscribed {
            return CadenceDecision::Idle;
        }
        if new_subscriber {
            return CadenceDecision::SendNow;
        }
        if !dirty {
            return CadenceDecision::Idle;
        }
        match self.last_send {
            Some(at) if now < at + CATALOGUE_SEND_WINDOW => {
                CadenceDecision::Defer(at + CATALOGUE_SEND_WINDOW)
            }
            _ => CadenceDecision::SendNow,
        }
    }

    /// Record a completed send (opens a fresh window).
    pub fn sent(&mut self, now: tokio::time::Instant) {
        self.last_send = Some(now);
    }
}

// ---------------------------------------------------------------------------
// Shape inference (tier 2): a recursive JSON type walk, merged across samples.
// ---------------------------------------------------------------------------

/// Accumulated shape of one position across samples. Growth is charged against the
/// owning entry's budget ([`MAX_SHAPE_BUDGET_PER_ENTRY`]): merging into positions the
/// accumulator already tracks is free, so the budget binds exactly the unbounded axis —
/// distinct keys/positions ever seen — and an exhausted accumulator records elision
/// markers instead of growing.
#[derive(Debug, Default)]
struct ShapeAcc {
    /// Primitive type names seen here (`null` / `boolean` / `number` / `string`).
    primitives: std::collections::BTreeSet<&'static str>,
    /// Merged object shape, when objects were seen here.
    object: Option<ObjectAcc>,
    /// Merged element shape, when arrays were seen here.
    array: Option<Box<ShapeAcc>>,
    /// Array content was seen here that the exhausted budget refused to track; renders
    /// as an `...` union member.
    elided: bool,
}

#[derive(Debug, Default)]
struct ObjectAcc {
    /// How many objects merged here — a field seen in fewer is optional.
    count: u64,
    /// Field name → (shape, times seen).
    fields: BTreeMap<String, (ShapeAcc, u64)>,
    /// Fields were seen here that the exhausted budget refused to track; renders as a
    /// trailing `...` member inside the braces.
    elided: bool,
}

/// Deduct `cost` from the remaining budget if it fits; a refusal leaves the budget
/// untouched (the caller records an elision instead of the node).
fn charge(budget: &mut usize, cost: usize) -> bool {
    match budget.checked_sub(cost) {
        Some(rest) => {
            *budget = rest;
            true
        }
        None => false,
    }
}

impl ShapeAcc {
    /// Whether nothing has merged here (renders as no inferred shape at all).
    fn is_empty(&self) -> bool {
        self.primitives.is_empty() && self.object.is_none() && self.array.is_none() && !self.elided
    }

    /// Merge one observed value, drawing every newly tracked node from `budget`: a new
    /// object field costs its key plus [`SHAPE_NODE_COST`], a new array-element
    /// accumulator [`SHAPE_NODE_COST`]. Positions already tracked merge for free.
    fn add(&mut self, value: &Value, budget: &mut usize) {
        match value {
            Value::Null => {
                self.primitives.insert("null");
            }
            Value::Bool(_) => {
                self.primitives.insert("boolean");
            }
            Value::Number(_) => {
                self.primitives.insert("number");
            }
            Value::String(_) => {
                self.primitives.insert("string");
            }
            Value::Array(items) => {
                if self.array.is_none() {
                    if !charge(budget, SHAPE_NODE_COST) {
                        self.elided = true;
                        return;
                    }
                    self.array = Some(Box::default());
                }
                if let Some(elem) = self.array.as_mut() {
                    for item in items {
                        elem.add(item, budget);
                    }
                }
            }
            Value::Object(map) => {
                let obj = self.object.get_or_insert_with(ObjectAcc::default);
                obj.count += 1;
                for (key, item) in map {
                    if let Some((shape, seen)) = obj.fields.get_mut(key) {
                        shape.add(item, budget);
                        *seen += 1;
                    } else if charge(budget, SHAPE_NODE_COST + key.len()) {
                        let (shape, seen) = obj.fields.entry(key.clone()).or_default();
                        shape.add(item, budget);
                        *seen += 1;
                    } else {
                        obj.elided = true;
                    }
                }
            }
        }
    }

    fn render(&self) -> String {
        let mut parts: Vec<String> = self.primitives.iter().map(ToString::to_string).collect();
        if let Some(obj) = &self.object {
            let mut fields: Vec<String> = obj
                .fields
                .iter()
                .map(|(name, (shape, seen))| {
                    let optional = if *seen < obj.count { "?" } else { "" };
                    format!("{}{optional}: {}", field_name(name), shape.render())
                })
                .collect();
            if obj.elided {
                fields.push("...".to_string());
            }
            if fields.is_empty() {
                parts.push("{}".to_string());
            } else {
                parts.push(format!("{{ {} }}", fields.join("; ")));
            }
        }
        if let Some(elem) = &self.array {
            let rendered = elem.render();
            if rendered == "unknown" {
                parts.push("unknown[]".to_string());
            } else if rendered.contains(" | ") {
                parts.push(format!("({rendered})[]"));
            } else {
                parts.push(format!("{rendered}[]"));
            }
        }
        if self.elided {
            parts.push("...".to_string());
        }
        if parts.is_empty() {
            "unknown".to_string()
        } else {
            parts.join(" | ")
        }
    }
}

/// Spell a field name for a rendered shape: identifiers bare, anything else quoted.
fn field_name(name: &str) -> String {
    let is_ident = !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            c == '_' || c == '$' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
        });
    if is_ident {
        name.to_string()
    } else {
        serde_json::to_string(name).expect("a string always serializes")
    }
}

/// Merge `values` into one rendered TS-ish shape: primitives by name, objects field-by-field
/// with fields marked optional when they vary across samples, arrays by merged element shape.
/// Bounded like an entry's accumulator ([`MAX_SHAPE_BUDGET_PER_ENTRY`], per call).
#[must_use]
pub fn infer_shape(values: &[&Value]) -> String {
    let mut acc = ShapeAcc::default();
    let mut budget = MAX_SHAPE_BUDGET_PER_ENTRY;
    for value in values {
        acc.add(value, &mut budget);
    }
    acc.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shape(values: &[Value]) -> String {
        infer_shape(&values.iter().collect::<Vec<_>>())
    }

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    #[test]
    fn infers_primitives_objects_and_optional_fields() {
        assert_eq!(shape(&[json!(1), json!(2)]), "number");
        assert_eq!(shape(&[json!(1), json!("x")]), "number | string");
        assert_eq!(
            shape(&[json!({ "hp": 1, "tag": "a" }), json!({ "hp": 2 })]),
            "{ hp: number; tag?: string }"
        );
        assert_eq!(shape(&[json!(null), json!(true)]), "boolean | null");
    }

    #[test]
    fn infers_arrays_and_odd_keys() {
        assert_eq!(shape(&[json!([1, 2, 3])]), "number[]");
        assert_eq!(shape(&[json!([1, "x"])]), "(number | string)[]");
        assert_eq!(shape(&[json!([])]), "unknown[]");
        assert_eq!(
            shape(&[json!({ "Mr. Foo": 1 })]),
            "{ \"Mr. Foo\": number }"
        );
        assert_eq!(shape(&[]), "unknown");
    }

    #[test]
    fn catalogue_entries_index_samples_and_survive_engine_reset() {
        let mut cat = RuntimeCatalogue::new();
        let producer = arc("smudgy://wbk/tracker");
        cat.declare(&producer, CatalogueKind::Event, "Prompt", Some("PromptEvent"), None);
        cat.sample_dynamic(
            &producer,
            CatalogueKind::Event,
            "prompt",
            "smudgy://wbk/tracker",
            r#"{"hp":1}"#,
        );
        cat.sample_dynamic(
            &producer,
            CatalogueKind::Event,
            "prompt",
            "smudgy://wbk/tracker",
            r#"{"hp":2}"#,
        );
        cat.observe_state_key(&arc("user"), "adhoc");
        assert!(cat.is_dirty(), "peeking leaves the flag standing");
        assert!(cat.take_dirty());
        assert!(!cat.take_dirty(), "consuming resets the flag");

        let store = SessionStore::new();
        let snap = cat.snapshot(&store);
        assert_eq!(snap.entries.len(), 2);
        let prompt = snap
            .entries
            .iter()
            .find(|e| e.kind == CatalogueKind::Event)
            .unwrap();
        assert_eq!(&*prompt.name, "Prompt", "declaration casing wins");
        assert!(prompt.declared);
        assert_eq!(prompt.occurrences, 2);
        assert_eq!(prompt.inferred_shape.as_deref(), Some("{ hp: number }"));
        let adhoc = snap
            .entries
            .iter()
            .find(|e| e.kind == CatalogueKind::State)
            .unwrap();
        assert!(!adhoc.declared, "observed-but-undeclared provenance");

        cat.reset_engine_state();
        let snap = cat.snapshot(&SessionStore::new());
        let prompt = snap
            .entries
            .iter()
            .find(|e| e.kind == CatalogueKind::Event)
            .unwrap();
        assert!(!prompt.declared, "declared is an engine-scoped fact");
        assert_eq!(prompt.occurrences, 2, "samples are session history");
    }

    #[test]
    fn oversized_payloads_are_truncated_and_skip_inference() {
        let mut cat = RuntimeCatalogue::new();
        cat.set_subscribed(true);
        let big = format!("\"{}\"", "x".repeat(SAMPLE_PAYLOAD_CAP * 2));
        cat.sample_dynamic(&arc("user"), CatalogueKind::Event, "big", "user", &big);
        let snap = cat.snapshot(&SessionStore::new());
        let entry = &snap.entries[0];
        assert!(entry.samples[0].truncated);
        assert!(entry.samples[0].display.len() <= SAMPLE_PAYLOAD_CAP);
        assert!(entry.inferred_shape.is_none());
    }

    #[test]
    fn ring_is_bounded() {
        let mut cat = RuntimeCatalogue::new();
        let user = arc("user");
        for i in 0..(SAMPLE_RING_CAP + 5) {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", &i.to_string());
        }
        let snap = cat.snapshot(&SessionStore::new());
        assert_eq!(snap.entries[0].samples.len(), SAMPLE_RING_CAP);
        assert_eq!(snap.entries[0].occurrences, (SAMPLE_RING_CAP + 5) as u64);
        assert_eq!(snap.entries[0].samples[0].display, "5", "oldest dropped");
    }

    #[test]
    fn shape_is_over_all_history_while_subscribed() {
        let mut cat = RuntimeCatalogue::new();
        cat.set_subscribed(true);
        let user = arc("user");
        // Early samples carry `tag`; enough later ones without it evict every early sample
        // from the ring. The accumulator still remembers them: `tag` renders optional.
        for _ in 0..3 {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"hp":1,"tag":"a"}"#);
        }
        for _ in 0..(SAMPLE_RING_CAP + 5) {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"hp":2}"#);
        }
        let snap = cat.snapshot(&SessionStore::new());
        assert_eq!(
            snap.entries[0].inferred_shape.as_deref(),
            Some("{ hp: number; tag?: string }"),
            "ring eviction must not un-merge history"
        );
    }

    #[test]
    fn unsubscribed_samples_defer_parsing_and_catch_up_at_snapshot() {
        let mut cat = RuntimeCatalogue::new();
        let user = arc("user");
        // Recorded unsubscribed and evicted before any snapshot: lost to inference (the
        // documented carve-out) — only the ring backlog catches up.
        for _ in 0..3 {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"tag":"a"}"#);
        }
        for _ in 0..(SAMPLE_RING_CAP + 5) {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"hp":2}"#);
        }
        let snap = cat.snapshot(&SessionStore::new());
        assert_eq!(
            snap.entries[0].inferred_shape.as_deref(),
            Some("{ hp: number }"),
            "evicted-while-unsubscribed samples cannot be recovered"
        );
        // Once subscribed, later samples merge incrementally on top of the caught-up ring.
        cat.set_subscribed(true);
        cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"mp":3}"#);
        let snap = cat.snapshot(&SessionStore::new());
        assert_eq!(
            snap.entries[0].inferred_shape.as_deref(),
            Some("{ hp?: number; mp?: number }")
        );
    }

    #[test]
    fn shape_accumulator_is_budget_bounded_against_dynamic_keys() {
        let mut cat = RuntimeCatalogue::new();
        cat.set_subscribed(true);
        let user = arc("user");
        // A hostile feed: every sample mints fresh keys (~44 KiB of distinct key text in
        // total). The accumulator must saturate at its budget, not accrete every key.
        for i in 0..4000 {
            cat.sample_dynamic(
                &user,
                CatalogueKind::Event,
                "e",
                "user",
                &format!(r#"{{"dyn_{i:05}":1}}"#),
            );
        }
        // A retained (early) key keeps merging after exhaustion — the budget binds new
        // positions only.
        cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"dyn_00000":"x"}"#);
        let snap = cat.snapshot(&SessionStore::new());
        let shape = snap.entries[0]
            .inferred_shape
            .clone()
            .expect("parsable samples were merged");
        assert!(
            shape.contains("..."),
            "the exhausted accumulator marks the elision: {shape}"
        );
        assert!(
            shape.len() < 4 * MAX_SHAPE_BUDGET_PER_ENTRY,
            "the rendered shape is bounded by the budget, got {} bytes",
            shape.len()
        );
        assert!(
            shape.contains("dyn_00000?: number | string"),
            "retained positions keep merging at zero budget: {shape}"
        );
    }

    #[test]
    fn subscriber_probe_sees_a_mid_turn_subscriber_without_a_drain() {
        let (tx, first_rx) = tokio::sync::broadcast::channel::<CatalogueEvent>(4);
        drop(first_rx);
        let mut cat = RuntimeCatalogue::new();
        cat.attach_subscriber_probe(tx.clone());
        let user = arc("user");
        // No receiver: recording defers parsing exactly like the pushed-flag path.
        cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"cold":1}"#);
        // A tab subscribes with NO drain in between (no `set_subscribed` push). Samples
        // recorded from here on must merge at record time — a >ring burst in this gap
        // would otherwise be lost to the all-history carve-out — and the first merged
        // record also catches up the still-ringed unsubscribed backlog (`cold`).
        let rx = tx.subscribe();
        for _ in 0..3 {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"hp":1,"tag":"a"}"#);
        }
        for _ in 0..(SAMPLE_RING_CAP + 5) {
            cat.sample_dynamic(&user, CatalogueKind::Event, "e", "user", r#"{"hp":2}"#);
        }
        drop(rx);
        let snap = cat.snapshot(&SessionStore::new());
        assert_eq!(
            snap.entries[0].inferred_shape.as_deref(),
            Some("{ cold?: number; hp?: number; tag?: string }"),
            "samples recorded after a mid-turn subscribe survive ring eviction"
        );
    }

    #[test]
    fn entry_budget_refuses_undeclared_and_admits_declared() {
        let mut cat = RuntimeCatalogue::new();
        let producer = arc("smudgy://wbk/minty");
        for i in 0..MAX_ENTRIES_PER_PRODUCER {
            cat.sample_dynamic(&producer, CatalogueKind::Event, &format!("e{i}"), "user", "1");
        }
        assert!(cat.take_refusal_notices().is_empty(), "under the cap: no notice");
        let _ = cat.take_dirty();

        // At the cap: an undeclared entry is refused (not recorded at all) with one notice…
        cat.sample_dynamic(&producer, CatalogueKind::Event, "overflow", "user", "1");
        cat.confirm_runtime(&producer, CatalogueKind::Event, "overflow2");
        cat.observe_state_key(&producer, "overflow3");
        let notices = cat.take_refusal_notices();
        assert_eq!(notices.len(), 1, "one teaching notice per producer");
        assert!(notices[0].contains("smudgy://wbk/minty"));
        assert!(!cat.take_dirty(), "refusals do not dirty the catalogue");

        // …while declared entries are always admitted, and other producers are unaffected.
        cat.declare(&producer, CatalogueKind::Event, "Declared", None, None);
        cat.observe_state_key(&arc("user"), "fine");
        let snap = cat.snapshot(&SessionStore::new());
        let names: Vec<&str> = snap.entries.iter().map(|e| &*e.name).collect();
        assert!(names.contains(&"Declared"));
        assert!(names.contains(&"fine"));
        assert!(!names.contains(&"overflow"));
        assert_eq!(snap.entries.len(), MAX_ENTRIES_PER_PRODUCER + 2);

        // Existing entries keep recording at the cap.
        cat.sample_dynamic(&producer, CatalogueKind::Event, "e0", "user", "2");
        let snap = cat.snapshot(&SessionStore::new());
        let e0 = snap.entries.iter().find(|e| &*e.name == "e0").unwrap();
        assert_eq!(e0.occurrences, 2);
    }

    #[test]
    fn cadence_leads_defers_and_trails() {
        let mut cadence = CatalogueCadence::default();
        let t0 = tokio::time::Instant::now();

        // Nobody subscribed: always idle, dirty or not.
        assert_eq!(cadence.on_drain(true, false, false, t0), CadenceDecision::Idle);
        // First dirty drain: leading edge, no window yet.
        assert_eq!(cadence.on_drain(true, true, false, t0), CadenceDecision::SendNow);
        cadence.sent(t0);
        // Clean drain inside the window: nothing owed.
        assert_eq!(
            cadence.on_drain(false, true, false, t0 + CATALOGUE_SEND_WINDOW / 3),
            CadenceDecision::Idle
        );
        // Dirty inside the window: defer to the window's trailing edge.
        assert_eq!(
            cadence.on_drain(true, true, false, t0 + CATALOGUE_SEND_WINDOW / 2),
            CadenceDecision::Defer(t0 + CATALOGUE_SEND_WINDOW)
        );
        // A new subscriber bypasses the window (fresh tabs need their first snapshot now).
        assert_eq!(
            cadence.on_drain(false, true, true, t0 + CATALOGUE_SEND_WINDOW / 2),
            CadenceDecision::SendNow
        );
        // At/after the deadline the deferred state sends.
        assert_eq!(
            cadence.on_drain(true, true, false, t0 + CATALOGUE_SEND_WINDOW),
            CadenceDecision::SendNow
        );
    }
}
