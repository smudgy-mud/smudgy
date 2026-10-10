//! TEMPORARY: remove this module, and every `// TEMPORARY(0.6.x)` call site,
//! once the 0.6.x series ends.
//!
//! Maps can no longer be made outside a folder, but earlier builds made
//! them. Once a session, when a map editor first has the viewer's map and
//! folder inventory, this files every map the viewer owns that sits in no
//! folder, so that no map shows on a different set of servers than before:
//!
//! - a map on this device goes in the editor's server's local default
//!   folder, since this device's maps are that server's own;
//! - a cloud map goes by the servers it was shown on. A map shown on one
//!   server goes in that server's cloud default folder; a map shown on every
//!   server goes in a "Loose maps" folder shown on every server; a map shown
//!   on several (or on a server that no longer exists) goes in a folder named
//!   for them, "Loose maps (A, B)", shown on exactly those.
//!
//! Running it again changes nothing: a filed map is no longer loose, a
//! server's default folder is the one its settings name, and the others are
//! found again by name and servers. Success is quiet; a failure shows a
//! notice, and the next session tries again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use iced::Task;
use smudgy_cloud::relocation::RELOCATION_IN_PROGRESS_SUFFIX;
use smudgy_cloud::{Area, AreaId, AtlasId, AtlasListItem, MapStorage, Mapper};
use smudgy_core::models::default_atlases::{LOOSE_MAPS_FOLDER, resolve_default_atlas};
use smudgy_core::models::map_scopes::{MapScopes, ScopeDelta, ScopeState};

use crate::update::Update;

use super::{Event, FolderKey, MapEditorWindow, Message};

/// One map as the migration reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRow {
    pub id: AreaId,
    pub storage: MapStorage,
    /// The viewer owns it (not a clan, not someone who shared it).
    pub owned: bool,
    pub atlas_id: Option<AtlasId>,
    /// A copy still being made by a move between storages.
    pub relocating: bool,
}

impl MapRow {
    #[must_use]
    pub fn of(area: &Area, storage: MapStorage) -> Self {
        Self {
            id: area.id,
            storage,
            owned: area.effective_access().is_owner && area.clan_id.is_none(),
            atlas_id: area.atlas_id,
            relocating: area.name.ends_with(RELOCATION_IN_PROGRESS_SUFFIX),
        }
    }

    /// The viewer's own durable map, in no folder.
    fn is_loose(&self) -> bool {
        self.owned
            && self.atlas_id.is_none()
            && !self.relocating
            && self.storage != MapStorage::Session
    }
}

/// One folder as the migration reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRow {
    pub id: AtlasId,
    pub name: String,
    pub storage: MapStorage,
    /// The viewer's own folder (not a clan's, not one they administer).
    pub owned: bool,
    /// When it was made, in milliseconds; the oldest of several namesakes
    /// is the one used.
    pub created: i64,
}

impl FolderRow {
    #[must_use]
    pub fn of(item: &AtlasListItem, storage: MapStorage) -> Self {
        Self {
            id: item.id,
            name: item.name.clone(),
            storage,
            owned: item.is_own(),
            created: item.created_at.timestamp_millis(),
        }
    }
}

/// Where one group of loose maps goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `server`'s default folder in the plan's storage.
    Default { server: String },
    /// A cloud folder of this name shown on exactly `entries` (none: every
    /// server).
    Named {
        name: String,
        entries: BTreeSet<String>,
    },
}

/// One group of loose maps and where they go: into `folder`, or, when it is
/// `None`, a folder the target resolves or makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub storage: MapStorage,
    pub target: Target,
    pub folder: Option<AtlasId>,
    pub maps: Vec<AreaId>,
}

/// Where cloud maps shown on `entries` go: one existing server's default
/// folder, else a folder shown on exactly those servers.
fn cloud_target(entries: BTreeSet<String>, servers: &BTreeSet<String>) -> Target {
    if entries.len() == 1
        && let Some(server) = entries.first()
        && servers.contains(server)
    {
        return Target::Default {
            server: server.clone(),
        };
    }
    let name = if entries.is_empty() {
        LOOSE_MAPS_FOLDER.to_string()
    } else {
        let list: Vec<&str> = entries.iter().map(String::as_str).collect();
        format!("{LOOSE_MAPS_FOLDER} ({})", list.join(", "))
    };
    Target::Named { name, entries }
}

/// The oldest of the viewer's own cloud folders named `name` and shown on
/// exactly `entries`. A namesake shown elsewhere would change where its new
/// maps show, so it isn't used.
fn named_folder(
    folders: &[FolderRow],
    scopes: &MapScopes,
    name: &str,
    entries: &BTreeSet<String>,
) -> Option<AtlasId> {
    folders
        .iter()
        .filter(|folder| {
            folder.storage == MapStorage::Cloud
                && folder.owned
                && folder.name == name
                && scopes.atlas_entries(&folder.id) == *entries
        })
        .min_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.0.cmp(&b.id.0)))
        .map(|folder| folder.id)
}

/// The work in each of `storages`. This device's loose maps go in `here`'s
/// local default (none without a server); cloud loose maps are grouped by
/// the servers `scopes` show each on, `servers` being those that exist. A
/// storage with no loose map needs nothing, so no empty folder is made.
#[must_use]
pub fn plan(
    maps: &[MapRow],
    folders: &[FolderRow],
    storages: &[MapStorage],
    scopes: &MapScopes,
    servers: &BTreeSet<String>,
    here: Option<&str>,
) -> Vec<Plan> {
    let mut plans = Vec::new();
    for storage in storages.iter().copied() {
        let loose = maps
            .iter()
            .filter(|map| map.storage == storage && map.is_loose())
            .map(|map| map.id);
        match storage {
            MapStorage::Local => {
                let loose: Vec<AreaId> = loose.collect();
                if let Some(server) = here
                    && !loose.is_empty()
                {
                    plans.push(Plan {
                        storage,
                        target: Target::Default {
                            server: server.to_string(),
                        },
                        folder: None,
                        maps: loose,
                    });
                }
            }
            MapStorage::Cloud => {
                let mut groups: BTreeMap<BTreeSet<String>, Vec<AreaId>> = BTreeMap::new();
                for id in loose {
                    groups.entry(scopes.area_entries(&id)).or_default().push(id);
                }
                for (entries, maps) in groups {
                    let target = cloud_target(entries, servers);
                    let folder = match &target {
                        Target::Default { .. } => None,
                        Target::Named { name, entries } => {
                            named_folder(folders, scopes, name, entries)
                        }
                    };
                    plans.push(Plan {
                        storage,
                        target,
                        folder,
                        maps,
                    });
                }
            }
            MapStorage::Session => {}
        }
    }
    plans
}

/// The scope change that keeps `filed` cloud maps on the servers they were
/// shown on, now that they are in `folder`. A server's default folder made
/// now is shown on that server, and one that existed is shown there too
/// when it wasn't; a named folder made now is shown on exactly its servers.
fn scope_delta(scopes: &MapScopes, folder: AtlasId, outcome: &Outcome) -> Option<ScopeDelta> {
    match &outcome.target {
        Target::Default { server } if outcome.created => Some(ScopeDelta::SetAtlasEntries {
            atlas_id: folder,
            entries: BTreeSet::from([server.clone()]),
        }),
        Target::Default { server } => (!outcome.filed.is_empty()
            && scopes.atlas_scope(&folder, server) != ScopeState::Here)
            .then(|| ScopeDelta::SetAtlasEntry {
                atlas_id: folder,
                entry: server.clone(),
                show: true,
            }),
        Target::Named { entries, .. } => {
            (outcome.created && !entries.is_empty()).then(|| ScopeDelta::SetAtlasEntries {
                atlas_id: folder,
                entries: entries.clone(),
            })
        }
    }
}

/// The storages already taken this session, each under its owner: the local
/// maps of one server entry, the cloud maps of one account. Shared by every
/// editor window, so two windows never run it at once.
#[derive(Debug, Default)]
pub struct Claims(BTreeSet<String>);

impl Claims {
    /// Takes the storages not yet taken, of those `keys` name.
    pub fn take(&mut self, keys: Vec<(MapStorage, String)>) -> Vec<MapStorage> {
        keys.into_iter()
            .filter(|(_, key)| self.0.insert(key.clone()))
            .map(|(storage, _)| storage)
            .collect()
    }
}

static CLAIMS: Mutex<Claims> = Mutex::new(Claims(BTreeSet::new()));

/// One group's result.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub storage: MapStorage,
    pub target: Target,
    pub folder: Option<AtlasId>,
    pub created: bool,
    pub filed: Vec<AreaId>,
    /// Some map, or the folder, couldn't be done.
    pub failed: bool,
}

/// The run's results; `failed` when some storage's inventory couldn't be
/// read.
#[derive(Debug, Clone)]
pub struct Done {
    pub outcomes: Vec<Outcome>,
    pub failed: bool,
}

/// Starts the run for whichever storages this session hasn't run yet: the
/// local maps always, the cloud maps once signed in with a verified email.
pub(super) fn start(window: &MapEditorWindow) -> Task<Message> {
    let account = window.cloud.snapshot.get();
    let mut keys = vec![(
        MapStorage::Local,
        format!(
            "local:{}",
            window.server_name.as_deref().unwrap_or_default()
        ),
    )];
    if account.signed_in
        && account.email_verified
        && let Some(profile) = &account.profile
    {
        keys.push((MapStorage::Cloud, format!("cloud:{}", profile.id)));
    }
    let storages = CLAIMS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take(keys);
    if storages.is_empty() {
        return Task::none();
    }
    let mapper = window.mapper.clone();
    let scopes = window.map_scopes.clone();
    let here = window.server_name.clone();
    Task::perform(run(mapper, storages, scopes, here), Message::LooseMaps)
}

/// Reads the inventory fresh (the cache may be stale for cloud maps filed
/// on another device), then files each storage's loose maps. A storage
/// whose folders can't be read waits for a later session.
pub async fn run(
    mapper: Mapper,
    storages: Vec<MapStorage>,
    scopes: MapScopes,
    here: Option<String>,
) -> Done {
    let mut failed = false;
    let mut readable = Vec::new();
    let mut folders = Vec::new();
    for storage in storages {
        match mapper.list_atlases_in(storage).await {
            Ok(atlases) => {
                folders.extend(atlases.iter().map(|item| FolderRow::of(item, storage)));
                readable.push(storage);
            }
            Err(error) => {
                log::warn!("loose maps: the {storage} folder list failed: {error}");
                failed = true;
            }
        }
    }
    // Which servers exist decides where a cloud map shown on one goes.
    let servers: BTreeSet<String> = match smudgy_core::models::server::list_servers() {
        Ok(servers) => servers.into_iter().map(|server| server.name).collect(),
        Err(error) => {
            log::warn!("loose maps: the server list failed: {error:#}");
            failed |= readable.contains(&MapStorage::Cloud);
            readable.retain(|storage| *storage != MapStorage::Cloud);
            BTreeSet::new()
        }
    };
    let areas = match mapper.list_areas().await {
        Ok(areas) => areas,
        Err(error) => {
            log::warn!("loose maps: map list failed: {error}");
            return Done {
                outcomes: Vec::new(),
                failed: true,
            };
        }
    };
    let local = mapper.local_area_ids();
    let session = mapper.session_area_ids();
    let maps: Vec<MapRow> = areas
        .iter()
        .map(|area| {
            let storage = if session.contains(&area.id) {
                MapStorage::Session
            } else if local.contains(&area.id) {
                MapStorage::Local
            } else {
                MapStorage::Cloud
            };
            MapRow::of(area, storage)
        })
        .collect();

    let mut outcomes = Vec::new();
    for plan in plan(
        &maps,
        &folders,
        &readable,
        &scopes,
        &servers,
        here.as_deref(),
    ) {
        outcomes.push(apply(&mapper, plan).await);
    }
    Done { outcomes, failed }
}

/// Finds or makes the folder and files each map, carrying on past a map that
/// fails.
async fn apply(mapper: &Mapper, plan: Plan) -> Outcome {
    let found = match (&plan.target, plan.folder) {
        (_, Some(folder)) => Ok((folder, false)),
        (Target::Default { server }, None) => resolve_default_atlas(mapper, server, plan.storage)
            .await
            .map(|default| (default.id, default.created))
            .map_err(|error| format!("{error:#}")),
        (Target::Named { name, .. }, None) => mapper
            .create_atlas_at(name.clone(), plan.storage)
            .await
            .map(|atlas| (atlas.id, true))
            .map_err(|error| error.to_string()),
    };
    let (folder, created) = match found {
        Ok(found) => found,
        Err(error) => {
            log::warn!(
                "loose maps: no {} folder for {:?}: {error}",
                plan.storage,
                plan.target
            );
            return Outcome {
                storage: plan.storage,
                target: plan.target,
                folder: None,
                created: false,
                filed: Vec::new(),
                failed: true,
            };
        }
    };
    let mut filed = Vec::with_capacity(plan.maps.len());
    let mut failed = false;
    for area_id in plan.maps {
        match mapper.move_area_to_atlas(area_id, Some(folder)).await {
            Ok(()) => filed.push(area_id),
            Err(error) => {
                log::warn!("loose maps: couldn't file map {area_id}: {error}");
                failed = true;
            }
        }
    }
    Outcome {
        storage: plan.storage,
        target: plan.target,
        folder: Some(folder),
        created,
        filed,
        failed,
    }
}

/// The run finished: keep each cloud map on the servers it showed on, show
/// the folders, and say so when something failed.
pub(super) fn finished(window: &mut MapEditorWindow, done: Done) -> Update<Message, Event> {
    let mut deltas = Vec::new();
    let mut changed = false;
    for outcome in &done.outcomes {
        let Some(folder) = outcome.folder else {
            continue;
        };
        changed |= outcome.created || !outcome.filed.is_empty();
        window.collapsed_folders.remove(&FolderKey::Atlas(folder));
        if outcome.storage == MapStorage::Cloud
            && let Some(delta) = scope_delta(&window.map_scopes, folder, outcome)
        {
            window.map_scopes.apply(&delta);
            deltas.push(delta);
        }
    }
    if done.failed || done.outcomes.iter().any(|outcome| outcome.failed) {
        window.editor_notice = Some((
            std::time::Instant::now(),
            crate::i18n::t!("mapper-loose-maps-failed"),
        ));
    }
    let task = if changed {
        window.refresh_default_atlases();
        window.fetch_atlases()
    } else {
        Task::none()
    };
    let event = (!deltas.is_empty()).then_some(Event::ScopeAssociationsChanged(deltas));
    Update::new(task, event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn atlas(n: u128) -> AtlasId {
        AtlasId(Uuid::from_u128(n))
    }

    fn map(n: u128, storage: MapStorage, atlas_id: Option<u128>) -> MapRow {
        MapRow {
            id: area(n),
            storage,
            owned: true,
            atlas_id: atlas_id.map(atlas),
            relocating: false,
        }
    }

    fn folder(n: u128, name: &str, storage: MapStorage, created: i64) -> FolderRow {
        FolderRow {
            id: atlas(n),
            name: name.to_string(),
            storage,
            owned: true,
            created,
        }
    }

    fn entries(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn default(server: &str) -> Target {
        Target::Default {
            server: server.to_string(),
        }
    }

    fn named(name: &str, servers: &[&str]) -> Target {
        Target::Named {
            name: name.to_string(),
            entries: entries(servers),
        }
    }

    const BOTH: [MapStorage; 2] = [MapStorage::Local, MapStorage::Cloud];

    #[test]
    fn only_the_viewers_own_durable_loose_maps_move() {
        let shared = MapRow {
            owned: false,
            ..map(3, MapStorage::Cloud, None)
        };
        let relocating = MapRow {
            relocating: true,
            ..map(6, MapStorage::Local, None)
        };
        let maps = [
            map(1, MapStorage::Local, None),
            map(2, MapStorage::Local, Some(50)),
            shared,
            map(4, MapStorage::Session, None),
            map(5, MapStorage::Cloud, None),
            relocating,
        ];
        let plans = plan(
            &maps,
            &[],
            &BOTH,
            &MapScopes::default(),
            &entries(&["Arctic"]),
            Some("Arctic"),
        );
        assert_eq!(
            plans,
            [
                Plan {
                    storage: MapStorage::Local,
                    target: default("Arctic"),
                    folder: None,
                    maps: vec![area(1)],
                },
                Plan {
                    storage: MapStorage::Cloud,
                    target: named("Loose maps", &[]),
                    folder: None,
                    maps: vec![area(5)],
                },
            ]
        );
    }

    #[test]
    fn only_the_storages_asked_for_run() {
        let maps = [
            map(1, MapStorage::Local, None),
            map(2, MapStorage::Cloud, None),
        ];
        let servers = entries(&["Arctic"]);
        let scopes = MapScopes::default();
        let plans = plan(
            &maps,
            &[],
            &[MapStorage::Cloud],
            &scopes,
            &servers,
            Some("Arctic"),
        );
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].storage, MapStorage::Cloud);
        assert!(
            plan(
                &maps,
                &[],
                &[MapStorage::Session],
                &scopes,
                &servers,
                Some("Arctic")
            )
            .is_empty()
        );
        // No server to file this device's maps for: they wait.
        assert!(plan(&maps, &[], &[MapStorage::Local], &scopes, &servers, None).is_empty());
    }

    #[test]
    fn cloud_maps_go_by_the_servers_they_were_shown_on() {
        let mut scopes = MapScopes::default();
        scopes.set_area_entries(area(2), entries(&["Arctic"]));
        scopes.set_area_entries(area(3), entries(&["Nukefire", "Arctic"]));
        scopes.set_area_entries(area(4), entries(&["Gone"]));
        scopes.set_area_entries(area(5), entries(&["Arctic"]));
        let maps = [
            map(1, MapStorage::Cloud, None),
            map(2, MapStorage::Cloud, None),
            map(3, MapStorage::Cloud, None),
            map(4, MapStorage::Cloud, None),
            map(5, MapStorage::Cloud, None),
        ];
        let plans = plan(
            &maps,
            &[],
            &[MapStorage::Cloud],
            &scopes,
            &entries(&["Arctic", "Nukefire"]),
            Some("Nukefire"),
        );
        let got: Vec<(Target, Vec<AreaId>)> = plans
            .into_iter()
            .map(|plan| (plan.target, plan.maps))
            .collect();
        assert_eq!(
            got,
            [
                // Shown everywhere: a folder shown everywhere.
                (named("Loose maps", &[]), vec![area(1)]),
                // One server, even another than the editor's: its default.
                (default("Arctic"), vec![area(2), area(5)]),
                // Several: a folder shown on exactly those.
                (
                    named("Loose maps (Arctic, Nukefire)", &["Arctic", "Nukefire"]),
                    vec![area(3)]
                ),
                // A server that no longer exists: a folder shown only there.
                (named("Loose maps (Gone)", &["Gone"]), vec![area(4)]),
            ]
        );
    }

    #[test]
    fn a_named_folder_is_used_again_only_shown_on_exactly_its_servers() {
        let mut scopes = MapScopes::default();
        scopes.set_area_entries(area(2), entries(&["Arctic", "Nukefire"]));
        // A "Loose maps" shown on one server isn't the one shown everywhere.
        scopes.set_atlas_entries(atlas(10), entries(&["Arctic"]));
        scopes.set_atlas_entries(atlas(13), entries(&["Arctic", "Nukefire"]));
        scopes.set_atlas_entries(atlas(14), entries(&["Arctic"]));
        let folders = [
            folder(10, "Loose maps", MapStorage::Cloud, 1),
            folder(11, "Loose maps", MapStorage::Cloud, 9),
            // The older of two namesakes wins.
            folder(12, "Loose maps", MapStorage::Cloud, 5),
            folder(13, "Loose maps (Arctic, Nukefire)", MapStorage::Cloud, 3),
            folder(14, "Loose maps (Arctic, Nukefire)", MapStorage::Cloud, 0),
            // This device's, or someone else's, never.
            folder(15, "Loose maps", MapStorage::Local, 0),
            FolderRow {
                owned: false,
                ..folder(16, "Loose maps", MapStorage::Cloud, 0)
            },
        ];
        let maps = [
            map(1, MapStorage::Cloud, None),
            map(2, MapStorage::Cloud, None),
        ];
        let plans = plan(
            &maps,
            &folders,
            &[MapStorage::Cloud],
            &scopes,
            &entries(&["Arctic", "Nukefire"]),
            Some("Arctic"),
        );
        assert_eq!(plans[0].folder, Some(atlas(12)));
        assert_eq!(plans[1].folder, Some(atlas(13)));
    }

    #[test]
    fn nothing_loose_makes_no_folder() {
        let maps = [map(1, MapStorage::Local, Some(50))];
        assert!(
            plan(
                &maps,
                &[],
                &BOTH,
                &MapScopes::default(),
                &BTreeSet::new(),
                Some("Arctic")
            )
            .is_empty()
        );
    }

    #[test]
    fn claims_run_each_owner_once() {
        let mut claims = Claims::default();
        let keys = || {
            vec![
                (MapStorage::Local, "local:Arctic".to_string()),
                (MapStorage::Cloud, "cloud:1".to_string()),
            ]
        };
        assert_eq!(claims.take(keys()), BOTH);
        assert!(claims.take(keys()).is_empty());
        // Another server's local maps, or another account, are their own.
        assert_eq!(
            claims.take(vec![(MapStorage::Local, "local:Nukefire".to_string())]),
            [MapStorage::Local]
        );
    }

    /// The servers a cloud map is shown on, of `servers`: its folder's when
    /// it has one, else its own.
    fn shown_on(
        scopes: &MapScopes,
        map: AreaId,
        folder: Option<AtlasId>,
        servers: &[&str],
    ) -> Vec<String> {
        servers
            .iter()
            .filter(|server| {
                let scope = match folder {
                    Some(folder) => scopes.atlas_scope(&folder, server),
                    None => scopes.area_scope(&map, server),
                };
                scope != ScopeState::Elsewhere
            })
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn filing_never_changes_where_a_map_shows() {
        const SERVERS: [&str; 4] = ["Arctic", "Nukefire", "Gone", "Other"];
        let mut scopes = MapScopes::default();
        scopes.set_area_entries(area(2), entries(&["Arctic"]));
        scopes.set_area_entries(area(3), entries(&["Arctic", "Nukefire"]));
        scopes.set_area_entries(area(4), entries(&["Gone"]));
        scopes.set_area_entries(area(5), entries(&["Nukefire"]));
        // Nukefire's default already exists and is shown on Nukefire.
        scopes.set_atlas_entries(atlas(20), entries(&["Nukefire"]));
        let maps = [1, 2, 3, 4, 5].map(|n| map(n, MapStorage::Cloud, None));
        let plans = plan(
            &maps,
            &[],
            &[MapStorage::Cloud],
            &scopes,
            &entries(&["Arctic", "Nukefire", "Other"]),
            Some("Other"),
        );
        let before: Vec<Vec<String>> = maps
            .iter()
            .map(|map| shown_on(&scopes, map.id, None, &SERVERS))
            .collect();

        let mut after = scopes.clone();
        let mut folder_of = std::collections::HashMap::new();
        for (plan, n) in plans.into_iter().zip(100..) {
            // Arctic's default and the named folders are made now;
            // Nukefire's existed.
            let (folder, created) = match &plan.target {
                Target::Default { server } if server == "Nukefire" => (atlas(20), false),
                _ => (atlas(n), true),
            };
            let outcome = Outcome {
                storage: plan.storage,
                target: plan.target,
                folder: Some(folder),
                created,
                filed: plan.maps.clone(),
                failed: false,
            };
            if let Some(delta) = scope_delta(&after, folder, &outcome) {
                after.apply(&delta);
            }
            for id in plan.maps {
                folder_of.insert(id, folder);
            }
        }
        for (map, was) in maps.iter().zip(before) {
            assert_eq!(
                shown_on(&after, map.id, folder_of.get(&map.id).copied(), &SERVERS),
                was,
                "{:?}",
                map.id
            );
        }
    }

    #[test]
    fn an_existing_default_is_shown_on_its_server() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(20), entries(&["Arctic"]));
        let outcome = |created, filed: Vec<AreaId>| Outcome {
            storage: MapStorage::Cloud,
            target: default("Nukefire"),
            folder: Some(atlas(20)),
            created,
            filed,
            failed: false,
        };
        // Filing into it shows it on its own server too.
        assert_eq!(
            scope_delta(&scopes, atlas(20), &outcome(false, vec![area(1)])),
            Some(ScopeDelta::SetAtlasEntry {
                atlas_id: atlas(20),
                entry: "Nukefire".to_string(),
                show: true,
            })
        );
        // Nothing filed into it: left alone.
        assert_eq!(
            scope_delta(&scopes, atlas(20), &outcome(false, Vec::new())),
            None
        );
        // Made now, it is shown on its server even with nothing filed.
        assert_eq!(
            scope_delta(&scopes, atlas(21), &outcome(true, Vec::new())),
            Some(ScopeDelta::SetAtlasEntries {
                atlas_id: atlas(21),
                entries: entries(&["Nukefire"]),
            })
        );
    }

    #[tokio::test]
    async fn filing_local_loose_maps_is_idempotent() {
        // The home is the first test's to set; this test's server is its own.
        smudgy_core::set_smudgy_home(
            std::env::temp_dir().join(format!("smudgy-loose-maps-home-{}", std::process::id())),
        );
        let server = format!("loose-maps-{}", Uuid::new_v4().simple());
        smudgy_core::models::server::create_server(
            &server,
            smudgy_core::models::server::ServerConfig::new("mud.example.org".to_string(), 4000),
        )
        .expect("server");
        let root = smudgy_core::get_smudgy_home()
            .expect("home")
            .join(&server)
            .join("maps");
        let mapper = Mapper::new(
            std::sync::Arc::new(smudgy_cloud::LocalBackend::new(root.join("local"))),
            root.join("cache"),
        );
        let roads = mapper
            .create_atlas_at("Roads".to_string(), MapStorage::Local)
            .await
            .expect("folder");
        let filed = mapper
            .create_area_at(
                "Filed".to_string(),
                smudgy_cloud::MapDestination::in_atlas(MapStorage::Local, roads.id),
            )
            .await
            .expect("filed map");
        let mut loose = Vec::new();
        for name in ["One", "Two"] {
            loose.push(
                mapper
                    .create_area_at(
                        name.to_string(),
                        smudgy_cloud::MapDestination::loose(MapStorage::Local),
                    )
                    .await
                    .expect("loose map"),
            );
        }
        let run_local = || {
            run(
                mapper.clone(),
                vec![MapStorage::Local],
                MapScopes::default(),
                Some(server.clone()),
            )
        };

        let first = run_local().await;
        assert!(!first.failed);
        assert_eq!(first.outcomes.len(), 1);
        let outcome = &first.outcomes[0];
        assert!(outcome.created && !outcome.failed);
        let mut moved = outcome.filed.clone();
        moved.sort_by_key(|id| id.0);
        loose.sort_by_key(|id| id.0);
        assert_eq!(moved, loose);
        let folder = outcome.folder.expect("folder made");
        // It is the server's local default now.
        assert_eq!(
            smudgy_core::models::default_atlases::default_atlas(&server, MapStorage::Local)
                .expect("settings"),
            Some(folder)
        );

        let areas = mapper.list_areas().await.expect("areas");
        for area in &areas {
            let expected = if area.id == filed {
                Some(roads.id)
            } else {
                Some(folder)
            };
            assert_eq!(area.atlas_id, expected, "{}", area.name);
        }

        // Again: nothing loose, nothing made.
        let second = run_local().await;
        assert!(!second.failed);
        assert!(second.outcomes.is_empty());

        // A map made loose later joins the same folder.
        let later = mapper
            .create_area_at(
                "Later".to_string(),
                smudgy_cloud::MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("later map");
        let third = run_local().await;
        assert_eq!(third.outcomes.len(), 1);
        assert_eq!(third.outcomes[0].folder, Some(folder));
        assert!(!third.outcomes[0].created);
        assert_eq!(third.outcomes[0].filed, [later]);
        let folders = mapper.list_atlases().await.expect("folders");
        assert_eq!(
            folders
                .iter()
                .filter(|item| item.name == LOOSE_MAPS_FOLDER)
                .count(),
            1
        );

        let _ = smudgy_core::models::server::delete_server(&server);
    }
}
