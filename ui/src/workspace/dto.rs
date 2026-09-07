//! The versioned workspace-template schema: the one format behind the
//! per-server `last-session.json` snapshots and named layout files.
//!
//! These types are the permanent file contract: they are deliberately
//! decoupled from the live model (`GroupLayout`, `SmudgyWindow`, the session
//! store) so internal refactors can never silently change what is on disk.
//! Everything here is a stable descriptor:
//!
//! - A **session slot** carries its own persisted id plus server/profile
//!   names and the connection *intent* (the explicit online/offline choice —
//!   never observed socket state). Slot ordinal is list position.
//! - A **window** carries a stable workspace window id (ordinal is list
//!   position — the creation order of the stable ids, meaningful across
//!   restarts where `window::Id` is not), its geometry with the scale
//!   context it was captured under, the explicitly active slot, and its
//!   split forest.
//! - A **pane** is the reserved main identity or a script pane's namespace
//!   plus case-folded name — exactly core's `PaneNameId` interning key, via
//!   the same folding and validation functions, so a persisted descriptor
//!   and a live registry entry can never disagree on identity. The display
//!   name rides along for diagnostics only.
//!
//! A snapshot records only what is loaded: a closed session simply is not in
//! the file (closed stays closed by omission), so no occupied/vacant field
//! exists. No secrets, no toolbar state, and no runtime ids appear at any
//! level.
//!
//! Reading is never fatal: [`Workspace::sanitized`] repairs or drops invalid
//! entries entry-by-entry (logging each), so one bad record degrades to a
//! smaller workspace instead of an empty one.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use smudgy_core::session::runtime::pane::{self, NON_MAIN_PANE_CAP};

/// The schema version this build reads and writes. A file with any other
/// version is set aside unread (see [`super::file`]); additive, defaulted
/// fields may land within a version, so unknown fields are ignored on read.
pub const SCHEMA_VERSION: u32 = 1;

/// Fallback window extent when a stored geometry is unusable. Matches the
/// proportions of a fresh default window rather than the OS minimum, so a
/// degraded restore still yields a workable window.
const FALLBACK_WINDOW_SIZE: (f32, f32) = (1280.0, 800.0);

/// The band of scale factors a stored geometry is trusted to carry. Real
/// display scales live well inside it; anything outside can only be a
/// corrupted or hand-mangled value, and it would poison restore's display
/// clamp, which converts through the stored scale — a micro-scale shrinks
/// an absurd stored position to within the display bounds, then blows it
/// back up into an off-screen window.
const SCALE_BAND: std::ops::RangeInclusive<f32> = 0.25..=8.0;

/// The whole workspace template: every session slot and every window worth
/// persisting (a window is worth persisting exactly when its layout is
/// non-empty).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    /// Session slots in ordinal order (list position is the slot ordinal the
    /// binding engine ties against).
    #[serde(default)]
    pub sessions: Vec<SessionSlot>,
    /// Windows in ordinal order (list position is the window ordinal).
    #[serde(default)]
    pub windows: Vec<Window>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            sessions: Vec::new(),
            windows: Vec::new(),
        }
    }
}

/// One persisted session: a stable slot id (unique within the file), the
/// server/profile descriptors that re-open it, and its connection intent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSlot {
    /// Stable slot id — what a pane's `slot` field references. Unique within
    /// the file; carries no meaning beyond identity.
    pub id: u64,
    pub server: String,
    pub profile: String,
    /// The slot's connection *intent*: `true` restores through the normal
    /// Connect path, `false` reopens offline. Mirrors the per-session
    /// `auto_connect` flag (Connect sets it, Disconnect clears it) — never
    /// the observed socket state, so a snapshot taken during a transient
    /// outage cannot flip next-launch behavior.
    pub connect: bool,
}

/// One persisted window: stable id, geometry, the explicitly active session
/// slot, and the split forest its grid is derived from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Stable workspace window id. List position, not this id, is the
    /// window's ordinal; the id exists so runtime state can track one window
    /// across snapshots within a run.
    pub id: u64,
    pub geometry: Geometry,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    /// The active session's slot id. Restore applies this explicitly rather
    /// than relying on active-session repair, which may prefer differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_slot: Option<u64>,
    /// Top-level clusters in order — the same forest shape as the live
    /// layout model: each cluster is a weighted share of the window holding
    /// a binary split tree whose leaves are tab groups.
    pub clusters: Vec<Cluster>,
}

/// A window's outer placement in logical coordinates, plus the scale factor
/// it was captured under so restore can clamp sensibly on a changed monitor
/// arrangement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The window's scale factor at capture time — context for clamping,
    /// never re-applied as-is (the display decides the live scale).
    pub scale: f32,
}

impl Default for Geometry {
    /// The origin at the fallback window size — what a window gets when no
    /// usable geometry was ever captured for it.
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: FALLBACK_WINDOW_SIZE.0,
            height: FALLBACK_WINDOW_SIZE.1,
            scale: 1.0,
        }
    }
}

/// One top-level cluster: its relative weight (shares renormalize as
/// clusters come and go) and its split tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    pub weight: f32,
    pub root: Node,
}

/// One subtree: a tab group, or a split of two subtrees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Group(Group),
    Split(Split),
}

/// An interior split: axis, divider sizing, and the two children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub axis: Axis,
    pub sizing: Sizing,
    pub a: Box<Node>,
    pub b: Box<Node>,
}

/// A split's orientation. `Horizontal` stacks its children vertically (the
/// divider runs horizontally), matching the live grid's axis convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// How a split's divider position is determined — the persisted form of the
/// live model's sizing: a user-owned ratio carried verbatim, or a script's
/// pixel request re-resolved against the region at every build.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sizing {
    Ratio(f32),
    Px { px: f32, sized_first: bool },
}

/// One tab group: its ordered panes and the durably selected member (an
/// index into `tabs`). Never empty in a sanitized workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub tabs: Vec<Pane>,
    /// Index of the durably selected tab. Durable selection is persisted
    /// as-is; the rendered fallback for a hidden selection is derived live
    /// and never stored.
    #[serde(default)]
    pub selected: usize,
}

/// One pane descriptor: which slot it belongs to, its stable identity, and
/// the user's eyeball-hidden preference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    /// The owning session slot's id.
    pub slot: u64,
    pub id: PaneIdentity,
    /// The user-set eyeball-hidden state, replayed once through the normal
    /// user-toggle path when the pane first materializes after restore.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// A pane's stable identity: the reserved main pane, or a script pane's
/// namespace plus case-folded name (core's interning key — the namespace
/// deliberately excludes package version).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneIdentity {
    /// The session's fused output+input pane.
    Main,
    /// A script-created pane, identified by namespace + folded name.
    Script {
        namespace: Namespace,
        /// The case-folded identity form ([`pane::fold`]); sanitizing folds
        /// a display-cased value rather than rejecting it.
        name: String,
        /// Display-cased name as the creator wrote it — diagnostics/UI only,
        /// never identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
}

/// The persisted form of core's `PaneNamespace`: user/main-isolate code, or
/// a package by owner + name (no version — stable across upgrades).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Namespace {
    User,
    Package { owner: String, name: String },
}

/// A pane identity reduced to its comparable key (display name excluded) —
/// what "the same pane" means for deduplication and selection remapping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum IdentityKey {
    Main,
    User(String),
    Package {
        owner: String,
        package: String,
        name: String,
    },
}

impl PaneIdentity {
    /// The folded comparison key for this identity, or `None` when the
    /// identity is invalid (a name the registry could never have created).
    fn key(&self) -> Option<IdentityKey> {
        match self {
            Self::Main => Some(IdentityKey::Main),
            Self::Script {
                namespace, name, ..
            } => {
                let folded = pane::fold(name);
                if pane::validate_name(&folded).is_err() || pane::is_main_pane_name(&folded) {
                    return None;
                }
                match namespace {
                    Namespace::User => Some(IdentityKey::User(folded)),
                    Namespace::Package {
                        owner,
                        name: package,
                    } => {
                        if owner.is_empty() || package.is_empty() {
                            None
                        } else {
                            Some(IdentityKey::Package {
                                owner: owner.clone(),
                                package: package.clone(),
                                name: folded,
                            })
                        }
                    }
                }
            }
        }
    }
}

/// Cross-window bookkeeping for pane sanitizing: which (slot, identity)
/// pairs are already hosted somewhere, and how many script panes each slot
/// has placed (a session's panes may be torn out across windows, so both
/// facts are workspace-global).
#[derive(Default)]
struct PaneBudget {
    hosted: HashSet<(u64, IdentityKey)>,
    script_counts: HashMap<u64, usize>,
}

impl PaneBudget {
    /// Whether `pane` may keep its place, updating the bookkeeping when so.
    fn admit(&mut self, slot: u64, key: IdentityKey) -> bool {
        let is_script = !matches!(key, IdentityKey::Main);
        if !self.hosted.insert((slot, key)) {
            return false;
        }
        if is_script {
            let count = self.script_counts.entry(slot).or_insert(0);
            if *count >= NON_MAIN_PANE_CAP {
                return false;
            }
            *count += 1;
        }
        true
    }
}

impl Workspace {
    /// Enforce every schema invariant leniently, dropping or repairing
    /// offending entries so one bad record costs itself, not the file:
    ///
    /// - duplicate slot ids keep the first occurrence; empty server/profile
    ///   descriptors drop the slot;
    /// - slots no window hosts a main pane for are dropped, and their panes
    ///   drop with them (a session exists on screen through its main, so
    ///   such a slot would spawn — and possibly auto-connect — a session no
    ///   window shows);
    /// - duplicate window ids are re-minted (ordinal, not id, carries
    ///   meaning);
    /// - panes referencing unknown slots, invalid or main-folding script
    ///   names, duplicate (slot, identity) pairs, and script panes past the
    ///   per-slot cap are dropped; surviving script names are stored folded;
    /// - groups emptied by those drops collapse exactly like the live
    ///   model's split collapse; windows emptied entirely are dropped;
    /// - non-finite or out-of-range numbers (weights, ratios, pixel
    ///   sizings, geometry, scale) are clamped or defaulted;
    /// - a dangling `active_slot` or out-of-range selection is cleared.
    ///
    /// Every repair is logged; a clean workspace passes through unchanged.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        let mut seen_slots = HashSet::new();
        self.sessions.retain(|slot| {
            if slot.server.is_empty() || slot.profile.is_empty() {
                log::warn!(
                    "[workspace] dropping session slot {} with an empty server/profile descriptor",
                    slot.id
                );
                return false;
            }
            if !seen_slots.insert(slot.id) {
                log::warn!(
                    "[workspace] dropping duplicate session slot id {} ({}/{})",
                    slot.id,
                    slot.server,
                    slot.profile
                );
                return false;
            }
            true
        });

        // A slot is real only while some window hosts its main pane: every
        // other kind of survival (script panes alone, a bare list entry)
        // would restore a session the user cannot see. At least the first
        // hosted occurrence of a main always survives the pane pass below
        // — its slot is known, its identity is valid, and duplicates keep
        // the first — so counting occurrences here is exact.
        let mut hosted_mains = HashSet::new();
        for window in &self.windows {
            for cluster in &window.clusters {
                collect_hosted_mains(&cluster.root, &mut hosted_mains);
            }
        }
        self.sessions.retain(|slot| {
            if hosted_mains.contains(&slot.id) {
                return true;
            }
            log::warn!(
                "[workspace] dropping session slot {} ({}/{}): no window hosts its main pane",
                slot.id,
                slot.server,
                slot.profile
            );
            false
        });
        seen_slots.retain(|id| hosted_mains.contains(id));

        let mut budget = PaneBudget::default();
        let mut window_ids = HashSet::new();
        let mut next_window_id = self
            .windows
            .iter()
            .map(|w| w.id)
            .max()
            .map_or(1, |max| max.saturating_add(1));
        let mut windows = Vec::with_capacity(self.windows.len());
        for mut window in self.windows {
            window.geometry = window.geometry.sanitized(window.id);
            if !window_ids.insert(window.id) {
                log::warn!(
                    "[workspace] window id {} duplicated; re-minting (ordinal is what matters)",
                    window.id
                );
                window.id = next_window_id;
                window_ids.insert(window.id);
                next_window_id = next_window_id.saturating_add(1);
            }
            if let Some(active) = window.active_slot
                && !seen_slots.contains(&active)
            {
                log::warn!(
                    "[workspace] window {} names unknown active slot {active}; clearing",
                    window.id
                );
                window.active_slot = None;
            }
            let clusters: Vec<Cluster> = window
                .clusters
                .into_iter()
                .filter_map(|cluster| sanitize_cluster(cluster, &seen_slots, &mut budget))
                .collect();
            if clusters.is_empty() {
                log::warn!(
                    "[workspace] window {} has no surviving panes; dropping it",
                    window.id
                );
                continue;
            }
            window.clusters = clusters;
            windows.push(window);
        }
        self.windows = windows;
        self
    }
}

impl Geometry {
    /// Repair non-finite or degenerate values. Restore-time display clamping
    /// is separate; this only guarantees the numbers are usable at all.
    pub(super) fn sanitized(self, window_id: u64) -> Self {
        let mut geometry = self;
        if !geometry.x.is_finite() || !geometry.y.is_finite() {
            log::warn!("[workspace] window {window_id} position is not finite; using the origin");
            geometry.x = 0.0;
            geometry.y = 0.0;
        }
        if !geometry.width.is_finite()
            || !geometry.height.is_finite()
            || geometry.width <= 0.0
            || geometry.height <= 0.0
        {
            log::warn!("[workspace] window {window_id} size is unusable; using the fallback size");
            geometry.width = FALLBACK_WINDOW_SIZE.0;
            geometry.height = FALLBACK_WINDOW_SIZE.1;
        }
        if !geometry.scale.is_finite() || geometry.scale <= 0.0 {
            log::warn!("[workspace] window {window_id} scale is unusable; assuming 1.0");
            geometry.scale = 1.0;
        } else if !SCALE_BAND.contains(&geometry.scale) {
            log::warn!(
                "[workspace] window {window_id} scale {} is outside {SCALE_BAND:?}; clamping",
                geometry.scale
            );
            geometry.scale = geometry.scale.clamp(*SCALE_BAND.start(), *SCALE_BAND.end());
        }
        geometry
    }
}

/// Record every slot id a subtree hosts a main pane for.
fn collect_hosted_mains(node: &Node, mains: &mut HashSet<u64>) {
    match node {
        Node::Group(group) => {
            for pane in &group.tabs {
                if matches!(pane.id, PaneIdentity::Main) {
                    mains.insert(pane.slot);
                }
            }
        }
        Node::Split(split) => {
            collect_hosted_mains(&split.a, mains);
            collect_hosted_mains(&split.b, mains);
        }
    }
}

/// Sanitize one cluster: repair its weight and clean its tree. `None` when
/// no pane in the cluster survives.
fn sanitize_cluster(
    mut cluster: Cluster,
    slots: &HashSet<u64>,
    budget: &mut PaneBudget,
) -> Option<Cluster> {
    if !cluster.weight.is_finite() || cluster.weight <= 0.0 {
        cluster.weight = 1.0;
    }
    cluster.root = sanitize_node(cluster.root, slots, budget)?;
    Some(cluster)
}

/// Sanitize one subtree. A split with one dead side collapses into the
/// other, exactly like the live model's leaf-removal collapse.
fn sanitize_node(node: Node, slots: &HashSet<u64>, budget: &mut PaneBudget) -> Option<Node> {
    match node {
        Node::Group(group) => sanitize_group(group, slots, budget).map(Node::Group),
        Node::Split(split) => {
            let sizing = sanitize_sizing(split.sizing);
            let a = sanitize_node(*split.a, slots, budget);
            let b = sanitize_node(*split.b, slots, budget);
            match (a, b) {
                (Some(a), Some(b)) => Some(Node::Split(Split {
                    axis: split.axis,
                    sizing,
                    a: Box::new(a),
                    b: Box::new(b),
                })),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

fn sanitize_sizing(sizing: Sizing) -> Sizing {
    match sizing {
        Sizing::Ratio(ratio) if ratio.is_finite() => Sizing::Ratio(ratio.clamp(0.0, 1.0)),
        Sizing::Px { px, sized_first } if px.is_finite() => Sizing::Px {
            px: px.max(0.0),
            sized_first,
        },
        _ => Sizing::Ratio(0.5),
    }
}

/// Sanitize one group: drop inadmissible panes, fold surviving script names,
/// and remap the durable selection to follow its tab (falling back to the
/// first tab when the selected one was dropped). `None` when no tab
/// survives — the group's leaf collapses out of the tree.
fn sanitize_group(group: Group, slots: &HashSet<u64>, budget: &mut PaneBudget) -> Option<Group> {
    let selected_key = group
        .tabs
        .get(group.selected)
        .map(|pane| (pane.slot, pane.id.key()));
    let tabs: Vec<Pane> = group
        .tabs
        .into_iter()
        .filter_map(|pane| {
            if !slots.contains(&pane.slot) {
                log::warn!(
                    "[workspace] dropping pane referencing unknown session slot {}",
                    pane.slot
                );
                return None;
            }
            let Some(key) = pane.id.key() else {
                log::warn!(
                    "[workspace] dropping pane with an invalid identity: {:?}",
                    pane.id
                );
                return None;
            };
            if !budget.admit(pane.slot, key.clone()) {
                log::warn!(
                    "[workspace] dropping duplicate or over-cap pane {key:?} of slot {}",
                    pane.slot
                );
                return None;
            }
            Some(fold_pane(pane))
        })
        .collect();
    if tabs.is_empty() {
        return None;
    }
    let selected = selected_key
        .and_then(|(slot, key)| {
            tabs.iter()
                .position(|pane| pane.slot == slot && pane.id.key() == key)
        })
        .unwrap_or(0);
    Some(Group { tabs, selected })
}

/// Store a script pane's identity in folded form (a hand-edited
/// display-cased name is repaired, not rejected — identity is the fold).
fn fold_pane(mut pane: Pane) -> Pane {
    if let PaneIdentity::Script { name, .. } = &mut pane.id {
        let folded = pane::fold(name);
        if *name != folded {
            *name = folded;
        }
    }
    pane
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed human-inspectable golden fixture. Regenerate only on a
    /// deliberate schema change, alongside a version-story decision.
    const GOLDEN: &str = include_str!("testdata/workspace_v1.json");

    fn slot(id: u64, server: &str, profile: &str, connect: bool) -> SessionSlot {
        SessionSlot {
            id,
            server: server.to_string(),
            profile: profile.to_string(),
            connect,
        }
    }

    fn main_pane(slot: u64) -> Pane {
        Pane {
            slot,
            id: PaneIdentity::Main,
            hidden: false,
        }
    }

    fn script_pane(slot: u64, name: &str) -> Pane {
        Pane {
            slot,
            id: PaneIdentity::Script {
                namespace: Namespace::User,
                name: name.to_string(),
                display: None,
            },
            hidden: false,
        }
    }

    fn group(tabs: Vec<Pane>, selected: usize) -> Node {
        Node::Group(Group { tabs, selected })
    }

    fn geometry() -> Geometry {
        Geometry {
            x: 100.0,
            y: 80.0,
            width: 1600.0,
            height: 900.0,
            scale: 1.0,
        }
    }

    fn window(id: u64, clusters: Vec<Cluster>) -> Window {
        Window {
            id,
            geometry: geometry(),
            maximized: false,
            active_slot: None,
            clusters,
        }
    }

    fn cluster(root: Node) -> Cluster {
        Cluster { weight: 1.0, root }
    }

    #[test]
    fn golden_fixture_parses_sanitizes_clean_and_round_trips() {
        let parsed: Workspace = serde_json::from_str(GOLDEN).expect("golden fixture parses");
        assert_eq!(parsed.version, SCHEMA_VERSION);
        assert_eq!(parsed.sessions.len(), 2);
        assert_eq!(parsed.windows.len(), 2);

        // The fixture is already valid, so sanitizing must be an identity.
        let sanitized = parsed.clone().sanitized();
        assert_eq!(sanitized, parsed);

        // Value-level equality between the fixture and a re-serialization
        // pins the wire shape: renaming, retagging, or dropping a field
        // fails here before it can corrupt anyone's workspace.
        let reserialized = serde_json::to_string_pretty(&parsed).expect("serialize");
        let round_tripped: Workspace = serde_json::from_str(&reserialized).expect("reparse");
        assert_eq!(round_tripped, parsed);
        let golden_value: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
        let reserialized_value: serde_json::Value = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(golden_value, reserialized_value);
    }

    #[test]
    fn golden_fixture_spot_checks() {
        let parsed: Workspace = serde_json::from_str(GOLDEN).expect("golden fixture parses");

        // Two slots on the same server/profile stay distinct by id + ordinal.
        assert_eq!(parsed.sessions[0].server, parsed.sessions[1].server);
        assert_eq!(parsed.sessions[0].profile, parsed.sessions[1].profile);
        assert_ne!(parsed.sessions[0].id, parsed.sessions[1].id);
        assert!(parsed.sessions[0].connect);
        assert!(!parsed.sessions[1].connect);

        // Window 1: the active slot is explicit, and its first cluster holds
        // a px-sized split whose second group carries a package-namespaced,
        // eyeball-hidden pane with a durable non-zero selection.
        let first = &parsed.windows[0];
        assert_eq!(first.active_slot, Some(1));
        assert!((first.geometry.scale - 1.25).abs() < f32::EPSILON);
        let Node::Split(split) = &first.clusters[0].root else {
            panic!("expected a split at the first cluster root");
        };
        assert_eq!(split.axis, Axis::Vertical);
        assert_eq!(
            split.sizing,
            Sizing::Px {
                px: 300.0,
                sized_first: false
            }
        );
        let Node::Group(scripts) = split.b.as_ref() else {
            panic!("expected a group on the b side");
        };
        assert_eq!(scripts.selected, 1);
        assert!(scripts.tabs[1].hidden);
        assert!(matches!(
            &scripts.tabs[1].id,
            PaneIdentity::Script {
                namespace: Namespace::Package { .. },
                ..
            }
        ));

        // Window 2 mixes both slots' panes — a torn-out arrangement — and
        // pins the maximized field's wire shape (window 1 relies on the
        // false default being omitted).
        let second = &parsed.windows[1];
        assert!(!first.maximized);
        assert!(second.maximized);
        let Node::Split(split) = &second.clusters[0].root else {
            panic!("expected a split in the second window");
        };
        assert_eq!(split.sizing, Sizing::Ratio(0.65));
    }

    #[test]
    fn maximal_workspace_round_trips_exactly() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(7, "Arctic", "imm", true),
                slot(9, "Other", "alt", false),
            ],
            windows: vec![Window {
                id: 3,
                geometry: Geometry {
                    x: -1920.0,
                    y: 40.5,
                    width: 800.25,
                    height: 600.0,
                    scale: 2.0,
                },
                maximized: true,
                active_slot: Some(9),
                clusters: vec![
                    Cluster {
                        weight: 0.25,
                        root: Node::Split(Split {
                            axis: Axis::Horizontal,
                            sizing: Sizing::Ratio(0.3),
                            a: Box::new(group(vec![main_pane(7)], 0)),
                            b: Box::new(Node::Split(Split {
                                axis: Axis::Vertical,
                                sizing: Sizing::Px {
                                    px: 120.0,
                                    sized_first: true,
                                },
                                a: Box::new(group(
                                    vec![Pane {
                                        slot: 7,
                                        id: PaneIdentity::Script {
                                            namespace: Namespace::Package {
                                                owner: "owner".to_string(),
                                                name: "pkg".to_string(),
                                            },
                                            name: "map".to_string(),
                                            display: Some("Map".to_string()),
                                        },
                                        hidden: true,
                                    }],
                                    0,
                                )),
                                b: Box::new(group(
                                    vec![script_pane(7, "chat"), script_pane(9, "chat")],
                                    1,
                                )),
                            })),
                        }),
                    },
                    cluster(group(vec![main_pane(9)], 0)),
                ],
            }],
        };
        let json = serde_json::to_string_pretty(&workspace).expect("serialize");
        let parsed: Workspace = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, workspace);
        // Already valid: sanitizing changes nothing.
        assert_eq!(parsed.sanitized(), workspace);
    }

    #[test]
    fn unknown_fields_are_ignored_within_a_version() {
        // Additive evolution within a schema version: a defaulted field a
        // newer build wrote must not break an older build's read.
        let json = r#"{
            "version": 1,
            "future_top_level": {"anything": true},
            "sessions": [{"id": 1, "server": "s", "profile": "p", "connect": false, "future": 1}],
            "windows": []
        }"#;
        let parsed: Workspace = serde_json::from_str(json).expect("parse ignores unknowns");
        assert_eq!(parsed.sessions.len(), 1);
    }

    #[test]
    fn sanitize_drops_bad_slots_and_their_panes() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(1, "Arctic", "imm", true),
                slot(1, "Arctic", "other", false), // duplicate id
                slot(2, "", "p", false),           // empty server
            ],
            windows: vec![window(
                1,
                vec![
                    cluster(group(vec![main_pane(1)], 0)),
                    cluster(group(vec![main_pane(2), main_pane(99)], 0)),
                ],
            )],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.sessions.len(), 1);
        assert_eq!(sanitized.sessions[0].profile, "imm");
        // Slot 2 and slot 99 panes are gone; their cluster collapsed away.
        assert_eq!(sanitized.windows[0].clusters.len(), 1);
    }

    #[test]
    fn sanitize_collapses_splits_and_drops_emptied_windows() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![
                window(
                    1,
                    vec![cluster(Node::Split(Split {
                        axis: Axis::Vertical,
                        sizing: Sizing::Ratio(0.5),
                        a: Box::new(group(vec![main_pane(1)], 0)),
                        b: Box::new(group(vec![main_pane(42)], 0)), // dies
                    }))],
                ),
                window(2, vec![cluster(group(vec![main_pane(43)], 0))]), // dies whole
            ],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.windows.len(), 1);
        // The split with one dead side collapsed into the surviving group.
        assert!(matches!(
            &sanitized.windows[0].clusters[0].root,
            Node::Group(g) if g.tabs.len() == 1
        ));
    }

    #[test]
    fn sanitize_remaps_selection_to_follow_its_tab() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![window(
                1,
                vec![cluster(group(
                    vec![
                        main_pane(99), // dropped (unknown slot)
                        script_pane(1, "chat"),
                        main_pane(1),
                    ],
                    2, // selects main(1), which survives at index 1
                ))],
            )],
        };
        let sanitized = workspace.sanitized();
        let Node::Group(g) = &sanitized.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(g.tabs.len(), 2);
        assert_eq!(g.selected, 1);
        assert_eq!(g.tabs[1].id, PaneIdentity::Main);
    }

    #[test]
    fn sanitize_clamps_out_of_range_selection() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![window(
                1,
                vec![cluster(group(
                    vec![main_pane(1), script_pane(1, "chat")],
                    7,
                ))],
            )],
        };
        let sanitized = workspace.sanitized();
        let Node::Group(g) = &sanitized.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(g.selected, 0);
    }

    #[test]
    fn sanitize_folds_names_and_drops_invalid_identities() {
        let bad_names = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![window(
                1,
                vec![cluster(group(
                    vec![
                        main_pane(1),
                        script_pane(1, "Chat"), // display-cased: folded, kept
                        script_pane(1, "MAIN"), // folds to the reserved main name
                        script_pane(1, ""),     // empty
                        script_pane(1, "a\nb"), // control character
                        Pane {
                            slot: 1,
                            id: PaneIdentity::Script {
                                namespace: Namespace::Package {
                                    owner: String::new(), // empty owner
                                    name: "pkg".to_string(),
                                },
                                name: "ok".to_string(),
                                display: None,
                            },
                            hidden: false,
                        },
                    ],
                    0,
                ))],
            )],
        };
        let sanitized = bad_names.sanitized();
        let Node::Group(g) = &sanitized.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(g.tabs.len(), 2);
        let PaneIdentity::Script { name, .. } = &g.tabs[1].id else {
            panic!("script pane expected");
        };
        assert_eq!(name, "chat", "identity is stored folded");
    }

    #[test]
    fn sanitize_dedupes_identities_across_windows() {
        // The same (slot, identity) hosted twice — the second occurrence
        // loses, wherever it lives.
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![
                window(
                    1,
                    vec![cluster(group(
                        vec![main_pane(1), script_pane(1, "chat")],
                        0,
                    ))],
                ),
                window(2, vec![cluster(group(vec![script_pane(1, "CHAT")], 0))]),
            ],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.windows.len(), 1, "the duplicate's window emptied");
    }

    #[test]
    fn sanitize_enforces_the_per_slot_script_pane_cap() {
        // One more script pane than the registry could ever host; the
        // overflow pane is dropped, main is untouched (it never counts).
        let mut tabs = vec![main_pane(1)];
        for i in 0..=NON_MAIN_PANE_CAP {
            tabs.push(script_pane(1, &format!("pane{i}")));
        }
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![window(1, vec![cluster(group(tabs, 0))])],
        };
        let sanitized = workspace.sanitized();
        let Node::Group(g) = &sanitized.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(g.tabs.len(), 1 + NON_MAIN_PANE_CAP);
    }

    #[test]
    fn sanitize_normalizes_numbers() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![slot(1, "Arctic", "imm", true)],
            windows: vec![Window {
                id: 1,
                geometry: Geometry {
                    x: f32::NAN,
                    y: 5.0,
                    width: -100.0,
                    height: f32::INFINITY,
                    scale: 0.0,
                },
                maximized: false,
                active_slot: Some(77), // dangling
                clusters: vec![Cluster {
                    weight: -3.0,
                    root: Node::Split(Split {
                        axis: Axis::Vertical,
                        sizing: Sizing::Ratio(f32::NAN),
                        a: Box::new(group(vec![main_pane(1)], 0)),
                        b: Box::new(Node::Split(Split {
                            axis: Axis::Horizontal,
                            sizing: Sizing::Px {
                                px: -50.0,
                                sized_first: true,
                            },
                            a: Box::new(group(vec![script_pane(1, "a")], 0)),
                            b: Box::new(group(vec![script_pane(1, "b")], 0)),
                        })),
                    }),
                }],
            }],
        };
        let sanitized = workspace.sanitized();
        let win = &sanitized.windows[0];
        assert_eq!(win.geometry.x, 0.0);
        assert_eq!(win.geometry.y, 0.0);
        assert_eq!(win.geometry.width, FALLBACK_WINDOW_SIZE.0);
        assert_eq!(win.geometry.scale, 1.0);
        assert_eq!(win.active_slot, None);
        assert_eq!(win.clusters[0].weight, 1.0);
        let Node::Split(outer) = &win.clusters[0].root else {
            panic!("split expected");
        };
        assert_eq!(outer.sizing, Sizing::Ratio(0.5));
        let Node::Split(inner) = outer.b.as_ref() else {
            panic!("inner split expected");
        };
        assert_eq!(
            inner.sizing,
            Sizing::Px {
                px: 0.0,
                sized_first: true
            }
        );
    }

    #[test]
    fn sanitize_clamps_scale_to_the_trusted_band() {
        let mut workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(1, "Arctic", "imm", true),
                slot(2, "Arctic", "alt", true),
                slot(3, "Arctic", "third", true),
            ],
            windows: vec![
                window(1, vec![cluster(group(vec![main_pane(1)], 0))]),
                window(2, vec![cluster(group(vec![main_pane(2)], 0))]),
                window(3, vec![cluster(group(vec![main_pane(3)], 0))]),
            ],
        };
        workspace.windows[0].geometry.scale = 0.001; // micro-scale
        workspace.windows[1].geometry.scale = 100.0; // absurdly large
        workspace.windows[2].geometry.scale = 1.5; // ordinary: untouched
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.windows[0].geometry.scale, 0.25);
        assert_eq!(sanitized.windows[1].geometry.scale, 8.0);
        assert_eq!(sanitized.windows[2].geometry.scale, 1.5);
    }

    #[test]
    fn sanitize_drops_slots_no_window_hosts_a_main_for() {
        // Slot 2 appears only through script panes: restoring it would
        // spawn (and auto-connect) a session no window shows.
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(1, "Arctic", "imm", true),
                slot(2, "Arctic", "ghost", true),
            ],
            windows: vec![window(
                1,
                vec![cluster(group(
                    vec![main_pane(1), script_pane(2, "chat")],
                    0,
                ))],
            )],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.sessions.len(), 1);
        assert_eq!(sanitized.sessions[0].id, 1);
        // The ghost slot's pane fell with it.
        let Node::Group(g) = &sanitized.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(g.tabs.len(), 1);
        assert_eq!(g.tabs[0].id, PaneIdentity::Main);
    }

    #[test]
    fn a_window_of_only_script_panes_of_a_dropped_slot_sanitizes_cleanly() {
        // The torn-out window holds nothing but the ghost slot's script
        // panes: the slot drops, the window empties and drops, and the
        // rest of the workspace is untouched.
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(1, "Arctic", "imm", true),
                slot(2, "Arctic", "ghost", false),
            ],
            windows: vec![
                window(1, vec![cluster(group(vec![main_pane(1)], 0))]),
                window(
                    2,
                    vec![cluster(group(
                        vec![script_pane(2, "chat"), script_pane(2, "notes")],
                        1,
                    ))],
                ),
            ],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.sessions.len(), 1);
        assert_eq!(sanitized.windows.len(), 1);
        assert_eq!(sanitized.windows[0].id, 1);
    }

    #[test]
    fn sanitize_reminting_keeps_both_windows_on_duplicate_ids() {
        let workspace = Workspace {
            version: SCHEMA_VERSION,
            sessions: vec![
                slot(1, "Arctic", "imm", true),
                slot(2, "Arctic", "imm", true),
            ],
            windows: vec![
                window(5, vec![cluster(group(vec![main_pane(1)], 0))]),
                window(5, vec![cluster(group(vec![main_pane(2)], 0))]),
            ],
        };
        let sanitized = workspace.sanitized();
        assert_eq!(sanitized.windows.len(), 2);
        assert_ne!(sanitized.windows[0].id, sanitized.windows[1].id);
    }

    #[test]
    fn empty_workspace_is_the_default() {
        let empty = Workspace::default();
        assert_eq!(empty.version, SCHEMA_VERSION);
        assert!(empty.sessions.is_empty());
        assert!(empty.windows.is_empty());
        // And it survives its own round trip.
        let json = serde_json::to_string(&empty).unwrap();
        assert_eq!(serde_json::from_str::<Workspace>(&json).unwrap(), empty);
    }
}
