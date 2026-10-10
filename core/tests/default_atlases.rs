//! A server's default atlases: adopted or made on first use, stored in the
//! server's settings, kept across a rename, replaced once gone, never made
//! when the cloud can't be read, and made once however many ask at once.

use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use smudgy_cloud::{
    Area, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem, CloudError,
    CloudResult, CompositeBackend, CreateAreaRequest, LocalBackend, MapStorage, Mapper,
    MapperBackend, Uuid,
    mutation::{MutationEnvelope, MutationResult},
};
use smudgy_core::models::default_atlases::{
    DefaultAtlas, default_atlas, default_atlas_name, resolve_default_atlas, set_default_atlas,
};
use smudgy_core::models::server::{ServerConfig, create_server};

/// The shared test home. The override is process-wide and set once, so each
/// test uses a server of its own.
fn smudgy_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("create temp home");
        let path = home.path().to_path_buf();
        std::mem::forget(home);
        smudgy_core::set_smudgy_home(&path);
        smudgy_core::get_smudgy_home().expect("smudgy home")
    })
    .clone()
}

fn server(name: &str) -> String {
    smudgy_home();
    create_server(name, ServerConfig::new("mud.example.org".to_string(), 4000))
        .expect("create server");
    name.to_string()
}

/// A mapper over `server`'s own local maps.
fn local_mapper(server: &str) -> Mapper {
    let root = smudgy_home().join(server).join("maps");
    Mapper::new(
        Arc::new(LocalBackend::new(root.join("local"))),
        root.join("cache"),
    )
}

/// A cloud stand-in holding folders in memory. Its list can be made to
/// fail, and every list and create can be made to take a while, so two
/// resolutions at once would interleave.
#[derive(Default)]
struct FolderCloud {
    atlases: Mutex<Vec<AtlasListItem>>,
    unreadable: bool,
    slow: bool,
    made: AtomicUsize,
}

impl FolderCloud {
    fn holding(atlases: Vec<AtlasListItem>) -> Self {
        Self {
            atlases: Mutex::new(atlases),
            ..Self::default()
        }
    }

    async fn pause(&self) {
        if self.slow {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn listed(name: &str, age_days: i64) -> AtlasListItem {
    AtlasListItem {
        id: AtlasId(Uuid::new_v4()),
        name: name.to_string(),
        created_at: Utc::now() - chrono::Duration::days(age_days),
        area_count: 0,
        rev: 1,
        is_owner: true,
        can_admin: true,
        owner_nickname: None,
        clan_id: None,
        clan_name: None,
        actions: std::collections::BTreeSet::new(),
    }
}

#[async_trait]
impl MapperBackend for FolderCloud {
    async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
        Err(CloudError::InvalidInput("no maps here".to_string()))
    }
    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(Vec::new())
    }
    async fn get_area(&self, _area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        Err(CloudError::NotFoundOrNoAccess)
    }
    async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
        Ok(())
    }
    async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
        Ok(())
    }
    async fn execute_mutation(
        &self,
        _area_id: &AreaId,
        _envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        Err(CloudError::NotFoundOrNoAccess)
    }
    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.pause().await;
        if self.unreadable {
            return Err(CloudError::InternalError("the folder list failed".into()));
        }
        Ok(self.atlases.lock().unwrap().clone())
    }
    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        self.create_atlas_at(name, MapStorage::Cloud).await
    }
    async fn create_atlas_at(&self, name: &str, storage: MapStorage) -> CloudResult<Atlas> {
        assert_eq!(storage, MapStorage::Cloud);
        self.pause().await;
        self.made.fetch_add(1, Ordering::SeqCst);
        let item = listed(name, 0);
        self.atlases.lock().unwrap().push(item.clone());
        Ok(Atlas {
            id: item.id,
            user_id: None,
            clan_id: None,
            name: item.name,
            created_at: item.created_at,
            rev: 1,
        })
    }
}

/// A mapper over `cloud`, with an empty local tier of `server`'s.
fn cloud_mapper(server: &str, cloud: Arc<FolderCloud>) -> Mapper {
    let root = smudgy_home().join(server).join("maps");
    Mapper::new(
        Arc::new(CompositeBackend::new(
            Arc::new(LocalBackend::new(root.join("local"))),
            cloud,
        )),
        root.join(format!("cache-{}", Uuid::new_v4())),
    )
}

async fn names_in(mapper: &Mapper, storage: MapStorage) -> Vec<String> {
    let mut names: Vec<String> = mapper
        .list_atlases_in(storage)
        .await
        .expect("atlases")
        .into_iter()
        .map(|atlas| atlas.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn the_first_resolution_makes_one_and_stores_it() {
    let server = server("DefaultsMake");
    let mapper = local_mapper(&server);
    assert_eq!(default_atlas(&server, MapStorage::Local).unwrap(), None);

    let made = resolve_default_atlas(&mapper, &server, MapStorage::Local)
        .await
        .expect("made");
    assert!(made.created);
    assert_eq!(
        default_atlas(&server, MapStorage::Local).unwrap(),
        Some(made.id)
    );
    assert_eq!(names_in(&mapper, MapStorage::Local).await, ["Loose maps"]);

    // Again, and from a mapper that has never seen it: the same one.
    let again = resolve_default_atlas(&mapper, &server, MapStorage::Local)
        .await
        .expect("again");
    assert_eq!(
        again,
        DefaultAtlas {
            id: made.id,
            created: false
        }
    );
    let fresh = local_mapper(&server);
    let found = resolve_default_atlas(&fresh, &server, MapStorage::Local)
        .await
        .expect("found");
    assert_eq!(
        found,
        DefaultAtlas {
            id: made.id,
            created: false
        }
    );
    assert_eq!(names_in(&fresh, MapStorage::Local).await, ["Loose maps"]);
}

#[tokio::test]
async fn an_own_namesake_is_adopted_oldest_first() {
    let server = server("DefaultsAdopt");
    let name = default_atlas_name(&server, MapStorage::Cloud);
    let oldest = listed(&name, 10);
    let mut clans = listed(&name, 40);
    clans.clan_id = Some(Uuid::new_v4());
    let mut administered = listed(&name, 50);
    administered.is_owner = false;
    let cloud = Arc::new(FolderCloud::holding(vec![
        listed(&name, 2),
        oldest.clone(),
        clans,
        administered,
        listed("Loose maps", 90),
    ]));
    let mapper = cloud_mapper(&server, cloud.clone());

    let adopted = resolve_default_atlas(&mapper, &server, MapStorage::Cloud)
        .await
        .expect("adopted");
    assert_eq!(
        adopted,
        DefaultAtlas {
            id: oldest.id,
            created: false
        }
    );
    assert_eq!(cloud.made.load(Ordering::SeqCst), 0);
    assert_eq!(
        default_atlas(&server, MapStorage::Cloud).unwrap(),
        Some(oldest.id)
    );
    // The local default is its own setting.
    assert_eq!(default_atlas(&server, MapStorage::Local).unwrap(), None);
}

#[tokio::test]
async fn a_setting_naming_a_gone_atlas_is_replaced() {
    let server = server("DefaultsStale");
    let mapper = local_mapper(&server);
    let gone = AtlasId(Uuid::new_v4());
    set_default_atlas(&server, MapStorage::Local, gone)
        .await
        .expect("set");

    let made = resolve_default_atlas(&mapper, &server, MapStorage::Local)
        .await
        .expect("made");
    assert!(made.created && made.id != gone);
    assert_eq!(
        default_atlas(&server, MapStorage::Local).unwrap(),
        Some(made.id)
    );

    // Deleted, it is made again rather than named from memory.
    mapper.delete_atlas(made.id).await.expect("delete");
    let remade = resolve_default_atlas(&mapper, &server, MapStorage::Local)
        .await
        .expect("remade");
    assert!(remade.created && remade.id != made.id);
}

#[tokio::test]
async fn a_renamed_default_stays_the_default() {
    let server = server("DefaultsRenamed");
    let mapper = local_mapper(&server);
    let made = resolve_default_atlas(&mapper, &server, MapStorage::Local)
        .await
        .expect("made");
    mapper
        .rename_atlas(made.id, "Arctic zones".to_string())
        .await
        .expect("rename");

    let fresh = local_mapper(&server);
    let found = resolve_default_atlas(&fresh, &server, MapStorage::Local)
        .await
        .expect("found");
    assert_eq!(
        found,
        DefaultAtlas {
            id: made.id,
            created: false
        }
    );
    assert_eq!(names_in(&fresh, MapStorage::Local).await, ["Arctic zones"]);
}

#[tokio::test]
async fn a_chosen_default_is_used() {
    let server = server("DefaultsChosen");
    let mapper = local_mapper(&server);
    let roads = mapper
        .create_atlas_at("Roads".to_string(), MapStorage::Local)
        .await
        .expect("folder");
    set_default_atlas(&server, MapStorage::Local, roads.id)
        .await
        .expect("set");
    assert_eq!(
        default_atlas(&server, MapStorage::Local).unwrap(),
        Some(roads.id)
    );
    let found = resolve_default_atlas(&local_mapper(&server), &server, MapStorage::Local)
        .await
        .expect("found");
    assert_eq!(
        found,
        DefaultAtlas {
            id: roads.id,
            created: false
        }
    );
    assert!(
        set_default_atlas(&server, MapStorage::Session, roads.id)
            .await
            .is_err()
    );
    assert!(
        resolve_default_atlas(&mapper, &server, MapStorage::Session)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_unreadable_cloud_makes_nothing() {
    let server = server("DefaultsUnreadable");
    let cloud = Arc::new(FolderCloud {
        unreadable: true,
        ..FolderCloud::default()
    });
    let mapper = cloud_mapper(&server, cloud.clone());

    // Every folder list still answers, with the local ones only.
    assert!(mapper.list_atlases().await.is_ok());
    assert!(
        resolve_default_atlas(&mapper, &server, MapStorage::Cloud)
            .await
            .is_err()
    );
    assert_eq!(cloud.made.load(Ordering::SeqCst), 0);
    assert_eq!(default_atlas(&server, MapStorage::Cloud).unwrap(), None);
}

#[tokio::test]
async fn two_resolutions_at_once_make_one() {
    let server = server("DefaultsAtOnce");
    let cloud = Arc::new(FolderCloud {
        slow: true,
        ..FolderCloud::default()
    });
    // Two sessions' mappers over the one cloud.
    let first = cloud_mapper(&server, cloud.clone());
    let second = cloud_mapper(&server, cloud.clone());

    let (a, b) = tokio::join!(
        resolve_default_atlas(&first, &server, MapStorage::Cloud),
        resolve_default_atlas(&second, &server, MapStorage::Cloud),
    );
    let (a, b) = (a.expect("first"), b.expect("second"));
    assert_eq!(a.id, b.id);
    assert_eq!(usize::from(a.created) + usize::from(b.created), 1);
    assert_eq!(cloud.made.load(Ordering::SeqCst), 1);
    assert_eq!(
        default_atlas(&server, MapStorage::Cloud).unwrap(),
        Some(a.id)
    );
}
