//! The session's pane registry: pane *existence* lives here on the session
//! thread (scripts and line routing run here); pane *placement* (grids,
//! windows) lives in the UI. See `docs/flexible-panes-plan.md` §2.2.
//!
//! Identity is layered:
//! - [`PaneKey`] is a never-reused *incarnation* id. Queued buffer updates,
//!   grid slots, and routing state hold keys, so index reuse would silently
//!   alias a new pane (ABA); a close permanently retires the key.
//! - [`PaneNameId`] is a *name* identity: assigned once per distinct folded
//!   (namespace, name) pair and never retired, so it is stable across close/recreate and
//!   script reloads. Hot loops (per-line routing via a `Pane` handle, widget
//!   targeting per frame) compare these integers; only `split()` and
//!   bare-string arguments pay fold+hash.

use std::collections::HashMap;
use std::sync::Arc;

/// Reserved name for the session's fused output+input pane. It resolves to
/// the main pane in *every* namespace (a future MXP `_top` maps here).
pub const MAIN_PANE_NAME: &str = "main";

/// The main pane's key. The registry mints it first at construction, so it is
/// a stable constant the UI can key its root grid slot by before any pane
/// event arrives.
pub const MAIN_PANE_KEY: PaneKey = PaneKey(0);

/// The main pane's interned name id (interned first at construction, in every
/// registry — so a `Pane` handle for "main" can carry it without a lookup).
pub const MAIN_PANE_NAME_ID: PaneNameId = PaneNameId(0);

/// At most this many non-main panes may be live per session (terminal and
/// widget panes both count). Bounds per-pane scrollback memory and keeps the
/// grid legible; `split()` past the cap throws.
pub const NON_MAIN_PANE_CAP: usize = 16;

/// Never-reused incarnation identity for one live pane. Monotonic per
/// session; a close permanently retires the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PaneKey(u32);

impl std::fmt::Display for PaneKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pane#{}", self.0)
    }
}

/// Name identity: assigned once per distinct folded (namespace, name) pair,
/// never retired — stable across close/recreate and reloads. Per-session; a
/// name id from one session is meaningless in another (cross-session ops
/// carry names instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct PaneNameId(u32);

impl PaneNameId {
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }

    #[must_use]
    pub fn from_u32(raw: u32) -> Self {
        Self(raw)
    }
}

/// Which code a pane name belongs to. `Main`-isolate scripts and user inline
/// scripts share `User`; each package isolate gets `Package { owner, name }`
/// — deliberately **excluding version**, so the namespace is stable across
/// package upgrades/reloads and get-or-create cannot mint duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PaneNamespace {
    User,
    Package { owner: Arc<str>, name: Arc<str> },
}

/// Whether a pane has a terminal scrollback. Every pane hosts script widget
/// trees (stacked over the terminal on `Terminal` panes, exactly like the
/// main pane's overlay); `Widgets` panes are widgets-only — no scrollback,
/// so the terminal ops (`echo`/`clear`) throw on them. The kind is also the
/// seam where future non-scrollback MXP panel bodies would slot in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Terminal,
    Widgets,
}

/// When a pane's title bar (its header/drag handle) is shown. `Normal` panes
/// follow the global distraction-free rule — headers show only while the
/// hosting window's toolbar is expanded (or when the global hide setting is
/// off); `AlwaysShow` pins the header on regardless, for a pane whose header
/// carries meaning (a labelled map/notes panel). A pane whose title bar is
/// hidden renders body-only and cannot be drag-rearranged (dividers still
/// resize it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitleBarPolicy {
    #[default]
    Normal,
    AlwaysShow,
}

impl TitleBarPolicy {
    /// Parse the script-facing string union (`'normal' | 'always-show'`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "always-show" => Some(Self::AlwaysShow),
            _ => None,
        }
    }
}

/// One live pane's definition, mirrored to the UI via `PaneOpened` (and
/// re-mirrored via `PaneUpdated` when a mutable field like `title_bar`
/// changes on an existing pane).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDef {
    pub key: PaneKey,
    pub name_id: PaneNameId,
    /// Display-cased name as the creator wrote it (identity is the folded form).
    pub name: Arc<str>,
    pub namespace: PaneNamespace,
    pub kind: PaneKind,
    pub is_main: bool,
    /// Header visibility policy. The one spec field `split()` *updates* on an
    /// existing pane (when given explicitly) — which is also the only way to
    /// set it on the main pane, whose def otherwise exists from construction.
    pub title_bar: TitleBarPolicy,
}

/// Which side of the reference pane a split places the new pane on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitDirection {
    /// Parse the script-facing string union (`'left'|'right'|'top'|'bottom'`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }
}

/// Where the UI should place a freshly created pane: split off `reference`
/// toward `direction`, with an optional initial extent in pixels along the
/// split axis (converted to a ratio against the reference pane's measured
/// extent at placement time; `None` ⇒ an even 0.5 split).
#[derive(Debug, Clone, Copy)]
pub struct PanePlacement {
    pub reference: PaneKey,
    pub direction: SplitDirection,
    pub size_px: Option<f32>,
}

/// Why a registry mutation was refused. Surfaced to scripts as a thrown error
/// naming the rule, so a denied call is an author bug they can fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaneError {
    #[error("invalid pane name: {0}")]
    InvalidName(String),
    #[error("'{0}' is a reserved pane name")]
    ReservedName(String),
    #[error("pane cap exceeded: at most {NON_MAIN_PANE_CAP} non-main panes per session")]
    CapExceeded,
    #[error("pane '{0}' already exists with a different kind")]
    KindMismatch(String),
    #[error("the main pane cannot be closed")]
    CloseMain,
    #[error("no pane named '{0}'")]
    NoSuchPane(String),
}

/// The result of a `split()`: the pane's definition, whether this call
/// created it (false ⇒ get-or-create returned the existing pane), and whether
/// an explicit `titleBar` in the spec changed an existing pane's policy (the
/// caller then mirrors the def to the UI via `PaneUpdated`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    pub def: PaneDef,
    pub created: bool,
    pub title_bar_changed: bool,
}

/// Registry member names reserved so `session.panes.<name>` dot access can
/// never be shadowed by a pane (`then` additionally keeps the proxy
/// non-thenable under `await`). Reserved-name panes cannot be created;
/// lookups for them are plain read misses.
const RESERVED_MEMBER_NAMES: [&str; 4] = ["get", "list", "exists", "then"];

/// Case-fold a pane name to its identity form. Pane identity is
/// case-insensitive (chosen now because MXP FRAME names are case-insensitive,
/// and changing identity later would break get-or-create).
fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Validate a pane name at `split()` time: non-empty, ≤ 64 chars, printable
/// (no control characters, which covers newlines).
fn validate_name(name: &str) -> Result<(), PaneError> {
    if name.is_empty() {
        return Err(PaneError::InvalidName("name is empty".to_string()));
    }
    if name.chars().count() > 64 {
        return Err(PaneError::InvalidName(
            "name is longer than 64 characters".to_string(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(PaneError::InvalidName(
            "name contains control characters".to_string(),
        ));
    }
    Ok(())
}

/// The session's pane registry. An `Rc<RefCell<PaneRegistry>>` on the runtime
/// is shared into every isolate's `OpState` (like `pending_line_operations`),
/// so pane ops mutate it synchronously; it is preserved across script reloads
/// (like `recent_lines`), which is what makes "panes survive reloads" true.
#[derive(Debug)]
pub struct PaneRegistry {
    /// Live panes by incarnation key.
    defs: HashMap<PaneKey, PaneDef>,
    /// Permanent name→id assignment, keyed by (namespace, folded name).
    /// Grows only at `split()`; bounded by panes ever created in the session.
    interned: HashMap<(PaneNamespace, String), PaneNameId>,
    /// The live pane per name identity: get-or-create and the per-line fast
    /// path both resolve through this single integer-keyed map.
    live: HashMap<PaneNameId, PaneKey>,
    next_key: u32,
    next_name_id: u32,
    /// Reload-sweep bookkeeping: the current claim epoch, bumped by
    /// [`Self::begin_claim_epoch`] before a script reload rebuilds the
    /// engine. Every `split()` stamps the pane's claim, so after the rebuild
    /// [`Self::sweep_unclaimed`] can close the panes no script attempted to
    /// recreate (e.g. a disabled package's leftover panel).
    claim_epoch: u64,
    /// Each live non-main pane's last-claimed epoch (absent ⇒ epoch 0).
    claimed: HashMap<PaneKey, u64>,
}

impl Default for PaneRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneRegistry {
    /// A fresh registry holding only the main pane (key 0, name id 0).
    #[must_use]
    pub fn new() -> Self {
        let main = PaneDef {
            key: MAIN_PANE_KEY,
            name_id: MAIN_PANE_NAME_ID,
            name: Arc::from(MAIN_PANE_NAME),
            namespace: PaneNamespace::User,
            kind: PaneKind::Terminal,
            is_main: true,
            title_bar: TitleBarPolicy::Normal,
        };
        let mut defs = HashMap::new();
        defs.insert(MAIN_PANE_KEY, main);
        let mut interned = HashMap::new();
        interned.insert(
            (PaneNamespace::User, MAIN_PANE_NAME.to_string()),
            MAIN_PANE_NAME_ID,
        );
        let mut live = HashMap::new();
        live.insert(MAIN_PANE_NAME_ID, MAIN_PANE_KEY);
        Self {
            defs,
            interned,
            live,
            next_key: 1,
            next_name_id: 1,
            claim_epoch: 0,
            claimed: HashMap::new(),
        }
    }

    /// Get-or-create the pane `name` in `namespace`. An existing (folded)
    /// name returns the existing pane. Spec differences on an existing pane
    /// are ignored — except `title_bar`: an explicit policy (`Some`) updates
    /// the live def (the only way to set the main pane's, since main is never
    /// created by a split) and is reported via
    /// [`SplitOutcome::title_bar_changed`]. A kind mismatch throws rather
    /// than silently reusing.
    ///
    /// # Errors
    ///
    /// [`PaneError::InvalidName`]/[`PaneError::ReservedName`] for a name the
    /// rules reject, [`PaneError::KindMismatch`] when the existing pane (or
    /// main) has a different kind, and [`PaneError::CapExceeded`] past the
    /// non-main cap.
    pub fn split(
        &mut self,
        namespace: &PaneNamespace,
        name: &str,
        kind: PaneKind,
        title_bar: Option<TitleBarPolicy>,
    ) -> Result<SplitOutcome, PaneError> {
        validate_name(name)?;
        let folded = fold(name);

        // "main" resolves to the main pane in every namespace.
        if folded == MAIN_PANE_NAME {
            if kind != PaneKind::Terminal {
                return Err(PaneError::KindMismatch(name.to_string()));
            }
            let title_bar_changed = self.apply_title_bar(MAIN_PANE_KEY, title_bar);
            return Ok(SplitOutcome {
                def: self.defs[&MAIN_PANE_KEY].clone(),
                created: false,
                title_bar_changed,
            });
        }
        if RESERVED_MEMBER_NAMES.contains(&folded.as_str()) {
            return Err(PaneError::ReservedName(name.to_string()));
        }

        // Get-or-create by the interned name identity.
        if let Some(&name_id) = self.interned.get(&(namespace.clone(), folded.clone()))
            && let Some(&key) = self.live.get(&name_id)
        {
            if self.defs[&key].kind != kind {
                return Err(PaneError::KindMismatch(name.to_string()));
            }
            self.claimed.insert(key, self.claim_epoch);
            let title_bar_changed = self.apply_title_bar(key, title_bar);
            return Ok(SplitOutcome {
                def: self.defs[&key].clone(),
                created: false,
                title_bar_changed,
            });
        }

        if self.defs.len() > NON_MAIN_PANE_CAP {
            return Err(PaneError::CapExceeded);
        }

        // Interning is creation-only and permanent: reuse the id a prior
        // incarnation of this name held, or mint the next one.
        let name_id = *self
            .interned
            .entry((namespace.clone(), folded))
            .or_insert_with(|| {
                let id = PaneNameId(self.next_name_id);
                self.next_name_id += 1;
                id
            });

        let key = PaneKey(self.next_key);
        self.next_key += 1;

        let def = PaneDef {
            key,
            name_id,
            name: Arc::from(name),
            namespace: namespace.clone(),
            kind,
            is_main: false,
            title_bar: title_bar.unwrap_or_default(),
        };
        self.defs.insert(key, def.clone());
        self.live.insert(name_id, key);
        self.claimed.insert(key, self.claim_epoch);
        Ok(SplitOutcome {
            def,
            created: true,
            title_bar_changed: false,
        })
    }

    /// Start a new claim epoch — called right before a script reload rebuilds
    /// the engine. From here until [`Self::sweep_unclaimed`], any `split()`
    /// (creation or get-or-create hit) counts as the pane being re-claimed by
    /// the reloading scripts.
    pub fn begin_claim_epoch(&mut self) {
        self.claim_epoch += 1;
    }

    /// Close every non-main pane no `split()` re-claimed since the last
    /// [`Self::begin_claim_epoch`], returning the retired keys (sorted, for a
    /// deterministic close order). This is the reload garbage collector: a
    /// pane whose creating script is gone (package disabled, trigger removed)
    /// stops occupying the grid — while everything the reload *did* recreate
    /// keeps its placement untouched. Interned name ids survive as always, so
    /// a later same-name `split()` transparently re-attaches widgets.
    pub fn sweep_unclaimed(&mut self) -> Vec<PaneKey> {
        let mut swept: Vec<PaneKey> = self
            .defs
            .iter()
            .filter(|(key, def)| {
                !def.is_main
                    && self.claimed.get(key).copied().unwrap_or(0) < self.claim_epoch
            })
            .map(|(key, _)| *key)
            .collect();
        swept.sort_unstable();
        for key in &swept {
            if let Some(def) = self.defs.remove(key) {
                self.live.remove(&def.name_id);
            }
            self.claimed.remove(key);
        }
        swept
    }

    /// Apply an explicitly-provided title-bar policy to a live pane's def,
    /// returning whether it actually changed (`None` = the spec omitted the
    /// key = leave the pane's policy alone).
    fn apply_title_bar(&mut self, key: PaneKey, title_bar: Option<TitleBarPolicy>) -> bool {
        match (title_bar, self.defs.get_mut(&key)) {
            (Some(policy), Some(def)) if def.title_bar != policy => {
                def.title_bar = policy;
                true
            }
            _ => false,
        }
    }

    /// Close the pane `name` in `namespace`, returning its retired key.
    /// Closing main throws; a name that is not live is `NoSuchPane` (callers
    /// that want idempotent close treat that as a no-op).
    ///
    /// # Errors
    ///
    /// [`PaneError::CloseMain`] for the main pane, [`PaneError::NoSuchPane`]
    /// when `name` is not a live pane.
    pub fn close(&mut self, namespace: &PaneNamespace, name: &str) -> Result<PaneKey, PaneError> {
        let folded = fold(name);
        if folded == MAIN_PANE_NAME {
            return Err(PaneError::CloseMain);
        }
        let name_id = self
            .interned
            .get(&(namespace.clone(), folded))
            .copied()
            .ok_or_else(|| PaneError::NoSuchPane(name.to_string()))?;
        let key = self
            .live
            .remove(&name_id)
            .ok_or_else(|| PaneError::NoSuchPane(name.to_string()))?;
        self.defs.remove(&key);
        self.claimed.remove(&key);
        Ok(key)
    }

    /// Resolve `name` in `namespace` to its live definition (read-only miss on
    /// unknown names — no interning).
    #[must_use]
    pub fn resolve(&self, namespace: &PaneNamespace, name: &str) -> Option<&PaneDef> {
        let folded = fold(name);
        if folded == MAIN_PANE_NAME {
            return self.defs.get(&MAIN_PANE_KEY);
        }
        let name_id = self.interned.get(&(namespace.clone(), folded))?;
        let key = self.live.get(name_id)?;
        self.defs.get(key)
    }

    /// The live pane currently holding `name_id`, if any — the per-line
    /// integer fast path for `Pane`-handle routing (a recreated same-name
    /// pane transparently reattaches here).
    #[must_use]
    pub fn live_by_name_id(&self, name_id: PaneNameId) -> Option<&PaneDef> {
        let key = self.live.get(&name_id)?;
        self.defs.get(key)
    }

    /// The live definition for `key`, if the pane is still open.
    #[must_use]
    pub fn get(&self, key: PaneKey) -> Option<&PaneDef> {
        self.defs.get(&key)
    }

    /// Whether `key` names a live pane — the sink validation routing applies
    /// when queuing an `AppendTo`.
    #[must_use]
    pub fn is_live(&self, key: PaneKey) -> bool {
        self.defs.contains_key(&key)
    }

    /// Whether any non-main pane is live. The main pane always exists, so this
    /// is just "more than the one". Routing checks it to skip maintaining the
    /// whole-line accumulator (a per-fragment deep copy) when no pane could
    /// ever consume it.
    #[must_use]
    pub fn has_non_main_panes(&self) -> bool {
        self.defs.len() > 1
    }

    /// The live panes visible to `namespace`: its own panes plus the main
    /// pane (which resolves in every namespace). Sorted by key for a stable
    /// script-facing order.
    #[must_use]
    pub fn list(&self, namespace: &PaneNamespace) -> Vec<PaneDef> {
        let mut panes: Vec<PaneDef> = self
            .defs
            .values()
            .filter(|def| def.is_main || def.namespace == *namespace)
            .cloned()
            .collect();
        panes.sort_by_key(|def| def.key);
        panes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> PaneNamespace {
        PaneNamespace::User
    }

    fn pkg(owner: &str, name: &str) -> PaneNamespace {
        PaneNamespace::Package {
            owner: Arc::from(owner),
            name: Arc::from(name),
        }
    }

    #[test]
    fn main_pane_exists_at_construction() {
        let reg = PaneRegistry::new();
        let main = reg.get(MAIN_PANE_KEY).expect("main pane");
        assert!(main.is_main);
        assert_eq!(main.name_id, MAIN_PANE_NAME_ID);
        assert_eq!(main.kind, PaneKind::Terminal);
        assert_eq!(main.title_bar, TitleBarPolicy::Normal);
    }

    #[test]
    fn reload_sweep_closes_only_unclaimed_panes() {
        let mut reg = PaneRegistry::new();
        let stale = reg.split(&user(), "stale", PaneKind::Terminal, None).unwrap();
        let kept = reg.split(&pkg("a", "map"), "map", PaneKind::Widgets, None).unwrap();

        // Nothing to sweep before an epoch begins (session start).
        assert!(reg.sweep_unclaimed().is_empty());

        // Reload: only "map" is re-claimed (a get-or-create hit counts).
        reg.begin_claim_epoch();
        let hit = reg.split(&pkg("a", "map"), "MAP", PaneKind::Widgets, None).unwrap();
        assert!(!hit.created);
        let swept = reg.sweep_unclaimed();
        assert_eq!(swept, vec![stale.def.key]);
        assert!(reg.resolve(&user(), "stale").is_none());
        assert!(!reg.is_live(stale.def.key));
        assert!(reg.is_live(kept.def.key));
        // Main is never swept.
        assert!(reg.is_live(MAIN_PANE_KEY));

        // The interned name identity survives the sweep: a later recreate
        // keeps the widget re-attach id while minting a fresh key.
        let again = reg.split(&user(), "stale", PaneKind::Terminal, None).unwrap();
        assert!(again.created);
        assert_eq!(again.def.name_id, stale.def.name_id);
        assert_ne!(again.def.key, stale.def.key);

        // A pane created after the sweep (mid-session, e.g. by a trigger) is
        // claimed at the current epoch and survives until the NEXT reload
        // fails to re-claim it.
        reg.begin_claim_epoch();
        let swept = reg.sweep_unclaimed();
        assert_eq!(swept.len(), 2, "neither pane was re-claimed this time: {swept:?}");
        assert_eq!(reg.list(&user()).len(), 1, "only main remains");
    }

    #[test]
    fn title_bar_policy_defaults_sets_and_updates() {
        let mut reg = PaneRegistry::new();
        // Omitted => Normal.
        let created = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert_eq!(created.def.title_bar, TitleBarPolicy::Normal);
        assert!(!created.title_bar_changed);

        // Explicit on creation.
        let pinned = reg
            .split(&user(), "map", PaneKind::Widgets, Some(TitleBarPolicy::AlwaysShow))
            .unwrap();
        assert_eq!(pinned.def.title_bar, TitleBarPolicy::AlwaysShow);
        assert!(!pinned.title_bar_changed);

        // Get-or-create with the key omitted leaves the policy alone...
        let hit = reg.split(&user(), "map", PaneKind::Widgets, None).unwrap();
        assert_eq!(hit.def.title_bar, TitleBarPolicy::AlwaysShow);
        assert!(!hit.title_bar_changed);
        // ...an explicit differing policy updates it and reports the change...
        let updated = reg
            .split(&user(), "chat", PaneKind::Terminal, Some(TitleBarPolicy::AlwaysShow))
            .unwrap();
        assert!(!updated.created);
        assert!(updated.title_bar_changed);
        assert_eq!(updated.def.title_bar, TitleBarPolicy::AlwaysShow);
        // ...and an explicit same policy is not a change.
        let same = reg
            .split(&user(), "chat", PaneKind::Terminal, Some(TitleBarPolicy::AlwaysShow))
            .unwrap();
        assert!(!same.title_bar_changed);
    }

    #[test]
    fn title_bar_policy_is_settable_on_main() {
        let mut reg = PaneRegistry::new();
        let outcome = reg
            .split(&user(), "main", PaneKind::Terminal, Some(TitleBarPolicy::AlwaysShow))
            .unwrap();
        assert!(!outcome.created);
        assert!(outcome.title_bar_changed);
        assert_eq!(
            reg.get(MAIN_PANE_KEY).unwrap().title_bar,
            TitleBarPolicy::AlwaysShow
        );
        // A recreated pane starts from its own spec, not the retired def's.
        reg.split(&user(), "chat", PaneKind::Terminal, Some(TitleBarPolicy::AlwaysShow))
            .unwrap();
        reg.close(&user(), "chat").unwrap();
        let again = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert_eq!(again.def.title_bar, TitleBarPolicy::Normal);
    }

    #[test]
    fn split_is_get_or_create_with_case_folding() {
        let mut reg = PaneRegistry::new();
        let first = reg.split(&user(), "Chat", PaneKind::Terminal, None).unwrap();
        assert!(first.created);
        // Display case is preserved; identity is folded.
        assert_eq!(&*first.def.name, "Chat");
        let second = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert!(!second.created);
        assert_eq!(second.def.key, first.def.key);
        assert_eq!(second.def.name_id, first.def.name_id);
    }

    #[test]
    fn kind_mismatch_throws() {
        let mut reg = PaneRegistry::new();
        reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert_eq!(
            reg.split(&user(), "chat", PaneKind::Widgets, None),
            Err(PaneError::KindMismatch("chat".to_string()))
        );
    }

    #[test]
    fn namespaces_are_isolated() {
        let mut reg = PaneRegistry::new();
        let a = reg.split(&pkg("alice", "chat-pkg"), "chat", PaneKind::Terminal, None).unwrap();
        let b = reg.split(&pkg("bob", "chat-pkg"), "chat", PaneKind::Terminal, None).unwrap();
        let c = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert_ne!(a.def.key, b.def.key);
        assert_ne!(a.def.key, c.def.key);
        assert!(reg.resolve(&user(), "chat").is_some());
        assert_eq!(reg.resolve(&user(), "chat").unwrap().key, c.def.key);
        // list() shows own panes plus main only.
        let listed = reg.list(&user());
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|d| d.is_main));
        assert!(listed.iter().any(|d| d.key == c.def.key));
    }

    #[test]
    fn main_resolves_in_every_namespace_and_cannot_close() {
        let mut reg = PaneRegistry::new();
        let via_pkg = reg.split(&pkg("a", "b"), "MAIN", PaneKind::Terminal, None).unwrap();
        assert!(!via_pkg.created);
        assert!(via_pkg.def.is_main);
        assert_eq!(reg.resolve(&pkg("a", "b"), "main").unwrap().key, MAIN_PANE_KEY);
        assert_eq!(reg.close(&user(), "main"), Err(PaneError::CloseMain));
        assert_eq!(
            reg.split(&user(), "main", PaneKind::Widgets, None),
            Err(PaneError::KindMismatch("main".to_string()))
        );
    }

    #[test]
    fn close_retires_key_and_recreate_keeps_name_id() {
        let mut reg = PaneRegistry::new();
        let first = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        let closed_key = reg.close(&user(), "chat").unwrap();
        assert_eq!(closed_key, first.def.key);
        assert!(!reg.is_live(closed_key));
        assert!(reg.resolve(&user(), "chat").is_none());

        let again = reg.split(&user(), "chat", PaneKind::Terminal, None).unwrap();
        assert!(again.created);
        // Fresh incarnation key (never reused)...
        assert_ne!(again.def.key, first.def.key);
        // ...but the interned name identity is stable (the widget re-attach identity).
        assert_eq!(again.def.name_id, first.def.name_id);
        assert_eq!(
            reg.live_by_name_id(first.def.name_id).unwrap().key,
            again.def.key
        );
    }

    #[test]
    fn close_unknown_is_no_such_pane() {
        let mut reg = PaneRegistry::new();
        assert_eq!(
            reg.close(&user(), "ghost"),
            Err(PaneError::NoSuchPane("ghost".to_string()))
        );
    }

    #[test]
    fn cap_is_enforced_across_kinds() {
        let mut reg = PaneRegistry::new();
        for i in 0..NON_MAIN_PANE_CAP {
            let kind = if i % 2 == 0 { PaneKind::Terminal } else { PaneKind::Widgets };
            reg.split(&user(), &format!("p{i}"), kind, None).unwrap();
        }
        assert_eq!(
            reg.split(&user(), "overflow", PaneKind::Terminal, None),
            Err(PaneError::CapExceeded)
        );
        // Get-or-create of an existing pane still works at the cap.
        assert!(!reg.split(&user(), "p0", PaneKind::Terminal, None).unwrap().created);
        // Closing one frees a slot.
        reg.close(&user(), "p0").unwrap();
        assert!(reg.split(&user(), "overflow", PaneKind::Terminal, None).unwrap().created);
    }

    #[test]
    fn name_validation_and_reserved_names() {
        let mut reg = PaneRegistry::new();
        assert!(matches!(
            reg.split(&user(), "", PaneKind::Terminal, None),
            Err(PaneError::InvalidName(_))
        ));
        let long = "x".repeat(65);
        assert!(matches!(
            reg.split(&user(), &long, PaneKind::Terminal, None),
            Err(PaneError::InvalidName(_))
        ));
        assert!(matches!(
            reg.split(&user(), "a\nb", PaneKind::Terminal, None),
            Err(PaneError::InvalidName(_))
        ));
        for reserved in ["get", "list", "exists", "then", "GET"] {
            assert!(matches!(
                reg.split(&user(), reserved, PaneKind::Terminal, None),
                Err(PaneError::ReservedName(_))
            ), "{reserved} should be reserved");
        }
        // Reserved names are read-only misses on lookup.
        assert!(reg.resolve(&user(), "get").is_none());
    }
}
