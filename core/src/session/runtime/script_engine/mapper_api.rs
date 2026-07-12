use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{
    GarbageCollected, OpState,
    op2, thiserror,
    v8::{self},
};
use serde::{Deserialize, Serialize};

use super::ops::SmudgyGrants;
use smudgy_cloud::{
    AreaId, AreaWithDetails, ExitArgs, ExitDirection, ExitId, ExitStyle, ExitUpdates,
    HorizontalAlignment, Label, LabelArgs, LabelId, LabelUpdates, Mapper, RoomNumber, RoomUpdates,
    Shape, ShapeArgs, ShapeId, ShapeType, ShapeUpdates, Uuid, VerticalAlignment,
    mapper::{RoomKey, area_cache::AreaCache, room_cache::RoomCache},
};

deno_core::extension!(
  smudgy_mapper,
  ops = [
      op_smudgy_mapper_list_area_ids,
      op_smudgy_mapper_create_area,
      op_smudgy_mapper_get_area_by_id,
      op_smudgy_mapper_get_area_name,
      op_smudgy_mapper_get_area_id,
      op_smudgy_mapper_rename_area,
      op_smudgy_mapper_list_area_room_numbers,
      op_smudgy_mapper_list_rooms_by_title_and_description,
      op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits,
      op_smudgy_mapper_get_area_room_by_number,
      op_smudgy_mapper_get_area_property,
      op_smudgy_mapper_get_area_next_room_number,
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
      op_smudgy_mapper_create_room_exit,
      op_smudgy_mapper_set_room_exit,
      op_smudgy_mapper_delete_room,
      op_smudgy_mapper_delete_room_exit,
      op_smudgy_mapper_get_area_labels,
      op_smudgy_mapper_get_area_shapes,
      op_smudgy_mapper_create_label,
      op_smudgy_mapper_create_shape,
      op_smudgy_mapper_set_label,
      op_smudgy_mapper_set_shape,
      op_smudgy_mapper_delete_label,
      op_smudgy_mapper_delete_shape,
      op_smudgy_mapper_import_areas,
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
    #[error("Failed to create map: {0}")]
    FailedToCreate(String),
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

/// A room reference as serialized to JS: the area id as a `u64` pair plus the
/// room number.
type JsRoomRef = ((u64, u64), i32);

#[op2]
#[serde]
fn op_smudgy_mapper_list_area_ids(state: &mut OpState) -> Result<Vec<(u64, u64)>, MapperError> {
    ensure_mapper(state, false)?;
    let mapper = state.try_borrow::<Mapper>();

    if let Some(mapper) = mapper {
        let atlas = mapper.get_current_atlas();

        // Skip areas the user marked inactive so enumeration (`mapper.areas`)
        // honors the same room-identification preference the lookup tables do.
        // Explicit `getAreaById` still resolves an inactive area by id.
        Ok(atlas
            .areas()
            .filter(|area| atlas.is_area_enabled(area.get_id()))
            .map(|area| area.get_id().0.as_u64_pair())
            .collect::<Vec<_>>())
    } else {
        Ok(vec![])
    }
}

#[op2(async(lazy), fast)]
#[cppgc]
async fn op_smudgy_mapper_create_area(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
) -> Result<JSArea, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        let mapper = state.try_borrow::<Mapper>();
        mapper.cloned()
    };

    if let Some(mapper) = mapper {
        let id = mapper
            .create_area(name)
            .await
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;

        return mapper
            .get_current_atlas()
            .get_area(&id)
            .map(|area| JSArea(area.clone()))
            .ok_or(MapperError::AreaNotFound);
    }

    Err(MapperError::MapperNotEnabled)
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

#[op2]
fn op_smudgy_mapper_rename_area(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[string] name: String,
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    let mapper = state.try_borrow::<Mapper>();

    if let Some(mapper) = mapper {
        let id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.rename_area(id, name.as_str());
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
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

#[op2(fast)]
#[smi]
fn op_smudgy_mapper_get_area_next_room_number(#[cppgc] area_wrapper: &JSArea) -> i32 {
    area_wrapper.0.get_max_room_number().0 + 1
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
    style: ExitStyle,
    color: Option<String>,
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
    style: Option<ExitStyle>,
    // Accepted from JS for parity with the exit-update API but dropped on
    // creation: `ExitArgs` has no color field, so this is ignored.
    #[allow(dead_code)]
    color: Option<String>,
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
    style: Option<ExitStyle>,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JSRoomParams {
    title: Option<String>,
    description: Option<String>,
    color: Option<String>,
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
}

impl From<JSRoomParams> for RoomUpdates {
    /// Project the script-supplied room fields onto a cloud `RoomUpdates` (all-`Option`, so an
    /// absent field is left unchanged). `is_secret` is never settable from a script.
    fn from(params: JSRoomParams) -> Self {
        Self {
            title: params.title,
            description: params.description,
            level: params.level,
            x: params.x,
            y: params.y,
            color: params.color,
            is_secret: None,
        }
    }
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
            style: exit.style,
            color: exit.color.clone(),
        })
        .collect()
}

/// ROOM SETTER METHODS
///
#[op2]
fn op_smudgy_mapper_set_room_title(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] title: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                title: Some(title),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_description(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] description: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                description: Some(description),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_color(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] color: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                color: Some(color),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_level(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    level: i32,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                level: Some(level),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_x(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    x: f32,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                x: Some(x),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_y(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    y: f32,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            RoomUpdates {
                y: Some(y),
                ..Default::default()
            },
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_property(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] name: String,
    #[string] value: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.set_room_property(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            name,
            value,
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_area_property(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[string] name: String,
    #[string] value: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.set_area_property(area_id, name, value);
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_add_room_tag(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] tag: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.add_room_tag(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            tag,
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_remove_room_tag(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[string] tag: String,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.remove_room_tag(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            tag,
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
#[smi]
fn op_smudgy_mapper_create_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] params: JSRoomParams,
) -> Result<i32, MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let current_atlas = mapper.get_current_atlas();
        let area = current_atlas.get_area(&area_id);

        if let Some(area) = area {
            let room_number = area.get_max_room_number().0 + 1;

            mapper.upsert_room(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                },
                RoomUpdates {
                    title: params.title,
                    description: params.description,
                    color: params.color,
                    level: params.level,
                    x: params.x,
                    y: params.y,
                    ..Default::default()
                },
            );

            Ok(room_number)
        } else {
            Err(MapperError::AreaNotFound)
        }
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `updateRoom(area, room, fields)`: upsert multiple room fields in ONE cache update
/// (one index rebuild) instead of N `setRoomX` ops. Only the fields present in `params` change;
/// absent fields are left untouched (`RoomUpdates` is all-`Option`). Write-gated.
#[op2]
fn op_smudgy_mapper_update_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] params: JSRoomParams,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        mapper.upsert_room(
            RoomKey {
                area_id,
                room_number: RoomNumber(room_number),
            },
            params.into(),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `updateRooms(area, [[n, fields], ...])`: batch-upsert many rooms of one area in a single
/// cache update (one index rebuild) via the cloud `upsert_rooms`. Each entry is a
/// `(room_number, fields)` pair; only the present fields of each change. Write-gated.
#[op2]
fn op_smudgy_mapper_update_rooms(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    #[serde] updates: Vec<(i32, JSRoomParams)>,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
        let updates = updates
            .into_iter()
            .map(|(room_number, params)| (RoomNumber(room_number), params.into()))
            .collect();
        mapper.upsert_rooms(area_id, updates);
        Ok(())
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

        let id = mapper
            .create_exit(
                RoomKey {
                    area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                    room_number: RoomNumber(room_number),
                },
                ExitArgs {
                    from_direction: params.from_direction,
                    to_direction: params.to_direction,
                    to_area_id: params
                        .to_area_id
                        .map(|area_id| AreaId(Uuid::from_u64_pair(area_id.0, area_id.1))),
                    to_room_number: params
                        .to_room_number
                        .map(RoomNumber),
                    is_hidden: params.is_hidden.unwrap_or(false),
                    is_closed: params.is_closed.unwrap_or(false),
                    is_locked: params.is_locked.unwrap_or(false),
                    weight: params.weight.unwrap_or(1.0),
                    command: params.command,
                    style: params.style,
                    path: None,
                    is_secret: None,
                },
            )
            .await
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;

        Ok(id.0.as_u64_pair())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_set_room_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] exit_id: (u64, u64),
    #[serde] params: JSExitUpdateParams,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        mapper.update_exit(
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
                to_room_number: params
                    .to_room_number
                    .map(RoomNumber),
                is_hidden: params.is_hidden,
                is_closed: params.is_closed,
                is_locked: params.is_locked,
                weight: params.weight,
                command: params.command,
                path: None,
                style: params.style,
                color: params.color,
                is_secret: None,
                clear_to: None,
            },
        );

        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_delete_room(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        mapper.delete_room(RoomKey {
            area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            room_number: RoomNumber(room_number),
        });
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

#[op2]
fn op_smudgy_mapper_delete_room_exit(
    state: Rc<RefCell<OpState>>,
    #[serde] area_id: (u64, u64),
    room_number: i32,
    #[serde] exit_id: (u64, u64),
) -> Result<(), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>().cloned() {
        mapper.delete_exit(
            RoomKey {
                area_id: AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
                room_number: RoomNumber(room_number),
            },
            ExitId(Uuid::from_u64_pair(exit_id.0, exit_id.1)),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
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
        let id = mapper
            .create_label(area_id, params.into())
            .await
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
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
        let id = mapper
            .create_shape(area_id, params.into())
            .await
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
        Ok(id.0.as_u64_pair())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `deleteLabel(area, labelId)`: remove a label from an area. Write-gated.
#[op2]
fn op_smudgy_mapper_delete_label(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] label_id: (u64, u64),
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        mapper.delete_label(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            LabelId(Uuid::from_u64_pair(label_id.0, label_id.1)),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `deleteShape(area, shapeId)`: remove a shape from an area. Write-gated.
#[op2]
fn op_smudgy_mapper_delete_shape(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] shape_id: (u64, u64),
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        mapper.delete_shape(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            ShapeId(Uuid::from_u64_pair(shape_id.0, shape_id.1)),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `setLabel(area, labelId, updates)`: update an existing label; only the present fields
/// change. Write-gated.
#[op2]
fn op_smudgy_mapper_set_label(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] label_id: (u64, u64),
    #[serde] params: JSLabelUpdateParams,
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        mapper.update_label(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            LabelId(Uuid::from_u64_pair(label_id.0, label_id.1)),
            params.into(),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

/// `setShape(area, shapeId, updates)`: update an existing shape; only the present fields
/// change. Write-gated.
#[op2]
fn op_smudgy_mapper_set_shape(
    state: &OpState,
    #[serde] area_id: (u64, u64),
    #[serde] shape_id: (u64, u64),
    #[serde] params: JSShapeUpdateParams,
) -> Result<(), MapperError> {
    ensure_mapper(state, true)?;
    if let Some(mapper) = state.try_borrow::<Mapper>() {
        mapper.update_shape(
            AreaId(Uuid::from_u64_pair(area_id.0, area_id.1)),
            ShapeId(Uuid::from_u64_pair(shape_id.0, shape_id.1)),
            params.into(),
        );
        Ok(())
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

// ============================================================================
// Import / export: whole-area JSON. `importAreas` is the one-shot fast path (avoids replaying a
// map room-by-room); `exportArea` serializes an area and is gated on `can_copy`.
// ============================================================================

/// `importAreas(areas)`: import full areas as new LOCAL areas (fresh ids), returning their ids.
/// Write-gated.
#[op2(async(lazy))]
#[serde]
async fn op_smudgy_mapper_import_areas(
    state: Rc<RefCell<OpState>>,
    #[serde] areas: Vec<AreaWithDetails>,
) -> Result<Vec<(u64, u64)>, MapperError> {
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, true)?;
        state.try_borrow::<Mapper>().cloned()
    };
    if let Some(mapper) = mapper {
        let ids = mapper
            .import_areas(areas)
            .await
            .map_err(|e| MapperError::FailedToCreate(e.to_string()))?;
        Ok(ids.into_iter().map(|id| id.0.as_u64_pair()).collect())
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
        .map_err(|e| MapperError::FailedToCreate(e.to_string()))
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
        let path = mapper
            .get_current_atlas()
            .get_path_between_rooms(&from_room_key, &to_room_key)
            .unwrap_or_default()
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
        let nearest = mapper
            .get_current_atlas()
            .find_nearest_room_matching_tags(&from_room_key, &required, &excluded)
            .map(|room_key| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0));
        Ok(nearest)
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
            .find_nearest_room_in_area(&from_room_key, &target_area_id)
            .map(|room_key| (room_key.area_id.0.as_u64_pair(), room_key.room_number.0));
        Ok(nearest)
    } else {
        Err(MapperError::MapperNotEnabled)
    }
}

