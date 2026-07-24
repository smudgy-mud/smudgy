//! The session-store inspector pane (`docs/interop.md` §10): the live store tree
//! per producer (with its budget usage) and the interop catalogue — every declared/observed
//! state key, event, and message, with provenance, declared + inferred payload shapes, and
//! the recent-sample ring. Data is a [`CatalogueSnapshot`] streamed from the session runtime
//! while this pane is open; rendering is pure (no queries — the snapshot is the whole view).
//! The producer trees arrive as shared `Node` roots (the committed store's own `Arc`-interior
//! tree, never a copy), and rendering walks them lazily: a collapsed node costs a length
//! read, and only expanded nodes' children are visited — the collapsed/paginated view is the
//! data model, so a massive published tree costs this pane what the visible slice costs.

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

/// Nodes at depth 0–1 default to expanded; deeper nodes default to collapsed. A toggle
/// inverts the default for that node.
const DEFAULT_EXPAND_DEPTH: usize = 2;

/// Samples shown per catalogue entry (newest first); the ring retains more, but the pane
/// stays scannable.
const SAMPLES_SHOWN: usize = 5;

const MONO: Font = fonts::GEIST_MONO_VF;

impl AutomationsWindow {
    pub(super) fn view_store_inspector(&self) -> Elem<'_> {
        let header = column![
            text("Session Store").size(30.0).font(Font {
                weight: iced::font::Weight::Light,
                ..fonts::GEIST_VF
            }),
            text("A live view of state updates, events, and messages published in this session")
                .size(13.0)
                .style(common::muted),
            iced::widget::rule::horizontal(1.0),
        ]
        .spacing(10.0);

        let mut body = column![header].spacing(24.0);
        match &self.catalogue {
            None => {
                body = body.push(
                    text("Waiting for the session's first snapshot\u{2026}")
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
        let mut section = column![common::section_label("Published state")].spacing(8.0);
        if snapshot.producers.is_empty() {
            return section
                .push(
                    text("Nothing is published yet. State appears here the moment a script or package sets it.")
                        .size(13.0)
                        .style(common::muted),
                )
                .into();
        }
        for producer in &snapshot.producers {
            let usage = format!(
                "{} entries \u{00B7} {}",
                producer.entries,
                format_bytes(producer.bytes)
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
            let mut rows = Column::new().spacing(2.0);
            rows = self.json_rows(
                rows,
                &producer.producer,
                String::new(),
                None,
                &producer.tree,
                0,
            );
            section = section.push(rows);
        }
        section.into()
    }

    /// Append the rows for one store node (and, when expanded, its children) to `rows`.
    /// `key` identifies the node for expansion toggling; `label` is `None` at a producer root.
    /// Lazy by construction: a collapsed container contributes one head row from its length
    /// alone; children are only iterated (and only up to [`NODE_CHILD_CAP`]) when expanded.
    fn json_rows<'a>(
        &'a self,
        mut rows: Column<'a, Message, crate::theme::Theme>,
        producer: &str,
        key: String,
        label: Option<&'a str>,
        node: &'a Node,
        depth: usize,
    ) -> Column<'a, Message, crate::theme::Theme> {
        let indent = Padding {
            left: 14.0 * depth as f32,
            ..Padding::ZERO
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
                let line = match label {
                    Some(label) => format!("{label}: {node}"),
                    None => node.to_string(),
                };
                rows = rows.push(
                    iced::widget::container(text(line).size(12.0).font(MONO)).padding(Padding {
                        left: indent.left + 17.0,
                        ..Padding::ZERO
                    }),
                );
            }
            Some(count) => {
                let node_key = format!("{producer}\u{0}{key}");
                let expanded =
                    (depth < DEFAULT_EXPAND_DEPTH) != self.store_toggled.contains(&node_key);
                let chevron = if expanded {
                    bootstrap_icons::CHEVRON_DOWN
                } else {
                    bootstrap_icons::CHEVRON_RIGHT
                };
                let head = row![
                    text(chevron)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(10.0)
                        .style(common::faint),
                    match label {
                        Some(label) => text(label).size(12.0).font(MONO),
                        None => text("\u{2022}").size(12.0).style(common::faint),
                    },
                    text(summary).size(11.0).style(common::faint),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center);
                rows = rows.push(
                    iced::widget::container(
                        button(head)
                            .style(button_style::list_item)
                            .on_press(Message::ToggleStoreNode(node_key))
                            .padding(Padding {
                                top: 1.0,
                                bottom: 1.0,
                                left: 2.0,
                                right: 2.0,
                            }),
                    )
                    .padding(indent),
                );
                if expanded {
                    let children: Box<dyn Iterator<Item = (&str, &Node)>> = match node {
                        Node::Object(object) => Box::new(object.iter()),
                        Node::Array(array) => Box::new(array.items().iter().map(|item| ("", item))),
                        _ => unreachable!("only containers report a child count"),
                    };
                    for (index, (child_label, child)) in children.enumerate() {
                        if index >= NODE_CHILD_CAP {
                            rows = rows.push(
                                iced::widget::container(
                                    text(format!(
                                        "\u{2026} {} more not shown",
                                        count - NODE_CHILD_CAP
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
                        let child_key = if child_label.is_empty() {
                            format!("{key}/[{index}]")
                        } else {
                            format!("{key}/{child_label}")
                        };
                        let label = if child_label.is_empty() {
                            None
                        } else {
                            Some(child_label)
                        };
                        rows = self.json_rows(rows, producer, child_key, label, child, depth + 1);
                    }
                }
            }
        }
        rows
    }

    /// The interop catalogue: one block per entry, grouped under producer sub-headers.
    fn view_catalogue_entries<'a>(&'a self, snapshot: &'a CatalogueSnapshot) -> Elem<'a> {
        let mut section = column![common::section_label("State, events & messages")].spacing(10.0);
        if snapshot.entries.is_empty() {
            return section
                .push(
                    text("No interop handles seen yet. Declared handles, emitted events, and posted messages all appear here.")
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

/// One catalogue entry: identity row, shapes, and the recent samples.
fn entry_block(entry: &CatalogueEntryView) -> Elem<'_> {
    let mut head = row![
        common::badge(entry.kind.as_str()),
        text(entry.name.to_string()).size(13.0).font(MONO),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    let provenance = match (entry.declared, entry.runtime_confirmed) {
        (true, true) => "declared",
        (true, false) => "declared \u{00B7} not seen this session",
        (false, true) => "created at runtime",
        (false, false) => "observed \u{00B7} undeclared",
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
            text(format!("shape: {shape}"))
                .size(11.0)
                .font(MONO)
                .style(common::muted),
        );
    }
    if let Some(declared) = &entry.declared_shape {
        block = block.push(
            text(format!("declared: {}", first_lines(declared, 6)))
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
        meta.push_str(" \u{00B7} truncated");
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
        0..=2 => "just now".to_string(),
        3..=99 => format!("{elapsed_s}s ago"),
        100..=5999 => format!("{}m ago", elapsed_s / 60),
        _ => format!("{}h ago", elapsed_s / 3600),
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
