//! Building the durable DTO from the live layout model.
//!
//! The walk is generic over the layout's binding type and reads the model
//! only through its public mirror ([`GroupLayout::structure`]) and content
//! queries, with a caller-supplied `describe` closure translating each tab
//! into durable terms (slot id + pane identity + hidden state). A tab the
//! closure cannot describe — an unbound placeholder with no pending
//! descriptor, or a binding whose session is mid-teardown — simply drops
//! out, collapsing its group and splits exactly like the sanitizer, so a
//! snapshot can never carry a pane it cannot name.

use std::hash::Hash;

use iced::widget::pane_grid::Axis;

use crate::pane_groups::{GroupLayout, SplitSizing, StructureNode, Tab};
use smudgy_session_model::workspace as dto;

/// What a named-layout footprint capture had to leave out. A layout stores
/// only loaded slots, so retained-but-unoccupied geometry cannot enter it;
/// the counts let the capture's caller say so instead of saving silently
/// less than what is on screen.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CaptureNotes {
    /// Vacancy placeholder tabs in captured windows — closed sessions'
    /// retained geometry, which the next open would adopt but no layout
    /// can carry.
    pub omitted_vacancies: usize,
    /// Panes in captured windows whose owning session's main pane lives
    /// outside the captured footprint — they could not round-trip (their
    /// slot has no main to restore through), so they are left out.
    pub omitted_foreign: usize,
}

impl CaptureNotes {
    /// Whether the capture is less than what the captured windows showed.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.omitted_vacancies > 0 || self.omitted_foreign > 0
    }
}

/// A tab translated into durable terms by the caller's `describe` closure.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneRecord {
    /// The owning session's stable slot id.
    pub slot: u64,
    pub identity: dto::PaneIdentity,
    /// The user's eyeball-hidden state for this pane.
    pub hidden: bool,
}

/// Serialize a window's split forest. Returns the DTO clusters in model
/// order; a cluster none of whose tabs could be described disappears, so
/// the result can be empty.
pub fn clusters<T, F>(layout: &GroupLayout<T>, describe: &mut F) -> Vec<dto::Cluster>
where
    T: Copy + Eq + Hash,
    F: FnMut(&Tab<T>) -> Option<PaneRecord>,
{
    layout
        .structure()
        .into_iter()
        .filter_map(|(weight, root)| {
            node(layout, &root, describe).map(|root| dto::Cluster { weight, root })
        })
        .collect()
}

/// Serialize one subtree; `None` when nothing in it could be described. A
/// split with one dead side collapses into the other, mirroring the live
/// model's leaf-removal collapse.
fn node<T, F>(
    layout: &GroupLayout<T>,
    structure: &StructureNode,
    describe: &mut F,
) -> Option<dto::Node>
where
    T: Copy + Eq + Hash,
    F: FnMut(&Tab<T>) -> Option<PaneRecord>,
{
    match structure {
        StructureNode::Leaf(gid) => group(layout, *gid, describe).map(dto::Node::Group),
        StructureNode::Split { axis, sizing, a, b } => {
            let a = node(layout, a, describe);
            let b = node(layout, b, describe);
            match (a, b) {
                (Some(a), Some(b)) => Some(dto::Node::Split(dto::Split {
                    axis: axis_dto(*axis),
                    sizing: sizing_dto(*sizing),
                    a: Box::new(a),
                    b: Box::new(b),
                })),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

/// Serialize one group: its describable tabs in strip order, with the
/// durable selection following its tab into the kept list (first tab when
/// the selected one could not be described).
fn group<T, F>(
    layout: &GroupLayout<T>,
    gid: crate::pane_groups::GroupId,
    describe: &mut F,
) -> Option<dto::Group>
where
    T: Copy + Eq + Hash,
    F: FnMut(&Tab<T>) -> Option<PaneRecord>,
{
    let tabs = layout.tabs(gid)?;
    let selected_id = layout.selected(gid);
    let mut kept = Vec::with_capacity(tabs.len());
    let mut selected = None;
    for tab in tabs {
        let Some(record) = describe(tab) else {
            continue;
        };
        if selected_id == Some(tab.id()) {
            selected = Some(kept.len());
        }
        kept.push(dto::Pane {
            slot: record.slot,
            id: record.identity,
            hidden: record.hidden,
        });
    }
    if kept.is_empty() {
        return None;
    }
    Some(dto::Group {
        tabs: kept,
        selected: selected.unwrap_or(0),
    })
}

fn axis_dto(axis: Axis) -> dto::Axis {
    match axis {
        Axis::Horizontal => dto::Axis::Horizontal,
        Axis::Vertical => dto::Axis::Vertical,
    }
}

fn sizing_dto(sizing: SplitSizing) -> dto::Sizing {
    match sizing {
        SplitSizing::Ratio(ratio) => dto::Sizing::Ratio(ratio),
        SplitSizing::Px { px, sized_first } => dto::Sizing::Px { px, sized_first },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Describe test bindings (plain integers): slot = binding / 100,
    /// identity = main for x00 bindings, else a script pane named after the
    /// binding; bindings of 9xx are undescribable (their session is gone).
    fn describe(tab: &Tab<u32>) -> Option<PaneRecord> {
        let binding = *tab.binding()?;
        if binding >= 900 {
            return None;
        }
        let slot = u64::from(binding / 100);
        let identity = if binding % 100 == 0 {
            dto::PaneIdentity::Main
        } else {
            dto::PaneIdentity::Script {
                namespace: dto::Namespace::User,
                name: format!("pane{binding}"),
                display: None,
            }
        };
        Some(PaneRecord {
            slot,
            identity,
            hidden: binding % 2 == 1,
        })
    }

    #[test]
    fn a_full_forest_serializes_shape_sizing_and_selection() {
        let mut layout = GroupLayout::new();
        let main = layout.push_cluster(Tab::bound(100)).expect("fresh");
        let notes = layout
            .split_group(
                main,
                Axis::Vertical,
                false,
                SplitSizing::Px {
                    px: 240.0,
                    sized_first: false,
                },
                Tab::bound(101),
            )
            .expect("split");
        let chat = Tab::bound(102);
        let chat_id = chat.id();
        layout.append_tab(notes, chat).expect("append");
        assert!(layout.select(chat_id));
        let _second = layout.push_cluster(Tab::bound(200)).expect("fresh");

        let clusters = clusters(&layout, &mut |tab| describe(tab));
        assert_eq!(clusters.len(), 2);
        let dto::Node::Split(split) = &clusters[0].root else {
            panic!("expected the first cluster to be a split");
        };
        assert_eq!(split.axis, dto::Axis::Vertical);
        assert_eq!(
            split.sizing,
            dto::Sizing::Px {
                px: 240.0,
                sized_first: false
            }
        );
        let dto::Node::Group(main_group) = split.a.as_ref() else {
            panic!("main side is a group");
        };
        assert_eq!(main_group.tabs[0].id, dto::PaneIdentity::Main);
        assert!(!main_group.tabs[0].hidden);
        let dto::Node::Group(notes_group) = split.b.as_ref() else {
            panic!("notes side is a group");
        };
        assert_eq!(notes_group.tabs.len(), 2);
        assert_eq!(notes_group.selected, 1, "durable selection follows its tab");
        assert!(
            notes_group.tabs[0].hidden,
            "odd bindings are eyeball-hidden"
        );
        assert_eq!(notes_group.tabs[0].slot, 1);
    }

    #[test]
    fn undescribable_tabs_drop_and_collapse_like_the_sanitizer() {
        let mut layout = GroupLayout::new();
        let main = layout.push_cluster(Tab::bound(100)).expect("fresh");
        // A group that will lose every member (dead session).
        let dead = layout
            .split_group(
                main,
                Axis::Horizontal,
                false,
                SplitSizing::Ratio(0.3),
                Tab::bound(900),
            )
            .expect("split");
        layout.append_tab(dead, Tab::bound(901)).expect("append");
        // A placeholder with no pending descriptor drops out too.
        layout.append_tab(main, Tab::placeholder()).expect("append");

        let clusters = clusters(&layout, &mut |tab| describe(tab));
        assert_eq!(clusters.len(), 1);
        // The split collapsed into the surviving group; the placeholder is
        // simply absent from the strip.
        let dto::Node::Group(group) = &clusters[0].root else {
            panic!("split must collapse into the surviving side");
        };
        assert_eq!(group.tabs.len(), 1);
        assert_eq!(group.tabs[0].id, dto::PaneIdentity::Main);
    }

    #[test]
    fn a_dropped_selection_falls_back_to_the_first_kept_tab() {
        let mut layout = GroupLayout::new();
        let group_id = layout.push_cluster(Tab::bound(101)).expect("fresh");
        let dying = Tab::bound(900);
        let dying_id = dying.id();
        layout.append_tab(group_id, dying).expect("append");
        assert!(layout.select(dying_id));

        let clusters = clusters(&layout, &mut |tab| describe(tab));
        let dto::Node::Group(group) = &clusters[0].root else {
            panic!("group expected");
        };
        assert_eq!(group.tabs.len(), 1);
        assert_eq!(group.selected, 0);
    }

    #[test]
    fn an_empty_model_serializes_to_no_clusters() {
        let layout: GroupLayout<u32> = GroupLayout::new();
        assert!(clusters(&layout, &mut |tab| describe(tab)).is_empty());
    }
}
