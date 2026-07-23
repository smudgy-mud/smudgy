//! Live-session harness for the benches that drive a REAL spawned session —
//! V8 isolates, module loading, trigger dispatch, the works — rather than a
//! `Manager` or `SessionStore` in isolation. Factored from the shape
//! `benches/script_dispatch.rs` proved out (that bench keeps its own copy so
//! its long-lived baselines stay bit-for-bit comparable); the generalizations
//! here are the ones the interop/churn benches need:
//!
//! - **any barrier marker**, not a hardcoded `ZZDONE`: interop passes often
//!   complete on a count-based echo from a JS callback (e.g. a subscriber
//!   that echoes after the K×S-th delivery), not on a barrier line's own
//!   trigger;
//! - **sandboxed packages**: a session can carry installed-untrusted packages
//!   (each in its own isolate) built through the same in-memory
//!   provider + consent-record path the core integration tests use
//!   (`session_store_isolates.rs`), so package-producer cells run the real
//!   per-package-isolate machinery;
//! - **transcript collection** for warmup/sanity, separated from the timed
//!   drain (which only scans for the marker).
//!
//! Sessions are intentionally never shut down: they idle between groups and
//! process exit reaps their threads, sidestepping engine-teardown races after
//! the numbers are in (the `script_dispatch` pattern). The smudgy home is a
//! leaked process-global tempdir for the same reason — session threads keep
//! log files open under it.

use std::{
    fs,
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use futures::{FutureExt, Stream, StreamExt};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    TaggedSessionEvent, runtime::RuntimeAction, spawn, spawn_with_package_provider,
    styled_line::StyledLine,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage,
};

/// Generous ceilings so a wedged session panics instead of hanging criterion.
const READY_TIMEOUT: Duration = Duration::from_mins(2);
const DRAIN_TIMEOUT: Duration = Duration::from_mins(1);

/// The process-global hermetic smudgy home (first caller creates it; every
/// later session reuses it). Leaked on purpose — see the module doc.
pub fn hermetic_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::tempdir().expect("create temp smudgy home");
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        smudgy_core::set_smudgy_home(&path);
        path
    })
}

/// Create `<home>/<server>/{modules,logs}` and write each `(name, source)`
/// under `modules/`. Local modules run in the MAIN isolate, allow-all.
pub fn prepare_server(server: &str, modules: &[(&str, String)]) {
    let server_dir = hermetic_home().join(server);
    fs::create_dir_all(server_dir.join("modules")).expect("create modules dir");
    fs::create_dir_all(server_dir.join("logs")).expect("create logs dir");
    for (name, source) in modules {
        fs::write(server_dir.join("modules").join(name), source).expect("write bench module");
    }
}

/// One installed-untrusted package for a bench session: its resolvable
/// source plus the consent record that grants its capabilities. Installed
/// under the session's server before spawn, so the loader gives it its own
/// sandboxed isolate with exactly these grants — the real package topology.
pub struct BenchPackage {
    pub owner: &'static str,
    pub name: &'static str,
    pub source: String,
    pub consent: PackagePermissions,
}

impl BenchPackage {
    fn spec(&self) -> String {
        format!("smudgy://{}/{}", self.owner, self.name)
    }

    fn resolved(&self) -> ResolvedPackage {
        let manifest_json = format!(r#"{{ "name": "{}", "version": "1.0.0" }}"#, self.name);
        ResolvedPackage {
            key: PackageKey {
                owner: self.owner.to_string(),
                name: self.name.to_string(),
            },
            resolved_version: "1.0.0".to_string(),
            manifest: PackageManifest::parse(&manifest_json).expect("valid manifest"),
            integrity: format!("bench-{}-{}", self.owner, self.name),
            modules: vec![PackageModuleSource {
                subpath: "index.js".to_string(),
                text: self.source.clone(),
            }],
        }
    }
}

/// A live spawned session plus the drain-side machinery a timed pass needs.
pub struct BenchSession {
    events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>,
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
}

impl BenchSession {
    /// Prepare the server dir, install + consent any packages, spawn, and
    /// block until the runtime hands back its action sender. `modules` are
    /// main-isolate local modules; `packages` get their own sandboxes.
    pub fn start(
        rt: &tokio::runtime::Runtime,
        server: &str,
        session_id: u32,
        modules: &[(&str, String)],
        packages: &[BenchPackage],
    ) -> Self {
        prepare_server(server, modules);
        for pkg in packages {
            shared_packages::install_package(server, &pkg.spec(), UpdateMode::Auto, true)
                .expect("install bench package");
            shared_packages::record_consent(server, &pkg.spec(), &pkg.consent)
                .expect("record bench package consent");
        }

        let params = Arc::new(SessionParams {
            session_id: SessionId::from(session_id),
            server_name: Arc::new(server.to_string()),
            profile_name: Arc::new("Bench".to_string()),
            profile_subtext: Arc::new(String::new()),
            mapper: None,
            package_client: None,
            extra_script_extensions: Arc::new(Vec::new),
            on_engine_rebuild: None,
        });

        let mut events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = if packages.is_empty() {
            Box::pin(spawn(params))
        } else {
            let resolved: Vec<ResolvedPackage> =
                packages.iter().map(BenchPackage::resolved).collect();
            let factory: PackageProviderFactory = Arc::new(move || {
                let mut provider = InMemoryPackageProvider::new();
                for pkg in &resolved {
                    provider.insert(pkg.clone());
                }
                let provider: Rc<dyn PackageProvider> = Rc::new(provider);
                provider
            });
            Box::pin(spawn_with_package_provider(params, factory))
        };

        let tx = rt.block_on(async {
            loop {
                let event = tokio::time::timeout(READY_TIMEOUT, events.next())
                    .await
                    .expect("timed out waiting for RuntimeReady")
                    .expect("session event stream ended before RuntimeReady");
                if let SessionEvent::RuntimeReady(tx) = event.event {
                    break tx;
                }
            }
        });

        Self { events, tx }
    }

    /// Queue one already-built line; each send costs one `Arc` bump.
    pub fn feed(&self, line: &Arc<StyledLine>) {
        self.tx
            .send(RuntimeAction::HandleIncomingLine(line.clone()))
            .expect("session runtime channel closed");
    }

    /// Drain until a displayed line's text CONTAINS `marker`, collecting every
    /// appended line (notices included). Returns `false` on timeout so the
    /// caller owns the panic and can print the transcript. Setup/sanity side —
    /// the timed drain is [`drain_until`](Self::drain_until).
    pub async fn drain_collect_until(&mut self, marker: &str, texts: &mut Vec<String>) -> bool {
        let events = &mut self.events;
        tokio::time::timeout(DRAIN_TIMEOUT, async {
            loop {
                let event = events.next().await.expect("session event stream ended");
                let mut done = false;
                if let SessionEvent::UpdateBuffer(updates) = &event.event {
                    for update in updates.as_slice() {
                        if let BufferUpdate::Append(line) = update {
                            done |= line.text.contains(marker);
                            texts.push(line.text.clone());
                        }
                    }
                }
                if done {
                    break;
                }
            }
        })
        .await
        .is_ok()
    }

    /// The timed drain: consume events until a displayed line's text contains
    /// `marker`. One timeout spans the whole drain, so the timed window pays a
    /// single timer registration.
    pub async fn drain_until(&mut self, marker: &str) {
        let events = &mut self.events;
        tokio::time::timeout(DRAIN_TIMEOUT, async {
            loop {
                let event = events.next().await.expect("session event stream ended");
                let mut done = false;
                if let SessionEvent::UpdateBuffer(updates) = &event.event {
                    for update in updates.as_slice() {
                        if let BufferUpdate::Append(line) = update {
                            done |= line.text.contains(marker);
                        }
                    }
                }
                std::hint::black_box(&event);
                if done {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out draining to the {marker} marker"));
    }

    /// One timed pass: feed every line, then drain to `marker`. The lines
    /// were built in setup; the completion echo (a barrier trigger's, or a
    /// count-based one from a JS callback) proves the pass's work finished
    /// because actions dispatch in order with depth-first expansion.
    pub async fn timed_pass(&mut self, lines: &[Arc<StyledLine>], marker: &str) -> Duration {
        let start = Instant::now();
        for line in lines {
            self.feed(line);
        }
        self.drain_until(marker).await;
        start.elapsed()
    }

    /// Non-blocking sweep of anything still queued (a barrier line's own
    /// display can trail its echo), so backlog never bleeds into the next
    /// timed pass.
    pub fn drain_stragglers(&mut self) {
        loop {
            match self.events.next().now_or_never() {
                Some(Some(event)) => {
                    std::hint::black_box(&event);
                }
                Some(None) => panic!("session event stream ended"),
                None => break,
            }
        }
    }
}

/// A single-threaded tokio runtime for driving sessions from bench code.
#[must_use]
pub fn bench_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

/// Build one styled line from text (no spans).
#[must_use]
pub fn styled(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}
