use std::collections::{BTreeSet, HashMap};

use ordered_float::OrderedFloat;

use crate::{
    ExitDirection, ExitId, RoomNumber, RoomUpdates, RoomWithDetails, parse_css_color,
    mapper::{RoomKey, exit_cache::ExitCache},
};

const DEFAULT_ROOM_COLOR: iced::Color = iced::Color::from_rgb8(128, 128, 128);

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct ExitBitfield(u16);

impl ExitBitfield {
    #[inline]
    const fn direction_to_bit(direction: ExitDirection) -> u16 {
        match direction {
            ExitDirection::North => 1,
            ExitDirection::East => 1 << 1,
            ExitDirection::South => 1 << 2,
            ExitDirection::West => 1 << 3,
            ExitDirection::Up => 1 << 4,
            ExitDirection::Down => 1 << 5,
            ExitDirection::Northeast => 1 << 6,
            ExitDirection::Southeast => 1 << 7,
            ExitDirection::Southwest => 1 << 8,
            ExitDirection::Northwest => 1 << 9,
            ExitDirection::In => 1 << 10,
            ExitDirection::Out => 1 << 11,
            ExitDirection::Special => 1 << 12,
            ExitDirection::Other => 1 << 13,
        }
    }
}

impl<'a, T> From<T> for ExitBitfield
where
    T: IntoIterator<Item = &'a ExitDirection>,
{
    fn from(directions: T) -> Self {
        ExitBitfield(directions.into_iter().fold(0, |map, direction| {
            ExitBitfield::direction_to_bit(*direction) | map
        }))
    }
}

/// A property value plus its owner-side secrecy flag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropertyEntry {
    pub value: String,
    pub is_secret: bool,
}

/// Room with all associated data
#[derive(Debug, Clone, Default)]

pub struct RoomCache {
    room_number: RoomNumber,
    title: String,
    description: String,
    title_and_description: String,
    level: i32,
    x: f32,
    y: f32,
    color: String,
    iced_color: iced::Color,
    properties: HashMap<String, PropertyEntry>,
    /// Case-insensitive tags, stored normalized to UPPERCASE. A `BTreeSet` for
    /// dedup + deterministic ordering (display + stable cache fingerprints).
    tags: BTreeSet<String>,
    exits: Vec<ExitCache>,
    visible_exit_bitfield: ExitBitfield,
    is_secret: bool,
    external_id: Option<String>,
}

impl RoomCache {
    #[must_use]
    pub fn new(room_number: RoomNumber) -> Self {
        Self {
            room_number,
            iced_color: iced::Color::from_rgb8(128, 128, 128),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn get_room_number(&self) -> RoomNumber {
        self.room_number
    }

    #[must_use]
    pub fn get_title_and_description(&self) -> &str {
        &self.title_and_description
    }

    #[must_use]
    pub fn get_title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn get_description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn get_level(&self) -> i32 {
        self.level
    }

    #[must_use]
    pub fn get_x(&self) -> f32 {
        self.x
    }

    #[must_use]
    pub fn get_y(&self) -> f32 {
        self.y
    }

    #[must_use]
    pub fn get_color(&self) -> &str {
        &self.color
    }

    #[inline]
    #[must_use]
    pub fn get_iced_color(&self) -> iced::Color {
        self.iced_color
    }

    #[must_use]
    pub fn get_exits(&self) -> &[ExitCache] {
        &self.exits
    }

    #[must_use]
    pub fn get_property(&self, name: &str) -> Option<&str> {
        self.properties.get(name).map(|p| p.value.as_str())
    }

    /// Iterates all properties in unspecified order; sort in the caller when
    /// stable ordering matters.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.value.as_str()))
    }

    /// Like [`Self::properties`] but including each property's secrecy flag.
    pub fn properties_with_secrecy(&self) -> impl Iterator<Item = (&str, &PropertyEntry)> {
        self.properties.iter().map(|(k, v)| (k.as_str(), v))
    }

    #[must_use]
    pub fn is_property_secret(&self, name: &str) -> bool {
        self.properties.get(name).is_some_and(|p| p.is_secret)
    }

    /// Case-insensitive tag membership test.
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.contains(&crate::mapper::normalize_tag(tag))
    }

    /// All tags, normalized to UPPERCASE, in sorted order.
    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(String::as_str)
    }

    #[must_use]
    pub fn get_tags(&self) -> &BTreeSet<String> {
        &self.tags
    }

    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.is_secret
    }

    /// The room's server-global external id (GMCP/MSDP room identity), if bound.
    #[must_use]
    pub fn get_external_id(&self) -> Option<&str> {
        self.external_id.as_deref()
    }

    #[must_use]
    pub fn with_secrecy(&self, is_secret: bool) -> Self {
        Self {
            is_secret,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn with_property_secrecy(&self, name: &str, is_secret: bool) -> Self {
        let mut new_properties = self.properties.clone();
        if let Some(entry) = new_properties.get_mut(name) {
            entry.is_secret = is_secret;
        }
        Self {
            properties: new_properties,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn get_visible_exit_bitfield(&self) -> ExitBitfield {
        self.visible_exit_bitfield
    }

    #[must_use]
    pub fn set_property(&self, name: String, value: String) -> Self {
        let mut new_properties = self.properties.clone();
        // Preserve secrecy on overwrite; new properties default to public.
        let is_secret = new_properties.get(&name).is_some_and(|p| p.is_secret);
        new_properties.insert(name, PropertyEntry { value, is_secret });

        Self {
            properties: new_properties,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn delete_property(&self, name: &str) -> Self {
        let mut new_properties = self.properties.clone();
        new_properties.remove(name);

        Self {
            properties: new_properties,
            ..self.clone()
        }
    }

    /// Returns a copy with `tag` (normalized to UPPERCASE) added. Idempotent.
    #[must_use]
    pub fn add_tag(&self, tag: &str) -> Self {
        let mut new_tags = self.tags.clone();
        new_tags.insert(crate::mapper::normalize_tag(tag));

        Self {
            tags: new_tags,
            ..self.clone()
        }
    }

    /// Returns a copy with `tag` (normalized to UPPERCASE) removed.
    #[must_use]
    pub fn remove_tag(&self, tag: &str) -> Self {
        let mut new_tags = self.tags.clone();
        new_tags.remove(&crate::mapper::normalize_tag(tag));

        Self {
            tags: new_tags,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn upsert_exit(&self, exit: ExitCache) -> Self {
        let mut new_exits = self.exits.clone();
        new_exits.retain(|e| e.id != exit.id);
        new_exits.push(exit);

        Self {
            visible_exit_bitfield: Self::visible_exit_bitfield_of(&new_exits),
            exits: new_exits,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn delete_exit(&self, exit_id: ExitId) -> Self {
        let mut new_exits = self.exits.clone();
        new_exits.retain(|e| e.id != exit_id);

        Self {
            visible_exit_bitfield: Self::visible_exit_bitfield_of(&new_exits),
            exits: new_exits,
            ..self.clone()
        }
    }

    /// Returns a copy with every exit whose destination is `target` reset to
    /// no destination, or `None` when no exit pointed there. Mirrors the
    /// server's inbound-exit cascade when `target` is deleted. Redacted
    /// (`to_unknown`) destinations never match — their `to_*` fields are
    /// already `None` — so they are left intact. The visible-exit bitfield is
    /// unchanged: clearing a destination touches neither `from_direction` nor
    /// `is_hidden`.
    #[must_use]
    pub fn null_exits_to(&self, target: &RoomKey) -> Option<Self> {
        let points_to = |exit: &ExitCache| {
            exit.to_area_id == Some(target.area_id)
                && exit.to_room_number == Some(target.room_number)
        };
        if !self.exits.iter().any(points_to) {
            return None;
        }

        let new_exits = self
            .exits
            .iter()
            .map(|exit| {
                if points_to(exit) {
                    ExitCache {
                        to_area_id: None,
                        to_room_number: None,
                        to_direction: None,
                        ..exit.clone()
                    }
                } else {
                    exit.clone()
                }
            })
            .collect();

        Some(Self {
            exits: new_exits,
            ..self.clone()
        })
    }

    fn visible_exit_bitfield_of(exits: &[ExitCache]) -> ExitBitfield {
        exits
            .iter()
            .filter(|e| !e.is_hidden)
            .map(|e| &e.from_direction)
            .into()
    }

    #[must_use]
    pub fn apply_updates(&self, updates: RoomUpdates) -> Self {
        let mut new_room = self.clone();

        if let Some(title) = &updates.title {
            new_room.title.clone_from(title);
        }
        if let Some(description) = &updates.description {
            new_room.description.clone_from(description);
        }

        if updates.title.is_some() || updates.description.is_some() {
            new_room.title_and_description =
                format!("{}\r\n{}", new_room.title, new_room.description);
        }

        if let Some(x) = updates.x {
            new_room.x = x;
        }
        if let Some(y) = updates.y {
            new_room.y = y;
        }
        if let Some(level) = updates.level {
            new_room.level = level;
        }
        if let Some(color) = updates.color {
            new_room.iced_color = parse_css_color(&color).unwrap_or(DEFAULT_ROOM_COLOR);
            new_room.color = color;
        }
        if let Some(is_secret) = updates.is_secret {
            new_room.is_secret = is_secret;
        }
        if let Some(external_id) = &updates.external_id {
            new_room.external_id.clone_from(external_id);
        }

        new_room
    }

    #[must_use]
    pub fn linked_room_keys(&self) -> impl IntoIterator<Item = RoomKey> {
        self.exits.iter().filter_map(|e| {
            e.to_area_id.and_then(|area_id| {
                e.to_room_number
                    .map(|room_number| RoomKey::new(area_id, room_number))
            })
        })
    }

    #[must_use]
    pub fn linked_room_keys_and_weights(
        &self,
    ) -> impl IntoIterator<Item = (RoomKey, OrderedFloat<f32>)> {
        self.exits.iter().filter_map(|e| {
            e.to_area_id.and_then(|area_id| {
                e.to_room_number.map(|room_number| {
                    (
                        RoomKey::new(area_id, room_number),
                        OrderedFloat::from(e.weight),
                    )
                })
            })
        })
    }
}

impl From<RoomWithDetails> for RoomCache {
    fn from(room: RoomWithDetails) -> Self {
        let iced_color = parse_css_color(&room.color).unwrap_or(DEFAULT_ROOM_COLOR);

        let exits: Vec<ExitCache> = room.exits.into_iter().map(std::convert::Into::into).collect();

        Self {
            room_number: room.room_number,
            title_and_description: format!("{}\r\n{}", room.title, room.description),
            title: room.title,
            description: room.description,
            level: room.level,
            x: room.x,
            y: room.y,
            color: room.color,
            properties: room
                .properties
                .into_iter()
                .map(|p| {
                    (
                        p.name,
                        PropertyEntry {
                            value: p.value,
                            is_secret: p.is_secret,
                        },
                    )
                })
                .collect(),
            tags: room.tags,
            visible_exit_bitfield: Self::visible_exit_bitfield_of(&exits),
            exits,
            iced_color,
            is_secret: room.is_secret,
            external_id: room.external_id,
        }
    }
}
