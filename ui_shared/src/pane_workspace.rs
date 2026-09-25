//! Platform-neutral mutations for a single-window pane workspace.
//!
//! Native keeps the cross-window transaction coordinator, while both native
//! and browser hosts use these operations for the shape changes inside one
//! window. This keeps drag semantics in the same crate as the durable pane
//! model and target classifier instead of duplicating them in each shell.

use std::hash::Hash;

use iced::widget::pane_grid;

use crate::{
    pane_drag::{DragAction, DropRegion, GridEdgeSide},
    pane_groups::{GroupId, GroupLayout, SplitSizing, TabId},
};

/// The kind of successful local workspace mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEffect {
    /// Only the order inside an existing tab strip changed.
    Reordered { group: GroupId },
    /// The pane-grid shape or the tabs occupying its slots changed.
    LayoutChanged,
}

/// Apply one already-classified drag action to a single window's layout.
///
/// `swap_partner` is the tab currently rendered in a center-swap target. It
/// is supplied by the host because effective selection incorporates
/// host-owned visibility state. `Vacant` is a cross-window destination and
/// is intentionally rejected here.
pub fn apply_local_drop<T: Copy + Eq + Hash>(
    layout: &mut GroupLayout<T>,
    dragged: TabId,
    action: DragAction,
    swap_partner: Option<TabId>,
) -> Option<DropEffect> {
    match action {
        DragAction::Merge { group, slot } => {
            let owner = layout.group_of(dragged)?;
            layout
                .merge_tab(dragged, group, slot)
                .then_some(if owner == group {
                    DropEffect::Reordered { group }
                } else {
                    DropEffect::LayoutChanged
                })
        }
        DragAction::Swap { group } => {
            let partner = swap_partner.filter(|partner| *partner != dragged)?;
            (layout.group_of(partner) == Some(group) && layout.swap_tabs(dragged, partner))
                .then_some(DropEffect::LayoutChanged)
        }
        DragAction::Split { group, region } => {
            let (axis, new_first) = split_axis(region)?;
            layout.split_tab_as_singleton(
                dragged,
                group,
                axis,
                new_first,
                SplitSizing::Ratio(0.5),
            )?;
            Some(DropEffect::LayoutChanged)
        }
        DragAction::GridEdge(side) => {
            let tab = layout.remove_tab(dragged)?;
            let result = match side {
                GridEdgeSide::Left => layout.insert_cluster_front(tab),
                GridEdgeSide::Right => layout.push_cluster(tab),
                GridEdgeSide::Top => layout.wrap_all(pane_grid::Axis::Horizontal, true, tab),
                GridEdgeSide::Bottom => layout.wrap_all(pane_grid::Axis::Horizontal, false, tab),
            };
            match result {
                Ok(_) => Some(DropEffect::LayoutChanged),
                Err(tab) => {
                    // A tab just detached from this layout is admissible. If
                    // an invariant changes, keep the pane reachable anyway.
                    let _ = layout.push_cluster(tab);
                    None
                }
            }
        }
        DragAction::Vacant => None,
    }
}

fn split_axis(region: DropRegion) -> Option<(pane_grid::Axis, bool)> {
    match region {
        DropRegion::Left => Some((pane_grid::Axis::Vertical, true)),
        DropRegion::Right => Some((pane_grid::Axis::Vertical, false)),
        DropRegion::Top => Some((pane_grid::Axis::Horizontal, true)),
        DropRegion::Bottom => Some((pane_grid::Axis::Horizontal, false)),
        DropRegion::Center => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_groups::Tab;

    #[test]
    fn merge_and_split_use_one_shared_mutation_path() {
        let mut layout = GroupLayout::new();
        let a = Tab::bound(1_u8);
        let a_id = a.id();
        let first = layout.push_cluster(a).expect("first cluster");
        let b = Tab::bound(2_u8);
        let b_id = b.id();
        let second = layout.push_cluster(b).expect("second cluster");

        assert_eq!(
            apply_local_drop(
                &mut layout,
                b_id,
                DragAction::Merge {
                    group: first,
                    slot: 1,
                },
                None,
            ),
            Some(DropEffect::LayoutChanged)
        );
        assert_eq!(layout.group_of(b_id), Some(first));

        assert_eq!(
            apply_local_drop(
                &mut layout,
                b_id,
                DragAction::Split {
                    group: first,
                    region: DropRegion::Right,
                },
                None,
            ),
            Some(DropEffect::LayoutChanged)
        );
        assert_ne!(layout.group_of(a_id), layout.group_of(b_id));
        assert!(!layout.contains_group(second));
    }
}
