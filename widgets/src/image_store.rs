//! The process-global image store: resolved sources → decoded iced image handles.
//!
//! One store serves every session (entries are keyed by *content source*, not by creator or
//! session, so sessions share fetches, decodes, and GPU uploads). It is `Send + Sync` and
//! deliberately NOT a UI-thread-local store like `MapStore`/`TextEditorStore`: a canvas
//! `SceneProgram` is built on the script thread, loader tasks live on the store's own tokio
//! runtime, and render closures read on the UI thread.
//!
//! ## Hot-path contract (plan D3)
//!
//! `createWidget()` runs every frame in some scripts, so the steady-state read path takes
//! **no locks, makes no syscalls, and does no parsing**: the map is an
//! [`ArcSwap`]`<HashMap<..>>` (lock-free load; copy-on-write swaps under a small writer
//! mutex, exactly the [`crate::WidgetRoot`] idiom) and each entry is an [`ImageEntryCell`]
//! — an `ArcSwap<EntryState>` in the `StoreBindingCell` shape. Build ops call
//! [`ImageStore::ensure`] once per (memoized) source and capture the returned `Arc` cell;
//! per-frame renders do one `cell.load()`.
//!
//! LRU recency is a relaxed atomic stamp touched on load, so reads never write shared
//! state; eviction sweeps run on the store runtime and, because a displayed image's stamp
//! is hot by construction, can only ever evict entries nothing is drawing.
//!
//! Handles are minted HERE and only here: `Handle::from_rgba` mints a unique id per call,
//! so a per-frame mint would re-upload the texture every frame.
//!
//! ## Held entries
//!
//! Pixels decoded elsewhere (terminal graphics) come in through [`ImageStore::ensure_rgba`]
//! and are owned by whoever decoded them: the caller takes a [hold](ImageStore::hold) on
//! the entry (as it is inserted, through [`ImageStore::ensure_rgba_held`], so the cap's
//! sweep never counts it) and the entry lives exactly as long as its last hold. A held
//! entry is outside the byte cap (the session's own quota bounds those pixels; counting
//! them here would have the cap sweep on every frame of an animation and find nothing to
//! evict) and is removed when the last hold is released, on the store runtime, only while
//! its key still maps to that very cell: a fresh entry minted under the key meanwhile is
//! left alone.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use arc_swap::ArcSwap;
use smudgy_cloud::image_source::{ImageSourcePolicy, ResolvedImageSource};
use tokio::sync::watch;

/// In-memory decoded-bytes cap (a **CPU-side** bound — GPU residency is `iced_wgpu`'s
/// hit-based atlas trim). The user-facing `image_cache_max_mb` setting bounds the *disk*
/// cache instead; this constant is deliberately not configurable.
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;

/// How long a *transient*-failed entry stays sticky before an `ensure` may retry it.
/// Permanent failures (policy denials, decode errors — [`FetchError::transient`] false)
/// never retry: the input can't get better on its own.
const FAILED_RETRY_TTL: Duration = Duration::from_secs(30);

/// Minimum interval between freshness re-stats of a local-file entry. The re-stat itself
/// runs on the store runtime — reads only compare this stamp (no frame-thread syscalls).
const LOCAL_RECHECK_INTERVAL_MS: u64 = 1_000;

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// mtime + size of an on-disk source at decode time, for cheap staleness probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub mtime: SystemTime,
    pub size: u64,
}

/// What a fetch produces: straight (non-premultiplied) RGBA8, already EXIF-oriented and
/// limit-checked by the fetcher. The store mints the iced handle from this.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Present for local-file-backed sources; drives the freshness re-probe.
    pub file_stamp: Option<FileStamp>,
}

/// Why a fetch failed, and whether waiting could help. `transient` (network/transport,
/// 5xx-class) failures retry after [`FAILED_RETRY_TTL`]; permanent ones (policy denial,
/// 4xx, decode/SVG rejection, size caps) stay `Failed` until a cache clear.
#[derive(Debug, Clone)]
pub struct FetchError {
    pub reason: String,
    pub transient: bool,
}

impl FetchError {
    #[must_use]
    pub fn permanent(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            transient: false,
        }
    }

    #[must_use]
    pub fn transient(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            transient: true,
        }
    }
}

/// The side that knows how to load bytes and decode them (implemented in `smudgy_ui`,
/// which can name `core` + `cloud`). Implementations own their decode-concurrency bound
/// and all I/O policy (HTTP cache, redirects, size caps); the store owns lifecycle,
/// memoization, and handle identity.
pub trait ImageFetcher: Send + Sync + 'static {
    /// Load + decode one source. `policy` is the *first requester's* isolate policy: its
    /// `server_name` attributes the fetch to that server's HTTP-cache namespace (plan D10),
    /// its `net_grants` re-validate every redirect hop for sandboxed loads, and its
    /// `read_grants` are re-checked post-canonicalization for local files. Entries are
    /// content-keyed, so a later requester with a different policy shares the result —
    /// its own policy already validated the *initial* URL/path at resolve time.
    fn fetch(
        &self,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
    ) -> BoxFut<Result<DecodedImage, FetchError>>;

    /// Cheap staleness probe for local-file-backed sources (`None` for others or on
    /// error). `policy` locates policy-relative sources (a local dev-override package's
    /// on-disk asset dir).
    fn probe(
        &self,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
    ) -> BoxFut<Option<FileStamp>>;
}

/// One image entry's current state. Swapped whole through the cell's `ArcSwap`.
#[derive(Debug, Clone)]
pub enum EntryState {
    /// Fetch in flight (or queued). Renders as the placeholder.
    Loading,
    /// Decoded and displayable. `handle` is the single minted identity for this content.
    Ready {
        handle: iced::widget::image::Handle,
        width: u32,
        height: u32,
        decoded_bytes: u64,
        file_stamp: Option<FileStamp>,
    },
    /// Load failed. Sticky: re-`ensure` is side-effect-free until `retry_at` (if any).
    Failed {
        reason: Arc<str>,
        retry_at: Option<Instant>,
    },
}

/// One source's live slot: state cell + recency/freshness stamps. Render closures and
/// resolve memos hold `Arc<ImageEntryCell>` and load per frame, lock-free.
#[derive(Debug)]
pub struct ImageEntryCell {
    state: ArcSwap<EntryState>,
    /// Millis since the store epoch of the last read — the LRU recency stamp. Relaxed:
    /// approximate order is all eviction needs.
    touched_ms: AtomicU64,
    /// Millis since epoch of the last freshness re-probe enqueue (local files only).
    last_probe_ms: AtomicU64,
    /// Set (under the writer lock) when the eviction sweep removes this cell from the
    /// map. Memo holders re-`ensure` on sight; live per-frame draws keep their stamp hot,
    /// so an evicted cell is by construction one nothing was drawing.
    evicted: AtomicBool,
    /// Abort handle for the in-flight fetch, if any. Cold path only.
    abort: Mutex<Option<tokio::task::AbortHandle>>,
    /// Holds on the entry ([`ImageStore::hold`]), changed under the writer lock. While
    /// any remain the cap's sweep leaves the entry alone, and the last one's release
    /// removes it.
    holders: AtomicUsize,
    /// The owning store's epoch, so reads can stamp recency without reaching back to the
    /// store. `Instant` is `Copy`; every cell of one store carries the same epoch.
    epoch: Instant,
}

impl ImageEntryCell {
    fn new(state: EntryState, now_ms: u64, epoch: Instant) -> Self {
        Self {
            state: ArcSwap::from_pointee(state),
            touched_ms: AtomicU64::new(now_ms),
            last_probe_ms: AtomicU64::new(now_ms),
            evicted: AtomicBool::new(false),
            abort: Mutex::new(None),
            holders: AtomicUsize::new(0),
            epoch,
        }
    }

    /// Whether a hold is on the entry.
    fn is_held(&self) -> bool {
        self.holders.load(Ordering::Relaxed) > 0
    }

    /// The bytes a `Ready` entry holds; zero otherwise.
    fn ready_bytes(&self) -> u64 {
        match &**self.state.load() {
            EntryState::Ready { decoded_bytes, .. } => *decoded_bytes,
            _ => 0,
        }
    }

    /// The current state (lock-free), stamping LRU recency — a drawn image is a hot image,
    /// even when its widget was built once and only re-renders (plan D3: "touched on
    /// load"), so `evict_cold` can never evict what a live render closure is displaying.
    /// One relaxed store + one monotonic-clock read; no locks, no syscalls.
    #[must_use]
    pub fn state(&self) -> Arc<EntryState> {
        self.touch();
        self.state.load_full()
    }

    /// Stamp recency without reading the state.
    pub fn touch(&self) {
        let now = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.touched_ms.store(now, Ordering::Relaxed);
    }

    /// Whether the eviction sweep dropped this cell from the map (memo holders must
    /// re-`ensure` instead of trusting a stale `Arc`).
    #[must_use]
    pub fn is_evicted(&self) -> bool {
        self.evicted.load(Ordering::Relaxed)
    }
}

type EntryMap = HashMap<String, Arc<ImageEntryCell>>;

struct StoreInner {
    map: ArcSwap<EntryMap>,
    /// Serializes map swaps (insert/evict/clear). Never held on a read path.
    writer: Mutex<()>,
    fetcher: Arc<dyn ImageFetcher>,
    /// Bumped on every state *transition* of a fetched entry (completion, failure,
    /// refresh, clear) — the canvas geometry cache compares this to know a redraw's
    /// inputs changed. Held entries (terminal graphics) come and go without bumping it:
    /// no canvas draws them, and a bump would clear every image-bearing canvas's cache
    /// on each frame of an animation.
    completion_generation: AtomicU64,
    /// Bumped on clear-cache. In-flight completions stamped with an older generation are
    /// discarded instead of resurrecting flushed entries.
    flush_generation: AtomicU64,
    /// Sum of `decoded_bytes` across `Ready` entries in the map.
    total_ready_bytes: AtomicU64,
    /// The part of `total_ready_bytes` under a hold, which the cap does not count.
    /// Changed under the writer lock, with the holds and the map.
    held_bytes: AtomicU64,
    /// Repaint wakers, one per live session (weak: sessions die, the store doesn't).
    wakers: Mutex<Vec<Weak<watch::Sender<()>>>>,
    epoch: Instant,
    runtime: OnceLock<tokio::runtime::Runtime>,
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        // A spawned fetch task holds a store clone; when it finishes LAST (tests build
        // short-lived stores; production's is global), this Drop runs on a runtime worker,
        // where a blocking Runtime::drop panics. Shutdown in the background instead.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// The global store handle (cheap to clone; all clones share one inner).
#[derive(Clone)]
pub struct ImageStore {
    inner: Arc<StoreInner>,
}

impl std::fmt::Debug for ImageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageStore {{ entries: {}, ready_bytes: {} }}",
            self.inner.map.load().len(),
            self.inner.total_ready_bytes.load(Ordering::Relaxed)
        )
    }
}

impl ImageStore {
    #[must_use]
    pub fn new(fetcher: Arc<dyn ImageFetcher>) -> Self {
        Self {
            inner: Arc::new(StoreInner {
                map: ArcSwap::from_pointee(EntryMap::new()),
                writer: Mutex::new(()),
                fetcher,
                completion_generation: AtomicU64::new(0),
                flush_generation: AtomicU64::new(0),
                total_ready_bytes: AtomicU64::new(0),
                held_bytes: AtomicU64::new(0),
                wakers: Mutex::new(Vec::new()),
                epoch: Instant::now(),
                runtime: OnceLock::new(),
            }),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.inner.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.inner.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("smudgy-images")
                .enable_all()
                .build()
                .expect("the image-loader runtime always builds")
        })
    }

    /// A handle to the store's own `smudgy-images` runtime (built on first use), for
    /// image work that lands somewhere other than a fetch: the terminal graphics
    /// decoder runs its off-thread decodes here rather than on a runtime of its own.
    /// The runtime is multi-threaded, so tasks on it may `block_in_place`.
    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime().handle().clone()
    }

    /// Register a session's repaint waker (the `WidgetRoot`'s watch sender). Weak — a
    /// closed session's entry is pruned on the next poke.
    ///
    /// # Panics
    /// If the waker registry mutex is poisoned (a registrant panicked mid-push).
    pub fn register_waker(&self, waker: Weak<watch::Sender<()>>) {
        self.inner.wakers.lock().expect("waker lock").push(waker);
    }

    /// Wake every live session (loads landed; repaint). Snapshot outside any store lock.
    fn poke_all(&self) {
        let snapshot: Vec<_> = {
            let mut wakers = self.inner.wakers.lock().expect("waker lock");
            wakers.retain(|w| w.strong_count() > 0);
            wakers.clone()
        };
        for waker in snapshot {
            if let Some(tx) = waker.upgrade() {
                tx.send(()).ok();
            }
        }
    }

    /// Monotonic state-transition counter of the fetched entries (canvas cache
    /// invalidation); entries minted from pixels decoded elsewhere
    /// ([`Self::ensure_rgba`]) and their removal leave it alone. Acquire: pairs with
    /// `complete()`'s Release bump so an observer of generation N+1 also observes the
    /// entry state that transition published — a canvas that clears its geometry cache
    /// on the bump must re-record the *new* state, not a stale `Loading`.
    #[must_use]
    pub fn completion_generation(&self) -> u64 {
        self.inner.completion_generation.load(Ordering::Acquire)
    }

    /// Total decoded bytes currently held (diagnostics / future UI): the fetched
    /// entries the cap bounds and the held ones it does not.
    #[must_use]
    pub fn ready_bytes(&self) -> u64 {
        self.inner.total_ready_bytes.load(Ordering::Relaxed)
    }

    /// The decoded bytes of the entries under a hold: outside the cap.
    #[must_use]
    pub(crate) fn held_bytes(&self) -> u64 {
        self.inner.held_bytes.load(Ordering::Relaxed)
    }

    /// The decoded bytes the cap applies to: every `Ready` entry's but the held ones'.
    fn capped_bytes(&self) -> u64 {
        self.ready_bytes().saturating_sub(self.held_bytes())
    }

    /// The existing cell for `key`, if any — one lock-free map load, no touch, no spawn.
    /// Lets a parse path distinguish already-known sources from ones whose `ensure`
    /// would spawn a brand-new fetch (the bound-scene new-source budget).
    #[must_use]
    pub fn peek(&self, key: &str) -> Option<Arc<ImageEntryCell>> {
        self.inner.map.load().get(key).cloned()
    }

    /// The entry cell for `source`, spawning a fetch exactly once per distinct source.
    /// Steady state (entry exists): one lock-free map load + one relaxed stamp store.
    /// `policy` drives a *newly spawned* fetch (cache namespace, hop grants — see
    /// [`ImageFetcher::fetch`]); an existing entry ignores it.
    #[must_use]
    pub fn ensure(
        &self,
        source: &ResolvedImageSource,
        policy: &Arc<ImageSourcePolicy>,
    ) -> Arc<ImageEntryCell> {
        self.ensure_keyed(&source.store_key(policy), source, policy)
    }

    /// [`ensure`](Self::ensure) with the cache key already in hand. The per-frame resolve
    /// memo stores `(source, key)` and calls this, so the steady-state frame path never
    /// re-derives the key (for a `data:` src that derivation once hashed megabytes) and
    /// never allocates: borrowed lookup, relaxed touch, one state load.
    #[must_use]
    pub fn ensure_keyed(
        &self,
        key: &str,
        source: &ResolvedImageSource,
        policy: &Arc<ImageSourcePolicy>,
    ) -> Arc<ImageEntryCell> {
        let now = self.now_ms();
        if let Some(cell) = self.inner.map.load().get(key) {
            cell.touched_ms.store(now, Ordering::Relaxed);
            self.maybe_refresh(key, cell, source, policy, now);
            return cell.clone();
        }
        self.insert_and_spawn(key, source, policy, now)
    }

    /// The entry cell for pixels decoded elsewhere, keyed by `key` (terminal graphics use
    /// `kitty:{uid}`). An existing cell is returned touched, as is; a new key inserts a
    /// `Ready` cell whose handle wraps `rgba` without copying it and accounts its bytes.
    /// The completion generation is not bumped (no canvas draws these). Nothing is ever
    /// fetched for such a key: no fetch is spawned here, and the fast path's
    /// `maybe_refresh` only acts on failed entries and file-stamped ones, which this
    /// never makes.
    ///
    /// `rgba` must be straight RGBA8, `width * height * 4` bytes: a buffer another length
    /// is never uploaded (the renderer would read past a short one) and gets a `Failed`
    /// cell that is not inserted, so the key stays free for the right pixels. Callers
    /// memoise the returned cell and call this again once it
    /// [`is_evicted`](ImageEntryCell::is_evicted), which mints a fresh handle from the same
    /// pixels. Until a [hold](Self::hold) is taken on it the entry counts against the cap
    /// like a fetched one.
    ///
    /// # Panics
    /// If the store's writer mutex is poisoned (a writer panicked mid-swap).
    #[must_use]
    pub fn ensure_rgba(
        &self,
        key: &str,
        width: u32,
        height: u32,
        rgba: bytes::Bytes,
    ) -> Arc<ImageEntryCell> {
        self.insert_rgba(key, width, height, rgba, false).0
    }

    /// [`Self::ensure_rgba`] for a caller that will [hold](Self::hold) the entry: the hold
    /// is taken as the entry is inserted, under the same writer lock, so the cap's sweep an
    /// insert over the cap runs counts the fetched entries' bytes alone — a hold taken
    /// after the insert would come too late for a fetched entry that fit beside the new
    /// one and was swept to make room for it. `None` with an entry the store already had
    /// (or a `Failed` cell for a buffer the size disagrees with), returned as
    /// [`Self::ensure_rgba`] returns it: whoever holds it keeps holding it, and a caller
    /// that does not may hold it now, with nothing to sweep between.
    ///
    /// # Panics
    /// If the store's writer mutex is poisoned (a writer panicked mid-swap).
    #[must_use]
    pub fn ensure_rgba_held(
        &self,
        key: &str,
        width: u32,
        height: u32,
        rgba: bytes::Bytes,
    ) -> (Arc<ImageEntryCell>, Option<EntryHold>) {
        self.insert_rgba(key, width, height, rgba, true)
    }

    /// [`Self::ensure_rgba`] and [`Self::ensure_rgba_held`]: the hold, when `take_hold`,
    /// is on an entry this call inserted.
    fn insert_rgba(
        &self,
        key: &str,
        width: u32,
        height: u32,
        rgba: bytes::Bytes,
        take_hold: bool,
    ) -> (Arc<ImageEntryCell>, Option<EntryHold>) {
        let now = self.now_ms();
        if let Some(cell) = self.inner.map.load().get(key) {
            cell.touched_ms.store(now, Ordering::Relaxed);
            return (cell.clone(), None);
        }
        let decoded_bytes = u64::from(width) * u64::from(height) * 4;
        if u64::try_from(rgba.len()) != Ok(decoded_bytes) {
            log::warn!(
                "{key}: {} RGBA bytes for {width}x{height}, {decoded_bytes} expected; not uploaded",
                rgba.len()
            );
            let failed = Arc::new(ImageEntryCell::new(
                EntryState::Failed {
                    reason: "pixel buffer does not match its size".into(),
                    retry_at: None,
                },
                now,
                self.inner.epoch,
            ));
            return (failed, None);
        }
        let (cell, hold, over_cap) = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            if let Some(existing) = self.inner.map.load().get(key) {
                existing.touched_ms.store(now, Ordering::Relaxed);
                return (existing.clone(), None);
            }
            let cell = Arc::new(ImageEntryCell::new(
                EntryState::Ready {
                    handle: iced::widget::image::Handle::from_rgba(width, height, rgba),
                    width,
                    height,
                    decoded_bytes,
                    file_stamp: None,
                },
                now,
                self.inner.epoch,
            ));
            let mut map: EntryMap = (**self.inner.map.load()).clone();
            map.insert(key.to_owned(), cell.clone());
            self.inner.map.store(Arc::new(map));
            self.inner
                .total_ready_bytes
                .fetch_add(decoded_bytes, Ordering::Relaxed);
            // Held before the cap is measured: a held entry's bytes are outside it.
            let hold = take_hold.then(|| self.hold_locked(key, &cell));
            (cell, hold, self.capped_bytes() > MAX_DECODED_BYTES)
        };
        // Outside the writer scope: evict_cold re-takes the (non-reentrant) writer lock.
        // The new cell carries the newest stamp, so the sweep takes colder entries first.
        if over_cap {
            self.evict_cold();
        }
        (cell, hold)
    }

    /// Slow path: insert a `Loading` cell (double-checked under the writer lock) and spawn
    /// its fetch on the store runtime. The flush generation is captured *inside* the lock
    /// scope, so a `clear()` serialized after this insert is guaranteed to observe a newer
    /// generation than the spawned task carries — its completion is then discarded.
    fn insert_and_spawn(
        &self,
        key: &str,
        source: &ResolvedImageSource,
        policy: &Arc<ImageSourcePolicy>,
        now: u64,
    ) -> Arc<ImageEntryCell> {
        let (cell, flush_gen) = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            if let Some(existing) = self.inner.map.load().get(key) {
                existing.touched_ms.store(now, Ordering::Relaxed);
                return existing.clone();
            }
            let cell = Arc::new(ImageEntryCell::new(
                EntryState::Loading,
                now,
                self.inner.epoch,
            ));
            let mut map: EntryMap = (**self.inner.map.load()).clone();
            map.insert(key.to_owned(), cell.clone());
            self.inner.map.store(Arc::new(map));
            (cell, self.inner.flush_generation.load(Ordering::Acquire))
        };
        self.spawn_fetch(
            key.to_owned(),
            &cell,
            source.clone(),
            policy.clone(),
            flush_gen,
        );
        cell
    }

    fn spawn_fetch(
        &self,
        key: String,
        cell: &Arc<ImageEntryCell>,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
        flush_gen: u64,
    ) {
        let store = self.clone();
        let fut = self.inner.fetcher.fetch(source, policy);
        let task_cell = cell.clone();
        let task = self.runtime().spawn(async move {
            let result = fut.await;
            store.complete(&key, &task_cell, flush_gen, result);
        });
        *cell.abort.lock().expect("abort lock") = Some(task.abort_handle());
    }

    /// Land a fetch result: swap the cell state, account bytes, maybe evict, wake.
    ///
    /// State swap + byte accounting happen under the writer lock, serialized against
    /// `clear()` and `evict_cold()`: a completion whose flush generation is stale, or whose
    /// cell is no longer the map's current cell for `key` (evicted mid-flight, or cleared
    /// and re-ensured), still lands its state (a render closure may hold the cell and can
    /// show the image) but is never *accounted* — `total_ready_bytes` tracks map-resident
    /// `Ready` cells exactly, so the cap can neither leak phantom bytes nor double-subtract.
    fn complete(
        &self,
        key: &str,
        cell: &Arc<ImageEntryCell>,
        flush_gen: u64,
        result: Result<DecodedImage, FetchError>,
    ) {
        let state = match result {
            Ok(decoded) => {
                let decoded_bytes = decoded.rgba.len() as u64;
                let handle = iced::widget::image::Handle::from_rgba(
                    decoded.width,
                    decoded.height,
                    decoded.rgba,
                );
                EntryState::Ready {
                    handle,
                    width: decoded.width,
                    height: decoded.height,
                    decoded_bytes,
                    file_stamp: decoded.file_stamp,
                }
            }
            Err(err) => {
                log::warn!("smudgy images: {key} failed to load: {}", err.reason);
                EntryState::Failed {
                    reason: Arc::from(err.reason),
                    retry_at: err.transient.then(|| Instant::now() + FAILED_RETRY_TTL),
                }
            }
        };
        let over_cap = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            if self.inner.flush_generation.load(Ordering::Acquire) != flush_gen {
                return;
            }
            let current = self
                .inner
                .map
                .load()
                .get(key)
                .is_some_and(|mapped| Arc::ptr_eq(mapped, cell));
            let previous_bytes = cell.ready_bytes();
            let new_bytes = match &state {
                EntryState::Ready { decoded_bytes, .. } => *decoded_bytes,
                _ => 0,
            };
            cell.state.store(Arc::new(state));
            if current {
                // Nothing holds a fetched entry; the held count follows all the same, so
                // it stays exact whatever is held.
                let counters = if cell.is_held() {
                    &[&self.inner.total_ready_bytes, &self.inner.held_bytes][..]
                } else {
                    &[&self.inner.total_ready_bytes][..]
                };
                for counter in counters {
                    if new_bytes > 0 {
                        counter.fetch_add(new_bytes, Ordering::Relaxed);
                    }
                    if previous_bytes > 0 {
                        counter.fetch_sub(previous_bytes, Ordering::Relaxed);
                    }
                }
            }
            *cell.abort.lock().expect("abort lock") = None;
            self.inner
                .completion_generation
                .fetch_add(1, Ordering::Release);
            self.capped_bytes() > MAX_DECODED_BYTES
        };
        // Outside the writer scope: evict_cold re-takes the (non-reentrant) writer lock.
        if over_cap {
            self.evict_cold();
        }
        self.poke_all();
    }

    /// Re-spawn behavior on the fast path: retry expired network failures, and throttle
    /// freshness probes for local-file entries. Never blocks, never stats — probes run on
    /// the store runtime.
    fn maybe_refresh(
        &self,
        key: &str,
        cell: &Arc<ImageEntryCell>,
        source: &ResolvedImageSource,
        policy: &Arc<ImageSourcePolicy>,
        now: u64,
    ) {
        match &**cell.state.load() {
            EntryState::Failed {
                retry_at: Some(at), ..
            } if *at <= Instant::now() => {
                // One winner: CAS-like via the writer lock (cold — a failed entry).
                let respawn = {
                    let _writer = self.inner.writer.lock().expect("writer lock");
                    if let EntryState::Failed {
                        retry_at: Some(at), ..
                    } = &**cell.state.load()
                        && *at <= Instant::now()
                    {
                        cell.state.store(Arc::new(EntryState::Loading));
                        Some(self.inner.flush_generation.load(Ordering::Acquire))
                    } else {
                        None
                    }
                };
                if let Some(flush_gen) = respawn {
                    self.spawn_fetch(
                        key.to_string(),
                        cell,
                        source.clone(),
                        policy.clone(),
                        flush_gen,
                    );
                }
            }
            EntryState::Ready {
                file_stamp: Some(stamp),
                ..
            } => {
                let last = cell.last_probe_ms.load(Ordering::Relaxed);
                if now.saturating_sub(last) >= LOCAL_RECHECK_INTERVAL_MS
                    && cell
                        .last_probe_ms
                        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                {
                    let store = self.clone();
                    let cell = cell.clone();
                    let source = source.clone();
                    let policy = policy.clone();
                    let key = key.to_string();
                    let expected = *stamp;
                    let probe = self.inner.fetcher.probe(source.clone(), policy.clone());
                    self.runtime().spawn(async move {
                        if probe.await.is_some_and(|current| current != expected) {
                            let flush_gen = store.inner.flush_generation.load(Ordering::Acquire);
                            store.spawn_fetch(key, &cell, source, policy, flush_gen);
                        }
                    });
                }
            }
            _ => {}
        }
    }

    /// Evict least-recently-touched `Ready` entries until under the byte cap. Runs under
    /// the writer lock (rare: only when a completion crosses the cap). A cell being drawn
    /// every frame has a hot stamp and is effectively unevictable; a held one is outside
    /// the cap altogether and never looked at.
    fn evict_cold(&self) {
        let _writer = self.inner.writer.lock().expect("writer lock");
        let mut map: EntryMap = (**self.inner.map.load()).clone();
        let mut ready: Vec<(u64, String, u64)> = map
            .iter()
            .filter(|(_, cell)| !cell.is_held())
            .filter_map(|(key, cell)| match &**cell.state.load() {
                EntryState::Ready { decoded_bytes, .. } => Some((
                    cell.touched_ms.load(Ordering::Relaxed),
                    key.clone(),
                    *decoded_bytes,
                )),
                _ => None,
            })
            .collect();
        ready.sort_unstable();
        let mut total = self.capped_bytes();
        for (_touched, key, bytes) in ready {
            if total <= MAX_DECODED_BYTES {
                break;
            }
            if let Some(cell) = map.remove(&key) {
                cell.evicted.store(true, Ordering::Relaxed);
                total = total.saturating_sub(bytes);
                self.inner
                    .total_ready_bytes
                    .fetch_sub(bytes, Ordering::Relaxed);
            }
        }
        self.inner.map.store(Arc::new(map));
    }

    /// Drop the entry under `key`, whatever its state and whoever holds it: its cell is
    /// flagged evicted (memo holders re-`ensure` on sight), a fetch in flight is aborted
    /// and its bytes leave the accounting. The completion generation is not bumped: this
    /// is terminal graphics' path, for an entry whose key no longer names its pixels (a
    /// root frame replaced in place), and no canvas draws those. Nothing happens for a key
    /// the map does not hold.
    ///
    /// # Panics
    /// If the store's writer mutex is poisoned (a writer panicked mid-swap).
    pub fn remove(&self, key: &str) {
        self.remove_when(key, |_| true);
    }

    /// [`Self::remove`] only while `key` still maps to `cell`: the identity-aware removal
    /// for a caller that minted `cell` and must not take a fresh entry minted under the
    /// key since. Nothing happens otherwise. What a hold's release does, for the tests of
    /// the identity check alone.
    #[cfg(test)]
    fn remove_if(&self, key: &str, cell: &Arc<ImageEntryCell>) {
        self.remove_when(key, |mapped| Arc::ptr_eq(mapped, cell));
    }

    /// Remove the entry under `key` when `wanted` says so of its cell, under the writer
    /// lock; the fetch abort, if any, happens outside it.
    fn remove_when(&self, key: &str, wanted: impl FnOnce(&Arc<ImageEntryCell>) -> bool) {
        let removed = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            if !self.inner.map.load().get(key).is_some_and(wanted) {
                return;
            }
            let mut map: EntryMap = (**self.inner.map.load()).clone();
            let cell = self.remove_cell(&mut map, key);
            self.inner.map.store(Arc::new(map));
            cell
        };
        if let Some(cell) = removed
            && let Some(abort) = cell.abort.lock().expect("abort lock").take()
        {
            abort.abort();
        }
    }

    /// Take `key` out of `map` (the writer's copy, under its lock), flagging the cell
    /// evicted and taking its bytes out of the accounting — the held count too when a
    /// hold is on it. Both settle before the caller swaps the map in: a reader that
    /// finds the key gone must find its bytes gone too, and its cell evicted.
    fn remove_cell(&self, map: &mut EntryMap, key: &str) -> Option<Arc<ImageEntryCell>> {
        let cell = map.remove(key)?;
        cell.evicted.store(true, Ordering::Relaxed);
        let bytes = cell.ready_bytes();
        if bytes > 0 {
            self.inner
                .total_ready_bytes
                .fetch_sub(bytes, Ordering::Relaxed);
            if cell.is_held() {
                self.inner.held_bytes.fetch_sub(bytes, Ordering::Relaxed);
            }
        }
        Some(cell)
    }

    /// A hold on `cell`, the entry under `key`: while any hold is on an entry the cap's
    /// sweep leaves it alone (its bytes are the holder's to bound), and the last hold's
    /// release removes it — on the store's own runtime, so the thread dropping the hold
    /// (the socket thread, mostly) only queues a task — only while the key still maps to
    /// this cell. A hold on a cell the map no longer has is inert: the caller finds it
    /// [evicted](ImageEntryCell::is_evicted) and ensures again.
    ///
    /// # Panics
    /// If the store's writer mutex is poisoned (a writer panicked mid-swap).
    #[must_use]
    pub fn hold(&self, key: &str, cell: &Arc<ImageEntryCell>) -> EntryHold {
        let _writer = self.inner.writer.lock().expect("writer lock");
        self.hold_locked(key, cell)
    }

    /// [`Self::hold`] with the writer lock held by the caller.
    fn hold_locked(&self, key: &str, cell: &Arc<ImageEntryCell>) -> EntryHold {
        let first = cell.holders.fetch_add(1, Ordering::Relaxed) == 0;
        let current = self
            .inner
            .map
            .load()
            .get(key)
            .is_some_and(|mapped| Arc::ptr_eq(mapped, cell));
        if first && current {
            let bytes = cell.ready_bytes();
            if bytes > 0 {
                self.inner.held_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
        }
        EntryHold {
            store: self.clone(),
            key: key.to_owned(),
            cell: Arc::downgrade(cell),
        }
    }

    /// Let go of one hold on `cell` under `key`; the last removes the entry while the key
    /// still maps to the cell.
    fn release(&self, key: &str, cell: &Weak<ImageEntryCell>) {
        let Some(cell) = cell.upgrade() else {
            // Dropped altogether: long gone from the map.
            return;
        };
        let removed = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            let last = cell.holders.load(Ordering::Relaxed) == 1;
            let current = self
                .inner
                .map
                .load()
                .get(key)
                .is_some_and(|mapped| Arc::ptr_eq(mapped, &cell));
            let removed = if last && current {
                let mut map: EntryMap = (**self.inner.map.load()).clone();
                let removed = self.remove_cell(&mut map, key);
                self.inner.map.store(Arc::new(map));
                removed
            } else {
                None
            };
            cell.holders.fetch_sub(1, Ordering::Relaxed);
            removed
        };
        if let Some(cell) = removed
            && let Some(abort) = cell.abort.lock().expect("abort lock").take()
        {
            abort.abort();
        }
    }

    /// Clear everything: abort in-flight fetches, drop all entries, bump both generations
    /// (so late completions are discarded and canvases redraw), and wake sessions. The
    /// disk cache is the fetcher's business — this clears memory only.
    ///
    /// # Panics
    /// If the store's writer mutex is poisoned (a writer panicked mid-swap).
    pub fn clear(&self) {
        let old = {
            let _writer = self.inner.writer.lock().expect("writer lock");
            self.inner.flush_generation.fetch_add(1, Ordering::AcqRel);
            let old = self.inner.map.load_full();
            self.inner.map.store(Arc::new(EntryMap::new()));
            self.inner.total_ready_bytes.store(0, Ordering::Relaxed);
            self.inner.held_bytes.store(0, Ordering::Relaxed);
            old
        };
        for cell in old.values() {
            cell.evicted.store(true, Ordering::Relaxed);
            if let Some(abort) = cell.abort.lock().expect("abort lock").take() {
                abort.abort();
            }
        }
        self.inner
            .completion_generation
            .fetch_add(1, Ordering::Release);
        self.poke_all();
    }
}

/// One hold on a store entry ([`ImageStore::hold`]), released when dropped.
pub struct EntryHold {
    store: ImageStore,
    key: String,
    /// Weak: the map keeps the cell alive while it matters, and a cell that is gone
    /// altogether has nothing to release.
    cell: Weak<ImageEntryCell>,
}

impl EntryHold {
    /// Whether this is a hold on `cell`.
    #[must_use]
    pub fn is_on(&self, cell: &Arc<ImageEntryCell>) -> bool {
        Weak::ptr_eq(&self.cell, &Arc::downgrade(cell))
    }
}

impl Drop for EntryHold {
    fn drop(&mut self) {
        let store = self.store.clone();
        let key = std::mem::take(&mut self.key);
        let cell = std::mem::take(&mut self.cell);
        self.store.runtime_handle().spawn(async move {
            store.release(&key, &cell);
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A store over an immediate 2×2 mock fetcher, for other modules' tests (canvas).
    pub(crate) fn test_store() -> (ImageStore, Arc<MockFetcher>) {
        let fetcher = MockFetcher::ok(2, 2);
        (ImageStore::new(fetcher.clone()), fetcher)
    }

    /// A fetcher that resolves immediately (or once `gate` releases it), counting calls.
    pub(crate) struct MockFetcher {
        pub(crate) calls: AtomicUsize,
        result: Mutex<Result<(u32, u32), FetchError>>,
        /// When present, fetches wait for a permit before resolving.
        gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl MockFetcher {
        pub(crate) fn ok(width: u32, height: u32) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                result: Mutex::new(Ok((width, height))),
                gate: None,
            })
        }

        fn gated(width: u32, height: u32) -> (Arc<Self>, Arc<tokio::sync::Semaphore>) {
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let fetcher = Arc::new(Self {
                calls: AtomicUsize::new(0),
                result: Mutex::new(Ok((width, height))),
                gate: Some(gate.clone()),
            });
            (fetcher, gate)
        }
    }

    impl ImageFetcher for MockFetcher {
        fn fetch(
            &self,
            _source: ResolvedImageSource,
            _policy: Arc<ImageSourcePolicy>,
        ) -> BoxFut<Result<DecodedImage, FetchError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result = self.result.lock().unwrap().clone();
            let gate = self.gate.clone();
            Box::pin(async move {
                if let Some(gate) = gate {
                    let _permit = gate.acquire().await.expect("gate open");
                }
                result.map(|(width, height)| DecodedImage {
                    width,
                    height,
                    rgba: vec![0u8; (width * height * 4) as usize],
                    file_stamp: None,
                })
            })
        }

        fn probe(
            &self,
            _source: ResolvedImageSource,
            _policy: Arc<ImageSourcePolicy>,
        ) -> BoxFut<Option<FileStamp>> {
            Box::pin(async { None })
        }
    }

    fn src(name: &str) -> ResolvedImageSource {
        ResolvedImageSource::PackageAsset {
            owner: "o".into(),
            name: "p".into(),
            version: "1".into(),
            subpath: name.into(),
        }
    }

    pub(crate) fn test_policy() -> Arc<ImageSourcePolicy> {
        Arc::new(ImageSourcePolicy {
            trusted: true,
            server_name: Arc::from("s"),
            hosted_packages: smudgy_cloud::image_source::HostedPackages::default(),
            net_grants: smudgy_cloud::image_source::NetGrants::default(),
            read_grants: Vec::new(),
            modules_root: std::path::PathBuf::from("/m"),
            packages_root: std::path::PathBuf::from("/p"),
        })
    }

    fn wait_ready(cell: &ImageEntryCell) -> Arc<EntryState> {
        for _ in 0..500 {
            let state = cell.state();
            if !matches!(&*state, EntryState::Loading) {
                return state;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("entry never left Loading");
    }

    /// Wait until the completion generation passes `after`. `wait_ready` observes the
    /// lock-free state swap, which lands *before* byte accounting inside `complete()`'s
    /// writer scope; the generation bump is a `Release` increment after both, so once it
    /// is visible the accounting is too.
    fn wait_generation(store: &ImageStore, after: u64) {
        for _ in 0..500 {
            if store.completion_generation() > after {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("completion generation never advanced past {after}");
    }

    #[test]
    fn ensure_dedups_and_completes() {
        let fetcher = MockFetcher::ok(2, 2);
        let store = ImageStore::new(fetcher.clone());
        let server = test_policy();
        let a = store.ensure(&src("x.png"), &server);
        let b = store.ensure(&src("x.png"), &server);
        assert!(Arc::ptr_eq(&a, &b), "same source shares one cell");
        let state = wait_ready(&a);
        match &*state {
            EntryState::Ready {
                width,
                height,
                decoded_bytes,
                ..
            } => {
                assert_eq!((*width, *height), (2, 2));
                assert_eq!(*decoded_bytes, 16);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1, "in-flight dedup");
        wait_generation(&store, 0);
        assert_eq!(store.ready_bytes(), 16);
    }

    #[test]
    fn failures_are_sticky_until_ttl() {
        let fetcher = MockFetcher::ok(1, 1);
        *fetcher.result.lock().unwrap() = Err(FetchError::transient("nope"));
        let store = ImageStore::new(fetcher.clone());
        let server = test_policy();
        let cell = store.ensure(&src("bad.png"), &server);
        let state = wait_ready(&cell);
        assert!(
            matches!(
                &*state,
                EntryState::Failed {
                    retry_at: Some(_),
                    ..
                }
            ),
            "transient failures carry a retry deadline"
        );
        // Re-ensure within the TTL is side-effect-free.
        let _ = store.ensure(&src("bad.png"), &server);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1, "no retry storm");
    }

    #[test]
    fn permanent_failures_never_retry() {
        let fetcher = MockFetcher::ok(1, 1);
        *fetcher.result.lock().unwrap() = Err(FetchError::permanent("svg rejected"));
        let store = ImageStore::new(fetcher.clone());
        let server = test_policy();
        let cell = store.ensure(&src("no.svg"), &server);
        let state = wait_ready(&cell);
        assert!(
            matches!(&*state, EntryState::Failed { retry_at: None, .. }),
            "permanent failures have no retry deadline"
        );
    }

    #[test]
    fn clear_mid_flight_never_accounts_the_late_completion() {
        let (fetcher, gate) = MockFetcher::gated(64, 64);
        let store = ImageStore::new(fetcher);
        let server = test_policy();
        let cell = store.ensure(&src("late.png"), &server);
        // Flush while the fetch is parked on the gate, then release it.
        store.clear();
        gate.add_permits(1);
        // The late completion must be discarded: no resurrected entry, no phantom bytes.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            store.ready_bytes(),
            0,
            "late completion accounted after clear"
        );
        assert!(cell.is_evicted());
        assert!(store.inner.map.load().is_empty(), "no resurrected entry");
    }

    #[test]
    fn clear_discards_and_bumps_generations() {
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher);
        let server = test_policy();
        let cell = store.ensure(&src("y.png"), &server);
        wait_ready(&cell);
        let generation = store.completion_generation();
        store.clear();
        assert!(cell.is_evicted());
        assert_eq!(store.ready_bytes(), 0);
        assert!(store.completion_generation() > generation);
        // A fresh ensure after clear starts a new fetch on a new cell.
        let fresh = store.ensure(&src("y.png"), &server);
        assert!(!Arc::ptr_eq(&cell, &fresh));
    }

    fn zeroed_rgba(width: u32, height: u32) -> bytes::Bytes {
        bytes::Bytes::from(vec![0u8; (width * height * 4) as usize])
    }

    fn ready_handle_id(cell: &ImageEntryCell) -> iced::advanced::image::Id {
        match &*cell.state() {
            EntryState::Ready { handle, .. } => handle.id(),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn ensure_rgba_inserts_a_ready_cell_and_accounts_its_bytes() {
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher.clone());
        let generation = store.completion_generation();
        let cell = store.ensure_rgba("kitty:1", 2, 3, zeroed_rgba(2, 3));
        match &*cell.state() {
            EntryState::Ready {
                width,
                height,
                decoded_bytes,
                file_stamp,
                ..
            } => {
                assert_eq!((*width, *height, *decoded_bytes), (2, 3, 24));
                assert!(
                    file_stamp.is_none(),
                    "no freshness probe for decoded pixels"
                );
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        assert_eq!(store.ready_bytes(), 24);
        assert_eq!(
            store.completion_generation(),
            generation,
            "no canvas draws it: nothing to invalidate"
        );
        assert!(Arc::ptr_eq(&store.peek("kitty:1").unwrap(), &cell));
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "nothing fetched");
    }

    #[test]
    fn ensure_rgba_returns_the_existing_cell_without_reminting() {
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher.clone());
        let cell = store.ensure_rgba("kitty:7", 2, 2, zeroed_rgba(2, 2));
        let handle = ready_handle_id(&cell);
        let generation = store.completion_generation();
        let again = store.ensure_rgba("kitty:7", 2, 2, zeroed_rgba(2, 2));
        assert!(Arc::ptr_eq(&cell, &again), "one cell per key");
        assert_eq!(
            ready_handle_id(&again),
            handle,
            "the handle is not re-minted"
        );
        assert_eq!(store.ready_bytes(), 16, "bytes are accounted once");
        assert_eq!(store.completion_generation(), generation);
        // Reaching the key through the fetching path changes nothing either: a Ready
        // entry without a file stamp is never refreshed.
        let keyed = store.ensure_keyed("kitty:7", &src("x.png"), &test_policy());
        assert!(Arc::ptr_eq(&cell, &keyed));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "nothing fetched");
    }

    #[test]
    fn ensure_rgba_after_clear_mints_a_fresh_cell() {
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher.clone());
        let cell = store.ensure_rgba("kitty:3", 2, 2, zeroed_rgba(2, 2));
        let handle = ready_handle_id(&cell);
        store.clear();
        assert!(cell.is_evicted());
        assert_eq!(store.ready_bytes(), 0);
        let fresh = store.ensure_rgba("kitty:3", 2, 2, zeroed_rgba(2, 2));
        assert!(!Arc::ptr_eq(&cell, &fresh));
        assert!(!fresh.is_evicted());
        assert_ne!(ready_handle_id(&fresh), handle, "a fresh handle");
        assert_eq!(store.ready_bytes(), 16);
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "nothing fetched");
    }

    #[test]
    fn remove_drops_an_entry_with_its_accounting_once() {
        let (store, _) = test_store();
        let kept = store.ensure_rgba("kitty:1", 2, 2, zeroed_rgba(2, 2));
        let cell = store.ensure_rgba("kitty:2", 2, 3, zeroed_rgba(2, 3));
        assert_eq!(store.ready_bytes(), 16 + 24);
        let generation = store.completion_generation();
        store.remove("kitty:2");
        assert!(cell.is_evicted(), "memo holders re-ensure on sight");
        assert!(store.peek("kitty:2").is_none());
        assert_eq!(store.ready_bytes(), 16, "its bytes leave the accounting");
        assert_eq!(store.completion_generation(), generation, "no bump");
        assert!(!kept.is_evicted() && store.peek("kitty:1").is_some());
        // Again, and for a key never held: nothing to do, nothing changes.
        store.remove("kitty:2");
        store.remove("kitty:never");
        assert_eq!(store.completion_generation(), generation);
        assert_eq!(store.ready_bytes(), 16);
        // The key is free for a fresh cell.
        let fresh = store.ensure_rgba("kitty:2", 2, 3, zeroed_rgba(2, 3));
        assert!(!Arc::ptr_eq(&cell, &fresh) && !fresh.is_evicted());
        assert_eq!(store.ready_bytes(), 16 + 24);
    }

    #[test]
    fn remove_aborts_a_fetch_in_flight_and_accounts_nothing_for_it() {
        let (fetcher, gate) = MockFetcher::gated(2, 2);
        let store = ImageStore::new(fetcher.clone());
        let cell = store.ensure(&src("slow.png"), &test_policy());
        assert!(matches!(&*cell.state(), EntryState::Loading));
        assert!(cell.abort.lock().unwrap().is_some(), "a fetch is in flight");
        let generation = store.completion_generation();
        let key = store
            .inner
            .map
            .load()
            .keys()
            .next()
            .cloned()
            .expect("the entry's key, as `ensure` derived it");
        store.remove(&key);
        assert!(store.inner.map.load().is_empty());
        assert!(cell.is_evicted());
        assert!(
            cell.abort.lock().unwrap().is_none(),
            "the fetch was aborted"
        );
        assert_eq!(store.completion_generation(), generation);
        // Releasing the gate completes nothing: the task is gone.
        gate.add_permits(1);
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(&*cell.state(), EntryState::Loading));
        assert_eq!(store.ready_bytes(), 0);
    }

    #[test]
    fn ensure_rgba_over_the_cap_evicts_the_coldest_and_the_key_re_ensures() {
        // Three 49 MiB images cross the 128 MiB cap. A zeroed `vec!` of this size is a
        // fresh mapping that neither the store nor the handle ever touches, so the test
        // costs address space, not memory.
        const SIDE: u32 = 3584;
        let bytes = u64::from(SIDE) * u64::from(SIDE) * 4;
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher.clone());
        // Distinct millisecond stamps make "coldest" unambiguous. `is_evicted` reads
        // without touching, so the checks below leave the stamps alone.
        let first = store.ensure_rgba("kitty:1", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        std::thread::sleep(Duration::from_millis(3));
        let second = store.ensure_rgba("kitty:2", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        std::thread::sleep(Duration::from_millis(3));
        assert!(!first.is_evicted() && !second.is_evicted(), "under the cap");
        let third = store.ensure_rgba("kitty:3", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        assert!(first.is_evicted(), "the coldest entry goes first");
        assert!(!second.is_evicted() && !third.is_evicted());
        assert!(store.peek("kitty:1").is_none());
        assert_eq!(store.ready_bytes(), 2 * bytes);

        // The evicted key comes back as a fresh Ready cell, which in turn pushes out
        // what is now the coldest.
        std::thread::sleep(Duration::from_millis(3));
        let again = store.ensure_rgba("kitty:1", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        assert!(!Arc::ptr_eq(&first, &again));
        assert!(!again.is_evicted());
        assert!(matches!(&*again.state(), EntryState::Ready { .. }));
        assert!(second.is_evicted());
        assert!(!third.is_evicted());
        assert_eq!(store.ready_bytes(), 2 * bytes);
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "nothing fetched");
    }

    #[test]
    fn wakers_fire_on_completion_and_prune_dead() {
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher);
        let server = test_policy();
        let tx = Arc::new(watch::channel(()).0);
        let rx = tx.subscribe();
        store.register_waker(Arc::downgrade(&tx));
        let dead = Arc::new(watch::channel(()).0);
        store.register_waker(Arc::downgrade(&dead));
        drop(dead);
        let cell = store.ensure(&src("w.png"), &server);
        wait_ready(&cell);
        // `poke_all` runs after the writer scope that publishes the state, so the poke
        // gets the same bounded grace as the state itself.
        for _ in 0..500 {
            if rx.has_changed().unwrap() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("completion never poked the live waker");
    }

    /// Poll until `done`, for up to a couple of seconds.
    pub(crate) fn wait_until(what: &str, done: impl Fn() -> bool) {
        for _ in 0..1000 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("{what} never happened");
    }

    #[test]
    fn a_dropped_hold_removes_its_entry_on_the_store_runtime() {
        let (store, _) = test_store();
        let cell = store.ensure_rgba("kitty:held", 2, 2, zeroed_rgba(2, 2));
        let hold = store.hold("kitty:held", &cell);
        assert!(hold.is_on(&cell));
        assert_eq!((store.ready_bytes(), store.held_bytes()), (16, 16));
        drop(hold);
        wait_until("the entry's removal", || store.peek("kitty:held").is_none());
        wait_until("the cell's eviction", || cell.is_evicted());
        wait_until("the bytes released", || store.ready_bytes() == 0);
        assert_eq!(store.held_bytes(), 0);
        assert!(!cell.is_held(), "the hold is gone with the entry");
    }

    #[test]
    fn the_last_hold_removes_the_entry_and_an_earlier_release_does_not() {
        let (store, _) = test_store();
        let cell = store.ensure_rgba("kitty:twice", 2, 2, zeroed_rgba(2, 2));
        let first = store.hold("kitty:twice", &cell);
        let second = store.hold("kitty:twice", &cell);
        assert_eq!(store.held_bytes(), 16, "counted once however many holds");
        // The releases as the dropped holds queue them, run here in order.
        let weak = Arc::downgrade(&cell);
        store.release("kitty:twice", &weak);
        assert!(
            store.peek("kitty:twice").is_some() && !cell.is_evicted(),
            "one hold remains"
        );
        assert_eq!(store.held_bytes(), 16);
        store.release("kitty:twice", &weak);
        assert!(store.peek("kitty:twice").is_none() && cell.is_evicted());
        assert_eq!((store.ready_bytes(), store.held_bytes()), (0, 0));
        // Their drops release nothing more: the cell is not the map's any more.
        let again = store.ensure_rgba("kitty:twice", 2, 2, zeroed_rgba(2, 2));
        drop((first, second));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!again.is_evicted() && store.peek("kitty:twice").is_some());
    }

    #[test]
    fn a_release_and_remove_if_leave_a_fresh_entry_under_the_key_alone() {
        // The entry a hold was taken on was dropped and the key minted again: the
        // hold's release, run late, must not take the new entry, and neither must an
        // identity-aware removal naming the old cell.
        let (store, _) = test_store();
        let old = store.ensure_rgba("kitty:k", 2, 2, zeroed_rgba(2, 2));
        let hold = store.hold("kitty:k", &old);
        store.remove("kitty:k");
        assert!(old.is_evicted());
        assert_eq!((store.ready_bytes(), store.held_bytes()), (0, 0));
        let fresh = store.ensure_rgba("kitty:k", 2, 2, zeroed_rgba(2, 2));
        assert!(!Arc::ptr_eq(&old, &fresh));
        store.release("kitty:k", &Arc::downgrade(&old));
        store.remove_if("kitty:k", &old);
        assert!(
            !fresh.is_evicted() && Arc::ptr_eq(&store.peek("kitty:k").unwrap(), &fresh),
            "the fresh entry stays"
        );
        assert_eq!(store.ready_bytes(), 16);
        std::mem::forget(hold);
        // Naming the current cell removes it, hold or no hold.
        let fresh_hold = store.hold("kitty:k", &fresh);
        store.remove_if("kitty:k", &fresh);
        assert!(fresh.is_evicted() && store.peek("kitty:k").is_none());
        assert_eq!((store.ready_bytes(), store.held_bytes()), (0, 0));
        drop(fresh_hold);
    }

    #[test]
    fn held_entries_are_outside_the_cap_and_never_swept() {
        // Two held 49 MiB entries pass the 128 MiB cap on their own; what the cap counts
        // is the rest. Zeroed `vec!`s of this size are fresh mappings nothing touches,
        // so the test costs address space, not memory.
        const SIDE: u32 = 3584;
        let bytes = u64::from(SIDE) * u64::from(SIDE) * 4;
        let fetcher = MockFetcher::ok(1, 1);
        let store = ImageStore::new(fetcher);
        let mut pinned: Vec<_> = ["kitty:h1", "kitty:h2"]
            .into_iter()
            .map(|key| {
                let cell = store.ensure_rgba(key, SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
                (cell.clone(), store.hold(key, &cell))
            })
            .collect();
        assert_eq!(store.held_bytes(), 2 * bytes);
        assert!(pinned.iter().all(|(cell, _)| !cell.is_evicted()));
        // An unheld one on top: 147 MiB in all, 49 MiB to the cap. Nothing goes.
        let cold = store.ensure_rgba("kitty:cold", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        assert!(!cold.is_evicted() && pinned.iter().all(|(cell, _)| !cell.is_evicted()));
        assert_eq!(store.ready_bytes(), 3 * bytes);
        std::thread::sleep(Duration::from_millis(3));
        // Two more unheld: the unheld bytes cross the cap and the coldest unheld entry
        // goes, while the held ones, colder still, stay.
        let warm = store.ensure_rgba("kitty:warm", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        std::thread::sleep(Duration::from_millis(3));
        let newest = store.ensure_rgba("kitty:new", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        assert!(cold.is_evicted(), "the coldest unheld entry");
        assert!(!warm.is_evicted() && !newest.is_evicted());
        assert!(
            pinned.iter().all(|(cell, _)| !cell.is_evicted()),
            "held: kept"
        );
        assert_eq!(store.ready_bytes(), 4 * bytes);
        assert_eq!(store.held_bytes(), 2 * bytes);
        // Releasing one hold removes its entry and its bytes from both counts; the other
        // stays held (its pair stays in `pinned`).
        let (gone, release) = pinned.remove(0);
        drop(release);
        wait_until("the held entry's removal", || gone.is_evicted());
        assert_eq!(store.ready_bytes(), 3 * bytes);
        assert_eq!(store.held_bytes(), bytes);
    }

    #[test]
    fn an_entry_held_as_it_is_inserted_sweeps_nothing_that_fit_beside_it() {
        // Two unheld 49 MiB entries sit under the 128 MiB cap. A 49 MiB entry inserted
        // for a holder takes the total past the cap, but its bytes are the holder's:
        // held as it is inserted, it leaves both alone. (Inserted first and held after,
        // the sweep between would have taken the colder one.)
        const SIDE: u32 = 3584;
        let bytes = u64::from(SIDE) * u64::from(SIDE) * 4;
        let store = ImageStore::new(MockFetcher::ok(1, 1));
        let first = store.ensure_rgba("fetched:1", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        std::thread::sleep(Duration::from_millis(3));
        let second = store.ensure_rgba("fetched:2", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        std::thread::sleep(Duration::from_millis(3));
        let (entry, hold) =
            store.ensure_rgba_held("kitty:held", SIDE, SIDE, zeroed_rgba(SIDE, SIDE));
        let hold = hold.expect("a hold on the entry it inserted");
        assert!(hold.is_on(&entry) && entry.is_held());
        assert!(!first.is_evicted() && !second.is_evicted(), "nothing swept");
        assert_eq!(store.ready_bytes(), 3 * bytes);
        assert_eq!(store.held_bytes(), bytes);
        // Ensured again, held or not, the entry comes back as it is, with no new hold.
        let (again, none) = store.ensure_rgba_held("kitty:held", SIDE, SIDE, zeroed_rgba(1, 1));
        assert!(Arc::ptr_eq(&again, &entry) && none.is_none());
        assert!(Arc::ptr_eq(
            &store.ensure_rgba("kitty:held", SIDE, SIDE, zeroed_rgba(1, 1)),
            &entry
        ));
        assert_eq!(store.held_bytes(), bytes, "counted once");
        // Its release removes it like any hold's.
        drop(hold);
        wait_until("the held entry's removal", || entry.is_evicted());
        assert_eq!((store.ready_bytes(), store.held_bytes()), (2 * bytes, 0));
    }

    #[test]
    fn a_buffer_the_size_disagrees_with_is_never_uploaded() {
        let (store, _) = test_store();
        let short = store.ensure_rgba("kitty:short", 2, 2, bytes::Bytes::from_static(&[0; 8]));
        assert!(matches!(&*short.state(), EntryState::Failed { .. }));
        assert!(store.peek("kitty:short").is_none(), "nothing is inserted");
        assert_eq!(store.ready_bytes(), 0);
        let (long, hold) =
            store.ensure_rgba_held("kitty:short", 2, 2, bytes::Bytes::from_static(&[0; 20]));
        assert!(matches!(&*long.state(), EntryState::Failed { .. }) && hold.is_none());
        // The right pixels take the key.
        let right = store.ensure_rgba("kitty:short", 2, 2, zeroed_rgba(2, 2));
        assert!(matches!(&*right.state(), EntryState::Ready { .. }));
        assert_eq!(store.ready_bytes(), 16);
    }

    #[test]
    fn held_entries_leave_the_completion_generation_alone() {
        let (store, _) = test_store();
        let generation = store.completion_generation();
        let cell = store.ensure_rgba("kitty:g", 2, 2, zeroed_rgba(2, 2));
        let hold = store.hold("kitty:g", &cell);
        store.remove_if("kitty:g", &cell);
        store.release("kitty:g", &Arc::downgrade(&cell));
        std::mem::forget(hold);
        store.remove("kitty:g");
        assert_eq!(
            store.completion_generation(),
            generation,
            "minting, holding and removing bump nothing"
        );
        store.clear();
        assert!(store.completion_generation() > generation, "a clear does");
    }
}
