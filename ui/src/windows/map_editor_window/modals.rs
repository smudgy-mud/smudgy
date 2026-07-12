//! Window-local modals: create-area naming, delete-area confirmation (the
//! only destructive action without undo), the owner-only secrets audit, and
//! the share dialog (create grants, preview the recipient's view, and manage
//! the existing grant tree).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use iced::alignment::Vertical;
use iced::widget::{
    Column, button, checkbox, column, container, radio, row, scrollable, space, text, text_input,
};
use iced::{Length, Task};
use smudgy_cloud::cloud_api::{
    CreateShareRequest, FriendView, GrantTreeNode, PreviewAudience, SecretEntity,
    SecretEntityKind, ShareDirection, ShareGrant, ShareGrantRow, SharePatch, ShareScope,
};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{
    AreaId, AreaWithDetails, AtlasId, ExitId, LabelId, CloudError, Mapper, RoomNumber, ShapeId, Uuid,
};

use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::{MapEditorWindow, Message};

#[derive(Debug, Clone)]
pub enum Modal {
    CreateArea {
        name: String,
        error: Option<String>,
        /// The folder the new area should be filed into (`None` = loose). Set
        /// when opened from a folder's "new map" affordance.
        atlas_id: Option<AtlasId>,
    },
    ConfirmDeleteArea {
        area_id: AreaId,
        name: String,
        room_count: usize,
    },
    /// Name a new folder (atlas) and pick its tier.
    CreateAtlas {
        name: String,
        error: Option<String>,
        /// Chosen tier: `true` = local (on this device), `false` = cloud
        /// (synced, shareable).
        local: bool,
        /// Whether the cloud tier is selectable (i.e. signed in). When false
        /// the folder is forced local.
        cloud_available: bool,
    },
    /// Gentle-delete confirmation for a folder: its maps survive as Loose.
    ConfirmDeleteAtlas {
        atlas_id: AtlasId,
        name: String,
        area_count: i64,
    },
    /// "Move to folder" picker for an owned area.
    MoveArea {
        area_id: AreaId,
        area_name: String,
        current_atlas: Option<AtlasId>,
        /// Available folders (id, name), name-sorted, built at open time.
        folders: Vec<(AtlasId, String)>,
    },
    /// The "Share folder…" dialog: atlas-scope grants in one step.
    ShareAtlas(ShareAtlasDialog),
    /// Owner-only flat list of every secret-marked entity in the area, with
    /// jump-to and per-row unmark. `entries: None` means still loading.
    SecretsAudit {
        area_id: AreaId,
        entries: Option<Vec<SecretEntity>>,
        error: Option<String>,
    },
    /// The share dialog: create new grants for the active area (or its
    /// atlas) and manage the grant tree the viewer is allowed to see.
    Share(ShareDialog),
    /// "Copy to my maps": clone a shared area (and optionally its whole
    /// atlas, when the atlas is visible) into the viewer's own maps.
    CopyArea(CopyAreaDialog),
    /// Offer to transfer ownership of an area or atlas to a friend.
    TransferOffer(TransferDialog),
}

/// State of the copy-to-my-maps modal.
#[derive(Debug, Clone)]
pub struct CopyAreaDialog {
    pub source: AreaId,
    pub source_name: String,
    /// Editable name for the clone, prefilled "<source name> (copy)".
    pub name: String,
    /// The source's atlas when visible to the viewer (rare: shared rows
    /// usually have a redacted `atlas_id`); enables "Copy whole atlas…".
    pub atlas_id: Option<AtlasId>,
    /// A copy request is in flight.
    pub busy: bool,
    pub error: Option<String>,
    /// Human-readable report from a whole-atlas copy.
    pub atlas_report: Option<String>,
    /// This is an owner self-copy ("Duplicate"): the dialog reads "Duplicate
    /// map", offers no atlas option, and the resulting clone starts inactive.
    pub duplicate: bool,
}

// ===========================================================================
// Share dialog state
// ===========================================================================

/// Which capability flag a toggle message refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantFlag {
    Edit,
    Reshare,
    Copy,
    Secrets,
    /// Full-deputy. Owner-minted only; implies all lower caps.
    Admin,
}

/// Counts derived from the owner's secrets audit, for the honesty warning.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretCounts {
    pub rooms: usize,
    pub exits: usize,
    /// Notes (room/area properties), labels, and shapes combined.
    pub other: usize,
}

impl SecretCounts {
    pub fn total(self) -> usize {
        self.rooms + self.exits + self.other
    }
}

/// A digest of `GET /areas/{id}/preview` small enough to live in a message.
#[derive(Debug, Clone)]
pub struct PreviewSummary {
    /// Human description of the simulated audience.
    pub audience: String,
    /// The area name as the recipient sees it.
    pub name: String,
    pub rooms: usize,
    pub exits: usize,
    pub labels: usize,
    pub shapes: usize,
    pub properties: usize,
    /// Names of linked areas the recipient can resolve.
    pub linked_visible: Vec<String>,
    /// Linked areas that render as "Unknown map" for the recipient.
    pub linked_unknown: usize,
}

#[derive(Debug, Clone)]
pub enum PreviewState {
    NotRequested,
    Loading,
    /// The audience cannot see the area at all (`200` + `data: null`).
    Nothing(String),
    Loaded(PreviewSummary),
    Error(String),
}

/// Inline flag editing for one existing grant row.
#[derive(Debug, Clone)]
pub struct GrantEdit {
    pub id: Uuid,
    /// The flags as the server last reported them, for change detection.
    pub original: ShareGrant,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    /// Full-deputy flag on this grant.
    pub can_admin: bool,
    /// Whether `include_secrets` may be raised here (root grants only —
    /// area and atlas roots both qualify).
    pub allow_secrets: bool,
    /// Whether `can_admin` may be changed here (the true owner, owner-minted
    /// root only).
    pub allow_admin: bool,
    pub saving: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ShareDialog {
    pub area_id: AreaId,
    pub area_name: String,
    /// The viewer owns the area (controls atlas scope, secrets, preview).
    pub is_owner: bool,
    /// Owner attribution for grants not made by the viewer (re-share case).
    pub owner_nickname: Option<String>,
    pub viewer_id: Option<Uuid>,
    /// `Some` only when atlas-scope sharing is allowed: the area has a
    /// non-redacted atlas id AND the viewer owns the area.
    pub atlas_id: Option<AtlasId>,
    pub scope_atlas: bool,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub filter: String,
    pub selected: HashSet<Uuid>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    /// Grant full-deputy (`can_admin`) to the selected recipients. Owner-only.
    pub can_admin: bool,
    /// `None` until the owner's audit fetch lands (or forever, for
    /// non-owners — the server uniform-404s and we skip the warning).
    pub secret_counts: Option<SecretCounts>,
    /// The grant tree reaching this area; `None` while loading.
    pub tree: Option<Result<Vec<GrantTreeNode>, String>>,
    /// Selected row in the manage tree; used as the preview audience.
    pub selected_grant: Option<Uuid>,
    pub editing: Option<GrantEdit>,
    /// Grant pending two-step revoke confirmation.
    pub revoking: Option<Uuid>,
    pub revoke_busy: bool,
    pub preview: PreviewState,
    pub submitting: bool,
    /// Per-recipient outcomes of the last [Share] press.
    pub results: Vec<(String, Result<(), CloudError>)>,
    /// All shares succeeded; the dialog closes itself after a beat.
    pub close_pending: bool,
    /// Errors from manage-tree operations (revoke, refresh).
    pub manage_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ShareMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    SecretsLoaded(Result<Vec<SecretEntity>, CloudError>),
    TreeLoaded(Result<Vec<GrantTreeNode>, CloudError>),
    ScopeAtlasChanged(bool),
    FilterChanged(String),
    RecipientToggled(Uuid, bool),
    FlagToggled(GrantFlag, bool),
    /// Close the dialog and open the secrets audit instead.
    ReviewSecrets,
    PreviewRequested,
    PreviewLoaded(Result<Option<PreviewSummary>, CloudError>),
    Submit,
    Submitted(Vec<(String, Result<(), CloudError>)>),
    /// Fires a beat after a fully successful share to close the dialog.
    CloseTick,
    GrantRowPressed(Uuid),
    EditGrant(Uuid),
    EditFlagToggled(GrantFlag, bool),
    EditCancelled,
    EditSaved,
    EditResult {
        id: Uuid,
        result: Result<ShareGrant, CloudError>,
    },
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult {
        id: Uuid,
        result: Result<(), CloudError>,
    },
}

fn share(message: ShareMessage) -> Message {
    Message::Share(message)
}

// ===========================================================================
// Share-folder (atlas-scope) dialog
// ===========================================================================

/// State of the "Share folder…" dialog. Deliberately simpler than the
/// area [`ShareDialog`]: atlas grants never carry secrets and have no
/// per-recipient preview, so this is just recipients + capabilities, plus a
/// "who has access" list of the existing atlas-scope grants.
#[derive(Debug, Clone)]
pub struct ShareAtlasDialog {
    pub atlas_id: AtlasId,
    pub atlas_name: String,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub filter: String,
    pub selected: HashSet<Uuid>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    /// Grant full-deputy (`can_admin`) on the whole folder. Owner-only.
    pub can_admin: bool,
    /// Include secrets on the atlas-scope grant (owner-only).
    pub include_secrets: bool,
    pub submitting: bool,
    /// Per-recipient outcomes of the last Share press.
    pub results: Vec<(String, Result<(), CloudError>)>,
    /// All shares succeeded; the dialog closes itself after a beat.
    pub close_pending: bool,
    /// Existing atlas-scope grants for this folder (from
    /// `GET /shares?direction=given`, filtered by `atlas_id`). `None` while
    /// loading.
    pub grants: Option<Result<Vec<ShareGrantRow>, String>>,
    /// Grant pending two-step revoke confirmation.
    pub revoking: Option<Uuid>,
    pub revoke_busy: bool,
    pub manage_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ShareAtlasMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    GrantsLoaded(Result<Vec<ShareGrantRow>, CloudError>),
    FilterChanged(String),
    RecipientToggled(Uuid, bool),
    FlagToggled(GrantFlag, bool),
    Submit,
    Submitted(Vec<(String, Result<(), CloudError>)>),
    CloseTick,
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult(Result<(), CloudError>),
}

fn share_atlas(message: ShareAtlasMessage) -> Message {
    Message::ShareAtlas(message)
}

// ===========================================================================
// Ownership-transfer offer dialog
// ===========================================================================

#[derive(Debug, Clone)]
pub enum TransferSubject {
    Area(AreaId, String),
    Atlas(AtlasId, String),
}

impl TransferSubject {
    fn name(&self) -> &str {
        match self {
            Self::Area(_, name) | Self::Atlas(_, name) => name,
        }
    }
}

/// Offer to transfer ownership of a map or folder to a friend. Single recipient;
/// the recipient completes it from their Friends panel (offers are non-expiring).
#[derive(Debug, Clone)]
pub struct TransferDialog {
    pub subject: TransferSubject,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub filter: String,
    pub selected: Option<Uuid>,
    pub submitting: bool,
    pub error: Option<String>,
    /// The offer was sent; the dialog closes itself after a beat.
    pub sent: bool,
}

#[derive(Debug, Clone)]
pub enum TransferMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    FilterChanged(String),
    RecipientSelected(Uuid),
    Submit,
    Submitted(Result<(), CloudError>),
    CloseTick,
}

fn transfer(message: TransferMessage) -> Message {
    Message::Transfer(message)
}

/// Open the transfer-offer modal and load the friends list. The caller gates on
/// raw ownership — transfer is `is_owner`-only (a `can_admin` deputy can't).
pub(super) fn open_transfer_dialog(
    window: &mut MapEditorWindow,
    subject: TransferSubject,
) -> Update<Message, super::Event> {
    window.modal = Some(Modal::TransferOffer(TransferDialog {
        subject,
        friends: None,
        filter: String::new(),
        selected: None,
        submitting: false,
        error: None,
        sent: false,
    }));
    let client = window.cloud.client.clone();
    Update::with_task(Task::perform(
        async move { client.friends().await },
        |result| transfer(TransferMessage::FriendsLoaded(result)),
    ))
}

pub(super) fn update_transfer(
    window: &mut MapEditorWindow,
    message: TransferMessage,
) -> Update<Message, super::Event> {
    let Some(Modal::TransferOffer(dialog)) = &mut window.modal else {
        return Update::none();
    };
    match message {
        TransferMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| error.to_string()));
            Update::none()
        }
        TransferMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        TransferMessage::RecipientSelected(user_id) => {
            dialog.selected = Some(user_id);
            Update::none()
        }
        TransferMessage::Submit => {
            let Some(to_user) = dialog.selected else {
                return Update::none();
            };
            if dialog.submitting {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.error = None;
            let client = window.cloud.client.clone();
            let subject = dialog.subject.clone();
            Update::with_task(Task::perform(
                async move {
                    match subject {
                        TransferSubject::Area(area_id, _) => {
                            client.offer_area_transfer(area_id, to_user).await.map(|_| ())
                        }
                        TransferSubject::Atlas(atlas_id, _) => {
                            client.offer_atlas_transfer(atlas_id, to_user).await.map(|_| ())
                        }
                    }
                },
                |result| transfer(TransferMessage::Submitted(result)),
            ))
        }
        TransferMessage::Submitted(result) => match result {
            Ok(()) => {
                dialog.sent = true;
                Update::with_task(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1600)).await },
                    |()| transfer(TransferMessage::CloseTick),
                ))
            }
            Err(error) => {
                dialog.submitting = false;
                dialog.error = Some(transfer_error_message(&error));
                Update::none()
            }
        },
        TransferMessage::CloseTick => {
            if matches!(&window.modal, Some(Modal::TransferOffer(d)) if d.sent) {
                window.modal = None;
            }
            Update::none()
        }
    }
}

fn transfer_error_message(error: &CloudError) -> String {
    match error {
        CloudError::NotFoundOrNoAccess => {
            "You can only transfer something you own, to a current friend.".to_string()
        }
        other => other.to_string(),
    }
}

fn transfer_offer_view(dialog: &TransferDialog) -> ThemedElement<'_, Message> {
    if dialog.sent {
        return column![text(format!(
            "Offer sent. \u{201c}{}\u{201d} transfers when they accept it from their Friends panel.",
            dialog.subject.name()
        ))
        .size(13),]
        .spacing(10)
        .into();
    }

    let leaving_folder = matches!(dialog.subject, TransferSubject::Area(..));
    let mut body = column![
        text(format!("Give \u{201c}{}\u{201d} to a friend.", dialog.subject.name())).size(13),
        text(
            "Once they accept, they own it. You keep admin rights \u{2014} but they can revoke \
             them, and only they can transfer it again or appoint admins."
        )
        .size(12)
        .style(builtins::text::danger),
    ]
    .spacing(8);

    if leaving_folder {
        body = body.push(
            text("If it's in a folder, it leaves the folder when accepted.")
                .size(11)
                .style(muted),
        );
    }

    body = body.push(section_label("Transfer to"));
    match &dialog.friends {
        None => body = body.push(text("Loading friends\u{2026}").size(12).style(muted)),
        Some(Err(error)) => {
            body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() => {
            body = body.push(
                text("No friends yet \u{2014} add a friend before transferring.")
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(friends)) => {
            body = body.push(
                text_input("filter\u{2026}", &dialog.filter)
                    .size(13)
                    .on_input(|value| transfer(TransferMessage::FilterChanged(value))),
            );
            let filter = dialog.filter.trim().to_lowercase();
            let mut list = column![].spacing(4);
            for friend in friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                let user_id = friend.user_id;
                let style = if dialog.selected == Some(user_id) {
                    builtins::button::primary
                } else {
                    builtins::button::secondary
                };
                list = list.push(
                    button(text(label).size(13))
                        .style(style)
                        .width(Length::Fill)
                        .on_press(transfer(TransferMessage::RecipientSelected(user_id))),
                );
            }
            body = body.push(list);
        }
    }

    if let Some(error) = &dialog.error {
        body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    let can_submit = dialog.selected.is_some() && !dialog.submitting;
    body = body.push(
        row![
            space::horizontal(),
            button(text("Cancel").size(13))
                .style(builtins::button::secondary)
                .on_press(Message::ModalDismissed),
            button(text(if dialog.submitting { "Sending\u{2026}" } else { "Send offer" }).size(13))
                .style(builtins::button::primary)
                .on_press_maybe(can_submit.then_some(transfer(TransferMessage::Submit))),
        ]
        .spacing(10)
        .align_y(Vertical::Center),
    );

    body.into()
}

/// Opens the share-folder dialog pre-scoped to `atlas_id` and kicks off the
/// friends + existing-grants fetches.
pub(super) fn open_share_atlas_dialog(
    window: &mut MapEditorWindow,
    atlas_id: AtlasId,
) -> Update<Message, super::Event> {
    let atlas_name = window
        .atlases
        .iter()
        .find(|atlas| atlas.id == atlas_id)
        .map(|atlas| atlas.name.clone())
        .unwrap_or_else(|| "this folder".to_string());

    window.modal = Some(Modal::ShareAtlas(ShareAtlasDialog {
        atlas_id,
        atlas_name,
        friends: None,
        filter: String::new(),
        selected: HashSet::new(),
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        include_secrets: false,
        submitting: false,
        results: Vec::new(),
        close_pending: false,
        grants: None,
        revoking: None,
        revoke_busy: false,
        manage_error: None,
    }));

    let friends_client = window.cloud.client.clone();
    Update::with_task(Task::batch([
        Task::perform(async move { friends_client.friends().await }, |result| {
            share_atlas(ShareAtlasMessage::FriendsLoaded(result))
        }),
        fetch_atlas_grants(window),
    ]))
}

/// Fetches the caller's given grants and keeps only those scoped to the
/// dialog's atlas.
fn fetch_atlas_grants(window: &MapEditorWindow) -> Task<Message> {
    let client = window.cloud.client.clone();
    Task::perform(
        async move { client.shares(ShareDirection::Given).await },
        |result| share_atlas(ShareAtlasMessage::GrantsLoaded(result)),
    )
}

/// Routes a share-folder dialog message. No-op unless the share-folder dialog
/// is the open modal (stale async completions are dropped).
#[allow(clippy::too_many_lines)]
pub(super) fn update_share_atlas(
    window: &mut MapEditorWindow,
    message: ShareAtlasMessage,
) -> Update<Message, super::Event> {
    let Some(Modal::ShareAtlas(dialog)) = &mut window.modal else {
        return Update::none();
    };

    match message {
        ShareAtlasMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| error.to_string()));
            Update::none()
        }
        ShareAtlasMessage::GrantsLoaded(result) => {
            match result {
                Ok(rows) => {
                    let atlas_id = dialog.atlas_id;
                    let mine: Vec<ShareGrantRow> = rows
                        .into_iter()
                        .filter(|row| row.grant.atlas_id == Some(atlas_id))
                        .collect();
                    let ids: HashSet<Uuid> = mine.iter().map(|row| row.grant.id).collect();
                    if dialog.revoking.is_some_and(|id| !ids.contains(&id)) {
                        dialog.revoking = None;
                    }
                    dialog.grants = Some(Ok(mine));
                    dialog.manage_error = None;
                }
                Err(error) => {
                    let message = error.to_string();
                    if dialog.grants.is_none() {
                        dialog.grants = Some(Err(message));
                    } else {
                        dialog.manage_error = Some(message);
                    }
                }
            }
            Update::none()
        }
        ShareAtlasMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        ShareAtlasMessage::RecipientToggled(user_id, selected) => {
            if selected {
                dialog.selected.insert(user_id);
            } else {
                dialog.selected.remove(&user_id);
            }
            Update::none()
        }
        ShareAtlasMessage::FlagToggled(flag, value) => {
            match flag {
                GrantFlag::Edit => dialog.can_edit = value,
                GrantFlag::Reshare => dialog.can_reshare = value,
                GrantFlag::Copy => dialog.can_copy = value,
                // Atlas-scope secrets are allowed (root-only, owner-only).
                GrantFlag::Secrets => dialog.include_secrets = value,
                // Owner-minted full-deputy over the whole folder.
                GrantFlag::Admin => dialog.can_admin = value,
            }
            Update::none()
        }
        ShareAtlasMessage::Submit => {
            if dialog.submitting || dialog.selected.is_empty() {
                return Update::none();
            }
            let Some(Ok(friends)) = &dialog.friends else {
                return Update::none();
            };
            let scope = ShareScope::Atlas {
                atlas_id: dialog.atlas_id,
            };
            let requests: Vec<(String, CreateShareRequest)> = friends
                .iter()
                .filter(|friend| dialog.selected.contains(&friend.user_id))
                .map(|friend| {
                    (
                        friend_label(friend),
                        CreateShareRequest {
                            grantee_id: friend.user_id,
                            scope,
                            can_edit: dialog.can_edit,
                            can_reshare: dialog.can_reshare,
                            can_copy: dialog.can_copy,
                            include_secrets: dialog.include_secrets,
                            can_admin: dialog.can_admin,
                        },
                    )
                })
                .collect();
            if requests.is_empty() {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.results = Vec::new();
            dialog.close_pending = false;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move {
                    let mut results = Vec::with_capacity(requests.len());
                    for (label, request) in requests {
                        let result = client.create_share(request).await.map(|_| ());
                        results.push((label, result));
                    }
                    results
                },
                |results| share_atlas(ShareAtlasMessage::Submitted(results)),
            ))
        }
        ShareAtlasMessage::Submitted(results) => {
            dialog.submitting = false;
            let all_ok = !results.is_empty() && results.iter().all(|(_, result)| result.is_ok());
            dialog.results = results;
            let mut tasks = Vec::new();
            if all_ok {
                dialog.close_pending = true;
                tasks.push(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1400)).await },
                    |()| share_atlas(ShareAtlasMessage::CloseTick),
                ));
            }
            // Refresh the access list either way; partial successes changed it.
            tasks.push(fetch_atlas_grants(window));
            Update::with_task(Task::batch(tasks))
        }
        ShareAtlasMessage::CloseTick => {
            if dialog.close_pending {
                window.modal = None;
            }
            Update::none()
        }
        ShareAtlasMessage::RevokeRequested(id) => {
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareAtlasMessage::RevokeCancelled => {
            dialog.revoking = None;
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareAtlasMessage::RevokeConfirmed => {
            let Some(id) = dialog.revoking else {
                return Update::none();
            };
            if dialog.revoke_busy {
                return Update::none();
            }
            dialog.revoke_busy = true;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.revoke_share(id).await },
                |result| share_atlas(ShareAtlasMessage::RevokeResult(result)),
            ))
        }
        ShareAtlasMessage::RevokeResult(result) => {
            dialog.revoke_busy = false;
            dialog.revoking = None;
            match result {
                Ok(()) => fetch_atlas_grants_update(window),
                Err(error) => {
                    if let Some(Modal::ShareAtlas(dialog)) = &mut window.modal {
                        dialog.manage_error = Some(match error {
                            CloudError::NotFoundOrNoAccess => {
                                "Couldn't revoke — the grant may already be gone.".to_string()
                            }
                            other => other.to_string(),
                        });
                    }
                    Update::none()
                }
            }
        }
    }
}

/// Helper: refetch the atlas grants as an `Update` (used after a successful
/// revoke, where `window.modal` is reborrowed).
fn fetch_atlas_grants_update(window: &MapEditorWindow) -> Update<Message, super::Event> {
    Update::with_task(fetch_atlas_grants(window))
}

/// Builds the dialog for the active area and kicks off the friends, grant
/// tree, and (owner-only) secret-count fetches. No-op when the viewer may
/// not share the area.
pub(super) fn open_share_dialog(window: &mut MapEditorWindow) -> Update<Message, super::Event> {
    let Some(area_id) = window.editor.area_id() else {
        return Update::none();
    };
    let atlas = window.mapper.get_current_atlas();
    let Some(area) = atlas.get_area(&area_id) else {
        return Update::none();
    };
    let access = area.effective_access();
    if !(access.is_owner || access.can_reshare) {
        return Update::none();
    }

    let is_owner = access.is_owner;
    window.modal = Some(Modal::Share(ShareDialog {
        area_id,
        area_name: area.get_name().to_string(),
        is_owner,
        owner_nickname: area.meta().owner_nickname.clone(),
        viewer_id: window
            .cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .map(|profile| profile.id),
        // Non-owners re-share area-scope only; a redacted atlas_id also
        // disables atlas scope.
        atlas_id: if is_owner { area.meta().atlas_id } else { None },
        scope_atlas: false,
        friends: None,
        filter: String::new(),
        selected: HashSet::new(),
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        include_secrets: false,
        secret_counts: None,
        tree: None,
        selected_grant: None,
        editing: None,
        revoking: None,
        revoke_busy: false,
        preview: PreviewState::NotRequested,
        submitting: false,
        results: Vec::new(),
        close_pending: false,
        manage_error: None,
    }));

    let friends_client = window.cloud.client.clone();
    let mut tasks = vec![
        Task::perform(async move { friends_client.friends().await }, |result| {
            share(ShareMessage::FriendsLoaded(result))
        }),
        fetch_tree(window, area_id),
    ];
    if is_owner {
        let secrets_client = window.cloud.client.clone();
        tasks.push(Task::perform(
            async move { secrets_client.area_secrets(area_id).await },
            |result| share(ShareMessage::SecretsLoaded(result)),
        ));
    }
    Update::with_task(Task::batch(tasks))
}

fn fetch_tree(window: &MapEditorWindow, area_id: AreaId) -> Task<Message> {
    let client = window.cloud.client.clone();
    Task::perform(async move { client.area_shares(area_id).await }, |result| {
        share(ShareMessage::TreeLoaded(result))
    })
}

fn count_secrets(entries: &[SecretEntity]) -> SecretCounts {
    let mut counts = SecretCounts::default();
    for entity in entries {
        match entity.kind {
            SecretEntityKind::Room => counts.rooms += 1,
            SecretEntityKind::Exit => counts.exits += 1,
            SecretEntityKind::Label
            | SecretEntityKind::Shape
            | SecretEntityKind::RoomProperty
            | SecretEntityKind::AreaProperty => counts.other += 1,
        }
    }
    counts
}

fn summarize_preview(details: &AreaWithDetails, audience: &str) -> PreviewSummary {
    PreviewSummary {
        audience: audience.to_string(),
        name: details.area.name.clone(),
        rooms: details.rooms.len(),
        exits: details.rooms.iter().map(|room| room.exits.len()).sum(),
        labels: details.labels.len(),
        shapes: details.shapes.len(),
        properties: details.properties.len(),
        linked_visible: details
            .linked_areas
            .iter()
            .filter(|linked| linked.visible)
            .map(|linked| {
                linked
                    .name
                    .clone()
                    .unwrap_or_else(|| "(unnamed area)".to_string())
            })
            .collect(),
        linked_unknown: details
            .linked_areas
            .iter()
            .filter(|linked| !linked.visible)
            .count(),
    }
}

fn friend_label(friend: &FriendView) -> String {
    friend
        .nickname.clone()
        .unwrap_or_else(|| friend.user_id.to_string())
}

/// Routes a share-dialog message. Everything here is a no-op unless the
/// share dialog is the open modal (stale async completions are dropped).
#[allow(clippy::too_many_lines)]
pub(super) fn update_share(
    window: &mut MapEditorWindow,
    message: ShareMessage,
) -> Update<Message, super::Event> {
    // ReviewSecrets swaps the modal entirely; handle it before borrowing the
    // dialog so we can call window methods.
    if matches!(message, ShareMessage::ReviewSecrets) {
        let Some(Modal::Share(dialog)) = &window.modal else {
            return Update::none();
        };
        let area_id = dialog.area_id;
        window.modal = Some(Modal::SecretsAudit {
            area_id,
            entries: None,
            error: None,
        });
        return Update::with_task(window.fetch_secrets_audit(area_id));
    }

    let Some(Modal::Share(dialog)) = &mut window.modal else {
        return Update::none();
    };

    match message {
        ShareMessage::ReviewSecrets => Update::none(), // handled above
        ShareMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| error.to_string()));
            Update::none()
        }
        ShareMessage::SecretsLoaded(result) => {
            match result {
                Ok(entries) => dialog.secret_counts = Some(count_secrets(&entries)),
                // Uniform 404: not the owner (or area gone) — skip the
                // warning section entirely rather than invent distinctions.
                Err(CloudError::NotFoundOrNoAccess) => {}
                Err(error) => log::warn!("share dialog: secret count fetch failed: {error}"),
            }
            Update::none()
        }
        ShareMessage::TreeLoaded(result) => {
            match result {
                Ok(nodes) => {
                    // Drop UI state pointing at grants that no longer exist.
                    let ids: HashSet<Uuid> = nodes.iter().map(|node| node.grant.id).collect();
                    if dialog
                        .selected_grant
                        .is_some_and(|id| !ids.contains(&id))
                    {
                        dialog.selected_grant = None;
                    }
                    if dialog
                        .editing
                        .as_ref()
                        .is_some_and(|edit| !ids.contains(&edit.id))
                    {
                        dialog.editing = None;
                    }
                    if dialog.revoking.is_some_and(|id| !ids.contains(&id)) {
                        dialog.revoking = None;
                    }
                    dialog.tree = Some(Ok(nodes));
                    dialog.manage_error = None;
                }
                Err(error) => {
                    let message = error.to_string();
                    if dialog.tree.is_none() {
                        dialog.tree = Some(Err(message));
                    } else {
                        dialog.manage_error = Some(message);
                    }
                }
            }
            Update::none()
        }
        ShareMessage::ScopeAtlasChanged(atlas) => {
            dialog.scope_atlas = atlas && dialog.atlas_id.is_some();
            if dialog.scope_atlas {
                // include_secrets is area-share-only.
                dialog.include_secrets = false;
            }
            Update::none()
        }
        ShareMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        ShareMessage::RecipientToggled(user_id, selected) => {
            if selected {
                dialog.selected.insert(user_id);
            } else {
                dialog.selected.remove(&user_id);
            }
            Update::none()
        }
        ShareMessage::FlagToggled(flag, value) => {
            match flag {
                GrantFlag::Edit => dialog.can_edit = value,
                GrantFlag::Reshare => dialog.can_reshare = value,
                GrantFlag::Copy => dialog.can_copy = value,
                GrantFlag::Secrets => {
                    if dialog.is_owner && !dialog.scope_atlas {
                        dialog.include_secrets = value;
                    }
                }
                // can_admin is owner-minted only.
                GrantFlag::Admin => {
                    if dialog.is_owner {
                        dialog.can_admin = value;
                    }
                }
            }
            Update::none()
        }
        ShareMessage::PreviewRequested => {
            if !dialog.is_owner {
                return Update::none();
            }
            dialog.preview = PreviewState::Loading;
            let area_id = dialog.area_id;
            let (audience, audience_label) = match dialog.selected_grant {
                Some(grant_id) => {
                    let handle = dialog
                        .tree
                        .as_ref()
                        .and_then(|tree| tree.as_ref().ok())
                        .and_then(|nodes| {
                            nodes
                                .iter()
                                .find(|node| node.grant.id == grant_id)
                                .and_then(|node| node.grantee_nickname.clone())
                        })
                        .unwrap_or_else(|| "the selected grant".to_string());
                    (PreviewAudience::Share(grant_id), handle)
                }
                None => (
                    PreviewAudience::WorstCase,
                    "worst case (no grant)".to_string(),
                ),
            };
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.preview(area_id, audience).await },
                move |result| {
                    share(ShareMessage::PreviewLoaded(result.map(|details| {
                        details
                            .map(|details| summarize_preview(&details, &audience_label))
                    })))
                },
            ))
        }
        ShareMessage::PreviewLoaded(result) => {
            dialog.preview = match result {
                Ok(Some(summary)) => PreviewState::Loaded(summary),
                Ok(None) => PreviewState::Nothing(
                    "This audience can't see this area at all.".to_string(),
                ),
                Err(error) => PreviewState::Error(error.to_string()),
            };
            Update::none()
        }
        ShareMessage::Submit => {
            if dialog.submitting || dialog.selected.is_empty() {
                return Update::none();
            }
            let Some(Ok(friends)) = &dialog.friends else {
                return Update::none();
            };
            let scope = match (dialog.scope_atlas, dialog.atlas_id) {
                (true, Some(atlas_id)) => ShareScope::Atlas { atlas_id },
                _ => ShareScope::Area {
                    area_id: dialog.area_id,
                },
            };
            let include_secrets = dialog.include_secrets
                && dialog.is_owner
                && matches!(scope, ShareScope::Area { .. });
            // can_admin is owner-minted only.
            let can_admin = dialog.can_admin && dialog.is_owner;
            let requests: Vec<(String, CreateShareRequest)> = friends
                .iter()
                .filter(|friend| dialog.selected.contains(&friend.user_id))
                .map(|friend| {
                    (
                        friend_label(friend),
                        CreateShareRequest {
                            grantee_id: friend.user_id,
                            scope,
                            can_edit: dialog.can_edit,
                            can_reshare: dialog.can_reshare,
                            can_copy: dialog.can_copy,
                            include_secrets,
                            can_admin,
                        },
                    )
                })
                .collect();
            if requests.is_empty() {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.results = Vec::new();
            dialog.close_pending = false;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move {
                    let mut results = Vec::with_capacity(requests.len());
                    for (label, request) in requests {
                        let result = client.create_share(request).await.map(|_| ());
                        results.push((label, result));
                    }
                    results
                },
                |results| share(ShareMessage::Submitted(results)),
            ))
        }
        ShareMessage::Submitted(results) => {
            dialog.submitting = false;
            let all_ok = !results.is_empty() && results.iter().all(|(_, result)| result.is_ok());
            dialog.results = results;
            let area_id = dialog.area_id;
            let mut tasks = Vec::new();
            if all_ok {
                dialog.close_pending = true;
                tasks.push(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1400)).await },
                    |()| share(ShareMessage::CloseTick),
                ));
            }
            // Refresh the manage tree either way; partial successes changed it.
            let client = window.cloud.client.clone();
            tasks.push(Task::perform(
                async move { client.area_shares(area_id).await },
                |result| share(ShareMessage::TreeLoaded(result)),
            ));
            Update::with_task(Task::batch(tasks))
        }
        ShareMessage::CloseTick => {
            if dialog.close_pending {
                window.modal = None;
            }
            Update::none()
        }
        ShareMessage::GrantRowPressed(id) => {
            dialog.selected_grant = if dialog.selected_grant == Some(id) {
                None
            } else {
                Some(id)
            };
            Update::none()
        }
        ShareMessage::EditGrant(id) => {
            let Some(Ok(nodes)) = &dialog.tree else {
                return Update::none();
            };
            let Some(node) = nodes.iter().find(|node| node.grant.id == id) else {
                return Update::none();
            };
            dialog.revoking = None;
            dialog.editing = Some(GrantEdit {
                id,
                original: node.grant.clone(),
                can_edit: node.grant.can_edit,
                can_reshare: node.grant.can_reshare,
                can_copy: node.grant.can_copy,
                include_secrets: node.grant.include_secrets,
                can_admin: node.grant.can_admin,
                // include_secrets is raisable on any ROOT grant (area and atlas
                // roots alike); only the owner may grant it.
                allow_secrets: dialog.is_owner && node.grant.parent_grant_id.is_none(),
                // can_admin: the true owner only, on an owner-minted root.
                allow_admin: dialog.is_owner
                    && node.grant.parent_grant_id.is_none()
                    && node.grant.grantor_id == node.grant.owner_id,
                saving: false,
                error: None,
            });
            Update::none()
        }
        ShareMessage::EditFlagToggled(flag, value) => {
            if let Some(edit) = &mut dialog.editing {
                match flag {
                    GrantFlag::Edit => edit.can_edit = value,
                    GrantFlag::Reshare => edit.can_reshare = value,
                    GrantFlag::Copy => edit.can_copy = value,
                    GrantFlag::Secrets => {
                        if edit.allow_secrets {
                            edit.include_secrets = value;
                        }
                    }
                    GrantFlag::Admin => {
                        if edit.allow_admin {
                            edit.can_admin = value;
                        }
                    }
                }
            }
            Update::none()
        }
        ShareMessage::EditCancelled => {
            dialog.editing = None;
            Update::none()
        }
        ShareMessage::EditSaved => {
            let Some(edit) = &mut dialog.editing else {
                return Update::none();
            };
            if edit.saving {
                return Update::none();
            }
            let patch = SharePatch {
                can_edit: (edit.can_edit != edit.original.can_edit).then_some(edit.can_edit),
                can_reshare: (edit.can_reshare != edit.original.can_reshare)
                    .then_some(edit.can_reshare),
                can_copy: (edit.can_copy != edit.original.can_copy).then_some(edit.can_copy),
                include_secrets: (edit.include_secrets != edit.original.include_secrets)
                    .then_some(edit.include_secrets),
                can_admin: (edit.can_admin != edit.original.can_admin).then_some(edit.can_admin),
            };
            if patch == SharePatch::default() {
                dialog.editing = None;
                return Update::none();
            }
            edit.saving = true;
            edit.error = None;
            let id = edit.id;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.update_share(id, patch).await },
                move |result| share(ShareMessage::EditResult { id, result }),
            ))
        }
        ShareMessage::EditResult { id, result } => {
            let area_id = dialog.area_id;
            match result {
                Ok(_) => {
                    if dialog.editing.as_ref().is_some_and(|edit| edit.id == id) {
                        dialog.editing = None;
                    }
                    // Lowering flags may have clamped or deleted descendant
                    // grants server-side — refetch the whole tree.
                    let client = window.cloud.client.clone();
                    Update::with_task(Task::perform(
                        async move { client.area_shares(area_id).await },
                        |result| share(ShareMessage::TreeLoaded(result)),
                    ))
                }
                Err(error) => {
                    if let Some(edit) = &mut dialog.editing
                        && edit.id == id
                    {
                        edit.saving = false;
                        edit.error = Some(match error {
                            CloudError::NotFoundOrNoAccess => {
                                "Couldn't update — the grant may be gone, or the change isn't allowed.".to_string()
                            }
                            other => other.to_string(),
                        });
                    }
                    Update::none()
                }
            }
        }
        ShareMessage::RevokeRequested(id) => {
            dialog.editing = None;
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareMessage::RevokeCancelled => {
            dialog.revoking = None;
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareMessage::RevokeConfirmed => {
            let Some(id) = dialog.revoking else {
                return Update::none();
            };
            if dialog.revoke_busy {
                return Update::none();
            }
            dialog.revoke_busy = true;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.revoke_share(id).await },
                move |result| share(ShareMessage::RevokeResult { id, result }),
            ))
        }
        ShareMessage::RevokeResult { id, result } => {
            dialog.revoke_busy = false;
            dialog.revoking = None;
            let area_id = dialog.area_id;
            match result {
                Ok(()) => {
                    if dialog.selected_grant == Some(id) {
                        dialog.selected_grant = None;
                    }
                    let client = window.cloud.client.clone();
                    Update::with_task(Task::perform(
                        async move { client.area_shares(area_id).await },
                        |result| share(ShareMessage::TreeLoaded(result)),
                    ))
                }
                Err(error) => {
                    dialog.manage_error = Some(match error {
                        CloudError::NotFoundOrNoAccess => {
                            "Couldn't revoke — the grant may already be gone.".to_string()
                        }
                        other => other.to_string(),
                    });
                    Update::none()
                }
            }
        }
    }
}

/// Display order and group headers for the audit list.
const KIND_GROUPS: [(SecretEntityKind, &str); 6] = [
    (SecretEntityKind::Room, "Rooms"),
    (SecretEntityKind::Exit, "Exits"),
    (SecretEntityKind::Label, "Labels"),
    (SecretEntityKind::Shape, "Shapes"),
    (SecretEntityKind::RoomProperty, "Room properties"),
    (SecretEntityKind::AreaProperty, "Area properties"),
];

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

impl Modal {
    #[allow(clippy::too_many_lines)]
    pub fn view(&self, mapper: &Mapper) -> ThemedElement<'_, Message> {
        let (title, body): (String, ThemedElement<'_, Message>) = match self {
            Modal::CreateArea { name, error, .. } => {
                let mut body = column![
                    text("Name the new area").size(13),
                    text_input("area name", name)
                        .size(14)
                        .on_input(Message::CreateAreaNameChanged)
                        .on_submit(Message::CreateAreaConfirmed),
                ]
                .spacing(10);

                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }

                body = body.push(
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                        button(text("Create").size(13))
                            .style(builtins::button::primary)
                            .on_press_maybe(
                                (!name.trim().is_empty()).then_some(Message::CreateAreaConfirmed)
                            ),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                );

                ("New area".to_string(), body.into())
            }
            Modal::ConfirmDeleteArea {
                name, room_count, ..
            } => {
                let body = column![
                    text(format!(
                        "Delete \u{201c}{name}\u{201d} and its {room_count} rooms?"
                    ))
                    .size(13),
                    text("This cannot be undone.").size(12).style(builtins::text::danger),
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                        button(text("Delete").size(13))
                            .style(builtins::button::primary)
                            .on_press(Message::DeleteAreaConfirmed),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                ]
                .spacing(10);

                ("Delete area".to_string(), body.into())
            }
            Modal::CreateAtlas {
                name,
                error,
                local,
                cloud_available,
            } => {
                let mut body = column![
                    text("Name the new folder").size(13),
                    text_input("folder name", name)
                        .size(14)
                        .on_input(Message::CreateAtlasNameChanged)
                        .on_submit(Message::CreateAtlasConfirmed),
                ]
                .spacing(10);

                // Tier choice: only offered when signed in (cloud needs an
                // account). Signed out, the folder is local — say so.
                if *cloud_available {
                    body = body.push(
                        column![
                            section_label("Save in"),
                            radio(
                                "Cloud \u{2014} synced across devices, shareable",
                                false,
                                Some(*local),
                                Message::CreateAtlasTierChanged,
                            )
                            .size(14)
                            .text_size(13),
                            radio(
                                "On this device \u{2014} local only, never synced",
                                true,
                                Some(*local),
                                Message::CreateAtlasTierChanged,
                            )
                            .size(14)
                            .text_size(13),
                        ]
                        .spacing(4),
                    );
                } else {
                    body = body.push(
                        text(
                            "Saved on this device. Sign in to create cloud folders that \
                             sync across devices and can be shared.",
                        )
                        .size(11)
                        .style(muted),
                    );
                }

                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }

                body = body.push(
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                        button(text("Create").size(13))
                            .style(builtins::button::primary)
                            .on_press_maybe(
                                (!name.trim().is_empty()).then_some(Message::CreateAtlasConfirmed)
                            ),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                );

                ("New folder".to_string(), body.into())
            }
            Modal::ConfirmDeleteAtlas {
                name, area_count, ..
            } => {
                let detail = match area_count {
                    0 => "This folder is empty.".to_string(),
                    1 => "Its 1 map will move to Loose maps.".to_string(),
                    n => format!("Its {n} maps will move to Loose maps."),
                };
                let body = column![
                    text(format!("Delete folder \u{201c}{name}\u{201d}?")).size(13),
                    text(detail).size(12).style(muted),
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                        button(text("Delete folder").size(13))
                            .style(builtins::button::primary)
                            .on_press(Message::DeleteAtlasConfirmed),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                ]
                .spacing(10);

                ("Delete folder".to_string(), body.into())
            }
            Modal::MoveArea {
                area_id,
                area_name,
                current_atlas,
                folders,
            } => {
                let mut list =
                    column![text(format!("Move \u{201c}{area_name}\u{201d} to:")).size(13)]
                        .spacing(6);

                list = list.push(move_target_button(
                    "Loose maps",
                    current_atlas.is_none(),
                    Message::MoveAreaToAtlas {
                        area: *area_id,
                        atlas: None,
                    },
                ));
                for (atlas_id, atlas_name) in folders {
                    list = list.push(move_target_button(
                        atlas_name,
                        *current_atlas == Some(*atlas_id),
                        Message::MoveAreaToAtlas {
                            area: *area_id,
                            atlas: Some(*atlas_id),
                        },
                    ));
                }

                list = list.push(
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                    ]
                    .align_y(Vertical::Center),
                );

                (
                    "Move to folder".to_string(),
                    container(scrollable(list)).max_height(360.0).into(),
                )
            }
            Modal::ShareAtlas(dialog) => (
                format!("Share folder \u{201c}{}\u{201d}", dialog.atlas_name),
                share_atlas_view(dialog),
            ),
            Modal::SecretsAudit {
                area_id,
                entries,
                error,
            } => {
                let area = mapper.get_current_atlas().get_area(area_id);

                let mut body = column![].spacing(10);

                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }

                match entries {
                    None => {
                        body = body.push(text("Loading\u{2026}").size(13));
                    }
                    Some(entries) if entries.is_empty() => {
                        body = body.push(text("No secrets in this area.").size(13));
                    }
                    Some(entries) => {
                        let mut list = Column::new().spacing(4);
                        for (kind, header) in &KIND_GROUPS {
                            let mut group_entries = entries
                                .iter()
                                .filter(|entity| entity.kind == *kind)
                                .peekable();
                            if group_entries.peek().is_none() {
                                continue;
                            }
                            list = list.push(text(*header).size(11).style(muted));
                            for entity in group_entries {
                                list = list.push(
                                    row![
                                        button(
                                            text(entity_label(area.as_ref(), entity)).size(13)
                                        )
                                        .style(builtins::button::list_item)
                                        .on_press(Message::SecretsAuditJump(entity.clone()))
                                        .width(Length::Fill),
                                        button(text("Unmark").size(12))
                                            .style(builtins::button::secondary)
                                            .on_press(Message::SecretsAuditUnmark(
                                                entity.clone()
                                            )),
                                    ]
                                    .spacing(8)
                                    .align_y(Vertical::Center),
                                );
                            }
                        }
                        body = body.push(container(scrollable(list)).max_height(320.0));
                    }
                }

                body = body.push(
                    row![
                        space::horizontal(),
                        button(text("Close").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                );

                ("Secrets in this area".to_string(), body.into())
            }
            Modal::Share(dialog) => (
                format!("Share \u{201c}{}\u{201d}", dialog.area_name),
                share_view(dialog),
            ),
            Modal::CopyArea(dialog) => {
                let intro = if dialog.duplicate {
                    format!(
                        "Makes a second copy of \u{201c}{}\u{201d} that you own \u{2014} useful \
                         for sharing a version with some secrets unmarked.",
                        dialog.source_name
                    )
                } else {
                    format!(
                        "Creates your own editable copy of \u{201c}{}\u{201d} exactly as you \
                         currently see it. Anything not shared with you is not copied.",
                        dialog.source_name
                    )
                };
                let mut body = column![
                    text(intro).size(12),
                    text_input("name for your copy", &dialog.name)
                        .size(14)
                        .on_input(Message::CopyAreaNameChanged)
                        .on_submit(Message::CopyAreaConfirmed),
                ]
                .spacing(10);

                // A duplicate starts inactive; say so up front.
                if dialog.duplicate {
                    body = body.push(
                        text(
                            "The duplicate starts inactive \u{2014} it won't be used to find your \
                             location, so it won't compete with this map. Activate it any time \
                             from the area list.",
                        )
                        .size(11)
                        .style(muted),
                    );
                }

                if let Some(error) = &dialog.error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }
                if let Some(report) = &dialog.atlas_report {
                    body =
                        body.push(text(report.clone()).size(12).style(builtins::text::success));
                }

                // Whole-atlas copy is offered only when the source's atlas
                // id survived projection (viewer holds an atlas-scope grant);
                // never on an owner duplicate.
                if !dialog.duplicate && dialog.atlas_id.is_some() {
                    body = body.push(
                        column![
                            iced::widget::rule::horizontal(1),
                            text("This map belongs to an atlas you can see. You can fork the \
                                  whole atlas instead — every member you're allowed to copy \
                                  comes along, with links between them re-pointed at your \
                                  copies.")
                                .size(11)
                                .style(muted),
                            button(text("Copy whole atlas\u{2026}").size(12))
                                .style(builtins::button::secondary)
                                .on_press_maybe(
                                    (!dialog.busy).then_some(Message::CopyAtlasRequested)
                                ),
                        ]
                        .spacing(6),
                    );
                }

                body = body.push(
                    row![
                        space::horizontal(),
                        button(text("Cancel").size(13))
                            .style(builtins::button::secondary)
                            .on_press(Message::ModalDismissed),
                        button(
                            text(if dialog.busy { "Copying\u{2026}" } else { "Copy" }).size(13)
                        )
                        .style(builtins::button::primary)
                        .on_press_maybe(
                            (!dialog.busy && !dialog.name.trim().is_empty())
                                .then_some(Message::CopyAreaConfirmed)
                        ),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                );

                let title = if dialog.duplicate {
                    "Duplicate map"
                } else {
                    "Copy to my maps"
                };
                (title.to_string(), body.into())
            }
            Modal::TransferOffer(dialog) => (
                format!("Transfer \u{201c}{}\u{201d}", dialog.subject.name()),
                transfer_offer_view(dialog),
            ),
        };

        let width = match self {
            Modal::Share(_) | Modal::ShareAtlas(_) => 600.0,
            Modal::TransferOffer(_) => 460.0,
            _ => 380.0,
        };

        container(column![
            container(
                row![text(title).size(14)]
                    .padding([0, 10])
                    .align_y(Vertical::Center)
                    .height(Length::Fill)
            )
            .style(builtins::container::modal_title_bar)
            .height(34.0)
            .width(Length::Fill),
            container(body)
                .style(builtins::container::modal_body)
                .padding(14)
                .width(Length::Fill),
        ])
        .style(builtins::container::modal_container)
        .width(width)
        .into()
    }
}

// ===========================================================================
// Share dialog view
// ===========================================================================

fn section_label<'a>(label: &'static str) -> iced::widget::Text<'a, crate::Theme> {
    text(label).size(11).style(muted)
}

#[allow(clippy::too_many_lines)]
fn share_view(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut content = Column::new().spacing(12);

    // ===== scope ==========================================================
    let mut scope = column![radio(
        "This area only",
        false,
        Some(dialog.scope_atlas),
        |value| share(ShareMessage::ScopeAtlasChanged(value)),
    )
    .size(14)
    .text_size(13)]
    .spacing(4);
    if dialog.atlas_id.is_some() {
        scope = scope.push(
            radio(
                "Its atlas (covers areas added later)",
                true,
                Some(dialog.scope_atlas),
                |value| share(ShareMessage::ScopeAtlasChanged(value)),
            )
            .size(14)
            .text_size(13),
        );
    }
    content = content.push(column![section_label("Scope"), scope].spacing(4));

    // ===== recipients =====================================================
    let mut recipients = column![
        section_label("Recipients"),
        text_input("filter by handle", &dialog.filter)
            .size(13)
            .on_input(|value| share(ShareMessage::FilterChanged(value))),
    ]
    .spacing(4);

    let mut friend_list = Column::new().spacing(2);
    match &dialog.friends {
        None => {
            friend_list = friend_list.push(text("Loading friends\u{2026}").size(12).style(muted));
        }
        Some(Err(error)) => {
            friend_list =
                friend_list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() => {
            friend_list = friend_list.push(
                text("No friends yet — add friends from the social panel first.")
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(friends)) => {
            let filter = dialog.filter.trim().to_lowercase();
            let mut any = false;
            for friend in friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                let user_id = friend.user_id;
                friend_list = friend_list.push(
                    checkbox(dialog.selected.contains(&user_id))
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |checked| {
                            share(ShareMessage::RecipientToggled(user_id, checked))
                        }),
                );
            }
            if !any {
                friend_list =
                    friend_list.push(text("No friends match the filter.").size(12).style(muted));
            }
        }
    }
    recipients = recipients.push(
        container(scrollable(friend_list))
            .max_height(140.0)
            .width(Length::Fill),
    );
    content = content.push(recipients);

    // ===== capabilities ===================================================
    let mut caps = column![
        section_label("They can"),
        checkbox(dialog.can_edit)
            .label("Can edit (collaborative editing of YOUR canonical map)")
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Edit, value))),
        checkbox(dialog.can_reshare)
            .label("Can re-share (they may pass read access on, one level deep)")
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Reshare, value))),
        checkbox(dialog.can_copy)
            .label("Can copy (they keep and may redistribute their own fork forever, regardless of re-share)")
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Copy, value))),
    ]
    .spacing(6);

    let secrets_allowed = dialog.is_owner && !dialog.scope_atlas;
    let mut secrets_box = checkbox(dialog.include_secrets && secrets_allowed)
        .label("Include secrets (area shares only)")
        .size(14)
        .text_size(13);
    if secrets_allowed {
        secrets_box = secrets_box
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Secrets, value)));
    }
    caps = caps.push(secrets_box);
    if !secrets_allowed {
        let reason = if dialog.is_owner {
            "Secrets never ride along on atlas-wide shares — share the area directly to include them."
        } else {
            "Only the map's owner can share its secrets."
        };
        caps = caps.push(text(reason).size(11).style(muted));
    }
    // Full-deputy. Owner-minted only; on the server it implies all the caps
    // above (incl. re-share). Everything the owner can do EXCEPT transfer ownership
    // or appoint other admins.
    if dialog.is_owner {
        caps = caps.push(
            checkbox(dialog.can_admin)
                .label("Make admin — rename, delete, move, manage shares & reveal secrets (everything but transferring ownership or appointing admins)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Admin, value))),
        );
    }
    content = content.push(caps);

    // ===== secret-count warning (owner only) ==============================
    if let Some(counts) = dialog.secret_counts {
        if counts.total() > 0 {
            content = content.push(
                column![
                    text(format!(
                        "{} secret rooms, {} secret exits, {} secret notes/labels/shapes will NOT be shared.",
                        counts.rooms, counts.exits, counts.other
                    ))
                    .size(12)
                    .style(builtins::text::danger),
                    button(text("Review secrets").size(12))
                        .style(builtins::button::secondary)
                        .on_press(share(ShareMessage::ReviewSecrets)),
                ]
                .spacing(6),
            );
        } else {
            content = content.push(
                text("Nothing in this area is marked secret — everything will be shared.")
                    .size(12)
                    .style(muted),
            );
        }
    }

    // Forward-only honesty line, always shown.
    content = content.push(
        text("Marking something secret AFTER sharing only affects future syncs — anything already shared may have been seen.")
            .size(11)
            .style(muted),
    );

    // ===== preview (owner only) ===========================================
    if dialog.is_owner {
        let audience_hint = match dialog.selected_grant {
            Some(_) => "previews the selected grant below",
            None => "previews the worst case (select a grant below to preview it)",
        };
        content = content.push(
            row![
                button(text("Preview as recipient").size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!matches!(dialog.preview, PreviewState::Loading))
                            .then_some(share(ShareMessage::PreviewRequested))
                    ),
                text(audience_hint).size(11).style(muted),
            ]
            .spacing(8)
            .align_y(Vertical::Center),
        );

        match &dialog.preview {
            PreviewState::NotRequested => {}
            PreviewState::Loading => {
                content = content.push(text("Generating preview\u{2026}").size(12).style(muted));
            }
            PreviewState::Nothing(message) => {
                content = content.push(text(message.clone()).size(12).style(muted));
            }
            PreviewState::Error(error) => {
                content =
                    content.push(text(error.clone()).size(12).style(builtins::text::danger));
            }
            PreviewState::Loaded(summary) => {
                content = content.push(preview_block(summary));
            }
        }
    }

    // ===== per-recipient results ==========================================
    if !dialog.results.is_empty() {
        let mut results = Column::new().spacing(2);
        for (label, result) in &dialog.results {
            results = results.push(match result {
                Ok(()) => text(format!("Shared with {label}."))
                    .size(12)
                    .style(builtins::text::success),
                Err(CloudError::NotFoundOrNoAccess) => {
                    text(format!("Couldn't share with {label} — are you still friends?"))
                        .size(12)
                        .style(builtins::text::danger)
                }
                Err(error) => text(format!("Couldn't share with {label} — {error}"))
                    .size(12)
                    .style(builtins::text::danger),
            });
        }
        content = content.push(results);
    }

    // ===== manage existing shares =========================================
    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(manage_section(dialog));

    // ===== bottom buttons =================================================
    let share_enabled = !dialog.submitting && !dialog.selected.is_empty();
    let buttons = row![
        space::horizontal(),
        button(text("Close").size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(text(if dialog.submitting { "Sharing\u{2026}" } else { "Share" }).size(13))
            .style(builtins::button::primary)
            .on_press_maybe(share_enabled.then_some(share(ShareMessage::Submit))),
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    column![
        container(scrollable(content)).max_height(540.0),
        buttons,
    ]
    .spacing(12)
    .into()
}

fn preview_block(summary: &PreviewSummary) -> ThemedElement<'_, Message> {
    fn count_row<'a>(
        label: &'static str,
        value: usize,
    ) -> iced::widget::Row<'a, Message, crate::Theme> {
        row![
            text(label).size(12).style(muted).width(120.0),
            text(value.to_string()).size(12),
        ]
    }

    let mut block = column![
        text(format!("Previewing as {}", summary.audience)).size(12),
        text(format!("Appears as: \u{201c}{}\u{201d}", summary.name)).size(13),
        count_row("Rooms visible", summary.rooms),
        count_row("Exits visible", summary.exits),
        count_row("Labels visible", summary.labels),
        count_row("Shapes visible", summary.shapes),
        count_row("Properties visible", summary.properties),
    ]
    .spacing(3);

    if !summary.linked_visible.is_empty() || summary.linked_unknown > 0 {
        block = block.push(text("Linked areas, as they see them:").size(12).style(muted));
        for name in &summary.linked_visible {
            block = block.push(text(format!("\u{2192} {name}")).size(12));
        }
        if summary.linked_unknown > 0 {
            block = block.push(
                text(format!(
                    "\u{2192} {} link(s) resolve to \u{201c}Unknown map\u{201d}",
                    summary.linked_unknown
                ))
                .size(12)
                .style(muted),
            );
        }
    }

    container(block).padding(8).width(Length::Fill).into()
}

#[allow(clippy::too_many_lines)]
fn manage_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text("Who has access").size(13)].spacing(6);

    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    match &dialog.tree {
        None => {
            section = section.push(text("Loading\u{2026}").size(12).style(muted));
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(nodes)) if nodes.is_empty() => {
            section = section.push(text("Not shared with anyone yet.").size(12).style(muted));
        }
        Some(Ok(nodes)) => {
            // Grant id -> grantee handle, to attribute child grants to the
            // re-sharer who made them (a child's grantor is its parent's
            // grantee).
            let handles: std::collections::HashMap<Uuid, String> = nodes
                .iter()
                .filter_map(|node| {
                    node.grantee_nickname
                        .clone()
                        .map(|handle| (node.grant.id, handle))
                })
                .collect();

            let mut list = Column::new().spacing(2);
            for node in nodes {
                list = list.push(grant_row(dialog, node, &handles));
                if let Some(edit) = &dialog.editing
                    && edit.id == node.grant.id
                {
                    list = list.push(grant_edit_row(node, edit));
                }
                if dialog.revoking == Some(node.grant.id) {
                    list = list.push(revoke_confirm_row(dialog, node));
                }
            }
            section = section.push(container(scrollable(list)).max_height(220.0));
        }
    }

    section.into()
}

/// One row of the manage tree: indentation by depth, the grantee handle,
/// compact capability badges, attribution, and (when permitted) edit/revoke.
fn grant_row<'a>(
    dialog: &'a ShareDialog,
    node: &'a GrantTreeNode,
    handles: &std::collections::HashMap<Uuid, String>,
) -> ThemedElement<'a, Message> {
    let grant = &node.grant;
    let id = grant.id;
    let selected = dialog.selected_grant == Some(id);

    let grantee = node
        .grantee_nickname
        .clone()
        .unwrap_or_else(|| grant.grantee_id.to_string());

    let mut badges = Vec::new();
    if grant.can_edit {
        badges.push("edit");
    }
    if grant.can_reshare {
        badges.push("re-share");
    }
    if grant.can_copy {
        badges.push("copy");
    }
    if grant.include_secrets {
        badges.push("secrets");
    }
    let mut badge_text = if badges.is_empty() {
        "view".to_string()
    } else {
        badges.join(" \u{00b7} ")
    };
    if grant.atlas_id.is_some() {
        badge_text.push_str(" (atlas)");
    }

    let shared_by = if dialog.viewer_id == Some(grant.grantor_id) {
        "shared by you".to_string()
    } else if let Some(handle) = grant
        .parent_grant_id
        .and_then(|parent| handles.get(&parent))
    {
        format!("via {handle}")
    } else if dialog.is_owner {
        // Root grants are made by the owner; if that isn't recognizably the
        // viewer (no profile loaded), still attribute honestly.
        "shared by you".to_string()
    } else {
        match &dialog.owner_nickname {
            Some(handle) => format!("shared by {handle}"),
            None => "shared by the owner".to_string(),
        }
    };

    let indent = f32::from(u8::try_from(node.depth.clamp(0, 12)).unwrap_or(0)) * 16.0;

    let label = row![
        text(grantee).size(13),
        text(badge_text).size(11).style(muted),
        space::horizontal(),
        text(shared_by).size(11).style(muted),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    // Owner may edit every row; a re-sharer only the grants they made.
    let may_edit = dialog.is_owner || dialog.viewer_id == Some(grant.grantor_id);

    let mut item = row![
        space::horizontal().width(indent),
        button(label)
            .style(if selected {
                builtins::button::list_item_selected
            } else {
                builtins::button::list_item
            })
            .on_press(share(ShareMessage::GrantRowPressed(id)))
            .width(Length::Fill),
    ]
    .spacing(4)
    .align_y(Vertical::Center);

    if may_edit {
        item = item.push(
            button(text("Edit flags").size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditGrant(id))),
        );
        item = item.push(
            button(text("Revoke").size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeRequested(id))),
        );
    }

    item.into()
}

fn grant_edit_row<'a>(node: &'a GrantTreeNode, edit: &'a GrantEdit) -> ThemedElement<'a, Message> {
    let mut flags = row![
        checkbox(edit.can_edit)
            .label("edit")
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Edit, value))),
        checkbox(edit.can_reshare)
            .label("re-share")
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Reshare, value))),
        checkbox(edit.can_copy)
            .label("copy")
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Copy, value))),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    let mut secrets_box = checkbox(edit.include_secrets)
        .label("secrets")
        .size(14)
        .text_size(12);
    if edit.allow_secrets {
        secrets_box = secrets_box
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Secrets, value)));
    }
    flags = flags.push(secrets_box);

    let mut admin_box = checkbox(edit.can_admin).label("admin").size(14).text_size(12);
    if edit.allow_admin {
        admin_box = admin_box
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Admin, value)));
    }
    flags = flags.push(admin_box);

    let mut block = column![flags].spacing(6).padding([4, 0]);

    if node.grant.can_reshare && !edit.can_reshare {
        block = block.push(
            text("Removing re-share also revokes everything they re-shared.")
                .size(11)
                .style(muted),
        );
    }
    if let Some(error) = &edit.error {
        block = block.push(text(error.clone()).size(11).style(builtins::text::danger));
    }

    block = block.push(
        row![
            space::horizontal(),
            button(text("Cancel").size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditCancelled)),
            button(text(if edit.saving { "Saving\u{2026}" } else { "Save" }).size(11))
                .style(builtins::button::primary)
                .on_press_maybe((!edit.saving).then_some(share(ShareMessage::EditSaved))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );

    container(block).padding([0, 16]).width(Length::Fill).into()
}

fn revoke_confirm_row<'a>(
    dialog: &'a ShareDialog,
    node: &'a GrantTreeNode,
) -> ThemedElement<'a, Message> {
    let mut block = column![
        text("Revokes their access and anything they re-shared. Copies they already made are theirs.")
            .size(11)
            .style(builtins::text::danger),
    ]
    .spacing(6)
    .padding([4, 0]);

    if node.grant.atlas_id.is_some() {
        block = block.push(
            text("This is an atlas-wide grant — revoking ends their access to every area in the atlas.")
                .size(11)
                .style(builtins::text::danger),
        );
    }

    block = block.push(
        row![
            space::horizontal(),
            button(text("Cancel").size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeCancelled)),
            button(
                text(if dialog.revoke_busy {
                    "Revoking\u{2026}"
                } else {
                    "Revoke"
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe(
                (!dialog.revoke_busy).then_some(share(ShareMessage::RevokeConfirmed))
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );

    container(block).padding([0, 16]).width(Length::Fill).into()
}

// ===========================================================================
// Move-to-folder + share-folder views
// ===========================================================================

/// One selectable folder target in the move modal; the current folder shows a
/// check.
fn move_target_button<'a>(
    label: &str,
    selected: bool,
    message: Message,
) -> iced::widget::Button<'a, Message, crate::Theme> {
    let item = row![
        text(label.to_string()).size(13),
        space::horizontal(),
        text(if selected { "\u{2713}" } else { "" })
            .size(13)
            .style(muted),
    ]
    .align_y(Vertical::Center);
    button(item)
        .style(if selected {
            builtins::button::list_item_selected
        } else {
            builtins::button::list_item
        })
        .width(Length::Fill)
        // The area's current folder is non-actionable (no redundant re-file).
        .on_press_maybe((!selected).then_some(message))
}

#[allow(clippy::too_many_lines)]
fn share_atlas_view(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let mut content = Column::new().spacing(12);

    content = content.push(
        text("Everyone you pick gets every map in this folder, including maps you add later.")
            .size(12)
            .style(muted),
    );

    // ===== recipients =====================================================
    let mut recipients = column![
        section_label("Recipients"),
        text_input("filter by handle", &dialog.filter)
            .size(13)
            .on_input(|value| share_atlas(ShareAtlasMessage::FilterChanged(value))),
    ]
    .spacing(4);

    let mut friend_list = Column::new().spacing(2);
    match &dialog.friends {
        None => {
            friend_list = friend_list.push(text("Loading friends\u{2026}").size(12).style(muted));
        }
        Some(Err(error)) => {
            friend_list =
                friend_list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() => {
            friend_list = friend_list.push(
                text("No friends yet — add friends from the social panel first.")
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(friends)) => {
            let filter = dialog.filter.trim().to_lowercase();
            let mut any = false;
            for friend in friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                let user_id = friend.user_id;
                friend_list = friend_list.push(
                    checkbox(dialog.selected.contains(&user_id))
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |checked| {
                            share_atlas(ShareAtlasMessage::RecipientToggled(user_id, checked))
                        }),
                );
            }
            if !any {
                friend_list =
                    friend_list.push(text("No friends match the filter.").size(12).style(muted));
            }
        }
    }
    recipients = recipients.push(
        container(scrollable(friend_list))
            .max_height(140.0)
            .width(Length::Fill),
    );
    content = content.push(recipients);

    // ===== capabilities (atlas scope; secrets + admin allowed) =============
    content = content.push(
        column![
            section_label("They can"),
            checkbox(dialog.can_edit)
                .label("Can edit (collaborative editing of YOUR maps in this folder)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                    GrantFlag::Edit,
                    value
                ))),
            checkbox(dialog.can_reshare)
                .label("Can re-share (they may pass read access on, one level deep)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                    GrantFlag::Reshare,
                    value
                ))),
            checkbox(dialog.can_copy)
                .label("Can copy (they keep and may redistribute their own fork forever)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                    GrantFlag::Copy,
                    value
                ))),
            checkbox(dialog.include_secrets)
                .label("Include secrets — reveals hidden rooms/exits in EVERY map in this folder, now and any added later (forward-only)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                    GrantFlag::Secrets,
                    value
                ))),
            checkbox(dialog.can_admin)
                .label("Make admin — full control of this folder and its maps (everything but transferring ownership or appointing admins)")
                .size(14)
                .text_size(13)
                .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                    GrantFlag::Admin,
                    value
                ))),
        ]
        .spacing(6),
    );

    // ===== per-recipient results ==========================================
    if !dialog.results.is_empty() {
        let mut results = Column::new().spacing(2);
        for (label, result) in &dialog.results {
            results = results.push(match result {
                Ok(()) => text(format!("Shared with {label}."))
                    .size(12)
                    .style(builtins::text::success),
                Err(CloudError::NotFoundOrNoAccess) => {
                    text(format!("Couldn't share with {label} — are you still friends?"))
                        .size(12)
                        .style(builtins::text::danger)
                }
                Err(error) => text(format!("Couldn't share with {label} — {error}"))
                    .size(12)
                    .style(builtins::text::danger),
            });
        }
        content = content.push(results);
    }

    // ===== manage existing folder grants ==================================
    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(atlas_manage_section(dialog));

    let share_enabled = !dialog.submitting && !dialog.selected.is_empty();
    let buttons = row![
        space::horizontal(),
        button(text("Close").size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(text(if dialog.submitting { "Sharing\u{2026}" } else { "Share" }).size(13))
            .style(builtins::button::primary)
            .on_press_maybe(share_enabled.then_some(share_atlas(ShareAtlasMessage::Submit))),
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    column![container(scrollable(content)).max_height(540.0), buttons]
        .spacing(12)
        .into()
}

fn atlas_manage_section(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text("Who has access").size(13)].spacing(6);

    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    // Resolve grantee handles from the friends list when available.
    let handles: std::collections::HashMap<Uuid, String> = match &dialog.friends {
        Some(Ok(friends)) => friends
            .iter()
            .filter_map(|friend| {
                friend
                    .nickname
                    .clone()
                    .map(|handle| (friend.user_id, handle))
            })
            .collect(),
        _ => std::collections::HashMap::new(),
    };

    match &dialog.grants {
        None => {
            section = section.push(text("Loading\u{2026}").size(12).style(muted));
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(rows)) if rows.is_empty() => {
            section = section.push(text("Not shared with anyone yet.").size(12).style(muted));
        }
        Some(Ok(rows)) => {
            let mut list = Column::new().spacing(2);
            for row in rows {
                list = list.push(atlas_grant_row(row, &handles));
                if dialog.revoking == Some(row.grant.id) {
                    list = list.push(atlas_revoke_confirm_row(dialog));
                }
            }
            section = section.push(container(scrollable(list)).max_height(200.0));
        }
    }

    section.into()
}

fn atlas_grant_row<'a>(
    row: &'a ShareGrantRow,
    handles: &std::collections::HashMap<Uuid, String>,
) -> ThemedElement<'a, Message> {
    let grant = &row.grant;
    let grantee = handles
        .get(&grant.grantee_id)
        .cloned()
        .unwrap_or_else(|| grant.grantee_id.to_string());

    let mut badges = Vec::new();
    if grant.can_edit {
        badges.push("edit");
    }
    if grant.can_reshare {
        badges.push("re-share");
    }
    if grant.can_copy {
        badges.push("copy");
    }
    let badge_text = if badges.is_empty() {
        "view".to_string()
    } else {
        badges.join(" \u{00b7} ")
    };

    row![
        text(grantee).size(13),
        text(badge_text).size(11).style(muted),
        space::horizontal(),
        button(text("Revoke").size(11))
            .style(builtins::button::secondary)
            .on_press(share_atlas(ShareAtlasMessage::RevokeRequested(grant.id))),
    ]
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
}

fn atlas_revoke_confirm_row(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let block = column![
        text(
            "Revokes their access to every map in this folder and anything they re-shared. \
             Copies they already made are theirs."
        )
        .size(11)
        .style(builtins::text::danger),
        row![
            space::horizontal(),
            button(text("Cancel").size(11))
                .style(builtins::button::secondary)
                .on_press(share_atlas(ShareAtlasMessage::RevokeCancelled)),
            button(
                text(if dialog.revoke_busy {
                    "Revoking\u{2026}"
                } else {
                    "Revoke"
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe(
                (!dialog.revoke_busy).then_some(share_atlas(ShareAtlasMessage::RevokeConfirmed))
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ]
    .spacing(6)
    .padding([4, 0]);

    container(block).padding([0, 16]).width(Length::Fill).into()
}

/// A human-readable label for one audit entry, enriched from the cache when
/// the entity is still present ("Room 12 — Kitchen", "Exit North from room
/// 4", "Property \u{201c}loot\u{201d} on room 12", ...).
fn entity_label(area: Option<&Arc<AreaCache>>, entity: &SecretEntity) -> String {
    match entity.kind {
        SecretEntityKind::Room => {
            let number = entity.room_number.unwrap_or_default();
            area.and_then(|area| area.get_room(&RoomNumber(number)))
                .map(|room| room.get_title())
                .filter(|title| !title.is_empty())
                .map_or_else(
                    || format!("Room {number}"),
                    |title| format!("Room {number} \u{2014} {title}"),
                )
        }
        SecretEntityKind::Exit => entity
            .id
            .map(ExitId)
            .and_then(|exit_id| {
                area.and_then(|area| {
                    area.get_rooms().iter().find_map(|room| {
                        room.get_exits()
                            .iter()
                            .find(|exit| exit.id == exit_id)
                            .map(|exit| {
                                format!(
                                    "Exit {} from room {}",
                                    exit.from_direction,
                                    room.get_room_number()
                                )
                            })
                    })
                })
            })
            .unwrap_or_else(|| match entity.id {
                Some(id) => format!("Exit {id}"),
                None => "Exit".to_string(),
            }),
        SecretEntityKind::Label => entity
            .id
            .map(LabelId)
            .and_then(|id| area.and_then(|area| area.get_label(&id).cloned()))
            .map_or_else(
                || "Label".to_string(),
                |label| format!("Label \u{201c}{}\u{201d}", snippet(&label.text)),
            ),
        SecretEntityKind::Shape => entity
            .id
            .map(ShapeId)
            .and_then(|id| area.and_then(|area| area.get_shape(&id).cloned()))
            .map_or_else(
                || "Shape".to_string(),
                |shape| format!("Shape at ({:.0}, {:.0})", shape.x, shape.y),
            ),
        SecretEntityKind::RoomProperty => {
            let number = entity.room_number.unwrap_or_default();
            let name = entity.name.as_deref().unwrap_or_default();
            format!("Property \u{201c}{name}\u{201d} on room {number}")
        }
        SecretEntityKind::AreaProperty => {
            let name = entity.name.as_deref().unwrap_or_default();
            format!("Area property \u{201c}{name}\u{201d}")
        }
    }
}

/// First 24 characters of a label's text, with an ellipsis when truncated.
fn snippet(text: &str) -> String {
    let mut out: String = text.chars().take(24).collect();
    if text.chars().count() > 24 {
        out.push('\u{2026}');
    }
    out
}
