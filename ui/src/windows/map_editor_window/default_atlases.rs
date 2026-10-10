//! The editor's server's default folders: where that server's scripts put a
//! new map that names no folder, one per storage (see
//! [`smudgy_core::models::default_atlases`]). An own folder's ⋯ menu and its
//! panel make it the default for its storage, and the map list marks the
//! defaults.

use iced::Task;
use iced::widget::{button, text};
use smudgy_cloud::{AtlasId, MapStorage};
use smudgy_core::models::default_atlases::{default_atlas, set_default_atlas};

use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::panels;
use super::{MapEditorWindow, Message};

/// A server's default folder in each durable storage, as its settings name
/// them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DefaultAtlases {
    pub cloud: Option<AtlasId>,
    pub local: Option<AtlasId>,
}

impl DefaultAtlases {
    /// `server`'s defaults; none without a server, or when its settings
    /// can't be read.
    #[must_use]
    pub fn load(server: Option<&str>) -> Self {
        let Some(server) = server else {
            return Self::default();
        };
        let read = |storage| {
            default_atlas(server, storage).unwrap_or_else(|error| {
                log::warn!(
                    "map editor: couldn't read {server}'s default {storage} folder: {error:#}"
                );
                None
            })
        };
        Self {
            cloud: read(MapStorage::Cloud),
            local: read(MapStorage::Local),
        }
    }

    /// The default in `storage`; a session map has none.
    #[must_use]
    pub const fn get(&self, storage: MapStorage) -> Option<AtlasId> {
        match storage {
            MapStorage::Cloud => self.cloud,
            MapStorage::Local => self.local,
            MapStorage::Session => None,
        }
    }

    /// Whether `atlas_id` is the default in either storage.
    #[must_use]
    pub fn contains(&self, atlas_id: AtlasId) -> bool {
        self.cloud == Some(atlas_id) || self.local == Some(atlas_id)
    }
}

/// The storage `atlas_id` is kept in.
fn storage_of(window: &MapEditorWindow, atlas_id: AtlasId) -> MapStorage {
    if window.mapper.local_atlas_ids().contains(&atlas_id) {
        MapStorage::Local
    } else {
        MapStorage::Cloud
    }
}

/// For one of the viewer's own folders (not a clan's, not one they only
/// administer) in an editor with a server: that server, and whether the
/// folder is its default for the folder's storage. `None` otherwise.
fn status(window: &MapEditorWindow, atlas_id: AtlasId) -> Option<(&str, bool)> {
    let server = window.server_name.as_deref()?;
    let own = window
        .atlases
        .iter()
        .any(|atlas| atlas.id == atlas_id && atlas.is_own());
    own.then(|| {
        let default = window.default_atlases.get(storage_of(window, atlas_id));
        (server, default == Some(atlas_id))
    })
}

/// "Use for new maps on <server>".
fn use_label(server: &str) -> String {
    crate::i18n::t!("area-list-use-for-new-maps", "server" => server)
}

/// The ⋯ menu entry that makes one of the viewer's own folders the default
/// for its storage: `None` without a server, or when it already is.
pub(super) fn menu_entry(window: &MapEditorWindow, atlas_id: AtlasId) -> Option<(String, Message)> {
    let (server, is_default) = status(window, atlas_id)?;
    (!is_default).then(|| (use_label(server), Message::UseForNewMaps(atlas_id)))
}

/// The atlas panel's line for one of the viewer's own folders: saying it is
/// where the server's new maps go, or offering to make it so.
pub(super) fn panel_line(
    window: &MapEditorWindow,
    atlas_id: AtlasId,
) -> Option<ThemedElement<'static, Message>> {
    let (server, is_default) = status(window, atlas_id)?;
    Some(if is_default {
        panels::note(crate::i18n::t!("area-list-default-tip", "server" => server))
    } else {
        button(text(use_label(server)).size(12))
            .style(builtins::button::secondary)
            .padding([4, 10])
            .on_press(Message::UseForNewMaps(atlas_id))
            .into()
    })
}

/// Makes `atlas_id` the editor's server's default for the storage it is in.
pub(super) fn use_for_new_maps(window: &MapEditorWindow, atlas_id: AtlasId) -> Task<Message> {
    let Some(server) = window.server_name.clone() else {
        return Task::none();
    };
    let storage = storage_of(window, atlas_id);
    Task::perform(
        async move {
            set_default_atlas(&server, storage, atlas_id)
                .await
                .map_err(|error| format!("{error:#}"))
        },
        Message::DefaultAtlasSet,
    )
}

/// The default changed (or didn't): show it, or say why not.
pub(super) fn set(window: &mut MapEditorWindow, result: Result<(), String>) {
    match result {
        Ok(()) => window.refresh_default_atlases(),
        Err(error) => {
            window.editor_notice = Some((
                std::time::Instant::now(),
                crate::i18n::t!("area-list-use-for-new-maps-failed", "error" => error),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    #[test]
    fn each_storage_has_its_own_default() {
        let cloud = AtlasId(Uuid::from_u128(1));
        let local = AtlasId(Uuid::from_u128(2));
        let defaults = DefaultAtlases {
            cloud: Some(cloud),
            local: Some(local),
        };
        assert_eq!(defaults.get(MapStorage::Cloud), Some(cloud));
        assert_eq!(defaults.get(MapStorage::Local), Some(local));
        assert_eq!(defaults.get(MapStorage::Session), None);
        assert!(defaults.contains(cloud) && defaults.contains(local));
        assert!(!defaults.contains(AtlasId(Uuid::from_u128(3))));
        // No server, no defaults.
        assert_eq!(DefaultAtlases::load(None), DefaultAtlases::default());
    }
}
