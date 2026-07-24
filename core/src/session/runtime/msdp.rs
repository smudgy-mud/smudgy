//! The host-side MSDP **producer** (`docs/gmcp-mapping.md` §9 item 3): the
//! session-thread half that turns inbound MSDP subnegotiations into session-store writes
//! under the `msdp` platform producer and catalogues each variable for the automations
//! window's Store tab. The wire half (decoding, framing, the handshake) is
//! `session::connection::msdp`.
//!
//! Deliberately smaller than its `gmcp` sibling: MSDP has no module registry (REPORT is
//! the whole subscription model, and the host sends the mapping baseline at negotiation),
//! no merge keys (variables are whole-value updates by definition), and no script send
//! surface yet — scripts consume `smudgy:state/msdp` and the `ready`/`closed` events.
//! Variable names are flat identifiers, so each writes at a single-segment path — `ROOM`
//! is one key, never a dotted path.

use serde_json::Value;

use super::IsolateId;
use super::catalogue::{CatalogueKind, SharedCatalogue};
use super::store::{PlatformProducer, ProducerKey, SessionStore, StorePath};
use crate::session::connection::msdp as wire;

/// What one ingested subnegotiation asks the dispatch arm to do beyond the store writes.
#[derive(Default)]
pub(super) struct IngestEffects {
    /// Session-notice lines to echo (the one-time budget notice).
    pub echoes: Vec<String>,
}

pub(super) struct MsdpProducer {
    /// Whether MSDP is currently negotiated on for the live connection. No shared cell:
    /// nothing script-side reads it synchronously (readiness is the `msdp:ready` event).
    enabled: bool,
    /// The catalogue producer key (`"msdp"`), interned once.
    producer_display: std::sync::Arc<str>,
    /// Whether the one-time budget-refusal session notice went out.
    budget_noticed: bool,
}

impl MsdpProducer {
    pub fn new() -> Self {
        Self {
            enabled: false,
            producer_display: std::sync::Arc::from(PlatformProducer::Msdp.as_str()),
            budget_noticed: false,
        }
    }

    /// MSDP negotiated on (the connection task has already framed the handshake): fresh
    /// server, fresh truth — the subtree is cleared by one root write. The caller emits
    /// `msdp:ready` after this returns.
    pub fn on_enabled(&mut self, store: &mut SessionStore) {
        self.enabled = true;
        store
            .set(
                ProducerKey::Platform(PlatformProducer::Msdp),
                StorePath::root(),
                Value::Object(serde_json::Map::new()),
                IsolateId::Main,
                0,
            )
            .ok();
    }

    /// MSDP negotiated off (or the connection dropped while enabled). Returns whether it
    /// *was* enabled — the caller's cue to emit `msdp:closed`. The subtree is retained
    /// for post-mortem reads, like the `gmcp` tree.
    pub fn on_disabled(&mut self) -> bool {
        std::mem::take(&mut self.enabled)
    }

    /// Ingest one inbound subnegotiation: decode its variables, catalogue each
    /// (variable-name granularity, occurrence sample), and write the store at each name.
    /// The store flush — and with it watcher/binding delivery — is the run loop's normal
    /// per-turn flush, so the `gmcp` wire-order guarantee holds here identically.
    pub fn ingest(
        &mut self,
        store: &mut SessionStore,
        catalogue: &SharedCatalogue,
        payload: &[u8],
    ) -> IngestEffects {
        let mut effects = IngestEffects::default();
        for (name, value) in wire::parse_variables(payload) {
            if name.is_empty() {
                continue;
            }
            // Sampled before the budget outcome: presence and history don't depend on the
            // store having room. The raw text for the ring is the decoded JSON (MSDP's
            // wire bytes are control-marked, not display-friendly).
            let sample = value.to_string();
            catalogue.borrow_mut().sample_dynamic(
                &self.producer_display,
                CatalogueKind::State,
                &name,
                PlatformProducer::Msdp.as_str(),
                &sample,
            );

            // Single-segment path: MSDP names are flat identifiers, and a name that
            // happens to contain a dot must not fan out into a subtree.
            let Ok(path) = StorePath::from_segments([name.as_str()]) else {
                log::warn!("MSDP variable name {name:?} does not map to a store path; dropped");
                continue;
            };
            match store.set(
                ProducerKey::Platform(PlatformProducer::Msdp),
                path,
                value,
                IsolateId::Main,
                0,
            ) {
                Ok(_) => {}
                Err(err) => {
                    log::warn!("MSDP write refused: {err}");
                    if !self.budget_noticed {
                        self.budget_noticed = true;
                        effects.echoes.push(format!(
                            "MSDP: the server's data exceeded the session store budget and is \
                             no longer being retained ({err}). Existing state is intact."
                        ));
                    }
                }
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use super::super::catalogue::RuntimeCatalogue;
    use super::*;
    use crate::session::connection::msdp::marker::{TABLE_CLOSE, TABLE_OPEN, VAL, VAR};

    fn harness() -> (MsdpProducer, SessionStore, SharedCatalogue) {
        (
            MsdpProducer::new(),
            SessionStore::new(),
            Rc::new(RefCell::new(RuntimeCatalogue::new())),
        )
    }

    fn read(store: &SessionStore, path: &str) -> Option<Value> {
        store.get(
            &ProducerKey::Platform(PlatformProducer::Msdp),
            &StorePath::parse(path).unwrap(),
            &IsolateId::Main,
        )
    }

    fn payload(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn variables_write_at_their_names_and_room_table_reads_by_path() {
        let (mut msdp, mut store, catalogue) = harness();
        let bytes = payload(&[
            &[VAR],
            b"ROOM",
            &[VAL, TABLE_OPEN, VAR],
            b"VNUM",
            &[VAL],
            b"14100",
            &[VAR],
            b"EXITS",
            &[VAL, TABLE_OPEN, VAR],
            b"east",
            &[VAL],
            b"14101",
            &[TABLE_CLOSE, TABLE_CLOSE, VAR],
            b"ROOM_VNUM",
            &[VAL],
            b"14100",
        ]);
        msdp.ingest(&mut store, &catalogue, &bytes);
        store.flush();
        assert_eq!(read(&store, "ROOM.VNUM"), Some(json!("14100")));
        assert_eq!(read(&store, "ROOM.EXITS.east"), Some(json!("14101")));
        assert_eq!(read(&store, "ROOM_VNUM"), Some(json!("14100")));
    }

    #[test]
    fn enable_clears_the_subtree_and_disable_reports_prior_state() {
        let (mut msdp, mut store, catalogue) = harness();
        msdp.ingest(&mut store, &catalogue, &payload(&[&[VAR], b"HEALTH", &[VAL], b"50"]));
        store.flush();
        assert_eq!(read(&store, "HEALTH"), Some(json!("50")));

        msdp.on_enabled(&mut store);
        store.flush();
        assert_eq!(read(&store, "HEALTH"), None, "fresh negotiation clears stale truth");

        assert!(msdp.on_disabled(), "was enabled");
        assert!(!msdp.on_disabled(), "second disable is idempotent");
    }
}
