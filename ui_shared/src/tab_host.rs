#![allow(clippy::pedantic)]

//! The body host for one pane-group slot: every tab's subtree stays mounted
//! while only the selected one is laid out fresh, drawn, and given events.
//!
//! # Why mounting matters
//!
//! iced widget state survives by *positional* reconciliation — there is no
//! id-based resurrection. Keeping inactive tabs' subtrees in the tree is what
//! preserves their ephemeral state (terminal scroll position, text selection,
//! scrollable offsets) across tab switches. Children are keyed by stable
//! [`TabId`]: the host snapshots the key order in its widget state, and each
//! diff first permutes the retained state subtrees into the incoming key
//! order — fresh state for arriving keys, dropped state for departed ones —
//! then runs the pairwise diff. A reorder therefore moves each subtree with
//! its tab instead of re-pairing it with whatever now sits at its old
//! position.
//!
//! # Layout caching
//!
//! Only the selected child is laid out on each pass. Inactive children reuse
//! their last layout, recomputed when they become selected or when the host's
//! limits change. Besides skipping per-tab O(visible lines) terminal layout
//! on every pass, this silences terminal layout's side effect — character
//! grid-size reporting — for tabs that aren't on screen. An inactive tab's
//! measured size therefore stays at its last rendered value until it is
//! selected again, matching the hidden-pane size contract.
//!
//! A cached layout can trail its subtree's *shape*: inactive tabs keep
//! receiving runtime updates, so a rebuild may reshape their widget trees
//! while the cache holds the old geometry. Every diff therefore marks all
//! cache entries stale, and any traversal that must pair live widgets with
//! layout nodes — operations, see below — re-lays stale children against
//! their cached limits first. Rendering never needs this: only the selected
//! child draws, and its layout is fresh by construction.
//!
//! # Event and operation routing
//!
//! Events, drawing, cursor interaction, and overlays reach the selected child
//! only: an obscured subtree can neither paint nor capture the pointer, and
//! it cannot participate in cursor-driven focus. Operations, by contrast,
//! traverse *all* children: targeted operations (focus-by-id, scroll-to)
//! still reach inactive subtrees, and a `focus(id)` operation unfocuses every
//! other focusable it visits — including a previously selected tab's input —
//! so switching tabs cannot strand a phantom focus in an obscured subtree.
//! That traversal serves each child a layout matching its live shape,
//! refreshing stale cache entries on demand.

use iced::advanced::widget::{Operation, tree};
use iced::advanced::{Widget, layout, overlay};
use iced::{Element, Event, Length, Rectangle, Size, Vector, mouse};
use rustc_hash::FxHashMap;

use crate::pane_groups::TabId;

/// The state-repair plan for one keyed child diff: for each new position,
/// the old position whose key matches — `None` for a key with no prior
/// state. Every old position is claimed at most once, so duplicate keys
/// degrade to positional pairing among themselves rather than aliasing one
/// subtree twice; old positions left unclaimed are dropped by the caller.
fn pairing_plan<K: PartialEq>(old: &[K], new: &[K]) -> Vec<Option<usize>> {
    let mut claimed = vec![false; old.len()];
    new.iter()
        .map(|key| {
            let matched = (0..old.len()).find(|&index| !claimed[index] && old[index] == *key);
            if let Some(index) = matched {
                claimed[index] = true;
            }
            matched
        })
        .collect()
}

/// One cached child layout, valid for the limits it was computed under.
struct CachedLayout {
    node: layout::Node,
    min: Size,
    max: Size,
    /// Whether a diff has touched the live subtree since `node` was computed.
    /// A stale node still renders faithfully — it is the subtree's last
    /// on-screen geometry — but it may disagree with the subtree's current
    /// *shape*, so it must never be paired with the live widgets in an
    /// operation traversal: iced containers unwrap their layout children.
    stale: bool,
}

/// Host widget state: the key list driving keyed child diffing, plus the
/// per-tab layout cache.
struct State {
    keys: Vec<TabId>,
    cache: FxHashMap<TabId, CachedLayout>,
}

/// The tab-group body host. `keys`, `children`, and `selected` are parallel:
/// `children[selected]` is the subtree that renders.
pub struct TabHost<'a, Message, Theme, Renderer> {
    keys: Vec<TabId>,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    selected: usize,
}

impl<'a, Message, Theme, Renderer> TabHost<'a, Message, Theme, Renderer> {
    /// A host over `children` keyed by `keys`, rendering `children[selected]`.
    /// The two vectors must be the same length; `selected` is clamped.
    pub fn new(
        keys: Vec<TabId>,
        children: Vec<Element<'a, Message, Theme, Renderer>>,
        selected: usize,
    ) -> Self {
        debug_assert_eq!(keys.len(), children.len());
        let selected = selected.min(children.len().saturating_sub(1));
        Self {
            keys,
            children,
            selected,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TabHost<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        // Diagnostic sibling of the tab press surface's fresh-subtree
        // tripwire: a fresh host after startup means the pane's whole
        // content subtree was rebuilt (every member's ephemeral state was
        // just lost).
        log::info!(
            "[pane-drag] tab host state created for keys {:?} (fresh subtree)",
            self.keys
        );
        tree::State::new(State {
            keys: self.keys.clone(),
            cache: FxHashMap::default(),
        })
    }

    fn children(&self) -> Vec<tree::Tree> {
        self.children.iter().map(tree::Tree::new).collect()
    }

    fn diff(&self, tree: &mut tree::Tree) {
        let tree::Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();

        // Key-driven re-pairing: the retained state subtrees are permuted
        // into the incoming key order before the pairwise diff, so a child
        // whose key moved carries its own state to its new position instead
        // of inheriting its neighbor's. Arriving keys get fresh state;
        // subtrees whose keys departed are dropped.
        let plan = pairing_plan(&state.keys, &self.keys);
        let mut retained: Vec<Option<tree::Tree>> =
            std::mem::take(children).into_iter().map(Some).collect();
        *children = plan
            .into_iter()
            .zip(&self.children)
            .map(|(source, child)| {
                source
                    .and_then(|index| retained.get_mut(index).and_then(Option::take))
                    .unwrap_or_else(|| tree::Tree::new(child.as_widget()))
            })
            .collect();
        for (child, child_tree) in self.children.iter().zip(children.iter_mut()) {
            child_tree.diff(child.as_widget());
        }

        if state.keys != self.keys {
            state.keys.clone_from(&self.keys);
        }

        // Every cached layout now trails the freshly diffed subtrees: the
        // diff may have reshaped any child. Layout passes recompute the
        // selected child (and any limits-invalidated one) immediately;
        // inactive children keep rendering their stale geometry and are
        // re-laid on demand before an operation traversal descends into them.
        for cached in state.cache.values_mut() {
            cached.stale = true;
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let tree::Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();
        let (min, max) = (limits.min(), limits.max());

        let mut nodes = Vec::with_capacity(self.children.len());
        for (index, (child, child_tree)) in self
            .children
            .iter_mut()
            .zip(children.iter_mut())
            .enumerate()
        {
            let Some(&key) = self.keys.get(index) else {
                nodes.push(layout::Node::new(Size::ZERO));
                continue;
            };
            let cached_valid = state
                .cache
                .get(&key)
                .is_some_and(|cached| cached.min == min && cached.max == max);
            if index == self.selected || !cached_valid {
                let node = child.as_widget_mut().layout(child_tree, renderer, limits);
                state.cache.insert(
                    key,
                    CachedLayout {
                        node: node.clone(),
                        min,
                        max,
                        stale: false,
                    },
                );
                nodes.push(node);
            } else if let Some(cached) = state.cache.get(&key) {
                nodes.push(cached.node.clone());
            }
        }
        // Departed tabs release their cached layout.
        state.cache.retain(|key, _| self.keys.contains(key));

        layout::Node::with_children(max, nodes)
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let tree::Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();

        // A traversal must pair each live subtree with a layout of the SAME
        // shape: iced containers unwrap their layout children, so descending
        // into a reshaped subtree with a frozen node panics. Any child whose
        // cache went stale under a diff is re-laid here against the limits
        // its cache was computed for. The selected child is always clean —
        // the layout pass that produced `layout` re-laid it after the same
        // diff. Re-laying an inactive terminal is O(visible lines) and
        // operations are rare, so refreshing on demand is cheap.
        for (index, (child, child_tree)) in self
            .children
            .iter_mut()
            .zip(children.iter_mut())
            .enumerate()
        {
            let Some(cached) = self
                .keys
                .get(index)
                .and_then(|key| state.cache.get_mut(key))
            else {
                continue;
            };
            if !cached.stale {
                continue;
            }
            let limits = layout::Limits::new(cached.min, cached.max);
            cached.node = child.as_widget_mut().layout(child_tree, renderer, &limits);
            cached.stale = false;
        }

        // All children, not just the selected one: targeted operations and
        // blanket unfocus must reach mounted-but-inactive subtrees. Children
        // are visited against their cached nodes — refreshed above where a
        // diff had outdated them — rather than `layout`'s frozen copies,
        // offset exactly as `layout.children()` would place them.
        let offset = {
            let position = layout.position();
            Vector::new(position.x, position.y)
        };
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for (index, (child, child_tree)) in self
                .children
                .iter_mut()
                .zip(children.iter_mut())
                .enumerate()
            {
                let Some(cached) = self.keys.get(index).and_then(|key| state.cache.get(key)) else {
                    // No layout has ever reached this child, so there is no
                    // geometry of any shape to traverse against. Skipping is
                    // recoverable (at worst a missed unfocus on a subtree
                    // that has never been shown); a mispaired traversal is a
                    // panic.
                    log::info!(
                        "[pane-drag] tab host skipping operation for unlaid child at {index}"
                    );
                    continue;
                };
                child.as_widget_mut().operate(
                    child_tree,
                    iced::advanced::Layout::with_offset(offset, &cached.node),
                    renderer,
                    operation,
                );
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &Event,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // Selected child only: an obscured subtree can neither capture the
        // pointer nor react to keyboard events. Inactive panes still receive
        // their runtime/buffer updates through session state, which does not
        // flow through widget events.
        let (Some(child), Some(child_tree), Some(child_layout)) = (
            self.children.get_mut(self.selected),
            tree.children.get_mut(self.selected),
            layout.children().nth(self.selected),
        ) else {
            return;
        };
        child.as_widget_mut().update(
            child_tree,
            event,
            child_layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // Selected child only, so the stale-cache hazard operations face
        // cannot arise here (nor in `mouse_interaction`/`overlay` below): a
        // subtree's shape only changes at diff time, and every layout pass —
        // which runs after the diff and before any draw — recomputes the
        // selected child, so `layout`'s node for it always matches its live
        // shape.
        let (Some(child), Some(child_tree), Some(child_layout)) = (
            self.children.get(self.selected),
            tree.children.get(self.selected),
            layout.children().nth(self.selected),
        ) else {
            return;
        };
        child.as_widget().draw(
            child_tree,
            renderer,
            theme,
            style,
            child_layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let (Some(child), Some(child_tree), Some(child_layout)) = (
            self.children.get(self.selected),
            tree.children.get(self.selected),
            layout.children().nth(self.selected),
        ) else {
            return mouse::Interaction::None;
        };
        child
            .as_widget()
            .mouse_interaction(child_tree, child_layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = self.children.get_mut(self.selected)?;
        let child_tree = tree.children.get_mut(self.selected)?;
        let child_layout = layout.children().nth(self.selected)?;
        child
            .as_widget_mut()
            .overlay(child_tree, child_layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<TabHost<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(host: TabHost<'a, Message, Theme, Renderer>) -> Self {
        Element::new(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_groups::Tab;

    use iced::Rectangle;
    use iced::advanced::Layout;
    use iced::advanced::widget::{Id, Operation};

    type Theme = iced::Theme;
    type TestRenderer = ();
    type TestElement = Element<'static, (), Theme, TestRenderer>;

    fn mint_keys(count: usize) -> Vec<TabId> {
        (0..count).map(|_| Tab::<()>::placeholder().id()).collect()
    }

    fn space() -> TestElement {
        iced::widget::Space::new().width(10).height(10).into()
    }

    fn boxed(content: TestElement) -> TestElement {
        iced::widget::container(content).into()
    }

    fn column_of(children: Vec<TestElement>) -> TestElement {
        iced::widget::Column::with_children(children).into()
    }

    /// Counts every `container` visit; `traverse` always descends, so the
    /// count measures how deep an operation traversal actually reached.
    #[derive(Default)]
    struct ContainerCount {
        count: usize,
    }

    impl Operation for ContainerCount {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn container(&mut self, _id: Option<&Id>, _bounds: Rectangle) {
            self.count += 1;
        }
    }

    /// Regression: an inactive tab's subtree deepens across a rebuild
    /// while its cached layout (same limits) is reused, and an operation
    /// traversal pairs the live widgets with the frozen node — inside iced's
    /// `Container::operate`, `layout.children().next().unwrap()` panics. The
    /// host must serve such children a fresh layout instead.
    #[test]
    fn operations_survive_a_reshaped_inactive_subtree() {
        let keys = mint_keys(2);
        let limits = layout::Limits::new(Size::ZERO, Size::new(800.0, 600.0));

        let mut host = TabHost::new(
            keys.clone(),
            vec![boxed(space()), column_of(vec![boxed(space())])],
            0,
        );
        let mut tree = tree::Tree::new(&host as &dyn Widget<(), Theme, TestRenderer>);
        let _ = host.layout(&mut tree, &(), &limits);

        // Rebuild: the inactive tab gains a nesting level (column → container
        // → container) while the selected tab keeps the focus of layout.
        let mut host = TabHost::new(
            keys.clone(),
            vec![boxed(space()), column_of(vec![boxed(boxed(space()))])],
            0,
        );
        host.diff(&mut tree);
        let node = host.layout(&mut tree, &(), &limits);

        let mut operation = ContainerCount::default();
        host.operate(&mut tree, Layout::new(&node), &(), &mut operation);

        // Host + selected container + the reshaped tab's column, outer, and
        // inner containers: the traversal reached every level of the NEW
        // subtree without panicking.
        assert_eq!(operation.count, 5);
    }

    /// The selected child must never be served from the cache: its layout is
    /// recomputed on every pass even when the cached entry's limits still
    /// match.
    #[test]
    fn selected_child_lays_fresh_despite_a_limits_valid_cache() {
        let keys = mint_keys(2);
        let limits = layout::Limits::new(Size::ZERO, Size::new(800.0, 600.0));

        let mut host = TabHost::new(
            keys.clone(),
            vec![column_of(vec![boxed(space())]), boxed(space())],
            0,
        );
        let mut tree = tree::Tree::new(&host as &dyn Widget<(), Theme, TestRenderer>);
        let node = host.layout(&mut tree, &(), &limits);
        assert_eq!(node.children()[0].children().len(), 1);

        let mut host = TabHost::new(
            keys.clone(),
            vec![
                column_of(vec![boxed(space()), boxed(space())]),
                boxed(space()),
            ],
            0,
        );
        host.diff(&mut tree);
        let node = host.layout(&mut tree, &(), &limits);
        assert_eq!(node.children()[0].children().len(), 2);
    }

    /// Departed tabs release their cached layouts on the next layout pass.
    #[test]
    fn departed_tabs_release_their_cached_layouts() {
        let keys = mint_keys(2);
        let limits = layout::Limits::new(Size::ZERO, Size::new(800.0, 600.0));

        let mut host = TabHost::new(keys.clone(), vec![boxed(space()), boxed(space())], 0);
        let mut tree = tree::Tree::new(&host as &dyn Widget<(), Theme, TestRenderer>);
        let _ = host.layout(&mut tree, &(), &limits);
        assert_eq!(
            tree.state.downcast_ref::<State>().cache.len(),
            2,
            "both tabs cache a layout after the first pass"
        );

        let mut host = TabHost::new(vec![keys[0]], vec![boxed(space())], 0);
        host.diff(&mut tree);
        let _ = host.layout(&mut tree, &(), &limits);

        let state = tree.state.downcast_ref::<State>();
        assert_eq!(state.cache.len(), 1);
        assert!(state.cache.contains_key(&keys[0]));
    }

    /// A child with no cached layout at all (no layout pass has reached it)
    /// is skipped by the operation traversal rather than traversed against
    /// geometry that does not exist.
    #[test]
    fn operate_skips_children_that_have_never_been_laid_out() {
        let keys = mint_keys(2);
        let mut host = TabHost::new(keys, vec![boxed(space()), boxed(space())], 0);
        let mut tree = tree::Tree::new(&host as &dyn Widget<(), Theme, TestRenderer>);

        let node = layout::Node::new(Size::new(800.0, 600.0));
        let mut operation = ContainerCount::default();
        host.operate(&mut tree, Layout::new(&node), &(), &mut operation);

        // Only the host itself reports; both unlaid children are skipped.
        assert_eq!(operation.count, 1);
    }

    #[test]
    fn identity_pairs_every_position_with_itself() {
        assert_eq!(
            pairing_plan(&['a', 'b', 'c'], &['a', 'b', 'c']),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!(pairing_plan::<char>(&[], &[]), Vec::<Option<usize>>::new());
    }

    #[test]
    fn rotation_carries_each_state_to_its_new_position() {
        assert_eq!(
            pairing_plan(&['a', 'b', 'c'], &['c', 'a', 'b']),
            vec![Some(2), Some(0), Some(1)]
        );
    }

    #[test]
    fn swap_exchanges_exactly_the_two_positions() {
        assert_eq!(
            pairing_plan(&['a', 'b'], &['b', 'a']),
            vec![Some(1), Some(0)]
        );
        assert_eq!(
            pairing_plan(&['a', 'b', 'c'], &['c', 'b', 'a']),
            vec![Some(2), Some(1), Some(0)]
        );
    }

    #[test]
    fn insert_gets_fresh_state_without_disturbing_neighbors() {
        assert_eq!(
            pairing_plan(&['a', 'c'], &['a', 'b', 'c']),
            vec![Some(0), None, Some(1)]
        );
        assert_eq!(pairing_plan(&[], &['a']), vec![None]);
    }

    #[test]
    fn remove_drops_only_the_departed_key() {
        assert_eq!(
            pairing_plan(&['a', 'b', 'c'], &['a', 'c']),
            vec![Some(0), Some(2)]
        );
        assert_eq!(pairing_plan(&['a'], &[]), Vec::<Option<usize>>::new());
    }

    #[test]
    fn mixed_reorder_insert_remove_matches_by_key() {
        assert_eq!(
            pairing_plan(&['a', 'b', 'c', 'd'], &['d', 'b', 'e']),
            vec![Some(3), Some(1), None]
        );
    }

    #[test]
    fn duplicate_keys_pair_positionally_among_themselves() {
        assert_eq!(
            pairing_plan(&['a', 'a'], &['a', 'a']),
            vec![Some(0), Some(1)]
        );
        assert_eq!(pairing_plan(&['a', 'a'], &['a']), vec![Some(0)]);
        assert_eq!(pairing_plan(&['a'], &['a', 'a']), vec![Some(0), None]);
    }
}
