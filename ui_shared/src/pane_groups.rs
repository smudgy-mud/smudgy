#![allow(clippy::pedantic)]

//! Shared tab-group pane layout model: every physical pane slot is a validated
//! non-empty *group* of tabs rather than a single value.
//!
//! # Structure vs. content
//!
//! A window's layout is an ordered list of **clusters**, each holding a split
//! tree whose leaves carry a stable [`GroupId`] — nothing else. The group's
//! content (its tab strip and selection) lives beside the tree in id-keyed
//! side maps. The separation is what makes tab-level operations safe and
//! cheap: selecting, reordering, merging, or swapping tabs touches only the
//! maps and can never invalidate a tree path mid-operation, while structural
//! operations (split, collapse, resize) are a single-leaf tree algebra
//! instantiated at `GroupId`.
//!
//! # Identity
//!
//! Group ids and tab ids are minted from one process-wide atomic counter, so
//! an id is unique across every window model in the process and remains valid
//! across a cross-window move (a moved [`Tab`] keeps its id). Ids are runtime
//! identities; they are never reused, so a persistence layer can map them to
//! durable descriptors without collision. The live grid payload — what
//! [`GroupLayout::build`] emits inside the `pane_grid::Configuration` — is
//! the stable `GroupId`, never a pane reference: selecting a tab changes what
//! renders *inside* a grid slot without rebuilding the grid.
//!
//! A tab's pane binding is an optional attachment: an unbound tab is a
//! placeholder awaiting materialization, and it participates in every
//! structural operation like any other tab.
//!
//! Hosting a [`Tab`] consumes the value, and every rejected placement hands
//! the tab back to the caller: a tab can neither enter a model twice nor be
//! silently dropped by a failed insertion.
//!
//! # Invariants
//!
//! - A group is never empty.
//! - Every tab id — and every live binding — occurs exactly once in a model.
//! - A group's `selected` tab is a member of its strip.
//! - Removing the selected tab selects the next tab if one exists, otherwise
//!   the previous one.
//! - Removing a group's final tab removes the leaf and collapses its parent
//!   split; a cluster whose last group leaves is dropped.
//!
//! [`GroupLayout::validate`] checks the full set, and every public mutation
//! re-checks it under `debug_assertions`. Multi-participant operations
//! resolve every participant before the first mutation, so a failed lookup
//! rejects the whole operation rather than leaving a partial state.
//!
//! # Filtering
//!
//! Hidden-pane filtering never mutates durable selection: a build drops
//! groups whose tabs are all hidden (an all-hidden slot leaves the derived
//! grid entirely), and [`GroupLayout::effective_selected`] derives a
//! deterministic visible stand-in for a hidden selected tab — the preferred
//! tab is rendered again the moment it becomes visible. Emitted divider
//! mirrors ([`EdgeTarget`]s) always address the unfiltered model.

use std::collections::BTreeMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::Size;
use iced::widget::pane_grid::{self, Axis, Configuration};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

/// Inline capacity of a group's tab strip. A session hosts its main pane plus
/// at most 16 script panes, so 17 tabs is the most a single-session window
/// can stack into one group — the dominant case stays off the heap entirely,
/// and only cross-session stacking in a multi-session window can spill.
const TAB_INLINE: usize = 17;

/// The process-wide stable-id well. One counter feeds both id types, so every
/// minted id is unique across all windows, models, and id kinds for the life
/// of the process, and ids survive a tab's move between window models.
static NEXT_STABLE_ID: AtomicU64 = AtomicU64::new(1);

fn mint_stable_id() -> u64 {
    NEXT_STABLE_ID.fetch_add(1, Ordering::Relaxed)
}

/// The stable identity of one physical pane slot (one tab strip). This is the
/// payload of the emitted grid configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(u64);

impl GroupId {
    fn mint() -> Self {
        Self(mint_stable_id())
    }

    /// The raw stable id — for deriving widget identities (scroll anchors,
    /// keyed children) that must survive rebuilds alongside the group.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Mint a group id outside any model — for geometry unit tests only.
    #[doc(hidden)]
    pub fn mint_for_tests() -> Self {
        Self::mint()
    }
}

/// The stable identity of one workspace pane (one tab), independent of which
/// group currently hosts it and of whether a live pane is bound to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(u64);

impl TabId {
    fn mint() -> Self {
        Self(mint_stable_id())
    }

    /// The raw stable id — for deriving widget identities (scroll anchors,
    /// keyed children) that must survive rebuilds alongside the tab.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// One tab: a stable id plus an optional live binding (`T` is the pane
/// reference type; the unit tests drive the model with plain integers). The
/// id is minted at construction and never changes, so a `Tab` removed from
/// one model and inserted into another keeps its identity.
///
/// Deliberately neither `Copy` nor `Clone`: a tab is a unique value that is
/// either in the caller's hand or hosted in exactly one model. Insertion
/// consumes it, removal returns it, and a rejected insertion gives it back.
#[derive(Debug, PartialEq, Eq)]
pub struct Tab<T> {
    id: TabId,
    binding: Option<T>,
}

impl<T> Tab<T> {
    /// A new tab bound to a live pane.
    #[must_use]
    pub fn bound(binding: T) -> Self {
        Self {
            id: TabId::mint(),
            binding: Some(binding),
        }
    }

    /// A new placeholder tab awaiting materialization.
    #[must_use]
    pub fn placeholder() -> Self {
        Self {
            id: TabId::mint(),
            binding: None,
        }
    }

    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    #[must_use]
    pub fn binding(&self) -> Option<&T> {
        self.binding.as_ref()
    }
}

/// One group's content: the tab strip and the durable selection. Kept out of
/// the split tree so tab operations never walk or reshape it.
#[derive(Debug, PartialEq, Eq)]
struct Group<T> {
    tabs: SmallVec<[Tab<T>; TAB_INLINE]>,
    /// The durably selected tab — always a member of `tabs`. Stored by id so
    /// reordering the strip never disturbs it.
    selected: TabId,
}

impl<T> Group<T> {
    fn singleton(tab: Tab<T>) -> Self {
        let selected = tab.id;
        let mut tabs = SmallVec::new();
        tabs.push(tab);
        Self { tabs, selected }
    }

    fn index_of(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab)
    }
}

/// How a split's divider position is determined at build time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitSizing {
    /// A user-owned (or default 0.5) ratio, carried verbatim across rebuilds.
    Ratio(f32),
    /// A script's initial pixel request: the sized child gets `px` along the
    /// split axis, re-resolved against the region's extent at every build
    /// until a user drag converts the edge to `Ratio`.
    Px { px: f32, sized_first: bool },
}

/// One subtree of a cluster: a group slot, or a split of two subtrees.
#[derive(Debug, Clone, PartialEq)]
enum LayoutNode {
    Leaf(GroupId),
    Split {
        axis: Axis,
        sizing: SplitSizing,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
}

/// One top-level cluster: a subtree plus its share of the window width
/// (weights are relative — shares renormalize as clusters come and go).
#[derive(Debug, Clone, PartialEq)]
struct Cluster {
    weight: f32,
    root: LayoutNode,
}

/// A branch selector into a split node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    A,
    B,
}

/// Which model edge a built divider corresponds to — the target of a user
/// resize. `TopLevel(i)` is the divider between cluster `i` and the fold of
/// the clusters after it; `Node` addresses a split inside one cluster. Always
/// an address into the *unfiltered* model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeTarget {
    TopLevel(usize),
    Node { cluster: usize, path: Vec<Branch> },
}

/// The build's structural mirror of the emitted [`Configuration`]: same tree
/// shape, each split annotated with its [`EdgeTarget`]. Walked in parallel
/// with the built `State`'s layout to map real `Split` ids back to the model.
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltNode {
    Leaf,
    Split {
        target: EdgeTarget,
        a: Box<BuiltNode>,
        b: Box<BuiltNode>,
    },
}

/// One node of the read-only forest mirror emitted by
/// [`GroupLayout::structure`]: the private tree's shape in public types,
/// leaves carrying the stable group id.
#[derive(Debug, Clone, PartialEq)]
pub enum StructureNode {
    Leaf(GroupId),
    Split {
        axis: Axis,
        sizing: SplitSizing,
        a: Box<StructureNode>,
        b: Box<StructureNode>,
    },
}

/// One node of a construction blueprint for [`GroupLayout::from_blueprint`]
/// — the write-side counterpart of [`StructureNode`], its leaves carrying
/// whole tab strips (bound tabs and placeholders alike) with a durable
/// selection index.
#[derive(Debug)]
pub enum Blueprint<T> {
    Group {
        tabs: Vec<Tab<T>>,
        selected: usize,
    },
    Split {
        axis: Axis,
        sizing: SplitSizing,
        a: Box<Blueprint<T>>,
        b: Box<Blueprint<T>>,
    },
}

/// The divider ratio that gives the sized child `px` pixels along the split
/// axis, within a region of extent `extent`. Falls back to an even split when
/// the region is unknown (pre-first-layout) or too small to honor the request.
fn resolve_px(px: f32, sized_first: bool, extent: f32, spacing: f32, min_size: f32) -> f32 {
    if !extent.is_finite() || extent <= 2.0 * min_size {
        return 0.5;
    }
    let raw = if sized_first {
        (px + spacing / 2.0) / extent
    } else {
        1.0 - (px + spacing / 2.0) / extent
    };
    raw.clamp(min_size / extent, 1.0 - min_size / extent)
}

impl LayoutNode {
    fn collect_into(&self, out: &mut Vec<GroupId>) {
        match self {
            Self::Leaf(g) => out.push(*g),
            Self::Split { a, b, .. } => {
                a.collect_into(out);
                b.collect_into(out);
            }
        }
    }

    /// This subtree's shape in the public mirror types.
    fn structure(&self) -> StructureNode {
        match self {
            Self::Leaf(g) => StructureNode::Leaf(*g),
            Self::Split { axis, sizing, a, b } => StructureNode::Split {
                axis: *axis,
                sizing: *sizing,
                a: Box::new(a.structure()),
                b: Box::new(b.structure()),
            },
        }
    }

    /// Replace the leaf `reference` with a split of it and `new`.
    fn split_leaf(
        &mut self,
        reference: GroupId,
        axis: Axis,
        new_first: bool,
        sizing: SplitSizing,
        new: GroupId,
    ) -> bool {
        match self {
            Self::Leaf(g) if *g == reference => {
                let old = Self::Leaf(*g);
                let (a, b) = if new_first {
                    (Self::Leaf(new), old)
                } else {
                    (old, Self::Leaf(new))
                };
                *self = Self::Split {
                    axis,
                    sizing,
                    a: Box::new(a),
                    b: Box::new(b),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { a, b, .. } => {
                a.split_leaf(reference, axis, new_first, sizing, new)
                    || b.split_leaf(reference, axis, new_first, sizing, new)
            }
        }
    }

    /// Remove the leaf `group`, collapsing its parent split into the sibling.
    /// `Ok(true)` = removed; `Ok(false)` = not found; `Err(())` = this node
    /// *was* the leaf (the caller drops the whole node).
    fn remove(&mut self, group: GroupId) -> Result<bool, ()> {
        match self {
            Self::Leaf(g) => {
                if *g == group {
                    Err(())
                } else {
                    Ok(false)
                }
            }
            Self::Split { a, b, .. } => {
                match a.remove(group) {
                    Err(()) => {
                        *self = std::mem::replace(b.as_mut(), Self::Leaf(group));
                        return Ok(true);
                    }
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                }
                match b.remove(group) {
                    Err(()) => {
                        *self = std::mem::replace(a.as_mut(), Self::Leaf(group));
                        Ok(true)
                    }
                    other => other,
                }
            }
        }
    }

    fn node_at_mut(&mut self, path: &[Branch]) -> Option<&mut Self> {
        let Some((head, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Leaf(_) => None,
            Self::Split { a, b, .. } => match head {
                Branch::A => a.node_at_mut(rest),
                Branch::B => b.node_at_mut(rest),
            },
        }
    }

    /// Whether any leaf in this subtree passes the visibility filter.
    fn any_visible(&self, visible: &FxHashSet<GroupId>) -> bool {
        match self {
            Self::Leaf(g) => visible.contains(g),
            Self::Split { a, b, .. } => a.any_visible(visible) || b.any_visible(visible),
        }
    }

    /// Emit the `Configuration` subtree plus its structural mirror, resolving
    /// px sizings against this node's extent (`size`, along each split's
    /// axis). Leaves failing the visibility filter drop out: a split with one
    /// hidden side emits just the other side, full-extent and dividerless, so
    /// every emitted divider — and its mirror `EdgeTarget` — corresponds to a
    /// real edge of the *unfiltered* model. `None` when the whole subtree is
    /// hidden.
    fn build(
        &self,
        size: Size,
        cluster: usize,
        path: &mut Vec<Branch>,
        spacing: f32,
        min_size: f32,
        visible: &FxHashSet<GroupId>,
    ) -> Option<(Configuration<GroupId>, BuiltNode)> {
        match self {
            Self::Leaf(g) => visible
                .contains(g)
                .then_some((Configuration::Pane(*g), BuiltNode::Leaf)),
            Self::Split { axis, sizing, a, b } => {
                match (a.any_visible(visible), b.any_visible(visible)) {
                    (false, false) => None,
                    (true, false) => {
                        path.push(Branch::A);
                        let built = a.build(size, cluster, path, spacing, min_size, visible);
                        path.pop();
                        built
                    }
                    (false, true) => {
                        path.push(Branch::B);
                        let built = b.build(size, cluster, path, spacing, min_size, visible);
                        path.pop();
                        built
                    }
                    (true, true) => {
                        let extent = match axis {
                            Axis::Vertical => size.width,
                            Axis::Horizontal => size.height,
                        };
                        let ratio = match *sizing {
                            SplitSizing::Ratio(r) => r,
                            SplitSizing::Px { px, sized_first } => {
                                resolve_px(px, sized_first, extent, spacing, min_size)
                            }
                        };
                        let (size_a, size_b) = match axis {
                            Axis::Vertical => (
                                Size::new((extent * ratio - spacing / 2.0).max(0.0), size.height),
                                Size::new(
                                    (extent * (1.0 - ratio) - spacing / 2.0).max(0.0),
                                    size.height,
                                ),
                            ),
                            Axis::Horizontal => (
                                Size::new(size.width, (extent * ratio - spacing / 2.0).max(0.0)),
                                Size::new(
                                    size.width,
                                    (extent * (1.0 - ratio) - spacing / 2.0).max(0.0),
                                ),
                            ),
                        };
                        path.push(Branch::A);
                        let built_a = a.build(size_a, cluster, path, spacing, min_size, visible);
                        path.pop();
                        path.push(Branch::B);
                        let built_b = b.build(size_b, cluster, path, spacing, min_size, visible);
                        path.pop();
                        let ((conf_a, built_a), (conf_b, built_b)) = (built_a?, built_b?);
                        Some((
                            Configuration::Split {
                                axis: *axis,
                                ratio,
                                a: Box::new(conf_a),
                                b: Box::new(conf_b),
                            },
                            BuiltNode::Split {
                                target: EdgeTarget::Node {
                                    cluster,
                                    path: path.clone(),
                                },
                                a: Box::new(built_a),
                                b: Box::new(built_b),
                            },
                        ))
                    }
                }
            }
        }
    }
}

/// The whole window's group layout model. Empty ⇔ the window renders the
/// empty connect state (`grid = None`; a `pane_grid::State` cannot be empty).
#[derive(Debug)]
pub struct GroupLayout<T> {
    clusters: Vec<Cluster>,
    /// Group content, keyed by the stable id the tree leaves carry.
    groups: FxHashMap<GroupId, Group<T>>,
    /// Each tab's hosting group — the O(1) entry point for every tab-level
    /// operation, and the model's uniqueness registry.
    tab_owner: FxHashMap<TabId, GroupId>,
}

impl<T> Default for GroupLayout<T> {
    fn default() -> Self {
        Self {
            clusters: Vec::new(),
            groups: FxHashMap::default(),
            tab_owner: FxHashMap::default(),
        }
    }
}

impl<T: Copy + Eq + Hash> GroupLayout<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }

    #[must_use]
    pub fn contains_group(&self, group: GroupId) -> bool {
        self.groups.contains_key(&group)
    }

    /// Whether `tab` is hosted anywhere in this model — with
    /// [`Self::panes`], the basis for checking that a pane occurs in at most
    /// one model process-wide.
    #[must_use]
    pub fn contains_tab(&self, tab: TabId) -> bool {
        self.tab_owner.contains_key(&tab)
    }

    /// Whether any tab in this model is bound to `binding`.
    #[must_use]
    pub fn contains_binding(&self, binding: T) -> bool {
        self.groups
            .values()
            .any(|g| g.tabs.iter().any(|t| t.binding == Some(binding)))
    }

    /// The group hosting `tab`.
    #[must_use]
    pub fn group_of(&self, tab: TabId) -> Option<GroupId> {
        self.tab_owner.get(&tab).copied()
    }

    /// The group's durably selected tab.
    #[must_use]
    pub fn selected(&self, group: GroupId) -> Option<TabId> {
        self.groups.get(&group).map(|g| g.selected)
    }

    /// The group's tab strip, in display order.
    #[must_use]
    pub fn tabs(&self, group: GroupId) -> Option<&[Tab<T>]> {
        self.groups.get(&group).map(|g| g.tabs.as_slice())
    }

    /// One tab by id.
    #[must_use]
    pub fn tab(&self, tab: TabId) -> Option<&Tab<T>> {
        let group = self.groups.get(self.tab_owner.get(&tab)?)?;
        group.tabs.iter().find(|t| t.id == tab)
    }

    /// Every group id, cluster by cluster, depth-first — the deterministic
    /// model order (never an artifact of map iteration).
    #[must_use]
    pub fn groups_depth_first(&self) -> Vec<GroupId> {
        let mut out = Vec::new();
        for cluster in &self.clusters {
            cluster.root.collect_into(&mut out);
        }
        out
    }

    /// Every tab, cluster by cluster, depth-first, in strip order within each
    /// group.
    #[must_use]
    pub fn panes(&self) -> Vec<&Tab<T>> {
        let mut out = Vec::new();
        for gid in self.groups_depth_first() {
            if let Some(group) = self.groups.get(&gid) {
                out.extend(group.tabs.iter());
            }
        }
        out
    }

    /// The tab the group renders under a visibility filter: the durable
    /// selection when visible, otherwise the first visible tab at or after
    /// the selected slot, otherwise the nearest visible tab before it. Purely
    /// derived — the durable selection is untouched, so the preferred tab is
    /// rendered again the moment it becomes visible. `None` when the group is
    /// unknown or fully hidden.
    #[must_use]
    pub fn effective_selected(
        &self,
        group: GroupId,
        visible: impl Fn(&Tab<T>) -> bool,
    ) -> Option<TabId> {
        let g = self.groups.get(&group)?;
        let sel = g.index_of(g.selected)?;
        g.tabs[sel..]
            .iter()
            .find(|t| visible(t))
            .or_else(|| g.tabs[..sel].iter().rev().find(|t| visible(t)))
            .map(|t| t.id)
    }

    /// A read-only mirror of the split forest — cluster weights and split
    /// geometry down to `GroupId` leaves — in model order. This is the
    /// serialization view: a durable snapshot walks it and reads each leaf's
    /// content through [`Self::tabs`] and [`Self::selected`], so persistence
    /// never depends on the private tree representation.
    #[must_use]
    pub fn structure(&self) -> Vec<(f32, StructureNode)> {
        self.clusters
            .iter()
            .map(|cluster| (cluster.weight, cluster.root.structure()))
            .collect()
    }

    /// Assemble a whole model from a blueprint — the restore path's inverse
    /// of [`Self::structure`]. Tabs are consumed exactly like every other
    /// insertion; a tab whose id or binding is already present in the
    /// assembled model is dropped (with its selection preference), an
    /// emptied group collapses out of its split, and an emptied cluster
    /// disappears, so the result always satisfies the model invariants
    /// whatever the blueprint held. Selection clamps to a surviving member.
    #[must_use]
    pub fn from_blueprint(clusters: Vec<(f32, Blueprint<T>)>) -> Self {
        let mut layout = Self::new();
        for (weight, blueprint) in clusters {
            let Some(root) = layout.assemble(blueprint) else {
                continue;
            };
            layout.clusters.push(Cluster { weight, root });
        }
        layout.debug_validate();
        layout
    }

    /// Build one blueprint subtree, registering its groups and tabs.
    /// `None` when nothing in the subtree survives admission.
    fn assemble(&mut self, blueprint: Blueprint<T>) -> Option<LayoutNode> {
        match blueprint {
            Blueprint::Group { tabs, selected } => {
                let selected_id = tabs.get(selected).map(Tab::id);
                // `admissible` rejects ids/bindings already registered in
                // earlier groups; the pairwise check rejects duplicates
                // within this same strip (nothing is registered yet).
                let mut kept: SmallVec<[Tab<T>; TAB_INLINE]> = SmallVec::new();
                for tab in tabs {
                    if !self.admissible(&tab) {
                        continue;
                    }
                    let duplicate = kept.iter().any(|earlier: &Tab<T>| {
                        earlier.id == tab.id
                            || (earlier.binding.is_some() && earlier.binding == tab.binding)
                    });
                    if !duplicate {
                        kept.push(tab);
                    }
                }
                let tabs = kept;
                if tabs.is_empty() {
                    return None;
                }
                let selected = selected_id
                    .and_then(|id| tabs.iter().find(|tab| tab.id == id))
                    .map_or_else(|| tabs[0].id, Tab::id);
                let gid = GroupId::mint();
                for tab in &tabs {
                    self.tab_owner.insert(tab.id, gid);
                }
                self.groups.insert(gid, Group { tabs, selected });
                Some(LayoutNode::Leaf(gid))
            }
            Blueprint::Split { axis, sizing, a, b } => {
                let a = self.assemble(*a);
                let b = self.assemble(*b);
                match (a, b) {
                    (Some(a), Some(b)) => Some(LayoutNode::Split {
                        axis,
                        sizing,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Structural operations (group-level)
    // ------------------------------------------------------------------

    /// The average existing weight — an appended/prepended cluster takes an
    /// even share of the window (1/(n+1) of it) while the existing clusters
    /// keep their relative proportions.
    fn even_share_weight(&self) -> f32 {
        if self.clusters.is_empty() {
            1.0
        } else {
            self.clusters.iter().map(|c| c.weight).sum::<f32>() / self.clusters.len() as f32
        }
    }

    /// Whether `tab` can enter this model as a new occurrence. An id or
    /// binding already hosted here rejects the insertion — the exactly-once
    /// invariant is enforced at the API boundary, not just observed by
    /// `validate`.
    fn admissible(&self, tab: &Tab<T>) -> bool {
        !self.tab_owner.contains_key(&tab.id)
            && tab.binding.is_none_or(|b| !self.contains_binding(b))
    }

    fn register_singleton(&mut self, gid: GroupId, tab: Tab<T>) {
        self.tab_owner.insert(tab.id, gid);
        self.groups.insert(gid, Group::singleton(tab));
    }

    /// Append a new top-level cluster hosting `tab` as a singleton group (the
    /// new-session placement rule). A tab whose id or binding is already
    /// hosted is rejected and handed back.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn push_cluster(&mut self, tab: Tab<T>) -> Result<GroupId, Tab<T>> {
        if !self.admissible(&tab) {
            return Err(tab);
        }
        let gid = GroupId::mint();
        let weight = self.even_share_weight();
        self.clusters.push(Cluster {
            weight,
            root: LayoutNode::Leaf(gid),
        });
        self.register_singleton(gid, tab);
        self.debug_validate();
        Ok(gid)
    }

    /// Insert a new singleton-group cluster on the left edge (whole-grid Left
    /// drop). A tab whose id or binding is already hosted is rejected and
    /// handed back.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn insert_cluster_front(&mut self, tab: Tab<T>) -> Result<GroupId, Tab<T>> {
        if !self.admissible(&tab) {
            return Err(tab);
        }
        let gid = GroupId::mint();
        let weight = self.even_share_weight();
        self.clusters.insert(
            0,
            Cluster {
                weight,
                root: LayoutNode::Leaf(gid),
            },
        );
        self.register_singleton(gid, tab);
        self.debug_validate();
        Ok(gid)
    }

    /// Split a new singleton group hosting `tab` off the `reference` group,
    /// wherever it lives (a script pane stays within its owning cluster
    /// because its reference does). The tab is rejected and handed back —
    /// with the model unchanged — when the reference is unknown or the tab's
    /// id or binding is already hosted.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn split_group(
        &mut self,
        reference: GroupId,
        axis: Axis,
        new_first: bool,
        sizing: SplitSizing,
        tab: Tab<T>,
    ) -> Result<GroupId, Tab<T>> {
        if !self.groups.contains_key(&reference) || !self.admissible(&tab) {
            return Err(tab);
        }
        let gid = GroupId::mint();
        let split = self
            .clusters
            .iter_mut()
            .any(|c| c.root.split_leaf(reference, axis, new_first, sizing, gid));
        if !split {
            // A group known to the map is always a tree leaf, so the split
            // cannot miss; nothing has been mutated if it somehow does.
            debug_assert!(false, "group map and tree desynced");
            return Err(tab);
        }
        self.register_singleton(gid, tab);
        self.debug_validate();
        Ok(gid)
    }

    /// Fold the whole current layout into a single cluster and split `tab`,
    /// as a new singleton group, against it (whole-grid Top/Bottom edge
    /// drops). A tab whose id or binding is already hosted is rejected and
    /// handed back.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn wrap_all(
        &mut self,
        axis: Axis,
        new_first: bool,
        tab: Tab<T>,
    ) -> Result<GroupId, Tab<T>> {
        if !self.admissible(&tab) {
            return Err(tab);
        }
        let Some(combined) = self.combined_node() else {
            return self.push_cluster(tab);
        };
        let gid = GroupId::mint();
        let (a, b) = if new_first {
            (LayoutNode::Leaf(gid), combined)
        } else {
            (combined, LayoutNode::Leaf(gid))
        };
        self.clusters = vec![Cluster {
            weight: 1.0,
            root: LayoutNode::Split {
                axis,
                sizing: SplitSizing::Ratio(0.5),
                a: Box::new(a),
                b: Box::new(b),
            },
        }];
        self.register_singleton(gid, tab);
        self.debug_validate();
        Ok(gid)
    }

    /// The current clusters folded into one node (right-associated, weights
    /// becoming ratios) — the shape `build` emits for the top level.
    fn combined_node(&self) -> Option<LayoutNode> {
        let mut iter = self.clusters.iter().rev();
        let mut acc = iter.next()?.root.clone();
        let mut rest_weight: f32 = self.clusters.last().map_or(0.0, |c| c.weight);
        for cluster in iter {
            let total = cluster.weight + rest_weight;
            acc = LayoutNode::Split {
                axis: Axis::Vertical,
                sizing: SplitSizing::Ratio(if total > 0.0 {
                    cluster.weight / total
                } else {
                    0.5
                }),
                a: Box::new(cluster.root.clone()),
                b: Box::new(acc),
            };
            rest_weight = total;
        }
        Some(acc)
    }

    /// Remove `group`'s leaf from the tree, collapsing its parent split; a
    /// cluster whose last group leaves is dropped. Tree-only — the caller
    /// settles the side maps.
    fn remove_group_leaf(&mut self, group: GroupId) -> bool {
        for (idx, cluster) in self.clusters.iter_mut().enumerate() {
            match cluster.root.remove(group) {
                Ok(true) => return true,
                Err(()) => {
                    self.clusters.remove(idx);
                    return true;
                }
                Ok(false) => {}
            }
        }
        false
    }

    /// The model address of `group`'s leaf.
    fn find_path(&self, group: GroupId) -> Option<(usize, Vec<Branch>)> {
        for (idx, cluster) in self.clusters.iter().enumerate() {
            let mut path = Vec::new();
            if find_leaf_path(&cluster.root, group, &mut path) {
                return Some((idx, path));
            }
        }
        None
    }

    /// Apply a script `pane.resize` for one axis: re-size the nearest
    /// in-cluster ancestor split on `axis` above `group`'s leaf to a
    /// script-owned px sizing — re-resolved against the region at every
    /// rebuild until a user drag re-owns the edge (last writer wins, both
    /// directions). Between the leaf and that ancestor lie only splits of the
    /// other axis, so the sized branch's extent along `axis` *is* the
    /// group's. Returns `false` (no-op) when the leaf has no such ancestor:
    /// the group spans its cluster on that axis, and the top-level cluster
    /// fold is the sessions' share of the window, not a pane edge.
    pub fn set_group_px(&mut self, group: GroupId, axis: Axis, px: f32) -> bool {
        let Some((cluster, path)) = self.find_path(group) else {
            return false;
        };
        let Some(root) = self.clusters.get_mut(cluster).map(|c| &mut c.root) else {
            return false;
        };
        for depth in (0..path.len()).rev() {
            let Some(LayoutNode::Split {
                axis: split_axis,
                sizing,
                ..
            }) = root.node_at_mut(&path[..depth])
            else {
                continue;
            };
            if *split_axis == axis {
                *sizing = SplitSizing::Px {
                    px,
                    sized_first: path[depth] == Branch::A,
                };
                return true;
            }
        }
        false
    }

    /// Apply a user divider drag to the model. A top-level edge re-weights
    /// the clusters (preserving relative proportions within the remainder);
    /// an in-cluster edge becomes a user-owned `Ratio` (a px sizing is
    /// consumed by the drag).
    pub fn set_split_ratio(&mut self, target: &EdgeTarget, ratio: f32) {
        match target {
            EdgeTarget::TopLevel(i) => {
                // A target can outlive the build that emitted it: clusters
                // removed since then may leave a top-level index out of
                // range. A stale address rejects the drag as a no-op, like
                // every other stale input.
                if *i >= self.clusters.len() {
                    return;
                }
                let rest: f32 = self.clusters[i + 1..].iter().map(|c| c.weight).sum();
                let Some(cluster) = self.clusters.get_mut(*i) else {
                    return;
                };
                let group = cluster.weight + rest;
                if group <= 0.0 || rest <= 0.0 {
                    return;
                }
                cluster.weight = ratio * group;
                let scale = ((1.0 - ratio) * group) / rest;
                for c in &mut self.clusters[i + 1..] {
                    c.weight *= scale;
                }
            }
            EdgeTarget::Node { cluster, path } => {
                if let Some(LayoutNode::Split { sizing, .. }) = self
                    .clusters
                    .get_mut(*cluster)
                    .and_then(|c| c.root.node_at_mut(path))
                {
                    *sizing = SplitSizing::Ratio(ratio);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Tab operations
    // ------------------------------------------------------------------

    /// Make `tab` its group's durable selection.
    pub fn select(&mut self, tab: TabId) -> bool {
        let Some(&owner) = self.tab_owner.get(&tab) else {
            return false;
        };
        let Some(group) = self.groups.get_mut(&owner) else {
            debug_assert!(false, "owner map points at a missing group");
            return false;
        };
        group.selected = tab;
        self.debug_validate();
        true
    }

    /// Insert `tab` into `group`'s strip at `index` (clamped to the strip).
    /// The group's selection is unchanged. The tab is rejected and handed
    /// back when the group is unknown or the tab's id or binding is already
    /// hosted.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn insert_tab(&mut self, group: GroupId, index: usize, tab: Tab<T>) -> Result<(), Tab<T>> {
        if !self.admissible(&tab) {
            return Err(tab);
        }
        let Some(g) = self.groups.get_mut(&group) else {
            return Err(tab);
        };
        let index = index.min(g.tabs.len());
        let id = tab.id;
        g.tabs.insert(index, tab);
        self.tab_owner.insert(id, group);
        self.debug_validate();
        Ok(())
    }

    /// Insert `tab` at the end of `group`'s strip.
    #[must_use = "a rejected tab is handed back rather than dropped"]
    pub fn append_tab(&mut self, group: GroupId, tab: Tab<T>) -> Result<(), Tab<T>> {
        self.insert_tab(group, usize::MAX, tab)
    }

    /// Move `tab` to final position `index` (clamped) within its own strip:
    /// the index names the slot the tab *ends up in*, counted after the tab
    /// vacates its current slot — contrast [`Self::merge_tab`], whose
    /// `insertion_slot` is counted with the tab still in place. Selection is
    /// id-based and therefore unaffected. Reordering to the current position
    /// is an exact no-op.
    pub fn reorder_tab(&mut self, tab: TabId, index: usize) -> bool {
        let Some(&owner) = self.tab_owner.get(&tab) else {
            return false;
        };
        let Some(group) = self.groups.get_mut(&owner) else {
            debug_assert!(false, "owner map points at a missing group");
            return false;
        };
        let Some(from) = group.index_of(tab) else {
            debug_assert!(false, "owner map points at a group missing the tab");
            return false;
        };
        let to = index.min(group.tabs.len() - 1);
        if to != from {
            let moved = group.tabs.remove(from);
            group.tabs.insert(to, moved);
        }
        self.debug_validate();
        true
    }

    /// Rebind `tab` to a different live pane (or unbind it back to a
    /// placeholder). Structure, order, and selection are untouched. `false`
    /// when the tab is unknown or the binding is already hosted elsewhere.
    pub fn set_binding(&mut self, tab: TabId, binding: Option<T>) -> bool {
        let Some(&owner) = self.tab_owner.get(&tab) else {
            return false;
        };
        if let Some(b) = binding {
            let taken_elsewhere = self
                .groups
                .values()
                .any(|g| g.tabs.iter().any(|t| t.binding == Some(b) && t.id != tab));
            if taken_elsewhere {
                return false;
            }
        }
        let Some(slot) = self
            .groups
            .get_mut(&owner)
            .and_then(|g| g.tabs.iter_mut().find(|t| t.id == tab))
        else {
            debug_assert!(false, "owner map points at a group missing the tab");
            return false;
        };
        slot.binding = binding;
        self.debug_validate();
        true
    }

    /// Remove `tab` from its group, keeping the strip's remaining tabs in
    /// place. When the removed tab was selected, the next tab (else the
    /// previous) becomes selected; when it was the group's final tab and
    /// `allow_last` holds, the group's leaf is removed and its parent split
    /// collapses. `None` — with nothing mutated — when the tab is unknown, or
    /// when it is the final tab and `allow_last` is false.
    fn take_tab(&mut self, tab: TabId, allow_last: bool) -> Option<Tab<T>> {
        let &owner = self.tab_owner.get(&tab)?;
        let Some(group) = self.groups.get_mut(&owner) else {
            debug_assert!(false, "owner map points at a missing group");
            return None;
        };
        let Some(idx) = group.index_of(tab) else {
            debug_assert!(false, "owner map points at a group missing the tab");
            return None;
        };
        if group.tabs.len() == 1 {
            if !allow_last {
                return None;
            }
            let taken = group.tabs.remove(0);
            self.groups.remove(&owner);
            self.tab_owner.remove(&tab);
            let removed = self.remove_group_leaf(owner);
            debug_assert!(removed, "group map and tree desynced");
            return Some(taken);
        }
        if group.selected == tab {
            group.selected = if idx + 1 < group.tabs.len() {
                group.tabs[idx + 1].id
            } else {
                group.tabs[idx - 1].id
            };
        }
        let taken = group.tabs.remove(idx);
        self.tab_owner.remove(&tab);
        Some(taken)
    }

    /// Remove `tab` from the model entirely, with full collapse semantics
    /// (selection hand-off; leaf removal and split collapse on the final tab;
    /// cluster drop when emptied). Returns the removed tab — still carrying
    /// its stable id, so it can be re-hosted in another window's model.
    #[must_use = "the returned tab is the only remaining handle on its identity"]
    pub fn remove_tab(&mut self, tab: TabId) -> Option<Tab<T>> {
        let taken = self.take_tab(tab, true);
        self.debug_validate();
        taken
    }

    /// Remove `tab` from its group while *keeping* the group: the detach
    /// building block for gestures that re-host the tab elsewhere. `None` for
    /// an unknown tab or a group's final tab (detaching that would empty the
    /// group — use [`Self::remove_tab`] or a move operation instead).
    #[must_use = "the returned tab is the only remaining handle on its identity"]
    pub fn detach_tab(&mut self, tab: TabId) -> Option<Tab<T>> {
        let taken = self.take_tab(tab, false);
        self.debug_validate();
        taken
    }

    /// Move `tab` into `into`'s strip at `insertion_slot` (clamped),
    /// collapsing the source group when the tab was its last. The
    /// destination's selection is unchanged (a gesture that also activates
    /// the moved tab follows with [`Self::select`]). The slot is counted in
    /// the destination strip *as it stands before the move* — contrast
    /// [`Self::reorder_tab`], whose index is the tab's final position. With
    /// the tab's own group as the destination, `insertion_slot` is still
    /// counted with the tab in place — the strip-local reorder a header drop
    /// within one group means.
    ///
    /// Both participants are resolved before any mutation; an unknown tab or
    /// group rejects the whole operation.
    pub fn merge_tab(&mut self, tab: TabId, into: GroupId, insertion_slot: usize) -> bool {
        let Some(&owner) = self.tab_owner.get(&tab) else {
            return false;
        };
        if !self.groups.contains_key(&into) {
            return false;
        }
        if owner == into {
            // Same-strip move: translate the insertion slot (computed with
            // the tab still in place) into the final position.
            let Some(group) = self.groups.get_mut(&into) else {
                return false;
            };
            let Some(from) = group.index_of(tab) else {
                debug_assert!(false, "owner map points at a group missing the tab");
                return false;
            };
            let slot = insertion_slot.min(group.tabs.len());
            let to = if slot > from { slot - 1 } else { slot };
            if to != from {
                let moved = group.tabs.remove(from);
                group.tabs.insert(to, moved);
            }
            self.debug_validate();
            return true;
        }
        let Some(moved) = self.take_tab(tab, true) else {
            debug_assert!(false, "resolved tab failed to detach");
            return false;
        };
        // Collapsing the source leaf cannot disturb a different group's map
        // entry, so the destination is still present; the defensive arm
        // re-hosts the tab rather than ever losing it.
        if let Some(group) = self.groups.get_mut(&into) {
            let slot = insertion_slot.min(group.tabs.len());
            group.tabs.insert(slot, moved);
            self.tab_owner.insert(tab, into);
        } else {
            debug_assert!(false, "destination group vanished during a merge");
            let gid = GroupId::mint();
            let weight = self.even_share_weight();
            self.clusters.push(Cluster {
                weight,
                root: LayoutNode::Leaf(gid),
            });
            self.register_singleton(gid, moved);
        }
        self.debug_validate();
        true
    }

    /// Detach `tab` into a brand-new singleton group split beside the whole
    /// `beside` group (a body-edge drop). The source group collapses when the
    /// tab was its last; the new group's selection is the tab itself. `None`
    /// — with nothing mutated — when either participant is unknown, or when
    /// the tab already *is* the `beside` singleton (splitting a group beside
    /// itself has no arrangement to produce).
    pub fn split_tab_as_singleton(
        &mut self,
        tab: TabId,
        beside: GroupId,
        axis: Axis,
        new_first: bool,
        sizing: SplitSizing,
    ) -> Option<GroupId> {
        let &owner = self.tab_owner.get(&tab)?;
        let beside_len = self.groups.get(&beside)?.tabs.len();
        if owner == beside && beside_len == 1 {
            return None;
        }
        let moved = self.take_tab(tab, true)?;
        let gid = GroupId::mint();
        let split = self
            .clusters
            .iter_mut()
            .any(|c| c.root.split_leaf(beside, axis, new_first, sizing, gid));
        if !split {
            // Unreachable given the pre-resolution above (the source collapse
            // can only remove `owner`'s leaf, and `owner != beside` whenever
            // that collapse happens): re-host rather than lose the tab.
            debug_assert!(false, "reference group vanished during a split");
            let weight = self.even_share_weight();
            self.clusters.push(Cluster {
                weight,
                root: LayoutNode::Leaf(gid),
            });
        }
        self.register_singleton(gid, moved);
        self.debug_validate();
        Some(gid)
    }

    /// Exchange the exact strip slots of two tabs — same group or across
    /// groups — atomically. Selection follows the *slot*: a group whose
    /// selected tab is swapped away keeps rendering the same strip position,
    /// which now holds the incoming tab (the spatial center-swap contract).
    /// Both participants are resolved before any mutation; a missing tab, or
    /// `a == b`, is a no-op.
    pub fn swap_tabs(&mut self, a: TabId, b: TabId) -> bool {
        if a == b {
            return false;
        }
        let (Some(&ga), Some(&gb)) = (self.tab_owner.get(&a), self.tab_owner.get(&b)) else {
            return false;
        };
        let (Some(ia), Some(ib)) = (
            self.groups.get(&ga).and_then(|g| g.index_of(a)),
            self.groups.get(&gb).and_then(|g| g.index_of(b)),
        ) else {
            debug_assert!(false, "owner map points at a group missing the tab");
            return false;
        };
        if ga == gb {
            if let Some(g) = self.groups.get_mut(&ga) {
                g.tabs.swap(ia, ib);
            }
        } else {
            // The two strips live under different map entries: lift one group
            // out of the map for the duration of the element swap, so both
            // slots are mutably reachable without copying a tab.
            let Some(mut group_a) = self.groups.remove(&ga) else {
                return false;
            };
            if let Some(group_b) = self.groups.get_mut(&gb) {
                std::mem::swap(&mut group_a.tabs[ia], &mut group_b.tabs[ib]);
            }
            self.groups.insert(ga, group_a);
            self.tab_owner.insert(a, gb);
            self.tab_owner.insert(b, ga);
        }
        // Selection follows the slot: whichever of the two tabs a group had
        // selected, the other now occupies that slot. Each affected group is
        // adjusted exactly once (the flip is not idempotent).
        let affected: &[GroupId] = if ga == gb { &[ga] } else { &[ga, gb] };
        for gid in affected {
            let Some(g) = self.groups.get_mut(gid) else {
                continue;
            };
            if g.selected == a {
                g.selected = b;
            } else if g.selected == b {
                g.selected = a;
            }
        }
        self.debug_validate();
        true
    }

    // ------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------

    /// Check every model invariant: tree leaves and the group map hold the
    /// same id set with no duplicates, no group is empty, every selection is
    /// a member of its strip, every tab id and live binding occurs exactly
    /// once, and the owner map agrees with the strips. Test suites call this
    /// directly; every public mutation re-checks it under
    /// `debug_assertions`.
    pub fn validate(&self) -> Result<(), String> {
        let leaf_ids = self.groups_depth_first();
        let mut seen_groups = FxHashSet::default();
        for gid in &leaf_ids {
            if !seen_groups.insert(*gid) {
                return Err(format!("{gid:?} occurs twice in the tree"));
            }
            if !self.groups.contains_key(gid) {
                return Err(format!("tree leaf {gid:?} has no group content"));
            }
        }
        if seen_groups.len() != self.groups.len() {
            return Err("group map holds ids absent from the tree".to_owned());
        }
        let mut seen_tabs = FxHashSet::default();
        let mut seen_bindings = FxHashSet::default();
        for (gid, group) in &self.groups {
            if group.tabs.is_empty() {
                return Err(format!("{gid:?} is empty"));
            }
            if group.index_of(group.selected).is_none() {
                return Err(format!(
                    "{gid:?} selects {:?}, which is not in its strip",
                    group.selected
                ));
            }
            for tab in &group.tabs {
                if !seen_tabs.insert(tab.id) {
                    return Err(format!("{:?} occurs twice in the model", tab.id));
                }
                if self.tab_owner.get(&tab.id) != Some(gid) {
                    return Err(format!("owner map disagrees about {:?}", tab.id));
                }
                if let Some(binding) = tab.binding
                    && !seen_bindings.insert(binding)
                {
                    return Err(format!("{:?}'s binding is hosted twice", tab.id));
                }
            }
        }
        if self.tab_owner.len() != seen_tabs.len() {
            return Err("owner map holds tabs absent from every strip".to_owned());
        }
        Ok(())
    }

    fn debug_validate(&self) {
        #[cfg(debug_assertions)]
        if let Err(err) = self.validate() {
            panic!("pane-group invariant violated: {err}");
        }
    }

    // ------------------------------------------------------------------
    // Build
    // ------------------------------------------------------------------

    /// Derive the `pane_grid` configuration (plus its structural mirror, for
    /// mapping real `Split` ids back to model edges). The emitted payloads
    /// are stable group ids. `area` is the grid's current on-screen size —
    /// px sizings resolve against it; when unknown (zero, pre-first-layout)
    /// they fall back to even splits until the next rebuild. `None` when the
    /// model is empty.
    #[must_use]
    pub fn build(
        &self,
        area: Size,
        spacing: f32,
        min_size: f32,
    ) -> Option<(Configuration<GroupId>, BuiltNode)> {
        self.build_filtered(area, spacing, min_size, |_| true)
    }

    /// [`Self::build`], restricted to the tabs passing `visible` (the
    /// hidden-panes state): a group with no visible tab drops out like a
    /// hidden leaf, a split with one hidden side gives the other side the
    /// whole region, and a fully hidden cluster gives up its share of the
    /// window. Durable selection is never mutated by filtering — which tab a
    /// visible group renders is [`Self::effective_selected`]'s answer. The
    /// mirror's `EdgeTarget`s always address the unfiltered model, so a
    /// divider drag on a filtered grid writes through to the right edge. (A
    /// top-level drag still reweights against the full cluster list, so that
    /// divider can shift once hidden clusters are shown again.) `None` when
    /// no tab passes.
    #[must_use]
    pub fn build_filtered(
        &self,
        area: Size,
        spacing: f32,
        min_size: f32,
        visible: impl Fn(&Tab<T>) -> bool,
    ) -> Option<(Configuration<GroupId>, BuiltNode)> {
        let visible_groups: FxHashSet<GroupId> = self
            .groups
            .iter()
            .filter(|(_, g)| g.tabs.iter().any(&visible))
            .map(|(gid, _)| *gid)
            .collect();
        let vis: Vec<usize> = (0..self.clusters.len())
            .filter(|&i| self.clusters[i].root.any_visible(&visible_groups))
            .collect();
        let total: f32 = vis.iter().map(|&i| self.clusters[i].weight).sum();
        self.build_group(&vis, area, total, spacing, min_size, &visible_groups)
    }

    /// Build the visible clusters `vis` as a right-associated fold of
    /// vertical splits.
    fn build_group(
        &self,
        vis: &[usize],
        size: Size,
        group_weight: f32,
        spacing: f32,
        min_size: f32,
        visible: &FxHashSet<GroupId>,
    ) -> Option<(Configuration<GroupId>, BuiltNode)> {
        let (&i, rest) = vis.split_first()?;
        let cluster = &self.clusters[i];
        let mut path = Vec::new();
        if rest.is_empty() {
            return cluster
                .root
                .build(size, i, &mut path, spacing, min_size, visible);
        }
        let ratio = if group_weight > 0.0 {
            (cluster.weight / group_weight).clamp(0.05, 0.95)
        } else {
            0.5
        };
        let size_a = Size::new((size.width * ratio - spacing / 2.0).max(0.0), size.height);
        let size_b = Size::new(
            (size.width * (1.0 - ratio) - spacing / 2.0).max(0.0),
            size.height,
        );
        let (conf_a, built_a) = cluster
            .root
            .build(size_a, i, &mut path, spacing, min_size, visible)?;
        let (conf_b, built_b) = self.build_group(
            rest,
            size_b,
            group_weight - cluster.weight,
            spacing,
            min_size,
            visible,
        )?;
        Some((
            Configuration::Split {
                axis: Axis::Vertical,
                ratio,
                a: Box::new(conf_a),
                b: Box::new(conf_b),
            },
            BuiltNode::Split {
                target: EdgeTarget::TopLevel(i),
                a: Box::new(built_a),
                b: Box::new(built_b),
            },
        ))
    }
}

/// Record the branch path to `group`'s leaf into `path` (left as the prefix
/// walked so far on a miss — callers pass a fresh buffer per cluster).
fn find_leaf_path(node: &LayoutNode, group: GroupId, path: &mut Vec<Branch>) -> bool {
    match node {
        LayoutNode::Leaf(g) => *g == group,
        LayoutNode::Split { a, b, .. } => {
            path.push(Branch::A);
            if find_leaf_path(a, group, path) {
                return true;
            }
            path.pop();
            path.push(Branch::B);
            if find_leaf_path(b, group, path) {
                return true;
            }
            path.pop();
            false
        }
    }
}

/// Map every real divider in a freshly built grid back to its model edge by
/// walking the grid's layout tree in parallel with the build's structural
/// mirror (`State::with_configuration` preserves the configuration's shape).
#[must_use]
pub fn split_targets(
    node: &pane_grid::Node,
    built: &BuiltNode,
) -> BTreeMap<pane_grid::Split, EdgeTarget> {
    let mut map = BTreeMap::new();
    collect_split_targets(node, built, &mut map);
    map
}

fn collect_split_targets(
    node: &pane_grid::Node,
    built: &BuiltNode,
    map: &mut BTreeMap<pane_grid::Split, EdgeTarget>,
) {
    match (node, built) {
        (
            pane_grid::Node::Split { id, a, b, .. },
            BuiltNode::Split {
                target,
                a: built_a,
                b: built_b,
            },
        ) => {
            map.insert(*id, target.clone());
            collect_split_targets(a, built_a, map);
            collect_split_targets(b, built_b, map);
        }
        (pane_grid::Node::Pane(_), BuiltNode::Leaf) => {}
        _ => debug_assert!(false, "grid layout desynced from the model mirror"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SPACING: f32 = 4.0;
    const MIN: f32 = 50.0;
    const AREA: Size = Size::new(1600.0, 900.0);

    const A_MAIN: u32 = 1;
    const A_NOTES: u32 = 2;
    const B_MAIN: u32 = 11;
    const B_NOTES: u32 = 12;

    /// Flatten a configuration to (depth-first ratios, rendered leaf
    /// bindings) for equality checks — `Configuration` has no `PartialEq`,
    /// and two independently assembled models never share ids, so structure
    /// is compared through bindings.
    fn flatten(
        layout: &GroupLayout<u32>,
        conf: &Configuration<GroupId>,
        ratios: &mut Vec<f32>,
        leaves: &mut Vec<u32>,
    ) {
        match conf {
            Configuration::Pane(gid) => {
                let selected = layout.selected(*gid).expect("emitted group is live");
                let tab = layout.tab(selected).expect("selection is a member");
                leaves.push(*tab.binding().expect("test tabs are bound"));
            }
            Configuration::Split { ratio, a, b, .. } => {
                ratios.push(*ratio);
                flatten(layout, a, ratios, leaves);
                flatten(layout, b, ratios, leaves);
            }
        }
    }

    fn built(layout: &GroupLayout<u32>) -> (Vec<f32>, Vec<u32>) {
        let (conf, _) = layout.build(AREA, SPACING, MIN).expect("non-empty");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(layout, &conf, &mut ratios, &mut leaves);
        (ratios, leaves)
    }

    /// Flatten a configuration to (depth-first ratios, leaf group ids) — the
    /// identity-level shape, indifferent to whether tabs are bound.
    fn flatten_ids(
        conf: &Configuration<GroupId>,
        ratios: &mut Vec<f32>,
        leaves: &mut Vec<GroupId>,
    ) {
        match conf {
            Configuration::Pane(gid) => leaves.push(*gid),
            Configuration::Split { ratio, a, b, .. } => {
                ratios.push(*ratio);
                flatten_ids(a, ratios, leaves);
                flatten_ids(b, ratios, leaves);
            }
        }
    }

    /// Every hosted binding, in model order.
    fn bindings(layout: &GroupLayout<u32>) -> Vec<u32> {
        layout
            .panes()
            .iter()
            .map(|t| *t.binding().expect("test tabs are bound"))
            .collect()
    }

    fn push(layout: &mut GroupLayout<u32>, binding: u32) -> (GroupId, TabId) {
        let tab = Tab::bound(binding);
        let id = tab.id();
        let gid = layout.push_cluster(tab).expect("fresh tab");
        (gid, id)
    }

    /// A session's script splitting a 200px notes pane off its main group.
    fn script_split(layout: &mut GroupLayout<u32>, main: GroupId, notes: u32) -> (GroupId, TabId) {
        let tab = Tab::bound(notes);
        let id = tab.id();
        let gid = layout
            .split_group(
                main,
                Axis::Vertical,
                false,
                SplitSizing::Px {
                    px: 200.0,
                    sized_first: false,
                },
                tab,
            )
            .expect("reference group is live");
        (gid, id)
    }

    /// A three-tab group beside a singleton: the standard multi-tab fixture.
    /// Returns (multi group, its tab ids, singleton group, its tab id).
    fn multi_fixture(layout: &mut GroupLayout<u32>) -> (GroupId, [TabId; 3], GroupId, TabId) {
        let (multi, t0) = push(layout, 100);
        let t1 = Tab::bound(101);
        let t2 = Tab::bound(102);
        let (id1, id2) = (t1.id(), t2.id());
        assert!(layout.append_tab(multi, t1).is_ok());
        assert!(layout.append_tab(multi, t2).is_ok());
        let (single, s0) = push(layout, 200);
        (multi, [t0, id1, id2], single, s0)
    }

    #[test]
    fn structure_mirrors_the_forest_shape() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        let (a_notes, _) = script_split(&mut layout, a_main, A_NOTES);
        let (b_main, _) = push(&mut layout, B_MAIN);

        let mirror = layout.structure();
        assert_eq!(mirror.len(), 2, "one cluster per session");
        assert!((mirror[0].0 - 1.0).abs() < f32::EPSILON);
        match &mirror[0].1 {
            StructureNode::Split { axis, sizing, a, b } => {
                assert_eq!(*axis, Axis::Vertical);
                assert_eq!(
                    *sizing,
                    SplitSizing::Px {
                        px: 200.0,
                        sized_first: false,
                    }
                );
                assert_eq!(**a, StructureNode::Leaf(a_main));
                assert_eq!(**b, StructureNode::Leaf(a_notes));
            }
            other => panic!("expected a split mirror, got {other:?}"),
        }
        assert_eq!(mirror[1].1, StructureNode::Leaf(b_main));

        // The mirror is model order, not an artifact of map iteration: it
        // lists exactly the groups the depth-first walk reports.
        let mut mirrored = Vec::new();
        fn collect(node: &StructureNode, out: &mut Vec<GroupId>) {
            match node {
                StructureNode::Leaf(gid) => out.push(*gid),
                StructureNode::Split { a, b, .. } => {
                    collect(a, out);
                    collect(b, out);
                }
            }
        }
        for (_, node) in &mirror {
            collect(node, &mut mirrored);
        }
        assert_eq!(mirrored, layout.groups_depth_first());
    }

    #[test]
    fn from_blueprint_reconstructs_structure_content_and_selection() {
        let main = Tab::bound(A_MAIN);
        let notes = Tab::bound(A_NOTES);
        let chat = Tab::placeholder();
        let (notes_id, chat_id) = (notes.id(), chat.id());
        let other = Tab::bound(B_MAIN);
        let layout = GroupLayout::from_blueprint(vec![
            (
                2.0,
                Blueprint::Split {
                    axis: Axis::Vertical,
                    sizing: SplitSizing::Px {
                        px: 240.0,
                        sized_first: false,
                    },
                    a: Box::new(Blueprint::Group {
                        tabs: vec![main],
                        selected: 0,
                    }),
                    b: Box::new(Blueprint::Group {
                        tabs: vec![notes, chat],
                        selected: 1, // the placeholder is durably selected
                    }),
                },
            ),
            (
                1.0,
                Blueprint::Group {
                    tabs: vec![other],
                    selected: 0,
                },
            ),
        ]);

        assert!(layout.validate().is_ok());
        let mirror = layout.structure();
        assert_eq!(mirror.len(), 2);
        assert!((mirror[0].0 - 2.0).abs() < f32::EPSILON);
        let StructureNode::Split {
            axis, sizing, b, ..
        } = &mirror[0].1
        else {
            panic!("expected the blueprint split to survive");
        };
        assert_eq!(*axis, Axis::Vertical);
        assert_eq!(
            *sizing,
            SplitSizing::Px {
                px: 240.0,
                sized_first: false,
            }
        );
        let StructureNode::Leaf(scripts) = **b else {
            panic!("leaf expected");
        };
        assert_eq!(layout.selected(scripts), Some(chat_id));
        assert_eq!(layout.tabs(scripts).unwrap().len(), 2);
        assert!(layout.contains_tab(notes_id));
        assert!(layout.tab(chat_id).unwrap().binding().is_none());
        // The placeholder participates in structure but carries no binding.
        assert!(layout.contains_binding(A_NOTES));
        assert!(!layout.contains_binding(3));
    }

    #[test]
    fn from_blueprint_drops_duplicates_and_collapses_emptied_nodes() {
        // A duplicate binding within one strip and a whole-group duplicate:
        // the survivors keep the invariants (exactly-once) intact, the
        // emptied group collapses out of its split, and selection clamps.
        let first = Tab::bound(A_MAIN);
        let duplicate_binding = Tab::bound(A_MAIN);
        let dup_id = duplicate_binding.id();
        let second_copy = Tab::bound(A_MAIN);
        let layout = GroupLayout::from_blueprint(vec![(
            1.0,
            Blueprint::Split {
                axis: Axis::Horizontal,
                sizing: SplitSizing::Ratio(0.5),
                a: Box::new(Blueprint::Group {
                    tabs: vec![first, duplicate_binding],
                    selected: 1, // selects the tab that gets dropped
                }),
                b: Box::new(Blueprint::Group {
                    tabs: vec![second_copy],
                    selected: 0,
                }),
            },
        )]);

        assert!(layout.validate().is_ok());
        assert_eq!(layout.panes().len(), 1, "one binding, hosted exactly once");
        assert!(!layout.contains_tab(dup_id));
        // The b-side group emptied entirely: the split collapsed.
        assert!(matches!(layout.structure()[0].1, StructureNode::Leaf(_)));
    }

    // ------------------------------------------------------------------
    // Singleton-group behavior (one tab per group)
    // ------------------------------------------------------------------

    #[test]
    fn cluster_placement_is_order_independent() {
        // Order 1: A's script fires before B's session opens.
        let mut first = GroupLayout::new();
        let (a_main, _) = push(&mut first, A_MAIN);
        script_split(&mut first, a_main, A_NOTES);
        let (b_main, _) = push(&mut first, B_MAIN);
        script_split(&mut first, b_main, B_NOTES);

        // Order 2: both sessions open, then both scripts fire.
        let mut second = GroupLayout::new();
        let (a_main, _) = push(&mut second, A_MAIN);
        let (b_main, _) = push(&mut second, B_MAIN);
        script_split(&mut second, a_main, A_NOTES);
        script_split(&mut second, b_main, B_NOTES);

        assert_eq!(built(&first), built(&second));

        // Both clusters divide the window evenly, and each notes pane
        // measures its requested 200px against its cluster's *final* extent.
        let (ratios, leaves) = built(&first);
        assert_eq!(leaves, vec![A_MAIN, A_NOTES, B_MAIN, B_NOTES]);
        assert!((ratios[0] - 0.5).abs() < 1e-6, "top divider: {}", ratios[0]);
        let cluster_w = AREA.width * 0.5 - SPACING / 2.0;
        let expected = 1.0 - (200.0 + SPACING / 2.0) / cluster_w;
        assert!(
            (ratios[1] - expected).abs() < 1e-6 && (ratios[2] - expected).abs() < 1e-6,
            "notes dividers {ratios:?} vs {expected}"
        );
        // ...i.e. the notes region really is 200px wide.
        let notes_px = cluster_w * (1.0 - ratios[1]) - SPACING / 2.0;
        assert!((notes_px - 200.0).abs() < 1.0, "notes width {notes_px}");
    }

    #[test]
    fn new_clusters_take_an_even_share() {
        let mut layout = GroupLayout::new();
        push(&mut layout, 1);
        push(&mut layout, 2);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.5).abs() < 1e-6);
        push(&mut layout, 3);
        let (ratios, _) = built(&layout);
        // Three even clusters: first divider 1/3, second 1/2.
        assert!((ratios[0] - 1.0 / 3.0).abs() < 1e-6, "{ratios:?}");
        assert!((ratios[1] - 0.5).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn user_resize_converts_px_to_owned_ratio_and_reweights_top_level() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        script_split(&mut layout, a_main, A_NOTES);
        push(&mut layout, B_MAIN);

        // Drag the notes divider: the px sizing becomes a user-owned ratio
        // that later rebuilds carry verbatim (a later build with a different
        // area no longer re-resolves 200px).
        layout.set_split_ratio(
            &EdgeTarget::Node {
                cluster: 0,
                path: Vec::new(),
            },
            0.7,
        );
        let (conf, _) = layout.build(Size::new(800.0, 600.0), SPACING, MIN).unwrap();
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&layout, &conf, &mut ratios, &mut leaves);
        assert!((ratios[1] - 0.7).abs() < 1e-6, "{ratios:?}");

        // Drag the top-level divider to 0.25/0.75.
        layout.set_split_ratio(&EdgeTarget::TopLevel(0), 0.25);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.25).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn stale_top_level_targets_reject_the_drag() {
        // A held top-level target outlives the removal of the clusters
        // behind it: the now out-of-range index must reject the drag as a
        // no-op, like every other stale input.
        let mut layout = GroupLayout::new();
        push(&mut layout, 1);
        let (_, t2) = push(&mut layout, 2);
        let (_, t3) = push(&mut layout, 3);
        let target = EdgeTarget::TopLevel(1);
        layout.set_split_ratio(&target, 0.3);

        assert!(layout.remove_tab(t2).is_some());
        assert!(layout.remove_tab(t3).is_some());
        let before = built(&layout);
        layout.set_split_ratio(&target, 0.6);
        assert_eq!(built(&layout), before, "stale target leaves the model");
        layout.validate().unwrap();
    }

    #[test]
    fn set_group_px_targets_the_nearest_axis_ancestor() {
        // main | (chat over notes): chat's width edge is the vertical split
        // at the cluster root; its height edge is the horizontal split below.
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        let (a_notes, _) = script_split(&mut layout, a_main, A_NOTES);
        let chat = Tab::bound(3);
        let a_chat = layout
            .split_group(
                a_notes,
                Axis::Horizontal,
                true,
                SplitSizing::Ratio(0.5),
                chat,
            )
            .expect("notes group is live");

        // Height: the nearest horizontal ancestor, chat on the A side.
        assert!(layout.set_group_px(a_chat, Axis::Horizontal, 120.0));
        // Width: the nearest vertical ancestor is the cluster root; chat's
        // subtree is the B side there, so the px sizes the second child.
        assert!(layout.set_group_px(a_chat, Axis::Vertical, 300.0));
        let (ratios, leaves) = built(&layout);
        assert_eq!(leaves, vec![A_MAIN, 3, A_NOTES]);
        // Root: 300px for the (chat|notes) side of a 1600px area.
        assert!(
            (ratios[0] - (1.0 - 302.0 / 1600.0)).abs() < 1e-4,
            "{ratios:?}"
        );
        // Inner: 120px for chat (the first child) of the 900px height.
        assert!((ratios[1] - 122.0 / 900.0).abs() < 1e-4, "{ratios:?}");

        // A group spanning its cluster on an axis has no edge there: main is
        // full-height, so a height resize is a no-op — and the top-level
        // cluster fold is the sessions' share, not a pane edge, so a lone
        // main has no width edge either.
        assert!(!layout.set_group_px(a_main, Axis::Horizontal, 100.0));
        let mut solo = GroupLayout::new();
        let (b_main, _) = push(&mut solo, B_MAIN);
        assert!(!solo.set_group_px(b_main, Axis::Vertical, 100.0));
        assert!(
            !solo.set_group_px(a_main, Axis::Vertical, 100.0),
            "unknown group"
        );
    }

    #[test]
    fn remove_collapses_splits_and_drops_empty_clusters() {
        let mut layout = GroupLayout::new();
        let (a_main, a_main_tab) = push(&mut layout, A_MAIN);
        let (_, a_notes_tab) = script_split(&mut layout, a_main, A_NOTES);
        let (_, b_main_tab) = push(&mut layout, B_MAIN);

        assert!(layout.remove_tab(a_notes_tab).is_some());
        assert!(!layout.contains_tab(a_notes_tab));
        assert_eq!(bindings(&layout), vec![A_MAIN, B_MAIN]);

        assert!(layout.remove_tab(a_main_tab).is_some());
        assert_eq!(bindings(&layout), vec![B_MAIN]);
        assert!(layout.remove_tab(b_main_tab).is_some());
        assert!(layout.panes().is_empty());
        assert!(layout.is_empty());
        assert!(layout.build(AREA, SPACING, MIN).is_none());
        assert!(layout.remove_tab(b_main_tab).is_none());
    }

    #[test]
    fn swap_exchanges_two_singleton_groups() {
        let mut layout = GroupLayout::new();
        let (a_main, a_main_tab) = push(&mut layout, A_MAIN);
        let (_, a_notes_tab) = script_split(&mut layout, a_main, A_NOTES);
        let (_, b_main_tab) = push(&mut layout, B_MAIN);
        assert!(layout.swap_tabs(a_notes_tab, b_main_tab));
        assert_eq!(bindings(&layout), vec![A_MAIN, B_MAIN, A_NOTES]);
        // Swapping with itself or a missing tab is a no-op.
        assert!(!layout.swap_tabs(a_main_tab, a_main_tab));
        let stale = Tab::<u32>::placeholder().id();
        assert!(!layout.swap_tabs(a_main_tab, stale));
        assert_eq!(bindings(&layout), vec![A_MAIN, B_MAIN, A_NOTES]);
    }

    #[test]
    fn rebinding_changes_only_the_tab_binding() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        let (_, a_notes_tab) = script_split(&mut layout, a_main, A_NOTES);
        let before = built(&layout).0;
        let groups_before = layout.groups_depth_first();
        assert!(layout.set_binding(a_notes_tab, Some(B_MAIN)));
        assert_eq!(bindings(&layout), vec![A_MAIN, B_MAIN]);
        assert_eq!(built(&layout).0, before, "split geometry is preserved");
        assert_eq!(
            layout.groups_depth_first(),
            groups_before,
            "identity is preserved"
        );
        // Unbinding turns the tab into a placeholder without touching
        // structure; a stale tab rejects the rebind.
        assert!(layout.set_binding(a_notes_tab, None));
        assert!(layout.tab(a_notes_tab).unwrap().binding().is_none());
        let stale = Tab::<u32>::placeholder().id();
        assert!(!layout.set_binding(stale, Some(99)));
    }

    #[test]
    fn wrap_all_and_edge_inserts() {
        let mut layout = GroupLayout::new();
        push(&mut layout, 1);
        push(&mut layout, 2);
        layout
            .insert_cluster_front(Tab::bound(3))
            .expect("fresh tab");
        assert_eq!(bindings(&layout), vec![3, 1, 2]);

        layout
            .wrap_all(Axis::Horizontal, true, Tab::bound(4))
            .expect("fresh tab");
        assert_eq!(bindings(&layout), vec![4, 3, 1, 2]);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.5).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn split_targets_map_every_grid_divider() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        script_split(&mut layout, a_main, A_NOTES);
        push(&mut layout, B_MAIN);

        let (conf, mirror) = layout.build(AREA, SPACING, MIN).unwrap();
        let state = pane_grid::State::with_configuration(conf);
        let map = split_targets(state.layout(), &mirror);
        assert_eq!(map.len(), 2, "one top-level + one in-cluster divider");
        assert!(map.values().any(|t| *t == EdgeTarget::TopLevel(0)));
        assert!(map.values().any(|t| matches!(
            t,
            EdgeTarget::Node { cluster: 0, path } if path.is_empty()
        )));

        // Round-trip: applying a resize through the mapped target changes
        // exactly that edge on the next build.
        let (&_split, target) = map
            .iter()
            .find(|(_, t)| matches!(t, EdgeTarget::Node { .. }))
            .unwrap();
        layout.set_split_ratio(target, 0.6);
        let (ratios, _) = built(&layout);
        assert!((ratios[1] - 0.6).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn filtered_build_drops_hidden_leaves_and_clusters() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        script_split(&mut layout, a_main, A_NOTES);
        let (b_main, _) = push(&mut layout, B_MAIN);
        script_split(&mut layout, b_main, B_NOTES);

        // Hiding one group's only tab collapses its split: the sibling takes
        // the whole cluster region and the split emits no divider.
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, |t| t.binding() != Some(&A_NOTES))
            .expect("visible panes remain");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&layout, &conf, &mut ratios, &mut leaves);
        assert_eq!(leaves, vec![A_MAIN, B_MAIN, B_NOTES]);
        assert_eq!(ratios.len(), 2, "top-level + B's notes divider only");

        // Hiding a whole cluster removes it from the top-level fold.
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, |t| {
                t.binding() == Some(&A_MAIN) || t.binding() == Some(&A_NOTES)
            })
            .expect("cluster A remains");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&layout, &conf, &mut ratios, &mut leaves);
        assert_eq!(leaves, vec![A_MAIN, A_NOTES]);
        assert_eq!(ratios.len(), 1, "no top-level divider for one cluster");

        // Nothing visible: nothing to build. The model itself is untouched.
        assert!(
            layout
                .build_filtered(AREA, SPACING, MIN, |_| false)
                .is_none()
        );
        let (_, leaves) = built(&layout);
        assert_eq!(leaves, vec![A_MAIN, A_NOTES, B_MAIN, B_NOTES]);
    }

    #[test]
    fn filtered_split_targets_address_the_unfiltered_model() {
        // main | (notes | chat), with notes hidden: the one emitted divider
        // separates main from chat but must map to the *outer* model edge, so
        // dragging it writes through to the real model.
        const A_CHAT: u32 = 3;
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        let (a_notes, _) = script_split(&mut layout, a_main, A_NOTES);
        layout
            .split_group(
                a_notes,
                Axis::Horizontal,
                false,
                SplitSizing::Ratio(0.5),
                Tab::bound(A_CHAT),
            )
            .expect("notes group is live");

        let (conf, mirror) = layout
            .build_filtered(AREA, SPACING, MIN, |t| t.binding() != Some(&A_NOTES))
            .expect("two panes visible");
        let state = pane_grid::State::with_configuration(conf);
        let map = split_targets(state.layout(), &mirror);
        assert_eq!(map.len(), 1);
        let target = map.values().next().unwrap();
        assert_eq!(
            *target,
            EdgeTarget::Node {
                cluster: 0,
                path: Vec::new(),
            }
        );

        layout.set_split_ratio(target, 0.6);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.6).abs() < 1e-6, "{ratios:?}");
    }

    // ------------------------------------------------------------------
    // Tab-group acceptance
    // ------------------------------------------------------------------

    #[test]
    fn removing_tabs_hands_selection_to_next_then_previous() {
        let mut layout = GroupLayout::new();
        let (group, t0) = push(&mut layout, 0);
        let extra: Vec<TabId> = (1..4)
            .map(|n| {
                let tab = Tab::bound(n);
                let id = tab.id();
                assert!(layout.append_tab(group, tab).is_ok());
                id
            })
            .collect();
        let (t1, t2, t3) = (extra[0], extra[1], extra[2]);

        // Removing an unselected first tab: order shrinks, selection stays.
        assert!(layout.select(t1));
        assert!(layout.remove_tab(t0).is_some());
        assert_eq!(bindings(&layout), vec![1, 2, 3]);
        assert_eq!(layout.selected(group), Some(t1));

        // Removing the selected middle tab selects the *next* tab.
        assert!(layout.select(t2));
        assert!(layout.remove_tab(t2).is_some());
        assert_eq!(layout.selected(group), Some(t3));

        // Removing the selected last tab selects the *previous* tab.
        assert!(layout.remove_tab(t3).is_some());
        assert_eq!(layout.selected(group), Some(t1));
        assert_eq!(bindings(&layout), vec![1]);
    }

    #[test]
    fn removing_the_final_tab_collapses_the_leaf() {
        let mut layout = GroupLayout::new();
        let (a_main, _) = push(&mut layout, A_MAIN);
        let (a_notes, notes_tab) = script_split(&mut layout, a_main, A_NOTES);
        push(&mut layout, B_MAIN);

        assert!(layout.remove_tab(notes_tab).is_some());
        assert!(!layout.contains_group(a_notes), "the group leaf is gone");
        assert_eq!(bindings(&layout), vec![A_MAIN, B_MAIN]);
        // The collapsed cluster is a lone leaf again: no in-cluster divider.
        let (ratios, _) = built(&layout);
        assert_eq!(ratios.len(), 1, "top-level divider only: {ratios:?}");
    }

    #[test]
    fn merge_moves_one_tab_between_groups() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, _t2], single, s0) = multi_fixture(&mut layout);

        // Singleton → multi: the source group collapses; the destination's
        // selection is unchanged (activation is a separate `select`).
        assert!(layout.select(t1));
        assert!(layout.merge_tab(s0, multi, 1));
        assert!(!layout.contains_group(single));
        assert_eq!(bindings(&layout), vec![100, 200, 101, 102]);
        assert_eq!(layout.selected(multi), Some(t1));
        assert_eq!(layout.group_of(s0), Some(multi));

        // Multi → fresh singleton beside it, then merge back at a clamped
        // index: an oversized index appends.
        let dest = layout
            .split_tab_as_singleton(s0, multi, Axis::Vertical, false, SplitSizing::Ratio(0.5))
            .expect("participants are live");
        assert!(layout.merge_tab(t0, dest, 999));
        assert_eq!(layout.group_of(t0), Some(dest));
        assert_eq!(layout.tabs(dest).unwrap().len(), 2);

        // Missing participants reject the whole operation.
        let stale = Tab::<u32>::placeholder().id();
        let before = bindings(&layout);
        assert!(!layout.merge_tab(stale, multi, 0));
        assert!(!layout.merge_tab(t1, GroupId::mint(), 0));
        assert_eq!(bindings(&layout), before);
    }

    #[test]
    fn merge_into_own_group_reorders() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, t2], _, _) = multi_fixture(&mut layout);

        // The insertion slot is computed with the tab still in place: moving
        // t0 to slot 2 lands it between t1 and t2.
        assert!(layout.merge_tab(t0, multi, 2));
        assert_eq!(bindings(&layout), vec![101, 100, 102, 200]);
        // Merging a tab onto its own current slot is an exact no-op.
        let before_bindings = bindings(&layout);
        let before_selected = layout.selected(multi);
        assert!(layout.merge_tab(t1, multi, 0));
        assert_eq!(bindings(&layout), before_bindings);
        assert_eq!(layout.selected(multi), before_selected);
        // Selection rides the tab id through same-strip moves.
        assert!(layout.select(t2));
        assert!(layout.merge_tab(t2, multi, 0));
        assert_eq!(bindings(&layout), vec![102, 101, 100, 200]);
        assert_eq!(layout.selected(multi), Some(t2));
    }

    #[test]
    fn reorder_moves_within_the_strip_only() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, _t1, t2], _, s0) = multi_fixture(&mut layout);

        assert!(layout.select(t0));
        assert!(layout.reorder_tab(t0, 2));
        assert_eq!(bindings(&layout), vec![101, 102, 100, 200]);
        assert_eq!(layout.selected(multi), Some(t0), "selection rides the id");
        // Oversized indices clamp to the end; the current position is an
        // exact no-op; a singleton reorders trivially; stale ids reject.
        assert!(layout.reorder_tab(t2, 99));
        assert_eq!(bindings(&layout), vec![101, 100, 102, 200]);
        assert!(layout.reorder_tab(t2, 2));
        assert_eq!(bindings(&layout), vec![101, 100, 102, 200]);
        assert!(layout.reorder_tab(s0, 0));
        assert!(!layout.reorder_tab(Tab::<u32>::placeholder().id(), 0));
    }

    #[test]
    fn split_tab_as_singleton_detaches_and_splits() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, _t2], single, s0) = multi_fixture(&mut layout);

        // Multi-tab source: the tab leaves the strip and lands in its own
        // group beside the target; the new group selects it.
        assert!(layout.select(t1));
        let gid = layout
            .split_tab_as_singleton(t1, single, Axis::Horizontal, true, SplitSizing::Ratio(0.5))
            .expect("participants are live");
        assert_eq!(layout.group_of(t1), Some(gid));
        assert_eq!(layout.selected(gid), Some(t1));
        assert_eq!(layout.selected(multi), Some(_t2), "next-neighbor rule");
        assert_eq!(bindings(&layout), vec![100, 102, 101, 200]);

        // Singleton source beside another group: a relocate — the source
        // leaf collapses.
        let gid2 = layout
            .split_tab_as_singleton(s0, multi, Axis::Vertical, false, SplitSizing::Ratio(0.5))
            .expect("participants are live");
        assert!(!layout.contains_group(single));
        assert_eq!(layout.group_of(s0), Some(gid2));

        // A singleton split beside itself has no arrangement to produce.
        let before = bindings(&layout);
        assert!(
            layout
                .split_tab_as_singleton(s0, gid2, Axis::Vertical, true, SplitSizing::Ratio(0.5))
                .is_none()
        );
        assert_eq!(bindings(&layout), before);
        // ...but a multi-tab member splitting beside its own group is valid.
        let gid3 = layout
            .split_tab_as_singleton(t0, multi, Axis::Vertical, true, SplitSizing::Ratio(0.5))
            .expect("multi-tab member beside its own group");
        assert_eq!(layout.group_of(t0), Some(gid3));
        layout.validate().unwrap();
    }

    #[test]
    fn merge_into_the_collapsing_sources_split_sibling() {
        // sibling | source: merging the source group's final tab into its own
        // split-sibling collapses the source leaf while the destination is
        // the very node that survives the collapse.
        let mut layout = GroupLayout::new();
        let (sibling, s_tab) = push(&mut layout, 1);
        let solo = Tab::bound(2);
        let solo_id = solo.id();
        let source = layout
            .split_group(
                sibling,
                Axis::Vertical,
                false,
                SplitSizing::Ratio(0.5),
                solo,
            )
            .expect("reference group is live");

        assert!(layout.merge_tab(solo_id, sibling, 0));
        assert!(!layout.contains_group(source), "the source leaf collapsed");
        assert_eq!(layout.group_of(solo_id), Some(sibling));
        assert_eq!(bindings(&layout), vec![2, 1]);
        assert_eq!(
            layout.selected(sibling),
            Some(s_tab),
            "destination selection is unchanged"
        );
        // The cluster is a lone leaf again: no divider.
        let (ratios, _) = built(&layout);
        assert!(ratios.is_empty(), "{ratios:?}");
        layout.validate().unwrap();
    }

    #[test]
    fn split_as_singleton_beside_the_collapsing_sources_sibling() {
        // sibling | source: detaching the source group's final tab collapses
        // its leaf, and the new singleton splits beside the survivor.
        let mut layout = GroupLayout::new();
        let (sibling, _) = push(&mut layout, 1);
        let solo = Tab::bound(2);
        let solo_id = solo.id();
        let source = layout
            .split_group(
                sibling,
                Axis::Vertical,
                false,
                SplitSizing::Ratio(0.5),
                solo,
            )
            .expect("reference group is live");

        let gid = layout
            .split_tab_as_singleton(
                solo_id,
                sibling,
                Axis::Horizontal,
                true,
                SplitSizing::Ratio(0.5),
            )
            .expect("the sibling survives the source collapse");
        assert!(!layout.contains_group(source));
        assert_eq!(layout.group_of(solo_id), Some(gid));
        assert_eq!(layout.selected(gid), Some(solo_id));
        assert_eq!(bindings(&layout), vec![2, 1], "new-first singleton leads");
        let (ratios, _) = built(&layout);
        assert_eq!(ratios.len(), 1, "one divider beside the sibling");
        layout.validate().unwrap();
    }

    #[test]
    fn swap_preserves_each_groups_rendered_slot() {
        // Cross-group, one side selected: the destination keeps rendering
        // the same strip slot, which now holds the incoming tab.
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, _t2], single, s0) = multi_fixture(&mut layout);
        assert!(layout.select(t1));
        assert!(layout.swap_tabs(t1, s0));
        assert_eq!(bindings(&layout), vec![100, 200, 102, 101]);
        assert_eq!(layout.selected(multi), Some(s0), "slot stays selected");
        assert_eq!(layout.selected(single), Some(t1), "singleton follows too");
        assert_eq!(layout.group_of(t1), Some(single));
        assert_eq!(layout.group_of(s0), Some(multi));

        // Both sides selected: each group keeps its slot.
        assert!(layout.swap_tabs(s0, t1));
        assert_eq!(layout.selected(multi), Some(t1));
        assert_eq!(layout.selected(single), Some(s0));
        assert_eq!(bindings(&layout), vec![100, 101, 102, 200]);

        // Same-group swap with the selected tab: the selected slot keeps its
        // position and renders the exchanged tab.
        assert_eq!(layout.selected(multi), Some(t1));
        assert!(layout.swap_tabs(t1, t0));
        assert_eq!(bindings(&layout), vec![101, 100, 102, 200]);
        assert_eq!(layout.selected(multi), Some(t0), "slot 1 stays selected");
        // Same-group swap of two unselected tabs leaves selection alone.
        assert!(layout.swap_tabs(t1, _t2));
        assert_eq!(layout.selected(multi), Some(t0));
        layout.validate().unwrap();
    }

    #[test]
    fn detach_keeps_the_group_and_the_tab_identity() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, _t2], _, s0) = multi_fixture(&mut layout);

        // Detaching the selected tab hands selection off like a removal, but
        // the group survives and the tab keeps its id and binding.
        assert!(layout.select(t0));
        let detached = layout.detach_tab(t0).expect("multi-tab group");
        assert_eq!(detached.id(), t0);
        assert_eq!(detached.binding(), Some(&100));
        assert!(layout.contains_group(multi));
        assert!(!layout.contains_tab(t0));
        assert_eq!(layout.selected(multi), Some(t1));

        // A group's final tab cannot be detached (the group would empty).
        assert!(layout.detach_tab(s0).is_none());
        assert!(layout.contains_tab(s0));

        // Cross-model move: the detached tab re-hosts in another window's
        // model with its identity intact, and occupancy is checkable across
        // both models.
        let mut other = GroupLayout::new();
        let dest = other.push_cluster(Tab::bound(999)).expect("fresh tab");
        assert!(other.append_tab(dest, detached).is_ok());
        assert!(other.contains_tab(t0));
        assert!(!layout.contains_tab(t0));
        other.validate().unwrap();
        layout.validate().unwrap();
    }

    #[test]
    fn hidden_selected_tab_falls_back_without_mutating_selection() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, t1, t2], _, _) = multi_fixture(&mut layout);
        assert!(layout.select(t1));

        // Hidden selected tab: the fallback is the first visible tab at or
        // after the selected slot...
        let hide_sel = |t: &Tab<u32>| t.id() != t1;
        assert_eq!(layout.effective_selected(multi, hide_sel), Some(t2));
        // ...and the durable selection is untouched.
        assert_eq!(layout.selected(multi), Some(t1));
        // No visible tab after the selected slot: the nearest one before it.
        let hide_tail = |t: &Tab<u32>| t.id() == t0;
        assert_eq!(layout.effective_selected(multi, hide_tail), Some(t0));
        // A visible selected tab is simply itself; full visibility restores
        // the preferred tab with no select() ever issued.
        assert_eq!(layout.effective_selected(multi, |_| true), Some(t1));
        // A fully hidden group has no rendered tab.
        assert_eq!(layout.effective_selected(multi, |_| false), None);
    }

    #[test]
    fn fully_hidden_group_drops_from_the_build() {
        let mut layout = GroupLayout::new();
        let (multi, tabs, _, _) = multi_fixture(&mut layout);
        let hidden: BTreeSet<TabId> = tabs.into_iter().collect();

        // Hiding every tab of the multi group removes its leaf from the
        // build, leaving the singleton full-extent and dividerless.
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, |t| !hidden.contains(&t.id()))
            .expect("the singleton remains");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&layout, &conf, &mut ratios, &mut leaves);
        assert_eq!(leaves, vec![200]);
        assert!(ratios.is_empty());

        // One visible tab keeps the group's leaf in the build.
        let partly = |t: &Tab<u32>| t.id() == tabs[2] || t.binding() == Some(&200);
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, partly)
            .expect("both groups visible");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&layout, &conf, &mut ratios, &mut leaves);
        assert_eq!(leaves.len(), 2);
        assert_eq!(ratios.len(), 1);
        assert_eq!(layout.effective_selected(multi, partly), Some(tabs[2]));
    }

    #[test]
    fn rebuilds_are_deterministic() {
        let mut layout = GroupLayout::new();
        let (_multi, [_, t1, _], single, _) = multi_fixture(&mut layout);
        let (a_main, _) = push(&mut layout, A_MAIN);
        script_split(&mut layout, a_main, A_NOTES);
        assert!(layout.select(t1));
        assert!(layout.merge_tab(t1, single, 0));

        // An unmutated model builds the identical configuration every time,
        // filtered or not.
        let reference = built(&layout);
        for _ in 0..3 {
            assert_eq!(built(&layout), reference);
        }
        let filter = |t: &Tab<u32>| t.binding() != Some(&A_NOTES);
        let flat = |conf: &Configuration<GroupId>| {
            let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
            flatten(&layout, conf, &mut ratios, &mut leaves);
            (ratios, leaves)
        };
        let (first, _) = layout
            .build_filtered(AREA, SPACING, MIN, filter)
            .expect("visible panes remain");
        let (second, _) = layout
            .build_filtered(AREA, SPACING, MIN, filter)
            .expect("visible panes remain");
        assert_eq!(flat(&first), flat(&second));
        assert_eq!(layout.groups_depth_first(), layout.groups_depth_first());
        layout.validate().unwrap();
    }

    #[test]
    fn ops_with_missing_ids_reject_without_mutating() {
        let mut layout = GroupLayout::new();
        let (multi, [t0, _, _], _, _) = multi_fixture(&mut layout);
        let stale_tab = Tab::<u32>::placeholder().id();
        let stale_group = GroupId::mint();
        let before_bindings = bindings(&layout);
        let before_groups = layout.groups_depth_first();

        assert!(!layout.select(stale_tab));
        assert!(!layout.reorder_tab(stale_tab, 0));
        assert!(layout.remove_tab(stale_tab).is_none());
        assert!(layout.detach_tab(stale_tab).is_none());
        assert!(!layout.merge_tab(stale_tab, multi, 0));
        assert!(!layout.merge_tab(t0, stale_group, 0));
        assert!(!layout.swap_tabs(stale_tab, t0));
        assert!(!layout.swap_tabs(t0, stale_tab));
        assert!(!layout.set_binding(stale_tab, Some(1)));
        assert!(layout.insert_tab(stale_group, 0, Tab::bound(500)).is_err());
        assert!(
            layout
                .split_tab_as_singleton(
                    stale_tab,
                    multi,
                    Axis::Vertical,
                    true,
                    SplitSizing::Ratio(0.5)
                )
                .is_none()
        );
        assert!(
            layout
                .split_tab_as_singleton(
                    t0,
                    stale_group,
                    Axis::Vertical,
                    true,
                    SplitSizing::Ratio(0.5)
                )
                .is_none()
        );
        assert!(
            layout
                .split_group(
                    stale_group,
                    Axis::Vertical,
                    true,
                    SplitSizing::Ratio(0.5),
                    Tab::bound(501)
                )
                .is_err()
        );
        assert!(!layout.set_group_px(stale_group, Axis::Vertical, 100.0));

        assert_eq!(bindings(&layout), before_bindings);
        assert_eq!(layout.groups_depth_first(), before_groups);
        layout.validate().unwrap();
    }

    #[test]
    fn placeholder_tabs_participate_until_materialized() {
        let mut layout = GroupLayout::new();
        let (group, t0) = push(&mut layout, 1);
        let placeholder = Tab::placeholder();
        let p = placeholder.id();
        assert!(layout.append_tab(group, placeholder).is_ok());
        layout.validate().unwrap();

        // Unbound tabs select, reorder, and move like any other tab...
        assert!(layout.select(p));
        assert!(layout.reorder_tab(p, 0));
        let gid = layout
            .split_tab_as_singleton(p, group, Axis::Vertical, false, SplitSizing::Ratio(0.5))
            .expect("participants are live");
        assert_eq!(layout.group_of(p), Some(gid));
        // ...are invisible to binding lookups, and materialize in place.
        assert!(!layout.contains_binding(2));
        assert!(layout.set_binding(p, Some(2)));
        assert!(layout.contains_binding(2));
        assert_eq!(bindings(&layout), vec![1, 2]);
        assert_eq!(layout.selected(group), Some(t0));
        layout.validate().unwrap();
    }

    #[test]
    fn rejected_insertions_hand_the_identical_tab_back() {
        // Move-only hosting makes a duplicate *id* unrepresentable to a
        // caller — inserting a tab consumes it — so the reachable duplicate
        // is a fresh tab carrying an already-hosted binding. Every insertion
        // path rejects it and returns the very same tab, model untouched.
        let mut layout = GroupLayout::new();
        let (group, _) = push(&mut layout, 1);
        assert!(layout.append_tab(group, Tab::bound(2)).is_ok());
        let before_bindings = bindings(&layout);
        let before_groups = layout.groups_depth_first();

        let dup = Tab::bound(2);
        let dup_id = dup.id();
        let dup = layout
            .insert_tab(group, 0, dup)
            .expect_err("binding is taken");
        let dup = layout.append_tab(group, dup).expect_err("binding is taken");
        let dup = layout.push_cluster(dup).expect_err("binding is taken");
        let dup = layout
            .insert_cluster_front(dup)
            .expect_err("binding is taken");
        let dup = layout
            .wrap_all(Axis::Vertical, true, dup)
            .expect_err("binding is taken");
        let dup = layout
            .split_group(group, Axis::Vertical, true, SplitSizing::Ratio(0.5), dup)
            .expect_err("binding is taken");
        assert_eq!(dup.id(), dup_id, "the tab rides every rejection intact");
        assert_eq!(dup.binding(), Some(&2));
        assert_eq!(bindings(&layout), before_bindings);
        assert_eq!(layout.groups_depth_first(), before_groups);

        // A binding held elsewhere rejects a rebind onto it.
        let other = Tab::bound(3);
        let other_id = other.id();
        assert!(layout.append_tab(group, other).is_ok());
        assert!(!layout.set_binding(other_id, Some(1)));

        assert_eq!(bindings(&layout), vec![1, 2, 3]);
        layout.validate().unwrap();
    }

    #[test]
    fn minted_ids_are_unique_across_models() {
        // Two models minting concurrently never collide: ids come from one
        // process-wide well.
        let mut a = GroupLayout::new();
        let mut b = GroupLayout::new();
        let mut seen = BTreeSet::new();
        for n in 0..50 {
            let (ga, ta) = push(&mut a, n);
            let (gb, tb) = push(&mut b, n);
            assert!(seen.insert(ga.0));
            assert!(seen.insert(gb.0));
            assert!(seen.insert(ta.0));
            assert!(seen.insert(tb.0));
        }
    }

    // ------------------------------------------------------------------
    // Deterministic operation-sequence fuzz
    // ------------------------------------------------------------------

    /// A 64-bit LCG (Knuth's MMIX constants); the sequence is a pure function
    /// of the seed, so every failure reproduces exactly.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    /// Every divider target in a build mirror, depth-first.
    fn collect_targets(built: &BuiltNode, out: &mut Vec<EdgeTarget>) {
        if let BuiltNode::Split { target, a, b } = built {
            out.push(target.clone());
            collect_targets(a, out);
            collect_targets(b, out);
        }
    }

    /// The divider targets a fresh unfiltered build emits — the mirror a
    /// window holds between rebuilds.
    fn harvest_targets(layout: &GroupLayout<u32>) -> Vec<EdgeTarget> {
        let mut out = Vec::new();
        if let Some((_, mirror)) = layout.build(AREA, SPACING, MIN) {
            collect_targets(&mirror, &mut out);
        }
        out
    }

    /// Drive hundreds of random operations against one model and check the
    /// full invariant set — plus exact tab *and binding* conservation against
    /// shadow sets — after every step. Interleaving is where
    /// duplicate-or-lost-pane bugs live; unit tests alone cannot reach these
    /// orderings.
    ///
    /// Beyond the always-valid ops, the generator mixes in divider drags
    /// through harvested [`EdgeTarget`]s — both fresh and deliberately held
    /// across structural mutations, since a stale target must reject rather
    /// than panic — a stale/missing-id battery, placeholder hosting,
    /// binding-collision rejections, and one seed that biases appends into a
    /// single group until its strip outgrows the inline capacity.
    #[test]
    fn seeded_operation_sequences_preserve_invariants() {
        for (seed, spill_bias) in [
            (1u64, false),
            (42, false),
            (0xDEAD_BEEF, false),
            (0x0123_4567_89AB_CDEF, false),
            (0x5EED_5EED, true),
        ] {
            let mut rng = Lcg(seed);
            let mut layout: GroupLayout<u32> = GroupLayout::new();
            let mut next_binding: u32 = 0;
            let mut expected: BTreeSet<TabId> = BTreeSet::new();
            let mut expected_bindings: BTreeSet<u32> = BTreeSet::new();
            let mut held_targets: Vec<EdgeTarget> = Vec::new();
            let mut max_strip = 0usize;
            let fresh = |expected: &mut BTreeSet<TabId>,
                         expected_bindings: &mut BTreeSet<u32>,
                         next_binding: &mut u32| {
                *next_binding += 1;
                let tab = Tab::bound(*next_binding);
                expected.insert(tab.id());
                expected_bindings.insert(*next_binding);
                tab
            };

            for step in 0..400 {
                let groups = layout.groups_depth_first();
                let tabs: Vec<TabId> = layout.panes().iter().map(|t| t.id()).collect();
                let first_strip = groups
                    .first()
                    .and_then(|g| layout.tabs(*g))
                    .map_or(0, <[_]>::len);
                // The spill seed's fat strip is exempt from the population
                // cap, so the biased appends below are never starved by it.
                let crowded = if spill_bias {
                    tabs.len() - first_strip >= 24
                } else {
                    tabs.len() >= 24
                };
                let pick_group = |rng: &mut Lcg| groups[rng.below(groups.len())];
                let pick_tab = |rng: &mut Lcg| tabs[rng.below(tabs.len())];
                let axis = if rng.chance(50) {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };

                let op = if spill_bias && first_strip < 30 && rng.chance(55) {
                    4
                } else {
                    rng.below(20)
                };
                match op {
                    0 if !crowded => {
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        assert!(layout.push_cluster(tab).is_ok());
                    }
                    1 if !crowded => {
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        assert!(layout.insert_cluster_front(tab).is_ok());
                    }
                    2 if !crowded && !groups.is_empty() => {
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        let gid = pick_group(&mut rng);
                        assert!(
                            layout
                                .split_group(
                                    gid,
                                    axis,
                                    rng.chance(50),
                                    SplitSizing::Ratio(0.5),
                                    tab
                                )
                                .is_ok()
                        );
                    }
                    3 if !crowded => {
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        assert!(layout.wrap_all(axis, rng.chance(50), tab).is_ok());
                    }
                    4 if !crowded && !groups.is_empty() => {
                        // The spill seed funnels appends into the first group
                        // so one strip outgrows its inline capacity.
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        let gid = if spill_bias {
                            groups[0]
                        } else {
                            pick_group(&mut rng)
                        };
                        let selected_before = layout.selected(gid);
                        assert!(layout.append_tab(gid, tab).is_ok());
                        assert_eq!(layout.selected(gid), selected_before, "append: selection");
                    }
                    5 if !crowded && !groups.is_empty() => {
                        let tab = fresh(&mut expected, &mut expected_bindings, &mut next_binding);
                        let gid = pick_group(&mut rng);
                        let index = rng.below(8);
                        let selected_before = layout.selected(gid);
                        assert!(layout.insert_tab(gid, index, tab).is_ok());
                        assert_eq!(layout.selected(gid), selected_before, "insert: selection");
                    }
                    6 if !tabs.is_empty() => {
                        let tab = pick_tab(&mut rng);
                        let taken = layout.remove_tab(tab).expect("hosted tab");
                        expected.remove(&tab);
                        if let Some(b) = taken.binding() {
                            expected_bindings.remove(b);
                        }
                    }
                    7 if !tabs.is_empty() => {
                        // Detached tabs either re-host in a random group or
                        // leave the model for good.
                        let tab = pick_tab(&mut rng);
                        if let Some(detached) = layout.detach_tab(tab) {
                            if rng.chance(50) {
                                let gid = pick_group(&mut rng);
                                assert!(layout.insert_tab(gid, rng.below(8), detached).is_ok());
                            } else {
                                expected.remove(&tab);
                                if let Some(b) = detached.binding() {
                                    expected_bindings.remove(b);
                                }
                            }
                        } else {
                            // Refused: the tab is its group's last.
                            assert_eq!(
                                layout.tabs(layout.group_of(tab).unwrap()).unwrap().len(),
                                1
                            );
                        }
                    }
                    8 if !tabs.is_empty() => {
                        assert!(layout.select(pick_tab(&mut rng)));
                    }
                    9 if !tabs.is_empty() => {
                        let tab = pick_tab(&mut rng);
                        let owner = layout.group_of(tab).expect("hosted tab");
                        let selected_before = layout.selected(owner);
                        assert!(layout.reorder_tab(tab, rng.below(8)));
                        assert_eq!(
                            layout.selected(owner),
                            selected_before,
                            "reorder: selection"
                        );
                    }
                    10 if !tabs.is_empty() && !groups.is_empty() => {
                        let tab = pick_tab(&mut rng);
                        let gid = pick_group(&mut rng);
                        let selected_before = layout.selected(gid);
                        assert!(layout.merge_tab(tab, gid, rng.below(8)));
                        assert_eq!(
                            layout.selected(gid),
                            selected_before,
                            "merge: destination selection"
                        );
                    }
                    11 if !tabs.is_empty() && !groups.is_empty() => {
                        let tab = pick_tab(&mut rng);
                        let gid = pick_group(&mut rng);
                        // Refused only for a singleton beside itself.
                        let sole = layout.group_of(tab) == Some(gid)
                            && layout.tabs(gid).unwrap().len() == 1;
                        let split = layout.split_tab_as_singleton(
                            tab,
                            gid,
                            axis,
                            rng.chance(50),
                            SplitSizing::Ratio(0.5),
                        );
                        assert_eq!(split.is_none(), sole);
                    }
                    12 if !tabs.is_empty() => {
                        let (a, b) = (pick_tab(&mut rng), pick_tab(&mut rng));
                        assert_eq!(layout.swap_tabs(a, b), a != b);
                    }
                    13 if !tabs.is_empty() => {
                        let tab = pick_tab(&mut rng);
                        let old = layout.tab(tab).and_then(|t| t.binding().copied());
                        next_binding += 1;
                        let binding = if rng.chance(80) {
                            Some(next_binding)
                        } else {
                            None
                        };
                        assert!(layout.set_binding(tab, binding));
                        if let Some(old) = old {
                            expected_bindings.remove(&old);
                        }
                        if let Some(new) = binding {
                            expected_bindings.insert(new);
                        }
                    }
                    14 if !groups.is_empty() => {
                        // May legitimately no-op when the group has no edge
                        // on the axis.
                        let gid = pick_group(&mut rng);
                        let _ = layout.set_group_px(gid, axis, 100.0 + rng.below(300) as f32);
                    }
                    15 if !expected.is_empty() => {
                        // Builds are read-only and deterministic, filtered
                        // included; every emitted leaf is a live group.
                        let flat = |conf: &Configuration<GroupId>| {
                            let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
                            flatten_ids(conf, &mut ratios, &mut leaves);
                            (ratios, leaves)
                        };
                        let (conf_a, _) = layout.build(AREA, SPACING, MIN).expect("non-empty");
                        let (conf_b, _) = layout.build(AREA, SPACING, MIN).expect("non-empty");
                        assert_eq!(flat(&conf_a), flat(&conf_b), "seed {seed} step {step}");
                        assert_eq!(flat(&conf_a).1, groups, "seed {seed} step {step}");
                        let mask = rng.next();
                        let filtered = layout.build_filtered(AREA, SPACING, MIN, |t| {
                            (t.id().0.wrapping_mul(0x9E37_79B9_7F4A_7C15) & mask) != 0
                        });
                        if let Some((conf, _)) = filtered {
                            let (_, leaves) = flat(&conf);
                            assert!(!leaves.is_empty());
                            assert!(leaves.iter().all(|g| groups.contains(g)));
                        }
                    }
                    16 if !groups.is_empty() => {
                        // Divider drags through targets harvested from the
                        // current build: every emitted edge accepts a resize.
                        let targets = harvest_targets(&layout);
                        if !targets.is_empty() {
                            let target = &targets[rng.below(targets.len())];
                            let ratio = 0.1 + (rng.below(81) as f32) / 100.0;
                            layout.set_split_ratio(target, ratio);
                        }
                    }
                    17 => {
                        // Stale and missing-id inputs (roughly one op in
                        // twenty): every operation rejects without mutating —
                        // the end-of-step shadow checks pin the no-mutation
                        // half.
                        let stale_tab = Tab::<u32>::placeholder().id();
                        let stale_group = GroupId::mint();
                        assert!(!layout.select(stale_tab));
                        assert!(!layout.reorder_tab(stale_tab, rng.below(8)));
                        assert!(layout.remove_tab(stale_tab).is_none());
                        assert!(layout.detach_tab(stale_tab).is_none());
                        assert!(!layout.set_binding(stale_tab, None));
                        assert!(
                            layout
                                .insert_tab(stale_group, rng.below(8), Tab::placeholder())
                                .is_err()
                        );
                        assert!(
                            layout
                                .split_group(
                                    stale_group,
                                    axis,
                                    true,
                                    SplitSizing::Ratio(0.5),
                                    Tab::placeholder()
                                )
                                .is_err()
                        );
                        assert!(!layout.set_group_px(stale_group, axis, 100.0));
                        // Cluster counts never exceed the tab count, so these
                        // targets are out of range by construction.
                        layout.set_split_ratio(&EdgeTarget::TopLevel(tabs.len() + 1), 0.5);
                        layout.set_split_ratio(
                            &EdgeTarget::Node {
                                cluster: tabs.len() + 1,
                                path: Vec::new(),
                            },
                            0.5,
                        );
                        if !tabs.is_empty() {
                            let live = pick_tab(&mut rng);
                            let live_group = layout.group_of(live).expect("hosted tab");
                            assert!(!layout.merge_tab(stale_tab, live_group, 0));
                            assert!(!layout.merge_tab(live, stale_group, 0));
                            assert!(!layout.swap_tabs(live, stale_tab));
                            assert!(!layout.swap_tabs(stale_tab, live));
                            assert!(
                                layout
                                    .split_tab_as_singleton(
                                        stale_tab,
                                        live_group,
                                        axis,
                                        true,
                                        SplitSizing::Ratio(0.5)
                                    )
                                    .is_none()
                            );
                            assert!(
                                layout
                                    .split_tab_as_singleton(
                                        live,
                                        stale_group,
                                        axis,
                                        true,
                                        SplitSizing::Ratio(0.5)
                                    )
                                    .is_none()
                            );
                        }
                    }
                    18 => {
                        // Placeholder hosting plus binding-collision
                        // rejections: the admissibility check's binding arm
                        // and the rebind guard.
                        if !groups.is_empty() && !crowded && rng.chance(60) {
                            let tab = Tab::placeholder();
                            expected.insert(tab.id());
                            let gid = pick_group(&mut rng);
                            assert!(layout.insert_tab(gid, rng.below(8), tab).is_ok());
                        }
                        let hosted: Vec<(TabId, u32)> = layout
                            .panes()
                            .iter()
                            .filter_map(|t| t.binding().map(|b| (t.id(), *b)))
                            .collect();
                        if !hosted.is_empty() && !groups.is_empty() {
                            let (_, binding) = hosted[rng.below(hosted.len())];
                            let dup = Tab::bound(binding);
                            let dup_id = dup.id();
                            let dup = layout
                                .append_tab(pick_group(&mut rng), dup)
                                .expect_err("hosted binding rejects insertion");
                            assert_eq!(dup.id(), dup_id, "rejected tab rides back");
                        }
                        if hosted.len() >= 2 {
                            let (id_a, _) = hosted[0];
                            let (_, taken) = hosted[hosted.len() - 1];
                            assert!(!layout.set_binding(id_a, Some(taken)));
                        }
                    }
                    19 => {
                        // Drags through the full mirror harvested at an
                        // *earlier* build — the split map a window caches
                        // between rebuilds. Structural mutations since the
                        // harvest can leave any of these stale, and a stale
                        // address must reject rather than panic.
                        for target in &held_targets {
                            let ratio = 0.1 + (rng.below(81) as f32) / 100.0;
                            layout.set_split_ratio(target, ratio);
                        }
                        held_targets = harvest_targets(&layout);
                    }
                    _ => {}
                }

                layout
                    .validate()
                    .unwrap_or_else(|err| panic!("seed {seed} step {step}: {err}"));
                let now: BTreeSet<TabId> = layout.panes().iter().map(|t| t.id()).collect();
                assert_eq!(now, expected, "tab conservation, seed {seed} step {step}");
                let mut live_bindings: Vec<u32> = layout
                    .panes()
                    .iter()
                    .filter_map(|t| t.binding().copied())
                    .collect();
                live_bindings.sort_unstable();
                let expected_sorted: Vec<u32> = expected_bindings.iter().copied().collect();
                assert_eq!(
                    live_bindings, expected_sorted,
                    "binding conservation, seed {seed} step {step}"
                );
                assert_eq!(
                    layout.is_empty(),
                    expected.is_empty(),
                    "seed {seed} step {step}"
                );
                max_strip = layout
                    .groups_depth_first()
                    .iter()
                    .filter_map(|g| layout.tabs(*g))
                    .map(|strip| strip.len())
                    .fold(max_strip, usize::max);
            }
            assert!(
                !layout.is_empty(),
                "seed {seed} produced a degenerate all-empty run"
            );
            if spill_bias {
                assert!(
                    max_strip > TAB_INLINE,
                    "seed {seed} never spilled a strip past its inline capacity ({max_strip})"
                );
            }
        }
    }
}
