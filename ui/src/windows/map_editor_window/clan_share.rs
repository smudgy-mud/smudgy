//! Sharing a clan folder with the clan's groups.
//!
//! A clan folder's Share… gives the clan's groups access in the same dialog
//! as a folder share: recipients are Everyone and the clan's own groups,
//! given the map actions, each a checkbox with a preset picker that sets
//! them (Reader, Contributor, Editor). It reaches the clan's Clan-owned
//! maps; a Member-owned map is shared one map at a time by its owners. A
//! group holds one grant over the folder (clans.md §5.3), changed in place
//! from Who has access.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{Column, button, checkbox, column, row, space, text};
use smudgy_cloud::clan_access::{GrantBody, GrantChange};
use smudgy_cloud::clans::{ClanGrant, ClanGrantFilter, ClanGroup, GrantRecipient, GrantScope};
use smudgy_cloud::{AtlasId, CloudError, Uuid};

use crate::components::cloud_errors::display_error;
use crate::components::preset_picker::Picker;
use crate::presets::{self, Facet, Kind, Preset, PresetPick};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::clan_maps::{ClanMessage, ClanModal};
use super::{MapEditorWindow, Message, modals};

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

// ===========================================================================
// A clan folder's Share dialog
// ===========================================================================

/// A group's grant over the folder being changed.
#[derive(Debug, Clone)]
pub struct FolderEdit {
    pub grant_id: Uuid,
    /// The grant's actions before the edit.
    pub before: BTreeSet<String>,
    /// The actions it holds once saved.
    pub actions: BTreeSet<String>,
    pub busy: bool,
}

/// State of a clan folder's Share dialog.
#[derive(Debug, Clone)]
pub struct ClanShareDialog {
    pub clan_id: Uuid,
    /// The folder shared.
    pub folder: (AtlasId, String),
    /// `None` while loading.
    pub groups: Option<Result<Vec<ClanGroup>, String>>,
    pub filter: String,
    pub selected: HashSet<Uuid>,
    /// What the picked groups are given.
    pub actions: BTreeSet<String>,
    pub submitting: bool,
    pub results: Vec<(String, Result<(), CloudError>)>,
    pub close_pending: bool,
    /// Grants to groups that reach the folder.
    pub grants: Option<Result<Vec<ClanGrant>, String>>,
    pub editing: Option<FolderEdit>,
    pub revoking: Option<Uuid>,
    pub revoke_busy: bool,
    pub manage_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ClanShareMessage {
    GroupsLoaded(Result<Vec<ClanGroup>, CloudError>),
    GrantsLoaded(Result<Vec<ClanGrant>, CloudError>),
    FilterChanged(String),
    GroupToggled(Uuid, bool),
    /// What the picked groups are given: a preset, or one action.
    Preset(PresetPick),
    ActionToggled(&'static str, bool),
    Submit,
    Submitted(Vec<(String, Result<(), CloudError>)>),
    CloseTick,
    EditRequested(Uuid),
    EditPreset(PresetPick),
    EditToggled(&'static str, bool),
    EditCancelled,
    EditSaved,
    EditResult(Result<(), CloudError>),
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult(Result<(), CloudError>),
}

fn share(message: ClanShareMessage) -> Message {
    Message::Clan(ClanMessage::Share(message))
}

/// The map actions the dialog gives, each a checkbox.
fn shown_actions() -> Vec<&'static str> {
    Kind::Map.actions().collect()
}

/// The map preset picker over `actions`.
fn picker(actions: &BTreeSet<String>) -> Picker {
    let facet = Facet::of(Preset::MapReader);
    Picker::new(
        facet,
        actions.iter().map(String::as_str),
        facet.presets(),
        |_| true,
    )
}

/// `actions` with `pick`'s actions checked and the rest of its facet's
/// unchecked.
fn apply(actions: &mut BTreeSet<String>, pick: PresetPick) {
    for (action, on) in pick.changes() {
        toggle(actions, action, on);
    }
}

fn toggle(actions: &mut BTreeSet<String>, action: &str, on: bool) {
    if on {
        actions.insert(action.to_string());
    } else {
        actions.remove(action);
    }
}

impl ClanShareDialog {
    fn scope(&self) -> GrantScope {
        GrantScope::Atlases {
            ids: vec![self.folder.0],
        }
    }

    /// The groups offered as recipients: Everyone first, then the clan's own
    /// groups. The owners need no grant and are left out.
    fn recipients(&self) -> Vec<(Uuid, String)> {
        let Some(Ok(groups)) = &self.groups else {
            return Vec::new();
        };
        let mut recipients: Vec<(Uuid, String)> = groups
            .iter()
            .filter(|group| group.builtin.as_deref() == Some("members"))
            .map(|group| (group.id, crate::i18n::t!("clan-maps-everyone")))
            .collect();
        recipients.extend(
            groups
                .iter()
                .filter(|group| !group.is_builtin())
                .map(|group| (group.id, group.name.clone())),
        );
        recipients
    }

    fn group_label(&self, group_id: Uuid) -> String {
        self.recipients()
            .into_iter()
            .find(|(id, _)| *id == group_id)
            .map_or_else(|| crate::i18n::t!("clan-maps-a-group"), |(_, label)| label)
    }

    /// The groups' grants over exactly this folder, whatever they give:
    /// each group's own, changed here.
    fn own_grants(&self) -> Vec<&ClanGrant> {
        let Some(Ok(grants)) = &self.grants else {
            return Vec::new();
        };
        let scope = self.scope();
        grants
            .iter()
            .filter(|grant| grant.recipient.group().is_some() && grant.scope == scope)
            .collect()
    }

    /// Grants to groups naming this folder beside others, which reach it
    /// but are changed where they were made.
    fn wider_grants(&self) -> Vec<&ClanGrant> {
        let Some(Ok(grants)) = &self.grants else {
            return Vec::new();
        };
        grants
            .iter()
            .filter(|grant| {
                grant.recipient.group().is_some()
                    && grant.gives_map_access()
                    && matches!(&grant.scope, GrantScope::Atlases { ids }
                        if ids.len() > 1 && ids.contains(&self.folder.0))
            })
            .collect()
    }

    /// Whether a group already holds a grant over this folder: sharing
    /// again would only add to it, so changes go through Who has access.
    fn has_access(&self, group_id: Uuid) -> bool {
        self.own_grants()
            .iter()
            .any(|grant| grant.recipient.group() == Some(group_id))
    }

    fn picked(&self) -> Vec<(String, Uuid)> {
        self.recipients()
            .into_iter()
            .filter(|(id, _)| self.selected.contains(id) && !self.has_access(*id))
            .map(|(id, label)| (label, id))
            .collect()
    }
}

fn dialog_mut(window: &mut MapEditorWindow) -> Option<&mut ClanShareDialog> {
    match &mut window.modal {
        Some(modals::Modal::Clan(modal)) => match modal.as_mut() {
            ClanModal::Share(dialog) => Some(dialog.as_mut()),
            _ => None,
        },
        _ => None,
    }
}

fn fetch_grants(window: &MapEditorWindow, clan_id: Uuid, folder: AtlasId) -> Task<Message> {
    let client = window.cloud.client.clone();
    let filter = ClanGrantFilter {
        atlas_id: Some(folder),
        ..ClanGrantFilter::default()
    };
    Task::perform(
        async move { client.clan_grants(clan_id, filter).await },
        |result| share(ClanShareMessage::GrantsLoaded(result)),
    )
}

/// Opens the Share dialog for one clan folder.
pub(super) fn open(
    window: &mut MapEditorWindow,
    clan_id: Uuid,
    folder: Option<AtlasId>,
) -> Update<Message, super::Event> {
    // Legacy clan-scoped grants cannot be recreated by this dialog.
    let Some(atlas_id) = folder else {
        return Update::none();
    };
    let name = window
        .atlases
        .iter()
        .find(|atlas| atlas.id == atlas_id)
        .map_or_else(
            || crate::i18n::t!("mapper-this-folder"),
            |atlas| atlas.name.clone(),
        );
    window.modal = Some(modals::Modal::Clan(Box::new(ClanModal::Share(Box::new(
        ClanShareDialog {
            clan_id,
            folder: (atlas_id, name),
            groups: None,
            filter: String::new(),
            selected: HashSet::new(),
            actions: Preset::MapReader.action_set(),
            submitting: false,
            results: Vec::new(),
            close_pending: false,
            grants: None,
            editing: None,
            revoking: None,
            revoke_busy: false,
            manage_error: None,
        },
    )))));
    let client = window.cloud.client.clone();
    Update::with_task(Task::batch([
        Task::perform(async move { client.clan_groups(clan_id).await }, |result| {
            share(ClanShareMessage::GroupsLoaded(result))
        }),
        fetch_grants(window, clan_id, atlas_id),
    ]))
}

#[allow(clippy::too_many_lines)]
pub(super) fn update(
    window: &mut MapEditorWindow,
    message: ClanShareMessage,
) -> Update<Message, super::Event> {
    let client = window.cloud.client.clone();
    let Some(dialog) = dialog_mut(window) else {
        return Update::none();
    };
    let (clan_id, folder) = (dialog.clan_id, dialog.folder.0);
    match message {
        ClanShareMessage::GroupsLoaded(result) => {
            dialog.groups = Some(result.map_err(|error| display_error(&error)));
            Update::none()
        }
        ClanShareMessage::GrantsLoaded(result) => {
            match result {
                Ok(grants) => {
                    let gone = |id: Uuid| !grants.iter().any(|grant| grant.id == id);
                    if dialog.revoking.is_some_and(gone) {
                        dialog.revoking = None;
                    }
                    if dialog
                        .editing
                        .as_ref()
                        .is_some_and(|edit| gone(edit.grant_id))
                    {
                        dialog.editing = None;
                    }
                    dialog.grants = Some(Ok(grants));
                    dialog.manage_error = None;
                }
                Err(error) => {
                    let message = display_error(&error);
                    if dialog.grants.is_none() {
                        dialog.grants = Some(Err(message));
                    } else {
                        dialog.manage_error = Some(message);
                    }
                }
            }
            Update::none()
        }
        ClanShareMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        ClanShareMessage::GroupToggled(group_id, selected) => {
            if selected {
                dialog.selected.insert(group_id);
            } else {
                dialog.selected.remove(&group_id);
            }
            Update::none()
        }
        ClanShareMessage::Preset(pick) => {
            apply(&mut dialog.actions, pick);
            Update::none()
        }
        ClanShareMessage::ActionToggled(action, on) => {
            toggle(&mut dialog.actions, action, on);
            Update::none()
        }
        ClanShareMessage::Submit => {
            let picked = dialog.picked();
            if dialog.submitting || picked.is_empty() || dialog.actions.is_empty() {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.results.clear();
            dialog.close_pending = false;
            let scope = dialog.scope();
            let body = GrantBody {
                actions: dialog.actions.iter().cloned().collect(),
                may_grant: None,
            };
            Update::with_task(Task::perform(
                async move {
                    let mut results = Vec::with_capacity(picked.len());
                    for (label, group_id) in picked {
                        let result = client
                            .grant_in_clan(
                                clan_id,
                                GrantRecipient::Group { group_id },
                                &scope,
                                &body,
                            )
                            .await
                            .map(|_| ());
                        results.push((label, result));
                    }
                    results
                },
                |results| share(ClanShareMessage::Submitted(results)),
            ))
        }
        ClanShareMessage::Submitted(results) => {
            dialog.submitting = false;
            let all_ok = !results.is_empty() && results.iter().all(|(_, result)| result.is_ok());
            dialog.results = results;
            let mut tasks = vec![fetch_grants(window, clan_id, folder)];
            if all_ok {
                if let Some(dialog) = dialog_mut(window) {
                    dialog.close_pending = true;
                    dialog.selected.clear();
                }
                tasks.push(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1400)).await },
                    |()| share(ClanShareMessage::CloseTick),
                ));
            }
            window.mapper.sync_now();
            Update::with_task(Task::batch(tasks))
        }
        ClanShareMessage::CloseTick => {
            if dialog.close_pending {
                window.modal = None;
            }
            Update::none()
        }
        ClanShareMessage::EditRequested(id) => {
            if let Some(grant) = dialog.own_grants().into_iter().find(|grant| grant.id == id) {
                dialog.editing = Some(FolderEdit {
                    grant_id: id,
                    before: grant.actions.clone(),
                    actions: grant.actions.clone(),
                    busy: false,
                });
                dialog.revoking = None;
            }
            Update::none()
        }
        ClanShareMessage::EditPreset(pick) => {
            if let Some(edit) = &mut dialog.editing {
                apply(&mut edit.actions, pick);
            }
            Update::none()
        }
        ClanShareMessage::EditToggled(action, on) => {
            if let Some(edit) = &mut dialog.editing {
                toggle(&mut edit.actions, action, on);
            }
            Update::none()
        }
        ClanShareMessage::EditCancelled => {
            dialog.editing = None;
            Update::none()
        }
        ClanShareMessage::EditSaved => {
            let Some(edit) = &mut dialog.editing else {
                return Update::none();
            };
            // Exactly the boxes changed, so others' changes meanwhile stay.
            let change = GrantChange::between(
                &edit.before,
                &BTreeSet::new(),
                &edit.actions,
                &BTreeSet::new(),
            );
            if edit.busy || change.is_empty() {
                return Update::none();
            }
            edit.busy = true;
            let grant_id = edit.grant_id;
            Update::with_task(Task::perform(
                async move {
                    client
                        .change_clan_grant(clan_id, grant_id, &change)
                        .await
                        .map(|_| ())
                },
                |result| share(ClanShareMessage::EditResult(result)),
            ))
        }
        ClanShareMessage::EditResult(result) => {
            match result {
                Ok(()) => dialog.editing = None,
                Err(error) => {
                    if let Some(edit) = &mut dialog.editing {
                        edit.busy = false;
                    }
                    dialog.manage_error = Some(display_error(&error));
                }
            }
            window.mapper.sync_now();
            Update::with_task(fetch_grants(window, clan_id, folder))
        }
        ClanShareMessage::RevokeRequested(id) => {
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
            dialog.editing = None;
            Update::none()
        }
        ClanShareMessage::RevokeCancelled => {
            dialog.revoking = None;
            dialog.revoke_busy = false;
            Update::none()
        }
        ClanShareMessage::RevokeConfirmed => {
            let Some(grant_id) = dialog.revoking else {
                return Update::none();
            };
            if dialog.revoke_busy {
                return Update::none();
            }
            dialog.revoke_busy = true;
            Update::with_task(Task::perform(
                async move { client.delete_clan_grant(clan_id, grant_id).await },
                |result| share(ClanShareMessage::RevokeResult(result)),
            ))
        }
        ClanShareMessage::RevokeResult(result) => {
            dialog.revoke_busy = false;
            dialog.revoking = None;
            if let Err(error) = result {
                dialog.manage_error = Some(match error {
                    CloudError::NotFoundOrNoAccess => crate::i18n::t!("mapper-could-not-revoke"),
                    other => display_error(&other),
                });
            }
            window.mapper.sync_now();
            Update::with_task(fetch_grants(window, clan_id, folder))
        }
    }
}

/// The preset picker and a checkbox per map action, over `actions`.
fn actions_editor<'a>(
    actions: &BTreeSet<String>,
    enabled: bool,
    pick: impl Fn(PresetPick) -> Message + 'a,
    toggled: impl Fn(&'static str, bool) -> Message + Copy + 'a,
) -> ThemedElement<'a, Message> {
    let picker = picker(actions);
    let mut col = column![picker.view(crate::i18n::t!("mapper-they-can"), enabled.then_some(pick))]
        .spacing(4);
    for action in shown_actions() {
        col = col.push(
            checkbox(actions.contains(action))
                .label(presets::action_label(action))
                .size(13)
                .text_size(12)
                .on_toggle_maybe(enabled.then_some(move |on| toggled(action, on))),
        );
    }
    col.into()
}

/// The dialog's title and body, laid out as a folder share.
pub(super) fn view(dialog: &ClanShareDialog) -> (String, ThemedElement<'_, Message>) {
    let title = crate::i18n::t!("mapper-share-folder-title", "name" => &dialog.folder.1);
    let help = crate::i18n::t!("clan-maps-share-folder-help");

    let mut list = Column::new().spacing(2);
    match &dialog.groups {
        None => {
            list = list.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            list = list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(_)) => {
            let filter = dialog.filter.trim().to_lowercase();
            let mut any = false;
            for (group_id, label) in dialog.recipients() {
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                let checked = dialog.selected.contains(&group_id);
                let item: ThemedElement<'_, Message> = if dialog.has_access(group_id) {
                    row![
                        checkbox(checked).label(label).size(14).text_size(13),
                        text(crate::i18n::t!("mapper-has-access"))
                            .size(11)
                            .style(muted),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center)
                    .into()
                } else {
                    checkbox(checked)
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |value| {
                            share(ClanShareMessage::GroupToggled(group_id, value))
                        })
                        .into()
                };
                list = list.push(item);
            }
            if !any {
                list = list.push(
                    text(crate::i18n::t!("clan-maps-no-groups"))
                        .size(12)
                        .style(muted),
                );
            }
        }
    }
    let recipients = modals::recipient_picker(
        &dialog.filter,
        crate::i18n::ts!("clan-maps-filter-groups-placeholder"),
        |value| share(ClanShareMessage::FilterChanged(value)),
        list,
    );

    let they_can = actions_editor(
        &dialog.actions,
        !dialog.submitting,
        |pick| share(ClanShareMessage::Preset(pick)),
        |action, on| share(ClanShareMessage::ActionToggled(action, on)),
    );

    let mut extra: Vec<ThemedElement<'_, Message>> = Vec::new();
    if !dialog.results.is_empty() {
        let mut results = Column::new().spacing(2);
        for (label, result) in &dialog.results {
            results = results.push(match result {
                Ok(()) => text(crate::i18n::t!("mapper-shared-with", "recipient" => label))
                    .size(12)
                    .style(builtins::text::success),
                Err(CloudError::NotFoundOrNoAccess) => {
                    text(crate::i18n::t!("mapper-share-failed", "recipient" => label))
                        .size(12)
                        .style(builtins::text::danger)
                }
                Err(error) => text(crate::i18n::t!(
                    "mapper-share-error",
                    "recipient" => label,
                    "error" => display_error(error)
                ))
                .size(12)
                .style(builtins::text::danger),
            });
        }
        extra.push(results.into());
    }

    let manage = access_section(dialog);
    let enabled = !dialog.submitting && !dialog.picked().is_empty() && !dialog.actions.is_empty();
    let body = modals::folder_share_layout(
        help,
        recipients,
        they_can,
        extra,
        manage,
        dialog.submitting,
        enabled.then_some(share(ClanShareMessage::Submit)),
    );
    (title, body)
}

/// A grant's actions as chips.
fn badge(grant: &ClanGrant) -> String {
    presets::chips(grant.actions.iter().map(String::as_str))
        .into_iter()
        .map(presets::Chip::label)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Who has access: each group's grant over the folder, with Edit and
/// Revoke, then the grants naming it beside other folders, changed where
/// they were made.
fn access_section(dialog: &ClanShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-who-has-access")).size(13)].spacing(6);
    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }
    match &dialog.grants {
        None => {
            section = section.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(_)) => {
            let grants = dialog.own_grants();
            let wider = dialog.wider_grants();
            if grants.is_empty() && wider.is_empty() {
                section = section.push(
                    text(crate::i18n::t!("mapper-not-shared"))
                        .size(12)
                        .style(muted),
                );
            }
            for grant in grants {
                let label = grant.recipient.group().map_or_else(
                    || crate::i18n::t!("clan-maps-a-group"),
                    |group| dialog.group_label(group),
                );
                section = section.push(
                    row![
                        text(label).size(13),
                        text(badge(grant)).size(11).style(muted),
                        space::horizontal(),
                        button(text(crate::i18n::t!("mapper-edit-flags")).size(11))
                            .style(builtins::button::secondary)
                            .on_press(share(ClanShareMessage::EditRequested(grant.id))),
                        button(text(crate::i18n::t!("mapper-revoke")).size(11))
                            .style(builtins::button::secondary)
                            .on_press(share(ClanShareMessage::RevokeRequested(grant.id))),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                );
                if let Some(edit) = dialog
                    .editing
                    .as_ref()
                    .filter(|edit| edit.grant_id == grant.id)
                {
                    section = section.push(edit_view(edit));
                }
                if dialog.revoking == Some(grant.id) {
                    section = section.push(revoke_confirm(
                        dialog.revoke_busy,
                        share(ClanShareMessage::RevokeCancelled),
                        share(ClanShareMessage::RevokeConfirmed),
                    ));
                }
            }
            for grant in wider {
                let label = grant.recipient.group().map_or_else(
                    || crate::i18n::t!("clan-maps-a-group"),
                    |group| dialog.group_label(group),
                );
                section = section.push(
                    row![
                        text(label).size(13),
                        text(badge(grant)).size(11).style(muted),
                        space::horizontal(),
                        text(crate::i18n::t!("clan-share-from-several-folders"))
                            .size(11)
                            .style(muted),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                );
            }
        }
    }
    section.into()
}

/// Changing a group's grant over the folder in place.
fn edit_view(edit: &FolderEdit) -> ThemedElement<'_, Message> {
    let unchanged = edit.before == edit.actions;
    column![
        actions_editor(
            &edit.actions,
            !edit.busy,
            |pick| share(ClanShareMessage::EditPreset(pick)),
            |action, on| share(ClanShareMessage::EditToggled(action, on)),
        ),
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press_maybe((!edit.busy).then_some(share(ClanShareMessage::EditCancelled))),
            button(text(crate::i18n::t!("action-save")).size(11))
                .style(builtins::button::primary)
                .on_press_maybe(
                    (!edit.busy && !unchanged).then_some(share(ClanShareMessage::EditSaved))
                ),
        ]
        .spacing(8),
    ]
    .spacing(6)
    .padding(iced::Padding {
        top: 2.0,
        bottom: 4.0,
        left: 12.0,
        right: 0.0,
    })
    .into()
}

fn revoke_confirm<'a>(busy: bool, cancel: Message, confirm: Message) -> ThemedElement<'a, Message> {
    row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(11))
            .style(builtins::button::secondary)
            .on_press(cancel),
        button(
            text(if busy {
                crate::i18n::t!("mapper-revoking")
            } else {
                crate::i18n::t!("mapper-revoke")
            })
            .size(11)
        )
        .style(builtins::button::primary)
        .on_press_maybe((!busy).then_some(confirm)),
    ]
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u128, name: &str, builtin: Option<&str>) -> ClanGroup {
        ClanGroup {
            id: Uuid::from_u128(id),
            name: name.to_string(),
            color: None,
            builtin: builtin.map(ToString::to_string),
            is_member: true,
            created_by_me: false,
            actions: BTreeSet::new(),
        }
    }

    fn dialog(groups: Vec<ClanGroup>) -> ClanShareDialog {
        ClanShareDialog {
            clan_id: Uuid::from_u128(1),
            folder: (AtlasId(Uuid::from_u128(2)), "Roads".to_string()),
            groups: Some(Ok(groups)),
            filter: String::new(),
            selected: HashSet::new(),
            actions: Preset::MapReader.action_set(),
            submitting: false,
            results: Vec::new(),
            close_pending: false,
            grants: Some(Ok(Vec::new())),
            editing: None,
            revoking: None,
            revoke_busy: false,
            manage_error: None,
        }
    }

    fn grant(id: u128, group: u128, actions: &[&str], folders: &[u128]) -> ClanGrant {
        ClanGrant {
            id: Uuid::from_u128(id),
            clan_id: Uuid::from_u128(1),
            recipient: GrantRecipient::Group {
                group_id: Uuid::from_u128(group),
            },
            actions: actions.iter().map(ToString::to_string).collect(),
            may_grant: None,
            scope: GrantScope::Atlases {
                ids: folders
                    .iter()
                    .map(|folder| AtlasId(Uuid::from_u128(*folder)))
                    .collect(),
            },
            delegated: Vec::new(),
            issuer_id: Uuid::from_u128(5),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn recipients_are_everyone_then_the_clans_groups_without_owners() {
        let dialog = dialog(vec![
            group(10, "Owner", Some("owners")),
            group(11, "All clan members", Some("members")),
            group(12, "City mappers", None),
        ]);
        let recipients = dialog.recipients();
        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].0, Uuid::from_u128(11));
        assert_eq!(recipients[0].1, crate::i18n::t!("clan-maps-everyone"));
        assert_eq!(recipients[1].1, "City mappers");
    }

    #[test]
    fn a_group_holding_any_grant_over_the_folder_is_edited_not_picked_again() {
        let mut dialog = dialog(vec![group(11, "All clan members", Some("members"))]);
        dialog.selected.insert(Uuid::from_u128(11));
        assert_eq!(dialog.picked().len(), 1);
        // Folder upkeep alone, with no map access, is still its grant here.
        dialog.grants = Some(Ok(vec![grant(30, 11, &["atlas.rename"], &[2])]));
        assert!(dialog.has_access(Uuid::from_u128(11)));
        assert!(dialog.picked().is_empty());
    }

    #[test]
    fn a_grant_naming_several_folders_is_shown_but_not_this_folders_own() {
        let mut dialog = dialog(vec![group(11, "All clan members", Some("members"))]);
        dialog.grants = Some(Ok(vec![grant(31, 11, &["area.read"], &[2, 3])]));
        assert!(!dialog.has_access(Uuid::from_u128(11)));
        assert_eq!(dialog.wider_grants().len(), 1);
    }

    #[test]
    fn a_preset_sets_the_map_boxes_and_leaves_the_rest() {
        let mut actions: BTreeSet<String> = ["area.read", "area.copy"]
            .iter()
            .map(ToString::to_string)
            .collect();
        apply(&mut actions, PresetPick::Preset(Preset::MapEditor));
        assert_eq!(
            actions,
            [
                "area.read",
                "area.add",
                "area.edit",
                "area.remove_content",
                "area.copy"
            ]
            .iter()
            .map(ToString::to_string)
            .collect()
        );
        assert_eq!(
            picker(&actions).state,
            presets::FacetState::Preset(Preset::MapEditor)
        );
        toggle(&mut actions, "area.edit", false);
        assert_eq!(picker(&actions).state, presets::FacetState::Custom);
    }
}
