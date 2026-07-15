//! Pure mutation helpers over [`AreaWithDetails`], shared by the backends
//! that own full area documents ([`super::local`] on disk,
//! [`super::ephemeral`] in memory). Each mirrors the server's semantics for
//! the corresponding write, so the backends stay thin wrappers that add
//! persistence and locking around one authoritative document edit.

use chrono::Utc;
use uuid::Uuid;

use crate::{
    AreaId, AreaWithDetails, CloudError, CloudResult, Exit, ExitArgs, ExitId, ExitStyle,
    ExitUpdates, Property, Room, RoomNumber, RoomUpdates, RoomWithDetails, mapper::RoomKey,
};

pub(super) fn apply_room_updates(room: &mut RoomWithDetails, updates: &RoomUpdates) {
    if let Some(title) = &updates.title {
        room.title.clone_from(title);
    }
    if let Some(description) = &updates.description {
        room.description.clone_from(description);
    }
    if let Some(level) = updates.level {
        room.level = level;
    }
    if let Some(x) = updates.x {
        room.x = x;
    }
    if let Some(y) = updates.y {
        room.y = y;
    }
    if let Some(color) = &updates.color {
        room.color.clone_from(color);
    }
    if let Some(is_secret) = updates.is_secret {
        room.is_secret = is_secret;
    }
    if let Some(external_id) = &updates.external_id {
        room.external_id.clone_from(external_id);
    }
}

pub(super) fn room_to_model(area_id: AreaId, room: &RoomWithDetails) -> Room {
    Room {
        area_id,
        room_number: room.room_number,
        title: room.title.clone(),
        description: room.description.clone(),
        level: room.level,
        x: room.x,
        y: room.y,
        color: room.color.clone(),
        created_at: Utc::now(),
        is_secret: room.is_secret,
        external_id: room.external_id.clone(),
    }
}

pub(super) fn apply_exit_updates(exit: &mut Exit, updates: ExitUpdates) {
    if let Some(from_direction) = updates.from_direction {
        exit.from_direction = from_direction;
    }
    // Mirror the server's COALESCE semantics: only `clear_to` nulls a
    // destination; an absent `to_*` leaves it unchanged.
    if updates.clear_to == Some(true) {
        exit.to_area_id = None;
        exit.to_room_number = None;
        exit.to_direction = None;
    } else {
        if let Some(to_area_id) = updates.to_area_id {
            exit.to_area_id = Some(to_area_id);
        }
        if let Some(to_room_number) = updates.to_room_number {
            exit.to_room_number = Some(to_room_number);
        }
        if let Some(to_direction) = updates.to_direction {
            exit.to_direction = Some(to_direction);
        }
    }
    if let Some(path) = updates.path {
        exit.path = path;
    }
    if let Some(is_hidden) = updates.is_hidden {
        exit.is_hidden = is_hidden;
    }
    if let Some(is_closed) = updates.is_closed {
        exit.is_closed = is_closed;
    }
    if let Some(is_locked) = updates.is_locked {
        exit.is_locked = is_locked;
    }
    if let Some(weight) = updates.weight {
        exit.weight = weight;
    }
    if let Some(command) = updates.command {
        exit.command = command;
    }
    if let Some(style) = updates.style {
        exit.style = style;
    }
    if let Some(color) = updates.color {
        exit.color = color;
    }
    if let Some(is_secret) = updates.is_secret {
        exit.is_secret = is_secret;
    }
}

/// Sets (or inserts) a property on a `Vec<Property>`, preserving secrecy on
/// overwrite.
pub(super) fn upsert_property(properties: &mut Vec<Property>, name: &str, value: &str) {
    if let Some(existing) = properties.iter_mut().find(|p| p.name == name) {
        existing.value = value.to_string();
    } else {
        properties.push(Property {
            name: name.to_string(),
            value: value.to_string(),
            is_secret: false,
        });
    }
}

/// Upserts a room: `PUT /areas/{a}/{room}` creates the room if absent.
pub(super) fn upsert_room(
    area: &mut AreaWithDetails,
    area_id: AreaId,
    number: RoomNumber,
    updates: &RoomUpdates,
) -> Room {
    if let Some(room) = area.rooms.iter_mut().find(|r| r.room_number == number) {
        apply_room_updates(room, updates);
    } else {
        let mut room = RoomWithDetails {
            room_number: number,
            title: String::new(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: Vec::new(),
            exits: Vec::new(),
            tags: std::collections::BTreeSet::default(),
            is_secret: false,
            external_id: None,
        };
        apply_room_updates(&mut room, updates);
        area.rooms.push(room);
    }
    let room = area
        .rooms
        .iter()
        .find(|r| r.room_number == number)
        .expect("room just upserted");
    room_to_model(area_id, room)
}

/// Deletes a room and mirrors the server's inbound-exit cascade within the
/// area (cross-area links are the live cache's concern).
pub(super) fn delete_room(area: &mut AreaWithDetails, area_id: AreaId, number: RoomNumber) {
    area.rooms.retain(|r| r.room_number != number);
    for room in &mut area.rooms {
        for exit in &mut room.exits {
            if exit.to_area_id == Some(area_id) && exit.to_room_number == Some(number) {
                exit.to_area_id = None;
                exit.to_room_number = None;
                exit.to_direction = None;
            }
        }
    }
}

/// Creates an exit on a room, minting its id.
pub(super) fn create_room_exit(
    area: &mut AreaWithDetails,
    room_key: &RoomKey,
    exit_data: ExitArgs,
) -> CloudResult<Exit> {
    let room = area
        .rooms
        .iter_mut()
        .find(|r| r.room_number == room_key.room_number)
        .ok_or_else(|| CloudError::RoomNotFound(room_key.clone()))?;
    let exit = Exit {
        id: ExitId(Uuid::new_v4()),
        from_direction: exit_data.from_direction,
        to_area_id: exit_data.to_area_id,
        to_room_number: exit_data.to_room_number,
        to_direction: exit_data.to_direction,
        path: exit_data.path.unwrap_or_default(),
        is_hidden: exit_data.is_hidden,
        is_closed: exit_data.is_closed,
        is_locked: exit_data.is_locked,
        weight: exit_data.weight,
        command: exit_data.command.unwrap_or_default(),
        style: exit_data.style.unwrap_or(ExitStyle::Normal),
        color: String::new(),
        to_unknown: false,
        to_area_token: None,
        is_secret: exit_data.is_secret.unwrap_or(false),
    };
    room.exits.push(exit.clone());
    Ok(exit)
}
