use crate::{AreaId, Exit, ExitDirection, ExitId, ExitStyle, RoomNumber, parse_css_color};

const DEFAULT_EXIT_COLOR: iced::Color = iced::Color::from_rgb8(128, 128, 128);

#[derive(Debug, Clone)]
pub struct ExitCache {
    pub id: ExitId,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
    pub weight: f32,
    pub command: Option<String>,
    pub style: ExitStyle,
    pub color: Option<String>,
    pub iced_color: iced::Color,
    /// Destination exists but is invisible to the viewer ("Unknown map").
    pub to_unknown: bool,
    /// Stable per-viewer token for the hidden destination; converging exits
    /// share one token.
    pub to_area_token: Option<String>,
    pub is_secret: bool,
}

impl From<Exit> for ExitCache {
    fn from(exit: Exit) -> Self {
        let iced_color = parse_css_color(&exit.color).unwrap_or(DEFAULT_EXIT_COLOR);

        Self {
            id: exit.id,
            from_direction: exit.from_direction,
            to_area_id: exit.to_area_id,
            to_room_number: exit.to_room_number,
            to_direction: exit.to_direction,
            path: (!exit.path.is_empty()).then_some(exit.path),
            is_hidden: exit.is_hidden,
            is_closed: exit.is_closed,
            is_locked: exit.is_locked,
            weight: exit.weight,
            command: (!exit.command.is_empty()).then_some(exit.command),
            style: exit.style,
            color: (!exit.color.is_empty()).then_some(exit.color),
            iced_color,
            to_unknown: exit.to_unknown,
            to_area_token: exit.to_area_token,
            is_secret: exit.is_secret,
        }
    }
}
