//! Each server's default atlases: where a script's new saved map goes when
//! it names no atlas. A server keeps one per durable storage in its settings
//! on this device ([`ServerConfig::default_cloud_atlas`] and
//! [`ServerConfig::default_local_atlas`]).
//!
//! The first time one is needed, and whenever the setting names an atlas
//! that no longer exists in that storage, [`resolve_default_atlas`] adopts
//! the oldest of the viewer's own atlases there with the default name, so a
//! second device with no setting yet uses the one the first device made, or
//! makes one; then it stores the id. From then on the id is the identity:
//! renaming the atlas keeps it the default, and [`set_default_atlas`] picks
//! another.
//!
//! Cloud atlases are the account's and shown per server, so the cloud
//! default carries the server's name ("Loose maps (Arctic)"); a server's
//! local maps are its own, so the local default is plain "Loose maps". The
//! names are the same in every language, so each device finds the same one.
//!
//! [`ServerConfig::default_cloud_atlas`]: super::server::ServerConfig::default_cloud_atlas
//! [`ServerConfig::default_local_atlas`]: super::server::ServerConfig::default_local_atlas

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use anyhow::{Context, Result, bail};
use smudgy_cloud::{AtlasId, MapStorage, Mapper, oldest_own_atlas_named};

use super::server::{ServerCas, load_server, update_server_if_unchanged};

/// The name loose maps' atlases start from, the same in every language.
pub const LOOSE_MAPS_FOLDER: &str = "Loose maps";

/// How many times storing a setting reads the server's settings again
/// after another write changed them first.
const WRITE_ATTEMPTS: usize = 8;

/// The name `server`'s default atlas in `storage` is made with.
#[must_use]
pub fn default_atlas_name(server: &str, storage: MapStorage) -> String {
    match storage {
        MapStorage::Cloud => format!("{LOOSE_MAPS_FOLDER} ({server})"),
        MapStorage::Local | MapStorage::Session => LOOSE_MAPS_FOLDER.to_string(),
    }
}

/// A server's default atlas in one storage, as [`resolve_default_atlas`]
/// found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultAtlas {
    pub id: AtlasId,
    /// Made just now: no atlas was set or had the default name.
    pub created: bool,
}

type Gate = Arc<tokio::sync::Mutex<()>>;

/// One gate per server and storage, shared by every session and editor, so
/// two resolutions never both make an atlas.
static GATES: LazyLock<Mutex<HashMap<(String, MapStorage), Gate>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn gate(server: &str, storage: MapStorage) -> Gate {
    GATES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry((server.to_string(), storage))
        .or_default()
        .clone()
}

/// A storage as an error names it.
const fn place(storage: MapStorage) -> &'static str {
    match storage {
        MapStorage::Cloud => "cloud",
        MapStorage::Local => "local",
        MapStorage::Session => "session",
    }
}

fn durable(storage: MapStorage) -> Result<()> {
    if storage == MapStorage::Session {
        bail!("session maps go in no atlas");
    }
    Ok(())
}

/// The atlas `server` has set for new maps in `storage`, as its settings
/// hold it; whether that atlas still exists isn't checked.
///
/// # Errors
/// The server's settings can't be read.
pub fn default_atlas(server: &str, storage: MapStorage) -> Result<Option<AtlasId>> {
    Ok(load_server(server)?.config.default_atlas(storage))
}

/// Makes `atlas` the one `server`'s new maps in `storage` go in.
///
/// # Errors
/// `storage` is the session's, or the server's settings can't be read or
/// written.
pub async fn set_default_atlas(server: &str, storage: MapStorage, atlas: AtlasId) -> Result<()> {
    durable(storage)?;
    let gate = gate(server, storage);
    let _turn = gate.lock().await;
    store(server, storage, atlas)
}

/// The atlas `server`'s new maps in `storage` go in, read through `mapper`:
/// the one its settings name while it is still the viewer's own there, else
/// the oldest of the viewer's own atlases there with the default name, else
/// one made now. The result is stored in the server's settings.
/// Resolutions of one server's storage take turns across the process.
///
/// `mapper` must hold `storage`'s atlases: a session's mapper holds the
/// cloud's and its own server's local ones.
///
/// # Errors
/// `storage` is the session's; the server's settings can't be read or
/// written; or the storage's atlases can't be listed or one can't be made.
/// A cloud whose atlases can't be listed is an error, never a reason to make
/// another.
pub async fn resolve_default_atlas(
    mapper: &Mapper,
    server: &str,
    storage: MapStorage,
) -> Result<DefaultAtlas> {
    durable(storage)?;
    let gate = gate(server, storage);
    let _turn = gate.lock().await;
    let set = default_atlas(server, storage)?;
    // The mapper's last inventory has it in this storage: no need to read
    // the list again.
    if let Some(id) = set
        && mapper.atlas_storage(&id) == Some(storage)
    {
        return Ok(DefaultAtlas { id, created: false });
    }
    let atlases = mapper
        .list_atlases_in(storage)
        .await
        .with_context(|| format!("couldn't list the {} atlases", place(storage)))?;
    if let Some(id) = set
        && atlases.iter().any(|atlas| atlas.id == id && atlas.is_own())
    {
        return Ok(DefaultAtlas { id, created: false });
    }
    let name = default_atlas_name(server, storage);
    let found = match oldest_own_atlas_named(&atlases, &name) {
        Some(id) => DefaultAtlas { id, created: false },
        None => DefaultAtlas {
            id: mapper
                .create_atlas_at(name.clone(), storage)
                .await
                .with_context(|| format!("couldn't make the {} atlas “{name}”", place(storage)))?
                .id,
            created: true,
        },
    };
    store(server, storage, found.id)?;
    Ok(found)
}

/// Writes `atlas` into `server`'s settings for `storage`, reading them again
/// when another write changed them first.
fn store(server: &str, storage: MapStorage, atlas: AtlasId) -> Result<()> {
    for _ in 0..WRITE_ATTEMPTS {
        let current = load_server(server)?;
        if current.config.default_atlas(storage) == Some(atlas) {
            return Ok(());
        }
        let mut config = current.config.clone();
        config.set_default_atlas(storage, Some(atlas));
        if let ServerCas::Applied(_) = update_server_if_unchanged(&current, config)? {
            return Ok(());
        }
    }
    bail!(
        "the settings of server '{server}' kept changing, so its {} atlas wasn't saved",
        place(storage)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_default_is_named_for_its_server() {
        assert_eq!(
            default_atlas_name("Arctic", MapStorage::Cloud),
            "Loose maps (Arctic)"
        );
        assert_eq!(
            default_atlas_name("Arctic", MapStorage::Local),
            "Loose maps"
        );
    }
}
