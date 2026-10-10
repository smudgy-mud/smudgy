//! Where a map the editor makes is filed: one of the viewer's own folders in
//! the chosen storage, or a new folder they name. The editor never makes a
//! map outside a folder: New map, Duplicate, Copy to my maps, and saving a
//! session map all file it in one.

use iced::Task;
use iced::widget::{column, pick_list, radio, text, text_input};
use smudgy_cloud::{AtlasId, AtlasListItem, MapDestination, MapStorage, Mapper};

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::update::Update;

use super::modals::{Modal, section_label};
use super::multi_select::{MoveDialog, MultiDialog, MultiMessage};
use super::{Event, FolderKey, MapEditorWindow, Message};

/// The folder a map goes in: one of the viewer's folders, or a new one they
/// name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPick {
    Existing(AtlasId),
    New,
}

/// One of the viewer's own folders and where it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnFolder {
    pub id: AtlasId,
    pub name: String,
    pub storage: MapStorage,
}

/// The folders the viewer may file their maps in: their own, on this device
/// or in the cloud — not a clan's, and not one they only administer —
/// name-sorted. `storage_of` names each folder's storage; folders it doesn't
/// know are left out.
#[must_use]
pub fn own_folders(
    atlases: &[AtlasListItem],
    storage_of: impl Fn(&AtlasId) -> Option<MapStorage>,
) -> Vec<OwnFolder> {
    let mut folders: Vec<OwnFolder> = atlases
        .iter()
        .filter(|atlas| atlas.is_owner && atlas.clan_id.is_none())
        .filter_map(|atlas| {
            let storage =
                storage_of(&atlas.id).filter(|storage| *storage != MapStorage::Session)?;
            Some(OwnFolder {
                id: atlas.id,
                name: atlas.name.clone(),
                storage,
            })
        })
        .collect();
    sort_folders(&mut folders);
    folders
}

fn sort_folders(folders: &mut [OwnFolder]) {
    folders.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Which storages the picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageChoice {
    /// The cloud and this device (signed in).
    Both,
    /// This device only, because the viewer is signed out; a note says so.
    SignedOut,
    /// Only the storage given: a copy lives in the cloud, and a folder's own
    /// New map in that folder's storage.
    Fixed,
}

/// A change in the picker.
#[derive(Debug, Clone)]
pub enum PickerMessage {
    Storage(MapStorage),
    Picked(FolderPick),
    NewName(String),
}

/// One entry of the folder dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    pick: FolderPick,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// The folder field of a dialog that makes a map.
#[derive(Debug, Clone)]
pub struct FolderPicker {
    folders: Vec<OwnFolder>,
    storage: MapStorage,
    storage_choice: StorageChoice,
    pick: FolderPick,
    new_name: String,
    /// The folder was given (a folder's own New map) and can't be changed.
    fixed: bool,
}

impl FolderPicker {
    /// The list header's New map: the open map's folder when it is one of the
    /// viewer's, else the first folder in the default storage (the cloud when
    /// signed in), else the first on this device, else a new folder to name.
    #[must_use]
    pub fn for_new_map(
        folders: Vec<OwnFolder>,
        signed_in: bool,
        open_folder: Option<AtlasId>,
    ) -> Self {
        let preferred = if signed_in {
            MapStorage::Cloud
        } else {
            MapStorage::Local
        };
        let reachable = |folder: &&OwnFolder| signed_in || folder.storage == MapStorage::Local;
        let open = open_folder
            .and_then(|id| folders.iter().find(|folder| folder.id == id))
            .filter(reachable);
        let has = |storage: MapStorage| folders.iter().any(|folder| folder.storage == storage);
        let storage = match open {
            Some(folder) => folder.storage,
            None if has(preferred) => preferred,
            None if signed_in && has(MapStorage::Local) => MapStorage::Local,
            None => preferred,
        };
        let pick = default_pick(&folders, storage, open.map(|folder| folder.id));
        Self {
            folders,
            storage,
            storage_choice: if signed_in {
                StorageChoice::Both
            } else {
                StorageChoice::SignedOut
            },
            pick,
            new_name: String::new(),
            fixed: false,
        }
    }

    /// A folder's own New map: that folder, and no other.
    #[must_use]
    pub fn in_folder(folder: OwnFolder) -> Self {
        Self {
            storage: folder.storage,
            storage_choice: StorageChoice::Fixed,
            pick: FolderPick::Existing(folder.id),
            folders: vec![folder],
            new_name: String::new(),
            fixed: true,
        }
    }

    /// A folder in `storage` only, `preferred` first when it is there.
    #[must_use]
    pub fn in_storage(
        folders: Vec<OwnFolder>,
        storage: MapStorage,
        preferred: Option<AtlasId>,
    ) -> Self {
        let pick = default_pick(&folders, storage, preferred);
        Self {
            folders,
            storage,
            storage_choice: StorageChoice::Fixed,
            pick,
            new_name: String::new(),
            fixed: false,
        }
    }

    pub fn update(&mut self, message: PickerMessage) {
        match message {
            PickerMessage::Storage(storage) => {
                if self.storage_choice == StorageChoice::Both
                    && storage != MapStorage::Session
                    && storage != self.storage
                {
                    self.storage = storage;
                    self.pick = default_pick(&self.folders, storage, None);
                }
            }
            PickerMessage::Picked(pick) => {
                let valid = match pick {
                    FolderPick::Existing(id) => self.in_storage_folders().any(|f| f.id == id),
                    FolderPick::New => true,
                };
                if !self.fixed && valid {
                    self.pick = pick;
                }
            }
            PickerMessage::NewName(name) => self.new_name = name,
        }
    }

    /// Where the map is stored.
    #[must_use]
    pub fn storage(&self) -> MapStorage {
        self.storage
    }

    /// The chosen existing folder; `None` while a new one is to be made.
    #[must_use]
    pub fn chosen(&self) -> Option<AtlasId> {
        match self.pick {
            FolderPick::Existing(id) => Some(id),
            FolderPick::New => None,
        }
    }

    /// The new folder's name, once one is to be made and named.
    #[must_use]
    pub fn new_folder_name(&self) -> Option<String> {
        let name = self.new_name.trim();
        (self.pick == FolderPick::New && !name.is_empty()).then(|| name.to_string())
    }

    /// Whether the picker names a folder: an existing one, or a named new one.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.chosen().is_some() || self.new_folder_name().is_some()
    }

    /// The new folder now exists: choose it, so trying again doesn't make
    /// another.
    pub fn folder_made(&mut self, folder: OwnFolder) {
        if folder.storage != self.storage {
            return;
        }
        self.pick = FolderPick::Existing(folder.id);
        self.new_name.clear();
        if !self.folders.iter().any(|known| known.id == folder.id) {
            self.folders.push(folder);
            sort_folders(&mut self.folders);
        }
    }

    fn in_storage_folders(&self) -> impl Iterator<Item = &OwnFolder> {
        self.folders
            .iter()
            .filter(move |folder| folder.storage == self.storage)
    }

    /// The storage choice and the folder field. `submit` is sent when Enter
    /// is pressed in the new folder's name.
    pub fn view(&self, submit: Message) -> ThemedElement<'_, Message> {
        let mut body = column![].spacing(10);
        match self.storage_choice {
            StorageChoice::Both => {
                body = body.push(
                    column![
                        section_label(crate::i18n::t!("mapper-save-in")),
                        radio(
                            crate::i18n::t!("mapper-save-cloud"),
                            MapStorage::Cloud,
                            Some(self.storage),
                            |storage| picker(PickerMessage::Storage(storage)),
                        )
                        .size(14)
                        .text_size(13),
                        radio(
                            crate::i18n::t!("mapper-save-local"),
                            MapStorage::Local,
                            Some(self.storage),
                            |storage| picker(PickerMessage::Storage(storage)),
                        )
                        .size(14)
                        .text_size(13),
                    ]
                    .spacing(4),
                );
            }
            StorageChoice::SignedOut => {
                body = body.push(
                    text(crate::i18n::t!("mapper-save-local-signed-out"))
                        .size(11)
                        .style(muted),
                );
            }
            StorageChoice::Fixed => {}
        }

        let mut field = column![section_label(crate::i18n::t!("mapper-folder-section"))].spacing(6);
        if self.fixed {
            let name = self
                .folders
                .first()
                .map(|folder| folder.name.clone())
                .unwrap_or_default();
            field = field.push(text(name).size(13));
            return body.push(field).into();
        }

        let mut choices: Vec<Choice> = self
            .in_storage_folders()
            .map(|folder| Choice {
                pick: FolderPick::Existing(folder.id),
                label: folder.name.clone(),
            })
            .collect();
        if choices.is_empty() {
            field = field.push(
                text(crate::i18n::t!("mapper-folder-none-yet"))
                    .size(11)
                    .style(muted),
            );
        } else {
            choices.push(Choice {
                pick: FolderPick::New,
                label: crate::i18n::t!("mapper-folder-new-option"),
            });
            let selected = choices
                .iter()
                .find(|choice| choice.pick == self.pick)
                .cloned();
            field = field.push(
                pick_list(choices, selected, |choice: Choice| {
                    picker(PickerMessage::Picked(choice.pick))
                })
                .text_size(13.0)
                .width(iced::Length::Fill),
            );
        }
        if self.pick == FolderPick::New {
            field = field.push(
                text_input(
                    crate::i18n::ts!("mapper-folder-name-placeholder"),
                    &self.new_name,
                )
                .size(14)
                .on_input(|name| picker(PickerMessage::NewName(name)))
                .on_submit(submit),
            );
        }
        body.push(field).into()
    }
}

/// `preferred` when it is in `storage`, else the first folder there, else a
/// new folder.
fn default_pick(
    folders: &[OwnFolder],
    storage: MapStorage,
    preferred: Option<AtlasId>,
) -> FolderPick {
    let mut here = folders.iter().filter(|folder| folder.storage == storage);
    preferred
        .filter(|id| {
            folders
                .iter()
                .any(|folder| folder.id == *id && folder.storage == storage)
        })
        .or_else(|| here.next().map(|folder| folder.id))
        .map_or(FolderPick::New, FolderPick::Existing)
}

fn picker(message: PickerMessage) -> Message {
    Message::FolderPicker(message)
}

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

/// The new-folder form of the Move dialog: a name, and where to keep it.
#[derive(Debug, Clone)]
pub struct NewFolderForm {
    pub name: String,
    pub storage: MapStorage,
    pub cloud_available: bool,
    pub busy: bool,
    pub error: Option<String>,
}

impl NewFolderForm {
    /// A blank form keeping the folder in `storage` (this device when the
    /// cloud isn't available).
    #[must_use]
    pub fn new(storage: MapStorage, cloud_available: bool) -> Self {
        let storage = match storage {
            MapStorage::Cloud if cloud_available => MapStorage::Cloud,
            MapStorage::Session if cloud_available => MapStorage::Cloud,
            _ => MapStorage::Local,
        };
        Self {
            name: String::new(),
            storage,
            cloud_available,
            busy: false,
            error: None,
        }
    }

    /// The name, once there is one.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        let name = self.name.trim();
        (!name.is_empty()).then(|| name.to_string())
    }

    pub fn set_storage(&mut self, storage: MapStorage) {
        if storage == MapStorage::Local || (storage == MapStorage::Cloud && self.cloud_available) {
            self.storage = storage;
        }
    }
}

/// A change in the Move dialog's new-folder form.
#[derive(Debug, Clone)]
pub enum NewFolderMessage {
    Opened,
    Name(String),
    Storage(MapStorage),
    Confirmed,
}

/// Makes the folder a dialog named, reporting back as
/// [`Message::NewFolderMade`].
pub(super) fn make_folder(mapper: Mapper, name: String, storage: MapStorage) -> Task<Message> {
    Task::perform(
        async move {
            mapper
                .create_atlas_at(name, storage)
                .await
                .map(|atlas| OwnFolder {
                    id: atlas.id,
                    name: atlas.name,
                    storage,
                })
                .map_err(|error| display_error(&error))
        },
        Message::NewFolderMade,
    )
}

impl MapEditorWindow {
    /// A folder named in the open dialog was made: the dialog carries on
    /// into it (making the map, the copy, or the move). On failure the
    /// dialog says why and stays open.
    pub(super) fn new_folder_made(
        &mut self,
        result: Result<OwnFolder, String>,
    ) -> Update<Message, Event> {
        let folder = match result {
            Ok(folder) => folder,
            Err(failure) => {
                match &mut self.modal {
                    Some(Modal::CreateArea { error, busy, .. }) => {
                        *error = Some(failure);
                        *busy = false;
                    }
                    Some(Modal::CopyArea(dialog)) => {
                        dialog.error = Some(failure);
                        dialog.busy = false;
                    }
                    Some(Modal::ConfirmDeleteAtlas { error, busy, .. }) => {
                        *error = Some(failure);
                        *busy = false;
                    }
                    Some(Modal::Multi(dialog)) => {
                        if let MultiDialog::Move(dialog) = dialog.as_mut()
                            && let Some(form) = &mut dialog.new_folder
                        {
                            form.error = Some(failure);
                            form.busy = false;
                        }
                    }
                    Some(Modal::MoveArea {
                        new_folder: Some(form),
                        ..
                    }) => {
                        form.error = Some(failure);
                        form.busy = false;
                    }
                    _ => {}
                }
                return Update::none();
            }
        };
        self.collapsed_folders.remove(&FolderKey::Atlas(folder.id));
        // Like any new folder, it shows on the server it was made from.
        let associated = self.associate_new_atlas(folder.id);
        let refetch = self.fetch_atlases();
        let destination = MapDestination::in_atlas(folder.storage, folder.id);
        let made = folder.id;
        let next = match &mut self.modal {
            Some(Modal::CreateArea {
                folder: picker,
                busy,
                ..
            }) => {
                picker.folder_made(folder);
                *busy = false;
                (picker.chosen() == Some(made)).then_some(Message::CreateAreaConfirmed)
            }
            Some(Modal::CopyArea(dialog)) => {
                dialog.folder.folder_made(folder);
                dialog.busy = false;
                (dialog.folder.chosen() == Some(made)).then_some(Message::CopyAreaConfirmed)
            }
            Some(Modal::ConfirmDeleteAtlas {
                folder: Some(picker),
                busy,
                ..
            }) => {
                picker.folder_made(folder);
                *busy = false;
                (picker.chosen() == Some(made)).then_some(Message::DeleteAtlasConfirmed)
            }
            Some(Modal::MoveArea {
                area_id,
                new_folder: Some(form),
                ..
            }) if form.busy => Some(Message::MoveAreaTo {
                area: *area_id,
                destination,
            }),
            Some(Modal::Multi(dialog))
                if matches!(
                    dialog.as_ref(),
                    MultiDialog::Move(MoveDialog {
                        new_folder: Some(NewFolderForm { busy: true, .. }),
                        ..
                    })
                ) =>
            {
                Some(Message::Multi(MultiMessage::MoveTo(destination)))
            }
            _ => None,
        };
        let mut update = next.map_or_else(Update::none, |message| self.update(message));
        update.task = Task::batch([refetch, update.task]);
        update.event = associated.or(update.event);
        update
    }

    /// The Move dialog's new-folder form: open it, fill it in, and make the
    /// folder (the move follows once it exists).
    pub(super) fn update_move_new_folder(
        &mut self,
        message: NewFolderMessage,
    ) -> Update<Message, Event> {
        let cloud_available = self.cloud.snapshot.get().signed_in;
        let Some(Modal::MoveArea {
            current,
            make_folder: allowed,
            new_folder,
            ..
        }) = &mut self.modal
        else {
            return Update::none();
        };
        match message {
            NewFolderMessage::Opened => {
                if *allowed && new_folder.is_none() {
                    *new_folder = Some(NewFolderForm::new(current.storage, cloud_available));
                }
            }
            NewFolderMessage::Name(name) => {
                if let Some(form) = new_folder {
                    form.name = name;
                }
            }
            NewFolderMessage::Storage(storage) => {
                if let Some(form) = new_folder {
                    form.set_storage(storage);
                }
            }
            NewFolderMessage::Confirmed => {
                let Some(form) = new_folder else {
                    return Update::none();
                };
                let Some(name) = form.name().filter(|_| !form.busy) else {
                    return Update::none();
                };
                form.busy = true;
                form.error = None;
                return Update::with_task(make_folder(self.mapper.clone(), name, form.storage));
            }
        }
        Update::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn folder(id: u128, name: &str, storage: MapStorage) -> OwnFolder {
        OwnFolder {
            id: AtlasId(Uuid::from_u128(id)),
            name: name.to_string(),
            storage,
        }
    }

    fn atlas(id: u128, name: &str, is_owner: bool, clan: Option<u128>) -> AtlasListItem {
        AtlasListItem {
            id: AtlasId(Uuid::from_u128(id)),
            name: name.to_string(),
            created_at: chrono::Utc::now(),
            area_count: 0,
            rev: 1,
            is_owner,
            can_admin: true,
            owner_nickname: None,
            clan_id: clan.map(Uuid::from_u128),
            clan_name: None,
            actions: std::collections::BTreeSet::new(),
        }
    }

    #[test]
    fn own_folders_leave_out_clan_and_administered_folders() {
        let atlases = [
            atlas(1, "roads", true, None),
            atlas(2, "Clan towns", false, Some(9)),
            atlas(3, "Their maps", false, None),
            atlas(4, "Cities", true, None),
            atlas(5, "Unknown storage", true, None),
        ];
        let folders = own_folders(&atlases, |id| match id.0.as_u128() {
            1 => Some(MapStorage::Local),
            5 => None,
            _ => Some(MapStorage::Cloud),
        });
        assert_eq!(
            folders,
            [
                folder(4, "Cities", MapStorage::Cloud),
                folder(1, "roads", MapStorage::Local),
            ]
        );
    }

    #[test]
    fn new_map_defaults_to_the_open_maps_folder() {
        let folders = vec![
            folder(1, "Cities", MapStorage::Cloud),
            folder(2, "Roads", MapStorage::Local),
        ];
        let picker = FolderPicker::for_new_map(folders, true, Some(AtlasId(Uuid::from_u128(2))));
        assert_eq!(picker.storage(), MapStorage::Local);
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(2))));
    }

    #[test]
    fn new_map_falls_back_to_a_storage_holding_folders() {
        // Signed in, but every folder is on this device.
        let picker =
            FolderPicker::for_new_map(vec![folder(2, "Roads", MapStorage::Local)], true, None);
        assert_eq!(picker.storage(), MapStorage::Local);
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(2))));

        // Signed out, a cloud folder is out of reach.
        let picker =
            FolderPicker::for_new_map(vec![folder(1, "Cities", MapStorage::Cloud)], false, None);
        assert_eq!(picker.storage(), MapStorage::Local);
        assert_eq!(picker.chosen(), None);
        assert!(!picker.ready(), "a new folder needs a name first");
    }

    #[test]
    fn no_folders_asks_for_a_new_one() {
        let mut picker = FolderPicker::for_new_map(Vec::new(), true, None);
        assert_eq!(picker.storage(), MapStorage::Cloud);
        assert!(!picker.ready());
        picker.update(PickerMessage::NewName("  Midgaard  ".into()));
        assert_eq!(picker.new_folder_name().as_deref(), Some("Midgaard"));
        assert!(picker.ready());
        // Once made, the new folder is the choice.
        picker.folder_made(folder(7, "Midgaard", MapStorage::Cloud));
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(7))));
        assert_eq!(picker.new_folder_name(), None);
    }

    #[test]
    fn switching_storage_picks_a_folder_there() {
        let folders = vec![
            folder(1, "Cities", MapStorage::Cloud),
            folder(2, "Roads", MapStorage::Local),
        ];
        let mut picker = FolderPicker::for_new_map(folders, true, None);
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(1))));
        picker.update(PickerMessage::Storage(MapStorage::Local));
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(2))));
        // A folder in the other storage can't be picked.
        picker.update(PickerMessage::Picked(FolderPick::Existing(AtlasId(
            Uuid::from_u128(1),
        ))));
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(2))));
        picker.update(PickerMessage::Storage(MapStorage::Session));
        assert_eq!(picker.storage(), MapStorage::Local);
    }

    #[test]
    fn a_folders_own_new_map_stays_in_it() {
        let mut picker = FolderPicker::in_folder(folder(3, "Roads", MapStorage::Local));
        picker.update(PickerMessage::Storage(MapStorage::Cloud));
        picker.update(PickerMessage::Picked(FolderPick::New));
        assert_eq!(picker.storage(), MapStorage::Local);
        assert_eq!(picker.chosen(), Some(AtlasId(Uuid::from_u128(3))));
    }

    #[test]
    fn a_new_folder_form_keeps_to_reachable_storage() {
        let form = NewFolderForm::new(MapStorage::Session, false);
        assert_eq!(form.storage, MapStorage::Local);
        let mut form = NewFolderForm::new(MapStorage::Session, true);
        assert_eq!(form.storage, MapStorage::Cloud);
        form.set_storage(MapStorage::Session);
        assert_eq!(form.storage, MapStorage::Cloud);
        assert_eq!(form.name(), None);
    }
}
