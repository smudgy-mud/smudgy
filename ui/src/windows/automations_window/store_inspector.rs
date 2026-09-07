//! The session-store inspector pane (`docs/interop.md` §10): the live store tree
//! per producer (with its budget usage) and the interop catalogue — every declared/observed
//! state key, event, and message, with provenance, declared + inferred payload shapes, and
//! the recent-sample ring. Data is a [`CatalogueSnapshot`] streamed from the session runtime
//! while this pane is open; rendering is pure (no queries — the snapshot is the whole view).
//! The producer trees arrive as shared `Node` roots (the committed store's own `Arc`-interior
//! tree, never a copy), and rendering walks them lazily: a collapsed node costs a length
//! read, and only expanded nodes' children are visited — the collapsed/paginated view is the
//! data model, so a massive published tree costs this pane what the visible slice costs.

use std::collections::HashSet;

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, row, text};
use iced::{Font, Length, Padding};
use smudgy_cloud::Node;
use smudgy_core::session::runtime::catalogue::{
    CatalogueEntryView, CatalogueSample, CatalogueSnapshot,
};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::builtins::button as button_style;

use super::editors::pane_scroll;
use super::{AutomationsWindow, Elem, Message, common};

/// Children rendered under one expanded object/array node before eliding the rest — the
/// budgets allow subtrees far larger than a usable page (interop.md §10: no silent caps, so the
/// elision row says how many were dropped).
const NODE_CHILD_CAP: usize = 200;

/// The Store tab's default: nodes at depth 0–1 expanded, deeper nodes collapsed. A toggle
/// inverts the default for that node. Other trees choose their own ([`TreeRows`]).
const DEFAULT_EXPAND_DEPTH: usize = 2;

/// The label of one store-tree row.
pub(super) enum RowLabel<'a> {
    /// A producer root or an array element: no key of its own, rendered as a bullet.
    Unkeyed,
    /// An object child, keyed by its first-published spelling.
    Key(&'a str),
    /// A caller-built head for a root row that stands in for its producer's heading.
    Head(Elem<'a>),
}

/// How one caller renders a store tree: the expansion set it reads, the message that flips a
/// node in it, and the trailing control it appends to addressable rows. The store pane reads
/// the tree plain; the editors' state browser hangs an expose toggle on every row.
pub(super) struct TreeRows<'a> {
    /// Nodes whose expansion the user flipped, keyed producer + NUL + path; membership inverts
    /// the depth default.
    pub toggled: &'a HashSet<String>,
    /// Nodes shallower than this start expanded; `0` starts every node collapsed.
    pub default_expand_depth: usize,
    /// Flips one node's expansion by that key.
    pub on_toggle: fn(String) -> Message,
    /// The trailing control for the row at `path` (the segments beneath the producer root),
    /// `None` for a plain row. Never asked beneath an array: arrays are addressed whole, so
    /// their elements have no path.
    #[allow(clippy::type_complexity)]
    pub trailing: Option<Box<dyn Fn(&[String]) -> Option<Elem<'a>> + 'a>>,
}

/// Samples shown per catalogue entry (newest first); the ring retains more, but the pane
/// stays scannable.
const SAMPLES_SHOWN: usize = 5;

const MONO: Font = fonts::GEIST_MONO_VF;

impl AutomationsWindow {
    pub(super) fn view_store_inspector(&self) -> Elem<'_> {
        let header = column![
            text(crate::i18n::t!("store-title")).size(30.0).font(Font {
                weight: iced::font::Weight::Light,
                ..fonts::GEIST_VF
            }),
            text(crate::i18n::t!("store-description"))
                .size(13.0)
                .style(common::muted),
            iced::widget::rule::horizontal(1.0),
        ]
        .spacing(10.0);

        let mut body = column![header].spacing(24.0);
        match &self.catalogue {
            None => {
                body = body.push(
                    text(crate::i18n::t!("store-waiting"))
                        .size(13.0)
                        .style(common::muted),
                );
            }
            Some(snapshot) => {
                body = body.push(self.view_store_trees(snapshot));
                body = body.push(self.view_catalogue_entries(snapshot));
            }
        }
        pane_scroll(body)
    }

    /// The committed store tree per producer, collapsible per node.
    fn view_store_trees<'a>(&'a self, snapshot: &'a CatalogueSnapshot) -> Elem<'a> {
        let mut section = column![common::section_label(crate::i18n::ts!(
            "store-published-state"
        ))]
        .spacing(8.0);
        if snapshot.producers.is_empty() {
            return section
                .push(
                    text(crate::i18n::t!("store-empty"))
                        .size(13.0)
                        .style(common::muted),
                )
                .into();
        }
        for producer in &snapshot.producers {
            let usage = crate::i18n::t!(
                "store-usage",
                "entries" => producer.entries,
                "bytes" => format_bytes(producer.bytes)
            );
            section = section.push(
                row![
                    text(producer.producer.clone()).size(14.0).font(MONO),
                    iced::widget::space::horizontal(),
                    text(usage).size(12.0).style(common::faint),
                ]
                .align_y(Vertical::Center)
                .spacing(8.0),
            );
            let tree = TreeRows {
                toggled: &self.store_toggled,
                default_expand_depth: DEFAULT_EXPAND_DEPTH,
                on_toggle: Message::ToggleStoreNode,
                trailing: None,
            };
            let rows = json_rows(
                &tree,
                Column::new().spacing(2.0),
                &producer.producer,
                String::new(),
                RowLabel::Unkeyed,
                &producer.tree,
                0,
                &mut Vec::new(),
                true,
            );
            section = section.push(rows);
        }
        section.into()
    }

    /// The interop catalogue: one block per entry, grouped under producer sub-headers.
    fn view_catalogue_entries<'a>(&'a self, snapshot: &'a CatalogueSnapshot) -> Elem<'a> {
        let mut section =
            column![common::section_label(crate::i18n::ts!("store-catalogue"))].spacing(10.0);
        if snapshot.entries.is_empty() {
            return section
                .push(
                    text(crate::i18n::t!("store-catalogue-empty"))
                        .size(13.0)
                        .style(common::muted),
                )
                .into();
        }
        let mut current_producer: Option<&str> = None;
        for entry in &snapshot.entries {
            if current_producer != Some(&*entry.producer) {
                current_producer = Some(&*entry.producer);
                section = section.push(
                    iced::widget::container(
                        text(entry.producer.to_string())
                            .size(13.0)
                            .font(MONO)
                            .style(common::muted),
                    )
                    .padding(Padding {
                        top: 8.0,
                        ..Padding::ZERO
                    }),
                );
            }
            section = section.push(entry_block(entry));
        }
        section.into()
    }
}

/// Append the rows for one store node (and, when expanded, its children) to `rows`. `key`
/// identifies the node for expansion toggling; `path` is the node's address beneath the
/// producer root, maintained across the recursion, and `addressable` is false beneath an array
/// (no path spells an element). Lazy by construction: a collapsed container contributes one
/// head row from its length alone; children are only iterated (and only up to
/// [`NODE_CHILD_CAP`]) when expanded.
#[allow(clippy::too_many_arguments)]
pub(super) fn json_rows<'a>(
    tree: &TreeRows<'a>,
    mut rows: Column<'a, Message, crate::theme::Theme>,
    producer: &str,
    key: String,
    label: RowLabel<'a>,
    node: &'a Node,
    depth: usize,
    path: &mut Vec<String>,
    addressable: bool,
) -> Column<'a, Message, crate::theme::Theme> {
    let indent = Padding {
        left: 14.0 * depth as f32,
        ..Padding::ZERO
    };
    let trailing = if addressable {
        tree.trailing.as_ref().and_then(|build| build(path))
    } else {
        None
    };
    // Containers render a toggleable head; scalars render one leaf row. Arrays are
    // addressed whole (no index grammar), but their elements still render.
    let (child_count, summary) = match node {
        Node::Object(object) => (Some(object.len()), format!("{{{}}}", object.len())),
        Node::Array(array) => (
            Some(array.items().len()),
            format!("[{}]", array.items().len()),
        ),
        _ => (None, String::new()),
    };
    match child_count {
        None => {
            let body: Elem<'a> = match label {
                RowLabel::Key(label) => text(format!("{label}: {node}"))
                    .size(12.0)
                    .font(MONO)
                    .into(),
                RowLabel::Unkeyed => text(node.to_string()).size(12.0).font(MONO).into(),
                RowLabel::Head(head) => row![head, text(node.to_string()).size(12.0).font(MONO)]
                    .spacing(6.0)
                    .align_y(Vertical::Center)
                    .into(),
            };
            rows = rows.push(
                iced::widget::container(with_trailing(body, trailing)).padding(Padding {
                    left: indent.left + 17.0,
                    ..Padding::ZERO
                }),
            );
        }
        Some(count) => {
            let node_key = format!("{producer}\u{0}{key}");
            let expanded = (depth < tree.default_expand_depth) != tree.toggled.contains(&node_key);
            let chevron = if expanded {
                bootstrap_icons::CHEVRON_DOWN
            } else {
                bootstrap_icons::CHEVRON_RIGHT
            };
            let label: Elem<'a> = match label {
                RowLabel::Key(label) => text(label).size(12.0).font(MONO).into(),
                RowLabel::Unkeyed => text("\u{2022}").size(12.0).style(common::faint).into(),
                RowLabel::Head(head) => head,
            };
            let head = row![
                text(chevron)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(10.0)
                    .style(common::faint),
                label,
                text(summary).size(11.0).style(common::faint),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center);
            let body: Elem<'a> = button(head)
                .style(button_style::list_item)
                .on_press((tree.on_toggle)(node_key))
                .padding(Padding {
                    top: 1.0,
                    bottom: 1.0,
                    left: 2.0,
                    right: 2.0,
                })
                .into();
            rows =
                rows.push(iced::widget::container(with_trailing(body, trailing)).padding(indent));
            if expanded {
                let children: Box<dyn Iterator<Item = (&'a str, &'a Node)>> = match node {
                    Node::Object(object) => Box::new(object.iter()),
                    Node::Array(array) => Box::new(array.items().iter().map(|item| ("", item))),
                    _ => unreachable!("only containers report a child count"),
                };
                for (index, (child_label, child)) in children.enumerate() {
                    if index >= NODE_CHILD_CAP {
                        rows = rows.push(
                            iced::widget::container(
                                text(crate::i18n::t!(
                                    "store-more-hidden",
                                    "count" => count - NODE_CHILD_CAP
                                ))
                                .size(12.0)
                                .style(common::faint),
                            )
                            .padding(Padding {
                                left: 14.0 * (depth + 1) as f32 + 17.0,
                                ..Padding::ZERO
                            }),
                        );
                        break;
                    }
                    // An element (or an empty key, which no path can spell) is unkeyed and
                    // leaves the address alone; a keyed child extends it for its subtree.
                    let (child_key, label) = if child_label.is_empty() {
                        (format!("{key}/[{index}]"), RowLabel::Unkeyed)
                    } else {
                        path.push(child_label.to_string());
                        (format!("{key}/{child_label}"), RowLabel::Key(child_label))
                    };
                    let keyed = !child_label.is_empty();
                    rows = json_rows(
                        tree,
                        rows,
                        producer,
                        child_key,
                        label,
                        child,
                        depth + 1,
                        path,
                        addressable && keyed,
                    );
                    if keyed {
                        path.pop();
                    }
                }
            }
        }
    }
    rows
}

/// A row body with its trailing control after it, or the body alone.
fn with_trailing<'a>(body: Elem<'a>, trailing: Option<Elem<'a>>) -> Elem<'a> {
    match trailing {
        None => body,
        Some(trailing) => row![body, trailing]
            .spacing(8.0)
            .align_y(Vertical::Center)
            .into(),
    }
}

/// One catalogue entry: identity row, shapes, and the recent samples.
fn entry_block(entry: &CatalogueEntryView) -> Elem<'_> {
    let mut head = row![
        common::badge(entry.kind.as_str()),
        text(entry.name.to_string()).size(13.0).font(MONO),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    let provenance = match (entry.declared, entry.runtime_confirmed) {
        (true, true) => crate::i18n::ts!("store-provenance-declared"),
        (true, false) => crate::i18n::ts!("store-provenance-declared-unseen"),
        (false, true) => crate::i18n::ts!("store-provenance-runtime"),
        (false, false) => crate::i18n::ts!("store-provenance-undeclared"),
    };
    head = head.push(text(provenance).size(11.0).style(common::faint));
    if let Some(alias) = &entry.type_alias {
        head = head.push(
            text(alias.to_string())
                .size(11.0)
                .font(MONO)
                .style(common::faint),
        );
    }
    if entry.occurrences > 0 {
        head = head.push(iced::widget::space::horizontal());
        head = head.push(
            text(format!("\u{00D7}{}", entry.occurrences))
                .size(11.0)
                .style(common::muted),
        );
    }

    let mut block = column![head].spacing(4.0);
    if let Some(shape) = &entry.inferred_shape {
        block = block.push(
            text(crate::i18n::t!("store-shape", "shape" => shape.to_string()))
                .size(11.0)
                .font(MONO)
                .style(common::muted),
        );
    }
    if let Some(declared) = &entry.declared_shape {
        block = block.push(
            text(crate::i18n::t!(
                "store-declared-shape",
                "shape" => first_lines(declared, 6)
            ))
            .size(11.0)
            .font(MONO)
            .style(common::faint),
        );
    }
    for sample in entry.samples.iter().rev().take(SAMPLES_SHOWN) {
        block = block.push(sample_row(sample.as_ref()));
    }

    iced::widget::container(block)
        .style(common::card_style)
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 10.0,
            right: 10.0,
        })
        .width(Length::Fill)
        .into()
}

fn sample_row(sample: &CatalogueSample) -> Elem<'_> {
    let mut meta = format!("{} \u{00B7} {}", ago(sample.at_epoch_ms), sample.sender);
    if sample.truncated {
        meta.push_str(" \u{00B7} ");
        meta.push_str(&crate::i18n::t!("store-truncated"));
    }
    row![
        text(meta)
            .size(11.0)
            .style(common::faint)
            .width(Length::Fixed(190.0)),
        text(sample.display.clone()).size(11.0).font(MONO),
    ]
    .spacing(8.0)
    .into()
}

/// A compact relative timestamp ("just now", "42s ago", "3m ago", "2h ago").
fn ago(at_epoch_ms: u64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let elapsed_s = now_ms.saturating_sub(at_epoch_ms) / 1000;
    match elapsed_s {
        0..=2 => crate::i18n::t!("time-just-now"),
        3..=99 => crate::i18n::t!("time-seconds-ago", "count" => elapsed_s),
        100..=5999 => {
            crate::i18n::t!("time-minutes-ago", "count" => elapsed_s / 60)
        }
        _ => crate::i18n::t!("time-hours-ago", "count" => elapsed_s / 3600),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// The first `n` lines of a declared-shape source, eliding the rest.
fn first_lines(source: &str, n: usize) -> String {
    let mut lines = source.lines();
    let shown: Vec<&str> = lines.by_ref().take(n).collect();
    if lines.next().is_some() {
        format!("{} \u{2026}", shown.join("\n"))
    } else {
        shown.join("\n")
    }
}
