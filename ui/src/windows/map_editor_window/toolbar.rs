//! The editor toolbar: the tools, the level stepper and undo/redo on the
//! left; the map's save status, Active switch and actions on the right.

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, space, text, tooltip};
use iced::{Length, Padding};
use smudgy_cloud::MapStorage;
use smudgy_cloud::clans::action;
use smudgy_cloud::mapper::AreaSaveStatus;
use smudgy_map_widget::map_editor::Tool;

use crate::assets::{bootstrap_icons, fonts};
use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::widgets::dropdown::Dropdown;

use super::{MapEditorWindow, Message, ScopeTarget};

const ICON_SIZE: f32 = 16.0;

fn icon(codepoint: &'static str) -> iced::widget::Text<'static, crate::Theme> {
    text(codepoint).font(fonts::BOOTSTRAP_ICONS).size(ICON_SIZE)
}

fn tool_button(
    codepoint: &'static str,
    label: &'static str,
    tool: Tool,
    active_tool: Tool,
    enabled: bool,
) -> ThemedElement<'static, Message> {
    tooltip(
        button(icon(codepoint))
            .style(if tool == active_tool {
                builtins::button::list_item_selected
            } else {
                builtins::button::toolbar
            })
            .on_press_maybe(enabled.then_some(Message::ToolSelected(tool))),
        label,
        tooltip::Position::Bottom,
    )
    .into()
}

/// An icon-and-label toolbar button, as in the automations top bar.
fn action_button<'a>(
    codepoint: &'static str,
    label: String,
    message: Message,
    active: bool,
) -> button::Button<'a, Message, crate::Theme> {
    button(
        row![
            text(codepoint).font(fonts::BOOTSTRAP_ICONS).size(13.0),
            text(label).size(13.0),
        ]
        .spacing(7.0)
        .align_y(Vertical::Center),
    )
    .style(if active {
        builtins::button::toolbar_active
    } else {
        builtins::button::toolbar
    })
    .padding(Padding {
        top: 5.0,
        bottom: 5.0,
        left: 9.0,
        right: 9.0,
    })
    .on_press(message)
}

pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let active_tool = window.editor.tool();
    // Rooms, labels and shapes go where "Add to" says; links still go into
    // the map.
    let can_edit = window.can_edit_active_area();
    let can_add = window.can_add_here();

    let tools = row![
        tool_button(
            bootstrap_icons::CURSOR,
            crate::i18n::ts!("mapper-tool-select"),
            Tool::Select,
            active_tool,
            true
        ),
        tool_button(
            bootstrap_icons::PLUS_SQUARE,
            crate::i18n::ts!("mapper-tool-add-room"),
            Tool::AddRoom,
            active_tool,
            can_add
        ),
        tool_button(
            bootstrap_icons::ARROW_REPEAT,
            crate::i18n::ts!("mapper-tool-link"),
            Tool::Link,
            active_tool,
            can_edit
        ),
        tool_button(
            bootstrap_icons::FONTS,
            crate::i18n::ts!("mapper-tool-add-label"),
            Tool::AddLabel,
            active_tool,
            can_add
        ),
        tool_button(
            bootstrap_icons::BOUNDING_BOX,
            crate::i18n::ts!("mapper-tool-add-shape"),
            Tool::AddShape,
            active_tool,
            can_add
        ),
    ]
    .spacing(2);

    let level = row![
        tooltip(
            button(icon(bootstrap_icons::CHEVRON_DOWN))
                .style(builtins::button::toolbar)
                .on_press(Message::LevelDown),
            crate::i18n::ts!("mapper-level-down"),
            tooltip::Position::Bottom,
        ),
        text(crate::i18n::t!("mapper-level", "level" => window.editor.level())).size(14),
        tooltip(
            button(icon(bootstrap_icons::CHEVRON_UP))
                .style(builtins::button::toolbar)
                .on_press(Message::LevelUp),
            crate::i18n::ts!("mapper-level-up"),
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(2)
    .align_y(Vertical::Center);

    // Undo/redo replay mutations, so each entry is checked against the
    // places it writes (also enforced in the Hotkey::Undo/Redo handlers).
    let history = row![
        tooltip(
            button(icon(bootstrap_icons::ARROW_COUNTERCLOCKWISE))
                .style(builtins::button::toolbar)
                .on_press_maybe((window.can_undo() && window.may_undo()).then_some(Message::Undo)),
            crate::i18n::ts!("mapper-undo"),
            tooltip::Position::Bottom,
        ),
        tooltip(
            button(icon(bootstrap_icons::ARROW_CLOCKWISE))
                .style(builtins::button::toolbar)
                .on_press_maybe((window.can_redo() && window.may_redo()).then_some(Message::Redo)),
            crate::i18n::ts!("mapper-redo"),
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(2);

    let mut bar = row![].align_y(Vertical::Center).padding(4).spacing(4);
    if let Some(picker) = super::secrets::add_to_picker(window) {
        bar = bar.push(picker);
        bar = bar.push(space::horizontal().width(16.0));
    }
    bar = bar.extend([
        tools.into(),
        space::horizontal().width(16.0).into(),
        level.into(),
        space::horizontal().width(16.0).into(),
        history.into(),
        space::horizontal().into(),
        sync_indicator(window),
    ]);

    if let Some(area_id) = window.editor.area_id() {
        bar = bar.push(space::horizontal().width(12.0));
        bar = bar.push(active_switch(window, area_id));
        bar = bar.push(space::horizontal().width(12.0));
        if window.can_share_active_area()
            && window.mapper.area_storage(&area_id) == MapStorage::Cloud
        {
            bar = bar.push(action_button(
                bootstrap_icons::SHARE,
                crate::i18n::t!("mapper-share"),
                Message::ShareDialogRequested,
                false,
            ));
        }
        if let Some(menu) = map_menu(window, area_id) {
            bar = bar.push(menu);
        }
    }

    container(bar)
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .into()
}

/// Whether the map is used to find your location: a switch showing its
/// state, the tooltip saying what that means.
fn active_switch(
    window: &MapEditorWindow,
    area_id: smudgy_cloud::AreaId,
) -> ThemedElement<'_, Message> {
    let enabled = window.mapper.is_area_enabled(&area_id);
    let (codepoint, label) = if enabled {
        (
            bootstrap_icons::TOGGLE_ON,
            crate::i18n::t!("inspector-active"),
        )
    } else {
        (
            bootstrap_icons::TOGGLE_OFF,
            crate::i18n::t!("inspector-inactive"),
        )
    };
    tooltip(
        button(
            row![
                text(codepoint).font(fonts::BOOTSTRAP_ICONS).size(ICON_SIZE),
                text(label).size(13.0),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
        )
        .style(builtins::button::toolbar)
        .padding([3, 6])
        .on_press(Message::ToggleAreaEnabled(area_id)),
        container(text(crate::i18n::t!("inspector-active-help")).size(12))
            .padding(6.0)
            .style(builtins::container::tooltip),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The ⋯ menu of the map's own actions; `None` when none applies.
fn map_menu(
    window: &MapEditorWindow,
    area_id: smudgy_cloud::AreaId,
) -> Option<ThemedElement<'_, Message>> {
    let storage = window.mapper.area_storage(&area_id);
    let cloud = storage == MapStorage::Cloud;
    let owned = window.area_owned(area_id);
    let atlas = window.mapper.get_current_atlas();
    let filed = atlas
        .get_area(&area_id)
        .is_some_and(|area| area.meta().atlas_id.is_some());

    // On a clan's map the map's own actions decide; on any other map, ownership.
    let may = |clan_action| window.may_manage_area(area_id, clan_action);
    let mut entries: Vec<(String, Message)> = Vec::new();
    if may(action::RENAME_AREA) {
        entries.push((
            crate::i18n::t!("mapper-menu-rename"),
            Message::RenameAreaStarted(area_id),
        ));
    }
    if cloud && owned {
        entries.push((
            crate::i18n::t!("mapper-duplicate"),
            Message::DuplicateAreaRequested,
        ));
    }
    if window.can_copy_active_area() {
        entries.push((
            crate::i18n::t!("mapper-copy-to-my-maps"),
            Message::CopyAreaRequested,
        ));
    }
    if may(action::REFILE_AREA) {
        let label = if storage == MapStorage::Session {
            crate::i18n::t!("mapper-menu-save")
        } else {
            crate::i18n::t!("mapper-menu-move-to-folder")
        };
        entries.push((label, Message::MoveAreaRequested(area_id)));
    }
    // A loose cloud map carries its own server checklist; a filed one is
    // scoped by its folder.
    if cloud && !filed {
        entries.push((
            crate::i18n::t!("area-list-servers-action"),
            Message::ServersChecklistRequested(ScopeTarget::Area(area_id)),
        ));
    }
    // A clan's map is never transferred; it leaves the clan only as a copy.
    if cloud && owned && window.area_clan(area_id).is_none() {
        entries.push((
            crate::i18n::t!("mapper-transfer-action"),
            Message::TransferOwnershipRequested,
        ));
    }
    // A map someone put in a clan folder can be taken back out of the clan.
    if may(action::DELETE_AREA) {
        entries.push((
            crate::i18n::t!("mapper-menu-delete"),
            Message::DeleteAreaRequested(area_id),
        ));
    }
    if entries.is_empty() {
        return None;
    }

    let open = window.map_menu_open;
    let trigger = button(text("⋯").size(ICON_SIZE))
        .style(if open {
            builtins::button::toolbar_active
        } else {
            builtins::button::toolbar
        })
        .padding([3, 9])
        .on_press(Message::MapMenuToggled(!open));
    let menu = open.then(|| {
        let mut list = column![].spacing(2);
        for (label, message) in entries {
            list = list.push(
                button(text(label).size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(builtins::button::link)
                    .on_press(Message::MenuPicked(Box::new(message))),
            );
        }
        container(list)
            .width(200)
            .padding(6)
            .style(builtins::container::card)
            .into()
    });
    Some(Dropdown::new(trigger, menu, Message::MapMenuToggled(false)).into())
}

/// Why the edited map's writes are parked, in the viewer's language: the
/// server's refusal through [`display_error`], or the queue's own `message`
/// for a park the server did not refuse.
fn parked_reason(window: &MapEditorWindow, message: String) -> String {
    window
        .editor
        .area_id()
        .and_then(|area_id| window.mapper.parked_error(area_id))
        .map_or(message, |error| display_error(&error))
}

/// The mapper's cloud-sync readout. While a tick is in flight it is a passive
/// status; otherwise it doubles as a **Sync** button that triggers an immediate
/// sync — the engine no longer polls on a timer, so this is how the user pulls
/// remote changes (and retries after a failure) on demand.
fn sync_indicator(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let status = window
        .editor
        .area_id()
        .map_or(AreaSaveStatus::Saved, |area_id| {
            window.mapper.area_save_status(area_id)
        });
    type StyleFn = fn(&crate::Theme) -> iced::widget::text::Style;
    let normal: StyleFn = |theme: &crate::Theme| iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.7)),
    };
    let error: StyleFn = |theme: &crate::Theme| iced::widget::text::Style {
        color: Some(theme.styles.text.error),
    };
    let (codepoint, label, color) = match &status {
        AreaSaveStatus::Saved => (
            bootstrap_icons::CLOUD_CHECK,
            crate::i18n::t!("mapper-status-saved"),
            normal,
        ),
        AreaSaveStatus::Saving(pending) => (
            bootstrap_icons::CLOUD_UPLOAD,
            crate::i18n::t!("mapper-status-saving", "count" => *pending),
            normal,
        ),
        AreaSaveStatus::Offline(pending) => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            crate::i18n::t!("mapper-status-offline", "count" => *pending),
            error,
        ),
        AreaSaveStatus::Held(pending) => (
            bootstrap_icons::CLOUD_UPLOAD,
            crate::i18n::t!("mapper-status-held", "count" => *pending),
            normal,
        ),
        AreaSaveStatus::ConflictNeedsReview => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            crate::i18n::t!("mapper-status-conflict"),
            error,
        ),
        AreaSaveStatus::CouldNotSave { .. } => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            crate::i18n::t!("mapper-status-could-not-save"),
            error,
        ),
    };

    let content = row![
        text(codepoint)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(13.0)
            .style(color),
        text(label).size(12).style(color),
    ]
    .spacing(4)
    .align_y(Vertical::Center);

    match status {
        AreaSaveStatus::ConflictNeedsReview => row![
            container(content).padding([2, 6]),
            button(text(crate::i18n::t!("mapper-save-keep-mine")).size(11))
                .style(builtins::button::secondary)
                .on_press(Message::KeepMineRequested),
            button(text(crate::i18n::t!("mapper-save-keep-theirs")).size(11))
                .style(builtins::button::secondary)
                .on_press(Message::KeepTheirsRequested),
        ]
        .spacing(4)
        .align_y(Vertical::Center)
        .into(),
        AreaSaveStatus::CouldNotSave { message, retryable } => row![
            tooltip(
                container(content).padding([2, 6]),
                text(parked_reason(window, message)).size(12),
                tooltip::Position::Bottom,
            ),
            button(text(crate::i18n::t!("action-retry")).size(11))
                .style(builtins::button::secondary)
                .on_press_maybe(retryable.then_some(Message::RetrySaveRequested)),
            button(text(crate::i18n::t!("editor-discard")).size(11))
                .style(builtins::button::secondary)
                .on_press(Message::DiscardFailedSaveRequested),
        ]
        .spacing(4)
        .align_y(Vertical::Center)
        .into(),
        AreaSaveStatus::Saving(_) => tooltip(
            container(content).padding([2, 6]),
            crate::i18n::ts!("mapper-pending-tip"),
            tooltip::Position::Bottom,
        )
        .into(),
        AreaSaveStatus::Saved => tooltip(
            button(content)
                .style(builtins::button::subtle)
                .padding([2, 6])
                .on_press(Message::SyncNowRequested),
            crate::i18n::ts!("mapper-sync-tip"),
            tooltip::Position::Bottom,
        )
        .into(),
        AreaSaveStatus::Offline(_) | AreaSaveStatus::Held(_) => tooltip(
            button(content)
                .style(builtins::button::subtle)
                .padding([2, 6])
                .on_press(Message::SyncNowRequested),
            crate::i18n::ts!("mapper-pending-tip"),
            tooltip::Position::Bottom,
        )
        .into(),
    }
}
