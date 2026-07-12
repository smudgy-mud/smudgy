//! The host-side GMCP **producer** (`docs/gmcp-plan.md` §4): the session-thread half that
//! turns inbound GMCP messages into session-store writes under the `gmcp` platform
//! producer, catalogues each message for the automations window's Store tab, deep-merges
//! the delta-shaped messages (merge keys), and memoizes parses of repeated payloads. The
//! wire half (splitting, framing, the handshake) is `session::connection::gmcp`.
//!
//! The host is structurally the sole writer of the `gmcp` subtree: no creator descriptor
//! resolves to a platform producer, so the op layer can never mint a producer seat for it
//! (`store::is_home` is `false` for every isolate). Writes here go through
//! [`SessionStore::set`] — the same budget-gated choke point every producer write passes —
//! under [`IsolateId::Main`]; the arm runs between JS turns, so no isolate ever observes a
//! journaled-but-unflushed GMCP write.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use super::catalogue::{CatalogueKind, SharedCatalogue};
use super::store::{PlatformProducer, ProducerKey, SessionStore, StorePath};
use super::IsolateId;
use crate::session::connection::gmcp as wire;

/// Compact JSON array of strings (module tokens are host-composed but module *names* come
/// from scripts, so real escaping — not string splicing — is required).
fn json_string_array(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// The GMCP enabled flag, shared (the same cell) between the producer — whose
/// `on_enabled`/`on_disabled` write it — and every isolate's `op_smudgy_gmcp_enabled`
/// (the `gmcp.enabled` getter / `gmcp.onReady` fast path). A newtype so the `OpState`
/// type-key cannot collide with another `Rc<Cell<bool>>`.
#[derive(Clone, Default)]
pub(crate) struct SharedGmcpEnabled(Rc<Cell<bool>>);

impl SharedGmcpEnabled {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> bool {
        self.0.get()
    }

    fn set(&self, enabled: bool) {
        self.0.set(enabled);
    }
}

/// Parse-memoization capacity (`docs/gmcp-plan.md` §4.5): distinct message names whose last
/// raw payload is retained. Real servers use dozens of names; the cap only bounds a hostile
/// name-minting feed (whose store entries the store budgets already bound).
const MEMO_CAP: usize = 1024;

/// One memoized parse: the exact raw payload text last seen for a name, and the value it
/// parsed to. An identical consecutive payload skips the parse, **not** the write —
/// occurrence consumers stay loss-free (`docs/interop.md` §2).
struct MemoEntry {
    raw: Arc<str>,
    value: Value,
}

/// What one ingested message asks the dispatch arm to do beyond the store write.
#[derive(Default)]
pub(super) struct IngestEffects {
    /// Session-notice lines to echo (`Core.Goodbye`'s reason; the one-time budget notice).
    pub echoes: Vec<String>,
}

/// One registered GMCP module (`docs/gmcp-plan.md` §6.2) — the gmod design with isolates
/// as the "user" key, so no package can drop a module another package still uses.
struct ModuleEntry {
    /// First-registered casing — what goes on the wire.
    display: String,
    /// Highest version any holder requested (`Core.Supports.Add` replaces on higher
    /// version server-side, so the registry only ever raises it).
    version: u32,
    /// Isolates currently holding a ref. Emptied on engine rebuild (a reloading package
    /// re-registers as it re-evaluates); the entry itself is kept for version memory.
    holders: HashSet<IsolateId>,
}

pub(super) struct GmcpProducer {
    /// Whether GMCP is currently negotiated on for the live connection (shared with the
    /// script engine's ops — see [`SharedGmcpEnabled`]).
    enabled: SharedGmcpEnabled,
    /// Folded message names whose object payloads deep-merge into the retained value
    /// instead of replacing it (IRE `Char.Status` sends deltas after the initial full
    /// send — `docs/gmcp-plan.md` §4.3). Session-scoped; extended by `gmcp.mergeKeys`.
    merge_keys: HashSet<String>,
    /// Per-name parse memoization, keyed by folded name (`docs/gmcp-plan.md` §4.5).
    memo: HashMap<String, MemoEntry>,
    /// The module registry, keyed by folded module name (`docs/gmcp-plan.md` §6.2).
    modules: HashMap<String, ModuleEntry>,
    /// The catalogue producer key (`"gmcp"`), interned once.
    producer_display: Arc<str>,
    /// Whether the one-time budget-refusal session notice went out.
    budget_noticed: bool,
    /// Whether the one-time send-while-disabled notice went out.
    send_disabled_noticed: bool,
    /// Folded names already given the not-JSON teaching diagnostic this session.
    non_json_warned: HashSet<String>,
}

impl GmcpProducer {
    pub fn new(enabled: SharedGmcpEnabled) -> Self {
        Self {
            enabled,
            merge_keys: HashSet::from(["char.status".to_string()]),
            memo: HashMap::new(),
            modules: HashMap::new(),
            producer_display: Arc::from(PlatformProducer::Gmcp.as_str()),
            budget_noticed: false,
            send_disabled_noticed: false,
            non_json_warned: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub fn enabled(&self) -> bool {
        self.enabled.get()
    }

    /// GMCP negotiated on (the connection task has already sent the handshake): fresh
    /// server, fresh truth — the subtree is cleared by one root write (`previousValue`
    /// retains the displaced generation like any other write batch). The caller emits
    /// `gmcp:ready` after this returns.
    pub fn on_enabled(&mut self, store: &mut SessionStore) {
        self.enabled.set(true);
        self.memo.clear();
        store
            .set(
                ProducerKey::Platform(PlatformProducer::Gmcp),
                StorePath::root(),
                Value::Object(serde_json::Map::new()),
                IsolateId::Main,
                0,
            )
            .ok();
    }

    /// GMCP negotiated off (or the connection dropped while enabled). Returns whether it
    /// *was* enabled — the caller's cue to emit `gmcp:closed`. The subtree is retained for
    /// post-mortem reads (`docs/gmcp-plan.md` §4.6).
    pub fn on_disabled(&mut self) -> bool {
        self.memo.clear();
        let was = self.enabled.get();
        self.enabled.set(false);
        was
    }

    /// Gate one outbound send on the negotiated state. Returns whether the send may go
    /// out, plus a one-time author-facing notice when it may not — a `gmcp.send` against a
    /// server that never negotiated GMCP is an author mistake worth teaching once, not a
    /// per-call echo storm.
    pub fn send_gate(&mut self) -> (bool, Option<String>) {
        if self.enabled.get() {
            return (true, None);
        }
        let notice = if self.send_disabled_noticed {
            None
        } else {
            self.send_disabled_noticed = true;
            Some(
                "GMCP: outbound message dropped — GMCP is not enabled on this connection \
                 (gmcp.onReady() waits for it)."
                    .to_string(),
            )
        };
        (false, notice)
    }

    /// Register `isolate`'s use of `module` (`docs/gmcp-plan.md` §6.2). Returns the framed
    /// wire bytes to write — empty when nothing goes out: GMCP not negotiated yet (the
    /// `GmcpEnabled` arm folds recorded modules in via [`Self::supports_add_frame`]), or
    /// the module already active with a sufficient version.
    pub fn enable_module(&mut self, isolate: IsolateId, module: &str, version: u32) -> Vec<u8> {
        let folded = module.to_ascii_lowercase();
        let entry = self
            .modules
            .entry(folded)
            .or_insert_with(|| ModuleEntry {
                display: module.to_string(),
                version: 1,
                holders: HashSet::new(),
            });
        let was_active = !entry.holders.is_empty();
        entry.holders.insert(isolate);
        let version_bumped = version > entry.version;
        entry.version = entry.version.max(version);
        let display = entry.display.clone();
        let leaf_version = entry.version;
        if !self.enabled.get() || (was_active && !version_bumped) {
            return Vec::new();
        }
        let tokens = self.supports_tokens(&display, leaf_version);
        let mut frame = Vec::new();
        wire::frame_message("Core.Supports.Add", Some(&json_string_array(&tokens)), &mut frame);
        frame
    }

    /// Release `isolate`'s ref on `module`. Last-ref-out (while negotiated) sends
    /// `Core.Supports.Remove` for the module itself — no prefix expansion on the way down
    /// (gmod parity: removing `IRE.Rift` must not disable `IRE` for its other users).
    pub fn disable_module(&mut self, isolate: &IsolateId, module: &str) -> Vec<u8> {
        let folded = module.to_ascii_lowercase();
        let Some(entry) = self.modules.get_mut(&folded) else {
            return Vec::new();
        };
        entry.holders.remove(isolate);
        if !entry.holders.is_empty() || !self.enabled.get() {
            return Vec::new();
        }
        let payload = json_string_array(std::slice::from_ref(&entry.display));
        let mut frame = Vec::new();
        wire::frame_message("Core.Supports.Remove", Some(&payload), &mut frame);
        frame
    }

    /// Every actively-held module as one `Core.Supports.Add` — sent by the `GmcpEnabled`
    /// arm right after the connection task's baseline `Set`, which is both how pre-`ready`
    /// registrations fold into the handshake and how a renegotiation (copyover) re-sends
    /// automatically. Empty when nothing is held.
    pub fn supports_add_frame(&self) -> Vec<u8> {
        let mut active: Vec<(&str, u32)> = self
            .modules
            .values()
            .filter(|entry| !entry.holders.is_empty())
            .map(|entry| (entry.display.as_str(), entry.version))
            .collect();
        if active.is_empty() {
            return Vec::new();
        }
        active.sort_unstable_by_key(|entry| entry.0.to_ascii_lowercase());
        let mut tokens: Vec<String> = Vec::new();
        for (display, version) in active {
            for token in self.supports_tokens(display, version) {
                if !tokens.contains(&token) {
                    tokens.push(token);
                }
            }
        }
        let mut frame = Vec::new();
        wire::frame_message("Core.Supports.Add", Some(&json_string_array(&tokens)), &mut frame);
        frame
    }

    /// The `Core.Supports` tokens for one module, dotted prefixes included (gmod-style:
    /// enabling `IRE.Rift` also enables `IRE`). Each prefix carries its own registered
    /// version when it is separately registered, else `1`; the leaf carries `version`.
    fn supports_tokens(&self, display: &str, version: u32) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut end = 0usize;
        let bytes = display.as_bytes();
        while end < bytes.len() {
            end = display[end..]
                .find('.')
                .map_or(display.len(), |dot| end + dot);
            let prefix = &display[..end];
            let prefix_version = if prefix.len() == display.len() {
                version
            } else {
                self.modules
                    .get(&prefix.to_ascii_lowercase())
                    .map_or(1, |entry| entry.version)
            };
            tokens.push(format!("{prefix} {prefix_version}"));
            end += 1;
        }
        tokens
    }

    /// Engine rebuild: every isolate's module refs are released — a reloading package
    /// re-registers as it re-evaluates (`docs/gmcp-plan.md` §6.2), and redundant
    /// `Supports.Add`s are idempotent server-side, so nothing is sent here. Version
    /// memory is kept with the entries.
    pub fn reset_engine_refs(&mut self) {
        for entry in self.modules.values_mut() {
            entry.holders.clear();
        }
    }

    /// Extend the merge-key set (`gmcp.mergeKeys`, `docs/gmcp-plan.md` §4.3). Folded like
    /// every structural name; additive only (the default `char.status` never leaves).
    pub fn add_merge_keys(&mut self, names: &[String]) {
        for name in names {
            self.merge_keys.insert(name.to_ascii_lowercase());
        }
    }

    /// Ingest one inbound message: catalogue it (message-name granularity, occurrence
    /// sample), parse (or reuse the memoized parse), apply the merge-key semantics, and
    /// write the store at the name's path. Returns the side effects the dispatch arm
    /// surfaces (echo lines); the store flush — and with it watcher/binding delivery — is
    /// the run loop's normal per-turn flush.
    pub fn ingest(
        &mut self,
        store: &mut SessionStore,
        catalogue: &SharedCatalogue,
        name: &str,
        data: Option<&str>,
    ) -> IngestEffects {
        let mut effects = IngestEffects::default();

        // Message-name-granular catalogue entry + occurrence sample (`docs/gmcp-plan.md`
        // §5). Recorded before parse/budget outcomes: presence and history don't depend on
        // the payload being well-formed or the store having room.
        catalogue.borrow_mut().sample_dynamic(
            &self.producer_display,
            CatalogueKind::State,
            name,
            PlatformProducer::Gmcp.as_str(),
            data.unwrap_or("null"),
        );

        let Ok(path) = StorePath::from_segments(name.split('.')) else {
            log::warn!("GMCP message name {name:?} does not map to a store path; dropped");
            return effects;
        };
        let folded = name.to_ascii_lowercase();

        let value = self.parse_payload(&folded, data);

        // Host reaction (`docs/gmcp-plan.md` §6.1): surface the goodbye reason as a system
        // line. (`Core.Ping` is answered at the wire by the connection task.)
        if folded == "core.goodbye" {
            let reason = match &value {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            effects.echoes.push(if reason.is_empty() {
                "GMCP: the server says goodbye.".to_string()
            } else {
                format!("GMCP: the server says goodbye: {reason}")
            });
        }

        // List reducers (`docs/gmcp-plan.md` §4.4) compute against the state *before* this
        // message's own write; unknown shapes yield `None` and degrade to the plain write.
        let reduced = Self::reduce(store, &folded, &value);

        let value = self.apply_merge_keys(store, &folded, &path, value);
        self.write(store, path, value, &mut effects);
        if let Some((maintained_path, maintained)) = reduced {
            self.write(store, maintained_path, maintained, &mut effects);
        }
        effects
    }

    /// One budget-gated store write under the `gmcp` producer, with the shared refusal
    /// handling (log each; session notice once).
    fn write(
        &mut self,
        store: &mut SessionStore,
        path: StorePath,
        value: Value,
        effects: &mut IngestEffects,
    ) {
        match store.set(
            ProducerKey::Platform(PlatformProducer::Gmcp),
            path,
            value,
            IsolateId::Main,
            0,
        ) {
            Ok(_) => {}
            Err(err) => {
                log::warn!("GMCP write refused: {err}");
                if !self.budget_noticed {
                    self.budget_noticed = true;
                    effects.echoes.push(format!(
                        "GMCP: the server's data exceeded the session store budget and is \
                         no longer being retained ({err}). Existing state is intact."
                    ));
                }
            }
        }
    }

    /// The protocol-level list reducers (`docs/gmcp-plan.md` §4.4): IRE's documented
    /// delta-shaped messages maintain the list they patch, so `.value` never lies about a
    /// list the server updates incrementally. Best-effort by design — any shape mismatch
    /// returns `None` and the message degrades to its plain set-at-name.
    fn reduce(store: &SessionStore, folded: &str, delta: &Value) -> Option<(StorePath, Value)> {
        let producer = ProducerKey::Platform(PlatformProducer::Gmcp);
        match folded {
            "char.items.add" | "char.items.remove" | "char.items.update" => {
                let fields = delta.as_object()?;
                let location = fields.get("location")?;
                let item = fields.get("item")?;
                let list_path = StorePath::from_segments(["Char", "Items", "List"]).ok()?;
                let Some(Value::Object(mut list)) =
                    store.get(&producer, &list_path, &IsolateId::Main)
                else {
                    return None;
                };
                // The retained List is per location (IRE sends one list per location); a
                // delta for a different location has nothing to patch.
                if list.get("location") != Some(location) {
                    return None;
                }
                let items = list.get_mut("items")?.as_array_mut()?;
                match folded {
                    "char.items.add" => items.push(item.clone()),
                    "char.items.remove" => {
                        let id = item_id(item)?;
                        items.retain(|existing| item_id(existing) != Some(id.clone()));
                    }
                    _ => {
                        let id = item_id(item)?;
                        if let Some(slot) = items
                            .iter_mut()
                            .find(|existing| item_id(existing) == Some(id.clone()))
                        {
                            *slot = item.clone();
                        } else {
                            items.push(item.clone());
                        }
                    }
                }
                Some((list_path, Value::Object(list)))
            }
            "room.addplayer" | "room.removeplayer" => {
                let players_path = StorePath::from_segments(["Room", "Players"]).ok()?;
                let retained = store.get(&producer, &players_path, &IsolateId::Main);
                let mut players = match retained {
                    Some(Value::Array(players)) => players,
                    // AddPlayer can seed an empty list; RemovePlayer with no list has
                    // nothing to patch.
                    _ if folded == "room.addplayer" => Vec::new(),
                    _ => return None,
                };
                let name = player_name(delta)?;
                if folded == "room.addplayer" {
                    if !players
                        .iter()
                        .any(|player| player_name(player) == Some(name.clone()))
                    {
                        players.push(delta.clone());
                    }
                } else {
                    players.retain(|player| player_name(player) != Some(name.clone()));
                }
                Some((players_path, Value::Array(players)))
            }
            _ => None,
        }
    }

    /// Parse the data part — or skip the parse when the raw text is byte-identical to the
    /// last payload seen for this name (the write still happens; §4.5). No data is `null`;
    /// data that is not JSON is retained as a JSON string of the raw text, with a
    /// once-per-name teaching diagnostic — liberal ingest, never silent loss.
    fn parse_payload(&mut self, folded: &str, data: Option<&str>) -> Value {
        let Some(raw) = data else {
            return Value::Null;
        };
        if let Some(entry) = self.memo.get(folded)
            && *entry.raw == *raw
        {
            return entry.value.clone();
        }
        let value = serde_json::from_str::<Value>(raw).unwrap_or_else(|err| {
            if self.non_json_warned.insert(folded.to_string()) {
                log::warn!(
                    "GMCP {folded:?} carried a data part that is not valid JSON ({err}); \
                     stored as a raw string"
                );
            }
            Value::String(raw.to_string())
        });
        if self.memo.len() < MEMO_CAP || self.memo.contains_key(folded) {
            self.memo.insert(
                folded.to_string(),
                MemoEntry {
                    raw: Arc::from(raw),
                    value: value.clone(),
                },
            );
        }
        value
    }

    /// Merge-key semantics (`docs/gmcp-plan.md` §4.3): for a matched name whose incoming
    /// and retained values are both objects, deep-merge the delta into the current subtree
    /// (objects merge recursively, everything else replaces) and return the merged
    /// document — still one set-at-path per message.
    fn apply_merge_keys(
        &self,
        store: &SessionStore,
        folded: &str,
        path: &StorePath,
        value: Value,
    ) -> Value {
        if !self.merge_keys.contains(folded) {
            return value;
        }
        let Value::Object(delta) = value else {
            return value;
        };
        let base = store.get(
            &ProducerKey::Platform(PlatformProducer::Gmcp),
            path,
            &IsolateId::Main,
        );
        match base {
            Some(Value::Object(mut base)) => {
                deep_merge(&mut base, delta);
                Value::Object(base)
            }
            _ => Value::Object(delta),
        }
    }
}

/// An item's identity for the `Char.Items` reducers: its `id` field rendered as a string
/// (IRE sends numbers and numeric strings interchangeably), or the value itself when the
/// server sends a bare id.
fn item_id(item: &Value) -> Option<String> {
    let id = match item {
        Value::Object(fields) => fields.get("id")?,
        other => other,
    };
    match id {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A player's identity for the `Room.Players` reducers: the `name` field, or the value
/// itself when `Room.RemovePlayer` sends a bare name string.
fn player_name(player: &Value) -> Option<String> {
    match player {
        Value::String(name) => Some(name.clone()),
        Value::Object(fields) => match fields.get("name") {
            Some(Value::String(name)) => Some(name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Recursive object merge: object-into-object merges per key, anything else replaces.
/// Keys match exactly here; the store's uniform write-time fold resolves any
/// case-collision at the write, exactly as it would for a producer publishing both
/// spellings.
fn deep_merge(base: &mut serde_json::Map<String, Value>, delta: serde_json::Map<String, Value>) {
    for (key, incoming) in delta {
        match (base.get_mut(&key), incoming) {
            (Some(Value::Object(existing)), Value::Object(delta_child)) => {
                deep_merge(existing, delta_child);
            }
            (_, incoming) => {
                base.insert(key, incoming);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use super::super::catalogue::RuntimeCatalogue;
    use super::super::store::StoreBudgets;
    use super::*;

    fn harness() -> (GmcpProducer, SessionStore, SharedCatalogue) {
        (
            GmcpProducer::new(SharedGmcpEnabled::new()),
            SessionStore::new(),
            Rc::new(RefCell::new(RuntimeCatalogue::new())),
        )
    }

    fn read(store: &SessionStore, path: &str) -> Option<Value> {
        store.get(
            &ProducerKey::Platform(PlatformProducer::Gmcp),
            &StorePath::parse(path).unwrap(),
            &IsolateId::Main,
        )
    }

    #[test]
    fn message_writes_at_its_name_with_casing_preserved() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Vitals",
            Some(r#"{ "hp": 100, "maxhp": 120 }"#),
        );
        store.flush();
        assert_eq!(read(&store, "Char.Vitals.hp"), Some(json!(100)));
        // The store fold makes the Mudlet-cased and server-cased spellings one key.
        assert_eq!(read(&store, "char.vitals.hp"), Some(json!(100)));
    }

    #[test]
    fn no_data_is_null_and_non_json_is_a_raw_string() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.ingest(&mut store, &catalogue, "Core.Ping", None);
        gmcp.ingest(&mut store, &catalogue, "Weird.Msg", Some("not json at all"));
        store.flush();
        assert_eq!(read(&store, "Core.Ping"), Some(Value::Null));
        assert_eq!(read(&store, "Weird.Msg"), Some(json!("not json at all")));
    }

    #[test]
    fn merge_key_deep_merges_deltas_and_replace_still_replaces() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Status",
            Some(r#"{ "level": 10, "guild": { "name": "Mages", "rank": 1 } }"#),
        );
        store.flush();
        // The delta only carries the changed field; the merged document keeps the rest.
        gmcp.ingest(
            &mut store,
            &catalogue,
            "char.status",
            Some(r#"{ "guild": { "rank": 2 } }"#),
        );
        store.flush();
        assert_eq!(read(&store, "Char.Status.level"), Some(json!(10)));
        assert_eq!(read(&store, "Char.Status.guild.name"), Some(json!("Mages")));
        assert_eq!(read(&store, "Char.Status.guild.rank"), Some(json!(2)));

        // A non-merge-keyed name replaces wholesale.
        gmcp.ingest(&mut store, &catalogue, "Char.Vitals", Some(r#"{ "hp": 1 }"#));
        store.flush();
        gmcp.ingest(&mut store, &catalogue, "Char.Vitals", Some(r#"{ "mp": 2 }"#));
        store.flush();
        assert_eq!(read(&store, "Char.Vitals.hp"), None);
        assert_eq!(read(&store, "Char.Vitals.mp"), Some(json!(2)));
    }

    #[test]
    fn memoized_repeat_skips_the_parse_but_not_the_write() {
        let (mut gmcp, mut store, catalogue) = harness();
        let payload = r#"{ "hp": 50 }"#;
        gmcp.ingest(&mut store, &catalogue, "Char.Vitals", Some(payload));
        gmcp.ingest(&mut store, &catalogue, "Char.Vitals", Some(payload));
        // Two ingests, two journal entries — the write is never suppressed (occurrence
        // consumers depend on it), only the parse is reused.
        let actions = store.flush();
        drop(actions);
        assert_eq!(read(&store, "Char.Vitals.hp"), Some(json!(50)));
        assert_eq!(
            catalogue
                .borrow_mut()
                .snapshot(&store)
                .entries
                .iter()
                .find(|e| &*e.name == "Char.Vitals")
                .map(|e| e.occurrences),
            Some(2)
        );
    }

    #[test]
    fn enable_clears_the_subtree_and_goodbye_echoes() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.on_enabled(&mut store);
        assert!(gmcp.enabled());
        gmcp.ingest(&mut store, &catalogue, "Char.Vitals", Some(r#"{ "hp": 5 }"#));
        store.flush();
        assert_eq!(read(&store, "Char.Vitals.hp"), Some(json!(5)));

        // Renegotiation (copyover): fresh truth.
        gmcp.on_enabled(&mut store);
        store.flush();
        assert_eq!(read(&store, "Char.Vitals"), None);
        assert_eq!(read(&store, ""), Some(json!({})));

        let effects = gmcp.ingest(&mut store, &catalogue, "Core.Goodbye", Some(r#""bye now""#));
        assert_eq!(
            effects.echoes,
            vec!["GMCP: the server says goodbye: bye now".to_string()]
        );
        assert!(gmcp.on_disabled());
        assert!(!gmcp.on_disabled(), "second disable is not 'was enabled'");
    }

    fn sandbox_isolate() -> IsolateId {
        IsolateId::Package {
            owner: "wbk".into(),
            name: "tracker".into(),
            version: "1.0.0".into(),
        }
    }

    fn frame_text(frame: &[u8]) -> String {
        String::from_utf8_lossy(frame).into_owned()
    }

    #[test]
    fn module_registry_refcounts_prefixes_versions_and_resend() {
        let (mut gmcp, mut store, _catalogue) = harness();

        // Pre-ready registration records without sending.
        let frame = gmcp.enable_module(IsolateId::Main, "IRE.Rift", 1);
        assert!(frame.is_empty(), "pre-ready enable only records");

        // The enable arm re-sends everything held — pre-ready folds into the handshake.
        gmcp.on_enabled(&mut store);
        let resend = frame_text(&gmcp.supports_add_frame());
        assert!(
            resend.contains(r#"Core.Supports.Add ["IRE 1","IRE.Rift 1"]"#),
            "dotted prefixes ride along, leaf carries its version: {resend}"
        );

        // A second holder of an active module sends nothing.
        let frame = gmcp.enable_module(sandbox_isolate(), "ire.rift", 1);
        assert!(frame.is_empty(), "already-active enable is silent");

        // A higher version re-sends (Core.Supports.Add replaces on higher version).
        let frame = frame_text(&gmcp.enable_module(IsolateId::Main, "IRE.Rift", 3));
        assert!(frame.contains(r#"["IRE 1","IRE.Rift 3"]"#), "{frame}");

        // First release keeps the module (another isolate holds it); last release removes,
        // without prefix expansion.
        assert!(gmcp.disable_module(&IsolateId::Main, "IRE.Rift").is_empty());
        let frame = frame_text(&gmcp.disable_module(&sandbox_isolate(), "IRE.RIFT"));
        assert!(frame.contains(r#"Core.Supports.Remove ["IRE.Rift"]"#), "{frame}");

        // Engine rebuild releases refs silently; nothing is re-sent until re-registration.
        let frame = frame_text(&gmcp.enable_module(IsolateId::Main, "Comm.Channel", 1));
        assert!(frame.contains(r#"["Comm 1","Comm.Channel 1"]"#), "{frame}");
        gmcp.reset_engine_refs();
        assert!(gmcp.supports_add_frame().is_empty(), "no holders after rebuild");
        // Version memory survives the rebuild.
        let frame = frame_text(&gmcp.enable_module(IsolateId::Main, "ire.rift", 1));
        assert!(frame.contains("IRE.Rift 3"), "version memory kept: {frame}");
    }

    #[test]
    fn merge_keys_api_extends_the_set() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.add_merge_keys(&["Char.Defences".to_string()]);
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Defences",
            Some(r#"{ "shield": true, "armor": 5 }"#),
        );
        store.flush();
        gmcp.ingest(&mut store, &catalogue, "char.defences", Some(r#"{ "armor": 6 }"#));
        store.flush();
        assert_eq!(read(&store, "Char.Defences.shield"), Some(json!(true)));
        assert_eq!(read(&store, "Char.Defences.armor"), Some(json!(6)));
    }

    #[test]
    fn items_reducers_maintain_the_per_location_list() {
        let (mut gmcp, mut store, catalogue) = harness();
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Items.List",
            Some(r#"{ "location": "inv", "items": [ { "id": 1, "name": "a sword" } ] }"#),
        );
        store.flush();
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Items.Add",
            Some(r#"{ "location": "inv", "item": { "id": 2, "name": "a shield" } }"#),
        );
        store.flush();
        assert_eq!(
            read(&store, "Char.Items.List.items")
                .and_then(|v| v.as_array().map(Vec::len)),
            Some(2),
            "Add appends to the maintained list"
        );
        // The raw delta is still retained at its own name (the occurrence).
        assert_eq!(read(&store, "Char.Items.Add.item.id"), Some(json!(2)));

        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Items.Update",
            Some(r#"{ "location": "inv", "item": { "id": 2, "name": "a tower shield" } }"#),
        );
        store.flush();
        let items = read(&store, "Char.Items.List.items").unwrap();
        assert!(
            items.as_array().unwrap().iter().any(|i| i["name"] == json!("a tower shield")),
            "Update replaces by id: {items}"
        );

        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Items.Remove",
            Some(r#"{ "location": "inv", "item": { "id": 1 } }"#),
        );
        store.flush();
        assert_eq!(
            read(&store, "Char.Items.List.items")
                .and_then(|v| v.as_array().map(Vec::len)),
            Some(1),
            "Remove drops by id"
        );

        // A delta for a location other than the retained list has nothing to patch.
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Char.Items.Add",
            Some(r#"{ "location": "room", "item": { "id": 9 } }"#),
        );
        store.flush();
        assert_eq!(
            read(&store, "Char.Items.List.items")
                .and_then(|v| v.as_array().map(Vec::len)),
            Some(1),
            "cross-location delta degrades to its plain set-at-name"
        );
    }

    #[test]
    fn player_reducers_maintain_room_players() {
        let (mut gmcp, mut store, catalogue) = harness();
        // AddPlayer seeds the list from nothing.
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Room.AddPlayer",
            Some(r#"{ "name": "Bob", "fullname": "Bob the Builder" }"#),
        );
        store.flush();
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Room.AddPlayer",
            Some(r#"{ "name": "Bob", "fullname": "Bob the Builder" }"#),
        );
        gmcp.ingest(
            &mut store,
            &catalogue,
            "Room.AddPlayer",
            Some(r#"{ "name": "Alice" }"#),
        );
        store.flush();
        assert_eq!(
            read(&store, "Room.Players").and_then(|v| v.as_array().map(Vec::len)),
            Some(2),
            "AddPlayer dedupes by name"
        );
        // RemovePlayer accepts the bare-name form.
        gmcp.ingest(&mut store, &catalogue, "Room.RemovePlayer", Some(r#""Bob""#));
        store.flush();
        let players = read(&store, "Room.Players").unwrap();
        assert_eq!(players.as_array().map(Vec::len), Some(1), "{players}");
        assert_eq!(players[0]["name"], json!("Alice"));
    }

    #[test]
    fn send_gate_notices_once_while_disabled() {
        let (mut gmcp, mut store, _catalogue) = harness();
        let (allowed, notice) = gmcp.send_gate();
        assert!(!allowed);
        assert!(notice.is_some(), "first drop teaches");
        let (allowed, notice) = gmcp.send_gate();
        assert!(!allowed);
        assert!(notice.is_none(), "the notice is once per session");
        gmcp.on_enabled(&mut store);
        let (allowed, notice) = gmcp.send_gate();
        assert!(allowed);
        assert!(notice.is_none());
    }

    #[test]
    fn budget_refusal_notices_once_and_keeps_existing_state() {
        let mut gmcp = GmcpProducer::new(SharedGmcpEnabled::new());
        let catalogue: SharedCatalogue = Rc::new(RefCell::new(RuntimeCatalogue::new()));
        let mut store = SessionStore::with_budgets(StoreBudgets {
            max_entries: 4,
            max_bytes: 1024,
        });
        gmcp.ingest(&mut store, &catalogue, "A.B", Some("1"));
        store.flush();
        let first = gmcp.ingest(
            &mut store,
            &catalogue,
            "Big.Table",
            Some(r#"{ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5 }"#),
        );
        assert!(!first.echoes.is_empty(), "first refusal carries the notice");
        let second = gmcp.ingest(
            &mut store,
            &catalogue,
            "Big.Table",
            Some(r#"{ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5 }"#),
        );
        assert!(second.echoes.is_empty(), "the notice is once per session");
        store.flush();
        assert_eq!(read(&store, "A.B"), Some(json!(1)), "existing state intact");
        assert_eq!(read(&store, "Big.Table"), None);
    }
}
