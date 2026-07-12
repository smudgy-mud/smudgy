//! The declarative per-window pane layout model (flexible-panes plan §2.12).
//!
//! A window's layout is an ordered list of **clusters** — one session's main
//! pane plus the script panes split within it — each holding a split tree.
//! The `pane_grid::State` the window renders is **derived** from this model
//! via `State::with_configuration` after every structural mutation, rather
//! than mutated imperatively. That makes automatic placement deterministic:
//! a new session always divides the window against the existing clusters at
//! the top level, and a script pane always splits within its own cluster —
//! regardless of the order sessions and their scripts arrive in.
//!
//! Sizing: a script's `width`/`height` request is stored as **pixels** and
//! resolved to a divider ratio at every build, against the extent the
//! reference region has *in that build* — the piece that makes the two
//! creation orders converge (a ratio resolved once at creation would bake in
//! the transient pre-`B` extent). A user divider drag converts the edge to a
//! user-owned ratio, which rebuilds then carry verbatim.
//!
//! The model is payload-generic (`T` = the grid slot type) so the unit tests
//! below can drive it with plain integers.

use std::collections::BTreeMap;

use iced::Size;
use iced::widget::pane_grid::{self, Axis, Configuration};

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

/// One subtree of a cluster: a pane slot, or a split of two subtrees.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode<T> {
    Leaf(T),
    Split {
        axis: Axis,
        sizing: SplitSizing,
        a: Box<LayoutNode<T>>,
        b: Box<LayoutNode<T>>,
    },
}

/// One top-level cluster: a session's subtree plus its share of the window
/// width (weights are relative — shares renormalize as clusters come and go).
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster<T> {
    weight: f32,
    root: LayoutNode<T>,
}

/// A branch selector into a [`LayoutNode::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    A,
    B,
}

/// Which model edge a built divider corresponds to — the target of a user
/// resize. `TopLevel(i)` is the divider between cluster `i` and the fold of
/// the clusters after it; `Node` addresses a split inside one cluster.
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

impl<T: Copy + PartialEq> LayoutNode<T> {
    fn contains(&self, slot: T) -> bool {
        match self {
            Self::Leaf(t) => *t == slot,
            Self::Split { a, b, .. } => a.contains(slot) || b.contains(slot),
        }
    }

    fn collect_into(&self, out: &mut Vec<T>) {
        match self {
            Self::Leaf(t) => out.push(*t),
            Self::Split { a, b, .. } => {
                a.collect_into(out);
                b.collect_into(out);
            }
        }
    }

    /// Replace the leaf `reference` with a split of it and `new`.
    fn split_leaf(
        &mut self,
        reference: T,
        axis: Axis,
        new_first: bool,
        sizing: SplitSizing,
        new: T,
    ) -> bool {
        match self {
            Self::Leaf(t) if *t == reference => {
                let old = Self::Leaf(*t);
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

    /// Remove the leaf `slot`, collapsing its parent split into the sibling.
    /// `Ok(true)` = removed; `Ok(false)` = not found; `Err(())` = this node
    /// *was* the leaf (the caller drops the whole node).
    fn remove(&mut self, slot: T) -> Result<bool, ()> {
        match self {
            Self::Leaf(t) => {
                if *t == slot {
                    Err(())
                } else {
                    Ok(false)
                }
            }
            Self::Split { a, b, .. } => {
                match a.remove(slot) {
                    Err(()) => {
                        *self = std::mem::replace(b.as_mut(), Self::Leaf(slot));
                        return Ok(true);
                    }
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                }
                match b.remove(slot) {
                    Err(()) => {
                        *self = std::mem::replace(a.as_mut(), Self::Leaf(slot));
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
    fn any_visible(&self, visible: &impl Fn(T) -> bool) -> bool {
        match self {
            Self::Leaf(t) => visible(*t),
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
        visible: &impl Fn(T) -> bool,
    ) -> Option<(Configuration<T>, BuiltNode)> {
        match self {
            Self::Leaf(t) => visible(*t).then_some((Configuration::Pane(*t), BuiltNode::Leaf)),
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

/// The whole window's layout model. Empty ⇔ the window renders the empty
/// connect state (`grid = None`; a `pane_grid::State` cannot be empty).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowLayout<T> {
    clusters: Vec<Cluster<T>>,
}

impl<T: Copy + PartialEq> WindowLayout<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            clusters: Vec::new(),
        }
    }

    #[must_use]
    pub fn contains(&self, slot: T) -> bool {
        self.clusters.iter().any(|c| c.root.contains(slot))
    }

    /// Every pane slot in the model, cluster by cluster, depth-first.
    #[must_use]
    pub fn panes(&self) -> Vec<T> {
        let mut out = Vec::new();
        for cluster in &self.clusters {
            cluster.root.collect_into(&mut out);
        }
        out
    }

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

    /// Append a new top-level cluster (the new-session placement rule).
    pub fn push_cluster(&mut self, slot: T) {
        let weight = self.even_share_weight();
        self.clusters.push(Cluster {
            weight,
            root: LayoutNode::Leaf(slot),
        });
    }

    /// Insert a new top-level cluster on the left edge (whole-grid Left drop).
    pub fn insert_cluster_front(&mut self, slot: T) {
        let weight = self.even_share_weight();
        self.clusters.insert(
            0,
            Cluster {
                weight,
                root: LayoutNode::Leaf(slot),
            },
        );
    }

    /// Split `new` off the `reference` leaf, wherever it lives (a script pane
    /// stays within its owning cluster because its reference does). Returns
    /// `false` (model unchanged) when `reference` is not in the model.
    pub fn split_leaf(
        &mut self,
        reference: T,
        axis: Axis,
        new_first: bool,
        sizing: SplitSizing,
        new: T,
    ) -> bool {
        self.clusters
            .iter_mut()
            .any(|c| c.root.split_leaf(reference, axis, new_first, sizing, new))
    }

    /// Remove `slot`'s leaf, collapsing its parent split; a cluster whose
    /// last pane leaves is dropped. Returns whether the slot was found.
    pub fn remove(&mut self, slot: T) -> bool {
        for (idx, cluster) in self.clusters.iter_mut().enumerate() {
            match cluster.root.remove(slot) {
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

    /// The model address of `slot`'s leaf.
    fn find_path(&self, slot: T) -> Option<(usize, Vec<Branch>)> {
        for (idx, cluster) in self.clusters.iter().enumerate() {
            let mut path = Vec::new();
            if find_leaf_path(&cluster.root, slot, &mut path) {
                return Some((idx, path));
            }
        }
        None
    }

    /// Swap the payloads of two leaves (the drop-on-center gesture). Resolved
    /// by address first, so the momentary duplicate payload a naive rewrite
    /// would create can never swap the wrong leaf back.
    pub fn swap(&mut self, x: T, y: T) {
        if x == y {
            return;
        }
        let (Some(at_x), Some(at_y)) = (self.find_path(x), self.find_path(y)) else {
            return;
        };
        for ((cluster, path), value) in [(at_x, y), (at_y, x)] {
            if let Some(LayoutNode::Leaf(t)) = self
                .clusters
                .get_mut(cluster)
                .and_then(|c| c.root.node_at_mut(&path))
            {
                *t = value;
            }
        }
    }

    /// Fold the whole current layout into a single cluster and split the
    /// dragged pane against it (whole-grid Top/Bottom edge drops).
    pub fn wrap_all(&mut self, axis: Axis, new_first: bool, slot: T) {
        let Some(combined) = self.combined_node() else {
            self.push_cluster(slot);
            return;
        };
        let (a, b) = if new_first {
            (LayoutNode::Leaf(slot), combined)
        } else {
            (combined, LayoutNode::Leaf(slot))
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
    }

    /// The current clusters folded into one node (right-associated, weights
    /// becoming ratios) — the shape `build` emits for the top level.
    fn combined_node(&self) -> Option<LayoutNode<T>> {
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

    /// Apply a user divider drag to the model. A top-level edge re-weights
    /// the clusters (preserving relative proportions within the remainder);
    /// an in-cluster edge becomes a user-owned `Ratio` (a px sizing is
    /// consumed by the drag).
    pub fn set_split_ratio(&mut self, target: &EdgeTarget, ratio: f32) {
        match target {
            EdgeTarget::TopLevel(i) => {
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

    /// Derive the `pane_grid` configuration (plus its structural mirror, for
    /// mapping real `Split` ids back to model edges). `area` is the grid's
    /// current on-screen size — px sizings resolve against it; when unknown
    /// (zero, pre-first-layout) they fall back to even splits until the next
    /// rebuild. `None` when the model is empty.
    #[must_use]
    pub fn build(
        &self,
        area: Size,
        spacing: f32,
        min_size: f32,
    ) -> Option<(Configuration<T>, BuiltNode)> {
        self.build_filtered(area, spacing, min_size, |_| true)
    }

    /// [`Self::build`], restricted to the slots passing `visible` (the
    /// hidden-panes state): a hidden leaf drops out, a split with one hidden
    /// side gives the other side the whole region, and a fully hidden cluster
    /// gives up its share of the window. The mirror's `EdgeTarget`s always
    /// address the unfiltered model, so a divider drag on a filtered grid
    /// writes through to the right edge. (A top-level drag still reweights
    /// against the full cluster list, so that divider can shift once hidden
    /// clusters are shown again.) `None` when no slot passes.
    #[must_use]
    pub fn build_filtered(
        &self,
        area: Size,
        spacing: f32,
        min_size: f32,
        visible: impl Fn(T) -> bool,
    ) -> Option<(Configuration<T>, BuiltNode)> {
        let vis: Vec<usize> = (0..self.clusters.len())
            .filter(|&i| self.clusters[i].root.any_visible(&visible))
            .collect();
        let total: f32 = vis.iter().map(|&i| self.clusters[i].weight).sum();
        self.build_group(&vis, area, total, spacing, min_size, &visible)
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
        visible: &impl Fn(T) -> bool,
    ) -> Option<(Configuration<T>, BuiltNode)> {
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

/// Record the branch path to `slot`'s leaf into `path` (left as the prefix
/// walked so far on a miss — callers pass a fresh buffer per cluster).
fn find_leaf_path<T: Copy + PartialEq>(
    node: &LayoutNode<T>,
    slot: T,
    path: &mut Vec<Branch>,
) -> bool {
    match node {
        LayoutNode::Leaf(t) => *t == slot,
        LayoutNode::Split { a, b, .. } => {
            path.push(Branch::A);
            if find_leaf_path(a, slot, path) {
                return true;
            }
            path.pop();
            path.push(Branch::B);
            if find_leaf_path(b, slot, path) {
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

    const SPACING: f32 = 4.0;
    const MIN: f32 = 50.0;
    const AREA: Size = Size::new(1600.0, 900.0);

    /// Flatten a configuration to (depth-first ratios, leaf payloads) for
    /// equality checks — `Configuration` itself has no `PartialEq`.
    fn flatten(conf: &Configuration<u32>, ratios: &mut Vec<f32>, leaves: &mut Vec<u32>) {
        match conf {
            Configuration::Pane(t) => leaves.push(*t),
            Configuration::Split { ratio, a, b, .. } => {
                ratios.push(*ratio);
                flatten(a, ratios, leaves);
                flatten(b, ratios, leaves);
            }
        }
    }

    fn built(layout: &WindowLayout<u32>) -> (Vec<f32>, Vec<u32>) {
        let (conf, _) = layout.build(AREA, SPACING, MIN).expect("non-empty");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&conf, &mut ratios, &mut leaves);
        (ratios, leaves)
    }

    const A_MAIN: u32 = 1;
    const A_NOTES: u32 = 2;
    const B_MAIN: u32 = 11;
    const B_NOTES: u32 = 12;

    /// A session's script splitting a 200px notes pane off its main pane.
    fn script_split(layout: &mut WindowLayout<u32>, main: u32, notes: u32) {
        assert!(layout.split_leaf(
            main,
            Axis::Vertical,
            false,
            SplitSizing::Px {
                px: 200.0,
                sized_first: false,
            },
            notes,
        ));
    }

    /// The request-4 scenario: session B created before vs after session A's
    /// script fires must converge on the same layout — 50/50 clusters, each
    /// with a 200px notes pane.
    #[test]
    fn cluster_placement_is_order_independent() {
        // Order 1: A's script fires first, then B opens (the "wedged" order).
        let mut first = WindowLayout::new();
        first.push_cluster(A_MAIN);
        script_split(&mut first, A_MAIN, A_NOTES);
        first.push_cluster(B_MAIN);
        script_split(&mut first, B_MAIN, B_NOTES);

        // Order 2: both sessions open, then both scripts fire.
        let mut second = WindowLayout::new();
        second.push_cluster(A_MAIN);
        second.push_cluster(B_MAIN);
        script_split(&mut second, A_MAIN, A_NOTES);
        script_split(&mut second, B_MAIN, B_NOTES);

        assert_eq!(first, second);
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
        let mut layout = WindowLayout::new();
        layout.push_cluster(1);
        layout.push_cluster(2);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.5).abs() < 1e-6);
        layout.push_cluster(3);
        let (ratios, _) = built(&layout);
        // Three even clusters: first divider 1/3, second 1/2.
        assert!((ratios[0] - 1.0 / 3.0).abs() < 1e-6, "{ratios:?}");
        assert!((ratios[1] - 0.5).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn user_resize_converts_px_to_owned_ratio_and_reweights_top_level() {
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        layout.push_cluster(B_MAIN);

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
        let (conf, _) = layout
            .build(Size::new(800.0, 600.0), SPACING, MIN)
            .unwrap();
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&conf, &mut ratios, &mut leaves);
        assert!((ratios[1] - 0.7).abs() < 1e-6, "{ratios:?}");

        // Drag the top-level divider to 0.25/0.75.
        layout.set_split_ratio(&EdgeTarget::TopLevel(0), 0.25);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.25).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn remove_collapses_splits_and_drops_empty_clusters() {
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        layout.push_cluster(B_MAIN);

        assert!(layout.remove(A_NOTES));
        assert!(!layout.contains(A_NOTES));
        assert_eq!(layout.panes(), vec![A_MAIN, B_MAIN]);

        assert!(layout.remove(A_MAIN));
        assert_eq!(layout.panes(), vec![B_MAIN]);
        assert!(layout.remove(B_MAIN));
        assert!(layout.panes().is_empty());
        assert!(layout.build(AREA, SPACING, MIN).is_none());
        assert!(!layout.remove(B_MAIN));
    }

    #[test]
    fn swap_exchanges_two_leaves() {
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        layout.push_cluster(B_MAIN);
        layout.swap(A_NOTES, B_MAIN);
        assert_eq!(layout.panes(), vec![A_MAIN, B_MAIN, A_NOTES]);
        // Swapping with itself or a missing slot is a no-op.
        layout.swap(A_MAIN, A_MAIN);
        layout.swap(A_MAIN, 99);
        assert_eq!(layout.panes(), vec![A_MAIN, B_MAIN, A_NOTES]);
    }

    #[test]
    fn wrap_all_and_edge_inserts() {
        let mut layout = WindowLayout::new();
        layout.push_cluster(1);
        layout.push_cluster(2);
        layout.insert_cluster_front(3);
        assert_eq!(layout.panes(), vec![3, 1, 2]);

        layout.wrap_all(Axis::Horizontal, true, 4);
        assert_eq!(layout.panes(), vec![4, 3, 1, 2]);
        let (ratios, _) = built(&layout);
        assert!((ratios[0] - 0.5).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn split_targets_map_every_grid_divider() {
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        layout.push_cluster(B_MAIN);

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
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        layout.push_cluster(B_MAIN);
        script_split(&mut layout, B_MAIN, B_NOTES);

        // Hiding one leaf collapses its split: the sibling takes the whole
        // cluster region and the split emits no divider.
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, |t| t != A_NOTES)
            .expect("visible panes remain");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&conf, &mut ratios, &mut leaves);
        assert_eq!(leaves, vec![A_MAIN, B_MAIN, B_NOTES]);
        assert_eq!(ratios.len(), 2, "top-level + B's notes divider only");

        // Hiding a whole cluster removes it from the top-level fold.
        let (conf, _) = layout
            .build_filtered(AREA, SPACING, MIN, |t| t == A_MAIN || t == A_NOTES)
            .expect("cluster A remains");
        let (mut ratios, mut leaves) = (Vec::new(), Vec::new());
        flatten(&conf, &mut ratios, &mut leaves);
        assert_eq!(leaves, vec![A_MAIN, A_NOTES]);
        assert_eq!(ratios.len(), 1, "no top-level divider for one cluster");

        // Nothing visible: nothing to build. The model itself is untouched.
        assert!(layout.build_filtered(AREA, SPACING, MIN, |_| false).is_none());
        let (_, leaves) = built(&layout);
        assert_eq!(leaves, vec![A_MAIN, A_NOTES, B_MAIN, B_NOTES]);
    }

    #[test]
    fn filtered_split_targets_address_the_unfiltered_model() {
        // main | (notes | chat), with notes hidden: the one emitted divider
        // separates main from chat but must map to the *outer* model edge, so
        // dragging it writes through to the real model.
        const A_CHAT: u32 = 3;
        let mut layout = WindowLayout::new();
        layout.push_cluster(A_MAIN);
        script_split(&mut layout, A_MAIN, A_NOTES);
        assert!(layout.split_leaf(
            A_NOTES,
            Axis::Horizontal,
            false,
            SplitSizing::Ratio(0.5),
            A_CHAT,
        ));

        let (conf, mirror) = layout
            .build_filtered(AREA, SPACING, MIN, |t| t != A_NOTES)
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
}
