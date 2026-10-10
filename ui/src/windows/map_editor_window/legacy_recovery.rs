use super::{Event, MapEditorWindow, Message};
use crate::{theme::Element as ThemedElement, update::Update};
use iced::{
    Length, Task,
    widget::{button, column, container, row, text},
};

pub(super) fn view(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    let notice = window.mapper.legacy_cloud_recovery()?;
    if !notice.incomplete && window.dismissed_legacy_recovery.as_ref() == Some(&notice.folder) {
        return None;
    }
    let explanation = if notice.incomplete {
        crate::i18n::t!("mapper-legacy-recovery-incomplete")
    } else {
        crate::i18n::t!("mapper-legacy-recovery-complete")
    };
    let mut actions = row![
        button(text(crate::i18n::t!("mapper-open-recovery-folder")))
            .on_press(Message::OpenLegacyRecovery)
    ]
    .spacing(8);
    actions = if notice.incomplete {
        actions.push(
            button(text(crate::i18n::t!("mapper-retry-recovery")))
                .on_press(Message::RetryLegacyRecovery),
        )
    } else {
        actions.push(
            button(text(crate::i18n::t!("mapper-dismiss-recovery")))
                .on_press(Message::DismissLegacyRecovery),
        )
    };
    Some(
        container(column![text(explanation).size(13), actions].spacing(8))
            .padding(10)
            .width(Length::Fill)
            .into(),
    )
}

pub(super) fn open(window: &MapEditorWindow) -> Update<Message, Event> {
    let Some(notice) = window.mapper.legacy_cloud_recovery() else {
        return Update::none();
    };
    // On an archive error its directory might not exist yet. The viewer root
    // contains the untouched original records and is the useful fallback.
    let folder = if notice.folder.is_dir() {
        notice.folder
    } else {
        let Some(root) = notice.folder.parent().and_then(std::path::Path::parent) else {
            return Update::none();
        };
        root.to_path_buf()
    };
    Update::with_task(Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                open::that(folder).map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(std::convert::identity)
        },
        Message::LegacyRecoveryActionFinished,
    ))
}

pub(super) fn retry(window: &MapEditorWindow) -> Update<Message, Event> {
    let mapper = window.mapper.clone();
    Update::with_task(Task::perform(
        async move {
            tokio::task::spawn_blocking(move || mapper.retry_legacy_cloud_recovery())
                .await
                .map_err(|error| error.to_string())
        },
        Message::LegacyRecoveryActionFinished,
    ))
}
