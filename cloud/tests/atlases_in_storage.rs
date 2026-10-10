//! Listing the folders of one storage, and finding the caller's own folder
//! of a name among them.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use smudgy_cloud::{
    Area, AreaId, AreaUpdates, AreaWithDetails, AtlasId, AtlasListItem, CloudError, CloudResult,
    CompositeBackend, CreateAreaRequest, LocalBackend, MapStorage, Mapper, MapperBackend,
    mutation::{MutationEnvelope, MutationResult},
    oldest_own_atlas_named,
};
use uuid::Uuid;

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    Scratch(std::env::temp_dir().join(format!("smudgy-atlases-in-{tag}-{}", Uuid::new_v4())))
}

/// A cloud whose folder list can't be read: signed in, with a list that
/// fails, or signed out.
struct UnreadableCloud {
    signed_in: bool,
}

#[async_trait]
impl MapperBackend for UnreadableCloud {
    fn has_credential(&self) -> bool {
        self.signed_in
    }
    async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
        Err(CloudError::InternalError("unreadable".to_string()))
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
        if !self.signed_in {
            // Answers as though nothing were there: only the composite's
            // own check says why.
            return Ok(Vec::new());
        }
        Err(CloudError::InternalError(
            "the folder list failed".to_string(),
        ))
    }
}

#[tokio::test]
async fn a_storage_lists_only_its_own_folders() {
    let scratch = scratch("local");
    let mapper = Mapper::new(
        Arc::new(LocalBackend::new(scratch.0.join("local"))),
        scratch.0.join("cache"),
    );
    let roads = mapper
        .create_atlas_at("Roads".to_string(), MapStorage::Local)
        .await
        .expect("folder");

    let local = mapper
        .list_atlases_in(MapStorage::Local)
        .await
        .expect("local folders");
    assert_eq!(
        local.iter().map(|atlas| atlas.id).collect::<Vec<_>>(),
        [roads.id]
    );
    assert_eq!(mapper.atlas_storage(&roads.id), Some(MapStorage::Local));
    assert!(
        mapper
            .list_atlases_in(MapStorage::Cloud)
            .await
            .expect("no cloud here")
            .is_empty()
    );

    // A folder deleted since is gone from the next read and from the
    // storage the mapper knows it by.
    mapper.delete_atlas(roads.id).await.expect("delete");
    assert!(
        mapper
            .list_atlases_in(MapStorage::Local)
            .await
            .expect("local folders")
            .is_empty()
    );
    assert_eq!(mapper.atlas_storage(&roads.id), None);
}

fn composite_over(scratch: &Scratch, cloud: UnreadableCloud) -> Mapper {
    Mapper::new(
        Arc::new(CompositeBackend::new(
            Arc::new(LocalBackend::new(scratch.0.join("local"))),
            Arc::new(cloud),
        )),
        scratch.0.join("cache"),
    )
}

#[tokio::test]
async fn an_unreadable_cloud_fails_its_own_list() {
    let scratch = scratch("unreadable");
    let mapper = composite_over(&scratch, UnreadableCloud { signed_in: true });
    let roads = mapper
        .create_atlas_at("Roads".to_string(), MapStorage::Local)
        .await
        .expect("folder");

    // Every folder list answers with what it could read...
    let all = mapper.list_atlases().await.expect("the readable folders");
    assert_eq!(
        all.iter().map(|atlas| atlas.id).collect::<Vec<_>>(),
        [roads.id]
    );
    // ...but the cloud's own list says it couldn't.
    assert!(mapper.list_atlases_in(MapStorage::Cloud).await.is_err());
    assert_eq!(
        mapper
            .list_atlases_in(MapStorage::Local)
            .await
            .expect("local folders")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_signed_out_cloud_fails_its_own_list() {
    let scratch = scratch("signed-out");
    let mapper = composite_over(&scratch, UnreadableCloud { signed_in: false });
    assert!(mapper.list_atlases().await.is_ok());
    assert!(matches!(
        mapper.list_atlases_in(MapStorage::Cloud).await,
        Err(CloudError::Unauthorized(_))
    ));
    assert!(mapper.list_atlases_in(MapStorage::Local).await.is_ok());
}

fn item(n: u128, name: &str, age_days: i64) -> AtlasListItem {
    AtlasListItem {
        id: AtlasId(Uuid::from_u128(n)),
        name: name.to_string(),
        created_at: Utc::now() - Duration::days(age_days),
        area_count: 0,
        rev: 1,
        is_owner: true,
        can_admin: true,
        owner_nickname: None,
        clan_id: None,
        clan_name: None,
        actions: BTreeSet::new(),
    }
}

#[test]
fn the_oldest_own_folder_of_the_name_is_found() {
    let atlases = [
        item(1, "Loose maps", 1),
        item(2, "Loose maps", 5),
        item(3, "loose maps", 9),
        AtlasListItem {
            clan_id: Some(Uuid::from_u128(77)),
            ..item(4, "Loose maps", 20)
        },
        AtlasListItem {
            is_owner: false,
            ..item(5, "Loose maps", 30)
        },
    ];
    assert_eq!(
        oldest_own_atlas_named(&atlases, "Loose maps"),
        Some(AtlasId(Uuid::from_u128(2)))
    );
    assert_eq!(oldest_own_atlas_named(&atlases, "Roads"), None);
}
