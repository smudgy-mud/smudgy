//! The session's pane registry: pane *existence* lives here on the session
//! thread (scripts and line routing run here); pane *placement* (grids,
//! windows) lives in the UI. See `docs/panes.md` §2.2.
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

#[cfg(test)]
impl PaneKey {
    /// Mint an arbitrary key for unit tests of key-agnostic containers; real
    /// keys only ever come from the registry.
    pub(crate) fn from_raw_for_tests(raw: u32) -> Self {
        Self(raw)
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

/// A pane-hosted input line (`docs/input.md` §3.7): the pane owns
/// its own `SessionInput` under its body, whose submissions are delivered to
/// the creating script's `onSubmit` handler instead of the session pipeline.
/// The handler itself never lives here — it is a v8 function registered
/// beside the split (engine-generation state); this is the display half the
/// UI builds the input from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInputDef {
    /// Hint text shown while the input is empty.
    pub placeholder: Option<Arc<str>>,
}

/// One live pane's definition, mirrored to the UI via `PaneOpened` (and
/// re-mirrored via `PaneUpdated` when a mutable def-state field —
/// `title_bar`, `hidden`, `font_size` — changes on an existing pane).
#[derive(Debug, Clone, PartialEq)]
pub struct PaneDef {
    pub key: PaneKey,
    pub name_id: PaneNameId,
    /// Display-cased name as the creator wrote it (identity is the folded form).
    pub name: Arc<str>,
    pub namespace: PaneNamespace,
    pub kind: PaneKind,
    pub is_main: bool,
    /// Header visibility policy. A def-state field `split()` *updates* on an
    /// existing pane (when given explicitly) — which is also the only way to
    /// set it on the main pane, whose def otherwise exists from construction.
    pub title_bar: TitleBarPolicy,
    /// The title-bar eyeball's toggle state — a soft display state (the pane
    /// keeps running; a hidden pane drops from the collapsed-toolbar grid).
    /// This is the source of truth the UI's `hidden_panes` derives from; user
    /// eyeball clicks report back here through `RuntimeAction::PaneUserHidden`.
    /// Always `false` for main via scripts (hiding main throws); the user's
    /// eyeball may still set it.
    pub hidden: bool,
    /// Per-pane font size override in px (validated to the same
    /// 8–40 range as the global setting); `None` follows the global setting.
    /// Applies to the pane's scrollback and input, including inputs hosted by
    /// widgets-only panes.
    pub font_size: Option<f32>,
    /// The pane's own input line, when the creating spec asked for one.
    /// Creation-time identity, like `kind`: a `split()` hitting an existing
    /// pane without one while asking for one throws rather than silently
    /// reusing. Both pane kinds may host an input. Main carries `None` —
    /// its fused input is the session's command input, not a pane input.
    pub input: Option<PaneInputDef>,
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

/// Where a pane moved into a tab group lands relative to its reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabPosition {
    Before,
    After,
    End,
}

impl TabPosition {
    /// Parse the script-facing string union (`'before'|'after'|'end'`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            "end" => Some(Self::End),
            _ => None,
        }
    }
}

/// Where the UI should place a freshly created pane: either split beside a
/// reference group or insert into the reference's current tab group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanePlacement {
    Split {
        reference: PaneKey,
        direction: SplitDirection,
        size_px: Option<f32>,
    },
    Tab {
        reference: PaneKey,
        position: TabPosition,
        selected: bool,
    },
}

impl PanePlacement {
    #[must_use]
    pub fn reference(self) -> PaneKey {
        match self {
            Self::Split { reference, .. } | Self::Tab { reference, .. } => reference,
        }
    }

    #[must_use]
    pub fn with_reference(self, reference: PaneKey) -> Self {
        match self {
            Self::Split {
                direction,
                size_px,
                ..
            } => Self::Split {
                reference,
                direction,
                size_px,
            },
            Self::Tab {
                position,
                selected,
                ..
            } => Self::Tab {
                reference,
                position,
                selected,
            },
        }
    }
}

/// Why a registry mutation was refused. Surfaced to scripts as a thrown error
/// naming the rule, so a denied call is an author bug they can fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PaneError {
    #[error("invalid pane name: {0}")]
    InvalidName(String),
    #[error("'{0}' is a reserved pane name")]
    ReservedName(String),
    #[error("pane cap exceeded: at most {NON_MAIN_PANE_CAP} non-main panes per session")]
    CapExceeded,
    #[error("pane '{0}' already exists with a different kind")]
    KindMismatch(String),
    #[error(
        "pane '{0}' already exists without an input; close it first to recreate it with one"
    )]
    InputMismatch(String),
    #[error("the main pane's input is the session's command input (session.input), not a pane input")]
    MainInput,
    #[error("the main pane cannot be closed")]
    CloseMain,
    #[error("the main pane cannot be hidden")]
    HideMain,
    #[error("the main pane cannot be resized (resize the sibling pane instead)")]
    ResizeMain,
    #[error("the main pane cannot be relocated")]
    RelocateMain,
    #[error("the main pane cannot be torn out")]
    TearOutMain,
    #[error("invalid font size {0}: must be between {MIN_PANE_FONT_SIZE} and {MAX_PANE_FONT_SIZE}")]
    InvalidFontSize(f32),
    #[error("no pane named '{0}'")]
    NoSuchPane(String),
}

/// The per-pane font override's valid range — the same bounds the Settings
/// window enforces on the global terminal font size (`ui/src/prefs.rs`).
/// Scripts get a thrown error rather than a silent clamp: explicit beats
/// clamp in an API.
pub const MIN_PANE_FONT_SIZE: f32 = 8.0;
pub const MAX_PANE_FONT_SIZE: f32 = 40.0;

/// Validate a script-supplied font override.
///
/// # Errors
///
/// [`PaneError::InvalidFontSize`] outside the settings range (or non-finite).
pub fn validate_font_size(px: f32) -> Result<(), PaneError> {
    if !px.is_finite() || !(MIN_PANE_FONT_SIZE..=MAX_PANE_FONT_SIZE).contains(&px) {
        return Err(PaneError::InvalidFontSize(px));
    }
    Ok(())
}

/// One pane's last laid-out pixel size, as mirrored from the UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneSize {
    pub width: f32,
    pub height: f32,
}

/// The session-thread cache of pane pixel sizes — the read-back half of the
/// placement surface. Pane sizes are UI-authoritative (ratios, user drags),
/// so the UI's `PaneMirrorFeed` sends coalesced `PaneDisplayChanged` updates
/// here and the `pane.size` read op consults the cache synchronously; the
/// `pane:resize` host event derives from the same feed's edges.
///
/// Interest-gated exactly like the input mirror (`docs/input.md` §3.3): the
/// UI sends size messages only after the session thread has flagged interest
/// (the first `pane.size` read, or a `pane:resize` subscription), then pushes
/// a baseline for every pane. Session-scoped — interest is a session fact and
/// sizes outlive engine reloads like the registry itself. Entries are purged
/// when their pane closes (keys are never reused, so a stale entry could
/// never be read again anyway).
#[derive(Debug, Default)]
pub struct PaneSizeMirror {
    sizes: HashMap<PaneKey, PaneSize>,
    interest: bool,
}

impl PaneSizeMirror {
    /// The last mirrored size for `key`, if any layout has been reported.
    #[must_use]
    pub fn get(&self, key: PaneKey) -> Option<PaneSize> {
        self.sizes.get(&key).copied()
    }

    /// Apply one UI report, returning the prior size. `None` means this was
    /// the pane's baseline report — callers treat that as a seed, not an
    /// edge (no `pane:resize` fires for state that merely predates the
    /// subscription), mirroring the input mirror's baseline rule.
    pub fn apply(&mut self, key: PaneKey, size: PaneSize) -> Option<PaneSize> {
        self.sizes.insert(key, size)
    }

    /// Drop a closed pane's entry (keys are never reused).
    pub fn remove(&mut self, key: PaneKey) {
        self.sizes.remove(&key);
    }

    #[must_use]
    pub fn interest(&self) -> bool {
        self.interest
    }

    /// Flag interest; returns `true` on the flip (the caller then tells the
    /// UI once — interest never clears).
    pub fn flag_interest(&mut self) -> bool {
        let flipped = !self.interest;
        self.interest = true;
        flipped
    }
}

/// The explicit def-state fields of a split spec. `None` = the spec omitted
/// the key = leave an existing pane's state alone (and take the default on
/// creation); `Some` updates an existing pane — the restatement rule that
/// makes reload re-claims keep the user's eyeball toggle unless the spec
/// says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DefStateSpec {
    pub title_bar: Option<TitleBarPolicy>,
    pub hidden: Option<bool>,
    pub font_size: Option<f32>,
}

/// The result of a `split()`: the pane's definition, whether this call
/// created it (false ⇒ get-or-create returned the existing pane), and whether
/// an explicit def-state field in the spec (`titleBar`, `hidden`, `fontSize`)
/// changed an existing pane's def (the caller then mirrors the def to the UI
/// via `PaneUpdated`).
#[derive(Debug, Clone, PartialEq)]
pub struct SplitOutcome {
    pub def: PaneDef,
    pub created: bool,
    pub def_changed: bool,
    /// Whether the change included the `hidden` toggle — the caller then also
    /// fires the `pane:visibility` host event (every actual toggle fires it,
    /// whatever the spelling: spec restatement, `hide()`/`show()`, eyeball).
    pub hidden_changed: bool,
}

/// Registry member names reserved so `session.panes.<name>` dot access can
/// never be shadowed by a pane (`then` additionally keeps the proxy
/// non-thenable under `await`). Reserved-name panes cannot be created;
/// lookups for them are plain read misses.
const RESERVED_MEMBER_NAMES: [&str; 4] = ["get", "list", "exists", "then"];

/// Case-fold a pane name to its identity form. Pane identity is
/// case-insensitive (chosen now because MXP FRAME names are case-insensitive,
/// and changing identity later would break get-or-create).
///
/// Public because persisted pane descriptors share this identity: anything
/// that stores a pane name durably folds it through this same function, so a
/// stored descriptor and a live registry entry can never disagree on identity.
#[must_use]
pub fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Whether `name` folds to the reserved main-pane name — which resolves to
/// the main pane in *every* namespace, so callers that treat main specially
/// can fork on the name without a registry lookup.
#[must_use]
pub fn is_main_pane_name(name: &str) -> bool {
    fold(name) == MAIN_PANE_NAME
}

/// Validate a pane name: non-empty, ≤ 64 chars, printable (no control
/// characters, which covers newlines). `split()` enforces this at creation;
/// consumers of persisted pane descriptors apply the same rule, so a stored
/// name is valid exactly when the registry would have accepted it.
///
/// # Errors
///
/// [`PaneError::InvalidName`] naming the violated rule.
pub fn validate_name(name: &str) -> Result<(), PaneError> {
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

/// The session's data-only pane registry. An `Arc<Mutex<PaneRegistry>>` on the
/// runtime is shared into every isolate's `OpState`, so own- and same-server
/// foreign pane ops can resolve and mutate it synchronously without sharing
/// V8 state across threads. It is preserved across script reloads (like
/// `recent_lines`), which is what makes "panes survive reloads" true.
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
            hidden: false,
            font_size: None,
            input: None,
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
    /// are ignored — except the def-state fields `title_bar`, `hidden`, and
    /// `font_size`: an explicit value (`Some`) updates the live def (for
    /// `title_bar`/`font_size` also the only spec route onto the main pane,
    /// since main is never created by a split) and is reported via
    /// [`SplitOutcome::def_changed`]. Placement fields (`width`/`height`)
    /// stay creation-only — `resize()` is the post-creation spelling. A kind
    /// mismatch throws rather than silently reusing. An explicit `hidden` on
    /// main throws [`PaneError::HideMain`] — scripts never drive main's
    /// visibility, though the user's eyeball may.
    ///
    /// `input` is creation-time identity like `kind`: a spec asking for an
    /// input on an existing pane that has none throws (the input line is part
    /// of what the pane *is*), while a spec omitting it on a pane that has
    /// one is an ignored difference — the reload re-claim path, where the
    /// handler re-registers beside the split. Placeholder differences on an
    /// existing input are ignored like any other spec field.
    ///
    /// # Errors
    ///
    /// [`PaneError::InvalidName`]/[`PaneError::ReservedName`] for a name the
    /// rules reject, [`PaneError::KindMismatch`] when the existing pane (or
    /// main) has a different kind, [`PaneError::InputMismatch`]/
    /// [`PaneError::MainInput`] when the spec asks for an input the target
    /// cannot host, and [`PaneError::CapExceeded`] past the non-main cap.
    pub fn split(
        &mut self,
        namespace: &PaneNamespace,
        name: &str,
        kind: PaneKind,
        def_state: DefStateSpec,
        input: Option<PaneInputDef>,
    ) -> Result<SplitOutcome, PaneError> {
        validate_name(name)?;
        if let Some(px) = def_state.font_size {
            validate_font_size(px)?;
        }
        let folded = fold(name);

        // "main" resolves to the main pane in every namespace.
        if folded == MAIN_PANE_NAME {
            if kind != PaneKind::Terminal {
                return Err(PaneError::KindMismatch(name.to_string()));
            }
            if input.is_some() {
                return Err(PaneError::MainInput);
            }
            if def_state.hidden.is_some() {
                return Err(PaneError::HideMain);
            }
            let (def_changed, _) = self.apply_def_state(
                MAIN_PANE_KEY,
                DefStateSpec {
                    hidden: None,
                    ..def_state
                },
            );
            return Ok(SplitOutcome {
                def: self.defs[&MAIN_PANE_KEY].clone(),
                created: false,
                def_changed,
                hidden_changed: false,
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
            if input.is_some() && self.defs[&key].input.is_none() {
                return Err(PaneError::InputMismatch(name.to_string()));
            }
            self.claimed.insert(key, self.claim_epoch);
            let (def_changed, hidden_changed) = self.apply_def_state(key, def_state);
            return Ok(SplitOutcome {
                def: self.defs[&key].clone(),
                created: false,
                def_changed,
                hidden_changed,
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
            title_bar: def_state.title_bar.unwrap_or_default(),
            hidden: def_state.hidden.unwrap_or(false),
            font_size: def_state.font_size,
            input,
        };
        self.defs.insert(key, def.clone());
        self.live.insert(name_id, key);
        self.claimed.insert(key, self.claim_epoch);
        Ok(SplitOutcome {
            def,
            created: true,
            def_changed: false,
            hidden_changed: false,
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

    /// Apply explicitly-provided def-state fields to a live pane's def,
    /// returning whether anything actually changed (`None` = the spec omitted
    /// the key = leave the pane's state alone). Spec-borne `font_size` can
    /// only set a value; reverting to the global setting is `setFontSize(null)`
    /// via [`Self::set_font_size`].
    fn apply_def_state(&mut self, key: PaneKey, def_state: DefStateSpec) -> (bool, bool) {
        let Some(def) = self.defs.get_mut(&key) else {
            return (false, false);
        };
        let mut changed = false;
        let mut hidden_changed = false;
        if let Some(policy) = def_state.title_bar
            && def.title_bar != policy
        {
            def.title_bar = policy;
            changed = true;
        }
        if let Some(hidden) = def_state.hidden
            && def.hidden != hidden
        {
            def.hidden = hidden;
            changed = true;
            hidden_changed = true;
        }
        if let Some(px) = def_state.font_size
            && def.font_size != Some(px)
        {
            def.font_size = Some(px);
            changed = true;
        }
        (changed, hidden_changed)
    }

    /// Set a pane's hidden state — the script `hide()`/`show()` spelling.
    /// Returns the updated def when the state actually changed, `None` when
    /// it already was so (callers skip the `PaneUpdated` mirror then).
    ///
    /// # Errors
    ///
    /// [`PaneError::HideMain`] — scripts never drive main's visibility (in
    /// either direction; the user's eyeball owns it);
    /// [`PaneError::NoSuchPane`] when `name` is not a live pane.
    pub fn set_hidden(
        &mut self,
        namespace: &PaneNamespace,
        name: &str,
        hidden: bool,
    ) -> Result<Option<PaneDef>, PaneError> {
        if is_main_pane_name(name) {
            return Err(PaneError::HideMain);
        }
        let key = self
            .resolve(namespace, name)
            .map(|def| def.key)
            .ok_or_else(|| PaneError::NoSuchPane(name.to_string()))?;
        let (changed, _) = self.apply_def_state(
            key,
            DefStateSpec {
                hidden: Some(hidden),
                ..DefStateSpec::default()
            },
        );
        Ok(changed.then(|| self.defs[&key].clone()))
    }

    /// Set a pane's hidden state by key with no main guard — the user
    /// eyeball's path (`RuntimeAction::PaneUserHidden`), which may hide any
    /// pane including main. Returns the updated def when the state actually
    /// changed; a retired key or an already-matching state is `None` (the
    /// caller drops the report whole).
    pub fn set_hidden_by_key(&mut self, key: PaneKey, hidden: bool) -> Option<PaneDef> {
        let def = self.defs.get_mut(&key)?;
        if def.hidden == hidden {
            return None;
        }
        def.hidden = hidden;
        Some(def.clone())
    }

    /// Set (or with `None` clear) a pane's terminal font override — the
    /// script `setFontSize()` spelling. Allowed on main: the one script
    /// mutator main accepts, a per-session override of the global preference
    /// (the op layer adds the `change-display` gate for that case). Returns
    /// the updated def when the value actually changed.
    ///
    /// # Errors
    ///
    /// [`PaneError::InvalidFontSize`] outside the settings range,
    /// [`PaneError::NoSuchPane`] when `name` is not a live pane.
    pub fn set_font_size(
        &mut self,
        namespace: &PaneNamespace,
        name: &str,
        font_size: Option<f32>,
    ) -> Result<Option<PaneDef>, PaneError> {
        if let Some(px) = font_size {
            validate_font_size(px)?;
        }
        let key = self
            .resolve(namespace, name)
            .map(|def| def.key)
            .ok_or_else(|| PaneError::NoSuchPane(name.to_string()))?;
        let Some(def) = self.defs.get_mut(&key) else {
            return Err(PaneError::NoSuchPane(name.to_string()));
        };
        if def.font_size == font_size {
            return Ok(None);
        }
        def.font_size = font_size;
        Ok(Some(def.clone()))
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
        let stale = reg.split(&user(), "stale", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        let kept = reg.split(&pkg("a", "map"), "map", PaneKind::Widgets, DefStateSpec::default(), None).unwrap();

        // Nothing to sweep before an epoch begins (session start).
        assert!(reg.sweep_unclaimed().is_empty());

        // Reload: only "map" is re-claimed (a get-or-create hit counts).
        reg.begin_claim_epoch();
        let hit = reg.split(&pkg("a", "map"), "MAP", PaneKind::Widgets, DefStateSpec::default(), None).unwrap();
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
        let again = reg.split(&user(), "stale", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
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
        let created = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert_eq!(created.def.title_bar, TitleBarPolicy::Normal);
        assert!(!created.def_changed);

        // Explicit on creation.
        let pinned = reg
            .split(&user(), "map", PaneKind::Widgets, DefStateSpec { title_bar: Some(TitleBarPolicy::AlwaysShow), ..Default::default() }, None)
            .unwrap();
        assert_eq!(pinned.def.title_bar, TitleBarPolicy::AlwaysShow);
        assert!(!pinned.def_changed);

        // Get-or-create with the key omitted leaves the policy alone...
        let hit = reg.split(&user(), "map", PaneKind::Widgets, DefStateSpec::default(), None).unwrap();
        assert_eq!(hit.def.title_bar, TitleBarPolicy::AlwaysShow);
        assert!(!hit.def_changed);
        // ...an explicit differing policy updates it and reports the change...
        let updated = reg
            .split(&user(), "chat", PaneKind::Terminal, DefStateSpec { title_bar: Some(TitleBarPolicy::AlwaysShow), ..Default::default() }, None)
            .unwrap();
        assert!(!updated.created);
        assert!(updated.def_changed);
        assert_eq!(updated.def.title_bar, TitleBarPolicy::AlwaysShow);
        // ...and an explicit same policy is not a change.
        let same = reg
            .split(&user(), "chat", PaneKind::Terminal, DefStateSpec { title_bar: Some(TitleBarPolicy::AlwaysShow), ..Default::default() }, None)
            .unwrap();
        assert!(!same.def_changed);
    }

    #[test]
    fn title_bar_policy_is_settable_on_main() {
        let mut reg = PaneRegistry::new();
        let outcome = reg
            .split(&user(), "main", PaneKind::Terminal, DefStateSpec { title_bar: Some(TitleBarPolicy::AlwaysShow), ..Default::default() }, None)
            .unwrap();
        assert!(!outcome.created);
        assert!(outcome.def_changed);
        assert_eq!(
            reg.get(MAIN_PANE_KEY).unwrap().title_bar,
            TitleBarPolicy::AlwaysShow
        );
        // A recreated pane starts from its own spec, not the retired def's.
        reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec { title_bar: Some(TitleBarPolicy::AlwaysShow), ..Default::default() }, None)
            .unwrap();
        reg.close(&user(), "chat").unwrap();
        let again = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert_eq!(again.def.title_bar, TitleBarPolicy::Normal);
    }

    #[test]
    fn split_is_get_or_create_with_case_folding() {
        let mut reg = PaneRegistry::new();
        let first = reg.split(&user(), "Chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert!(first.created);
        // Display case is preserved; identity is folded.
        assert_eq!(&*first.def.name, "Chat");
        let second = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert!(!second.created);
        assert_eq!(second.def.key, first.def.key);
        assert_eq!(second.def.name_id, first.def.name_id);
    }

    #[test]
    fn kind_mismatch_throws() {
        let mut reg = PaneRegistry::new();
        reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert_eq!(
            reg.split(&user(), "chat", PaneKind::Widgets, DefStateSpec::default(), None),
            Err(PaneError::KindMismatch("chat".to_string()))
        );
    }

    fn input_def(placeholder: Option<&str>) -> PaneInputDef {
        PaneInputDef {
            placeholder: placeholder.map(Arc::from),
        }
    }

    #[test]
    fn hidden_follows_the_def_state_restatement_rule() {
        let mut reg = PaneRegistry::new();
        // Pre-hidden creation (reveal-on-event panes never flash at load).
        let created = reg
            .split(&user(), "combat", PaneKind::Terminal, DefStateSpec { hidden: Some(true), ..Default::default() }, None)
            .unwrap();
        assert!(created.created && created.def.hidden);
        assert!(!created.hidden_changed, "creation is not a toggle");

        // Omitted hidden on a re-claim keeps the toggle (the user's included).
        let hit = reg
            .split(&user(), "combat", PaneKind::Terminal, DefStateSpec::default(), None)
            .unwrap();
        assert!(hit.def.hidden && !hit.def_changed);

        // Explicit restatement updates and reports the toggle.
        let shown = reg
            .split(&user(), "combat", PaneKind::Terminal, DefStateSpec { hidden: Some(false), ..Default::default() }, None)
            .unwrap();
        assert!(!shown.def.hidden && shown.def_changed && shown.hidden_changed);

        // set_hidden: Some(def) on a change, None when already so.
        assert!(reg.set_hidden(&user(), "combat", true).unwrap().is_some());
        assert!(reg.set_hidden(&user(), "combat", true).unwrap().is_none());
        assert_eq!(
            reg.set_hidden(&user(), "gone", true),
            Err(PaneError::NoSuchPane("gone".to_string()))
        );

        // Scripts never drive main's visibility, in either direction...
        assert_eq!(reg.set_hidden(&user(), "main", true), Err(PaneError::HideMain));
        assert_eq!(reg.set_hidden(&user(), "MAIN", false), Err(PaneError::HideMain));
        assert_eq!(
            reg.split(&user(), "main", PaneKind::Terminal, DefStateSpec { hidden: Some(false), ..Default::default() }, None),
            Err(PaneError::HideMain)
        );
        // ...but the user's eyeball (the by-key path) may hide any pane.
        assert!(reg.set_hidden_by_key(MAIN_PANE_KEY, true).is_some());
        assert!(reg.set_hidden_by_key(MAIN_PANE_KEY, true).is_none(), "already so");
        assert!(reg.get(MAIN_PANE_KEY).unwrap().hidden);
        // A retired key drops the report whole.
        let key = reg.resolve(&user(), "combat").unwrap().key;
        reg.close(&user(), "combat").unwrap();
        assert!(reg.set_hidden_by_key(key, false).is_none());
    }

    #[test]
    fn font_size_is_validated_and_main_accepts_it() {
        let mut reg = PaneRegistry::new();
        assert_eq!(
            reg.split(&user(), "big", PaneKind::Terminal, DefStateSpec { font_size: Some(64.0), ..Default::default() }, None),
            Err(PaneError::InvalidFontSize(64.0))
        );
        let created = reg
            .split(&user(), "big", PaneKind::Terminal, DefStateSpec { font_size: Some(24.0), ..Default::default() }, None)
            .unwrap();
        assert_eq!(created.def.font_size, Some(24.0));

        // Explicit restatement updates; omitted keeps; there is no spec
        // spelling for "revert" — that is set_font_size(None).
        let hit = reg
            .split(&user(), "big", PaneKind::Terminal, DefStateSpec::default(), None)
            .unwrap();
        assert_eq!(hit.def.font_size, Some(24.0));
        let updated = reg
            .split(&user(), "big", PaneKind::Terminal, DefStateSpec { font_size: Some(12.0), ..Default::default() }, None)
            .unwrap();
        assert!(updated.def_changed && !updated.hidden_changed);
        assert_eq!(updated.def.font_size, Some(12.0));
        assert!(reg.set_font_size(&user(), "big", None).unwrap().is_some());
        assert_eq!(reg.resolve(&user(), "big").unwrap().font_size, None);

        // Main accepts the override (the op layer adds the change-display
        // gate); the same value twice is no news.
        assert!(reg.set_font_size(&user(), "main", Some(18.0)).unwrap().is_some());
        assert!(reg.set_font_size(&user(), "MAIN", Some(18.0)).unwrap().is_none());
        assert_eq!(reg.get(MAIN_PANE_KEY).unwrap().font_size, Some(18.0));
        assert_eq!(
            reg.set_font_size(&user(), "main", Some(7.0)),
            Err(PaneError::InvalidFontSize(7.0))
        );
    }

    #[test]
    fn size_mirror_baselines_edges_and_purges() {
        let mut mirror = PaneSizeMirror::default();
        // Interest flips once and sticks.
        assert!(mirror.flag_interest());
        assert!(!mirror.flag_interest());
        assert!(mirror.interest());

        let key = PaneKey::from_raw_for_tests(3);
        assert_eq!(mirror.get(key), None, "no size before the first report");
        // The first report is a baseline (no prior), the second an edge.
        let size = PaneSize { width: 320.0, height: 200.0 };
        assert_eq!(mirror.apply(key, size), None);
        assert_eq!(mirror.get(key), Some(size));
        let grown = PaneSize { width: 400.0, height: 200.0 };
        assert_eq!(mirror.apply(key, grown), Some(size));
        // A close purges; the next same-key report would be a baseline again
        // (keys are never reused, so this is belt-and-braces).
        mirror.remove(key);
        assert_eq!(mirror.get(key), None);
        assert_eq!(mirror.apply(key, size), None);
    }

    #[test]
    fn input_is_creation_time_identity() {
        let mut reg = PaneRegistry::new();
        // Both pane kinds may host an input.
        let chat = reg
            .split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), Some(input_def(Some("say..."))))
            .unwrap();
        assert_eq!(
            chat.def.input,
            Some(input_def(Some("say..."))),
            "the created def carries the input"
        );
        let notes = reg
            .split(&user(), "notes", PaneKind::Widgets, DefStateSpec::default(), Some(input_def(None)))
            .unwrap();
        assert!(notes.def.input.is_some());

        // A spec omitting the input on a pane that has one is an ignored
        // difference (the reload re-claim path); the input survives.
        let hit = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert!(!hit.created);
        assert!(hit.def.input.is_some());

        // Placeholder differences on an existing input are ignored too.
        let re_hit = reg
            .split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), Some(input_def(Some("other"))))
            .unwrap();
        assert_eq!(re_hit.def.input, Some(input_def(Some("say..."))));

        // Asking for an input on an existing pane without one throws.
        reg.split(&user(), "plain", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert_eq!(
            reg.split(&user(), "plain", PaneKind::Terminal, DefStateSpec::default(), Some(input_def(None))),
            Err(PaneError::InputMismatch("plain".to_string()))
        );

        // Main's fused input is the session input, never a pane input.
        assert_eq!(
            reg.split(&user(), "main", PaneKind::Terminal, DefStateSpec::default(), Some(input_def(None))),
            Err(PaneError::MainInput)
        );

        // Close-and-recreate starts from the new spec, like title_bar.
        reg.close(&user(), "chat").unwrap();
        let again = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert!(again.created);
        assert!(again.def.input.is_none(), "a recreated pane starts from its own spec");
    }

    #[test]
    fn namespaces_are_isolated() {
        let mut reg = PaneRegistry::new();
        let a = reg.split(&pkg("alice", "chat-pkg"), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        let b = reg.split(&pkg("bob", "chat-pkg"), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        let c = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
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
        let via_pkg = reg.split(&pkg("a", "b"), "MAIN", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        assert!(!via_pkg.created);
        assert!(via_pkg.def.is_main);
        assert_eq!(reg.resolve(&pkg("a", "b"), "main").unwrap().key, MAIN_PANE_KEY);
        assert_eq!(reg.close(&user(), "main"), Err(PaneError::CloseMain));
        assert_eq!(
            reg.split(&user(), "main", PaneKind::Widgets, DefStateSpec::default(), None),
            Err(PaneError::KindMismatch("main".to_string()))
        );
    }

    #[test]
    fn close_retires_key_and_recreate_keeps_name_id() {
        let mut reg = PaneRegistry::new();
        let first = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
        let closed_key = reg.close(&user(), "chat").unwrap();
        assert_eq!(closed_key, first.def.key);
        assert!(!reg.is_live(closed_key));
        assert!(reg.resolve(&user(), "chat").is_none());

        let again = reg.split(&user(), "chat", PaneKind::Terminal, DefStateSpec::default(), None).unwrap();
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
            reg.split(&user(), &format!("p{i}"), kind, DefStateSpec::default(), None).unwrap();
        }
        assert_eq!(
            reg.split(&user(), "overflow", PaneKind::Terminal, DefStateSpec::default(), None),
            Err(PaneError::CapExceeded)
        );
        // Get-or-create of an existing pane still works at the cap.
        assert!(!reg.split(&user(), "p0", PaneKind::Terminal, DefStateSpec::default(), None).unwrap().created);
        // Closing one frees a slot.
        reg.close(&user(), "p0").unwrap();
        assert!(reg.split(&user(), "overflow", PaneKind::Terminal, DefStateSpec::default(), None).unwrap().created);
    }

    #[test]
    fn name_validation_and_reserved_names() {
        let mut reg = PaneRegistry::new();
        assert!(matches!(
            reg.split(&user(), "", PaneKind::Terminal, DefStateSpec::default(), None),
            Err(PaneError::InvalidName(_))
        ));
        let long = "x".repeat(65);
        assert!(matches!(
            reg.split(&user(), &long, PaneKind::Terminal, DefStateSpec::default(), None),
            Err(PaneError::InvalidName(_))
        ));
        assert!(matches!(
            reg.split(&user(), "a\nb", PaneKind::Terminal, DefStateSpec::default(), None),
            Err(PaneError::InvalidName(_))
        ));
        for reserved in ["get", "list", "exists", "then", "GET"] {
            assert!(matches!(
                reg.split(&user(), reserved, PaneKind::Terminal, DefStateSpec::default(), None),
                Err(PaneError::ReservedName(_))
            ), "{reserved} should be reserved");
        }
        // Reserved names are read-only misses on lookup.
        assert!(reg.resolve(&user(), "get").is_none());
    }
}
