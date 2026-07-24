//! The editor toolbar: tool toggles, level stepper, undo/redo, and the
//! background-sync status indicator.

use iced::Length;
use iced::alignment::Vertical;
use iced::widget::{button, container, row, space, text, tooltip};
use smudgy_cloud::mapper::AreaSaveStatus;
use smudgy_map_widget::map_editor::Tool;

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::{MapEditorWindow, Message};

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

pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let active_tool = window.editor.tool();
    // Creation tools mutate the map; view-only shared areas get Select only.
    let can_edit = window.can_edit_active_area();

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
            can_edit
        ),
        tool_button(
            bootstrap_icons::ARROW_REPEAT,
            "Link rooms (Ctrl: one-way)",
            Tool::Link,
            active_tool,
            can_edit
        ),
        tool_button(
            bootstrap_icons::FONTS,
            crate::i18n::ts!("mapper-tool-add-label"),
            Tool::AddLabel,
            active_tool,
            can_edit
        ),
        tool_button(
            bootstrap_icons::BOUNDING_BOX,
            crate::i18n::ts!("mapper-tool-add-shape"),
            Tool::AddShape,
            active_tool,
            can_edit
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

    // Undo/redo replay mutations, so they share the creation tools' gate
    // (also enforced in the Hotkey::Undo/Redo handlers).
    let history = row![
        tooltip(
            button(icon(bootstrap_icons::ARROW_COUNTERCLOCKWISE))
                .style(builtins::button::toolbar)
                .on_press_maybe((can_edit && window.can_undo()).then_some(Message::Undo)),
            crate::i18n::ts!("mapper-undo"),
            tooltip::Position::Bottom,
        ),
        tooltip(
            button(icon(bootstrap_icons::ARROW_CLOCKWISE))
                .style(builtins::button::toolbar)
                .on_press_maybe((can_edit && window.can_redo()).then_some(Message::Redo)),
            crate::i18n::ts!("mapper-redo"),
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(2);

    let mut bar = row![tools, space::horizontal().width(16.0), level]
        .align_y(Vertical::Center)
        .padding(4)
        .spacing(4);

    // Secrets audit is owner-only; non-owners never see the entry point.
    if window.area_is_owned() {
        bar = bar.push(space::horizontal().width(16.0));
        bar = bar.push(tooltip(
            button(
                text(super::ICON_LOCK_FILL)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(ICON_SIZE),
            )
            .style(builtins::button::toolbar)
            .on_press(Message::SecretsAuditRequested),
            crate::i18n::ts!("mapper-secrets-title"),
            tooltip::Position::Bottom,
        ));
    }

    // Sharing: owners always, plus re-share grantees.
    if window.can_share_active_area() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text(crate::i18n::t!("mapper-share")).size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::ShareDialogRequested),
            crate::i18n::ts!("mapper-share-area-tip"),
            tooltip::Position::Bottom,
        ));
    }

    // Modify-by-clone: shared areas whose grant includes `copy`.
    if window.can_copy_active_area() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text(crate::i18n::t!("mapper-copy-to-my-maps")).size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::CopyAreaRequested),
            crate::i18n::ts!("mapper-copy-shared-tip"),
            tooltip::Position::Bottom,
        ));
    }

    // Duplicate: owner self-copy (e.g. to share a version with some secrets
    // unmarked). Shared areas get "Copy to my maps" above instead.
    if window.area_is_owned() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text(crate::i18n::t!("mapper-duplicate")).size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::DuplicateAreaRequested),
            crate::i18n::ts!("mapper-duplicate-tip"),
            tooltip::Position::Bottom,
        ));
    }

    // transfer ownership — owner-only (a can_admin deputy cannot transfer).
    if window.area_is_owned() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text(crate::i18n::t!("mapper-transfer-action")).size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::TransferOwnershipRequested),
            crate::i18n::ts!("mapper-transfer-tip"),
            tooltip::Position::Bottom,
        ));
    }

    bar = bar.push(space::horizontal());
    // Quiet "inactive" chip when the active area is disabled for room
    // identification (display only; toggle lives in the area list).
    if window.active_area_disabled() {
        bar = bar.push(inactive_chip());
        bar = bar.push(space::horizontal().width(12.0));
    }
    bar = bar.push(sync_indicator(window));
    bar = bar.push(space::horizontal().width(16.0));
    bar = bar.push(history);

    container(bar)
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .into()
}

/// A quiet, non-interactive chip flagging that the active area is inactive
/// (not used to find your location). The switch itself lives in the area list.
fn inactive_chip() -> ThemedElement<'static, Message> {
    let muted = |theme: &crate::Theme| iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.5)),
    };
    tooltip(
        row![
            text(bootstrap_icons::TOGGLE_OFF)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(13.0)
                .style(muted),
            text(crate::i18n::t!("inspector-inactive")).size(12).style(muted),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
        crate::i18n::ts!("mapper-inactive-location-tip"),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The mapper's cloud-sync readout. While a tick is in flight it is a passive
/// status; otherwise it doubles as a **Sync** button that triggers an immediate
/// sync — the engine no longer polls on a timer, so this is how the user pulls
/// remote changes (and retries after a failure) on demand.
fn sync_indicator(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    const PENDING_WARNING: &str = "Pending changes are held only for this session. Closing now attempts a final sync, but may lose unsent work.";
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
        AreaSaveStatus::Saved => (bootstrap_icons::CLOUD_CHECK, "Saved".to_string(), normal),
        AreaSaveStatus::Saving(pending) => (
            bootstrap_icons::CLOUD_UPLOAD,
            format!("Saving {pending} changes"),
            normal,
        ),
        AreaSaveStatus::Offline(pending) => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            format!("Offline, {pending} changes pending"),
            error,
        ),
        AreaSaveStatus::ConflictNeedsReview => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            "Conflict needs review".to_string(),
            error,
        ),
        AreaSaveStatus::CouldNotSave(_) => (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            "Could not save".to_string(),
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
        AreaSaveStatus::CouldNotSave(message) => row![
            tooltip(
                container(content).padding([2, 6]),
                text(message).size(12),
                tooltip::Position::Bottom,
            ),
            button(text(crate::i18n::t!("action-retry")).size(11))
                .style(builtins::button::secondary)
                .on_press(Message::RetrySaveRequested),
            button(text(crate::i18n::t!("editor-discard")).size(11))
                .style(builtins::button::secondary)
                .on_press(Message::DiscardFailedSaveRequested),
        ]
        .spacing(4)
        .align_y(Vertical::Center)
        .into(),
        AreaSaveStatus::Saving(_) => tooltip(
            container(content).padding([2, 6]),
            PENDING_WARNING,
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
        AreaSaveStatus::Offline(_) => tooltip(
            button(content)
                .style(builtins::button::subtle)
                .padding([2, 6])
                .on_press(Message::SyncNowRequested),
            PENDING_WARNING,
            tooltip::Position::Bottom,
        )
        .into(),
    }
}
