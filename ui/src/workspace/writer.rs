//! The single-writer persistence worker and the process-global snapshot
//! cell.
//!
//! Every serialized last-session snapshot is **published** into the cell
//! with its destination path and a monotonically increasing generation; a
//! dedicated worker thread drains the cell to disk through the canonical
//! atomic-replace. The cell always holds the newest *completed*
//! serialization per destination, so three consumers share one truth:
//!
//! - the debounce/checkpoint serializer publishes (and never touches disk);
//! - the worker writes every destination disk is behind on, off the update
//!   thread;
//! - the Windows session-end hook ([`end_session_flush`]) writes the cell
//!   synchronously when the event loop can no longer be serviced.
//!
//! Destinations include window preferences and per-server `last-session.json` files. Only one
//! server's file updates per snapshot (the one owning the active session),
//! but a switch of active server mid-flight must not lose the previous
//! server's pending write — hence per-destination cells rather than one.
//!
//! Ordering is enforced with two locks: `state` (fast — bytes and
//! generations) and `write_lock`, held across every disk write. A write
//! always re-checks its destination's newest generation under `write_lock`
//! and skips when disk is already current, so an older generation can never
//! overwrite a newer one, however requests interleave. The session-end hook
//! takes the same `write_lock` across its check-and-write, so it can
//! neither tear an in-flight background write nor race one with older
//! bytes.
//!
//! One content rule backs all of it: an **empty** snapshot (no sessions, no
//! windows) never replaces a non-empty file. A footprint capture with
//! nothing in it is never published at all — the pipeline skips instead —
//! so an empty serialization reaching a file that holds an arrangement can
//! only be a desync or teardown artifact, and the snapshot on disk outranks
//! it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use smudgy_core::models::persistence::write_atomic;

/// Completion acknowledgement for one published snapshot: sent once the
/// worker has drained everything pending (the snapshot's destination
/// included). The quit path is the one consumer — the documented place a
/// fire-and-forget write is replaced by an awaited one.
pub type Ack = tokio::sync::oneshot::Sender<()>;

/// The longest the quit path waits for the final flush acknowledgement.
/// Generous against a slow disk; small against a user watching an app that
/// will not exit.
pub const QUIT_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Await one flush acknowledgement, bounded by `limit`. The quit path must
/// never hang on a wedged write: past the limit the wait is abandoned with
/// a warning and shutdown proceeds. The last *completed* write stands
/// either way — the atomic replace cannot tear the file — so abandoning the
/// wait costs at most the final snapshot's delta. A dropped sender (the
/// writer is gone) resolves the wait the same as an acknowledgement:
/// waiting longer cannot help.
pub async fn await_ack_bounded(
    done: tokio::sync::oneshot::Receiver<()>,
    limit: std::time::Duration,
) {
    if tokio::time::timeout(limit, done).await.is_err() {
        log::warn!(
            "[workspace] the final flush did not acknowledge within {limit:?}; exiting with the last completed write"
        );
    }
}

/// The process-global writer, created once at startup ([`init_global`]).
static WRITER: OnceLock<Arc<Writer>> = OnceLock::new();

/// The snapshot cell plus its worker's request channel.
pub struct Writer {
    /// Held for the duration of every disk write. Serializes the worker and
    /// the session-end hook against each other and orders generations onto
    /// disk.
    write_lock: Mutex<()>,
    /// The latest completed serializations and the write bookkeeping.
    state: Mutex<State>,
    /// Wakes the worker; each request carries an optional completion ack.
    requests: mpsc::Sender<Option<Ack>>,
}

struct State {
    /// The newest serialized snapshot awaiting write, per destination,
    /// exactly as it should appear on disk.
    pending: HashMap<PathBuf, (Arc<[u8]>, u64)>,
    /// The generation each destination file currently holds (or has settled
    /// as handled).
    written: HashMap<PathBuf, u64>,
}

impl State {
    /// The newest generation known for `path`, pending or written.
    fn newest_generation(&self, path: &Path) -> u64 {
        let pending = self
            .pending
            .get(path)
            .map_or(0, |&(_, generation)| generation);
        let written = self.written.get(path).copied().unwrap_or(0);
        pending.max(written)
    }
}

/// Recover a poisoned lock: a panic elsewhere must never wedge the shutdown
/// flush, and the cell's state (bytes + integers) is valid at every point a
/// panic could interrupt it.
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether serialized workspace bytes describe the empty workspace: no
/// session slots and no windows. Unparseable bytes count as non-empty — the
/// refusal below must never block a real snapshot, only a recognizably
/// empty one.
fn snapshot_is_empty(bytes: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Probe {
        sessions: Vec<serde::de::IgnoredAny>,
        windows: Vec<serde::de::IgnoredAny>,
    }
    serde_json::from_slice::<Probe>(bytes)
        .is_ok_and(|probe| probe.sessions.is_empty() && probe.windows.is_empty())
}

/// Whether the destination file currently holds a workspace with content
/// worth protecting. A missing, unreadable, or already-empty file has
/// nothing to lose; unrecognizable content is conservatively protected.
/// Read only on the rare empty-snapshot path, under the write lock, never
/// per ordinary write.
fn on_disk_holds_content(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| !snapshot_is_empty(&bytes))
}

impl Writer {
    /// Create a writer and start its worker thread.
    pub fn new() -> Arc<Self> {
        let (requests, receiver) = mpsc::channel::<Option<Ack>>();
        let writer = Arc::new(Self {
            write_lock: Mutex::new(()),
            state: Mutex::new(State {
                pending: HashMap::new(),
                written: HashMap::new(),
            }),
            requests,
        });
        let worker = Arc::clone(&writer);
        std::thread::Builder::new()
            .name("workspace-writer".to_string())
            .spawn(move || {
                while let Ok(ack) = receiver.recv() {
                    worker.write_newest();
                    if let Some(ack) = ack {
                        // The receiver having gone away just means the quit
                        // path stopped waiting; the write already happened.
                        let _ = ack.send(());
                    }
                }
            })
            .expect("spawning the workspace writer thread");
        writer
    }

    /// Publish one serialized snapshot for `path`. `generation` must be
    /// newer than any previously published one (the mirror mints them
    /// monotonically); a stale publish is ignored, but its ack still
    /// resolves once the worker drains, so no waiter can hang on a lost
    /// race.
    pub fn publish(&self, generation: u64, path: PathBuf, bytes: Arc<[u8]>, ack: Option<Ack>) {
        {
            let mut state = lock_unpoisoned(&self.state);
            if generation > state.newest_generation(&path) {
                state.pending.insert(path, (bytes, generation));
            } else {
                log::warn!(
                    "[workspace] ignoring stale snapshot generation {generation} for {} (cell holds {})",
                    path.display(),
                    state.newest_generation(&path)
                );
            }
        }
        if self.requests.send(ack).is_err() {
            // The worker thread is gone (a startup failure); the cell still
            // holds the bytes, so the session-end hook remains able to
            // flush them.
            log::warn!("[workspace] the writer worker is not running; snapshot held in memory");
        }
    }

    /// Acknowledge only after all pending destinations have been drained.
    pub fn flush(&self, ack: Ack) {
        let _ = self.requests.send(Some(ack));
    }

    /// Publish without waking the worker — the snapshot lands in the cell
    /// only. Exercises the cell/flush interplay the way a crash between
    /// serialization and write would.
    #[cfg(test)]
    fn publish_unscheduled(&self, generation: u64, path: PathBuf, bytes: Arc<[u8]>) {
        let mut state = lock_unpoisoned(&self.state);
        if generation > state.newest_generation(&path) {
            state.pending.insert(path, (bytes, generation));
        }
    }

    /// Drain the cell: write every destination whose disk is behind.
    /// Returns whether any write happened. A failed write leaves its
    /// snapshot pending (unless a newer one superseded it meanwhile), so
    /// the next request (or the session-end flush) retries.
    ///
    /// An **empty** snapshot never replaces a non-empty file: it is refused
    /// with a warning and its generation is settled as handled — retrying
    /// could not change the answer, and a later non-empty snapshot
    /// supersedes it normally.
    fn write_newest(&self) -> bool {
        let _writing = lock_unpoisoned(&self.write_lock);
        let mut wrote = false;
        let mut failed: Vec<(PathBuf, Arc<[u8]>, u64)> = Vec::new();
        loop {
            let entry = {
                let mut state = lock_unpoisoned(&self.state);
                let path = state.pending.keys().next().cloned();
                path.map(|path| {
                    let (bytes, generation) = state
                        .pending
                        .remove(&path)
                        .expect("the key was read from the map itself");
                    (path, bytes, generation)
                })
            };
            let Some((path, bytes, generation)) = entry else {
                break;
            };
            if generation
                <= lock_unpoisoned(&self.state)
                    .written
                    .get(&path)
                    .copied()
                    .unwrap_or(0)
            {
                // A publish raced this drain with something disk already
                // holds; the publish guard makes this unreachable in
                // practice, and skipping is the safe answer regardless.
                continue;
            }
            if snapshot_is_empty(&bytes) && on_disk_holds_content(&path) {
                log::warn!(
                    "[workspace] refusing to overwrite the non-empty {} with an empty snapshot",
                    path.display()
                );
                lock_unpoisoned(&self.state)
                    .written
                    .insert(path, generation);
                continue;
            }
            match write_atomic(&path, &bytes) {
                Ok(()) => {
                    lock_unpoisoned(&self.state)
                        .written
                        .insert(path.clone(), generation);
                    log::info!(
                        "[workspace] wrote {} (generation {generation}, {} bytes)",
                        path.display(),
                        bytes.len()
                    );
                    wrote = true;
                }
                Err(err) => {
                    log::warn!("[workspace] failed to write {}: {err}", path.display());
                    failed.push((path, bytes, generation));
                }
            }
        }
        if !failed.is_empty() {
            // Failed snapshots go back into the cell for the next drain —
            // unless a newer publish already replaced them mid-write.
            let mut state = lock_unpoisoned(&self.state);
            for (path, bytes, generation) in failed {
                if generation > state.newest_generation(&path) {
                    state.pending.insert(path, (bytes, generation));
                }
            }
        }
        wrote
    }

    /// The synchronous flush for paths that cannot await the worker (the
    /// `WM_ENDSESSION` subclass proc). Holds the write lock across the whole
    /// check-and-write: an in-flight background write finishes first, and
    /// nothing older can land afterwards. When disk already holds every
    /// destination's newest generation this is a no-op — the last
    /// checkpoint stands.
    pub fn flush_now(&self) -> bool {
        self.write_newest()
    }
}

/// Create the process-global writer. Later calls are ignored (first wins),
/// mirroring the smudgy-home override discipline.
pub fn init_global() {
    if WRITER.set(Writer::new()).is_err() {
        log::warn!("[workspace] writer already initialized; ignoring later init");
    }
}

/// The process-global writer, if startup initialized one.
#[must_use]
pub fn global() -> Option<Arc<Writer>> {
    WRITER.get().cloned()
}

/// Synchronously flush the newest completed snapshots from the session-end
/// hook. Deliberately does nothing when no writer was ever initialized —
/// the hook must never allocate global state mid-shutdown.
pub fn end_session_flush() {
    if let Some(writer) = WRITER.get() {
        writer.flush_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, PathBuf, Arc<Writer>) {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("last-session.json");
        let writer = Writer::new();
        (dir, path, writer)
    }

    fn publish_and_wait(writer: &Writer, generation: u64, path: &Path, bytes: &[u8]) {
        let (ack, done) = tokio::sync::oneshot::channel();
        writer.publish(generation, path.to_path_buf(), Arc::from(bytes), Some(ack));
        done.blocking_recv().expect("worker acknowledges");
    }

    #[test]
    fn the_newest_complete_generation_wins() {
        let (_dir, path, writer) = setup();
        writer.publish(1, path.clone(), Arc::from(&b"one"[..]), None);
        publish_and_wait(&writer, 2, &path, b"two");
        assert_eq!(fs::read(&path).unwrap(), b"two");

        // A stale generation arriving late neither replaces the cell nor
        // reaches disk, and its waiter still resolves.
        publish_and_wait(&writer, 1, &path, b"stale");
        assert_eq!(fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn quit_flush_acknowledges_only_after_the_bytes_are_durable() {
        let (_dir, path, writer) = setup();
        publish_and_wait(&writer, 1, &path, b"final state");
        // The ack has resolved, so the bytes must already be on disk.
        assert_eq!(fs::read(&path).unwrap(), b"final state");
    }

    #[test]
    fn destinations_never_clobber_each_other() {
        // The regression the per-destination cell exists for: the active
        // server switching between two snapshots must not lose the first
        // server's pending write.
        let (dir, arctic, writer) = setup();
        let elephant = dir.path().join("elephant").join("last-session.json");
        fs::create_dir_all(elephant.parent().unwrap()).unwrap();
        writer.publish_unscheduled(1, arctic.clone(), Arc::from(&b"arctic arrangement"[..]));
        writer.publish_unscheduled(2, elephant.clone(), Arc::from(&b"elephant arrangement"[..]));
        assert!(writer.flush_now());
        assert_eq!(fs::read(&arctic).unwrap(), b"arctic arrangement");
        assert_eq!(fs::read(&elephant).unwrap(), b"elephant arrangement");
    }

    #[test]
    fn end_session_flush_writes_a_pending_snapshot_synchronously() {
        let (_dir, path, writer) = setup();
        publish_and_wait(&writer, 1, &path, b"checkpointed");

        // A newer serialization landed in the cell but its background write
        // never ran (forced shutdown): the synchronous flush writes it.
        writer.publish_unscheduled(2, path.clone(), Arc::from(&b"newer than disk"[..]));
        assert!(writer.flush_now());
        assert_eq!(fs::read(&path).unwrap(), b"newer than disk");

        // Disk is current: the flush is a no-op, not a rewrite.
        assert!(!writer.flush_now());
    }

    #[test]
    fn flush_with_nothing_published_is_a_no_op() {
        let (dir, path, writer) = setup();
        assert!(!writer.flush_now());
        assert!(!path.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_never_acking_flush_releases_the_waiter_at_the_bound() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let (ack, done) = tokio::sync::oneshot::channel::<()>();
        let limit = std::time::Duration::from_millis(50);
        let started = std::time::Instant::now();
        runtime.block_on(await_ack_bounded(done, limit));
        assert!(
            started.elapsed() >= limit,
            "the waiter held until the bound"
        );
        // Held un-sent across the whole wait: the release above can only
        // have been the timeout.
        drop(ack);
    }

    #[test]
    fn an_acknowledged_flush_releases_the_waiter_before_the_bound() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let (ack, done) = tokio::sync::oneshot::channel::<()>();
        ack.send(()).expect("receiver alive");
        let started = std::time::Instant::now();
        runtime.block_on(await_ack_bounded(done, std::time::Duration::from_secs(60)));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    /// A recognizably non-empty workspace serialization (one session slot).
    const FULL: &[u8] =
        br#"{"version":1,"sessions":[{"id":1,"server":"s","profile":"p","connect":false}],"windows":[]}"#;
    /// The empty workspace — a shape the snapshot pipeline never publishes
    /// (an empty footprint skips instead), so one arriving is an artifact.
    const EMPTY: &[u8] = br#"{"version":1,"sessions":[],"windows":[]}"#;

    #[test]
    fn an_empty_snapshot_never_replaces_a_non_empty_workspace() {
        let (_dir, path, writer) = setup();
        publish_and_wait(&writer, 1, &path, FULL);

        // The background write path refuses, resolves the ack, and settles
        // the generation rather than wedging on retries.
        publish_and_wait(&writer, 2, &path, EMPTY);
        assert_eq!(fs::read(&path).unwrap(), FULL);

        // The synchronous session-end flush obeys the same refusal.
        writer.publish_unscheduled(3, path.clone(), Arc::from(EMPTY));
        assert!(!writer.flush_now());
        assert_eq!(fs::read(&path).unwrap(), FULL);

        // A later real snapshot supersedes the refused one normally.
        let repopulated = br#"{"version":1,"sessions":[],"windows":[{"id":1}]}"#;
        publish_and_wait(&writer, 4, &path, repopulated);
        assert_eq!(fs::read(&path).unwrap(), repopulated);
    }

    #[test]
    fn preferences_are_not_mistaken_for_an_empty_workspace() {
        let (_dir, path, writer) = setup();
        publish_and_wait(&writer, 1, &path, br#"{"last_server":"Arctic"}"#);
        writer.publish_unscheduled(
            2,
            path.clone(),
            Arc::from(&br#"{"last_server":"Other"}"#[..]),
        );
        let (ack, done) = tokio::sync::oneshot::channel();
        writer.flush(ack);
        done.blocking_recv().unwrap();
        assert_eq!(fs::read(path).unwrap(), br#"{"last_server":"Other"}"#);
    }

    #[test]
    fn an_empty_snapshot_writes_freely_when_nothing_is_at_stake() {
        let (_dir, path, writer) = setup();

        // No file yet: a genuinely empty first write persists normally.
        publish_and_wait(&writer, 1, &path, EMPTY);
        assert_eq!(fs::read(&path).unwrap(), EMPTY);

        // Empty over empty is not a loss either.
        let empty_reformatted = br#"{ "version": 1, "sessions": [], "windows": [] }"#;
        publish_and_wait(&writer, 2, &path, empty_reformatted);
        assert_eq!(fs::read(&path).unwrap(), empty_reformatted);
    }

    #[test]
    fn a_stale_generation_can_never_overwrite_disk() {
        let (_dir, path, writer) = setup();
        publish_and_wait(&writer, 5, &path, b"five");
        // Even bypassing the publish guard, the write path itself refuses:
        // the cell's generation is not newer than what disk holds.
        writer.publish_unscheduled(4, path.clone(), Arc::from(&b"four"[..]));
        assert!(!writer.flush_now());
        assert_eq!(fs::read(&path).unwrap(), b"five");
    }
}
