//! Clans in the map editor: each clan's folders in the map list, the clan
//! header and clan folder menus, a new clan folder, and the maps and folders
//! members give the clan or offer it to own (Incoming maps).
//!
//! A clan's maps live in its folders: Clan-owned maps, and the Member-owned
//! maps the viewer owns or was given, which show to nobody else. A
//! Member-owned map whose folder was deleted shows under the clan itself.
//! Controls follow the actions the server lists on the clan, its folders, and
//! its maps.

use std::collections::{HashMap, HashSet};

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, pick_list, row, space, text, text_input};
use iced::{Length, Padding, Task};
use smudgy_cloud::clan_maps::{AreaOwnershipOffer, MapOwnership};
use smudgy_cloud::clans::{ClanSummary, ClansOverview, action};
use smudgy_cloud::cloud_api::TransferRecipient;
use smudgy_cloud::cloud_api::TransferView;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::{Area, AreaId, AtlasId, AtlasListItem, CloudError, Uuid};

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;
use crate::widgets::dropdown::Dropdown;

use super::area_list::{AreaSummary, Folder};
use super::{FolderKey, MapEditorWindow, Message, ScopeTarget, modals};

/// What the map editor knows about the viewer's clans.
#[derive(Debug, Clone, Default)]
pub struct ClanState {
    /// The viewer's clans by name (`GET /clans`); empty while signed out.
    pub clans: Vec<ClanSummary>,
    /// The Member-owned maps their owners offer each clan to own, for the
    /// clans whose incoming maps the viewer handles
    /// (`GET /clans/{c}/area-offers`).
    pub area_offers: HashMap<Uuid, Vec<AreaOwnershipOffer>>,
    /// The maps and folders members give each clan whose incoming transfers
    /// the viewer handles (`GET /clans/{c}/transfers`).
    pub transfers: HashMap<Uuid, Vec<TransferView>>,
    /// Clan names seen on map rows and folders, for clans not yet loaded.
    pub names: HashMap<Uuid, String>,
    /// The owner of each listed map someone else owns, as its row names them.
    pub owners: HashMap<AreaId, String>,
    /// The clan whose header menu is open.
    pub menu: Option<Uuid>,
}

impl ClanState {
    /// Learns the map owners and clan names from the area list rows.
    pub fn index_rows(&mut self, areas: &[Area]) {
        self.owners = areas
            .iter()
            .filter_map(|area| area.owner_nickname.clone().map(|owner| (area.id, owner)))
            .collect();
        for area in areas {
            if let (Some(clan), Some(name)) = (area.clan_id, &area.clan_name) {
                self.names.insert(clan, name.clone());
            }
        }
    }

    fn clan(&self, clan_id: Uuid) -> Option<&ClanSummary> {
        self.clans.iter().find(|clan| clan.id == clan_id)
    }

    /// The clan's name, as the clan list or a map row or folder last named it.
    #[must_use]
    pub fn name(&self, clan_id: Uuid) -> Option<String> {
        self.clan(clan_id)
            .map(|clan| clan.name.clone())
            .or_else(|| self.names.get(&clan_id).cloned())
    }

    /// The maps offered to a clan to own, and the maps and folders given to
    /// it, waiting for an answer.
    #[must_use]
    pub fn waiting(&self, clan_id: Uuid) -> usize {
        self.area_offers.get(&clan_id).map_or(0, Vec::len)
            + self.transfers.get(&clan_id).map_or(0, Vec::len)
    }
}

/// Whether the viewer handles a clan's incoming transfers: holders of
/// `atlas.accept_transfer` on the clan (owners hold it) or on one of its
/// folders.
fn receives_transfers(clan: &ClanSummary, atlases: &[AtlasListItem]) -> bool {
    clan.is_owner
        || clan.can(action::ACCEPT_TRANSFER)
        || atlases
            .iter()
            .any(|atlas| atlas.clan_id == Some(clan.id) && atlas.can(action::ACCEPT_TRANSFER))
}

/// The clan's folders on which the viewer holds `clan_action`, by name.
fn folder_choices(window: &MapEditorWindow, clan_id: Uuid, clan_action: &str) -> Vec<FolderChoice> {
    let mut folders: Vec<FolderChoice> = window
        .atlases
        .iter()
        .filter(|atlas| atlas.clan_id == Some(clan_id) && atlas.can(clan_action))
        .map(|atlas| FolderChoice {
            id: atlas.id,
            name: atlas.name.clone(),
        })
        .collect();
    folders.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    folders
}

/// Destinations where the caller can complete the whole selected transfer.
/// Maps need an existing authorized folder; whole folders need clan ownership.
#[derive(Debug, Clone, Default)]
pub(super) struct TransferDestinations {
    pub clans: Vec<(Uuid, String)>,
    pub folders: HashMap<Uuid, Vec<FolderChoice>>,
    pub folder: Option<FolderChoice>,
}

impl TransferDestinations {
    pub fn new(window: &MapEditorWindow, maps: bool, atlases: bool) -> Self {
        let mut result = Self::default();
        for clan in &window.clans.clans {
            let folders = folder_choices(window, clan.id, action::ACCEPT_TRANSFER);
            if (maps && folders.is_empty()) || (atlases && !clan.is_owner) {
                continue;
            }
            result.clans.push((clan.id, clan.name.clone()));
            result.folders.insert(clan.id, folders);
        }
        result
    }

    pub fn select(&mut self, recipient: TransferRecipient) {
        self.folder = match recipient {
            TransferRecipient::Clan(clan, _) => self
                .folders
                .get(&clan)
                .filter(|folders| folders.len() == 1)
                .map(|folders| folders[0].clone()),
            TransferRecipient::User(_) => None,
        };
    }

    pub fn ready(&self, recipient: TransferRecipient, maps: bool) -> bool {
        match recipient {
            TransferRecipient::User(_) => true,
            TransferRecipient::Clan(clan, _) => {
                self.clans.iter().any(|(id, _)| *id == clan)
                    && (!maps
                        || self.folder.as_ref().is_some_and(|chosen| {
                            self.folders
                                .get(&clan)
                                .is_some_and(|folders| folders.contains(chosen))
                        }))
            }
        }
    }
}

/// The clan folders on file, by id.
pub(super) fn clan_folder(window: &MapEditorWindow, atlas_id: AtlasId) -> Option<&AtlasListItem> {
    window
        .atlases
        .iter()
        .find(|atlas| atlas.id == atlas_id && atlas.clan_id.is_some())
}

/// Whether `atlas_id` is one of the viewer's clans' folders.
#[must_use]
pub fn is_clan_folder(window: &MapEditorWindow, atlas_id: AtlasId) -> bool {
    clan_folder(window, atlas_id).is_some()
}

/// Every clan map the viewer sees. "Shared by" leaves these out.
#[must_use]
pub fn in_clan_folders(atlas: &AtlasCache) -> HashSet<AreaId> {
    atlas
        .areas()
        .filter(|area| area.meta().clan_id.is_some())
        .map(|area| *area.get_id())
        .collect()
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ClanMessage {
    Loaded {
        auth_projection_revision: u64,
        clans: Result<ClansOverview, CloudError>,
        offers: Vec<(Uuid, Result<Vec<AreaOwnershipOffer>, CloudError>)>,
        transfers: Vec<(Uuid, Result<Vec<TransferView>, CloudError>)>,
    },
    /// A clan header's menu opened (`Some`) or closed.
    MenuToggled(Option<Uuid>),
    NewFolderRequested(Uuid),
    FolderNameChanged(String),
    FolderConfirmed,
    FolderCreated(Result<AtlasId, CloudError>),
    DeleteFolderRequested(AtlasId),
    DeleteFolderConfirmed,
    FolderDeleted(Result<(), CloudError>),
    IncomingRequested(Uuid),
    /// A folder picked for a map offered to the clan to own, by offer.
    FolderPicked(Uuid, FolderChoice),
    Accept(Uuid),
    Decline(Uuid),
    Answered {
        offer: Uuid,
        result: Result<(), CloudError>,
    },
    /// A folder picked for a map given to the clan, by offer.
    GiftFolderPicked(Uuid, FolderChoice),
    AcceptGift(Uuid),
    DeclineGift(Uuid),
    GiftAnswered {
        offer: Uuid,
        result: Result<(), CloudError>,
    },
    ShareFolderRequested(AtlasId),
    Share(super::clan_share::ClanShareMessage),
}

fn clan(message: ClanMessage) -> Message {
    Message::Clan(message)
}

/// Fetches the viewer's clans, and for each clan whose incoming maps they
/// handle, the maps offered to it to own and the maps and folders given to
/// it. No-op while signed out.
pub(super) fn fetch(window: &MapEditorWindow) -> Task<Message> {
    if !window.cloud.snapshot.get().signed_in {
        return Task::none();
    }
    let client = window.cloud.client.clone();
    let atlases = window.atlases.clone();
    let auth_projection_revision = window.mapper.auth_projection_revision();
    Task::perform(
        async move {
            let clans = client.clans().await;
            let mut offers = Vec::new();
            let mut transfers = Vec::new();
            if let Ok(overview) = &clans {
                for summary in &overview.clans {
                    if receives_transfers(summary, &atlases) {
                        offers.push((summary.id, client.clan_area_offers(summary.id).await));
                        transfers.push((summary.id, client.clan_transfers(summary.id).await));
                    }
                }
            }
            (clans, offers, transfers)
        },
        move |(clans, offers, transfers)| {
            clan(ClanMessage::Loaded {
                auth_projection_revision,
                clans,
                offers,
                transfers,
            })
        },
    )
}

/// After a clan change: the mapper syncs, and the list, folders and clans
/// load again.
fn refresh(window: &MapEditorWindow) -> Task<Message> {
    window.mapper.sync_now();
    Task::batch([
        window.fetch_sharers(),
        window.fetch_atlases(),
        fetch(window),
    ])
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

/// A clan folder a map can be put in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderChoice {
    pub id: AtlasId,
    pub name: String,
}

impl std::fmt::Display for FolderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// A map or folder a member gives the clan, in the Incoming maps dialog.
#[derive(Debug, Clone)]
pub struct GiftRow {
    pub offer: TransferView,
    /// Where a given map goes; a given folder needs none.
    pub folder: Option<FolderChoice>,
    pub busy: bool,
    /// The offer was cancelled or answered elsewhere; the row stays only to
    /// show its error, without buttons.
    pub gone: bool,
    pub error: Option<String>,
}

impl GiftRow {
    fn is_folder(&self) -> bool {
        self.offer.subject_kind == "atlas"
    }
}

/// Brings an open dialog's given maps and folders in line with the clan's
/// refreshed `offers`: a row whose offer is no longer listed leaves, unless
/// it shows an error (it stays, without buttons) or waits on an answer; an
/// offer not shown yet gets a row, in `folder` when there is one choice.
fn reconcile_gifts(
    rows: &mut Vec<GiftRow>,
    offers: &[TransferView],
    folder: Option<&FolderChoice>,
) {
    rows.retain_mut(|row| {
        if row.busy || offers.iter().any(|offer| offer.id == row.offer.id) {
            return true;
        }
        row.gone = true;
        row.error.is_some()
    });
    for offer in offers {
        if !rows.iter().any(|row| row.offer.id == offer.id) {
            rows.push(GiftRow {
                offer: offer.clone(),
                folder: folder.cloned(),
                busy: false,
                gone: false,
                error: None,
            });
        }
    }
}

/// [`reconcile_gifts`] for the maps offered to the clan to own: a withdrawn
/// offer's row leaves unless it shows an error or is already marked gone.
fn reconcile_offers(
    rows: &mut Vec<IncomingRow>,
    offers: &[AreaOwnershipOffer],
    folder: Option<&FolderChoice>,
) {
    rows.retain_mut(|row| {
        if row.busy || offers.iter().any(|offer| offer.id == row.offer.id) {
            return true;
        }
        let shown = row.gone || row.error.is_some();
        row.gone = true;
        shown
    });
    for offer in offers {
        if !rows.iter().any(|row| row.offer.id == offer.id) {
            rows.push(IncomingRow {
                offer: offer.clone(),
                folder: folder.cloned(),
                busy: false,
                gone: false,
                error: None,
            });
        }
    }
}

/// One Member-owned map its owners offer the clan to own, in the Incoming
/// maps dialog.
#[derive(Debug, Clone)]
pub struct IncomingRow {
    pub offer: AreaOwnershipOffer,
    pub folder: Option<FolderChoice>,
    pub busy: bool,
    /// The offer was withdrawn or answered elsewhere.
    pub gone: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ClanModal {
    NewFolder {
        clan_id: Uuid,
        clan_name: String,
        name: String,
        busy: bool,
        error: Option<String>,
    },
    DeleteFolder {
        atlas_id: AtlasId,
        name: String,
        busy: bool,
        error: Option<String>,
    },
    Incoming {
        clan_id: Uuid,
        clan_name: String,
        rows: Vec<IncomingRow>,
        /// The folders a given or offered map may go into: where the viewer
        /// holds `atlas.accept_transfer`.
        gift_folders: Vec<FolderChoice>,
        gifts: Vec<GiftRow>,
        /// Whether the viewer accepts a given folder (on the clan).
        accepts_folders: bool,
    },
    Share(Box<super::clan_share::ClanShareDialog>),
}

/// An open Incoming maps dialog follows the clan's refreshed lists, so an
/// offer cancelled or answered elsewhere loses its buttons without the
/// dialog being reopened.
fn reconcile_open_dialog(window: &mut MapEditorWindow) {
    let Some(ClanModal::Incoming { clan_id, .. }) = modal_mut(window) else {
        return;
    };
    let clan_id = *clan_id;
    let giving = folder_choices(window, clan_id, action::ACCEPT_TRANSFER);
    let only_gift = (giving.len() == 1).then(|| giving[0].clone());
    let offered = window
        .clans
        .area_offers
        .get(&clan_id)
        .cloned()
        .unwrap_or_default();
    let offers = window
        .clans
        .transfers
        .get(&clan_id)
        .cloned()
        .unwrap_or_default();
    if let Some(ClanModal::Incoming { rows, gifts, .. }) = modal_mut(window) {
        reconcile_offers(rows, &offered, only_gift.as_ref());
        reconcile_gifts(gifts, &offers, only_gift.as_ref());
    }
}

fn modal_mut(window: &mut MapEditorWindow) -> Option<&mut ClanModal> {
    match &mut window.modal {
        Some(modals::Modal::Clan(modal)) => Some(modal),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn update(
    window: &mut MapEditorWindow,
    message: ClanMessage,
) -> Update<Message, super::Event> {
    match message {
        ClanMessage::Loaded {
            auth_projection_revision,
            clans,
            offers,
            transfers,
        } => {
            if auth_projection_revision != window.mapper.auth_projection_revision() {
                return Update::none();
            }
            match clans {
                Ok(overview) => {
                    for summary in &overview.clans {
                        window.clans.names.insert(summary.id, summary.name.clone());
                    }
                    window.clans.clans = overview.clans;
                }
                Err(CloudError::Unauthorized(_) | CloudError::EmailNotVerified) => {
                    window.clans = ClanState::default();
                }
                Err(error) => log::warn!("map editor: clan list fetch failed: {error}"),
            }
            let known: HashSet<Uuid> = window.clans.clans.iter().map(|clan| clan.id).collect();
            window
                .clans
                .area_offers
                .retain(|clan, _| known.contains(clan));
            for (clan_id, result) in offers {
                match result {
                    Ok(offers) => {
                        window.clans.area_offers.insert(clan_id, offers);
                    }
                    Err(CloudError::NotFoundOrNoAccess) => {
                        window.clans.area_offers.remove(&clan_id);
                    }
                    Err(error) => log::warn!("map editor: map offers fetch failed: {error}"),
                }
            }
            window
                .clans
                .transfers
                .retain(|clan, _| known.contains(clan));
            for (clan_id, result) in transfers {
                match result {
                    Ok(offers) => {
                        window.clans.transfers.insert(clan_id, offers);
                    }
                    Err(CloudError::NotFoundOrNoAccess) => {
                        window.clans.transfers.remove(&clan_id);
                    }
                    Err(error) => log::warn!("map editor: clan transfers fetch failed: {error}"),
                }
            }
            reconcile_open_dialog(window);
            Update::none()
        }
        ClanMessage::MenuToggled(open) => {
            window.clans.menu = open;
            window.folder_menu = None;
            Update::none()
        }
        ClanMessage::NewFolderRequested(clan_id) => {
            let clan_name = window.clans.name(clan_id).unwrap_or_default();
            window.modal = Some(modals::Modal::Clan(Box::new(ClanModal::NewFolder {
                clan_id,
                clan_name,
                name: String::new(),
                busy: false,
                error: None,
            })));
            Update::none()
        }
        ClanMessage::FolderNameChanged(value) => {
            if let Some(ClanModal::NewFolder { name, .. }) = modal_mut(window) {
                *name = value;
            }
            Update::none()
        }
        ClanMessage::FolderConfirmed => {
            let Some(ClanModal::NewFolder {
                clan_id,
                name,
                busy,
                error,
                ..
            }) = modal_mut(window)
            else {
                return Update::none();
            };
            let name = name.trim().to_string();
            if name.is_empty() || *busy {
                return Update::none();
            }
            *busy = true;
            *error = None;
            let clan_id = *clan_id;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.create_clan_atlas(clan_id, &name).await },
                |result| clan(ClanMessage::FolderCreated(result.map(|atlas| atlas.id))),
            ))
        }
        ClanMessage::FolderCreated(result) => match result {
            Ok(atlas_id) => {
                if matches!(modal_mut(window), Some(ClanModal::NewFolder { .. })) {
                    window.modal = None;
                }
                window.collapsed_folders.remove(&FolderKey::Atlas(atlas_id));
                // Like any new folder, it shows on the server it was made from.
                let associated = window.associate_new_atlas(atlas_id);
                Update::new(window.fetch_atlases(), associated)
            }
            Err(failure) => {
                if let Some(ClanModal::NewFolder { busy, error, .. }) = modal_mut(window) {
                    *busy = false;
                    *error = Some(display_error(&failure));
                }
                Update::none()
            }
        },
        ClanMessage::DeleteFolderRequested(atlas_id) => {
            if let Some(folder) = clan_folder(window, atlas_id) {
                window.modal = Some(modals::Modal::Clan(Box::new(ClanModal::DeleteFolder {
                    atlas_id,
                    name: folder.name.clone(),
                    busy: false,
                    error: None,
                })));
            }
            Update::none()
        }
        ClanMessage::DeleteFolderConfirmed => {
            let Some(ClanModal::DeleteFolder {
                atlas_id,
                busy,
                error,
                ..
            }) = modal_mut(window)
            else {
                return Update::none();
            };
            if *busy {
                return Update::none();
            }
            *busy = true;
            *error = None;
            let atlas_id = *atlas_id;
            let mapper = window.mapper.clone();
            Update::with_task(Task::perform(
                async move { mapper.delete_atlas(atlas_id).await },
                |result| clan(ClanMessage::FolderDeleted(result)),
            ))
        }
        ClanMessage::FolderDeleted(result) => match result {
            Ok(()) => {
                if let Some(ClanModal::DeleteFolder { atlas_id, .. }) = modal_mut(window) {
                    let atlas_id = *atlas_id;
                    window.atlases.retain(|atlas| atlas.id != atlas_id);
                    window.collapsed_folders.remove(&FolderKey::Atlas(atlas_id));
                    window.modal = None;
                }
                Update::with_task(refresh(window))
            }
            Err(failure) => {
                if let Some(ClanModal::DeleteFolder { busy, error, .. }) = modal_mut(window) {
                    *busy = false;
                    *error = Some(display_error(&failure));
                }
                Update::none()
            }
        },
        ClanMessage::IncomingRequested(clan_id) => {
            let gift_folders = folder_choices(window, clan_id, action::ACCEPT_TRANSFER);
            let only_gift = (gift_folders.len() == 1).then(|| gift_folders[0].clone());
            let gifts = window
                .clans
                .transfers
                .get(&clan_id)
                .into_iter()
                .flatten()
                .map(|offer| GiftRow {
                    offer: offer.clone(),
                    folder: only_gift.clone(),
                    busy: false,
                    gone: false,
                    error: None,
                })
                .collect();
            let accepts_folders = window
                .clans
                .clan(clan_id)
                .is_some_and(|clan| clan.is_owner || clan.can(action::ACCEPT_TRANSFER));
            let rows = window
                .clans
                .area_offers
                .get(&clan_id)
                .into_iter()
                .flatten()
                .map(|offer| IncomingRow {
                    offer: offer.clone(),
                    folder: only_gift.clone(),
                    busy: false,
                    gone: false,
                    error: None,
                })
                .collect();
            window.modal = Some(modals::Modal::Clan(Box::new(ClanModal::Incoming {
                clan_id,
                clan_name: window.clans.name(clan_id).unwrap_or_default(),
                rows,
                gift_folders,
                gifts,
                accepts_folders,
            })));
            Update::none()
        }
        ClanMessage::GiftFolderPicked(offer, choice) => {
            if let Some(ClanModal::Incoming { gifts, .. }) = modal_mut(window)
                && let Some(row) = gifts.iter_mut().find(|row| row.offer.id == offer)
            {
                row.folder = Some(choice);
            }
            Update::none()
        }
        ClanMessage::AcceptGift(offer) | ClanMessage::DeclineGift(offer) => {
            let accept = matches!(message, ClanMessage::AcceptGift(_));
            let client = window.cloud.client.clone();
            let Some(ClanModal::Incoming {
                gifts,
                accepts_folders,
                ..
            }) = modal_mut(window)
            else {
                return Update::none();
            };
            let accepts_folders = *accepts_folders;
            let Some(row) = gifts.iter_mut().find(|row| row.offer.id == offer) else {
                return Update::none();
            };
            if row.busy || row.gone {
                return Update::none();
            }
            let folder = row.folder.as_ref().map(|choice| choice.id);
            if accept
                && !(if row.is_folder() {
                    accepts_folders
                } else {
                    folder.is_some()
                })
            {
                return Update::none();
            }
            row.busy = true;
            row.error = None;
            Update::with_task(Task::perform(
                async move {
                    if accept {
                        client
                            .accept_transfer(offer, None, folder)
                            .await
                            .map(|_| ())
                    } else {
                        client.decline_transfer(offer).await
                    }
                },
                move |result| clan(ClanMessage::GiftAnswered { offer, result }),
            ))
        }
        ClanMessage::GiftAnswered { offer, result } => {
            if let Some(ClanModal::Incoming { gifts, .. }) = modal_mut(window) {
                match result {
                    Ok(()) => gifts.retain(|row| row.offer.id != offer),
                    Err(failure) => {
                        if let Some(row) = gifts.iter_mut().find(|row| row.offer.id == offer) {
                            row.busy = false;
                            row.error = Some(match failure {
                                CloudError::NotFoundOrNoAccess => {
                                    crate::i18n::t!("clan-maps-could-not-accept")
                                }
                                other => display_error(&other),
                            });
                        }
                        return Update::with_task(refresh(window));
                    }
                }
            }
            // The badge follows at once; the refetch confirms it.
            for offers in window.clans.transfers.values_mut() {
                offers.retain(|known| known.id != offer);
            }
            Update::with_task(refresh(window))
        }
        ClanMessage::FolderPicked(offer, choice) => {
            if let Some(ClanModal::Incoming { rows, .. }) = modal_mut(window)
                && let Some(row) = rows.iter_mut().find(|row| row.offer.id == offer)
            {
                row.folder = Some(choice);
            }
            Update::none()
        }
        ClanMessage::Accept(offer) | ClanMessage::Decline(offer) => {
            let accept = matches!(message, ClanMessage::Accept(_));
            let client = window.cloud.client.clone();
            let Some(ClanModal::Incoming { rows, .. }) = modal_mut(window) else {
                return Update::none();
            };
            let Some(row) = rows.iter_mut().find(|row| row.offer.id == offer) else {
                return Update::none();
            };
            if row.busy || row.gone {
                return Update::none();
            }
            let folder = row.folder.as_ref().map(|choice| choice.id);
            if accept && folder.is_none() {
                return Update::none();
            }
            row.busy = true;
            row.error = None;
            let area_id = row.offer.area_id;
            Update::with_task(Task::perform(
                async move {
                    if accept {
                        client
                            .accept_area_offer(area_id, offer, folder)
                            .await
                            .map(|_| ())
                    } else {
                        client.decline_area_offer(area_id, offer).await
                    }
                },
                move |result| clan(ClanMessage::Answered { offer, result }),
            ))
        }
        ClanMessage::Answered { offer, result } => {
            let Some(ClanModal::Incoming { rows, .. }) = modal_mut(window) else {
                return Update::with_task(refresh(window));
            };
            match result {
                Ok(()) => rows.retain(|row| row.offer.id != offer),
                Err(failure) => {
                    if let Some(row) = rows.iter_mut().find(|row| row.offer.id == offer) {
                        row.busy = false;
                        match failure {
                            CloudError::NotFoundOrNoAccess => row.gone = true,
                            other => row.error = Some(display_error(&other)),
                        }
                    }
                }
            }
            // The badge follows at once; the refetch confirms it.
            for offers in window.clans.area_offers.values_mut() {
                offers.retain(|known| known.id != offer);
            }
            Update::with_task(refresh(window))
        }
        ClanMessage::ShareFolderRequested(atlas_id) => {
            let Some(clan_id) = clan_folder(window, atlas_id).and_then(|atlas| atlas.clan_id)
            else {
                return Update::none();
            };
            super::clan_share::open(window, clan_id, Some(atlas_id))
        }
        ClanMessage::Share(message) => super::clan_share::update(window, message),
    }
}

// ---------------------------------------------------------------------------
// The map list
// ---------------------------------------------------------------------------

/// One clan in the map list: its folders, name-sorted, the Member-owned
/// maps a folder's deletion left in no folder, and how many maps wait for
/// an answer.
pub struct ClanGroup {
    pub clan_id: Uuid,
    pub label: String,
    pub incoming: usize,
    pub folders: Vec<Folder>,
    pub unfiled: Vec<AreaSummary>,
}

fn sort_folders(folders: &mut [Folder]) {
    folders.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });
}

/// One map as the clan grouping reads it.
#[derive(Debug, Clone)]
pub struct ListedMap {
    pub id: AreaId,
    pub name: String,
    /// The clan that owns it, on a clan's map.
    pub clan_id: Option<Uuid>,
    pub atlas_id: Option<AtlasId>,
    pub atlas_name: Option<String>,
    pub owned: bool,
    pub can_edit: bool,
    pub enabled: bool,
    pub in_family: bool,
}

impl ListedMap {
    fn summary(&self) -> AreaSummary {
        AreaSummary {
            id: self.id,
            name: self.name.clone(),
            owned: self.owned,
            can_edit: self.can_edit,
            can_admin: false,
            enabled: self.enabled,
            reshare_owner: None,
            sharer_label: None,
            in_family: self.in_family,
        }
    }
}

/// The viewer's clans by name, each with the folders they see and the maps
/// in them, and the Member-owned maps in no folder. Session maps are left
/// out.
#[must_use]
pub fn clan_groups(
    window: &MapEditorWindow,
    atlas: &AtlasCache,
    family_members: &HashSet<AreaId>,
    ephemeral: &HashSet<AreaId>,
) -> Vec<ClanGroup> {
    let maps: Vec<ListedMap> = atlas
        .areas()
        .filter(|area| !ephemeral.contains(area.get_id()))
        .map(|area| {
            let access = area.effective_access();
            let meta = area.meta();
            let id = *area.get_id();
            ListedMap {
                id,
                name: area.get_name().to_string(),
                clan_id: meta.clan_id,
                atlas_id: meta.atlas_id,
                atlas_name: meta.atlas_name.clone(),
                owned: access.is_owner,
                can_edit: access.can_edit,
                enabled: atlas.is_area_enabled(&id),
                in_family: family_members.contains(&id),
            }
        })
        .collect();
    group_clans(&window.clans, &window.atlases, &maps)
}

/// [`clan_groups`] over plain rows: the clans and clan folders the viewer
/// sees, and every listed map.
#[must_use]
pub fn group_clans(
    state: &ClanState,
    atlases: &[AtlasListItem],
    maps: &[ListedMap],
) -> Vec<ClanGroup> {
    // clan -> folder -> (label, areas)
    let mut by_clan: HashMap<Uuid, HashMap<AtlasId, (String, Vec<AreaSummary>)>> = HashMap::new();
    let mut unfiled: HashMap<Uuid, Vec<AreaSummary>> = HashMap::new();
    for clan in &state.clans {
        by_clan.entry(clan.id).or_default();
    }
    for folder in atlases {
        if let Some(clan_id) = folder.clan_id {
            by_clan
                .entry(clan_id)
                .or_default()
                .insert(folder.id, (folder.name.clone(), Vec::new()));
        }
    }

    for map in maps {
        let Some(clan_id) = map.clan_id else {
            continue;
        };
        let Some(atlas_id) = map.atlas_id else {
            by_clan.entry(clan_id).or_default();
            unfiled.entry(clan_id).or_default().push(map.summary());
            continue;
        };
        // A folder missing from the inventory takes the name the map
        // carries, until the inventory catches up.
        let fallback = map
            .atlas_name
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("area-list-shared-folder"));
        by_clan
            .entry(clan_id)
            .or_default()
            .entry(atlas_id)
            .or_insert_with(|| (fallback, Vec::new()))
            .1
            .push(map.summary());
    }

    let mut groups: Vec<ClanGroup> = by_clan
        .into_iter()
        .map(|(clan_id, folders)| {
            let mut folders: Vec<Folder> = folders
                .into_iter()
                .map(|(atlas_id, (label, mut areas))| {
                    areas.sort_by(|a, b| {
                        a.name
                            .to_lowercase()
                            .cmp(&b.name.to_lowercase())
                            .then_with(|| a.name.cmp(&b.name))
                    });
                    Folder {
                        key: FolderKey::Atlas(atlas_id),
                        label,
                        areas,
                        owned: false,
                    }
                })
                .collect();
            sort_folders(&mut folders);
            let mut loose = unfiled.remove(&clan_id).unwrap_or_default();
            loose.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.name.cmp(&b.name))
            });
            ClanGroup {
                clan_id,
                label: state
                    .name(clan_id)
                    .or_else(|| {
                        atlases
                            .iter()
                            .find(|folder| folder.clan_id == Some(clan_id))
                            .and_then(|folder| folder.clan_name.clone())
                    })
                    .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-clan")),
                incoming: state.waiting(clan_id),
                folders,
                unfiled: loose,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.clan_id.cmp(&b.clan_id))
    });
    groups
}

/// A dimmed, subtle badge.
fn badge<'a>(content: String) -> iced::widget::Text<'a, crate::Theme> {
    text(content)
        .size(10)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.45)),
        })
}

/// A ⋯ menu: the trigger, and the open list of entries.
fn dots_menu(
    entries: Vec<(String, Message)>,
    open: bool,
    toggle: Message,
    close: Message,
) -> ThemedElement<'static, Message> {
    let trigger = button(text("⋯").size(14.0))
        .style(if open {
            builtins::button::toolbar_active
        } else {
            builtins::button::toolbar
        })
        .padding([0, 6])
        .on_press(toggle);
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
    Dropdown::new(trigger, menu, close).into()
}

/// A clan's header: its name, how many maps wait for a folder, and its ⋯
/// menu (New folder, Share…, Incoming maps…). `with_menu` is false where the
/// clan's name only labels folders in another section.
pub fn clan_header<'a>(
    window: &MapEditorWindow,
    group: &ClanGroup,
    with_menu: bool,
) -> ThemedElement<'a, Message> {
    let mut header = row![
        text(group.label.clone())
            .size(12)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.6)),
            })
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .width(Length::Fill);
    if with_menu && group.incoming > 0 {
        header = header.push(badge(
            crate::i18n::t!("clan-maps-incoming-badge", "count" => group.incoming),
        ));
    }
    header = header.push(space::horizontal());
    if with_menu {
        let summary = window.clans.clan(group.clan_id);
        let mut entries: Vec<(String, Message)> = Vec::new();
        if summary.is_some_and(|clan| clan.can(action::CREATE_ATLAS)) {
            entries.push((
                crate::i18n::t!("clan-maps-new-folder"),
                clan(ClanMessage::NewFolderRequested(group.clan_id)),
            ));
        }
        if group.incoming > 0 {
            entries.push((
                crate::i18n::t!("clan-maps-incoming"),
                clan(ClanMessage::IncomingRequested(group.clan_id)),
            ));
        }
        if !entries.is_empty() {
            let open = window.clans.menu == Some(group.clan_id);
            header = header.push(dots_menu(
                entries,
                open,
                clan(ClanMessage::MenuToggled((!open).then_some(group.clan_id))),
                clan(ClanMessage::MenuToggled(None)),
            ));
        }
    }
    container(header)
        .padding(Padding {
            top: 4.0,
            bottom: 0.0,
            left: 0.0,
            right: 0.0,
        })
        .into()
}

/// A clan folder's ⋯ entries, as its actions allow: New map in folder,
/// Rename, Share…, Servers…, Delete folder.
#[must_use]
pub fn folder_entries(window: &MapEditorWindow, atlas_id: AtlasId) -> Vec<(String, Message)> {
    let Some(folder) = clan_folder(window, atlas_id) else {
        return vec![(
            crate::i18n::t!("area-list-servers-action"),
            Message::ServersChecklistRequested(ScopeTarget::Atlas(atlas_id)),
        )];
    };
    let mut entries = Vec::new();
    if folder.can(action::CREATE_AREA) || folder.can(action::CREATE_MEMBER_OWNED_AREA) {
        entries.push((
            crate::i18n::t!("area-list-new-map-folder"),
            Message::NewAreaInAtlas(atlas_id),
        ));
    }
    if folder.can(action::RENAME_ATLAS) {
        entries.push((
            crate::i18n::t!("mapper-menu-rename"),
            Message::RenameAtlasStarted(atlas_id),
        ));
    }
    if folder.can(action::MANAGE_GRANTS) {
        entries.push((
            crate::i18n::t!("area-list-share-action"),
            clan(ClanMessage::ShareFolderRequested(atlas_id)),
        ));
    }
    entries.push((
        crate::i18n::t!("area-list-servers-action"),
        Message::ServersChecklistRequested(ScopeTarget::Atlas(atlas_id)),
    ));
    if folder.can(action::DELETE_ATLAS) {
        entries.push((
            crate::i18n::t!("area-list-delete-folder"),
            clan(ClanMessage::DeleteFolderRequested(atlas_id)),
        ));
    }
    entries
}

/// The clan folders a clan map can move to: the clan's other folders the
/// viewer may put maps in.
#[must_use]
pub fn refile_targets(
    window: &MapEditorWindow,
    clan_id: Uuid,
    current: Option<AtlasId>,
) -> Vec<(AtlasId, String)> {
    let mut targets: Vec<(AtlasId, String)> = window
        .atlases
        .iter()
        .filter(|atlas| {
            atlas.clan_id == Some(clan_id)
                && Some(atlas.id) != current
                && atlas.can(action::ACCEPT_FILING)
        })
        .map(|atlas| (atlas.id, atlas.name.clone()))
        .collect();
    targets.sort_by_key(|(_, name)| name.to_lowercase());
    targets
}

// ---------------------------------------------------------------------------
// Dialog views
// ---------------------------------------------------------------------------

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

fn buttons<'a>(
    confirm: String,
    busy: bool,
    on_confirm: Option<Message>,
) -> ThemedElement<'a, Message> {
    row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(text(confirm).size(13))
            .style(builtins::button::primary)
            .on_press_maybe(on_confirm.filter(|_| !busy)),
    ]
    .spacing(10)
    .align_y(Vertical::Center)
    .into()
}

impl ClanModal {
    /// The dialog's width.
    #[must_use]
    pub fn width(&self) -> f32 {
        match self {
            Self::Share(_) => 600.0,
            Self::Incoming { .. } => 560.0,
            _ => 380.0,
        }
    }

    /// The dialog's title and body.
    #[must_use]
    pub fn view(&self) -> (String, ThemedElement<'_, Message>) {
        match self {
            Self::NewFolder {
                clan_name,
                name,
                busy,
                error,
                ..
            } => {
                let mut body = column![
                    text(crate::i18n::t!("mapper-name-new-folder")).size(13),
                    text_input(crate::i18n::ts!("mapper-folder-name-placeholder"), name)
                        .size(14)
                        .on_input(|value| clan(ClanMessage::FolderNameChanged(value)))
                        .on_submit(clan(ClanMessage::FolderConfirmed)),
                ]
                .spacing(10);
                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }
                let footer = buttons(
                    crate::i18n::t!("clan-maps-create"),
                    *busy,
                    (!name.trim().is_empty()).then_some(clan(ClanMessage::FolderConfirmed)),
                );
                (
                    crate::i18n::t!("clan-maps-new-folder-title", "clan" => clan_name),
                    super::modals::scrolling_body(body, footer),
                )
            }
            Self::DeleteFolder {
                name, busy, error, ..
            } => {
                let mut body = column![
                    text(crate::i18n::t!("clan-maps-delete-folder-question", "name" => name))
                        .size(13),
                    text(crate::i18n::t!("clan-maps-delete-folder-detail"))
                        .size(12)
                        .style(muted),
                ]
                .spacing(10);
                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }
                let footer = buttons(
                    crate::i18n::t!("mapper-delete-folder"),
                    *busy,
                    Some(clan(ClanMessage::DeleteFolderConfirmed)),
                );
                (
                    crate::i18n::t!("mapper-delete-folder"),
                    super::modals::scrolling_body(body, footer),
                )
            }
            Self::Incoming {
                clan_name,
                rows,
                gift_folders,
                gifts,
                accepts_folders,
                ..
            } => (
                crate::i18n::t!("clan-maps-incoming-title", "clan" => clan_name),
                incoming_view(rows, gift_folders, gifts, *accepts_folders),
            ),
            Self::Share(dialog) => super::clan_share::view(dialog),
        }
    }
}

/// A given map or folder, with its destination and actions below its name
/// so a long name cannot push the buttons out of view.
fn gift_view<'a>(
    gift_folders: &'a [FolderChoice],
    row_state: &'a GiftRow,
    accepts_folders: bool,
) -> ThemedElement<'a, Message> {
    let offer = &row_state.offer;
    let id = offer.id;
    let subject = offer.subject_name.clone().unwrap_or_default();
    let name = if row_state.is_folder() {
        crate::i18n::t!("clan-maps-given-folder", "name" => subject)
    } else {
        subject
    };
    let owner = offer
        .from_nickname
        .clone()
        .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
    let given_by = if offer.ownership == Some(MapOwnership::Members) {
        crate::i18n::t!("clan-maps-given-stays", "owner" => owner)
    } else {
        crate::i18n::t!("clan-maps-given-by", "owner" => owner)
    };
    let mut item = column![text(name).size(13), text(given_by).size(11).style(muted),].spacing(4);
    if row_state.gone {
        if let Some(error) = &row_state.error {
            item = item.push(text(error.clone()).size(11).style(builtins::text::danger));
        }
        return item.into();
    }
    let mut line = row![space::horizontal()]
        .spacing(8)
        .align_y(Vertical::Center);
    let ready = if row_state.is_folder() {
        accepts_folders
    } else {
        line = line.push(
            pick_list(gift_folders, row_state.folder.clone(), move |choice| {
                clan(ClanMessage::GiftFolderPicked(id, choice))
            })
            .placeholder(crate::i18n::t!("clan-maps-folder-placeholder"))
            .text_size(12.0)
            .width(150),
        );
        row_state.folder.is_some()
    };
    line = line
        .push(
            button(text(crate::i18n::t!("clan-maps-accept")).size(12))
                .style(builtins::button::primary)
                .on_press_maybe(
                    (ready && !row_state.busy).then_some(clan(ClanMessage::AcceptGift(id))),
                ),
        )
        .push(
            button(text(crate::i18n::t!("clan-maps-decline")).size(12))
                .style(builtins::button::secondary)
                .on_press_maybe((!row_state.busy).then_some(clan(ClanMessage::DeclineGift(id)))),
        );
    item = item.push(line);
    if let Some(error) = &row_state.error {
        item = item.push(text(error.clone()).size(11).style(builtins::text::danger));
    }
    item.into()
}

/// Member-owned maps offered to the clan to own, followed by the maps and
/// folders members give the clan. Each row puts its actions below its name.
fn incoming_view<'a>(
    rows: &'a [IncomingRow],
    gift_folders: &'a [FolderChoice],
    gifts: &'a [GiftRow],
    accepts_folders: bool,
) -> ThemedElement<'a, Message> {
    let mut list = Column::new().spacing(8);
    if rows.is_empty() && gifts.is_empty() {
        list = list.push(
            text(crate::i18n::t!("clan-maps-incoming-empty"))
                .size(12)
                .style(muted),
        );
    }
    if !rows.is_empty() {
        list = list.push(
            text(crate::i18n::t!("clan-maps-offered-help"))
                .size(11)
                .style(muted),
        );
    }
    for row_state in rows {
        let offer = &row_state.offer;
        if row_state.gone {
            list = list.push(
                text(crate::i18n::t!("clan-maps-gone", "name" => &offer.area_name))
                    .size(12)
                    .style(muted),
            );
            continue;
        }
        let owner = offer
            .initiator_nickname
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-member"));
        let id = offer.id;
        let picker = pick_list(gift_folders, row_state.folder.clone(), move |choice| {
            clan(ClanMessage::FolderPicked(id, choice))
        })
        .placeholder(crate::i18n::t!("clan-maps-folder-placeholder"))
        .text_size(12.0)
        .width(150);
        let accept = row_state.folder.is_some() && !row_state.busy;
        let mut item = column![
            text(offer.area_name.clone()).size(13),
            text(crate::i18n::t!("clan-maps-offered-by", "owner" => owner))
                .size(11)
                .style(muted),
            row![
                space::horizontal(),
                picker,
                button(text(crate::i18n::t!("clan-maps-accept")).size(12))
                    .style(builtins::button::primary)
                    .on_press_maybe(accept.then_some(clan(ClanMessage::Accept(id)))),
                button(text(crate::i18n::t!("clan-maps-decline")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe((!row_state.busy).then_some(clan(ClanMessage::Decline(id)))),
            ]
            .spacing(8)
            .align_y(Vertical::Center),
        ]
        .spacing(4);
        if let Some(error) = &row_state.error {
            item = item.push(text(error.clone()).size(11).style(builtins::text::danger));
        }
        list = list.push(item);
    }
    if !gifts.is_empty() {
        list = list.push(
            text(crate::i18n::t!("clan-maps-given-help"))
                .size(11)
                .style(muted),
        );
        for row_state in gifts {
            list = list.push(gift_view(gift_folders, row_state, accepts_folders));
        }
    }
    super::modals::scrolling_body(
        list,
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-close")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::ModalDismissed),
        ],
    )
}

// ---------------------------------------------------------------------------
// A clan map's own actions
// ---------------------------------------------------------------------------

impl MapEditorWindow {
    /// Whether the viewer may `clan_action` the map: on a clan's map, as its
    /// actions allow; on any other map, as its owner.
    pub(super) fn may_manage_area(&self, area_id: AreaId, clan_action: &str) -> bool {
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| match area.meta().clan_id {
                Some(_) => area
                    .meta()
                    .actions
                    .as_ref()
                    .is_some_and(|actions| actions.contains(clan_action)),
                None => area.is_owned(),
            })
    }

    /// The clan that owns the map, on a clan's map.
    pub(super) fn area_clan(&self, area_id: AreaId) -> Option<Uuid> {
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .and_then(|area| area.meta().clan_id)
    }
}

/// Opens Move to folder for a clan's map: the clan's other folders the
/// viewer may put maps in.
pub(super) fn open_refile(
    window: &mut MapEditorWindow,
    area_id: AreaId,
    clan_id: Uuid,
) -> Update<Message, super::Event> {
    if !window.may_manage_area(area_id, action::REFILE_AREA) {
        return Update::none();
    }
    let atlas = window.mapper.get_current_atlas();
    let Some(area) = atlas.get_area(&area_id) else {
        return Update::none();
    };
    let current_atlas = area.meta().atlas_id;
    let targets = refile_targets(window, clan_id, current_atlas)
        .into_iter()
        .map(|(atlas_id, name)| {
            (
                smudgy_cloud::MapDestination::in_atlas(smudgy_cloud::MapStorage::Cloud, atlas_id),
                name,
            )
        })
        .collect();
    window.modal = Some(modals::Modal::MoveArea {
        area_id,
        area_name: area.get_name().to_string(),
        current: smudgy_cloud::MapDestination {
            storage: smudgy_cloud::MapStorage::Cloud,
            atlas_id: current_atlas,
        },
        targets,
        make_folder: false,
        new_folder: None,
    });
    Update::none()
}

/// Moves a clan's map to another of the clan's folders.
pub(super) fn refile(
    window: &mut MapEditorWindow,
    area_id: AreaId,
    atlas_id: Option<AtlasId>,
) -> Update<Message, super::Event> {
    let Some(atlas_id) = atlas_id else {
        return Update::none();
    };
    if !window.may_manage_area(area_id, action::REFILE_AREA) {
        return Update::none();
    }
    window.review_filing(super::filing::Request::Maps {
        ids: vec![area_id],
        destination: smudgy_cloud::MapDestination::in_atlas(
            smudgy_cloud::MapStorage::Cloud,
            atlas_id,
        ),
        multi: false,
        clan: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: u128, clan: u128, name: &str) -> AtlasListItem {
        AtlasListItem {
            id: AtlasId(Uuid::from_u128(id)),
            name: name.to_string(),
            created_at: chrono::Utc::now(),
            area_count: 0,
            rev: 1,
            is_owner: false,
            can_admin: false,
            owner_nickname: None,
            clan_id: Some(Uuid::from_u128(clan)),
            clan_name: None,
            actions: std::collections::BTreeSet::new(),
        }
    }

    fn map(id: u128, name: &str, clan: Option<u128>, atlas: Option<u128>) -> ListedMap {
        ListedMap {
            id: AreaId(Uuid::from_u128(id)),
            name: name.to_string(),
            clan_id: clan.map(Uuid::from_u128),
            atlas_id: atlas.map(|atlas| AtlasId(Uuid::from_u128(atlas))),
            atlas_name: None,
            owned: clan.is_none(),
            can_edit: true,
            enabled: true,
            in_family: false,
        }
    }

    #[test]
    fn clans_list_by_name_with_their_folders_and_unfiled_maps() {
        let (lantern, archivists) = (1, 2);
        let mut state = ClanState::default();
        state
            .names
            .insert(Uuid::from_u128(lantern), "Lantern Company".into());
        state
            .names
            .insert(Uuid::from_u128(archivists), "Archivists".into());
        let atlases = [
            folder(10, lantern, "Roads"),
            folder(11, lantern, "Towns"),
            folder(21, archivists, "Stacks"),
        ];
        let maps = [
            map(100, "Grove", Some(lantern), Some(11)),
            map(101, "Haven", Some(lantern), Some(10)),
            map(102, "My own", None, None),
            // A Member-owned map whose folder was deleted.
            map(103, "Hollow", Some(lantern), None),
        ];
        let groups = group_clans(&state, &atlases, &maps);

        let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
        assert_eq!(labels, ["Archivists", "Lantern Company"]);
        let lantern_group = &groups[1];
        let folders: Vec<(&str, Vec<&str>)> = lantern_group
            .folders
            .iter()
            .map(|folder| {
                (
                    folder.label.as_str(),
                    folder.areas.iter().map(|area| area.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            folders,
            [("Roads", vec!["Haven"]), ("Towns", vec!["Grove"])]
        );
        let unfiled: Vec<&str> = lantern_group
            .unfiled
            .iter()
            .map(|area| area.name.as_str())
            .collect();
        assert_eq!(unfiled, ["Hollow"]);
        assert!(groups[0].folders[0].areas.is_empty());
        // A personal map is in no clan.
        assert!(
            groups
                .iter()
                .flat_map(|group| &group.folders)
                .flat_map(|folder| &folder.areas)
                .all(|area| area.name != "My own")
        );
    }

    #[test]
    fn a_clan_map_whose_folder_is_not_listed_yet_still_shows() {
        let mut state = ClanState::default();
        state
            .names
            .insert(Uuid::from_u128(1), "Lantern Company".into());
        let mut haven = map(101, "Haven", Some(1), Some(10));
        haven.atlas_name = Some("Roads".into());
        let groups = group_clans(&state, &[], &[haven]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].folders[0].label, "Roads");
        assert_eq!(groups[0].incoming, 0);
    }

    #[test]
    fn handling_incoming_maps_follows_accept_transfer() {
        let clan_id = Uuid::from_u128(1);
        let summary = |is_owner: bool, actions: &[&str]| ClanSummary {
            id: clan_id,
            name: "Lantern Company".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            member_count: 2,
            is_owner,
            group_ids: Vec::new(),
            actions: actions.iter().map(ToString::to_string).collect(),
        };
        let folder = AtlasListItem {
            id: AtlasId(Uuid::from_u128(2)),
            name: "Roads".to_string(),
            created_at: chrono::Utc::now(),
            area_count: 0,
            rev: 1,
            is_owner: false,
            can_admin: false,
            owner_nickname: None,
            clan_id: Some(clan_id),
            clan_name: None,
            actions: [action::ACCEPT_FILING.to_string()].into(),
        };
        assert!(receives_transfers(&summary(true, &[]), &[]));
        assert!(receives_transfers(
            &summary(false, &[action::ACCEPT_TRANSFER]),
            &[]
        ));
        assert!(!receives_transfers(
            &summary(false, &[action::ACCEPT_FILING]),
            std::slice::from_ref(&folder)
        ));
        let accepting = AtlasListItem {
            actions: [action::ACCEPT_TRANSFER.to_string()].into(),
            ..folder
        };
        assert!(receives_transfers(&summary(false, &[]), &[accepting]));
    }

    fn gift(id: u128, name: &str) -> TransferView {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(id),
            "subject_kind": "area",
            "subject_name": name,
            "area_id": Uuid::from_u128(id + 100),
            "from_user_id": Uuid::from_u128(7),
            "to_clan_id": Uuid::from_u128(1),
            "status": "Offered",
            "created_at": "2026-10-06T09:00:00Z",
        }))
        .unwrap()
    }

    fn gift_row(id: u128, name: &str) -> GiftRow {
        GiftRow {
            offer: gift(id, name),
            folder: None,
            busy: false,
            gone: false,
            error: None,
        }
    }

    #[tokio::test]
    async fn incoming_gift_actions_fit_beside_long_names() {
        let folders = vec![FolderChoice {
            id: AtlasId(Uuid::from_u128(10)),
            name: "Roads".into(),
        }];
        let mut gift = gift_row(1, &"A long map name with several words ".repeat(3));
        gift.folder = Some(folders[0].clone());
        for size in [(340, 360), (600, 500)] {
            let messages = crate::widgets::dialog::tests::check_actions(
                gift_view(&folders, &gift, true),
                size,
                &[
                    crate::i18n::t!("clan-maps-accept"),
                    crate::i18n::t!("clan-maps-decline"),
                ],
                "incoming-gift",
            )
            .await;
            assert_eq!(messages.len(), 4);
            assert!(matches!(
                &messages[0],
                Message::Clan(ClanMessage::AcceptGift(_))
            ));
            assert!(matches!(
                &messages[1],
                Message::Clan(ClanMessage::DeclineGift(_))
            ));
        }
    }

    #[tokio::test]
    async fn incoming_dialog_keeps_close_visible_with_many_offers() {
        let modal = modals::Modal::Clan(Box::new(ClanModal::Incoming {
            clan_id: Uuid::from_u128(1),
            clan_name: "Lantern Company".into(),
            rows: (1..20)
                .map(|n| IncomingRow {
                    offer: offer(n, "The northern lands with a long name"),
                    folder: None,
                    busy: false,
                    gone: false,
                    error: None,
                })
                .collect(),
            gift_folders: Vec::new(),
            gifts: (20..40)
                .map(|n| gift_row(n, "The western lands with a long name"))
                .collect(),
            accepts_folders: true,
        }));
        for size in [(380, 360), (640, 500)] {
            let messages = crate::widgets::dialog::tests::check_actions(
                modal.view(),
                size,
                &[crate::i18n::t!("action-close")],
                "incoming-maps",
            )
            .await;
            assert_eq!(messages.len(), 2);
            assert!(
                messages
                    .iter()
                    .all(|m| matches!(m, Message::ModalDismissed))
            );
        }
    }

    /// An open Incoming maps dialog follows each refresh: a given map whose offer is gone
    /// (cancelled, or answered elsewhere) leaves, unless its row shows an error, which stays
    /// without its buttons; one given since appears.
    #[test]
    fn a_refresh_rebuilds_the_given_maps_in_an_open_dialog() {
        let roads = FolderChoice {
            id: AtlasId(Uuid::from_u128(10)),
            name: "Roads".to_string(),
        };
        let mut failed = gift_row(2, "Old Mill");
        failed.error = Some("could not accept".to_string());
        let mut busy = gift_row(3, "Harbor");
        busy.busy = true;
        let mut rows = vec![
            gift_row(1, "Lantern Docks"),
            failed,
            busy,
            gift_row(4, "Kept"),
        ];

        reconcile_gifts(&mut rows, &[gift(4, "Kept"), gift(5, "New")], Some(&roads));

        let seen: Vec<(&str, bool)> = rows
            .iter()
            .map(|row| (row.offer.subject_name.as_deref().unwrap_or(""), row.gone))
            .collect();
        assert_eq!(
            seen,
            [
                ("Old Mill", true),
                ("Harbor", false),
                ("Kept", false),
                ("New", false)
            ]
        );
        assert_eq!(rows[3].folder.as_ref(), Some(&roads));
    }

    fn offer(id: u128, name: &str) -> AreaOwnershipOffer {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(id),
            "area_id": Uuid::from_u128(id + 100),
            "area_name": name,
            "clan_id": Uuid::from_u128(1),
            "recipients": [],
            "ownership": "clan",
            "initiator_id": Uuid::from_u128(7),
            "created_at": "2026-10-06T09:00:00Z",
        }))
        .unwrap()
    }

    /// The maps offered to the clan to own follow a refresh the same way.
    #[test]
    fn a_refresh_rebuilds_the_offered_maps_in_an_open_dialog() {
        let row = |id: u128, name: &str| IncomingRow {
            offer: offer(id, name),
            folder: None,
            busy: false,
            gone: false,
            error: None,
        };
        let mut failed = row(2, "Old Mill");
        failed.error = Some("offline".to_string());
        let mut rows = vec![row(1, "Withdrawn"), failed, row(3, "Kept")];

        reconcile_offers(&mut rows, &[offer(3, "Kept"), offer(4, "New")], None);

        let seen: Vec<(&str, bool)> = rows
            .iter()
            .map(|row| (row.offer.area_name.as_str(), row.gone))
            .collect();
        assert_eq!(seen, [("Old Mill", true), ("Kept", false), ("New", false)]);
    }

    #[test]
    fn transfers_to_a_clan_wait_beside_its_offered_maps() {
        let clan_id = Uuid::from_u128(1);
        let offer: TransferView = serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(5),
            "subject_kind": "area",
            "area_id": Uuid::from_u128(6),
            "from_user_id": Uuid::from_u128(7),
            "to_clan_id": clan_id,
            "to_clan_name": "Lantern Company",
            "status": "Offered",
            "created_at": "2026-10-06T09:00:00Z",
        }))
        .unwrap();
        let mut state = ClanState::default();
        assert_eq!(state.waiting(clan_id), 0);
        state.transfers.insert(clan_id, vec![offer]);
        assert_eq!(state.waiting(clan_id), 1);
        state
            .area_offers
            .insert(clan_id, vec![self::offer(9, "Grove")]);
        assert_eq!(state.waiting(clan_id), 2);
    }
}
