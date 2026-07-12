//! The editor toolbar: tool toggles, level stepper, undo/redo, and the
//! background-sync status indicator.

use iced::alignment::Vertical;
use iced::widget::{button, container, row, space, text, tooltip};
use iced::Length;
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
            "Select",
            Tool::Select,
            active_tool,
            true
        ),
        tool_button(
            bootstrap_icons::PLUS_SQUARE,
            "Add room",
            Tool::AddRoom,
            active_tool,
            can_edit
        ),
        tool_button(
            bootstrap_icons::FONTS,
            "Add label",
            Tool::AddLabel,
            active_tool,
            can_edit
        ),
        tool_button(
            bootstrap_icons::BOUNDING_BOX,
            "Add shape",
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
            "Level down",
            tooltip::Position::Bottom,
        ),
        text(format!("Level {}", window.editor.level())).size(14),
        tooltip(
            button(icon(bootstrap_icons::CHEVRON_UP))
                .style(builtins::button::toolbar)
                .on_press(Message::LevelUp),
            "Level up",
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
            "Undo",
            tooltip::Position::Bottom,
        ),
        tooltip(
            button(icon(bootstrap_icons::ARROW_CLOCKWISE))
                .style(builtins::button::toolbar)
                .on_press_maybe((can_edit && window.can_redo()).then_some(Message::Redo)),
            "Redo",
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
            "Secrets in this area",
            tooltip::Position::Bottom,
        ));
    }

    // Sharing: owners always, plus re-share grantees.
    if window.can_share_active_area() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text("Share").size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::ShareDialogRequested),
            "Share this area with friends",
            tooltip::Position::Bottom,
        ));
    }

    // Modify-by-clone: shared areas whose grant includes `copy`.
    if window.can_copy_active_area() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text("Copy to my maps").size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::CopyAreaRequested),
            "Make your own editable copy of this shared map",
            tooltip::Position::Bottom,
        ));
    }

    // Duplicate: owner self-copy (e.g. to share a version with some secrets
    // unmarked). Shared areas get "Copy to my maps" above instead.
    if window.area_is_owned() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text("Duplicate").size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::DuplicateAreaRequested),
            "Make a copy of this map \u{2014} useful for sharing a version with some secrets unmarked",
            tooltip::Position::Bottom,
        ));
    }

    // transfer ownership — owner-only (a can_admin deputy cannot transfer).
    if window.area_is_owned() {
        bar = bar.push(space::horizontal().width(8.0));
        bar = bar.push(tooltip(
            button(text("Transfer\u{2026}").size(13))
                .style(builtins::button::toolbar)
                .on_press(Message::TransferOwnershipRequested),
            "Give this map to a friend (they become the owner once they accept)",
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
            text("Inactive").size(12).style(muted),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
        "Not used to find your location \u{2014} activate it in the area list",
        tooltip::Position::Bottom,
    )
    .into()
}

/// The mapper's cloud-sync readout. While a tick is in flight it is a passive
/// status; otherwise it doubles as a **Sync** button that triggers an immediate
/// sync — the engine no longer polls on a timer, so this is how the user pulls
/// remote changes (and retries after a failure) on demand.
fn sync_indicator(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let stats = window.mapper.get_sync_stats();
    let pending = stats.pending_operations();
    let failed = stats.operations_failed();

    // "Busy" = a sync is actively flushing pending writes; only then is the
    // readout passive. Idle and post-failure states are clickable.
    let busy = pending > 0;

    type StyleFn = fn(&crate::Theme) -> iced::widget::text::Style;
    let (codepoint, label, color): (&str, String, StyleFn) = if busy {
        (
            bootstrap_icons::CLOUD_UPLOAD,
            format!("syncing {pending}"),
            |theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.7)),
            },
        )
    } else if failed > 0 {
        (
            bootstrap_icons::EXCLAMATION_TRIANGLE,
            format!("{failed} failed"),
            |theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.error),
            },
        )
    } else {
        (
            bootstrap_icons::CLOUD_CHECK,
            "Sync".to_string(),
            |theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.4)),
            },
        )
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

    if busy {
        // Padding matches the idle button so the row doesn't shift when a tick
        // starts or finishes.
        container(content).padding([2, 6]).into()
    } else {
        tooltip(
            button(content)
                .style(builtins::button::subtle)
                .padding([2, 6])
                .on_press(Message::SyncNowRequested),
            "Sync with the cloud now",
            tooltip::Position::Bottom,
        )
        .into()
    }
}
