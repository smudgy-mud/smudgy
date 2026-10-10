//! Sharing a clan folder, or every map in a clan, with the clan's groups.
//!
//! A clan folder's Share… (or the clan header's, for every map in the clan)
//! gives the clan's groups access in the same dialog as a folder share:
//! recipients are Everyone and the clan's own groups, given one of the map
//! presets (Reader, Contributor, Editor). It reaches the clan's Clan-owned
//! maps; a Member-owned map is shared one map at a time by its owners.

use std::collections::HashSet;
use std::time::Duration;

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{Column, button, checkbox, column, pick_list, row, space, text};
use smudgy_cloud::clans::{ClanGrant, ClanGrantFilter, ClanGroup, GrantRecipient, GrantScope};
use smudgy_cloud::{AtlasId, CloudError, Uuid};

use crate::components::cloud_errors::display_error;
use crate::presets::{self, Kind, Preset};
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
// A clan folder's, or the whole clan's, Share dialog
// ===========================================================================

/// State of a clan folder's or clan's Share dialog.
#[derive(Debug, Clone)]
pub struct ClanShareDialog {
    pub clan_id: Uuid,
    pub clan_name: String,
    /// The folder shared; `None` shares every map in the clan.
    pub folder: Option<(AtlasId, String)>,
    /// `None` while loading.
    pub groups: Option<Result<Vec<ClanGroup>, String>>,
    pub filter: String,
    pub selected: HashSet<Uuid>,
    /// What the picked groups are given: a map preset.
    pub preset: Preset,
    pub submitting: bool,
    pub results: Vec<(String, Result<(), CloudError>)>,
    pub close_pending: bool,
    /// Grants to groups that reach the shared folder or clan.
    pub grants: Option<Result<Vec<ClanGrant>, String>>,
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
    PresetPicked(Preset),
    Submit,
    Submitted(Vec<(String, Result<(), CloudError>)>),
    CloseTick,
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult(Result<(), CloudError>),
}

fn share(message: ClanShareMessage) -> Message {
    Message::Clan(ClanMessage::Share(message))
}

impl ClanShareDialog {
    fn scope(&self) -> GrantScope {
        match &self.folder {
            Some((atlas_id, _)) => GrantScope::Atlases {
                ids: vec![*atlas_id],
            },
            None => GrantScope::Clan,
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

    /// The grants of this dialog's scope to groups that give map access.
    fn shown_grants(&self) -> Vec<&ClanGrant> {
        let Some(Ok(grants)) = &self.grants else {
            return Vec::new();
        };
        let scope = self.scope();
        grants
            .iter()
            .filter(|grant| grant.recipient.group().is_some() && grant.gives_map_access())
            .filter(|grant| match (&scope, &grant.scope) {
                (GrantScope::Clan, GrantScope::Clan) => true,
                (GrantScope::Atlases { ids: wanted }, GrantScope::Atlases { ids }) => {
                    wanted.iter().all(|id| ids.contains(id))
                }
                _ => false,
            })
            .collect()
    }

    /// Whether a group already holds a grant here: sharing again would add a
    /// second one, so changes go through Who has access.
    fn has_access(&self, group_id: Uuid) -> bool {
        self.shown_grants()
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
            ClanModal::Share(dialog) => Some(dialog),
            _ => None,
        },
        _ => None,
    }
}

fn fetch_grants(window: &MapEditorWindow, clan_id: Uuid, folder: Option<AtlasId>) -> Task<Message> {
    let client = window.cloud.client.clone();
    let filter = ClanGrantFilter {
        atlas_id: folder,
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
    let folder = Some(atlas_id).map(|atlas_id| {
        let name = window
            .atlases
            .iter()
            .find(|atlas| atlas.id == atlas_id)
            .map_or_else(
                || crate::i18n::t!("mapper-this-folder"),
                |atlas| atlas.name.clone(),
            );
        (atlas_id, name)
    });
    let folder_id = folder.as_ref().map(|(id, _)| *id);
    window.modal = Some(modals::Modal::Clan(Box::new(ClanModal::Share(
        ClanShareDialog {
            clan_id,
            clan_name: window.clans.name(clan_id).unwrap_or_default(),
            folder,
            groups: None,
            filter: String::new(),
            selected: HashSet::new(),
            preset: Preset::MapReader,
            submitting: false,
            results: Vec::new(),
            close_pending: false,
            grants: None,
            revoking: None,
            revoke_busy: false,
            manage_error: None,
        },
    ))));
    let client = window.cloud.client.clone();
    Update::with_task(Task::batch([
        Task::perform(async move { client.clan_groups(clan_id).await }, |result| {
            share(ClanShareMessage::GroupsLoaded(result))
        }),
        fetch_grants(window, clan_id, folder_id),
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
    match message {
        ClanShareMessage::GroupsLoaded(result) => {
            dialog.groups = Some(result.map_err(|error| display_error(&error)));
            Update::none()
        }
        ClanShareMessage::GrantsLoaded(result) => {
            match result {
                Ok(grants) => {
                    if dialog
                        .revoking
                        .is_some_and(|id| !grants.iter().any(|grant| grant.id == id))
                    {
                        dialog.revoking = None;
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
        ClanShareMessage::PresetPicked(preset) => {
            if preset.kind() == Kind::Map {
                dialog.preset = preset;
            }
            Update::none()
        }
        ClanShareMessage::Submit => {
            let picked = dialog.picked();
            if dialog.submitting || picked.is_empty() {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.results.clear();
            dialog.close_pending = false;
            let clan_id = dialog.clan_id;
            let scope = dialog.scope();
            let access = dialog.preset;
            Update::with_task(Task::perform(
                async move {
                    let mut results = Vec::with_capacity(picked.len());
                    for (label, group_id) in picked {
                        let result = client
                            .create_clan_grant(
                                clan_id,
                                GrantRecipient::Group { group_id },
                                &scope,
                                access.actions(),
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
            let (clan_id, folder) = (dialog.clan_id, dialog.folder.as_ref().map(|(id, _)| *id));
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
        ClanShareMessage::RevokeRequested(id) => {
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
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
            let clan_id = dialog.clan_id;
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
            let (clan_id, folder) = (dialog.clan_id, dialog.folder.as_ref().map(|(id, _)| *id));
            window.mapper.sync_now();
            Update::with_task(fetch_grants(window, clan_id, folder))
        }
    }
}

/// The dialog's title and body, laid out as a folder share.
pub(super) fn view(dialog: &ClanShareDialog) -> (String, ThemedElement<'_, Message>) {
    let title = match &dialog.folder {
        Some((_, name)) => crate::i18n::t!("mapper-share-folder-title", "name" => name),
        None => crate::i18n::t!("clan-maps-share-clan-title", "clan" => &dialog.clan_name),
    };
    let help = match &dialog.folder {
        Some(_) => crate::i18n::t!("clan-maps-share-folder-help"),
        None => crate::i18n::t!("clan-maps-share-clan-help"),
    };

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

    let they_can = column![
        modals::section_label(crate::i18n::t!("mapper-they-can")),
        pick_list(Kind::Map.presets(), Some(dialog.preset), |preset| {
            share(ClanShareMessage::PresetPicked(preset))
        })
        .text_size(13),
    ]
    .spacing(6)
    .into();

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
    let enabled = !dialog.submitting && !dialog.picked().is_empty();
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

/// Who has access: each group holding a grant here, Read or Edit, with
/// Revoke.
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
            let grants = dialog.shown_grants();
            if grants.is_empty() {
                section = section.push(
                    text(crate::i18n::t!("mapper-not-shared"))
                        .size(12)
                        .style(muted),
                );
            }
            for grant in grants {
                let badge = presets::chips(grant.actions.iter().map(String::as_str))
                    .into_iter()
                    .map(presets::Chip::label)
                    .collect::<Vec<_>>()
                    .join(" · ");
                let label = grant.recipient.group().map_or_else(
                    || crate::i18n::t!("clan-maps-a-group"),
                    |group| dialog.group_label(group),
                );
                section = section.push(
                    row![
                        text(label).size(13),
                        text(badge).size(11).style(muted),
                        space::horizontal(),
                        button(text(crate::i18n::t!("mapper-revoke")).size(11))
                            .style(builtins::button::secondary)
                            .on_press(share(ClanShareMessage::RevokeRequested(grant.id))),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                );
                if dialog.revoking == Some(grant.id) {
                    section = section.push(revoke_confirm(
                        dialog.revoke_busy,
                        share(ClanShareMessage::RevokeCancelled),
                        share(ClanShareMessage::RevokeConfirmed),
                    ));
                }
            }
        }
    }
    section.into()
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
            actions: std::collections::BTreeSet::new(),
        }
    }

    fn dialog(groups: Vec<ClanGroup>) -> ClanShareDialog {
        ClanShareDialog {
            clan_id: Uuid::from_u128(1),
            clan_name: "Lantern Company".to_string(),
            folder: Some((AtlasId(Uuid::from_u128(2)), "Roads".to_string())),
            groups: Some(Ok(groups)),
            filter: String::new(),
            selected: HashSet::new(),
            preset: Preset::MapReader,
            submitting: false,
            results: Vec::new(),
            close_pending: false,
            grants: Some(Ok(Vec::new())),
            revoking: None,
            revoke_busy: false,
            manage_error: None,
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
    fn a_group_holding_a_grant_here_is_not_picked_again() {
        let mut dialog = dialog(vec![group(11, "All clan members", Some("members"))]);
        dialog.selected.insert(Uuid::from_u128(11));
        assert_eq!(dialog.picked().len(), 1);
        dialog.grants = Some(Ok(vec![ClanGrant {
            id: Uuid::from_u128(30),
            clan_id: Uuid::from_u128(1),
            recipient: GrantRecipient::Group {
                group_id: Uuid::from_u128(11),
            },
            actions: ["area.read".to_string()].into(),
            may_grant: None,
            scope: GrantScope::Atlases {
                ids: vec![AtlasId(Uuid::from_u128(2))],
            },
            issuer_id: Uuid::from_u128(5),
            parent_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }]));
        assert!(dialog.has_access(Uuid::from_u128(11)));
        assert!(dialog.picked().is_empty());
    }
}
