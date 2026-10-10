//! The canvas's context menu: a right click (or Shift+F10 / the Menu key)
//! offers what applies where it lands. Each entry reuses the action its
//! shortcut runs.

use iced::alignment::Vertical;
use iced::keyboard::{self, key::Named};
use iced::widget::{Column, button, container, row, rule, space, text};
use iced::{Length, Point};
use smudgy_cloud::{ConnectionId, ConnectionRouting};
use smudgy_map_widget::map_editor::EntityId;

use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::widgets::dropdown::Dropdown;

use super::{MapEditorWindow, Message};

/// An open context menu: where it floats (canvas-relative), the map point
/// it was opened over, and its page.
#[derive(Debug, Clone, Copy)]
pub struct ContextMenu {
    pub at: Point,
    pub map: Point,
    pub page: Page,
    /// Opened from the keyboard: at the canvas's middle rather than a
    /// clicked point, so nothing is offered "here".
    pub keyboard: bool,
    /// The row the arrow keys have highlighted.
    pub cursor: Option<usize>,
}

impl ContextMenu {
    #[must_use]
    pub fn new(at: Point, map: Point) -> Self {
        Self {
            at,
            map,
            page: Page::Main,
            keyboard: false,
            cursor: None,
        }
    }

    /// A menu opened by Shift+F10 or the Menu key, its first row highlighted.
    #[must_use]
    pub fn keyboard(at: Point, map: Point) -> Self {
        Self {
            keyboard: true,
            cursor: Some(0),
            ..Self::new(at, map)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Main,
    /// The places the selection can move to.
    MoveTo,
}

#[derive(Debug, Clone)]
pub enum ContextAction {
    Cut,
    Copy,
    Delete,
    /// Paste with the clipboard's center at a map point.
    PasteHere(Point),
    AddPointHere(ConnectionId, Point),
    RemovePoint,
    /// Open the "Move to" page.
    MoveToPage,
    /// Back to the main page.
    Back,
    MoveTo(smudgy_cloud::SourceId),
}

impl ContextAction {
    /// The page this entry turns to, when it turns one instead of acting.
    #[must_use]
    pub fn page(&self) -> Option<Page> {
        match self {
            Self::MoveToPage => Some(Page::MoveTo),
            Self::Back => Some(Page::Main),
            _ => None,
        }
    }
}

struct Entry {
    label: String,
    shortcut: Option<&'static str>,
    action: ContextAction,
}

/// The modifier the editor's shortcuts use, as the platform writes it.
fn shortcut(key: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        match key {
            "X" => "\u{2318}X",
            "C" => "\u{2318}C",
            _ => key,
        }
    } else {
        match key {
            "X" => "Ctrl+X",
            "C" => "Ctrl+C",
            _ => key,
        }
    }
}

/// The menu's entries in groups, ruled apart: clipboard, editing, then
/// destructive last. Empty when nothing applies (the menu doesn't open).
fn entries(window: &MapEditorWindow, menu: ContextMenu) -> Vec<Vec<Entry>> {
    let selection = window.editor.selection();
    let atlas = window.mapper.get_current_atlas();
    let area = window
        .editor
        .area_id()
        .and_then(|area_id| atlas.get_area(&area_id));
    // Where the selection can move: one place to one place.
    let targets = area
        .as_ref()
        .filter(|_| window.secrets_apply())
        .and_then(|area| match window.selection_place(area)? {
            super::moves::SelectionPlace::One(from) => Some(window.move_targets(area, from)),
            super::moves::SelectionPlace::Several => None,
        })
        .unwrap_or_default();

    if menu.page == Page::MoveTo {
        let back = Entry {
            label: format!("\u{2039} {}", crate::i18n::t!("mapper-menu-move-to")),
            shortcut: None,
            action: ContextAction::Back,
        };
        let places = targets
            .into_iter()
            .map(|place| Entry {
                label: place.name,
                shortcut: None,
                action: ContextAction::MoveTo(place.source),
            })
            .collect();
        return vec![vec![back], places];
    }
    // New content goes into the selected source; existing content is
    // governed by the source that owns it.
    let selected_source = area
        .as_ref()
        .and_then(|area| match window.selection_place(area)? {
            super::moves::SelectionPlace::One(source) => Some(source),
            super::moves::SelectionPlace::Several => None,
        });
    let can_copy = area
        .as_ref()
        .zip(selected_source)
        .is_some_and(|(area, source)| super::secrets::can(area, source, "copy"));
    let can_cut = area
        .as_ref()
        .zip(selected_source)
        .is_some_and(|(area, source)| {
            super::secrets::can_remove(area, source) || super::secrets::can(area, source, "edit")
        });
    let can_change = window.selection_writable();
    let delete = || Entry {
        label: crate::i18n::t!("action-delete"),
        shortcut: Some("Del"),
        action: ContextAction::Delete,
    };

    if selection.is_empty() {
        let clipboard = window.clipboard.load_full();
        let reposition = clipboard.cut.as_ref().is_some_and(|cut| {
            window.editor.area_id() == Some(cut.map)
                && window.add_to() == cut.source
                && area
                    .as_ref()
                    .is_some_and(|area| super::secrets::can(area, cut.source, "edit"))
        });
        if clipboard.is_empty() || (!window.can_add_here() && !reposition) {
            return Vec::new();
        }
        return vec![vec![Entry {
            label: crate::i18n::t!("mapper-menu-paste-here"),
            shortcut: None,
            action: ContextAction::PasteHere(menu.map),
        }]];
    }

    if window.editor.selected_waypoint().is_some() {
        if !can_change {
            return Vec::new();
        }
        return vec![vec![Entry {
            label: crate::i18n::t!("mapper-menu-remove-point"),
            shortcut: Some("Del"),
            action: ContextAction::RemovePoint,
        }]];
    }

    let mut clipboard_entries = Vec::new();
    if can_cut {
        clipboard_entries.push(Entry {
            label: crate::i18n::t!("action-cut"),
            shortcut: Some(shortcut("X")),
            action: ContextAction::Cut,
        });
    }
    if can_copy {
        clipboard_entries.push(Entry {
            label: crate::i18n::t!("action-copy"),
            shortcut: Some(shortcut("C")),
            action: ContextAction::Copy,
        });
    }
    let mut groups = Vec::new();
    if !clipboard_entries.is_empty() {
        groups.push(clipboard_entries);
    }
    if !targets.is_empty() {
        groups.push(vec![Entry {
            label: format!("{} \u{203A}", crate::i18n::t!("mapper-menu-move-to")),
            shortcut: None,
            action: ContextAction::MoveToPage,
        }]);
    }
    if can_change && let Some(EntityId::Connection(connection_id)) = selection.single() {
        // Stub routing draws no line to put a point on.
        let routed = area
            .as_ref()
            .and_then(|area| area.find_connection(connection_id))
            .is_some_and(|(_, connection)| connection.routing != ConnectionRouting::Stub);
        if routed && !menu.keyboard {
            groups.push(vec![Entry {
                label: crate::i18n::t!("mapper-menu-add-point-here"),
                shortcut: None,
                action: ContextAction::AddPointHere(connection_id, menu.map),
            }]);
        }
    }
    if can_change {
        groups.push(vec![delete()]);
    }
    groups
}

/// The menu's rows' actions in order, for the arrow keys and Enter.
pub fn actions(window: &MapEditorWindow, menu: ContextMenu) -> Vec<ContextAction> {
    entries(window, menu)
        .into_iter()
        .flatten()
        .map(|entry| entry.action)
        .collect()
}

/// Whether a menu at `menu` would offer anything.
pub fn applies(window: &MapEditorWindow, menu: ContextMenu) -> bool {
    !entries(window, menu).is_empty()
}

fn faint(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.45)),
    }
}

/// The canvas with the open menu floated over it. The canvas is always
/// wrapped, so opening and closing the menu keeps its state.
pub fn view<'a>(
    window: &'a MapEditorWindow,
    canvas: ThemedElement<'a, Message>,
) -> ThemedElement<'a, Message> {
    let open = window.context_menu;
    let content = open.map(|menu| {
        let mut list = Column::new().spacing(2);
        let mut row_index = 0;
        for (index, group) in entries(window, menu).into_iter().enumerate() {
            if index > 0 {
                list = list.push(container(rule::horizontal(1)).padding([3, 0]));
            }
            for entry in group {
                let mut line = row![text(entry.label).size(13), space::horizontal()]
                    .spacing(12)
                    .align_y(Vertical::Center);
                if let Some(keys) = entry.shortcut {
                    line = line.push(text(keys).size(11).style(faint));
                }
                let highlighted = menu.cursor == Some(row_index);
                row_index += 1;
                list = list.push(
                    button(line)
                        .width(Length::Fill)
                        .padding([5, 10])
                        .style(if highlighted {
                            builtins::button::list_item_selected
                        } else {
                            builtins::button::list_item
                        })
                        .on_press(Message::ContextAction(entry.action)),
                );
            }
        }
        container(list)
            .width(220)
            .padding(6)
            .style(builtins::container::card)
            .into()
    });
    Dropdown::new(canvas, content, Message::ContextMenuClosed)
        .at(open.map_or(Point::ORIGIN, |menu| menu.at))
        .on_key(|key| match key {
            keyboard::Key::Named(Named::ArrowDown) => Some(Message::ContextMenuStep(1)),
            keyboard::Key::Named(Named::ArrowUp) => Some(Message::ContextMenuStep(-1)),
            keyboard::Key::Named(Named::Enter) => Some(Message::ContextMenuActivated),
            _ => None,
        })
        .into()
}
