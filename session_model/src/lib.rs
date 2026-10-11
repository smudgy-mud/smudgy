//! Portable session presentation types shared by native and browser hosts.

#![allow(clippy::pedantic)]

use derive_more::{Add, Display, From, Into};
use serde::{Deserialize, Serialize};

pub mod automation;
pub mod connect;
pub mod inline_content;
pub mod input;
pub mod input_policy;
pub mod layout_template;
pub mod line_operation;
pub mod naming;
pub mod native_callback;
pub mod pane;
pub mod pane_name;
pub mod preferences;
pub mod profile_activation;
pub mod script_target;
pub mod send_failure;
pub mod styled_line;
pub mod system_row;
pub mod workspace;

pub use automation::*;
pub use input::*;
pub use line_operation::*;
pub use styled_line::*;
pub use system_row::*;

/// Maximum terminal-link tooltip delay accepted by either host.
pub const MAX_LINK_TOOLTIP_DELAY_MS: u64 = 60_000;

/// Native and browser terminal history limits. The cap is a preference, not
/// an allocation request until a session buffer is constructed.
pub const MIN_SCROLLBACK_LINES: usize = 100;
pub const MAX_SCROLLBACK_LINES: usize = 10_000_000;
pub const DEFAULT_SCROLLBACK_LINES: usize = 100_000;

#[derive(From, Into, Display, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Add)]
#[repr(transparent)]
pub struct SessionId(u32);

#[derive(Display, Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct HotkeyId(#[doc(hidden)] pub usize);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnsiColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Ansi { color: AnsiColor, bold: bool },
    Rgb { r: u8, g: u8, b: u8 },
    Echo,
    Output,
    Warn,
    DefaultForeground { bold: bool },
    DefaultBackground,
}

pub mod text_shader;
