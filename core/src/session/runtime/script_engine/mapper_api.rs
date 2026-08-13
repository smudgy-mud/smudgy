use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{
    GarbageCollected, OpState, op2, thiserror,
    v8::{self},
};
use serde::{Deserialize, Serialize};

use super::ops::SmudgyGrants;
use crate::session::runtime::action::{ActionQueue, RuntimeAction};
use smudgy_cloud::{
    AreaId, AreaWithDetails, AtlasId, Connection, ConnectionArgs, ConnectionDash,
    ConnectionEndpoint, ConnectionId, ConnectionKind, ConnectionRouting, ConnectionUpdates,
    CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS, ExitArgs, ExitDirection,
    ExitId, ExitUpdates, HorizontalAlignment, Label, LabelArgs, LabelId, LabelUpdates,
    MapDestination, MapPoint, MapStorage, Mapper, PortMode, RelocationMode, RoomNumber, RoomSide,
    RoomUpdates, SegmentShape, Shape, ShapeArgs, ShapeId, ShapeType, ShapeUpdates, Uuid,
    VerticalAlignment,
    mapper::{
        AreaMutationBatch, MutationSubmission, RoomKey, area_cache::AreaCache,
        room_cache::RoomCache,
    },
    mutation::{AreaMutation, MAX_MUTATION_OPERATIONS},
};

deno_core::extension!(
  smudgy_mapper,
  ops = [
      op_smudgy_mapper_list_area_ids,
      op_smudgy_mapper_refresh_areas,
      op_smudgy_mapper_create_area,
      op_smudgy_mapper_get_area_storage,
      op_smudgy_mapper_get_atlas_storage,
      op_smudgy_mapper_list_atlases,
      op_smudgy_mapper_create_atlas,
      op_smudgy_mapper_relocate_areas,
      op_smudgy_mapper_relocate_atlas,
      op_smudgy_mapper_delete_area,
      op_smudgy_mapper_get_area_is_ephemeral,
      op_smudgy_mapper_get_room_external_id,
      op_smudgy_mapper_set_room_external_id,
      op_smudgy_mapper_find_room_by_external_id,
      op_smudgy_mapper_rescue_room_by_external_id,
      op_smudgy_mapper_get_area_by_id,
      op_smudgy_mapper_get_area_name,
      op_smudgy_mapper_get_area_id,
      op_smudgy_mapper_get_area_uuid,
      op_smudgy_mapper_rename_area,
      op_smudgy_mapper_list_area_room_numbers,
      op_smudgy_mapper_list_rooms_by_title_and_description,
      op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits,
      op_smudgy_mapper_get_area_room_by_number,
      op_smudgy_mapper_get_area_property,
      op_smudgy_mapper_get_area_next_room_number,
      op_smudgy_mapper_reserve_room_number,
      op_smudgy_mapper_release_room_reservations,
      op_smudgy_mapper_get_room_area_id,
      op_smudgy_mapper_get_room_number,
      op_smudgy_mapper_get_room_title,
      op_smudgy_mapper_get_room_description,
      op_smudgy_mapper_get_room_level,
      op_smudgy_mapper_get_room_x,
      op_smudgy_mapper_get_room_y,
      op_smudgy_mapper_get_room_color,
      op_smudgy_mapper_get_room_property,
      op_smudgy_mapper_get_room_tags,
      op_smudgy_mapper_has_tag,
      op_smudgy_mapper_get_room_exits,
      op_smudgy_mapper_set_room_title,
      op_smudgy_mapper_set_room_description,
      op_smudgy_mapper_set_room_color,
      op_smudgy_mapper_set_room_level,
      op_smudgy_mapper_set_room_x,
      op_smudgy_mapper_set_room_y,
      op_smudgy_mapper_set_room_property,
      op_smudgy_mapper_set_area_property,
      op_smudgy_mapper_add_room_tag,
      op_smudgy_mapper_remove_room_tag,
      op_smudgy_mapper_find_nearest_room_with_tags,
      op_smudgy_mapper_find_nearest_room_in_area,
      op_smudgy_mapper_create_room,
      op_smudgy_mapper_update_room,
      op_smudgy_mapper_update_rooms,
      op_smudgy_mapper_generate_id,
      op_smudgy_mapper_mutate_area,
      op_smudgy_mapper_create_room_exit,
      op_smudgy_mapper_set_room_exit,
      op_smudgy_mapper_merge_rooms,
      op_smudgy_mapper_delete_room,
      op_smudgy_mapper_delete_room_exit,
      op_smudgy_mapper_get_area_labels,
      op_smudgy_mapper_get_area_shapes,
      op_smudgy_mapper_get_area_connections,
      op_smudgy_mapper_create_link,
      op_smudgy_mapper_set_connection,
      op_smudgy_mapper_unlink_exit,
      op_smudgy_mapper_pair_connections,
      op_smudgy_mapper_delete_link,
      op_smudgy_mapper_create_label,
      op_smudgy_mapper_create_shape,
      op_smudgy_mapper_set_label,
      op_smudgy_mapper_set_shape,
      op_smudgy_mapper_delete_label,
      op_smudgy_mapper_delete_shape,
      op_smudgy_mapper_import_areas,
      op_smudgy_mapper_import_areas_if_absent,
      op_smudgy_mapper_export_area,
      op_smudgy_mapper_get_path_between_rooms,
      ],
  esm_entry_point = "ext:smudgy_mapper/mapper.ts",
  esm = [ dir "src/session/runtime/script_engine/mapper", "mapper.ts" ],
  options = {
    mapper: Option<Mapper>,
  },
  state = |state, options| {
    if let Some(mapper) = options.mapper {
        state.put::<Mapper>(mapper);
    }
  },
);

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum MapperError {
    #[class(generic)]
    #[error("Mapper not enabled in this session")]
    MapperNotEnabled,
    #[class(generic)]
    #[error("Area not found")]
    AreaNotFound,
    #[class(generic)]
    #[error("Atlas not found")]
    AtlasNotFound,
    #[class(generic)]
    #[error("Failed to create map: {0}")]
    FailedToCreate(String),
    /// A non-creation mapper operation failed. `operation` is the
    /// author-facing verb phrase ("delete area", "mutate area", ...), so the
    /// script-visible message names what actually failed; creation paths keep
    /// [`Self::FailedToCreate`]'s established text.
    #[class(generic)]
    #[error("Failed to {operation}: {message}")]
    OperationFailed {
        operation: &'static str,
        message: String,
    },
    /// A capability gate denied a mapper op (see `PACKAGE-ISOLATES-OP-CAPABILITIES.md`).
    /// Same `NotCapable`-style message + generic class as the `smudgy_ops` gate, so author
    /// debugging is uniform.
    #[class(generic)]
    #[error("smudgy: this package did not request the '{0}' capability")]
    NotCapable(&'static str),
    /// Export was denied because the viewer lacks copy rights (`can_copy`) on the area.
    #[class(generic)]
    #[error("smudgy: this map cannot be exported (you do not have copy rights to it)")]
    NotCopyable,
}

/// Gate a mapper op on the isolate's [`SmudgyGrants`] (seeded into `OpState` by the `smudgy_ops`
/// extension, always present alongside `smudgy_mapper`): a READ op needs `mapper_read`, a WRITE op
/// needs `mapper_write` (see `PACKAGE-ISOLATES-OP-CAPABILITIES.md`). Only the ops that reach `Mapper`
/// through `OpState` are gated — the `&JSArea`/`&JSRoom` wrapper accessors operate on a handle the
/// script must first obtain via one of these gated entry ops, so they need no separate check.
fn ensure_mapper(state: &OpState, write: bool) -> Result<(), MapperError> {
    let grants = *state.borrow::<SmudgyGrants>();
    let (allowed, cap) = if write {
        (grants.mapper_write, "mapper-write")
    } else {
        (grants.mapper_read, "mapper-read")
    };
    if allowed {
        Ok(())
    } else {
        Err(MapperError::NotCapable(cap))
    }
}

/// Build a [`MapperError::OperationFailed`] mapper for a named operation.
fn operation_failed<E: std::fmt::Display>(operation: &'static str) -> impl Fn(E) -> MapperError {
    move |error| MapperError::OperationFailed {
        operation,
        message: error.to_string(),
    }
}

async fn await_mapper_submission(
    mapper: &Mapper,
    submission: MutationSubmission,
) -> Result<Option<(u64, u64)>, MapperError> {
    let Some(operation_id) = submission.operation_id() else {
        return Ok(None);
    };
    mapper
        .wait_for_mutation(operation_id)
        .await
        .map_err(operation_failed("commit map changes"))?;
    Ok(Some(operation_id.as_u64_pair()))
}

/// A room reference as serialized to JS: the area id as a `u64` pair plus the
/// room number.
type JsRoomRef = ((u64, u64), i32);

/// Queue a bind-on-use navigation hint for the UI daemon. A demonstrated
/// speedwalk / find-nearest resolution into `area_id` is evidence the player is
/// navigating in that area's map; the daemon (which owns the per-server scope
/// store) decides whether the area's atlas is unassigned and worth binding, so
/// the op stays policy-free. Cheap by construction: one `VecDeque` push, dwarfed
/// by the pathfinding that just ran.
fn note_navigation(state: &OpState, area_id: AreaId) {
    state
        .borrow::<ActionQueue>()
        .borrow_mut()
        .push_back(RuntimeAction::NoteMapperNavigation(area_id));
}

#[op2]
#[serde]
fn op_smudgy_mapper_list_area_ids(state: &mut OpState) -> Result<Vec<(u64, u64)>, MapperError> {
    ensure_mapper(state, false)?;
    let mapper = state.try_borrow::<Mapper>();

    if let Some(mapper) = mapper {
        let atlas = mapper.get_current_atlas();

        // Skip areas excluded from identification — whether the user marked
        // them inactive or per-server scoping excludes them — so enumeration
        // (`mapper.areas`) honors the same participation the lookup tables do.
        // Explicit `getAreaById` still resolves an excluded area by id.
        Ok(atlas
            .areas()
            .filter(|area| atlas.is_area_included(area.get_id()))
            .map(|area| area.get_id().0.as_u64_pair())
            .collect::<Vec<_>>())
    } else {
        Ok(vec![])
    }
}

/// Refresh the mapper's complete area projection from its authoritative
/// backends. Package entry points use this before presence-based upserts so
/// startup order or a mapping-owner handoff cannot make a resident map look
/// absent in a stale per-session cache.
#[op2(async(lazy), fast)]
async fn op_smudgy_mapper_refresh_areas(state: Rc<RefCell<OpState>>) -> Result<(), MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, false)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    mapper
        .load_all_areas()
        .await
        .map(|_| ())
        .map_err(operation_failed("refresh areas"))
}

#[op2(async(lazy))]
#[cppgc]
async fn op_smudgy_mapper_create_area(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[serde] options: JsCreateAreaOptions,
) -> Result<JSArea, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        let mapper = state.try_borrow::<Mapper>();
        mapper.cloned()
    };

    if let Some(mapper) = mapper {
        // The deprecated `ephemeral` flag works through 0.5.x; teach the
        // replacement once per isolate. Omitting `storage` altogether is the
        // supported default and draws no notice.
        if options.ephemeral.is_some() {
            warn_ephemeral_create_area_once(&state);
        }
        let atlas_id = options
            .atlas_id
            .map(|(hi, lo)| AtlasId(Uuid::from_u64_pair(hi, lo)));
        if let Some(atlas_id) = atlas_id
            && mapper.atlas_storage(&atlas_id).is_none()
        {
            mapper
                .list_atlases()
                .await
                .map_err(operation_failed("validate destination atlas"))?;
            if mapper.atlas_storage(&atlas_id).is_none() {
                return Err(MapperError::AtlasNotFound);
            }
        }
        let storage = resolve_create_storage(options.storage, options.ephemeral)
            .or_else(|| atlas_id.and_then(|id| mapper.atlas_storage(&id)));
        let id = if let Some(storage) = storage {
            mapper
                .create_area_at(name, MapDestination { storage, atlas_id })
                .await
        } else {
            // No tier was requested: create durable in the default tier —
            // cloud when signed in, local otherwise.
            mapper.create_area(name).await
        }
        .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;

        // A non-ephemeral (cloud-tier) area created from a session is associated
        // with that session's server entry — nothing user-created starts
        // unassigned. Ephemeral areas are session-scoped by nature and get no
        // association. The daemon gates the association on the area actually
        // being cloud-tier (signed in), so a local-tier create is harmless.
        if mapper.area_storage(&id) == MapStorage::Cloud {
            state
                .borrow()
                .borrow::<ActionQueue>()
                .borrow_mut()
                .push_back(RuntimeAction::AssociateCreatedArea(id));
        }

        return mapper
            .get_current_atlas()
            .get_area(&id)
            .map(|area| JSArea(area.clone()))
            .ok_or(MapperError::AreaNotFound);
    }

    Err(MapperError::MapperNotEnabled)
}

#[derive(Debug, Default, Deserialize)]
struct JsCreateAreaOptions {
    #[serde(default)]
    storage: Option<MapStorage>,
    #[serde(default)]
    atlas_id: Option<(u64, u64)>,
    /// `Some` only when the caller actually supplied the deprecated flag;
    /// the fully supported storage-less default arrives as `None`.
    #[serde(default)]
    ephemeral: Option<bool>,
}

/// Per-isolate latch for the `ephemeral`-option deprecation notice.
struct EphemeralCreateAreaWarnIssued;

/// Echo the `ephemeral`-option deprecation notice to the session, once per
/// isolate. First-party packages still pass the flag deliberately through
/// 0.5.x, so the note is informational — it teaches the replacement and
/// never repeats within an isolate's lifetime.
fn warn_ephemeral_create_area_once(state: &Rc<RefCell<OpState>>) {
    let mut state = state.borrow_mut();
    if state
        .try_borrow::<EphemeralCreateAreaWarnIssued>()
        .is_some()
    {
        return;
    }
    state.put(EphemeralCreateAreaWarnIssued);
    log::warn!(
        "smudgy: mapper.createArea was called with the deprecated ephemeral option (supported through 0.5.x; use storage: \"session\")"
    );
    state
        .borrow::<ActionQueue>()
        .borrow_mut()
        .push_back(RuntimeAction::Echo(Arc::new(
            "[mapper] A script passed the deprecated ephemeral option to createArea. Maps \
             were created normally; the option keeps working through Smudgy 0.5.x. Scripts \
             should select the session tier with { storage: \"session\" } instead before 0.6."
                .to_string(),
        )));
}

/// Resolve the storage tier requested by a `createArea` call: an explicit
/// `storage` wins, then the deprecated `ephemeral` flag's mapping. `None`
/// means no tier was requested; the caller falls through to the atlas's tier
/// or the supported default (durable: cloud when signed in, local otherwise).
fn resolve_create_storage(
    explicit: Option<MapStorage>,
    ephemeral: Option<bool>,
) -> Option<MapStorage> {
    explicit.or_else(|| compat_ephemeral_storage(ephemeral))
}

/// Map the deprecated `ephemeral` creation flag onto the storage model:
/// `true` selects the session tier, `false` requests nothing. This
/// compatibility mapping is supported through 0.5.x and must be removed in
/// 0.6.0 along with the flag itself.
fn compat_ephemeral_storage(ephemeral: Option<bool>) -> Option<MapStorage> {
    (ephemeral == Some(true)).then_some(MapStorage::Session)
}

#[derive(Debug, Serialize)]
struct JsAtlas {
    id: (u64, u64),
    name: String,
    storage: MapStorage,
}

#[derive(Debug, Deserialize)]
struct JsMapDestination {
    storage: MapStorage,
    #[serde(default)]
    atlas_id: Option<(u64, u64)>,
}

impl JsMapDestination {
    fn into_destination(self) -> MapDestination {
        MapDestination {
            storage: self.storage,
            atlas_id: self
                .atlas_id
                .map(|(hi, lo)| AtlasId(Uuid::from_u64_pair(hi, lo))),
        }
    }
}

/// The serialized name of a storage tier, as the `MapStorage` string union.
fn storage_str(storage: MapStorage) -> &'static str {
    match storage {
        MapStorage::Session => "session",
        MapStorage::Local => "local",
        MapStorage::Cloud => "cloud",
    }
}

/// Live tier read backing `Area.storage`. An absent mapper or an id the
/// current atlas no longer holds (a deleted area's stale handle) errors like
/// the other area ops, rather than defaulting to a tier the area is not in.
#[op2]
#[string]
fn op_smudgy_mapper_get_area_storage(
    state: &OpState,
    #[cppgc] area_wrapper: &JSArea,
) -> Result<&'static str, MapperError> {
    let mapper = state
        .try_borrow::<Mapper>()
        .ok_or(MapperError::MapperNotEnabled)?;
    let area_id = area_wrapper.0.get_id();
    if mapper.get_current_atlas().get_area(area_id).is_none() {
        return Err(MapperError::AreaNotFound);
    }
    Ok(storage_str(mapper.area_storage(area_id)))
}

/// Live tier read backing `Atlas.storage`. A relocation mints a new atlas id,
/// so a stale source handle errors after a move instead of lying that every
/// unknown id is cloud-owned; callers use the `Atlas` returned by `moveAtlas`.
#[op2]
#[string]
fn op_smudgy_mapper_get_atlas_storage(
    state: &OpState,
    #[serde] atlas_id: (u64, u64),
) -> Result<&'static str, MapperError> {
    ensure_mapper(state, false)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .ok_or(MapperError::MapperNotEnabled)?;
    let atlas_id = AtlasId(Uuid::from_u64_pair(atlas_id.0, atlas_id.1));
    mapper
        .atlas_storage(&atlas_id)
        .map(storage_str)
        .ok_or(MapperError::AtlasNotFound)
}

#[op2(async(lazy), fast)]
#[serde]
async fn op_smudgy_mapper_list_atlases(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<JsAtlas>, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, false)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let atlases = mapper
        .list_atlases()
        .await
        .map_err(operation_failed("list atlases"))?;
    Ok(atlases
        .into_iter()
        .map(|atlas| JsAtlas {
            id: atlas.id.0.as_u64_pair(),
            name: atlas.name,
            storage: mapper
                .atlas_storage(&atlas.id)
                .expect("list_atlases records every returned atlas tier"),
        })
        .collect())
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_create_atlas(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[serde] storage: MapStorage,
) -> Result<JsAtlas, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let atlas = mapper
        .create_atlas_at(name, storage)
        .await
        .map_err(operation_failed("create atlas"))?;
    if storage == MapStorage::Cloud {
        state
            .borrow()
            .borrow::<ActionQueue>()
            .borrow_mut()
            .push_back(RuntimeAction::AssociateCreatedAtlas(atlas.id));
    }
    Ok(JsAtlas {
        id: atlas.id.0.as_u64_pair(),
        name: atlas.name,
        storage,
    })
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_relocate_areas(
    state: Rc<RefCell<OpState>>,
    #[serde] source_ids: Vec<(u64, u64)>,
    #[serde] destination: JsMapDestination,
    move_source: bool,
) -> Result<Vec<(u64, u64)>, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let result = mapper
        .relocate_areas(
            source_ids
                .into_iter()
                .map(|(hi, lo)| AreaId(Uuid::from_u64_pair(hi, lo)))
                .collect(),
            destination.into_destination(),
            if move_source {
                RelocationMode::Move
            } else {
                RelocationMode::Copy
            },
        )
        .await
        .map_err(operation_failed(if move_source {
            "move areas"
        } else {
            "copy areas"
        }))?;
    if result.destination.storage == MapStorage::Cloud {
        let state = state.borrow();
        let mut queue = state.borrow::<ActionQueue>().borrow_mut();
        for area_id in &result.destination_ids {
            queue.push_back(RuntimeAction::AssociateCreatedArea(*area_id));
        }
    }
    Ok(result
        .destination_ids
        .into_iter()
        .map(|id| id.0.as_u64_pair())
        .collect())
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_relocate_atlas(
    state: Rc<RefCell<OpState>>,
    #[serde] source_id: (u64, u64),
    #[serde] storage: MapStorage,
    move_source: bool,
) -> Result<JsAtlas, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let result = mapper
        .relocate_atlas(
            AtlasId(Uuid::from_u64_pair(source_id.0, source_id.1)),
            storage,
            if move_source {
                RelocationMode::Move
            } else {
                RelocationMode::Copy
            },
        )
        .await
        .map_err(operation_failed(if move_source {
            "move atlas"
        } else {
            "copy atlas"
        }))?;
    if storage == MapStorage::Cloud {
        state
            .borrow()
            .borrow::<ActionQueue>()
            .borrow_mut()
            .push_back(RuntimeAction::AssociateCreatedAtlas(
                result.destination_atlas_id,
            ));
    }
    Ok(JsAtlas {
        id: result.destination_atlas_id.0.as_u64_pair(),
        name: result.destination_atlas_name,
        storage,
    })
}

pub struct JSArea(pub Arc<AreaCache>);

unsafe impl GarbageCollected for JSArea {
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"Area"
    }

    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

pub struct JSRoom(pub Arc<RoomCache>, pub AreaId);

unsafe impl GarbageCollected for JSRoom {
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"Room"
    }

    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

#[op2]
#[cppgc]
fn op_smudgy_mapper_get_area_by_id(
    state: Rc<RefCell<OpState>>,
    #[serde] id: (u64, u64),
) -> Result<JSArea, MapperError> {
    let atlas = {
        let state = state.borrow();
        ensure_mapper(&state, false)?;
        let mapper = state.try_borrow::<Mapper>();
        mapper.map(smudgy_cloud::Mapper::get_current_atlas)
    };

    if let Some(atlas) = atlas {
        let id = AreaId(Uuid::from_u64_pair(id.0, id.1));
        if let Some(area) = atlas.get_area(&id) {
            return Ok(JSArea(area.clone()));
        }
        return Err(MapperError::AreaNotFound);
    }

    Err(MapperError::MapperNotEnabled)
}

#[op2(async(lazy))]
async fn op_smudgy_mapper_delete_area(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
) -> Result<(), MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    mapper
        .delete_area_and_wait(id)
        .await
        .map_err(operation_failed("delete area"))
}

#[op2(async(lazy))]
async fn op_smudgy_mapper_rename_area(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[string] name: String,
) -> Result<(), MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?
    };
    let id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    mapper
        .rename_area_and_wait(id, name.as_str())
        .await
        .map_err(operation_failed("rename area"))
}

/// AREA WRAPPER METHODS
///
#[op2]
fn op_smudgy_mapper_get_area_name<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] area_wrapper: &JSArea,
) -> v8::Local<'a, v8::String> {
    v8::String::new(scope, area_wrapper.0.get_name())
        .unwrap_or_else(|| v8::String::new(scope, "unknown").expect("Failed to create string"))
}

#[op2]
#[serde]
fn op_smudgy_mapper_get_area_id(#[cppgc] area_wrapper: &JSArea) -> (u64, u64) {
    area_wrapper.0.get_id().0.as_u64_pair()
}

/// `area.uuid`: the area id as its canonical hyphenated lowercase UUID
/// string -- the JSON-safe spelling of `area.id`, matching the `map:room`
/// event's `areaId` field and the spelling MapView apply-area scoping
/// accepts. Wrapper accessor on a `JSArea` handle -- not gated.
#[op2]
#[string]
fn op_smudgy_mapper_get_area_uuid(#[cppgc] area_wrapper: &JSArea) -> String {
    area_wrapper.0.get_id().to_string()
}

/// `area.isEphemeral`: whether the area lives in the session-lifetime
/// ephemeral tier. Wrapper accessor on a `JSArea` handle -- not gated.
#[op2(fast)]
fn op_smudgy_mapper_get_area_is_ephemeral(state: &OpState, #[cppgc] area_wrapper: &JSArea) -> bool {
    state
        .try_borrow::<Mapper>()
        .is_some_and(|mapper| mapper.area_storage(area_wrapper.0.get_id()) == MapStorage::Session)
}
#[op2]
#[serde]
fn op_smudgy_mapper_list_area_room_numbers(#[cppgc] area_wrapper: &JSArea) -> Vec<i32> {
    area_wrapper
        .0
        .get_rooms()
        .iter()
        .map(|room| room.get_room_number().0)
        .collect()
}

#[op2]
#[serde]
fn op_smudgy_mapper_list_rooms_by_title_and_description(
    state: &OpState,
    #[string] title: &str,
    #[string] description: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    ensure_mapper(state, false)?;
    let mapper = state.try_borrow::<Mapper>();

    if let Some(mapper) = mapper {
        let atlas = mapper.get_current_atlas();
        let rooms = atlas.get_rooms_by_title_and_description(title, description);
        Ok(rooms
            .map(|(area_id, room)| (area_id.0.as_u64_pair(), room.get_room_number().0))
            .collect())
    } else {
        Ok(vec![])
    }
}

#[op2]
#[serde]
fn op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits(
    state: &OpState,
    #[string] title: &str,
    #[string] description: &str,
    #[serde] visible_exit_directions: Vec<ExitDirection>,
) -> Result<Vec<JsRoomRef>, MapperError> {
    ensure_mapper(state, false)?;
    let mapper = state.try_borrow::<Mapper>();

    if let Some(mapper) = mapper {
        let atlas = mapper.get_current_atlas();
        let rooms = atlas.get_rooms_by_title_description_and_visible_exits(
            title,
            description,
            visible_exit_directions.iter(),
        );
        Ok(rooms
            .map(|(area_id, room)| (area_id.0.as_u64_pair(), room.get_room_number().0))
            .collect())
    } else {
        Ok(vec![])
    }
}

#[op2]
#[cppgc]
fn op_smudgy_mapper_get_area_room_by_number(
    #[cppgc] area_wrapper: &JSArea,
    room_number: i32,
) -> Option<JSRoom> {
    area_wrapper
        .0
        .get_room(&RoomNumber(room_number))
        .map(|room| JSRoom(room.clone(), *area_wrapper.0.get_id()))
}

#[op2]
fn op_smudgy_mapper_get_area_property<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] area_wrapper: &JSArea,
    #[string] name: String,
) -> v8::Local<'a, v8::Value> {
    match area_wrapper.0.get_property(&name) {
        Some(property) => v8::String::new(scope, property)
            .expect("Invalid property")
            .into(),
        None => v8::undefined(scope).into(),
    }
}

/// Reservation-aware: consults the live allocator (which skips numbers held
/// by open `mutateArea` drafts) and falls back to the wrapper's snapshot when
/// the session has no mapper attached.
#[op2(fast)]
#[smi]
fn op_smudgy_mapper_get_area_next_room_number(
    state: &OpState,
    #[cppgc] area_wrapper: &JSArea,
) -> i32 {
    state
        .try_borrow::<Mapper>()
        .and_then(|mapper| mapper.next_room_number(area_wrapper.0.get_id()))
        .map_or_else(
            || area_wrapper.0.get_max_room_number().0 + 1,
            |number| number.0,
        )
}

/// Reserve the next free room number for an open scripted mutator. Ambient
/// creation skips the number until every reservation under `token` is
/// released; releasing without committing returns it to the allocator.
#[op2]
#[smi]
fn op_smudgy_mapper_reserve_room_number(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] token: (u64, u64),
) -> Result<i32, MapperError> {
    ensure_mapper(state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .ok_or(MapperError::MapperNotEnabled)?;
    let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    let token = Uuid::from_u64_pair(token.0, token.1);
    mapper
        .reserve_room_number(&area_id, token)
        .map(|number| number.0)
        .map_err(|_| MapperError::AreaNotFound)
}

/// Release every room-number reservation held under `token` for an area.
/// Idempotent; called when a mutator finishes or aborts.
#[op2]
fn op_smudgy_mapper_release_room_reservations(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] token: (u64, u64),
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        mapper.release_room_reservations(
            &AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            Uuid::from_u64_pair(token.0, token.1),
        );
    }
    Ok(())
}

/// ROOM WRAPPER METHODS
///
///
#[op2]
#[serde]
fn op_smudgy_mapper_get_room_area_id(#[cppgc] room_wrapper: &JSRoom) -> (u64, u64) {
    room_wrapper.1.0.as_u64_pair()
}

#[op2(fast)]
#[smi]
fn op_smudgy_mapper_get_room_number(#[cppgc] room_wrapper: &JSRoom) -> i32 {
    room_wrapper.0.get_room_number().0
}

#[op2]
fn op_smudgy_mapper_get_room_title<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] room_wrapper: &JSRoom,
) -> v8::Local<'a, v8::String> {
    v8::String::new(scope, room_wrapper.0.get_title()).expect("Failed to create string")
}

#[op2]
fn op_smudgy_mapper_get_room_description<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] room_wrapper: &JSRoom,
) -> v8::Local<'a, v8::String> {
    v8::String::new(scope, room_wrapper.0.get_description()).expect("Failed to create string")
}

#[op2]
fn op_smudgy_mapper_get_room_color<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] room_wrapper: &JSRoom,
) -> v8::Local<'a, v8::String> {
    v8::String::new(scope, room_wrapper.0.get_color()).expect("Failed to create string")
}

#[op2(fast)]
#[smi]
fn op_smudgy_mapper_get_room_level(#[cppgc] room_wrapper: &JSRoom) -> i32 {
    room_wrapper.0.get_level()
}

#[op2(fast)]
fn op_smudgy_mapper_get_room_x(#[cppgc] room_wrapper: &JSRoom) -> f32 {
    room_wrapper.0.get_x()
}

#[op2(fast)]
fn op_smudgy_mapper_get_room_y(#[cppgc] room_wrapper: &JSRoom) -> f32 {
    room_wrapper.0.get_y()
}

/// `room.externalId`: the room's server-global external id, or `undefined`.
/// Wrapper accessor on a `JSRoom` handle -- not gated.
#[op2]
#[string]
fn op_smudgy_mapper_get_room_external_id(#[cppgc] room_wrapper: &JSRoom) -> Option<String> {
    room_wrapper.0.get_external_id().map(str::to_string)
}

#[op2]
fn op_smudgy_mapper_get_room_property<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[cppgc] room_wrapper: &JSRoom,
    #[string] name: String,
) -> v8::Local<'a, v8::Value> {
    match room_wrapper.0.get_property(&name) {
        Some(property) => v8::String::new(scope, property)
            .expect("Invalid property")
            .into(),
        None => v8::undefined(scope).into(),
    }
}

/// The room's tags, normalized to UPPERCASE and sorted. A wrapper accessor on a
/// handle the script already obtained through a gated entry op, so it is not
/// separately capability-gated (see [`ensure_mapper`]).
#[op2]
#[serde]
fn op_smudgy_mapper_get_room_tags(#[cppgc] room_wrapper: &JSRoom) -> Vec<String> {
    room_wrapper.0.tags().map(String::from).collect()
}

/// Case-insensitive tag-membership test. Wrapper accessor — not gated.
#[op2(fast)]
fn op_smudgy_mapper_has_tag(#[cppgc] room_wrapper: &JSRoom, #[string] tag: String) -> bool {
    room_wrapper.0.has_tag(&tag)
}

#[derive(Debug, Serialize)]
struct JSExit {
    id: (u64, u64),
    from_direction: String,
    from_area_id: (u64, u64),
    from_room_number: i32,
    to_direction: Option<String>,
    to_area_id: Option<(u64, u64)>,
    to_room_number: Option<i32>,
    is_hidden: bool,
    is_closed: bool,
    is_locked: bool,
    weight: f32,
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JSExitCreateParams {
    from_direction: ExitDirection,
    to_direction: Option<ExitDirection>,
    to_area_id: Option<(u64, u64)>,
    to_room_number: Option<i32>,
    is_hidden: Option<bool>,
    is_closed: Option<bool>,
    is_locked: Option<bool>,
    weight: Option<f32>,
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JSExitUpdateParams {
    from_direction: Option<ExitDirection>,
    to_direction: Option<ExitDirection>,
    to_area_id: Option<(u64, u64)>,
    to_room_number: Option<i32>,
    is_hidden: Option<bool>,
    is_closed: Option<bool>,
    is_locked: Option<bool>,
    weight: Option<f32>,
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JSRoomParams {
    title: Option<String>,
    description: Option<String>,
    color: Option<String>,
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    #[serde(rename = "externalId")]
    external_id: Option<String>,
}

impl From<JSRoomParams> for RoomUpdates {
    /// Project the script-supplied room fields onto a cloud `RoomUpdates` (all-`Option`, so an
    /// absent field is left unchanged). `is_secret` is never settable from a script. The script
    /// spelling of "clear the external id" is the empty string (a JS-friendly stand-in for the
    /// wire's present-but-null).
    fn from(params: JSRoomParams) -> Self {
        Self {
            title: params.title,
            description: params.description,
            level: params.level,
            x: params.x,
            y: params.y,
            color: params.color,
            is_secret: None,
            external_id: params
                .external_id
                .map(|id| if id.is_empty() { None } else { Some(id) }),
        }
    }
}

fn submit_room_update(
    state: &Rc<RefCell<OpState>>,
    area_id: (u64, u64),
    room_number: i32,
    updates: RoomUpdates,
) -> Result<(Mapper, MutationSubmission), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let submission = mapper
        .upsert_room(
            RoomKey {
                area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                room_number: RoomNumber(room_number),
            },
            updates,
        )
        .map_err(operation_failed("update room"))?;
    Ok((mapper, submission))
}

#[op2]
#[serde]
fn op_smudgy_mapper_get_room_exits(#[cppgc] room_wrapper: &JSRoom) -> Vec<JSExit> {
    room_wrapper
        .0
        .get_exits()
        .iter()
        .map(|exit| JSExit {
            id: exit.id.0.as_u64_pair(),
            from_direction: exit.from_direction.to_string(),
            from_area_id: room_wrapper.1.0.as_u64_pair(),
            from_room_number: room_wrapper.0.get_room_number().0,
            to_direction: exit.to_direction.map(|direction| direction.to_string()),
            to_area_id: exit.to_area_id.map(|area_id| area_id.0.as_u64_pair()),
            to_room_number: exit.to_room_number.map(|room_number| room_number.0),
            is_hidden: exit.is_hidden,
            is_closed: exit.is_closed,
            is_locked: exit.is_locked,
            weight: exit.weight,
            command: exit.command.clone(),
        })
        .collect()
}

/// ROOM SETTER METHODS
///
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_title(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] title: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            title: Some(title),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_external_id(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] external_id: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let binding = if external_id.is_empty() {
        None
    } else {
        Some(external_id)
    };
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            external_id: Some(binding),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

/// `mapper.findRoomByExternalId`: O(1) resolve of a server-global room id
/// against the atlas cache's reverse index. Best-effort under duplicate
/// bindings; disabled areas don't resolve.
#[op2]
#[serde]
fn op_smudgy_mapper_find_room_by_external_id(
    state: Rc<RefCell<OpState>>,
    #[string] external_id: String,
) -> Result<Option<JsRoomRef>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        Ok(mapper
            .get_current_atlas()
            .find_room_by_external_id(&external_id)
            .map(|(room_key, _)| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0)))
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// Cross-entry rescue check, called by the auto-mapper before it mints a room:
/// if this server-global id is already mapped on a *different* server entry (a
/// scope-excluded area), raise the "show here too?" offer and return `true` so
/// the caller does not create a duplicate. Returns `false` when the id is
/// unknown elsewhere, leaving the caller free to map it as new terrain.
///
/// The policy — whether to offer, how to phrase it, the once-per-atlas
/// rate-limit, and the association write on accept — lives entirely in the UI
/// daemon; this op only reports the match and hands the daemon the atlas
/// context. Keeping the offer a side effect of the same call keeps the calling
/// package to a single decision ("was it rescued? then don't create").
#[op2(fast)]
fn op_smudgy_mapper_rescue_room_by_external_id(
    state: Rc<RefCell<OpState>>,
    #[string] external_id: String,
) -> Result<bool, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    let Some(mapper) = state.try_borrow::<Mapper>() else {
        return Err(MapperError::MapperNotEnabled);
    };
    let Some(hit) = mapper.find_room_elsewhere_by_external_id(&external_id) else {
        return Ok(false);
    };
    state
        .borrow::<ActionQueue>()
        .borrow_mut()
        .push_back(RuntimeAction::OfferMapRescue {
            area_id: hit.room_key.area_id,
            atlas_id: hit.atlas_id,
            atlas_name: hit.atlas_name,
        });
    Ok(true)
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_description(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] description: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            description: Some(description),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_color(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] color: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            color: Some(color),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_level(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    level: i32,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            level: Some(level),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_x(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    x: f32,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            x: Some(x),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_y(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    y: f32,
) -> Result<Option<(u64, u64)>, MapperError> {
    let (mapper, submission) = submit_room_update(
        &state,
        area_id,
        room_number,
        RoomUpdates {
            y: Some(y),
            ..Default::default()
        },
    )?;
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_property(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] name: String,
    #[string] value: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let submission = mapper
            .set_room_property(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                },
                name,
                value,
            )
            .map_err(operation_failed("update room"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_area_property(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[string] name: String,
    #[string] value: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let submission = mapper
            .set_area_property(area_id, name, value)
            .map_err(operation_failed("update area"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_add_room_tag(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] tag: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let submission = mapper
            .add_room_tag(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                },
                tag,
            )
            .map_err(operation_failed("add room tag"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_remove_room_tag(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] tag: String,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let submission = mapper
            .remove_room_tag(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                },
                tag,
            )
            .map_err(operation_failed("remove room tag"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[smi]
async fn op_smudgy_mapper_create_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] params: JSRoomParams,
) -> Result<i32, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        // Reservation-aware allocation: numbers drafted by an open
        // `mutateArea` callback are skipped, so an ambient create landing
        // mid-callback cannot silently merge with a draft.
        let Some(room_number) = mapper.next_room_number(&area_id) else {
            return Err(MapperError::AreaNotFound);
        };

        // Create-only submission: a cross-client race on this number is
        // refused (`room_number_exists`) instead of silently merging two
        // logical rooms. Server floor: requires the smudgy-web release that
        // knows `create_room` (server-ships-before-client).
        let submission = mapper
            .create_room(
                RoomKey {
                    area_id,
                    room_number,
                },
                params.into(),
            )
            .map_err(|error| MapperError::FailedToCreate(error.to_string()))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await?;
        Ok(room_number.0)
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `updateRoom(area, room, fields)`: upsert multiple room fields in ONE cache update
/// (one index rebuild) instead of N `setRoomX` ops. Only the fields present in `params` change;
/// absent fields are left untouched (`RoomUpdates` is all-`Option`). Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_update_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] params: JSRoomParams,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let submission = mapper
            .upsert_room(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                },
                params.into(),
            )
            .map_err(operation_failed("update room"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `updateRooms(area, [[n, fields], ...])`: batch-upsert many rooms of one area in a single
/// cache update (one index rebuild) via the cloud `upsert_rooms`. Each entry is a
/// `(room_number, fields)` pair; only the present fields of each change. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_update_rooms(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] updates: Vec<(i32, JSRoomParams)>,
) -> Result<Vec<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let updates = updates
            .into_iter()
            .map(|(room_number, params)| (RoomNumber(room_number), params.into()))
            .collect();
        let submissions = mapper
            .upsert_rooms(area_id, updates)
            .map_err(operation_failed("update rooms"))?;
        drop(state);
        let mut operation_ids = Vec::new();
        for submission in submissions {
            if let Some(operation_id) = await_mapper_submission(&mapper, submission).await? {
                operation_ids.push(operation_id);
            }
        }
        Ok(operation_ids)
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_create_room_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] params: JSExitCreateParams,
) -> Result<(u64, u64), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        drop(state);

        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let id = ExitId::new();
        let submission = mapper
            .mutate_area(
                area_id,
                vec![AreaMutation::CreateExit {
                    room_number: RoomNumber(room_number),
                    body: ExitArgs {
                        id: Some(id),
                        connection_id: None,
                        new_connection_id: None,
                        from_direction: params.from_direction,
                        to_direction: params.to_direction,
                        to_area_id: params
                            .to_area_id
                            .map(|area_id| AreaId(Uuid::from_u64_pair(area_id.0, area_id.1))),
                        to_room_number: params.to_room_number.map(RoomNumber),
                        is_hidden: params.is_hidden.unwrap_or(false),
                        is_closed: params.is_closed.unwrap_or(false),
                        is_locked: params.is_locked.unwrap_or(false),
                        weight: params.weight.unwrap_or(1.0),
                        command: params.command,
                        path: None,
                        is_secret: None,
                    },
                }],
                "Create scripted exit",
            )
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
        await_mapper_submission(&mapper, submission).await?;
        Ok(id.0.as_u64_pair())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_room_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] exit_id: (u64, u64),
    #[serde] params: JSExitUpdateParams,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        drop(state);
        let submission = mapper
            .update_exit(
                RoomKey {
                    area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                    room_number: RoomNumber(room_number),
                },
                ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
                ExitUpdates {
                    from_direction: params.from_direction,
                    to_direction: params.to_direction,
                    to_area_id: params
                        .to_area_id
                        .map(|area_id| AreaId(Uuid::from_u64_pair(area_id.0, area_id.1))),
                    to_room_number: params.to_room_number.map(RoomNumber),
                    is_hidden: params.is_hidden,
                    is_closed: params.is_closed,
                    is_locked: params.is_locked,
                    weight: params.weight,
                    command: params.command,
                    path: None,
                    is_secret: None,
                    clear_to: None,
                },
            )
            .map_err(operation_failed("update exit"))?;
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_merge_rooms(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    keep_room_number: i32,
    remove_room_number: i32,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        drop(state);
        let submission = mapper
            .merge_rooms(
                AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                RoomNumber(keep_room_number),
                RoomNumber(remove_room_number),
            )
            .map_err(operation_failed("merge rooms"))?;
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_delete_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .delete_room(RoomKey {
                area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                room_number: RoomNumber(room_number),
            })
            .map_err(operation_failed("delete room"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_delete_room_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] exit_id: (u64, u64),
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .delete_exit(
                RoomKey {
                    area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                    room_number: RoomNumber(room_number),
                },
                ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
            )
            .map_err(operation_failed("delete exit"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

// ============================================================================
// Connections: shared topology/appearance queried at area scope and changed
// only through the same atomic mutation envelope as the editor.
// ============================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct JSConnectionEndpoint {
    room_number: i32,
    side: RoomSide,
    port_offset: f32,
    port_mode: PortMode,
}

impl From<ConnectionEndpoint> for JSConnectionEndpoint {
    fn from(endpoint: ConnectionEndpoint) -> Self {
        Self {
            room_number: endpoint.room_number.0,
            side: endpoint.side,
            port_offset: endpoint.port_offset,
            port_mode: endpoint.port_mode,
        }
    }
}

impl From<JSConnectionEndpoint> for ConnectionEndpoint {
    fn from(endpoint: JSConnectionEndpoint) -> Self {
        Self {
            room_number: RoomNumber(endpoint.room_number),
            side: endpoint.side,
            port_offset: endpoint.port_offset,
            port_mode: endpoint.port_mode,
        }
    }
}

#[derive(Debug, Serialize)]
struct JSConnection {
    id: (u64, u64),
    endpoint_a: JSConnectionEndpoint,
    endpoint_b: Option<JSConnectionEndpoint>,
    kind: ConnectionKind,
    routing: ConnectionRouting,
    segment_shape: SegmentShape,
    corner: CornerStyle,
    route_points: Vec<MapPoint>,
    dash: ConnectionDash,
    color: String,
    thickness: f32,
}

impl From<&Connection> for JSConnection {
    fn from(connection: &Connection) -> Self {
        Self {
            id: connection.id.0.as_u64_pair(),
            endpoint_a: connection.endpoint_a.into(),
            endpoint_b: connection.endpoint_b.map(Into::into),
            kind: connection.kind,
            routing: connection.routing,
            segment_shape: connection.segment_shape,
            corner: connection.corner,
            route_points: connection.route_points.clone(),
            dash: connection.dash,
            color: connection.color.clone(),
            thickness: connection.thickness,
        }
    }
}

#[derive(Debug, Deserialize)]
struct JSConnectionUpdateParams {
    endpoint_a: Option<JSConnectionEndpoint>,
    endpoint_b: Option<JSConnectionEndpoint>,
    routing: Option<ConnectionRouting>,
    segment_shape: Option<SegmentShape>,
    corner: Option<CornerStyle>,
    route_points: Option<Vec<MapPoint>>,
    dash: Option<ConnectionDash>,
    color: Option<String>,
    thickness: Option<f32>,
}

impl From<JSConnectionUpdateParams> for ConnectionUpdates {
    fn from(params: JSConnectionUpdateParams) -> Self {
        Self {
            endpoint_a: params.endpoint_a.map(Into::into),
            endpoint_b: params.endpoint_b.map(Into::into),
            routing: params.routing,
            segment_shape: params.segment_shape,
            corner: params.corner,
            route_points: params.route_points,
            dash: params.dash,
            color: params.color,
            thickness: params.thickness,
        }
    }
}

#[derive(Debug, Deserialize)]
struct JSLinkCreateParams {
    endpoint_a: JSConnectionEndpoint,
    endpoint_b: Option<JSConnectionEndpoint>,
    routing: Option<ConnectionRouting>,
    segment_shape: Option<SegmentShape>,
    corner: Option<CornerStyle>,
    route_points: Option<Vec<MapPoint>>,
    dash: Option<ConnectionDash>,
    color: Option<String>,
    thickness: Option<f32>,
    traversals: Vec<JSLinkTraversalParams>,
}

#[derive(Debug, Deserialize)]
struct JSLinkTraversalParams {
    room_number: i32,
    #[serde(flatten)]
    exit: JSExitCreateParams,
}

/// One author-facing operation recorded by `mapper.mutateArea`. These are
/// deliberately higher-level than the wire contract: a `create_link` remains
/// one indivisible author operation even though it expands to a Connection and
/// one or two exits in the mutation envelope.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JSAreaBatchOperation {
    UpsertRoom {
        room_number: i32,
        body: JSRoomParams,
    },
    /// `AreaMutator.createRoom` drafts: must-not-exist creation, so a
    /// number that raced into existence between draft and submission is
    /// refused (`room_number_exists`) instead of merging.
    CreateRoom {
        room_number: i32,
        body: JSRoomParams,
    },
    DeleteRoom {
        room_number: i32,
    },
    UpsertRoomProperty {
        room_number: i32,
        name: String,
        value: String,
    },
    UpsertAreaProperty {
        name: String,
        value: String,
    },
    AddRoomTag {
        room_number: i32,
        tag: String,
    },
    RemoveRoomTag {
        room_number: i32,
        tag: String,
    },
    CreateExit {
        room_number: i32,
        id: (u64, u64),
        body: JSExitCreateParams,
    },
    UpdateExit {
        exit_id: (u64, u64),
        body: JSExitUpdateParams,
    },
    DeleteExit {
        exit_id: (u64, u64),
    },
    CreateLink {
        connection_id: (u64, u64),
        body: JSLinkCreateParams,
    },
    UpdateConnection {
        connection_id: (u64, u64),
        body: JSConnectionUpdateParams,
    },
}

fn script_exit_args(
    params: JSExitCreateParams,
    id: ExitId,
    connection_id: Option<ConnectionId>,
) -> ExitArgs {
    ExitArgs {
        id: Some(id),
        connection_id,
        new_connection_id: None,
        from_direction: params.from_direction,
        to_direction: params.to_direction,
        to_area_id: params
            .to_area_id
            .map(|area_id| AreaId(Uuid::from_u64_pair(area_id.0, area_id.1))),
        to_room_number: params.to_room_number.map(RoomNumber),
        is_hidden: params.is_hidden.unwrap_or(false),
        is_closed: params.is_closed.unwrap_or(false),
        is_locked: params.is_locked.unwrap_or(false),
        weight: params.weight.unwrap_or(1.0),
        command: params.command,
        path: None,
        is_secret: None,
    }
}

fn script_connection_args(
    connection_id: ConnectionId,
    params: &mut JSLinkCreateParams,
) -> ConnectionArgs {
    ConnectionArgs {
        id: connection_id,
        endpoint_a: params.endpoint_a.into(),
        endpoint_b: params.endpoint_b.map(Into::into),
        routing: params.routing.unwrap_or_default(),
        segment_shape: params.segment_shape.unwrap_or_default(),
        corner: params.corner.unwrap_or_default(),
        route_points: std::mem::take(&mut params.route_points).unwrap_or_default(),
        dash: params.dash.unwrap_or_default(),
        color: params
            .color
            .take()
            .unwrap_or_else(|| DEFAULT_CONNECTION_COLOR.to_string()),
        thickness: params.thickness.unwrap_or(DEFAULT_CONNECTION_THICKNESS),
    }
}

impl JSAreaBatchOperation {
    fn into_group(self) -> Vec<AreaMutation> {
        match self {
            Self::UpsertRoom { room_number, body } => vec![AreaMutation::UpsertRoom {
                room_number: RoomNumber(room_number),
                body: body.into(),
            }],
            Self::CreateRoom { room_number, body } => vec![AreaMutation::CreateRoom {
                room_number: RoomNumber(room_number),
                body: body.into(),
            }],
            Self::DeleteRoom { room_number } => vec![AreaMutation::DeleteRoom {
                room_number: RoomNumber(room_number),
            }],
            Self::UpsertRoomProperty {
                room_number,
                name,
                value,
            } => vec![AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(room_number),
                name,
                value,
                is_secret: None,
            }],
            Self::UpsertAreaProperty { name, value } => {
                vec![AreaMutation::UpsertAreaProperty {
                    name,
                    value,
                    is_secret: None,
                }]
            }
            Self::AddRoomTag { room_number, tag } => vec![AreaMutation::AddRoomTag {
                room_number: RoomNumber(room_number),
                tag,
            }],
            Self::RemoveRoomTag { room_number, tag } => vec![AreaMutation::RemoveRoomTag {
                room_number: RoomNumber(room_number),
                tag,
            }],
            Self::CreateExit {
                room_number,
                id,
                body,
            } => vec![AreaMutation::CreateExit {
                room_number: RoomNumber(room_number),
                body: script_exit_args(body, ExitId(Uuid::from_u64_pair(id.0, id.1)), None),
            }],
            Self::UpdateExit { exit_id, body } => vec![AreaMutation::UpdateExit {
                exit_id: ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
                body: ExitUpdates {
                    from_direction: body.from_direction,
                    to_direction: body.to_direction,
                    to_area_id: body
                        .to_area_id
                        .map(|area_id| AreaId(Uuid::from_u64_pair(area_id.0, area_id.1))),
                    to_room_number: body.to_room_number.map(RoomNumber),
                    is_hidden: body.is_hidden,
                    is_closed: body.is_closed,
                    is_locked: body.is_locked,
                    weight: body.weight,
                    command: body.command,
                    path: None,
                    is_secret: None,
                    clear_to: None,
                },
            }],
            Self::DeleteExit { exit_id } => vec![AreaMutation::DeleteExit {
                exit_id: ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
            }],
            Self::CreateLink {
                connection_id,
                mut body,
            } => {
                let connection_id =
                    ConnectionId(Uuid::from_u64_pair(connection_id.0, connection_id.1));
                let mut operations = Vec::with_capacity(body.traversals.len() + 1);
                operations.push(AreaMutation::CreateConnection {
                    body: script_connection_args(connection_id, &mut body),
                });
                operations.extend(body.traversals.into_iter().map(|traversal| {
                    AreaMutation::CreateExit {
                        room_number: RoomNumber(traversal.room_number),
                        body: script_exit_args(traversal.exit, ExitId::new(), Some(connection_id)),
                    }
                }));
                operations
            }
            Self::UpdateConnection {
                connection_id,
                body,
            } => vec![AreaMutation::UpdateConnection {
                connection_id: ConnectionId(Uuid::from_u64_pair(connection_id.0, connection_id.1)),
                body: body.into(),
            }],
        }
    }
}

fn pack_area_batch_operations(
    operations: Vec<JSAreaBatchOperation>,
) -> Result<Vec<Vec<AreaMutation>>, MapperError> {
    let mut chunks = Vec::<Vec<AreaMutation>>::new();
    let mut current = Vec::<AreaMutation>::new();
    for group in operations.into_iter().map(JSAreaBatchOperation::into_group) {
        if group.len() > MAX_MUTATION_OPERATIONS {
            return Err(MapperError::OperationFailed {
                operation: "mutate area",
                message: format!(
                    "one scripted mapper operation expands to {} mutations; the limit is {MAX_MUTATION_OPERATIONS}",
                    group.len()
                ),
            });
        }
        if !current.is_empty() && current.len() + group.len() > MAX_MUTATION_OPERATIONS {
            chunks.push(std::mem::take(&mut current));
        }
        current.extend(group);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

/// Mint an opaque mapper id without touching storage. Scoped batch editors use
/// this so create calls can return stable ids before their envelope is sent.
#[op2]
#[serde]
fn op_smudgy_mapper_generate_id(state: &OpState) -> Result<(u64, u64), MapperError> {
    ensure_mapper(state, true)?;
    Ok(Uuid::new_v4().as_u64_pair())
}

/// The host outcome of a scripted `mutateArea`: the acknowledged envelope
/// operation ids in submission order, plus the failure message when a later
/// envelope failed after earlier ones were already accepted. The TS layer
/// shapes a non-`null` `error` into the thrown `Error` and attaches
/// `committed` as its `committedOperations` property.
#[derive(Serialize)]
struct JsMutateAreaOutcome {
    committed: Vec<(u64, u64)>,
    error: Option<String>,
}

/// Single-area batching for the script API. Author operations are packed
/// without splitting an individual high-level operation; oversized work
/// becomes ordered envelopes staged all-or-nothing (a local validation
/// failure in any envelope publishes none of them). Each envelope remains
/// independently atomic at the backend; acknowledged envelopes are never
/// rolled back, so a mid-sequence backend failure reports the committed
/// prefix instead of discarding it.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_mutate_area(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] operations: Vec<JSAreaBatchOperation>,
    #[string] description: String,
) -> Result<JsMutateAreaOutcome, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    drop(state);

    let chunks = pack_area_batch_operations(operations)?;
    let chunk_count = chunks.len();
    let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    let batches = chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let chunk_description = if chunk_count > 1 {
                format!("{description} ({}/{chunk_count})", index + 1)
            } else {
                description.clone()
            };
            AreaMutationBatch::strict(area_id, chunk, chunk_description)
        })
        .collect();
    let submissions = mapper
        .mutate_batches(batches)
        .map_err(operation_failed("mutate area"))?;

    let mut committed = Vec::with_capacity(chunk_count);
    for submission in submissions {
        match await_mapper_submission(&mapper, submission).await {
            Ok(Some(operation_id)) => committed.push(operation_id),
            Ok(None) => {}
            Err(error) => {
                return Ok(JsMutateAreaOutcome {
                    committed,
                    error: Some(error.to_string()),
                });
            }
        }
    }
    Ok(JsMutateAreaOutcome {
        committed,
        error: None,
    })
}

#[op2]
#[serde]
fn op_smudgy_mapper_get_area_connections(#[cppgc] area_wrapper: &JSArea) -> Vec<JSConnection> {
    area_wrapper
        .0
        .get_connections()
        .iter()
        .map(JSConnection::from)
        .collect()
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_create_link(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] params: JSLinkCreateParams,
) -> Result<(u64, u64), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    let connection_id = ConnectionId::new();
    let mut operations = Vec::with_capacity(params.traversals.len() + 1);
    operations.push(AreaMutation::CreateConnection {
        body: ConnectionArgs {
            id: connection_id,
            endpoint_a: params.endpoint_a.into(),
            endpoint_b: params.endpoint_b.map(Into::into),
            routing: params.routing.unwrap_or_default(),
            segment_shape: params.segment_shape.unwrap_or_default(),
            corner: params.corner.unwrap_or_default(),
            route_points: params.route_points.unwrap_or_default(),
            dash: params.dash.unwrap_or_default(),
            color: params
                .color
                .unwrap_or_else(|| DEFAULT_CONNECTION_COLOR.to_string()),
            thickness: params.thickness.unwrap_or(DEFAULT_CONNECTION_THICKNESS),
        },
    });
    for traversal in params.traversals {
        operations.push(AreaMutation::CreateExit {
            room_number: RoomNumber(traversal.room_number),
            body: ExitArgs {
                id: Some(ExitId::new()),
                connection_id: Some(connection_id),
                new_connection_id: None,
                from_direction: traversal.exit.from_direction,
                to_direction: traversal.exit.to_direction,
                to_area_id: traversal
                    .exit
                    .to_area_id
                    .map(|id| AreaId(Uuid::from_u64_pair(id.0, id.1))),
                to_room_number: traversal.exit.to_room_number.map(RoomNumber),
                is_hidden: traversal.exit.is_hidden.unwrap_or(false),
                is_closed: traversal.exit.is_closed.unwrap_or(false),
                is_locked: traversal.exit.is_locked.unwrap_or(false),
                weight: traversal.exit.weight.unwrap_or(1.0),
                command: traversal.exit.command,
                path: None,
                is_secret: None,
            },
        });
    }
    let submission = mapper
        .mutate_area(area_id, operations, "Create scripted link")
        .map_err(|error| MapperError::FailedToCreate(error.to_string()))?;
    drop(state);
    await_mapper_submission(&mapper, submission).await?;
    Ok(connection_id.0.as_u64_pair())
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_connection(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] connection_id: (u64, u64),
    #[serde] params: JSConnectionUpdateParams,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let submission = mapper
        .mutate_area(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            vec![AreaMutation::UpdateConnection {
                connection_id: ConnectionId(Uuid::from_u64_pair(connection_id.0, connection_id.1)),
                body: params.into(),
            }],
            "Update scripted connection",
        )
        .map_err(operation_failed("update connection"))?;
    drop(state);
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_unlink_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] exit_id: (u64, u64),
) -> Result<(u64, u64), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let connection_id = ConnectionId::new();
    let submission = mapper
        .mutate_area(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            vec![AreaMutation::Unlink {
                exit_id: ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
                new_connection_id: connection_id,
            }],
            "Unlink scripted traversal",
        )
        .map_err(operation_failed("unlink exit"))?;
    drop(state);
    await_mapper_submission(&mapper, submission).await?;
    Ok(connection_id.0.as_u64_pair())
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_pair_connections(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] keep_connection_id: (u64, u64),
    #[serde] merge_connection_id: (u64, u64),
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let submission = mapper
        .mutate_area(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            vec![AreaMutation::Pair {
                keep_connection_id: ConnectionId(Uuid::from_u64_pair(
                    keep_connection_id.0,
                    keep_connection_id.1,
                )),
                merge_connection_id: ConnectionId(Uuid::from_u64_pair(
                    merge_connection_id.0,
                    merge_connection_id.1,
                )),
            }],
            "Pair scripted connections",
        )
        .map_err(operation_failed("pair connections"))?;
    drop(state);
    await_mapper_submission(&mapper, submission).await
}

#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_delete_link(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] connection_id: (u64, u64),
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let submission = mapper
        .mutate_area(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            vec![AreaMutation::DeleteLink {
                connection_id: ConnectionId(Uuid::from_u64_pair(connection_id.0, connection_id.1)),
            }],
            "Delete scripted link",
        )
        .map_err(operation_failed("delete link"))?;
    drop(state);
    await_mapper_submission(&mapper, submission).await
}

// ============================================================================
// Labels + shapes: area-level annotations. Create/delete are write-gated; the
// `area.labels`/`area.shapes` reads are wrapper accessors on a `JSArea` the script
// already obtained through a gated entry op, so they need no separate gate.
// ============================================================================

#[derive(Debug, Serialize)]
struct JSLabel {
    id: (u64, u64),
    level: i32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal_alignment: HorizontalAlignment,
    vertical_alignment: VerticalAlignment,
    text: String,
    color: String,
    background_color: String,
    font_size: i32,
    font_weight: i32,
}

impl From<&Label> for JSLabel {
    fn from(label: &Label) -> Self {
        Self {
            id: label.id.0.as_u64_pair(),
            level: label.level,
            x: label.x,
            y: label.y,
            width: label.width,
            height: label.height,
            horizontal_alignment: label.horizontal_alignment.clone(),
            vertical_alignment: label.vertical_alignment.clone(),
            text: label.text.clone(),
            color: label.color.clone(),
            background_color: label.background_color.clone(),
            font_size: label.font_size,
            font_weight: label.font_weight,
        }
    }
}

#[derive(Debug, Serialize)]
struct JSShape {
    id: (u64, u64),
    level: i32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: ShapeType,
    border_radius: f32,
    stroke_width: f32,
}

impl From<&Shape> for JSShape {
    fn from(shape: &Shape) -> Self {
        Self {
            id: shape.id.0.as_u64_pair(),
            level: shape.level,
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
            background_color: shape.background_color.clone(),
            stroke_color: shape.stroke_color.clone(),
            shape_type: shape.shape_type.clone(),
            border_radius: shape.border_radius,
            stroke_width: shape.stroke_width,
        }
    }
}

/// `createLabel` fields: position, size, and `text` are required; the rest default host-side
/// (mirroring `CreateRoomParams`, where only the essentials are required).
#[derive(Debug, Deserialize)]
struct JSLabelParams {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    text: String,
    level: Option<i32>,
    horizontal_alignment: Option<HorizontalAlignment>,
    vertical_alignment: Option<VerticalAlignment>,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
}

impl From<JSLabelParams> for LabelArgs {
    /// Project the script-supplied label fields onto a cloud `LabelArgs`, filling defaults
    /// (level 0, Center/Center, `#ffffff`, 16px, weight 400). `is_secret` is never settable
    /// from a script (matching the room/exit ops).
    fn from(params: JSLabelParams) -> Self {
        Self {
            // Script-created labels carry no pre-minted identity; the mapper
            // mints one before enqueue.
            id: None,
            level: params.level.unwrap_or(0),
            x: params.x,
            y: params.y,
            width: params.width,
            height: params.height,
            horizontal_alignment: params.horizontal_alignment.unwrap_or_default(),
            vertical_alignment: params.vertical_alignment.unwrap_or_default(),
            text: params.text,
            color: params.color.unwrap_or_else(|| "#ffffff".to_string()),
            background_color: params.background_color,
            font_size: params.font_size.unwrap_or(16),
            font_weight: params.font_weight.unwrap_or(400),
            is_secret: None,
        }
    }
}

/// `setLabel` fields: all optional; only present fields change (mirrors `JSExitUpdateParams`).
#[derive(Debug, Deserialize)]
struct JSLabelUpdateParams {
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    text: Option<String>,
    level: Option<i32>,
    horizontal_alignment: Option<HorizontalAlignment>,
    vertical_alignment: Option<VerticalAlignment>,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
}

impl From<JSLabelUpdateParams> for LabelUpdates {
    /// `is_secret` is never settable from a script.
    fn from(params: JSLabelUpdateParams) -> Self {
        Self {
            level: params.level,
            x: params.x,
            y: params.y,
            width: params.width,
            height: params.height,
            horizontal_alignment: params.horizontal_alignment,
            vertical_alignment: params.vertical_alignment,
            text: params.text,
            color: params.color,
            background_color: params.background_color,
            font_size: params.font_size,
            font_weight: params.font_weight,
            is_secret: None,
        }
    }
}

/// `createShape` fields: position and size are required; the rest default host-side.
#[derive(Debug, Deserialize)]
struct JSShapeParams {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    level: Option<i32>,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: Option<ShapeType>,
    border_radius: Option<f32>,
    stroke_width: Option<f32>,
}

impl From<JSShapeParams> for ShapeArgs {
    /// Project the script-supplied shape fields onto a cloud `ShapeArgs`, filling defaults
    /// (level 0, `Rectangle`, radius 0). `is_secret` is never settable from a script.
    fn from(params: JSShapeParams) -> Self {
        Self {
            // Script-created shapes carry no pre-minted identity; the mapper
            // mints one before enqueue.
            id: None,
            level: params.level.unwrap_or(0),
            x: params.x,
            y: params.y,
            width: params.width,
            height: params.height,
            background_color: params.background_color,
            stroke_color: params.stroke_color,
            shape_type: params.shape_type.unwrap_or_default(),
            border_radius: params.border_radius.unwrap_or(0.0),
            stroke_width: params.stroke_width,
            is_secret: None,
        }
    }
}

/// `setShape` fields: all optional; only present fields change.
#[derive(Debug, Deserialize)]
struct JSShapeUpdateParams {
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    level: Option<i32>,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: Option<ShapeType>,
    border_radius: Option<f32>,
    stroke_width: Option<f32>,
}

impl From<JSShapeUpdateParams> for ShapeUpdates {
    /// `is_secret` is never settable from a script.
    fn from(params: JSShapeUpdateParams) -> Self {
        Self {
            level: params.level,
            x: params.x,
            y: params.y,
            width: params.width,
            height: params.height,
            background_color: params.background_color,
            stroke_color: params.stroke_color,
            shape_type: params.shape_type,
            border_radius: params.border_radius,
            stroke_width: params.stroke_width,
            is_secret: None,
        }
    }
}

/// `area.labels`: the area's text labels. Wrapper accessor on a `JSArea` handle -- not gated.
#[op2]
#[serde]
fn op_smudgy_mapper_get_area_labels(#[cppgc] area_wrapper: &JSArea) -> Vec<JSLabel> {
    area_wrapper.0.labels().iter().map(JSLabel::from).collect()
}

/// `area.shapes`: the area's graphical shapes. Wrapper accessor on a `JSArea` handle -- not gated.
#[op2]
#[serde]
fn op_smudgy_mapper_get_area_shapes(#[cppgc] area_wrapper: &JSArea) -> Vec<JSShape> {
    area_wrapper.0.shapes().iter().map(JSShape::from).collect()
}

/// `createLabel(area, args)`: add a text label to an area; returns its new id. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_create_label(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] params: JSLabelParams,
) -> Result<(u64, u64), MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state.try_borrow::<Mapper>().cloned()
    };
    if let Some(mapper) = mapper {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let id = LabelId(Uuid::new_v4());
        let mut body: LabelArgs = params.into();
        body.id = Some(id);
        let submission = mapper
            .mutate_area(
                area_id,
                vec![AreaMutation::CreateLabel { body }],
                "Create scripted label",
            )
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
        await_mapper_submission(&mapper, submission).await?;
        Ok(id.0.as_u64_pair())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `createShape(area, args)`: add a graphical shape to an area; returns its new id. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_create_shape(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] params: JSShapeParams,
) -> Result<(u64, u64), MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state.try_borrow::<Mapper>().cloned()
    };
    if let Some(mapper) = mapper {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let id = ShapeId(Uuid::new_v4());
        let mut body: ShapeArgs = params.into();
        body.id = Some(id);
        let submission = mapper
            .mutate_area(
                area_id,
                vec![AreaMutation::CreateShape { body }],
                "Create scripted shape",
            )
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
        await_mapper_submission(&mapper, submission).await?;
        Ok(id.0.as_u64_pair())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `deleteLabel(area, labelId)`: remove a label from an area. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_delete_label(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] label_id: (u64, u64),
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .delete_label(
                AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                LabelId(Uuid::from_u64_pair(label_id.0, label_id.1)),
            )
            .map_err(operation_failed("delete label"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `deleteShape(area, shapeId)`: remove a shape from an area. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_delete_shape(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] shape_id: (u64, u64),
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .delete_shape(
                AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                ShapeId(Uuid::from_u64_pair(shape_id.0, shape_id.1)),
            )
            .map_err(operation_failed("delete shape"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `setLabel(area, labelId, updates)`: update an existing label; only the present fields
/// change. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_label(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] label_id: (u64, u64),
    #[serde] params: JSLabelUpdateParams,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .update_label(
                AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                LabelId(Uuid::from_u64_pair(label_id.0, label_id.1)),
                params.into(),
            )
            .map_err(operation_failed("update label"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `setShape(area, shapeId, updates)`: update an existing shape; only the present fields
/// change. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_set_shape(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] shape_id: (u64, u64),
    #[serde] params: JSShapeUpdateParams,
) -> Result<Option<(u64, u64)>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let submission = mapper
            .update_shape(
                AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                ShapeId(Uuid::from_u64_pair(shape_id.0, shape_id.1)),
                params.into(),
            )
            .map_err(operation_failed("update shape"))?;
        drop(state);
        await_mapper_submission(&mapper, submission).await
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

// ============================================================================
// Import / export: whole-area JSON. `importAreas` is the one-shot fast path (avoids replaying a
// map room-by-room); `exportArea` serializes an area and is gated on `can_copy`.
// ============================================================================

/// `importAreas(areas)`: import full areas as new LOCAL areas (fresh ids), returning their ids.
/// Documents are accepted through [`smudgy_cloud::AreaImportDocument`], so a v1 (pre-Connection)
/// export migrates on the way in; anything newer than the client is rejected. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_import_areas(
    state: Rc<RefCell<OpState>>,
    #[serde] areas: Vec<smudgy_cloud::AreaImportDocument>,
) -> Result<Vec<(u64, u64)>, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state.try_borrow::<Mapper>().cloned()
    };
    if let Some(mapper) = mapper {
        let ids = mapper
            .import_areas(
                areas
                    .into_iter()
                    .map(smudgy_cloud::AreaImportDocument::into_inner)
                    .collect(),
            )
            .await
            .map_err(operation_failed("import areas"))?;
        Ok(ids.into_iter().map(|id| id.0.as_u64_pair()).collect())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// The result shape for `importAreasIfAbsent`: the new areas' id pairs plus the names skipped
/// because a resident area already bears them.
#[derive(serde::Serialize)]
struct JsAreasImportedIfAbsent {
    added: Vec<(u64, u64)>,
    skipped: Vec<String>,
}

/// `importAreasIfAbsent(areas)`: presence-checked import — imports only the areas whose name no
/// resident area already bears (shared, disabled, and scope-excluded areas all count as present),
/// after waiting for the session's initial map load. The offer-once seeding primitive: safe to
/// call from package top-level code, which runs before maps load. Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_import_areas_if_absent(
    state: Rc<RefCell<OpState>>,
    #[serde] areas: Vec<smudgy_cloud::AreaImportDocument>,
) -> Result<JsAreasImportedIfAbsent, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state.try_borrow::<Mapper>().cloned()
    };
    if let Some(mapper) = mapper {
        let outcome = mapper
            .import_areas_if_absent(
                areas
                    .into_iter()
                    .map(smudgy_cloud::AreaImportDocument::into_inner)
                    .collect(),
            )
            .await
            .map_err(operation_failed("import areas"))?;
        Ok(JsAreasImportedIfAbsent {
            added: outcome
                .added
                .into_iter()
                .map(|id| id.0.as_u64_pair())
                .collect(),
            skipped: outcome.skipped,
        })
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `exportArea(area)`: serialize an area to its full JSON. Read-gated, plus a per-area `can_copy`
/// gate -- dumping an area to JSON is making a copy, so a read-only share without copy rights is
/// refused. The cache is already viewer-redacted, so this can only emit what the viewer can see.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_export_area(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
) -> Result<AreaWithDetails, MapperError> {
    let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, false)?;
        state.try_borrow::<Mapper>().cloned()
    };
    let Some(mapper) = mapper else {
        return Err(MapperError::MapperNotEnabled);
    };
    match mapper.area_effective_access(area_id) {
        Some(access) if access.can_copy => {}
        Some(_) => return Err(MapperError::NotCopyable),
        None => return Err(MapperError::AreaNotFound),
    }
    mapper
        .export_area(area_id)
        .await
        .map_err(operation_failed("export area"))
}

#[op2]
#[serde]
fn op_smudgy_mapper_get_path_between_rooms(
    state: Rc<RefCell<OpState>>,
    #[serde] from_area_id: (u64, u64),
    from_room_number: i32,
    #[serde] to_area_id: (u64, u64),
    to_room_number: i32,
) -> Result<Vec<JsRoomRef>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let from_room_key = RoomKey {
            area_id: AreaId(Uuid::from_u64_pair(from_area_id.0, from_area_id.1)),
            room_number: RoomNumber(from_room_number),
        };
        let to_room_key = RoomKey {
            area_id: AreaId(Uuid::from_u64_pair(to_area_id.0, to_area_id.1)),
            room_number: RoomNumber(to_room_number),
        };
        let resolved = mapper
            .get_current_atlas()
            .get_path_between_rooms(&from_room_key, &to_room_key)
            .unwrap_or_default();
        // A resolved route into the destination area is demonstrated navigation
        // intent — hint the daemon (bind-on-use). Only on a real path.
        if !resolved.is_empty() {
            note_navigation(&state, to_room_key.area_id);
        }
        let path = resolved
            .into_iter()
            .map(|room_key| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0))
            .collect();
        Ok(path)
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// The nearest reachable room whose tags satisfy a conjunctive filter — carries
/// every tag in `required`, none in `excluded` (both case-insensitive) — as a
/// serialized room ref (or `null`). Backs `findNearestRoomWithTag(s)`. The script
/// resolves the ref to a `Room` via `getAreaById(...).room(...)`, then paths to it
/// with the existing methods. The predicate runs entirely in Rust over the local
/// cache (one normalization, per-room set lookups).
#[op2]
#[serde]
fn op_smudgy_mapper_find_nearest_room_with_tags(
    state: Rc<RefCell<OpState>>,
    #[serde] from_area_id: (u64, u64),
    from_room_number: i32,
    #[serde] required: Vec<String>,
    #[serde] excluded: Vec<String>,
) -> Result<Option<JsRoomRef>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let from_room_key = RoomKey {
            area_id: AreaId(Uuid::from_u64_pair(from_area_id.0, from_area_id.1)),
            room_number: RoomNumber(from_room_number),
        };
        let nearest = mapper.get_current_atlas().find_nearest_room_matching_tags(
            &from_room_key,
            &required,
            &excluded,
        );
        if let Some(room_key) = &nearest {
            note_navigation(&state, room_key.area_id);
        }
        Ok(nearest.map(|room_key| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0)))
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// The nearest reachable room belonging to `target_area_id`, as a serialized
/// room ref (or `null`). Backs `findNearestRoomInArea`. The search runs the
/// same weighted traversal as `getPathBetweenRooms`; a disabled target area is
/// still reachable because the caller named it explicitly. The script resolves
/// the ref to a `Room` via `getAreaById(...).room(...)`.
#[op2]
#[serde]
fn op_smudgy_mapper_find_nearest_room_in_area(
    state: Rc<RefCell<OpState>>,
    #[serde] from_area_id: (u64, u64),
    from_room_number: i32,
    #[serde] target_area_id: (u64, u64),
) -> Result<Option<JsRoomRef>, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        let from_room_key = RoomKey {
            area_id: AreaId(Uuid::from_u64_pair(from_area_id.0, from_area_id.1)),
            room_number: RoomNumber(from_room_number),
        };
        let target_area_id = AreaId(Uuid::from_u64_pair(target_area_id.0, target_area_id.1));
        let nearest = mapper
            .get_current_atlas()
            .find_nearest_room_in_area(&from_room_key, &target_area_id);
        if let Some(room_key) = &nearest {
            note_navigation(&state, room_key.area_id);
        }
        Ok(nearest.map(|room_key| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0)))
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::{
        AreaMutation, JSAreaBatchOperation, JSRoomParams, MAX_MUTATION_OPERATIONS, MapStorage,
        compat_ephemeral_storage, pack_area_batch_operations, resolve_create_storage,
    };
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use serde_json::json;

    #[test]
    fn ephemeral_flag_stays_pinned_to_session_through_0_5() {
        assert_eq!(compat_ephemeral_storage(None), None);
        assert_eq!(
            compat_ephemeral_storage(Some(false)),
            None,
            "an explicit `ephemeral: false` requests no tier, exactly like omitting the flag"
        );
        assert_eq!(
            compat_ephemeral_storage(Some(true)),
            Some(MapStorage::Session)
        );
        assert_eq!(
            resolve_create_storage(Some(MapStorage::Local), Some(true)),
            Some(MapStorage::Local),
            "the canonical explicit storage value wins over the compatibility flag"
        );
        assert_eq!(
            resolve_create_storage(None, None),
            None,
            "the storage-less default requests no tier: durable, cloud when signed in, local otherwise"
        );
    }

    #[test]
    fn scripted_area_batches_split_only_at_operation_boundaries() {
        let operations = (0..MAX_MUTATION_OPERATIONS + 1)
            .map(|index| JSAreaBatchOperation::UpsertRoom {
                room_number: i32::try_from(index + 1).expect("test room number"),
                body: JSRoomParams {
                    title: Some(format!("room {index}")),
                    description: None,
                    color: None,
                    level: None,
                    x: None,
                    y: None,
                    external_id: None,
                },
            })
            .collect();

        let chunks = pack_area_batch_operations(operations).expect("batch packs");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_MUTATION_OPERATIONS);
        assert_eq!(chunks[1].len(), 1);
        assert!(matches!(chunks[1][0], AreaMutation::UpsertRoom { .. }));
    }

    #[test]
    fn scripted_link_stays_whole_when_a_batch_boundary_is_reached() {
        let mut operations = (0..MAX_MUTATION_OPERATIONS - 1)
            .map(|index| JSAreaBatchOperation::UpsertRoom {
                room_number: i32::try_from(index + 1).expect("test room number"),
                body: JSRoomParams {
                    title: None,
                    description: None,
                    color: None,
                    level: None,
                    x: None,
                    y: None,
                    external_id: None,
                },
            })
            .collect::<Vec<_>>();
        operations.push(
            serde_json::from_value(json!({
                "create_link": {
                    "connection_id": [1, 2],
                    "body": {
                        "endpoint_a": {
                            "room_number": 1,
                            "side": "East",
                            "port_offset": 0.5,
                            "port_mode": "AutoPinned"
                        },
                        "endpoint_b": {
                            "room_number": 2,
                            "side": "West",
                            "port_offset": 0.5,
                            "port_mode": "AutoPinned"
                        },
                        "traversals": [
                            {
                                "room_number": 1,
                                "from_direction": "East",
                                "to_direction": "West",
                                "to_area_id": [3, 4],
                                "to_room_number": 2
                            },
                            {
                                "room_number": 2,
                                "from_direction": "West",
                                "to_direction": "East",
                                "to_area_id": [3, 4],
                                "to_room_number": 1
                            }
                        ]
                    }
                }
            }))
            .expect("link payload deserializes"),
        );

        let chunks = pack_area_batch_operations(operations).expect("batch packs");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_MUTATION_OPERATIONS - 1);
        assert_eq!(chunks[1].len(), 3);

        let connection_id = match &chunks[1][0] {
            AreaMutation::CreateConnection { body } => body.id,
            other => panic!("expected connection first, got {other:?}"),
        };
        for mutation in &chunks[1][1..] {
            match mutation {
                AreaMutation::CreateExit { body, .. } => {
                    assert_eq!(body.connection_id, Some(connection_id));
                }
                other => panic!("expected linked exit, got {other:?}"),
            }
        }
    }

    #[test]
    fn externally_tagged_batch_decodes_v8_bigint_ids() {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        let value = runtime
            .execute_script(
                "<mapper-batch-bigint>",
                FastString::from_static(
                    r#"[{ create_exit: {
                        room_number: 1,
                        id: [18446744073709551615n, 9223372036854775808n],
                        body: {
                            from_direction: "East",
                            to_direction: "West",
                            to_area_id: [18446744073709551614n, 9223372036854775809n],
                            to_room_number: 2
                        }
                    } }]"#,
                ),
            )
            .expect("evaluate bigint payload");
        deno_core::scope!(scope, &mut runtime);
        let local = deno_core::v8::Local::new(scope, value);
        let operations: Vec<JSAreaBatchOperation> =
            deno_core::serde_v8::from_v8(scope, local).expect("decode bigint ids");

        let chunks = pack_area_batch_operations(operations).expect("batch packs");
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0][0], AreaMutation::CreateExit { .. }));
    }
}
