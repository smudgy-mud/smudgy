//! The session store (`docs/interop.md` §2): a host-held, session-scoped
//! tree of JSON values, held as the structurally-shared immutable [`Node`] tree
//! (`smudgy_cloud::store_node` — `docs/interop-pre-gmcp-plan.md` §4). Producers write subtrees
//! they own with set-at-path (the only write op); consumers read snapshots synchronously and
//! watch for changes. Values cross the op boundary as JSON text either way; the tree shape is
//! host-internal.
//!
//! Semantics implemented here:
//! - **Keying is case-preserving, case-insensitive** (ASCII fold at write time, uniformly for
//!   dots and brackets): first-published casing is what enumeration displays; lookups fold.
//! - **Writes are turn-batched** through an order-preserving *write journal*: same-isolate reads
//!   within a turn observe the journal's net effect (read-your-writes), and the runtime flushes
//!   the journal before dispatching the next queued action (cross-isolate happens-before). The
//!   journal is a list, not a path→value map, so every same-turn write to one path survives to
//!   the flush — the ordering the per-write cadence below replays.
//! - **Two watch cadences** ([`WatchCadence`]): coalesced `watch` is one delivery per flush per
//!   watcher whose path is comparable to any written path (a write at or below the watched path
//!   changes its subtree; a write above it replaces it), carrying a snapshot of the watched
//!   path's final state; per-write `onWrite` replays the flushed journal — one delivery per
//!   set-at-path in write order, value-identical writes included, carrying `(path, snapshot)`.
//!   Both ride the same `CallJavascriptFunction` action + depth cap the event system uses.
//! - **Budgets** (entry count + total bytes per producer subtree) are enforced at the write
//!   choke point: a breaching write is rejected with an error naming the producer — never a
//!   silent eviction.
//! - **Previous generations** (`docs/interop-pre-gmcp-plan.md` §5): each flush that commits a
//!   producer's writes retains the root it displaces as that producer's *previous generation*
//!   — an `Arc` move, so the retention cost is the structural-sharing delta the new writes
//!   forced, and budgets keep charging the logical tree only, never retained generations. The
//!   `previous_*` reads resolve per reader, mirroring read-your-writes: an isolate mid-batch
//!   (its own writes in the journal) reads that batch's committed base; every other reader —
//!   to whom an open journal is invisible, like every read — reads the retained generation.
//!   `previousValue`'s anchor is therefore the state before the newest write batch *the
//!   reader can observe*, per producer.
//! - **Widget bindings** (interop.md §7) are host-side watchers with no JS side: `bind`
//!   dedupes a `(producer, path)` into a shared [`StoreBindingCell`] the UI's render closures
//!   read, a per-producer **path trie** finds the cells a flush invalidates, and the flush
//!   writes each dirty cell's committed snapshot (latest-wins; the UI coalesces per frame).
//!
//! The tree and its usage accounting outlive engine rebuilds (reloads don't drop state); the
//! journal, watchers, bindings, and diagnostic dedup sets are engine-scoped and reset by
//! [`SessionStore::reset_engine_state`]. All access happens on the one session thread.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use smudgy_cloud::store_node::{CONTAINER_OVERHEAD_BYTES, KEY_OVERHEAD_BYTES};
use smudgy_cloud::{Node, StoreBindingCell, StoreBindings};

pub use smudgy_cloud::Usage;

use super::script_engine::FunctionId;
use super::trigger::MatchCapture;
use super::{IsolateId, MAX_EVENT_DEPTH, Origin, RuntimeAction};

/// The session store handle shared (the same `Rc`) into every isolate's ops and the runtime's
/// flush points — legal because all isolates live on the one session thread.
pub(crate) type SharedSessionStore = Rc<std::cell::RefCell<SessionStore>>;

/// Which isolate is a package's interop **home** — the loader-known installed/trusted load whose
/// writes the store accepts (`docs/interop.md` §3). Version-blind on purpose: the
/// registry is rebuilt per engine from the lockfile *before* versions resolve, so top-level
/// writes during module evaluation already pass the gate, and a reload that lands a new version
/// keeps the same home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeIsolate {
    /// A trusted install: the package's code runs in the main isolate.
    Main,
    /// An untrusted install: the package's code runs in its own sandboxed isolate.
    OwnSandbox,
}

/// Per-engine map of installed package → home isolate, keyed by ASCII-folded `(owner, name)`.
/// Built from the lockfile before any module evaluates, then *pruned* of packages the load gates
/// (required-params / `min_smudgy_version`) subsequently refuse — so a blocked package is home
/// nowhere and a code-imported copy of it can't publish in its name. The `RefCell` exists for
/// that construction-time prune only; no op mutates it (phase 1 registers no homes at runtime; a
/// user-directed main-isolate load of an *uninstalled* package is deferred alongside activation
/// classing).
///
/// The op layer caches [`is_home`] verdicts in its per-isolate interned creator entries
/// (`ops::InteropIdentities`), which is sound exactly because this registry is fixed for the
/// engine's life — the interning table and the JS closures holding its ids die together on
/// rebuild. If the deferred runtime home registration above ever lands, those cached verdicts
/// must be invalidated (or the ops must resolve the verdict late) alongside the mutation.
pub(crate) type HomeRegistry = Rc<std::cell::RefCell<HashMap<(String, String), HomeIsolate>>>;

/// Whether `isolate` is `producer`'s home under `homes` — the write gate for `set` and `emit`.
/// User/module code is home exactly in the main isolate; a package is home in the isolate its
/// lockfile entry assigns (absent ⇒ not installed ⇒ nowhere is home, e.g. a copy embedded in
/// another package's closure).
pub(crate) fn is_home(homes: &HomeRegistry, producer: &ProducerKey, isolate: &IsolateId) -> bool {
    match producer {
        ProducerKey::User => *isolate == IsolateId::Main,
        // No isolate is ever home for a platform producer: the host is the sole writer
        // (`docs/gmcp-plan.md` §3.1), writing through `SessionStore::set` directly — the
        // op-layer seat machinery can never mint a producer seat for one.
        ProducerKey::Platform(_) => false,
        ProducerKey::Package { owner, name } => {
            match homes.borrow().get(&(owner.clone(), name.clone())) {
                Some(HomeIsolate::Main) => *isolate == IsolateId::Main,
                Some(HomeIsolate::OwnSandbox) => match isolate {
                    IsolateId::Package {
                        owner: iso_owner,
                        name: iso_name,
                        ..
                    } => iso_owner.eq_ignore_ascii_case(owner) && iso_name.eq_ignore_ascii_case(name),
                    IsolateId::Main => false,
                },
                None => false,
            }
        }
    }
}

/// A store producer the host itself maintains (`docs/gmcp-plan.md`). A closed set by design:
/// no creator descriptor resolves to one, so scripts can never obtain a producer seat — the
/// host writes through [`SessionStore::set`] directly and is structurally the sole writer.
/// The reserved scheme names (`package_resolver::PLATFORM_PRODUCERS`) are the superset; only
/// the members here are *store* producers (`sys`/`map` are event-only platform producers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlatformProducer {
    /// The GMCP tree: message name = path, payload replaces-at-path (`docs/gmcp-plan.md` §3).
    Gmcp,
    /// The MSDP tree: variable name = single-segment path, decoded value replaces
    /// (`docs/gmcp-mapping-plan.md` §9 item 3).
    Msdp,
}

impl PlatformProducer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gmcp => "gmcp",
            Self::Msdp => "msdp",
        }
    }
}

/// Owner of one producer subtree in the store. Package identity is version-independent (state
/// survives package updates) and ASCII-folded (the uniform fold applies everywhere names are
/// structural). `User` is the shared subtree for all main-isolate non-package code — user
/// scripts and local modules alike (`smudgy:state/user` in the consumer scheme, later).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProducerKey {
    User,
    /// A host-maintained subtree (consumer address = the platform name, e.g. `"gmcp"`).
    Platform(PlatformProducer),
    Package { owner: String, name: String },
}

impl ProducerKey {
    /// The producer a creator [`Origin`] publishes as: package code as its package, everything
    /// on main that isn't a package (user scripts, local modules) as the shared `user` subtree.
    #[must_use]
    pub fn from_origin(origin: &Origin) -> Self {
        match origin {
            Origin::User | Origin::Module { .. } => Self::User,
            Origin::Package { owner, name, .. } => Self::Package {
                owner: owner.to_ascii_lowercase(),
                name: name.to_ascii_lowercase(),
            },
        }
    }

    /// Parse a consumer-side producer address: `user`, a platform store producer (`gmcp`),
    /// or a package as `smudgy://owner/name` (`owner/name` also accepted). Folded like every
    /// structural name.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("user") {
            return Some(Self::User);
        }
        if spec.eq_ignore_ascii_case(PlatformProducer::Gmcp.as_str()) {
            return Some(Self::Platform(PlatformProducer::Gmcp));
        }
        if spec.eq_ignore_ascii_case(PlatformProducer::Msdp.as_str()) {
            return Some(Self::Platform(PlatformProducer::Msdp));
        }
        let coords = spec.strip_prefix("smudgy://").unwrap_or(spec);
        let (owner, name) = coords.split_once('/')?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self::Package {
            owner: owner.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
        })
    }
}

impl std::fmt::Display for ProducerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Platform(platform) => write!(f, "{}", platform.as_str()),
            Self::Package { owner, name } => write!(f, "smudgy://{owner}/{name}"),
        }
    }
}

/// A parsed store path: the segment list, original casing preserved (matching folds later).
/// Empty = the producer subtree's root. `Hash` is by exact segments (spelling included),
/// matching `Eq` — the interop identity table keys dedup entries by it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorePath(Vec<String>);

/// Why a path string failed to parse, surfaced verbatim to the calling script (a malformed
/// path is an author bug — loud, never a silent no-op).
#[derive(Debug, thiserror::Error)]
#[error("invalid store path: {0}")]
pub struct PathError(String);

impl StorePath {
    #[must_use]
    pub fn root() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Build a path structurally from pre-split segments — the GMCP producer's ingest path
    /// (message names split at dots, `docs/gmcp-plan.md` §3.1) and any other host-side
    /// caller with segments already in hand. The string grammar is never involved, so any
    /// non-empty segment text is a valid key (a spec-legal hyphenated GMCP package name is
    /// simply a key; consumers spell it bracket-quoted).
    ///
    /// # Errors
    ///
    /// Rejects empty segments and over-deep paths with the same loud [`PathError`] the
    /// grammar parser uses.
    pub fn from_segments<I>(segments: I) -> Result<Self, PathError>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut out = Vec::new();
        for segment in segments {
            if out.len() >= MAX_PATH_SEGMENTS {
                return Err(PathError(format!(
                    "path exceeds the {MAX_PATH_SEGMENTS}-segment depth limit"
                )));
            }
            let segment = segment.into();
            if segment.is_empty() {
                return Err(PathError("empty path segment".to_string()));
            }
            out.push(segment);
        }
        Ok(Self(out))
    }

    /// Parse the JS-ish path grammar (`docs/interop.md` §2): dot-separated
    /// identifier segments, brackets with single- or double-quoted keys otherwise —
    /// `Char.Vitals.hp`, `groupies["Mr. Foo"].hp`, `["odd key"].x`. Paths are lookups only;
    /// there is no expression syntax. Array indexing is not part of the phase-1 grammar
    /// (collections are addressed whole; object keys are the unit of identity).
    ///
    /// # Errors
    ///
    /// Returns a [`PathError`] naming the malformation (bad separator, unterminated or empty
    /// quoted key, over-deep path) — a malformed path is an author bug, rejected loudly.
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self::root());
        }
        let bytes = raw.as_bytes();
        let mut segments = Vec::new();
        let mut i = 0usize;
        let mut expect_separator = false;
        while i < bytes.len() {
            // Bound depth before parsing the next segment — set-at-path recursion (and the read
            // walks) run per segment on a fixed stack, so an unbounded path overflows it.
            if segments.len() >= MAX_PATH_SEGMENTS {
                return Err(PathError(format!(
                    "path exceeds the {MAX_PATH_SEGMENTS}-segment depth limit in {raw:?}"
                )));
            }
            if expect_separator {
                match bytes[i] {
                    b'.' => {
                        i += 1;
                        expect_separator = false;
                        continue;
                    }
                    // Bracket segments self-delimit; fall through to the bracket parse.
                    b'[' => {}
                    other => {
                        return Err(PathError(format!(
                            "expected '.' or '[' after a segment, found {:?} in {raw:?}",
                            char::from(other)
                        )));
                    }
                }
            }
            if bytes[i] == b'[' {
                let Some(quote) = bytes.get(i + 1).copied().filter(|b| *b == b'"' || *b == b'\'')
                else {
                    return Err(PathError(format!(
                        "brackets take a quoted key (e.g. [\"key\"]) in {raw:?}"
                    )));
                };
                // Scan to the closing quote; quoted keys carry no escape syntax (a key
                // containing a quote of one kind is written with the other).
                let start = i + 2;
                let Some(end) = raw[start..].find(char::from(quote)).map(|off| start + off)
                else {
                    return Err(PathError(format!("unterminated quoted key in {raw:?}")));
                };
                if bytes.get(end + 1) != Some(&b']') {
                    return Err(PathError(format!("expected ']' after the quoted key in {raw:?}")));
                }
                if start == end {
                    return Err(PathError(format!("empty key in {raw:?}")));
                }
                segments.push(raw[start..end].to_string());
                i = end + 2;
            } else {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i], i == start) {
                    i += 1;
                }
                if i == start {
                    return Err(PathError(format!(
                        "expected an identifier segment at byte {start} in {raw:?}"
                    )));
                }
                segments.push(raw[start..i].to_string());
            }
            expect_separator = true;
        }
        // A consumed '.' with nothing after it (`a.`) left the parser expecting a segment.
        if !expect_separator {
            return Err(PathError(format!("trailing '.' in {raw:?}")));
        }
        Ok(Self(segments))
    }

    /// This path extended by `sub` — how the op layer combines an interned root's constant
    /// path prefix with a call's dynamic subpath. The two halves were depth-checked by their
    /// own parses, so the combined depth is re-checked here to keep the recursion guard
    /// honest.
    ///
    /// # Errors
    ///
    /// Returns a [`PathError`] when the combined path exceeds the segment depth limit.
    pub fn joined(&self, sub: Self) -> Result<Self, PathError> {
        if self.0.is_empty() {
            return Ok(sub);
        }
        if self.0.len() + sub.0.len() > MAX_PATH_SEGMENTS {
            return Err(PathError(format!(
                "path exceeds the {MAX_PATH_SEGMENTS}-segment depth limit in {self}.{sub}"
            )));
        }
        let mut segments = Vec::with_capacity(self.0.len() + sub.0.len());
        segments.extend(self.0.iter().cloned());
        segments.extend(sub.0);
        Ok(Self(segments))
    }
}

/// Hard cap on path depth. Every write recurses [`Node::set_at`] once per segment on the
/// session thread's fixed stack (and the probe/extract/overlay walks run per segment too), so
/// an unbounded path is a stack-overflow abort a script could trigger with one `set`. No real
/// store address is remotely this deep — GMCP's deepest (`Char.Vitals.hp`) is three — so the
/// limit is a runaway guard, not a design constraint; a path past it is an author bug,
/// rejected loudly.
const MAX_PATH_SEGMENTS: usize = 64;

/// Cap on generations parked in [`SessionStore::flush`]'s deferred-drop bin. The runtime
/// drains the bin every pump, so under it the bin holds at most one root per producer that
/// committed since the last drain — a handful. The cap only binds for callers that drive
/// [`SessionStore::flush`] directly with no drain point (unit tests, store-level benches):
/// past it, the flush drops evicted generations inline instead of parking them, trading the
/// deferred-drop win for bounded memory.
const MAX_PARKED_GENERATIONS: usize = 64;

/// Whether `b` may appear in a dot-form identifier segment (`[A-Za-z_$][A-Za-z0-9_$]*`).
fn is_ident_byte(b: u8, first: bool) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || (!first && b.is_ascii_digit())
}

impl std::fmt::Display for StorePath {
    /// Canonical spelling: dots for identifier segments, double-quoted brackets otherwise.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            let bytes = segment.as_bytes();
            let is_ident = !bytes.is_empty()
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(at, b)| is_ident_byte(*b, at == 0));
            if is_ident {
                if index > 0 {
                    write!(f, ".")?;
                }
                write!(f, "{segment}")?;
            } else {
                write!(f, "[\"{segment}\"]")?;
            }
        }
        Ok(())
    }
}

/// Whether `prefix` is an ancestor-or-equal of `path` under the uniform ASCII fold.
fn is_prefix(prefix: &[String], path: &[String]) -> bool {
    prefix.len() <= path.len()
        && prefix
            .iter()
            .zip(path.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Whether two paths are comparable (one is an ancestor-or-equal of the other) — the relation
/// under which a write is visible to a watcher: a write at or below the watched path changes
/// its content; a write above it replaces the subtree containing it.
fn paths_comparable(a: &[String], b: &[String]) -> bool {
    is_prefix(a, b) || is_prefix(b, a)
}

/// Per-producer subtree budgets, enforced at the write choke point. Generous by design: they
/// exist to bound a buggy package or a hostile server feed (the future `gmcp` subtree), not to
/// squeeze honest producers.
#[derive(Clone, Copy, Debug)]
pub struct StoreBudgets {
    pub max_entries: u64,
    pub max_bytes: u64,
}

impl Default for StoreBudgets {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// A write rejected by the budget gate. The message names the producer and the exceeded
/// dimension so the diagnostic teaches, and rejection (vs eviction) keeps state semantics
/// intact — a producer that overflows sees the failure at the write, not corrupted reads later.
#[derive(Debug, thiserror::Error)]
#[error(
    "session store budget exceeded for {producer}: the write would put its subtree at \
     {would_entries} entries / {would_bytes} bytes (limit {max_entries} entries / {max_bytes} bytes); \
     the write was rejected"
)]
pub struct BudgetExceeded {
    pub producer: String,
    pub would_entries: u64,
    pub would_bytes: u64,
    pub max_entries: u64,
    pub max_bytes: u64,
}

/// One journaled set-at-path, pending flush. The value is already the store's [`Node`] form
/// (converted once at the write op's ingestion), so overlay reads borrow it and the per-write
/// replay serializes it without re-walking a `serde_json::Value`.
struct JournalEntry {
    isolate: IsolateId,
    producer: ProducerKey,
    path: StorePath,
    value: Node,
    /// The writing turn's event-delivery depth (0 outside handler dispatch); watch deliveries
    /// triggered by this write run one level deeper, sharing the event system's cycle cap.
    depth: u32,
}

/// A watcher's delivery cadence (`docs/interop.md` §2). The two cadences differ in
/// callback shape, not just frequency, which is why the script surface gives each its own verb
/// (`watch` / `onWrite`) rather than an options flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchCadence {
    /// One delivery per flushed turn that wrote a comparable path, carrying the watched
    /// path's final state (`(snapshot)`); write-triggered, no value diffing.
    Coalesced,
    /// A replay of the flushed journal: one delivery per set-at-path in write order,
    /// value-identical writes included, carrying `(path, snapshot)` — the written path and
    /// the value that write published.
    PerWrite,
}

/// One registered watcher (either cadence).
struct Watcher {
    isolate: IsolateId,
    function_id: FunctionId,
    producer: ProducerKey,
    path: StorePath,
    cadence: WatchCadence,
}

/// One registered widget binding: the addressed path plus the shared cell the UI reads.
/// Bindings have no JS side — invalidation writes the cell and never dispatches a function.
struct HostBinding {
    producer: ProducerKey,
    path: StorePath,
    cell: Arc<StoreBindingCell>,
}

/// One node of a producer's binding path trie, keyed by ASCII-folded segment. `ids` are the
/// bindings addressing exactly this node's path. Invalidation for a write walks the write
/// path from the root: every node passed holds ancestor-or-equal bindings (a write below
/// them changes their subtree), and if the walk completes, the whole subtree under the final
/// node holds descendant bindings (the write replaced the subtree containing them).
#[derive(Default)]
struct BindingTrieNode {
    children: HashMap<String, BindingTrieNode>,
    ids: Vec<u32>,
}

impl BindingTrieNode {
    fn collect_subtree(&self, into: &mut HashSet<u32>) {
        into.extend(&self.ids);
        for child in self.children.values() {
            child.collect_subtree(into);
        }
    }
}

/// What a successful `set` reports back to the op layer.
#[derive(Debug)]
pub struct SetOutcome {
    /// The published value contained two case-fold-equal spellings of one key in a single
    /// object (last value won, first casing kept) — worth a one-time teaching diagnostic.
    pub first_duplicate_key_collapse: bool,
}

/// The boundary form of one leaf-aware read (`docs/interop-pre-gmcp-plan.md` §2): the node's
/// kind, with a serialized payload only when a leaf or an array crosses. Objects cross as the
/// bare kind — the reader walks deeper with further tagged reads instead of pulling the
/// subtree — which is what makes a leaf read O(answer) instead of O(published tree).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaggedSnapshot {
    /// An object: no payload. Enumerate with [`SessionStore::keys`]; read children with
    /// further tagged gets.
    Object,
    /// An array, materialized whole as compact JSON (the path grammar addresses collections
    /// whole — there is no index segment to read one element by).
    Array(String),
    /// A scalar as compact JSON — including `null`: a stored null is a value, distinct from
    /// absence (which is no [`TaggedSnapshot`] at all).
    Scalar(String),
}

/// The reader-visible node at a path — the committed tree overlaid with the reader's own
/// unflushed journal — *without* materializing it. Where [`SessionStore::get`] clones and
/// patches values, this resolves the same overlay to a borrow plus facts: a same-turn write
/// at or above the path replaces the whole view (a borrowed slice of the written value), and
/// writes strictly below force the node to be an object regardless of its base (set-at-path
/// conjures object spines through non-object intermediates).
enum NodeView<'s> {
    /// Nothing at the path.
    Absent,
    /// The node exactly as this borrowed value; no same-turn write below it.
    Node(&'s Node),
    /// Same-turn writes strictly below the path make the node an object.
    Patched {
        /// The at-or-above view the below-writes patch into: `None` reads as an empty
        /// object (the write conjured the spine).
        base: Option<&'s Node>,
        /// First path segment (relative to the queried path) of each strictly-below write,
        /// in write order — the keys those writes add (or re-address, when fold-equal to a
        /// base key).
        added: Vec<&'s str>,
    },
}

pub struct SessionStore {
    /// Committed producer subtrees. Survives engine rebuilds (reloads don't drop state).
    roots: HashMap<ProducerKey, Node>,
    /// Committed per-producer usage, kept in lockstep with `roots` at flush.
    usage: HashMap<ProducerKey, Usage>,
    /// Per-producer retained generation: the committed root each producer's last committing
    /// flush displaced (`docs/interop-pre-gmcp-plan.md` §5). Retention is the `Arc` move out
    /// of `roots`, so a generation costs only the structural delta the displacing writes
    /// forced; usage accounting never charges it (budgets bound the logical tree). Absent
    /// until a producer's *second* commit — the state before the first batch is absence.
    /// Committed data like `roots`, so it survives engine rebuilds.
    previous: HashMap<ProducerKey, Node>,
    /// Generations [`Self::flush`] evicted from `previous` (each committing producer's
    /// *old* previous root, displaced by the newly retained one) — dead data, unreadable
    /// through any view, parked so their deallocation happens at the runtime's drain point
    /// ([`Self::drop_retired_generations`]) instead of inside the flush: dropping a
    /// displaced generation mid-flush returns a whole delta's worth of blocks (every spine
    /// node a wide-object rebuild forced, cloned keys included) to the allocator on the
    /// dispatch critical path, where the measured cost is the deallocation churn between
    /// head rebuilds, not the retention itself. Parking is one move; the runtime drains the
    /// bin every pump. Callers that never drain (unit tests, store-level benches driving
    /// [`Self::flush`] directly) are bounded by [`MAX_PARKED_GENERATIONS`], past which the
    /// flush drops eagerly — the pre-parking behavior.
    retired: Vec<Node>,
    /// This turn's pending writes, in write order.
    journal: Vec<JournalEntry>,
    /// Projected usage (committed ⊕ journal) for producers with pending writes.
    pending_usage: HashMap<ProducerKey, Usage>,
    /// The transient turn head (committed ⊕ this turn's journal) per producer written this
    /// turn: seeded on the producer's first write by an O(1) shallow clone of the committed
    /// root, then each write applies into it with `Arc::make_mut` along its spine — flat
    /// fan-out writes stay linear per turn. The budget probe reads it (memoized per-node
    /// usage makes the replaced-subtree lookup O(spine)), and the flush **freezes** it: heads
    /// swap into `roots` wholesale, with no journal replay. Producer-scoped, so only the
    /// probe and the flush read it — visibility-filtered reads ([`Self::get`]/[`Self::view`])
    /// overlay the journal instead, because a head can't answer for a non-writing isolate.
    turn_heads: HashMap<ProducerKey, Node>,
    /// Engine-scoped watcher registry; the index is the watch token, `None` = unwatched.
    watchers: Vec<Option<Watcher>>,
    /// Engine-scoped widget bindings; the index is the binding id carried by script tokens.
    bindings: Vec<HostBinding>,
    /// Dedup: ASCII-folded `(producer, path)` → existing binding id, so re-running a widget
    /// build (every remount re-calls `bind`) reuses one cell instead of accreting them.
    binding_ids: HashMap<(ProducerKey, Vec<String>), u32>,
    /// Per-producer invalidation trie over the bound paths (see [`BindingTrieNode`]).
    binding_trie: HashMap<ProducerKey, BindingTrieNode>,
    /// The shared id → cell registry the widget build ops resolve tokens against (parked in
    /// every isolate's `OpState`). Kept in lockstep with `bindings`.
    shared_bindings: StoreBindings,
    /// Whether the last flush updated any binding cell — the runtime's cue to wake the UI.
    bindings_changed: bool,
    budgets: StoreBudgets,
    /// Producers already given the duplicate-key-collapse diagnostic this engine run.
    collapse_warned: HashSet<ProducerKey>,
    /// (producer, isolate) pairs already given the non-home write diagnostic this engine run.
    non_home_warned: HashSet<(ProducerKey, IsolateId)>,
}

impl SessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::with_budgets(StoreBudgets::default())
    }

    #[must_use]
    pub fn with_budgets(budgets: StoreBudgets) -> Self {
        Self {
            roots: HashMap::new(),
            usage: HashMap::new(),
            previous: HashMap::new(),
            retired: Vec::new(),
            journal: Vec::new(),
            pending_usage: HashMap::new(),
            turn_heads: HashMap::new(),
            watchers: Vec::new(),
            bindings: Vec::new(),
            binding_ids: HashMap::new(),
            binding_trie: HashMap::new(),
            shared_bindings: StoreBindings::new(),
            bindings_changed: false,
            budgets,
            collapse_warned: HashSet::new(),
            non_home_warned: HashSet::new(),
        }
    }

    /// Drop everything scoped to a script engine (watchers hold `FunctionId`s into the old
    /// isolates' registries; binding ids live in tokens held by the old engine's widgets; the
    /// journal and diagnostic dedup sets belong to the old run) while keeping the committed
    /// tree + usage + retained previous generations (committed data, like the tree) — the
    /// store outlives instances, not the session.
    pub fn reset_engine_state(&mut self) {
        self.journal.clear();
        self.watchers.clear();
        self.bindings.clear();
        self.binding_ids.clear();
        self.binding_trie.clear();
        self.shared_bindings.clear();
        self.bindings_changed = false;
        self.pending_usage.clear();
        self.turn_heads.clear();
        self.collapse_warned.clear();
        self.non_home_warned.clear();
    }

    /// The shared id → cell registry (the same handle for the session's whole life), seeded
    /// into every isolate's `OpState` so the leaf widget ops can resolve binding tokens.
    #[must_use]
    pub fn bindings(&self) -> StoreBindings {
        self.shared_bindings.clone()
    }

    /// Journal one set-at-path for `producer` (the caller has already passed the capability and
    /// home gates). Rejects the write — journaling nothing — when it would push the producer's
    /// subtree past its budgets.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when the write's projected usage (committed ⊕ this turn's
    /// journal) would breach the producer's entry or byte budget.
    pub fn set(
        &mut self,
        producer: ProducerKey,
        path: StorePath,
        value: Value,
        isolate: IsolateId,
        depth: u32,
    ) -> Result<SetOutcome, BudgetExceeded> {
        // Values arrive as `serde_json::Value` (the op boundary's form) and become [`Node`]s
        // here, once — the conversion is also the fold-duplicate-key collapse (first spelling
        // and position, last value).
        let (value, collapsed) = Node::from_value_reporting(value);

        // Budget gate — one projected-usage comparison per write, at the single choke point
        // every producer write passes.
        let incoming = value.usage();
        let (replaced, missing_segments, conjures_container) = self.probe(&producer, &path);
        let base = self
            .pending_usage
            .get(&producer)
            .or_else(|| self.usage.get(&producer))
            .copied()
            .unwrap_or_default();
        let would = base
            .saturating_sub(replaced)
            .saturating_add(incoming)
            .saturating_add(intermediates_usage(&path, missing_segments, conjures_container));
        if would.entries > self.budgets.max_entries || would.bytes > self.budgets.max_bytes {
            return Err(BudgetExceeded {
                producer: producer.to_string(),
                would_entries: would.entries,
                would_bytes: would.bytes,
                max_entries: self.budgets.max_entries,
                max_bytes: self.budgets.max_bytes,
            });
        }
        self.pending_usage.insert(producer.clone(), would);

        // Maintain the turn head so the NEXT write's probe reads committed ⊕ journal-so-far:
        // seeded by a shallow (O(1), structure-sharing) clone of the committed root on this
        // producer's first write of the turn, then each write applies along its spine with
        // `Arc::make_mut`. The journal keeps its own handle on the value (another shallow
        // clone) for `get`'s overlay and the per-write replay.
        if !self.turn_heads.contains_key(&producer) {
            let seed = self
                .roots
                .get(&producer)
                .cloned()
                .unwrap_or_else(Node::empty_object);
            self.turn_heads.insert(producer.clone(), seed);
        }
        if let Some(head) = self.turn_heads.get_mut(&producer) {
            head.set_at(path.segments(), value.clone());
        }

        let first_collapse = collapsed && self.collapse_warned.insert(producer.clone());
        self.journal.push(JournalEntry {
            isolate,
            producer,
            path,
            value,
            depth,
        });
        Ok(SetOutcome {
            first_duplicate_key_collapse: first_collapse,
        })
    }

    /// Snapshot of the value at `(producer, path)` as `reader` observes it: the committed tree
    /// overlaid with this turn's journal entries from the *same isolate* (read-your-writes).
    /// Another isolate's unflushed writes are invisible — they become visible at the flush that
    /// precedes any action dispatched to the reader (the cross-isolate happens-before).
    /// Materializes to the boundary's `Value` form (an O(subtree) copy); the op layer's
    /// JSON-text reads take [`Self::get_json`], which serializes the shared tree directly.
    #[must_use]
    pub fn get(&self, producer: &ProducerKey, path: &StorePath, reader: &IsolateId) -> Option<Value> {
        self.projected(producer, path, reader).map(|node| node.to_value())
    }

    /// [`Self::get`]'s snapshot serialized as compact JSON text — identical visibility, no
    /// intermediate `Value`: the answer serializes straight off the (structurally shared)
    /// [`Node`] tree.
    #[must_use]
    pub fn get_json(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<String> {
        self.projected(producer, path, reader).map(|node| node.to_json())
    }

    /// The node `reader` observes at `(producer, path)`: committed ⊕ the reader's own journal
    /// entries. Owned but shallow — clones here are `Arc` bumps sharing structure with the
    /// committed tree and the journal's values; nothing walks the answered subtree.
    fn projected(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<Node> {
        // Overlay accumulator: `None` = untouched by the journal (fall through to committed).
        let mut overlay: Option<Option<Node>> = None;
        for entry in &self.journal {
            if entry.producer != *producer || entry.isolate != *reader {
                continue;
            }
            if is_prefix(entry.path.segments(), path.segments()) {
                // The entry rewrote this path or an ancestor: extract our slice of its value.
                let relative = &path.segments()[entry.path.segments().len()..];
                overlay = Some(entry.value.extract(relative).cloned());
            } else if is_prefix(path.segments(), entry.path.segments()) {
                // The entry wrote below this path: patch it into the current projection.
                let base = match overlay.take() {
                    Some(value) => value,
                    None => self.committed(producer, path).cloned(),
                };
                let mut node = base.unwrap_or_else(Node::empty_object);
                let relative = &entry.path.segments()[path.segments().len()..];
                node.set_at(relative, entry.value.clone());
                overlay = Some(Some(node));
            }
        }
        match overlay {
            Some(value) => value,
            None => self.committed(producer, path).cloned(),
        }
    }

    /// Resolve `(producer, path)` to the [`NodeView`] `reader` observes — [`Self::get`]'s
    /// exact visibility (same journal filter, same two prefix branches, same write order),
    /// derived without cloning: the last at-or-above write is the base borrow and clears any
    /// earlier below-writes (its value *replaces* the subtree, keys and all); the below-writes
    /// that survive contribute forced keys. Deliberately not derived from `turn_heads`,
    /// which is producer-scoped — reading it would leak another isolate's unflushed writes.
    fn view(&self, producer: &ProducerKey, path: &StorePath, reader: &IsolateId) -> NodeView<'_> {
        // `None` = untouched by the journal (fall through to committed); `Some(base)` = the
        // extracted slice of the last at-or-above write (which may itself be absent).
        let mut base: Option<Option<&Node>> = None;
        let mut added: Vec<&str> = Vec::new();
        for entry in &self.journal {
            if entry.producer != *producer || entry.isolate != *reader {
                continue;
            }
            if is_prefix(entry.path.segments(), path.segments()) {
                let relative = &path.segments()[entry.path.segments().len()..];
                base = Some(entry.value.extract(relative));
                added.clear();
            } else if is_prefix(path.segments(), entry.path.segments()) {
                added.push(entry.path.segments()[path.segments().len()].as_str());
            }
        }
        let base = base.unwrap_or_else(|| self.committed(producer, path));
        if added.is_empty() {
            match base {
                Some(node) => NodeView::Node(node),
                None => NodeView::Absent,
            }
        } else {
            NodeView::Patched { base, added }
        }
    }

    /// The kind (and leaf/array payload) at `(producer, path)` as `reader` observes it —
    /// [`Self::get`]'s exact read-your-writes visibility, serializing only what crosses the
    /// boundary. `None` = absent, distinct from a stored `null` (a scalar payload).
    #[must_use]
    pub fn get_tagged(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<TaggedSnapshot> {
        match self.view(producer, path, reader) {
            NodeView::Absent => None,
            // A same-turn write below the path forces an object whatever the base held.
            NodeView::Patched { .. } => Some(TaggedSnapshot::Object),
            NodeView::Node(node) => Some(classify_node(node)),
        }
    }

    /// Own keys of the object at `(producer, path)` as `reader` observes it — first-published
    /// casing, publish order — or `None` when the node is absent or not an object. Exactly the
    /// keys of [`Self::get`]'s value there: a same-turn write at or above the path *replaces*
    /// the key set (its value carries the keys); only writes strictly below add keys, deduped
    /// by fold against the base (set-at-path keeps a fold-matched key's stored casing).
    /// Borrowed from the store (committed tree or journal) so the caller serializes without a
    /// per-key clone — enumeration is a hot proxy trap.
    #[must_use]
    pub fn keys(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<Vec<&str>> {
        match self.view(producer, path, reader) {
            NodeView::Node(Node::Object(object)) => Some(object.keys().collect()),
            // Scalars and arrays have no keys; absence has no reading at all.
            NodeView::Absent | NodeView::Node(_) => None,
            NodeView::Patched { base, added } => {
                // A non-object base is replaced by the patch's object spine, so only an
                // object base contributes keys.
                let mut keys: Vec<&str> = match base {
                    Some(Node::Object(object)) => object.keys().collect(),
                    _ => Vec::new(),
                };
                for segment in added {
                    if !keys.iter().any(|key| key.eq_ignore_ascii_case(segment)) {
                        keys.push(segment);
                    }
                }
                Some(keys)
            }
        }
    }

    /// The previous-generation anchor for `producer` as `reader` observes it
    /// (`docs/interop-pre-gmcp-plan.md` §5): the state before the newest write batch the
    /// reader can see. Seat-aware, mirroring [`Self::get`]'s read-your-writes visibility: a
    /// reader whose own writes for this producer sit in the journal is mid-batch, and its
    /// batch's base is the committed root; every other reader cannot observe an open journal
    /// at all (timers across isolates can share one pump, so such a reader *can* run while
    /// another isolate's journal is open — the journal is merely invisible to it), so its
    /// newest write batch is the last committing flush and its anchor is the generation that
    /// flush retained. Journal state elsewhere never moves a reader's anchor. `None` before
    /// the first commit (and, for a reader outside an open batch, until the second — the
    /// state before the first batch is absence).
    ///
    /// The mid-batch test scans the journal with exactly [`Self::get`]/[`Self::view`]'s
    /// `(producer, isolate)` filter — `turn_heads` is producer-scoped and cannot say *whose*
    /// batch is open.
    ///
    /// The whole `previous_*` family is `#[cold]`/`#[inline(never)]`: `previousValue` is the
    /// diff surface, not the hot path, and outlining it keeps the previous machinery (this
    /// journal scan included) out of the head read ops' codegen — the head/previous split is
    /// per *op* on the JS side, so a head read never touches these bodies. A `previousValue`
    /// read pays one outlined call.
    #[cold]
    #[inline(never)]
    fn previous_anchor(&self, producer: &ProducerKey, reader: &IsolateId) -> Option<&Node> {
        let mid_batch = self
            .journal
            .iter()
            .any(|entry| entry.producer == *producer && entry.isolate == *reader);
        if mid_batch {
            self.roots.get(producer)
        } else {
            self.previous.get(producer)
        }
    }

    /// The node at `path` under the previous generation `reader` observes (see
    /// [`Self::previous_anchor`]).
    #[cold]
    #[inline(never)]
    fn previous_at(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<&Node> {
        self.previous_anchor(producer, reader)?.extract(path.segments())
    }

    /// [`Self::get_tagged`]'s boundary form over the previous generation `reader` observes:
    /// the kind at `(producer, path)` as of the state before the reader's newest write batch
    /// ([`Self::previous_anchor`]), payload only for leaves and arrays. `None` = absent
    /// (including before the producer's first commit), distinct from a stored `null`.
    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn previous_get_tagged(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<TaggedSnapshot> {
        self.previous_at(producer, path, reader).map(classify_node)
    }

    /// [`Self::keys`] over the previous generation `reader` observes: own keys of the object
    /// at `(producer, path)` (first-published casing, publish order), or `None` when the node
    /// is absent or not an object.
    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn previous_keys(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<Vec<&str>> {
        match self.previous_at(producer, path, reader) {
            Some(Node::Object(object)) => Some(object.keys().collect()),
            _ => None,
        }
    }

    /// [`Self::get_json`] over the previous generation `reader` observes: the subtree at
    /// `(producer, path)` serialized as compact JSON straight off the anchored (structurally
    /// shared) tree.
    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn previous_get_json(
        &self,
        producer: &ProducerKey,
        path: &StorePath,
        reader: &IsolateId,
    ) -> Option<String> {
        self.previous_at(producer, path, reader).map(Node::to_json)
    }

    /// Register a watcher on `(producer, path)`; the returned token cancels it. The handler is
    /// a `FunctionId` in `isolate`'s function registry, invoked per [`WatchCadence`]: coalesced
    /// with a `{ snapshot }` capture per flush that touched a comparable path, per-write with a
    /// `{ path, snapshot }` capture per journaled set-at-path.
    pub fn watch(
        &mut self,
        producer: ProducerKey,
        path: StorePath,
        isolate: IsolateId,
        function_id: FunctionId,
        cadence: WatchCadence,
    ) -> u32 {
        self.watchers.push(Some(Watcher {
            isolate,
            function_id,
            producer,
            path,
            cadence,
        }));
        u32::try_from(self.watchers.len() - 1).unwrap_or(u32::MAX)
    }

    /// Register (or reuse) a widget binding on `(producer, path)` and return its token id.
    /// Bindings are reads with no home gate; the cell is seeded with the committed snapshot
    /// (`Null` when absent) and updated at every flush that writes a comparable path. One id
    /// per folded path: a remount re-calling `bind` gets the same cell, so cells are bounded
    /// by the number of distinct bound paths per engine run.
    pub fn bind(&mut self, producer: ProducerKey, path: StorePath) -> u32 {
        let folded: Vec<String> = path
            .segments()
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        let key = (producer.clone(), folded.clone());
        if let Some(id) = self.binding_ids.get(&key) {
            return *id;
        }
        let id = u32::try_from(self.bindings.len()).unwrap_or(u32::MAX);
        let seed = self
            .committed(&producer, &path)
            .cloned()
            .unwrap_or(Node::Null);
        let cell = Arc::new(StoreBindingCell::new(seed));
        let mut node = self.binding_trie.entry(producer.clone()).or_default();
        for segment in folded {
            node = node.children.entry(segment).or_default();
        }
        node.ids.push(id);
        self.shared_bindings.insert(id, cell.clone());
        self.bindings.push(HostBinding {
            producer,
            path,
            cell,
        });
        self.binding_ids.insert(key, id);
        id
    }

    /// Whether the last [`Self::flush`] updated any binding cell, consumed by the runtime
    /// (which wakes the UI so render closures re-read the cells). Reading resets the flag.
    pub fn take_bindings_changed(&mut self) -> bool {
        std::mem::take(&mut self.bindings_changed)
    }

    /// Cancel a watch by its token. Scoped to the registering isolate (like event `off`):
    /// another isolate's token names its own registrations, so it can never cancel this one.
    /// Idempotent; an unknown token is a no-op.
    pub fn unwatch(&mut self, token: u32, isolate: &IsolateId) {
        // `usize: From<u32>` doesn't exist (usize may be 16-bit); a token that doesn't fit
        // can't name any watcher, so bail to the no-op.
        let Ok(index) = usize::try_from(token) else {
            return;
        };
        if let Some(slot) = self.watchers.get_mut(index)
            && slot.as_ref().is_some_and(|w| w.isolate == *isolate)
        {
            *slot = None;
        }
    }

    /// Whether any writes are journaled (lets the runtime skip flush bookkeeping when idle).
    #[must_use]
    pub fn has_pending_writes(&self) -> bool {
        !self.journal.is_empty()
    }

    /// Deallocate the generations [`Self::flush`] parked (see `retired`): called by the
    /// runtime at its drain point, after the turn's deliveries are queued, so the
    /// deallocation of a displaced generation's structural delta happens off the dispatch
    /// critical path. Clearing in place keeps the bin's capacity — a steady flush cadence
    /// retires generations with no per-turn allocation.
    pub fn drop_retired_generations(&mut self) {
        self.retired.clear();
    }

    /// Commit the journal to the tree and produce one coalesced delivery per watcher whose
    /// path is comparable to any written path — the final-state-this-turn snapshot, queued by
    /// the caller at the back of the action queue (delivery on the next pump, like events).
    pub fn flush(&mut self) -> Vec<RuntimeAction> {
        if self.journal.is_empty() {
            return Vec::new();
        }
        let journal = std::mem::take(&mut self.journal);
        // Freeze the turn heads: each head already carries the journal's net effect (writes
        // applied as they arrived), so committing a producer is one map move — no replay.
        // The displaced root becomes the producer's retained previous generation (an `Arc`
        // move; structural sharing bounds what it pins). A first commit displaces nothing,
        // which is exactly `previousValue`'s absent-before-first-commit semantics. The
        // commit swaps in place so the drained key moves into whichever map needs an owned
        // one — a `ProducerKey` clone is two `String` allocations, per committing producer,
        // per flush (the per-line path once GMCP lands). The generation the retention
        // insert evicts (the *old* previous root, now unreadable) is parked rather than
        // dropped here: its deallocation — a whole structural delta's worth of blocks —
        // belongs at the runtime's drain point, off the dispatch critical path (see
        // `retired`).
        for (producer, head) in self.turn_heads.drain() {
            if let Some(root) = self.roots.get_mut(&producer) {
                let displaced = std::mem::replace(root, head);
                if let Some(evicted) = self.previous.insert(producer, displaced)
                    && self.retired.len() < MAX_PARKED_GENERATIONS
                {
                    self.retired.push(evicted);
                }
            } else {
                self.roots.insert(producer, head);
            }
        }
        for (producer, usage) in self.pending_usage.drain() {
            self.usage.insert(producer, usage);
        }

        self.invalidate_bindings(&journal);

        let mut deliveries = Vec::new();
        // Per-write watchers replay the journal (`docs/interop.md` §2, D8): one
        // delivery per set-at-path in write order, value-identical writes included, carrying
        // the WRITTEN path (canonical spelling) and the value that write published. Queued
        // ahead of the coalesced deliveries below so a consumer holding both cadences sees
        // the occurrence stream before the turn's final-state summary. Each delivery inherits
        // its own write's depth (a runaway write→onWrite→write cycle terminates at the cap
        // without suppressing innocent same-flush writes).
        for entry in &journal {
            for watcher in self.watchers.iter().flatten() {
                if watcher.cadence != WatchCadence::PerWrite
                    || watcher.producer != entry.producer
                    || !paths_comparable(entry.path.segments(), watcher.path.segments())
                {
                    continue;
                }
                if entry.depth >= MAX_EVENT_DEPTH {
                    log::warn!(
                        "session store: per-write watch recursion limit reached at {}::{} — dropping the delivery",
                        watcher.producer,
                        watcher.path
                    );
                    continue;
                }
                deliveries.push(RuntimeAction::CallJavascriptFunction {
                    isolate: watcher.isolate.clone(),
                    id: watcher.function_id,
                    matches: Arc::new(vec![
                        MatchCapture {
                            name: Some(std::borrow::Cow::Borrowed("path")),
                            value: entry.path.to_string(),
                        },
                        MatchCapture {
                            name: Some(std::borrow::Cow::Borrowed("snapshot")),
                            value: entry.value.to_json(),
                        },
                    ]),
                    depth: entry.depth + 1,
                    is_captured: None,
                });
            }
        }
        for watcher in self.watchers.iter().flatten() {
            if watcher.cadence != WatchCadence::Coalesced {
                continue;
            }
            // Deliver at the *shallowest* contributing write's depth: a genuine watch→write→watch
            // runaway ratchets all of its own writes up together, so the min still climbs to the
            // cap and terminates it — but an innocent depth-0 write coalesced into the same flush
            // as a deep write is no longer suppressed along with it (max-fold dropped the whole
            // delivery, silently losing the shallow write's committed state).
            let mut min_depth = None;
            for entry in &journal {
                if entry.producer == watcher.producer
                    && paths_comparable(entry.path.segments(), watcher.path.segments())
                {
                    min_depth = Some(min_depth.map_or(entry.depth, |d: u32| d.min(entry.depth)));
                }
            }
            let Some(depth) = min_depth else {
                continue;
            };
            if depth >= MAX_EVENT_DEPTH {
                log::warn!(
                    "session store: watch recursion limit reached at {}::{} — dropping the delivery",
                    watcher.producer,
                    watcher.path
                );
                continue;
            }
            let snapshot = self
                .committed(&watcher.producer, &watcher.path)
                .map_or_else(|| "null".to_string(), Node::to_json);
            deliveries.push(RuntimeAction::CallJavascriptFunction {
                isolate: watcher.isolate.clone(),
                id: watcher.function_id,
                matches: Arc::new(vec![MatchCapture {
                    name: Some(std::borrow::Cow::Borrowed("snapshot")),
                    value: snapshot,
                }]),
                depth: depth + 1,
                is_captured: None,
            });
        }
        deliveries
    }

    /// Binding invalidation at flush: walk each written path through its producer's trie
    /// collecting ancestor-or-equal bindings en route and the whole bound subtree at the end,
    /// then write every dirty cell's committed snapshot. Write-triggered like `watch` (no
    /// value diffing), depth-free (no JS runs), latest-wins (later flushes overwrite the
    /// cell).
    fn invalidate_bindings(&mut self, journal: &[JournalEntry]) {
        let mut dirty: HashSet<u32> = HashSet::new();
        for entry in journal {
            let Some(mut node) = self.binding_trie.get(&entry.producer) else {
                continue;
            };
            dirty.extend(&node.ids);
            let mut walked_all = true;
            for segment in entry.path.segments() {
                let Some(child) = node.children.get(&segment.to_ascii_lowercase()) else {
                    walked_all = false;
                    break;
                };
                node = child;
                dirty.extend(&node.ids);
            }
            if walked_all {
                node.collect_subtree(&mut dirty);
            }
        }
        for id in &dirty {
            let Ok(index) = usize::try_from(*id) else {
                continue;
            };
            if let Some(binding) = self.bindings.get(index) {
                // A shallow clone: the cell pins the committed subtree by `Arc`, sharing its
                // structure — writes since only ever diverge the spines they touch.
                let snapshot = self
                    .committed(&binding.producer, &binding.path)
                    .cloned()
                    .unwrap_or(Node::Null);
                binding.cell.set(snapshot);
            }
        }
        self.bindings_changed |= !dirty.is_empty();
    }

    /// One-time gate for the non-home write diagnostic: `true` exactly once per
    /// `(producer, isolate)` per engine run.
    pub fn note_non_home_write(&mut self, producer: ProducerKey, isolate: IsolateId) -> bool {
        self.non_home_warned.insert((producer, isolate))
    }

    /// Every producer's committed subtree + usage for the runtime catalogue's snapshot
    /// (`docs/interop.md` §10: retained state *is* its own sample). Served as the tree's
    /// own `Node` root — an O(1) share of the committed tree (`Arc`-interior), never a deep
    /// copy — so the inspector walks the store's structure lazily and can `Arc::ptr_eq`
    /// nodes across snapshot generations (`docs/interop-pre-gmcp-plan.md` §6).
    /// Committed only — another isolate's unflushed journal is nobody's business, and the
    /// snapshot is built at the drain point, after the turn's flush.
    #[must_use]
    pub fn snapshot_producers(&self) -> Vec<(ProducerKey, Node, Usage)> {
        let mut producers: Vec<(ProducerKey, Node, Usage)> = self
            .roots
            .iter()
            .map(|(producer, tree)| {
                (
                    producer.clone(),
                    tree.clone(),
                    self.usage.get(producer).copied().unwrap_or_default(),
                )
            })
            .collect();
        producers.sort_by_key(|(producer, ..)| producer.to_string());
        producers
    }

    /// The committed (journal-free) value at `(producer, path)`.
    fn committed(&self, producer: &ProducerKey, path: &StorePath) -> Option<&Node> {
        self.roots.get(producer)?.extract(path.segments())
    }

    /// Budget probe against the journal-projected tree: the usage of the subtree a write at
    /// `path` would replace, how many trailing path segments don't exist yet (each becomes a
    /// created intermediate), and whether the write must additionally conjure the container
    /// holding the first missing key (see [`probe_node`]). Producer-scoped (not
    /// isolate-scoped): the home gate means all of a producer's journal entries come from one
    /// isolate, so the projection is unambiguous.
    ///
    /// Reads the producer's [`Self::turn_heads`] entry when it has already written this turn
    /// (committed ⊕ journal-so-far), else the committed root directly. Each node memoizes its
    /// subtree usage, so the replaced-subtree charge is an O(spine) walk ending in an O(1)
    /// read — never a re-measure of the replaced subtree. A producer with no root yet conjures
    /// its root object on first write, the same extra-container charge as a non-object tunnel.
    fn probe(&self, producer: &ProducerKey, path: &StorePath) -> (Usage, usize, bool) {
        match self
            .turn_heads
            .get(producer)
            .or_else(|| self.roots.get(producer))
        {
            Some(root) => probe_node(root, path.segments()),
            None => (Usage::default(), path.segments().len(), true),
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// A resolved node's boundary form (the shared classification behind [`SessionStore::get_tagged`]
/// and [`SessionStore::previous_get_tagged`]): objects cross as the bare kind, arrays and
/// scalars carry their compact JSON.
fn classify_node(node: &Node) -> TaggedSnapshot {
    match node {
        Node::Object(_) => TaggedSnapshot::Object,
        Node::Array(_) => TaggedSnapshot::Array(node.to_json()),
        _ => TaggedSnapshot::Scalar(node.to_json()),
    }
}

/// Walk `path` under `node`, returning the replaced subtree's usage, the count of missing
/// trailing segments (each becomes a created intermediate), and whether the write conjures
/// the *container* holding the first missing key. The walk can stop short two ways, and they
/// charge differently: an *object* missing the next key replaces nothing and already holds
/// the new key's slot (the write just adds an entry); a *non-object* stops the walk because
/// set-at-path will overwrite it — and its whole subtree — with a fresh object spine, so its
/// usage is reclaimed (not crediting it once left the destroyed subtree charged forever,
/// eventually bricking the producer against its own budget) **and** the replacement object is
/// one more created node than the missing-segment spine alone accounts for.
fn probe_node(node: &Node, path: &[String]) -> (Usage, usize, bool) {
    let mut node = node;
    for (index, segment) in path.iter().enumerate() {
        let Some(child) = node.get(segment) else {
            let is_object = node.is_object();
            let replaced = if is_object {
                Usage::default()
            } else {
                node.usage()
            };
            return (replaced, path.len() - index, !is_object);
        };
        node = child;
    }
    (node.usage(), 0, false)
}

/// Usage charged for what a write conjures beyond its value (`missing` trailing segments of
/// its path): each missing segment adds its key text, the per-key overhead, and one length
/// byte; each but the innermost also adds one object node; and when the write tunnels through
/// a non-object (or seeds a brand-new producer root), `conjures_container` charges the one
/// extra object that replaces it. Charged with the same constants — and the same per-entry
/// length byte — the [`Node`] tree memoizes, so the usage frozen at flush lands exactly on
/// the committed tree's own [`Node::usage`] (cross-checked by
/// `committed_usage_matches_the_tree_after_flush`); an undercharge here compounds per write
/// and under-enforces the budget in exactly the hostile-feed direction it exists for.
fn intermediates_usage(path: &StorePath, missing: usize, conjures_container: bool) -> Usage {
    let mut usage = Usage::default();
    if missing == 0 {
        return usage;
    }
    if conjures_container {
        usage.entries += 1;
        usage.bytes += CONTAINER_OVERHEAD_BYTES;
    }
    let segments = path.segments();
    for (index, segment) in segments.iter().enumerate().skip(segments.len() - missing) {
        usage.bytes += segment.len() as u64 + KEY_OVERHEAD_BYTES + 1;
        if index < segments.len() - 1 {
            usage.entries += 1;
            usage.bytes += CONTAINER_OVERHEAD_BYTES;
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn main_isolate() -> IsolateId {
        IsolateId::Main
    }

    fn pkg_isolate(owner: &str, name: &str) -> IsolateId {
        IsolateId::Package {
            owner: Arc::from(owner),
            name: Arc::from(name),
            version: Arc::from("1.0.0"),
        }
    }

    fn user_set(store: &mut SessionStore, path: &str, value: Value) {
        store
            .set(
                ProducerKey::User,
                StorePath::parse(path).unwrap(),
                value,
                main_isolate(),
                0,
            )
            .expect("set within budget");
    }

    fn user_get(store: &SessionStore, path: &str) -> Option<Value> {
        store.get(
            &ProducerKey::User,
            &StorePath::parse(path).unwrap(),
            &main_isolate(),
        )
    }

    // ---- path grammar --------------------------------------------------------------------

    #[test]
    fn path_grammar_parses_dots_and_quoted_brackets() {
        let path = StorePath::parse(r#"groupies["Mr. Foo"].hp"#).unwrap();
        assert_eq!(path.segments(), ["groupies", "Mr. Foo", "hp"]);
        let bracket_first = StorePath::parse(r#"["odd key"].x"#).unwrap();
        assert_eq!(bracket_first.segments(), ["odd key", "x"]);
        let single_quotes = StorePath::parse("a['b c']").unwrap();
        assert_eq!(single_quotes.segments(), ["a", "b c"]);
        assert_eq!(StorePath::parse("").unwrap(), StorePath::root());
        assert_eq!(StorePath::parse("  ").unwrap(), StorePath::root());
    }

    #[test]
    fn path_grammar_rejects_malformed_paths() {
        for bad in [".a", "a..b", "a.", "a[b]", "a[\"b\"", "a[\"\"]", "a[\"b\"x", "a b", "1a"] {
            assert!(StorePath::parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn path_display_round_trips_canonical_spelling() {
        let path = StorePath::parse(r"groupies['Mr. Foo'].hp").unwrap();
        assert_eq!(path.to_string(), r#"groupies["Mr. Foo"].hp"#);
    }

    #[test]
    fn joined_extends_a_root_prefix_and_rechecks_depth() {
        let root = StorePath::parse("vitals").unwrap();
        let joined = root.joined(StorePath::parse("stats.hp").unwrap()).unwrap();
        assert_eq!(joined.segments(), ["vitals", "stats", "hp"]);
        // An empty subpath addresses the root itself; an empty root passes the sub through.
        assert_eq!(root.joined(StorePath::root()).unwrap().segments(), ["vitals"]);
        assert_eq!(
            StorePath::root().joined(StorePath::parse("a.b").unwrap()).unwrap().segments(),
            ["a", "b"]
        );
        // The two halves parse under the cap independently; the combination is re-checked.
        let deep = StorePath::parse(&vec!["s"; 63].join(".")).unwrap();
        assert!(root.joined(deep.clone()).is_ok());
        let over = StorePath::parse("a.b").unwrap();
        assert!(over.joined(deep).is_err(), "a combined path past the cap is rejected");
    }

    // ---- fold + casing -------------------------------------------------------------------

    #[test]
    fn lookups_fold_case_and_enumeration_keeps_first_published_casing() {
        let mut store = SessionStore::new();
        user_set(&mut store, "Char.Vitals", json!({ "hp": 10 }));
        store.flush();
        // Dots and brackets fold identically.
        assert_eq!(user_get(&store, "char.vitals.HP"), Some(json!(10)));
        assert_eq!(user_get(&store, r#"CHAR["VITALS"].hp"#), Some(json!(10)));
        // A later write through a differently-cased path keeps the first-published casing.
        user_set(&mut store, "CHAR.VITALS.hp", json!(11));
        store.flush();
        let root = user_get(&store, "").unwrap();
        assert_eq!(root.to_string(), r#"{"Char":{"Vitals":{"hp":11}}}"#);
    }

    #[test]
    fn duplicate_folded_keys_in_one_object_are_last_wins_first_casing() {
        let mut store = SessionStore::new();
        let value: Value =
            serde_json::from_str(r#"{ "Foo": 1, "foo": 2, "bar": 3 }"#).expect("parse");
        let outcome = store
            .set(
                ProducerKey::User,
                StorePath::root(),
                value,
                main_isolate(),
                0,
            )
            .unwrap();
        assert!(outcome.first_duplicate_key_collapse, "the collapse is reported once");
        store.flush();
        let root = user_get(&store, "").unwrap();
        assert_eq!(root.to_string(), r#"{"Foo":2,"bar":3}"#);
        // The second collapse for the same producer is not re-reported.
        let value: Value = serde_json::from_str(r#"{ "A": 1, "a": 2 }"#).expect("parse");
        let outcome = store
            .set(ProducerKey::User, StorePath::root(), value, main_isolate(), 0)
            .unwrap();
        assert!(!outcome.first_duplicate_key_collapse);
    }

    #[test]
    fn object_entry_order_is_preserved_as_published() {
        let mut store = SessionStore::new();
        let value: Value = serde_json::from_str(r#"{ "z": 1, "a": 2, "m": 3 }"#).expect("parse");
        user_set(&mut store, "ordered", value);
        store.flush();
        assert_eq!(
            user_get(&store, "ordered").unwrap().to_string(),
            r#"{"z":1,"a":2,"m":3}"#
        );
    }

    // ---- journal: read-your-writes -------------------------------------------------------

    #[test]
    fn same_isolate_reads_observe_the_journal() {
        let mut store = SessionStore::new();
        user_set(&mut store, "a.b", json!(1));
        // Not flushed: the writer sees it, another isolate does not.
        assert_eq!(user_get(&store, "a.b"), Some(json!(1)));
        let other = pkg_isolate("wbk", "other");
        assert_eq!(
            store.get(
                &ProducerKey::User,
                &StorePath::parse("a.b").unwrap(),
                &other
            ),
            None
        );
        store.flush();
        // After the flush everyone sees it.
        assert_eq!(
            store.get(
                &ProducerKey::User,
                &StorePath::parse("a.b").unwrap(),
                &other
            ),
            Some(json!(1))
        );
    }

    #[test]
    fn journal_overlay_applies_writes_in_order() {
        let mut store = SessionStore::new();
        user_set(&mut store, "s", json!({ "hp": 1, "mana": 2 }));
        user_set(&mut store, "s.hp", json!(3));
        // Net effect within the turn: the descendant write patches the earlier subtree write.
        assert_eq!(user_get(&store, "s"), Some(json!({ "hp": 3, "mana": 2 })));
        // A later ancestor write shadows both.
        user_set(&mut store, "s", json!({ "hp": 9 }));
        assert_eq!(user_get(&store, "s"), Some(json!({ "hp": 9 })));
        assert_eq!(user_get(&store, "s.mana"), None);
        store.flush();
        assert_eq!(user_get(&store, "s"), Some(json!({ "hp": 9 })));
    }

    #[test]
    fn journal_overlay_patches_descendant_writes_over_committed_state() {
        let mut store = SessionStore::new();
        user_set(&mut store, "s", json!({ "hp": 1, "mana": 2 }));
        store.flush();
        user_set(&mut store, "s.hp", json!(5));
        assert_eq!(user_get(&store, "s"), Some(json!({ "hp": 5, "mana": 2 })));
        assert_eq!(user_get(&store, "s.mana"), Some(json!(2)));
    }

    #[test]
    fn set_at_path_creates_intermediates_and_replaces_non_objects() {
        let mut store = SessionStore::new();
        user_set(&mut store, "a.b.c", json!(1));
        store.flush();
        assert_eq!(user_get(&store, "a"), Some(json!({ "b": { "c": 1 } })));
        // Writing through a scalar replaces it with an object.
        user_set(&mut store, "a.b", json!(7));
        user_set(&mut store, "a.b.d", json!(8));
        store.flush();
        assert_eq!(user_get(&store, "a.b"), Some(json!({ "d": 8 })));
    }

    // ---- read path: tagged classification + keys under the overlay ------------------------

    fn user_tagged(store: &SessionStore, path: &str) -> Option<TaggedSnapshot> {
        store.get_tagged(
            &ProducerKey::User,
            &StorePath::parse(path).unwrap(),
            &main_isolate(),
        )
    }

    fn user_keys<'s>(store: &'s SessionStore, path: &str) -> Option<Vec<&'s str>> {
        store.keys(
            &ProducerKey::User,
            &StorePath::parse(path).unwrap(),
            &main_isolate(),
        )
    }

    #[test]
    fn tagged_get_classifies_committed_kinds_and_distinguishes_absent_from_null() {
        let mut store = SessionStore::new();
        user_set(
            &mut store,
            "t",
            json!({ "n": 7, "s": "x", "nil": null, "arr": [1, 2], "obj": { "k": 1 } }),
        );
        store.flush();
        assert_eq!(user_tagged(&store, "t"), Some(TaggedSnapshot::Object));
        assert_eq!(user_tagged(&store, "t.obj"), Some(TaggedSnapshot::Object));
        assert_eq!(
            user_tagged(&store, "t.n"),
            Some(TaggedSnapshot::Scalar("7".into()))
        );
        assert_eq!(
            user_tagged(&store, "t.s"),
            Some(TaggedSnapshot::Scalar("\"x\"".into()))
        );
        assert_eq!(
            user_tagged(&store, "t.arr"),
            Some(TaggedSnapshot::Array("[1,2]".into()))
        );
        // A stored null is a scalar payload; absence is no reading at all.
        assert_eq!(
            user_tagged(&store, "t.nil"),
            Some(TaggedSnapshot::Scalar("null".into()))
        );
        assert_eq!(user_tagged(&store, "t.nope"), None);
        // Lookups fold case like every read.
        assert_eq!(
            user_tagged(&store, "T.N"),
            Some(TaggedSnapshot::Scalar("7".into()))
        );
    }

    #[test]
    fn tagged_get_honors_the_journal_overlay_kind_changes() {
        let mut store = SessionStore::new();
        user_set(&mut store, "t", json!({ "a": { "x": 1 }, "s": 5 }));
        store.flush();
        // A same-turn write AT a path replaces its kind: object -> scalar.
        user_set(&mut store, "t.a", json!(9));
        assert_eq!(
            user_tagged(&store, "t.a"),
            Some(TaggedSnapshot::Scalar("9".into()))
        );
        // A same-turn write BELOW a scalar forces the object spine at every level.
        user_set(&mut store, "t.s.sub.leaf", json!(1));
        assert_eq!(user_tagged(&store, "t.s"), Some(TaggedSnapshot::Object));
        assert_eq!(user_tagged(&store, "t.s.sub"), Some(TaggedSnapshot::Object));
        assert_eq!(
            user_tagged(&store, "t.s.sub.leaf"),
            Some(TaggedSnapshot::Scalar("1".into()))
        );
        // An ancestor write replaces the subtree: paths it doesn't carry read absent.
        user_set(&mut store, "t", json!({ "fresh": true }));
        assert_eq!(user_tagged(&store, "t.a"), None);
        assert_eq!(
            user_tagged(&store, "t.fresh"),
            Some(TaggedSnapshot::Scalar("true".into()))
        );
    }

    #[test]
    fn tagged_get_ignores_other_isolates_unflushed_writes() {
        let mut store = SessionStore::new();
        user_set(&mut store, "t", json!({ "hp": 1 }));
        store.flush();
        // Main's unflushed write is invisible to a package-isolate reader...
        user_set(&mut store, "t.hp", json!({ "deep": 2 }));
        let other = pkg_isolate("wbk", "other");
        assert_eq!(
            store.get_tagged(&ProducerKey::User, &StorePath::parse("t.hp").unwrap(), &other),
            Some(TaggedSnapshot::Scalar("1".into()))
        );
        assert_eq!(
            store.keys(&ProducerKey::User, &StorePath::parse("t").unwrap(), &other),
            Some(vec!["hp"])
        );
        // ...while the writer reads its own journal.
        assert_eq!(user_tagged(&store, "t.hp"), Some(TaggedSnapshot::Object));
    }

    #[test]
    fn keys_read_committed_objects_in_publish_order_with_first_casing() {
        let mut store = SessionStore::new();
        let value: Value = serde_json::from_str(r#"{ "z": 1, "A": 2, "m": 3 }"#).expect("parse");
        user_set(&mut store, "t", value);
        store.flush();
        assert_eq!(
            user_keys(&store, "t"),
            Some(vec!["z", "A", "m"])
        );
        // Non-objects and absent paths have no keys (arrays are addressed whole).
        user_set(&mut store, "arr", json!([1, 2]));
        user_set(&mut store, "n", json!(4));
        store.flush();
        assert_eq!(user_keys(&store, "arr"), None);
        assert_eq!(user_keys(&store, "n"), None);
        assert_eq!(user_keys(&store, "nope"), None);
    }

    #[test]
    fn keys_merge_journal_writes_and_respect_replacement() {
        let mut store = SessionStore::new();
        user_set(&mut store, "t", json!({ "a": 1, "B": 2 }));
        store.flush();
        // A strictly-below write adds its first segment as a key, in write order.
        user_set(&mut store, "t.c.deep", json!(1));
        assert_eq!(
            user_keys(&store, "t"),
            Some(vec!["a", "B", "c"])
        );
        // A fold-equal below-write re-addresses the existing key (stored casing kept).
        user_set(&mut store, "t.b.x", json!(1));
        assert_eq!(
            user_keys(&store, "t"),
            Some(vec!["a", "B", "c"])
        );
        // A same-turn write AT the path replaces the key set — committed keys must not leak
        // through (the overlay's first branch extracts a slice of the written value).
        user_set(&mut store, "t", json!({ "only": 1 }));
        assert_eq!(user_keys(&store, "t"), Some(vec!["only"]));
        // Below-writes after the replacement patch the fresh value.
        user_set(&mut store, "t.later", json!(2));
        assert_eq!(
            user_keys(&store, "t"),
            Some(vec!["only", "later"])
        );
    }

    #[test]
    fn keys_of_a_below_patched_non_object_are_the_written_segments() {
        let mut store = SessionStore::new();
        user_set(&mut store, "s", json!(7));
        store.flush();
        // The below-write replaces the scalar with an object spine: its keys are exactly the
        // written segments, not a merge with the destroyed scalar.
        user_set(&mut store, "s.k", json!(1));
        user_set(&mut store, "s.j", json!(2));
        user_set(&mut store, "s.k", json!(3));
        assert_eq!(
            user_keys(&store, "s"),
            Some(vec!["k", "j"])
        );
        // An entirely-journal subtree (no committed base at all) enumerates the same way.
        user_set(&mut store, "fresh.x", json!(1));
        assert_eq!(user_keys(&store, "fresh"), Some(vec!["x"]));
        assert_eq!(user_tagged(&store, "fresh"), Some(TaggedSnapshot::Object));
    }

    // ---- watchers -------------------------------------------------------------------------

    #[test]
    fn watch_coalesces_to_one_delivery_per_flush_with_final_state() {
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::parse("s").unwrap(),
            main_isolate(),
            FunctionId(3),
            WatchCadence::Coalesced,
        );
        user_set(&mut store, "s.hp", json!(1));
        user_set(&mut store, "s.hp", json!(2));
        user_set(&mut store, "s.mana", json!(9));
        let deliveries = store.flush();
        assert_eq!(deliveries.len(), 1, "three writes coalesce to one delivery");
        let RuntimeAction::CallJavascriptFunction { id, matches, depth, .. } = &deliveries[0]
        else {
            panic!("watch delivery must be a CallJavascriptFunction");
        };
        assert_eq!(*id, FunctionId(3));
        assert_eq!(*depth, 1, "a depth-0 write delivers at depth 1");
        assert_eq!(matches[0].name.as_deref(), Some("snapshot"));
        assert_eq!(matches[0].value, r#"{"hp":2,"mana":9}"#);
        // A flush with no writes delivers nothing.
        assert!(store.flush().is_empty());
    }

    #[test]
    fn watch_fires_for_comparable_paths_only() {
        let mut store = SessionStore::new();
        let above = store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        let exact = store.watch(
            ProducerKey::User,
            StorePath::parse("a.b").unwrap(),
            main_isolate(),
            FunctionId(1),
            WatchCadence::Coalesced,
        );
        let below = store.watch(
            ProducerKey::User,
            StorePath::parse("a.b.c").unwrap(),
            main_isolate(),
            FunctionId(2),
            WatchCadence::Coalesced,
        );
        let sibling = store.watch(
            ProducerKey::User,
            StorePath::parse("a.z").unwrap(),
            main_isolate(),
            FunctionId(3),
            WatchCadence::Coalesced,
        );
        let other_producer = store.watch(
            ProducerKey::Package {
                owner: "wbk".into(),
                name: "pkg".into(),
            },
            StorePath::root(),
            main_isolate(),
            FunctionId(4),
            WatchCadence::Coalesced,
        );
        let _ = (above, exact, below, sibling, other_producer);
        user_set(&mut store, "a.b", json!({ "c": 1 }));
        let mut fired: Vec<usize> = store
            .flush()
            .iter()
            .map(|action| match action {
                RuntimeAction::CallJavascriptFunction { id, .. } => usize::from(*id),
                _ => panic!("unexpected action"),
            })
            .collect();
        fired.sort_unstable();
        assert_eq!(
            fired,
            vec![0, 1, 2],
            "root, exact, and descendant watchers fire; sibling and other producers don't"
        );
    }

    #[test]
    fn unwatch_is_isolate_scoped_and_stops_deliveries() {
        let mut store = SessionStore::new();
        let token = store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        // Another isolate cannot cancel it.
        store.unwatch(token, &pkg_isolate("wbk", "pkg"));
        user_set(&mut store, "x", json!(1));
        assert_eq!(store.flush().len(), 1);
        // The owner can.
        store.unwatch(token, &main_isolate());
        user_set(&mut store, "x", json!(2));
        assert!(store.flush().is_empty());
    }

    #[test]
    fn watch_deliveries_inherit_write_depth_and_cap_at_the_event_limit() {
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        store
            .set(
                ProducerKey::User,
                StorePath::parse("x").unwrap(),
                json!(1),
                main_isolate(),
                5,
            )
            .unwrap();
        let deliveries = store.flush();
        let RuntimeAction::CallJavascriptFunction { depth, .. } = &deliveries[0] else {
            panic!("expected a delivery");
        };
        assert_eq!(*depth, 6);
        // At the cap the delivery is dropped, not queued.
        store
            .set(
                ProducerKey::User,
                StorePath::parse("x").unwrap(),
                json!(2),
                main_isolate(),
                MAX_EVENT_DEPTH,
            )
            .unwrap();
        assert!(store.flush().is_empty(), "a write at the depth cap delivers nothing");
    }

    #[test]
    fn watch_snapshot_is_null_when_the_watched_path_vanishes() {
        let mut store = SessionStore::new();
        user_set(&mut store, "a.b", json!(1));
        store.flush();
        store.watch(
            ProducerKey::User,
            StorePath::parse("a.b").unwrap(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        // Replacing the ancestor without the watched key: the watcher fires with null.
        user_set(&mut store, "a", json!({ "z": 1 }));
        let deliveries = store.flush();
        let RuntimeAction::CallJavascriptFunction { matches, .. } = &deliveries[0] else {
            panic!("expected a delivery");
        };
        assert_eq!(matches[0].value, "null");
    }

    // ---- per-write watchers ----------------------------------------------------------------

    /// Unpack a per-write delivery's `(path, snapshot)` captures.
    fn per_write_capture(action: &RuntimeAction) -> (String, String) {
        let RuntimeAction::CallJavascriptFunction { matches, .. } = action else {
            panic!("per-write delivery must be a CallJavascriptFunction");
        };
        assert_eq!(matches[0].name.as_deref(), Some("path"));
        assert_eq!(matches[1].name.as_deref(), Some("snapshot"));
        (matches[0].value.clone(), matches[1].value.clone())
    }

    #[test]
    fn per_write_watch_replays_every_write_in_order_including_identical_values() {
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::parse("chat").unwrap(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::PerWrite,
        );
        // Two identical writes are two occurrences (the exact case coalesced watch loses).
        user_set(&mut store, "chat.last", json!("hi"));
        user_set(&mut store, "chat.last", json!("hi"));
        user_set(&mut store, "chat", json!({ "last": "bye" }));
        let deliveries = store.flush();
        assert_eq!(deliveries.len(), 3, "one delivery per set-at-path");
        assert_eq!(
            per_write_capture(&deliveries[0]),
            ("chat.last".to_string(), "\"hi\"".to_string())
        );
        assert_eq!(
            per_write_capture(&deliveries[1]),
            ("chat.last".to_string(), "\"hi\"".to_string())
        );
        assert_eq!(
            per_write_capture(&deliveries[2]),
            ("chat".to_string(), r#"{"last":"bye"}"#.to_string())
        );
    }

    #[test]
    fn per_write_watch_hears_comparable_paths_only_and_carries_the_written_path() {
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::parse("a.b").unwrap(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::PerWrite,
        );
        user_set(&mut store, "a.b.c", json!(1)); // below: heard
        user_set(&mut store, "a.z", json!(2)); // sibling: not heard
        user_set(&mut store, "a", json!({ "b": 3 })); // ancestor: heard, path is the write's
        let deliveries = store.flush();
        assert_eq!(deliveries.len(), 2);
        assert_eq!(per_write_capture(&deliveries[0]).0, "a.b.c");
        assert_eq!(per_write_capture(&deliveries[1]).0, "a");
    }

    #[test]
    fn per_write_deliveries_precede_the_coalesced_summary_and_share_the_depth_cap() {
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(1),
            WatchCadence::PerWrite,
        );
        store
            .set(
                ProducerKey::User,
                StorePath::parse("deep").unwrap(),
                json!(1),
                main_isolate(),
                MAX_EVENT_DEPTH,
            )
            .unwrap();
        user_set(&mut store, "shallow", json!(2));
        let deliveries = store.flush();
        // The at-cap write's per-write delivery is dropped individually; the shallow write's
        // survives, and the coalesced summary trails the per-write stream.
        assert_eq!(deliveries.len(), 2);
        let RuntimeAction::CallJavascriptFunction { id, depth, .. } = &deliveries[0] else {
            panic!("expected a delivery");
        };
        assert_eq!(*id, FunctionId(1), "per-write first");
        assert_eq!(*depth, 1, "inherits its own write's depth + 1");
        assert_eq!(per_write_capture(&deliveries[0]).0, "shallow");
        let RuntimeAction::CallJavascriptFunction { id, .. } = &deliveries[1] else {
            panic!("expected a delivery");
        };
        assert_eq!(*id, FunctionId(0), "coalesced summary last");
    }

    #[test]
    fn per_write_watch_unwatch_and_engine_reset_stop_deliveries() {
        let mut store = SessionStore::new();
        let token = store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::PerWrite,
        );
        store.unwatch(token, &main_isolate());
        user_set(&mut store, "x", json!(1));
        assert!(store.flush().is_empty());
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(1),
            WatchCadence::PerWrite,
        );
        store.reset_engine_state();
        user_set(&mut store, "x", json!(2));
        assert!(store.flush().is_empty(), "engine reset drops per-write watchers too");
    }

    // ---- widget bindings -------------------------------------------------------------------

    #[test]
    fn bind_dedupes_per_folded_path_and_seeds_from_committed_state() {
        let mut store = SessionStore::new();
        user_set(&mut store, "Char.Vitals.hp", json!(10));
        store.flush();
        let id = store.bind(ProducerKey::User, StorePath::parse("Char.Vitals.hp").unwrap());
        // Case-folded and re-spelled paths address the same binding.
        let same = store.bind(ProducerKey::User, StorePath::parse(r#"char["vitals"].HP"#).unwrap());
        assert_eq!(id, same);
        let cell = store.bindings().cell(id).expect("registered cell");
        assert_eq!(*cell.load(), json!(10), "seeded from the committed tree");
        // An unbound-yet path seeds Null.
        let absent = store.bind(ProducerKey::User, StorePath::parse("nope").unwrap());
        assert_ne!(id, absent);
        assert_eq!(*store.bindings().cell(absent).unwrap().load(), Value::Null);
    }

    #[test]
    fn flush_updates_cells_for_comparable_paths_only() {
        let mut store = SessionStore::new();
        let exact = store.bind(ProducerKey::User, StorePath::parse("a.b").unwrap());
        let above = store.bind(ProducerKey::User, StorePath::root());
        let below = store.bind(ProducerKey::User, StorePath::parse("a.b.c").unwrap());
        let sibling = store.bind(ProducerKey::User, StorePath::parse("a.z").unwrap());
        let other = store.bind(
            ProducerKey::Package {
                owner: "wbk".into(),
                name: "pkg".into(),
            },
            StorePath::root(),
        );
        assert!(!store.take_bindings_changed(), "registration alone changes nothing");

        user_set(&mut store, "a.b", json!({ "c": 1 }));
        store.flush();
        assert!(store.take_bindings_changed());
        assert!(!store.take_bindings_changed(), "reading resets the flag");
        let cells = store.bindings();
        assert_eq!(*cells.cell(exact).unwrap().load(), json!({ "c": 1 }));
        assert_eq!(*cells.cell(above).unwrap().load(), json!({ "a": { "b": { "c": 1 } } }));
        assert_eq!(*cells.cell(below).unwrap().load(), json!(1));
        assert_eq!(*cells.cell(sibling).unwrap().load(), Value::Null, "sibling untouched");
        assert_eq!(*cells.cell(other).unwrap().load(), Value::Null, "other producer untouched");

        // A sibling-only turn leaves the flag unset for the bound paths it didn't touch...
        user_set(&mut store, "unrelated", json!(1));
        store.flush();
        // ...but the root binding (`above`) is comparable to everything this producer writes.
        assert!(store.take_bindings_changed());
        assert_eq!(*cells.cell(exact).unwrap().load(), json!({ "c": 1 }), "exact cell untouched");
    }

    #[test]
    fn flush_with_no_bound_producer_does_not_mark_changed() {
        let mut store = SessionStore::new();
        store.bind(
            ProducerKey::Package {
                owner: "wbk".into(),
                name: "pkg".into(),
            },
            StorePath::parse("x").unwrap(),
        );
        user_set(&mut store, "anything", json!(1));
        store.flush();
        assert!(!store.take_bindings_changed());
    }

    #[test]
    fn binding_cell_goes_null_when_an_ancestor_write_drops_the_path() {
        let mut store = SessionStore::new();
        user_set(&mut store, "a.b", json!(1));
        store.flush();
        let id = store.bind(ProducerKey::User, StorePath::parse("a.b").unwrap());
        user_set(&mut store, "a", json!({ "z": 2 }));
        store.flush();
        assert_eq!(*store.bindings().cell(id).unwrap().load(), Value::Null);
    }

    #[test]
    fn value_identical_writes_still_mark_changed() {
        // Write-triggered like `watch`: no value diffing at the flush.
        let mut store = SessionStore::new();
        user_set(&mut store, "x", json!(1));
        store.flush();
        store.bind(ProducerKey::User, StorePath::parse("x").unwrap());
        store.take_bindings_changed();
        user_set(&mut store, "x", json!(1));
        store.flush();
        assert!(store.take_bindings_changed());
    }

    #[test]
    fn reset_engine_state_drops_bindings_but_keeps_the_tree() {
        let mut store = SessionStore::new();
        user_set(&mut store, "keep", json!(1));
        store.flush();
        let id = store.bind(ProducerKey::User, StorePath::parse("keep").unwrap());
        let shared = store.bindings();
        store.reset_engine_state();
        assert!(shared.cell(id).is_none(), "stale token ids resolve to nothing");
        // The same shared registry handle serves the next engine generation.
        let next = store.bind(ProducerKey::User, StorePath::parse("keep").unwrap());
        assert_eq!(*shared.cell(next).unwrap().load(), json!(1));
        user_set(&mut store, "keep", json!(2));
        store.flush();
        assert_eq!(*shared.cell(next).unwrap().load(), json!(2));
    }

    // ---- budgets ---------------------------------------------------------------------------

    #[test]
    fn budget_rejects_the_breaching_write_and_keeps_prior_state() {
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 10,
            max_bytes: 10_000,
        });
        user_set(&mut store, "ok", json!([1, 2, 3]));
        let err = store
            .set(
                ProducerKey::User,
                StorePath::parse("big").unwrap(),
                json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
                main_isolate(),
                0,
            )
            .expect_err("the write must breach the entry budget");
        assert!(err.to_string().contains("user"), "names the producer: {err}");
        store.flush();
        assert_eq!(user_get(&store, "ok"), Some(json!([1, 2, 3])));
        assert_eq!(user_get(&store, "big"), None, "the rejected write journaled nothing");
    }

    #[test]
    fn budget_accounts_replacement_not_just_addition() {
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 12,
            max_bytes: 10_000,
        });
        user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8]));
        store.flush();
        // Replacing the large array with a small one shrinks usage; a follow-up write of the
        // same large array fits again — replacement is credited, not double-charged.
        user_set(&mut store, "data", json!(0));
        store.flush();
        user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8]));
        store.flush();
        assert_eq!(
            user_get(&store, "data"),
            Some(json!([1, 2, 3, 4, 5, 6, 7, 8]))
        );
    }

    #[test]
    fn budget_charges_within_turn_writes_cumulatively() {
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 5,
            max_bytes: 10_000,
        });
        // Each `[1, 2]` measures 3 entries (the array + two numbers). The first write fits;
        // the second, IN THE SAME TURN, projects to 6 > 5 — the unflushed journal counts.
        user_set(&mut store, "a", json!([1, 2]));
        let err = store.set(
            ProducerKey::User,
            StorePath::parse("b").unwrap(),
            json!([1, 2]),
            main_isolate(),
            0,
        );
        assert!(err.is_err(), "projected usage must include the unflushed journal");
    }

    #[test]
    fn budgets_are_per_producer() {
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 5,
            max_bytes: 10_000,
        });
        user_set(&mut store, "a", json!([1, 2]));
        // Another producer has its own budget; the same write fits there.
        let pkg = ProducerKey::Package {
            owner: "wbk".into(),
            name: "pkg".into(),
        };
        store
            .set(
                pkg,
                StorePath::parse("a").unwrap(),
                json!([1, 2]),
                pkg_isolate("wbk", "pkg"),
                0,
            )
            .expect("an independent producer subtree has its own budget");
    }

    // ---- engine reset / persistence across reloads ------------------------------------------

    #[test]
    fn reset_engine_state_keeps_the_tree_and_drops_watchers() {
        let mut store = SessionStore::new();
        user_set(&mut store, "keep", json!(42));
        store.flush();
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        store.reset_engine_state();
        assert_eq!(user_get(&store, "keep"), Some(json!(42)), "state survives a reload");
        user_set(&mut store, "keep", json!(43));
        assert!(
            store.flush().is_empty(),
            "old watchers are gone (their FunctionIds pointed into dead isolates)"
        );
    }

    #[test]
    fn path_depth_is_capped_to_prevent_stack_overflow() {
        // A path past the cap is rejected loudly rather than recursing set-at-path off the stack.
        let too_deep = vec!["a"; MAX_PATH_SEGMENTS + 1].join(".");
        assert!(
            StorePath::parse(&too_deep).is_err(),
            "an over-deep path must be rejected, never a stack-overflow abort"
        );
        // Exactly at the cap still parses.
        let at_cap = vec!["a"; MAX_PATH_SEGMENTS].join(".");
        assert_eq!(StorePath::parse(&at_cap).unwrap().segments().len(), MAX_PATH_SEGMENTS);
    }

    #[test]
    fn writing_through_a_non_object_reclaims_the_destroyed_subtree() {
        // The budget must credit a subtree that set-at-path destroys by tunneling through it
        // (`data.note` over `data: [array]` replaces the array with an object). Before the fix
        // the array's usage leaked every cycle, eventually bricking the producer against its own
        // budget while the real tree stayed tiny.
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 20,
            max_bytes: 10_000,
        });
        for _ in 0..50 {
            user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
            store.flush();
            // Tunnels through the array — the whole array is reclaimed, not left charged.
            user_set(&mut store, "data.note", json!(1));
            store.flush();
        }
        // Still writable after many cycles: no phantom-usage accumulation.
        user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
        store.flush();
        assert_eq!(
            user_get(&store, "data"),
            Some(json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]))
        );
    }

    #[test]
    fn coalesced_delivery_uses_the_shallowest_contributing_depth() {
        // A deep (at-cap) write coalesced in the same turn as an innocent depth-0 write to a
        // comparable path must NOT suppress the delivery: the min-fold delivers at the shallow
        // write's depth so the committed shallow change still reaches the watcher.
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::root(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        store
            .set(
                ProducerKey::User,
                StorePath::parse("deep").unwrap(),
                json!(1),
                main_isolate(),
                MAX_EVENT_DEPTH,
            )
            .unwrap();
        store
            .set(
                ProducerKey::User,
                StorePath::parse("shallow").unwrap(),
                json!(2),
                main_isolate(),
                0,
            )
            .unwrap();
        let deliveries = store.flush();
        assert_eq!(deliveries.len(), 1, "the shallow write is not dropped with the deep one");
        let RuntimeAction::CallJavascriptFunction { depth, .. } = &deliveries[0] else {
            panic!("expected a delivery");
        };
        assert_eq!(*depth, 1, "delivered at the shallowest contributing depth + 1");
    }

    // ---- home gate helper --------------------------------------------------------------------

    #[test]
    fn is_home_matches_the_registry() {
        let homes: HomeRegistry = Rc::new(std::cell::RefCell::new(HashMap::from([
            (("wbk".to_string(), "sandboxed".to_string()), HomeIsolate::OwnSandbox),
            (("wbk".to_string(), "trusted".to_string()), HomeIsolate::Main),
        ])));
        let sandboxed = ProducerKey::Package {
            owner: "wbk".into(),
            name: "sandboxed".into(),
        };
        let trusted = ProducerKey::Package {
            owner: "wbk".into(),
            name: "trusted".into(),
        };
        let uninstalled = ProducerKey::Package {
            owner: "wbk".into(),
            name: "ghost".into(),
        };
        // A sandboxed install is home only in its own isolate (any version).
        assert!(is_home(&homes, &sandboxed, &pkg_isolate("wbk", "sandboxed")));
        assert!(!is_home(&homes, &sandboxed, &IsolateId::Main));
        assert!(!is_home(&homes, &sandboxed, &pkg_isolate("wbk", "other")));
        // A trusted install is home on main only.
        assert!(is_home(&homes, &trusted, &IsolateId::Main));
        assert!(!is_home(&homes, &trusted, &pkg_isolate("wbk", "trusted")));
        // An uninstalled package is home nowhere.
        assert!(!is_home(&homes, &uninstalled, &IsolateId::Main));
        assert!(!is_home(&homes, &uninstalled, &pkg_isolate("wbk", "ghost")));
        // User/module code is home exactly on main.
        assert!(is_home(&homes, &ProducerKey::User, &IsolateId::Main));
        assert!(!is_home(&homes, &ProducerKey::User, &pkg_isolate("wbk", "sandboxed")));
    }

    // ---- persistent-tree internals (docs/interop-pre-gmcp-plan.md §4) ------------------------

    #[test]
    fn budget_credits_same_turn_replacement() {
        // The probe reads the turn head (committed ⊕ journal-so-far), so replacing a large
        // subtree WITHIN a turn frees its budget for later writes in the same turn — the
        // committed tree alone would still charge the replaced subtree.
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 12,
            max_bytes: 10_000,
        });
        user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8]));
        user_set(&mut store, "data", json!(0));
        user_set(&mut store, "data", json!([1, 2, 3, 4, 5, 6, 7, 8]));
        store.flush();
        assert_eq!(
            user_get(&store, "data"),
            Some(json!([1, 2, 3, 4, 5, 6, 7, 8]))
        );
    }

    #[test]
    fn committed_usage_matches_the_tree_after_flush() {
        // The committed ledger is frozen from per-write projections, never recomputed, so any
        // per-write undercharge compounds forever and the budget under-enforces — the
        // hostile-feed direction it exists for. The projection math must therefore land
        // exactly on the committed tree's memoized usage. Exercised shapes: a brand-new
        // producer root, plain adds, deep spines through existing objects, replacements, and
        // the alternating scalar-then-tunnel writes that conjure a replacement object per key.
        let mut store = SessionStore::new();
        for i in 0..8 {
            user_set(&mut store, &format!("k{i}"), json!(i));
            user_set(&mut store, &format!("k{i}.x"), json!({ "deep": [1, 2, i] }));
        }
        user_set(&mut store, "spine.a.b.c", json!("leaf"));
        store.flush();
        // A later turn: replacements, a tunnel through a committed scalar, and a fresh spine.
        user_set(&mut store, "spine.a", json!(3.5));
        user_set(&mut store, "spine.a.z", json!(true));
        user_set(&mut store, "k0.x.deep", json!(null));
        user_set(&mut store, "wide", json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
        store.flush();
        let tracked = store
            .usage
            .get(&ProducerKey::User)
            .copied()
            .expect("the flush froze a usage entry");
        let tree = store
            .roots
            .get(&ProducerKey::User)
            .expect("the flush committed a root")
            .usage();
        assert_eq!(tracked, tree, "tracked usage must equal the committed tree's usage");
    }

    #[test]
    fn binding_cell_snapshots_share_structure_with_the_committed_tree() {
        // A binding-cell update is a shallow clone: two consecutive flushes that leave a
        // sibling subtree untouched pin the SAME allocation for it, not a copy per flush.
        let mut store = SessionStore::new();
        let id = store.bind(ProducerKey::User, StorePath::root());
        user_set(&mut store, "stable", json!({ "deep": [1, 2, 3] }));
        user_set(&mut store, "hot", json!(1));
        store.flush();
        let first = store.bindings().cell(id).unwrap().load();
        user_set(&mut store, "hot", json!(2));
        store.flush();
        let second = store.bindings().cell(id).unwrap().load();
        let (Some(Node::Object(a)), Some(Node::Object(b))) =
            (first.get("stable"), second.get("stable"))
        else {
            panic!("both snapshots carry the stable subtree as an object");
        };
        assert!(
            Arc::ptr_eq(a, b),
            "the untouched sibling subtree is shared across flush snapshots"
        );
        assert!(*second.get("hot").unwrap() == json!(2));
        assert!(*first.get("hot").unwrap() == json!(1), "the pinned snapshot is immutable");
    }

    #[test]
    fn snapshots_serialize_byte_identically_to_the_published_json() {
        // Number formatting and string escaping must survive the Node round-trip exactly:
        // watcher snapshots and op-boundary reads compare as text.
        let mut store = SessionStore::new();
        store.watch(
            ProducerKey::User,
            StorePath::parse("t").unwrap(),
            main_isolate(),
            FunctionId(0),
            WatchCadence::Coalesced,
        );
        let value = json!({
            "big": 18_446_744_073_709_551_615_u64,
            "neg": -2.5,
            "exp": 1e30,
            "text": "quote\" slash\\ ctrl\u{1f}",
            "Mixed Key": null
        });
        let expected = value.to_string();
        user_set(&mut store, "t", value);
        assert_eq!(user_get(&store, "t").unwrap().to_string(), expected, "journal overlay read");
        let deliveries = store.flush();
        let RuntimeAction::CallJavascriptFunction { matches, .. } = &deliveries[0] else {
            panic!("expected a delivery");
        };
        assert_eq!(matches[0].value, expected, "watcher snapshot text");
        assert_eq!(user_get(&store, "t").unwrap().to_string(), expected, "committed read");
    }

    // ---- previous generations (docs/interop-pre-gmcp-plan.md §5) ------------------------------

    fn user_prev_tagged(store: &SessionStore, path: &str) -> Option<TaggedSnapshot> {
        prev_tagged_as(store, path, &main_isolate())
    }

    fn prev_tagged_as(
        store: &SessionStore,
        path: &str,
        reader: &IsolateId,
    ) -> Option<TaggedSnapshot> {
        store.previous_get_tagged(&ProducerKey::User, &StorePath::parse(path).unwrap(), reader)
    }

    #[test]
    fn previous_generation_is_absent_before_the_first_commit() {
        let mut store = SessionStore::new();
        assert_eq!(user_prev_tagged(&store, ""), None);
        // The first batch's base is absence, open journal or not.
        user_set(&mut store, "hp", json!(1));
        assert_eq!(user_prev_tagged(&store, ""), None);
        store.flush();
        // After the first commit the state before the first batch is still absence.
        assert_eq!(user_prev_tagged(&store, ""), None);
        assert_eq!(
            store.previous_get_json(&ProducerKey::User, &StorePath::root(), &main_isolate()),
            None
        );
    }

    #[test]
    fn previous_generation_anchors_to_the_newest_write_batch() {
        let mut store = SessionStore::new();
        user_set(&mut store, "hp", json!(1));
        store.flush();
        // An open journal reads its own base: the committed root, not the (absent) retained map.
        user_set(&mut store, "hp", json!(2));
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("1".into())));
        store.flush();
        // Retained: the generation the second commit displaced, held across quiet turns.
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("1".into())));
        assert_eq!(
            store.previous_get_json(&ProducerKey::User, &StorePath::root(), &main_isolate()),
            Some(r#"{"hp":1}"#.to_string())
        );
        store.flush(); // a writeless flush moves nothing
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("1".into())));
        // The next batch re-anchors: its base is the now-committed hp=2, superseding hp=1.
        user_set(&mut store, "hp", json!(3));
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("2".into())));
        store.flush();
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("2".into())));
    }

    #[test]
    fn previous_generations_are_per_producer() {
        let mut store = SessionStore::new();
        let pkg = ProducerKey::Package {
            owner: "wbk".into(),
            name: "pkg".into(),
        };
        user_set(&mut store, "hp", json!(1));
        store.flush();
        user_set(&mut store, "hp", json!(2));
        store.flush();
        // Another producer's committing flush must not move this producer's anchor.
        store
            .set(
                pkg.clone(),
                StorePath::parse("x").unwrap(),
                json!(1),
                pkg_isolate("wbk", "pkg"),
                0,
            )
            .unwrap();
        store.flush();
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("1".into())));
        // And the package, one commit in, still reads absent.
        assert_eq!(
            store.previous_get_tagged(&pkg, &StorePath::root(), &pkg_isolate("wbk", "pkg")),
            None,
            "a producer's first commit retains nothing"
        );
    }

    #[test]
    fn previous_anchor_is_seat_aware_while_a_batch_is_open() {
        let mut store = SessionStore::new();
        user_set(&mut store, "hp", json!(1));
        store.flush();
        user_set(&mut store, "hp", json!(2));
        store.flush();
        // Main (the writing isolate) opens a new batch: its own anchor is the batch's
        // committed base...
        user_set(&mut store, "hp", json!(3));
        assert_eq!(
            user_prev_tagged(&store, "hp"),
            Some(TaggedSnapshot::Scalar("2".into())),
            "the isolate mid-batch reads its open batch's committed base"
        );
        // ...while a reader in another isolate — to whom the open journal is invisible —
        // keeps the retained generation: a producer opening a batch must not move a
        // consumer's anchor.
        let reader = pkg_isolate("wbk", "watcher");
        assert_eq!(
            prev_tagged_as(&store, "hp", &reader),
            Some(TaggedSnapshot::Scalar("1".into())),
            "a cross-isolate reader's anchor stays the retained generation"
        );
        store.flush();
        // The committing flush is what moves every reader's anchor; the seats re-converge.
        assert_eq!(prev_tagged_as(&store, "hp", &reader), Some(TaggedSnapshot::Scalar("2".into())));
        assert_eq!(user_prev_tagged(&store, "hp"), Some(TaggedSnapshot::Scalar("2".into())));
    }

    #[test]
    fn previous_reads_classify_and_enumerate_the_retained_generation() {
        let mut store = SessionStore::new();
        user_set(
            &mut store,
            "t",
            json!({ "a": 1, "B": { "k": 1 }, "arr": [1, 2], "nil": null }),
        );
        store.flush();
        user_set(&mut store, "t", json!({ "fresh": true }));
        store.flush();
        assert_eq!(
            store.previous_keys(
                &ProducerKey::User,
                &StorePath::parse("t").unwrap(),
                &main_isolate()
            ),
            Some(vec!["a", "B", "arr", "nil"]),
            "keys read the retained generation in publish order, first casing"
        );
        // Lookups fold case like every read; kinds classify off the retained tree.
        assert_eq!(user_prev_tagged(&store, "T.b"), Some(TaggedSnapshot::Object));
        assert_eq!(
            user_prev_tagged(&store, "t.arr"),
            Some(TaggedSnapshot::Array("[1,2]".into()))
        );
        // A stored null is a scalar payload; the head's fresh key is absent in the previous view.
        assert_eq!(
            user_prev_tagged(&store, "t.nil"),
            Some(TaggedSnapshot::Scalar("null".into()))
        );
        assert_eq!(user_prev_tagged(&store, "t.fresh"), None);
        // Non-objects have no keys under the previous view either.
        assert_eq!(
            store.previous_keys(
                &ProducerKey::User,
                &StorePath::parse("t.arr").unwrap(),
                &main_isolate()
            ),
            None
        );
    }

    #[test]
    fn previous_generation_shares_structure_with_the_committed_tree() {
        // Retention is an `Arc` move: a subtree the displacing writes left untouched is the
        // SAME allocation in the retained generation and the committed head, not a copy.
        let mut store = SessionStore::new();
        user_set(&mut store, "stable", json!({ "deep": [1, 2, 3] }));
        user_set(&mut store, "hot", json!(1));
        store.flush();
        user_set(&mut store, "hot", json!(2));
        store.flush();
        let (Some(Node::Object(prev)), Some(Node::Object(head))) = (
            store
                .previous
                .get(&ProducerKey::User)
                .and_then(|n| n.get("stable")),
            store
                .roots
                .get(&ProducerKey::User)
                .and_then(|n| n.get("stable")),
        ) else {
            panic!("both generations carry the stable subtree as an object");
        };
        assert!(
            Arc::ptr_eq(prev, head),
            "the untouched subtree is shared between the retained generation and the head"
        );
        // The generation is immutable: the displaced value stays readable as written.
        assert_eq!(user_prev_tagged(&store, "hot"), Some(TaggedSnapshot::Scalar("1".into())));
    }

    #[test]
    fn reset_engine_state_keeps_previous_generations() {
        let mut store = SessionStore::new();
        user_set(&mut store, "hp", json!(1));
        store.flush();
        user_set(&mut store, "hp", json!(2));
        store.flush();
        store.reset_engine_state();
        assert_eq!(
            user_prev_tagged(&store, "hp"),
            Some(TaggedSnapshot::Scalar("1".into())),
            "retained generations are committed data and outlive engine rebuilds, like the tree"
        );
    }

    #[test]
    fn producer_key_parses_specs_and_folds() {
        assert_eq!(ProducerKey::parse("user"), Some(ProducerKey::User));
        assert_eq!(
            ProducerKey::parse("smudgy://WBK/Tracker"),
            Some(ProducerKey::Package {
                owner: "wbk".into(),
                name: "tracker".into()
            })
        );
        assert_eq!(
            ProducerKey::parse("wbk/tracker"),
            Some(ProducerKey::Package {
                owner: "wbk".into(),
                name: "tracker".into()
            })
        );
        assert_eq!(ProducerKey::parse("nonsense"), None);
        assert_eq!(ProducerKey::parse("a/b/c"), None);
    }

    #[test]
    fn producer_key_parses_platform_store_producers() {
        assert_eq!(
            ProducerKey::parse("gmcp"),
            Some(ProducerKey::Platform(PlatformProducer::Gmcp))
        );
        assert_eq!(
            ProducerKey::parse("GMCP"),
            Some(ProducerKey::Platform(PlatformProducer::Gmcp))
        );
        assert_eq!(
            ProducerKey::Platform(PlatformProducer::Gmcp).to_string(),
            "gmcp"
        );
        // Event-only platform names are reserved in the schemes but are NOT store producers.
        assert_eq!(ProducerKey::parse("sys"), None);
        assert_eq!(ProducerKey::parse("map"), None);
        // No isolate is ever home for a platform producer (the host is the sole writer).
        let homes: HomeRegistry = Rc::new(std::cell::RefCell::new(HashMap::new()));
        assert!(!is_home(
            &homes,
            &ProducerKey::Platform(PlatformProducer::Gmcp),
            &IsolateId::Main
        ));
    }
}
