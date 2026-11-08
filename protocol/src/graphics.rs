//! Kitty terminal graphics protocol: the wire layer.
//!
//! This module owns everything about a graphics command that can be decided
//! from its bytes alone — control-data parsing, payload decoding, chunk
//! reassembly, the PNG header probe, and response framing. It deliberately
//! owns no image table, no pixel decoding and no I/O; those belong to the
//! host that drives it. Keeping this layer pure keeps it fuzzable and lets
//! every example in the specification become a unit test.
//!
//! A command arrives as an APC string, `ESC _ G <control> ; <payload> ESC \`,
//! which vtparse hands to [`crate::vt::VtProcessor`] already stripped of its
//! delimiters. The control data is comma-separated `key=value` pairs; the
//! payload is base64. Chunked payloads (`m=1` … `m=0`) are joined by
//! [`Reassembler`].
//!
//! What a command does once whole — the image table, placements, quota and
//! the decode hand-off — lives in [`state`], re-exported here.
//!
//! Specification: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.

pub mod placeholder;
mod state;

pub use state::{
    Animation, AnimationState, CELL_FIXED_SCALE, DEFAULT_FRAME_GAP_MS, DEFAULT_QUOTA_BYTES,
    FALLBACK_CELL, Fit, Frame, FrameId, GraphicsEnv, GraphicsState, ImageData, ImageRecord,
    ImageUid, MAX_FRAMES, MAX_IMAGE_BYTES, MAX_IMAGES, MAX_PLACEMENT_CELLS, MAX_PLACEMENT_ROWS,
    MAX_QUEUED_FRAME_OPS, PLACEMENT_DEPTH_LIMIT, Placement, PlacementGeometry, RelativeOffset,
    SourceRect, UiSlot, VirtualPlacement, anchor_outset, cell_fixed, cell_or_fallback,
    cell_whole_pixels, geometry,
};

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use std::collections::VecDeque;
use std::io::Read as _;

/// The byte that distinguishes a graphics APC from any other APC string.
const GRAPHICS_APC_LEAD: u8 = b'G';

/// The largest width or height an image may declare, as in kitty. Checked
/// before anything is decoded: on `s`/`v` by [`ControlData::validate`], on a
/// PNG's header once its data is whole.
pub const MAX_IMAGE_DIMENSION: u32 = 10_000;

/// How the host treats graphics commands for one connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsMode {
    /// Commands are parsed and discarded; nothing is drawn and nothing is
    /// written back. Terminal queries (DA1, pixel-size reports) stay silent.
    Off,
    /// Images are shown, but the connection never answers: no `OK`/error
    /// replies and no terminal-query responses. For a server that emits
    /// graphics but would mistake our replies for typed commands.
    DisplayOnly,
    /// The full protocol, including replies and the terminal queries clients
    /// use to detect support.
    #[default]
    Full,
}

impl GraphicsMode {
    /// Whether this connection writes protocol replies and query responses.
    #[must_use]
    pub fn replies(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Whether graphics commands have any visible effect.
    #[must_use]
    pub fn displays(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// The `a=` key: what a command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `a=t`: store pixel data under an id (the default when `a` is absent).
    Transmit,
    /// `a=T`: store and place at the cursor.
    TransmitAndDisplay,
    /// `a=q`: validate the data and reply, storing nothing.
    Query,
    /// `a=p`: place previously stored data at the cursor.
    Put,
    /// `a=d`: delete placements and optionally data.
    Delete,
    /// `a=f`: transmit one animation frame.
    Frame,
    /// `a=a`: control animation playback.
    Animate,
    /// `a=c`: compose one frame onto another.
    Compose,
}

impl Action {
    fn from_letter(letter: u8) -> Option<Self> {
        Some(match letter {
            b't' => Self::Transmit,
            b'T' => Self::TransmitAndDisplay,
            b'q' => Self::Query,
            b'p' => Self::Put,
            b'd' => Self::Delete,
            b'f' => Self::Frame,
            b'a' => Self::Animate,
            b'c' => Self::Compose,
            _ => return None,
        })
    }

    /// The letter the specification spells this action with.
    #[must_use]
    pub fn letter(self) -> u8 {
        match self {
            Self::Transmit => b't',
            Self::TransmitAndDisplay => b'T',
            Self::Query => b'q',
            Self::Put => b'p',
            Self::Delete => b'd',
            Self::Frame => b'f',
            Self::Animate => b'a',
            Self::Compose => b'c',
        }
    }

    /// Whether this action carries pixel data in its payload.
    #[must_use]
    pub fn carries_pixels(self) -> bool {
        matches!(
            self,
            Self::Transmit | Self::TransmitAndDisplay | Self::Query | Self::Frame
        )
    }
}

/// The `f=` key: the payload's pixel encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// `f=24`: packed 8-bit RGB, `3 * width * height` bytes.
    Rgb,
    /// `f=32`: packed 8-bit RGBA, `4 * width * height` bytes. The default,
    /// which `f=0` names too (as in kitty).
    #[default]
    Rgba,
    /// `f=100`: a complete PNG file; the size comes from the file.
    Png,
    /// Any other number — `f=8` and `f=16` (palette indices, coloured from
    /// the terminal's colour scheme) among them. Parsed, never decoded:
    /// the specification has clients probe for `f=8` support with a query,
    /// and kitty answers the probe, like every number it does not know,
    /// with `EINVAL:Unknown image format: N`, so the key must parse for
    /// the client to get that answer rather than silence.
    Unsupported(u32),
}

impl Format {
    /// Bytes per pixel for the packed formats we decode; `None` for PNG and
    /// for the indexed formats, which are refused before any decode.
    #[must_use]
    pub fn bytes_per_pixel(self) -> Option<u32> {
        match self {
            Self::Rgb => Some(3),
            Self::Rgba => Some(4),
            Self::Png | Self::Unsupported(_) => None,
        }
    }

    /// The `f=` value of a format this terminal does not decode, for the
    /// `Unknown image format` reply.
    #[must_use]
    fn unsupported_number(self) -> Option<u32> {
        match self {
            Self::Unsupported(number) => Some(number),
            Self::Rgb | Self::Rgba | Self::Png => None,
        }
    }

    /// The exact decompressed payload length a packed format of these
    /// dimensions must have; `None` for PNG and the indexed formats.
    #[must_use]
    pub fn expected_len(self, width: u32, height: u32) -> Option<u64> {
        let bpp = u64::from(self.bytes_per_pixel()?);
        Some(bpp * u64::from(width) * u64::from(height))
    }
}

/// The `t=` key: where the pixel data lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Medium {
    /// `t=d`: in the payload itself. The only medium a remote peer may use.
    #[default]
    Direct,
    /// `t=f`: the payload names a regular file on the host.
    File,
    /// `t=t`: like `File`, but the host deletes it after reading.
    TempFile,
    /// `t=s`: the payload names a POSIX shared-memory object.
    SharedMemory,
}

/// The `o=` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None,
    /// `o=z`: RFC 1950 zlib, applied before base64.
    Zlib,
}

/// The `q=` key: which replies the client wants suppressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quiet {
    #[default]
    Verbose,
    /// `q=1`: no `OK` replies.
    SuppressOk,
    /// `q=2`: no replies at all.
    SuppressAll,
}

impl Quiet {
    /// Whether a reply with this outcome should be written.
    #[must_use]
    pub fn permits(self, ok: bool) -> bool {
        match self {
            Self::Verbose => true,
            Self::SuppressOk => !ok,
            Self::SuppressAll => false,
        }
    }
}

/// The parsed control data of one command. Every key keeps its specification
/// default when absent. Field names follow the key's meaning for display
/// commands; the animation actions overload several keys, and the doc comment
/// on each notes the alternate reading so the host dispatches on
/// [`Self::action`] before interpreting them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlData {
    /// `a`. Absent means transmit.
    pub action: Action,
    /// `e`: the action a feature query asks about. Meaningful only with
    /// `a=q` (see [`Self::is_feature_query`]); on any other action the key
    /// is ignored, as kitty ignores it.
    pub feature_query: Option<Action>,
    /// `q`.
    pub quiet: Quiet,
    /// `f`.
    pub format: Format,
    /// `t`.
    pub medium: Medium,
    /// `o`.
    pub compression: Compression,
    /// `s`: pixel width of packed data. For `a=a`, the playback state
    /// (1 stop, 2 run to the last frame, 3 loop).
    pub width: u32,
    /// `v`: pixel height of packed data. For `a=a`, the loop count.
    pub height: u32,
    /// `S`: bytes to read from a file or shared memory (0 = to the end).
    pub data_size: u32,
    /// `O`: byte offset to start reading at.
    pub data_offset: u32,
    /// `i`: image id, 0 = none.
    pub image_id: u32,
    /// `I`: client-chosen image number, 0 = none.
    pub image_number: u32,
    /// `p`: placement id, 0 = none.
    pub placement_id: u32,
    /// `m`: whether more chunks follow.
    pub more: bool,
    /// `N`: the usage-hint bitmask; bit 0 marks a transient image.
    pub usage_hint: u32,
    /// `d`: the delete target letter, validated by [`Self::delete_target`].
    pub delete: Option<u8>,
    /// `x`: source-rect left for display; frame-rect left for `a=f`;
    /// destination-rect left for `a=c`.
    pub x: u32,
    /// `y`: like `x`, vertically.
    pub y: u32,
    /// `w`: source-rect width for display (0 = whole image); rect width
    /// for `a=c`.
    pub w: u32,
    /// `h`: like `w`, vertically.
    pub h: u32,
    /// `X`: pixel offset inside the first cell for display. For `a=f`, 1
    /// selects overwrite composition. For `a=c`, the source-rect left.
    pub x_offset: u32,
    /// `Y`: pixel offset inside the first cell for display. For `a=f`, a
    /// 32-bit RGBA background colour. For `a=c`, the source-rect top.
    pub y_offset: u32,
    /// `c`: columns to scale to for display. Base frame for `a=f`,
    /// destination frame for `a=c`, frame to make current for `a=a`.
    pub columns: u32,
    /// `r`: rows to scale to for display. Frame being edited for `a=f`,
    /// source frame for `a=c`, frame being adjusted for `a=a`.
    pub rows: u32,
    /// `C`: 1 leaves the cursor in place after display. For `a=c`, 1 selects
    /// overwrite composition.
    pub cursor_movement: u32,
    /// `U`: 1 creates a virtual placement for Unicode placeholders.
    pub unicode_placeholder: bool,
    /// `z`: z-index for display; frame gap in milliseconds for `a=f`/`a=a`.
    pub z: i32,
    /// `P`: parent image id for a relative placement.
    pub parent_image: u32,
    /// `Q`: parent placement id for a relative placement.
    pub parent_placement: u32,
    /// `H`: cell offset from the parent, positive = right.
    pub offset_columns: i32,
    /// `V`: cell offset from the parent, positive = down.
    pub offset_rows: i32,
}

impl Default for ControlData {
    fn default() -> Self {
        Self {
            action: Action::Transmit,
            feature_query: None,
            quiet: Quiet::Verbose,
            format: Format::Rgba,
            medium: Medium::Direct,
            compression: Compression::None,
            width: 0,
            height: 0,
            data_size: 0,
            data_offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            more: false,
            usage_hint: 0,
            delete: None,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            x_offset: 0,
            y_offset: 0,
            columns: 0,
            rows: 0,
            cursor_movement: 0,
            unicode_placeholder: false,
            z: 0,
            parent_image: 0,
            parent_placement: 0,
            offset_columns: 0,
            offset_rows: 0,
        }
    }
}

/// What `a=d` deletes. Lowercase variants keep the pixel data; the `free`
/// flag carries the uppercase form, which also frees data no longer placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteTarget {
    /// `a`/`A`: every placement visible on screen.
    AllVisible,
    /// `i`/`I`: by image id (`i`), optionally one placement (`p`).
    ByImageId,
    /// `n`/`N`: by image number (`I`), optionally one placement (`p`).
    ByImageNumber,
    /// `c`/`C`: placements under the cursor.
    AtCursor,
    /// `f`/`F`: animation frames of image `i`.
    Frames,
    /// `p`/`P`: placements covering cell (`x`, `y`), 1-based.
    AtCell,
    /// `q`/`Q`: placements covering cell (`x`, `y`) with z-index `z`.
    AtCellWithZ,
    /// `r`/`R`: images whose id lies in `x..=y`.
    IdRange,
    /// `x`/`X`: placements intersecting column `x`.
    Column,
    /// `y`/`Y`: placements intersecting row `y`.
    Row,
    /// `z`/`Z`: placements with z-index `z`.
    ZIndex,
}

impl DeleteTarget {
    fn from_letter(letter: u8) -> Option<(Self, bool)> {
        let free = letter.is_ascii_uppercase();
        let target = match letter.to_ascii_lowercase() {
            b'a' => Self::AllVisible,
            b'i' => Self::ByImageId,
            b'n' => Self::ByImageNumber,
            b'c' => Self::AtCursor,
            b'f' => Self::Frames,
            b'p' => Self::AtCell,
            b'q' => Self::AtCellWithZ,
            b'r' => Self::IdRange,
            b'x' => Self::Column,
            b'y' => Self::Row,
            b'z' => Self::ZIndex,
            _ => return None,
        };
        Some((target, free))
    }
}

impl ControlData {
    /// The delete target and whether pixel data is freed too. `a=d` with no
    /// `d` key means `d=a`.
    ///
    /// # Errors
    /// `EINVAL` for a letter the specification does not define.
    pub fn delete_target(&self) -> Result<(DeleteTarget, bool), GraphicsError> {
        match self.delete {
            None => Ok((DeleteTarget::AllVisible, false)),
            Some(letter) => DeleteTarget::from_letter(letter).ok_or_else(|| {
                GraphicsError::invalid(format!("unknown delete target {:?}", char::from(letter)))
            }),
        }
    }

    /// Whether this is a feature query: `a=q` with `e=`, which transmits
    /// nothing and asks which keys of the named action are supported. `e`
    /// on any other action means nothing — kitty reads it only for queries
    /// — so a client that sends it there gets the command it asked for,
    /// not an error.
    #[must_use]
    pub fn is_feature_query(&self) -> bool {
        self.action == Action::Query && self.feature_query.is_some()
    }

    /// The semantic checks every command must pass before it runs, kept
    /// apart from parsing so the parser stays purely syntactic. Both `i`
    /// and `I` is an error; the specification reserves `I` for clients
    /// that let the terminal pick ids. Pixel data must come in a format
    /// this terminal decodes, packed pixel data may not declare a side
    /// over [`MAX_IMAGE_DIMENSION`], and packed pixel data must carry its
    /// dimensions whatever medium holds it — on its first chunk when it is
    /// chunked, as kitty checks them, since the chunks after it may carry
    /// nothing but `m` and `q`. A feature query transmits nothing and
    /// skips the pixel checks; `e` on any other action is ignored (see
    /// [`Self::is_feature_query`]).
    ///
    /// # Errors
    /// `EINVAL`, with the reason in the detail.
    pub fn validate(&self) -> Result<(), GraphicsError> {
        if self.image_id != 0 && self.image_number != 0 {
            return Err(GraphicsError::invalid(
                "image id and image number cannot both be specified",
            ));
        }
        if self.action.carries_pixels() && !self.is_feature_query() {
            if let Some(number) = self.format.unsupported_number() {
                return Err(GraphicsError::invalid(format!(
                    "Unknown image format: {number}"
                )));
            }
            if self.format != Format::Png && (self.width == 0 || self.height == 0) {
                return Err(GraphicsError::invalid(
                    "packed pixel data needs non-zero s and v",
                ));
            }
            if self.format != Format::Png
                && (self.width > MAX_IMAGE_DIMENSION || self.height > MAX_IMAGE_DIMENSION)
            {
                return Err(GraphicsError::invalid(format!(
                    "image dimensions {}x{} exceed {MAX_IMAGE_DIMENSION}",
                    self.width, self.height
                )));
            }
        }
        Ok(())
    }
}

/// A protocol error with the code the reply carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphicsError {
    pub code: ErrorCode,
    /// Printable ASCII, as the reply format requires.
    pub detail: String,
}

/// Error codes replies carry: the ones the specification names, and the
/// two kitty answers short and long pixel data with, which clients written
/// against it may look for. `EINVAL` covers everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// Malformed or unsupported control data or payload.
    Invalid,
    /// The referenced image, placement or frame does not exist.
    NotFound,
    /// A file or shared-memory read failed. Always the same message, so a
    /// peer cannot probe the filesystem through our replies.
    BadFile,
    /// Packed pixel data shorter than its size demands (kitty's
    /// `ENODATA:Insufficient image data: X < Y`).
    NoData,
    /// Packed pixel data longer than its size demands (kitty's
    /// `EFBIG:Too much data`).
    TooBig,
    /// Storage quota exhausted.
    NoSpace,
    /// A relative placement would form a cycle.
    Cycle,
    /// A relative placement names a missing parent.
    NoParent,
    /// A relative placement chain exceeds the supported depth.
    TooDeep,
}

impl ErrorCode {
    /// The code as it appears on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "EINVAL",
            Self::NotFound => "ENOENT",
            Self::BadFile => "EBADF",
            Self::NoData => "ENODATA",
            Self::TooBig => "EFBIG",
            Self::NoSpace => "ENOSPC",
            Self::Cycle => "ECYCLE",
            Self::NoParent => "ENOPARENT",
            Self::TooDeep => "ETOODEEP",
        }
    }
}

impl GraphicsError {
    #[must_use]
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, detail)
    }

    /// The one reply every file and shared-memory failure produces.
    #[must_use]
    pub fn bad_file() -> Self {
        Self::new(ErrorCode::BadFile, "Failed to read image file")
    }
}

impl std::fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.code.as_str(), self.detail)
    }
}

/// Split an APC string into graphics control data and its raw (still
/// base64) payload. `None` when the APC is not a graphics command at all,
/// which callers must leave to whatever else handles APC strings.
#[must_use]
pub fn parse_apc(data: &[u8]) -> Option<Result<(ControlData, &[u8]), GraphicsError>> {
    let rest = data.strip_prefix(&[GRAPHICS_APC_LEAD])?;
    let (control, payload) = match memchr::memchr(b';', rest) {
        Some(at) => (&rest[..at], &rest[at + 1..]),
        None => (rest, &rest[rest.len()..]),
    };
    Some(parse_control(control).map(|control| (control, payload)))
}

/// Parse comma-separated `key=value` control data. Purely syntactic: what
/// the keys mean together is [`ControlData::validate`]'s business.
///
/// Lenient where the specification is silent: empty items are skipped, an
/// unknown key is ignored (forward compatibility), and a repeated key keeps
/// its last value. A value that does not fit its key is an error.
///
/// # Errors
/// `EINVAL` for an item that is not `key=value`, a key longer than one
/// byte, or a value outside its key's domain.
pub fn parse_control(control: &[u8]) -> Result<ControlData, GraphicsError> {
    let mut out = ControlData::default();
    for item in control.split(|&b| b == b',') {
        if item.is_empty() {
            continue;
        }
        let Some(eq) = memchr::memchr(b'=', item) else {
            return Err(GraphicsError::invalid(format!(
                "malformed control item {:?}",
                String::from_utf8_lossy(item)
            )));
        };
        let (key, value) = (&item[..eq], &item[eq + 1..]);
        let [key] = key else {
            return Err(GraphicsError::invalid(format!(
                "malformed control key {:?}",
                String::from_utf8_lossy(key)
            )));
        };
        apply_key(&mut out, *key, value)?;
    }
    Ok(out)
}

/// Apply one `key=value` item; an unknown key is ignored.
fn apply_key(out: &mut ControlData, key: u8, value: &[u8]) -> Result<(), GraphicsError> {
    match key {
        b'a' => {
            out.action = Action::from_letter(letter(key, value)?)
                .ok_or_else(|| invalid_value(key, value))?;
        }
        b'e' => {
            out.feature_query = Some(
                Action::from_letter(letter(key, value)?)
                    .ok_or_else(|| invalid_value(key, value))?,
            );
        }
        b'q' => {
            out.quiet = match unsigned(key, value)? {
                0 => Quiet::Verbose,
                1 => Quiet::SuppressOk,
                2 => Quiet::SuppressAll,
                _ => return Err(invalid_value(key, value)),
            };
        }
        b'f' => {
            out.format = match unsigned(key, value)? {
                0 | 32 => Format::Rgba,
                24 => Format::Rgb,
                100 => Format::Png,
                number => Format::Unsupported(number),
            };
        }
        b't' => {
            out.medium = match letter(key, value)? {
                b'd' => Medium::Direct,
                b'f' => Medium::File,
                b't' => Medium::TempFile,
                b's' => Medium::SharedMemory,
                _ => return Err(invalid_value(key, value)),
            };
        }
        b'o' => {
            out.compression = match letter(key, value)? {
                b'z' => Compression::Zlib,
                _ => return Err(invalid_value(key, value)),
            };
        }
        b's' => out.width = unsigned(key, value)?,
        b'v' => out.height = unsigned(key, value)?,
        b'S' => out.data_size = unsigned(key, value)?,
        b'O' => out.data_offset = unsigned(key, value)?,
        b'i' => out.image_id = unsigned(key, value)?,
        b'I' => out.image_number = unsigned(key, value)?,
        b'p' => out.placement_id = unsigned(key, value)?,
        b'm' => {
            out.more = match unsigned(key, value)? {
                0 => false,
                1 => true,
                _ => return Err(invalid_value(key, value)),
            };
        }
        b'N' => out.usage_hint = unsigned(key, value)?,
        b'd' => out.delete = Some(letter(key, value)?),
        b'x' => out.x = unsigned(key, value)?,
        b'y' => out.y = unsigned(key, value)?,
        b'w' => out.w = unsigned(key, value)?,
        b'h' => out.h = unsigned(key, value)?,
        b'X' => out.x_offset = unsigned(key, value)?,
        b'Y' => out.y_offset = unsigned(key, value)?,
        b'c' => out.columns = unsigned(key, value)?,
        b'r' => out.rows = unsigned(key, value)?,
        b'C' => out.cursor_movement = unsigned(key, value)?,
        b'U' => {
            out.unicode_placeholder = match unsigned(key, value)? {
                0 => false,
                1 => true,
                _ => return Err(invalid_value(key, value)),
            };
        }
        b'z' => out.z = signed(key, value)?,
        b'P' => out.parent_image = unsigned(key, value)?,
        b'Q' => out.parent_placement = unsigned(key, value)?,
        b'H' => out.offset_columns = signed(key, value)?,
        b'V' => out.offset_rows = signed(key, value)?,
        _ => log::debug!(
            "ignoring unknown graphics control key {:?}",
            char::from(key)
        ),
    }
    Ok(())
}

fn invalid_value(key: u8, value: &[u8]) -> GraphicsError {
    GraphicsError::invalid(format!(
        "invalid value {:?} for key {:?}",
        String::from_utf8_lossy(value),
        char::from(key)
    ))
}

fn letter(key: u8, value: &[u8]) -> Result<u8, GraphicsError> {
    match value {
        [letter] if letter.is_ascii_alphanumeric() => Ok(*letter),
        _ => Err(invalid_value(key, value)),
    }
}

fn unsigned(key: u8, value: &[u8]) -> Result<u32, GraphicsError> {
    std::str::from_utf8(value)
        .ok()
        .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| invalid_value(key, value))
}

fn signed(key: u8, value: &[u8]) -> Result<i32, GraphicsError> {
    std::str::from_utf8(value)
        .ok()
        .filter(|text| {
            let digits = text.strip_prefix('-').unwrap_or(text);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| invalid_value(key, value))
}

/// RFC 4648 base64 with or without padding; every client in the wild pads,
/// but nothing is gained by rejecting the ones that do not.
const PAYLOAD_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Decode a chunk's base64 payload.
///
/// # Errors
/// `EINVAL` when the payload is not base64.
pub fn decode_payload(payload: &[u8]) -> Result<Vec<u8>, GraphicsError> {
    PAYLOAD_BASE64
        .decode(payload)
        .map_err(|err| GraphicsError::invalid(format!("payload is not valid base64: {err}")))
}

/// Encode bytes the way a client would, for tests and for echoing names.
#[must_use]
pub fn encode_payload(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The outcome of feeding one chunk to a [`Reassembler`].
#[derive(Debug, PartialEq, Eq)]
pub enum Fed {
    /// More chunks are expected.
    Pending,
    /// The command is whole: the first chunk's control data (with the last
    /// non-zero `q` of any chunk, and `m` cleared) and the joined,
    /// base64-decoded payload.
    Complete(ControlData, Vec<u8>),
}

/// Joins chunked payloads. One upload may be in flight per connection; the
/// specification forbids interleaving graphics commands with it, and a
/// delete must abandon it. While one is in flight, every direct-medium
/// pixel command continues it, as in kitty, whatever its other keys say.
#[derive(Debug, Default)]
pub struct Reassembler {
    pending: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    control: ControlData,
    data: Vec<u8>,
}

impl Reassembler {
    /// Whether an upload is waiting for more chunks.
    #[must_use]
    pub fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    /// The control data of the upload in flight, if any.
    #[must_use]
    pub fn pending_control(&self) -> Option<&ControlData> {
        self.pending.as_ref().map(|p| &p.control)
    }

    /// Drop the upload in flight.
    pub fn abort(&mut self) -> Option<ControlData> {
        self.pending.take().map(|p| p.control)
    }

    /// Whether `control` continues the upload in flight rather than starting
    /// something new: kitty takes any direct-medium transmit, query or frame
    /// command for the next chunk of the load in progress and ignores its
    /// other keys. A feature query transmits nothing and continues nothing;
    /// local media start a new load.
    #[must_use]
    pub fn continues(&self, control: &ControlData) -> bool {
        self.pending.is_some()
            && control.medium == Medium::Direct
            && !control.is_feature_query()
            && control.action.carries_pixels()
    }

    /// Feed one chunk. `cap` bounds the joined payload; exceeding it fails
    /// with `ENOSPC` and drops the upload. A chunk that does not continue the
    /// upload in flight replaces it (the caller decides whether that deserves
    /// a reply for the abandoned one).
    ///
    /// # Errors
    /// `EINVAL` for a payload that is not base64; `ENOSPC` when the joined
    /// payload would exceed `cap`.
    pub fn feed(
        &mut self,
        control: ControlData,
        payload: &[u8],
        cap: usize,
    ) -> Result<Fed, GraphicsError> {
        let decoded = decode_payload(payload)?;
        if self.continues(&control)
            && let Some(mut pending) = self.pending.take()
        {
            if pending.data.len().saturating_add(decoded.len()) > cap {
                return Err(GraphicsError::new(
                    ErrorCode::NoSpace,
                    "image data exceeds the storage quota",
                ));
            }
            pending.data.extend_from_slice(&decoded);
            // The reply is decided when the upload completes, and any chunk
            // may set its quiet level: the last non-zero `q` wins, which is
            // why continuation chunks are allowed the key at all.
            if control.quiet != Quiet::Verbose {
                pending.control.quiet = control.quiet;
            }
            if control.more {
                self.pending = Some(pending);
                return Ok(Fed::Pending);
            }
            pending.control.more = false;
            return Ok(Fed::Complete(pending.control, pending.data));
        }
        self.pending = None;
        if decoded.len() > cap {
            return Err(GraphicsError::new(
                ErrorCode::NoSpace,
                "image data exceeds the storage quota",
            ));
        }
        if control.more {
            self.pending = Some(Pending {
                control,
                data: decoded,
            });
            return Ok(Fed::Pending);
        }
        Ok(Fed::Complete(control, decoded))
    }
}

/// The dimensions a PNG stream declares in its IHDR chunk, which always
/// starts at byte 16. Enough to size a placement before the image decodes.
#[must_use]
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Inflate at most `max_out` bytes of a zlib stream — enough to read a PNG
/// header out of a compressed payload without inflating the whole image.
///
/// # Errors
/// `EINVAL` when the stream is not valid zlib.
pub fn inflate_prefix(data: &[u8], max_out: usize) -> Result<Vec<u8>, GraphicsError> {
    let mut out = Vec::with_capacity(max_out);
    let mut decoder = flate2::read::ZlibDecoder::new(data).take(max_out as u64);
    decoder
        .read_to_end(&mut out)
        .map_err(|err| GraphicsError::invalid(format!("zlib data is corrupt: {err}")))?;
    Ok(out)
}

/// Inflate a whole zlib payload, refusing to grow past `max_out`.
///
/// # Errors
/// `EINVAL` when the stream is not valid zlib; `ENOSPC` when the inflated
/// data would exceed `max_out`.
pub fn inflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, GraphicsError> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(data).take(max_out as u64 + 1);
    decoder
        .read_to_end(&mut out)
        .map_err(|err| GraphicsError::invalid(format!("zlib data is corrupt: {err}")))?;
    if out.len() > max_out {
        return Err(GraphicsError::new(
            ErrorCode::NoSpace,
            "decompressed image data exceeds the storage quota",
        ));
    }
    Ok(out)
}

/// One reply, framed as `ESC _ G <keys> ; <status> ESC \`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// `i=`; a reply without an id is only sent for commands that had none.
    pub image_id: Option<u32>,
    /// `I=`, echoed (with the id the terminal assigned) when the client used
    /// an image number.
    pub image_number: Option<u32>,
    /// `p=`, echoed for placement replies.
    pub placement_id: Option<u32>,
    /// Further `key=value` pairs, as a feature query echoes the keys it
    /// supports.
    pub extra: Vec<(u8, String)>,
    pub status: Result<(), GraphicsError>,
}

impl Response {
    /// An `OK` for `image_id`.
    #[must_use]
    pub fn ok(image_id: u32) -> Self {
        Self {
            image_id: Some(image_id),
            image_number: None,
            placement_id: None,
            extra: Vec::new(),
            status: Ok(()),
        }
    }

    /// An error reply for `image_id`.
    #[must_use]
    pub fn error(image_id: u32, error: GraphicsError) -> Self {
        Self {
            image_id: Some(image_id),
            image_number: None,
            placement_id: None,
            extra: Vec::new(),
            status: Err(error),
        }
    }

    /// Whether the reply says `OK`.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status.is_ok()
    }

    /// Append the framed reply to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b_G");
        let mut first = true;
        let mut key = |out: &mut Vec<u8>, key: u8, value: &str| {
            if !first {
                out.push(b',');
            }
            first = false;
            out.push(key);
            out.push(b'=');
            out.extend_from_slice(value.as_bytes());
        };
        if let Some(id) = self.image_id {
            key(out, b'i', &id.to_string());
        }
        if let Some(number) = self.image_number {
            key(out, b'I', &number.to_string());
        }
        if let Some(placement) = self.placement_id {
            key(out, b'p', &placement.to_string());
        }
        for (k, v) in &self.extra {
            key(out, *k, v);
        }
        out.push(b';');
        match &self.status {
            Ok(()) => out.extend_from_slice(b"OK"),
            Err(err) => {
                out.extend_from_slice(err.code.as_str().as_bytes());
                out.push(b':');
                out.extend(
                    err.detail
                        .bytes()
                        .map(|b| if (0x20..0x7f).contains(&b) { b } else { b'?' }),
                );
            }
        }
        out.extend_from_slice(b"\x1b\\");
    }

    /// The framed reply as bytes.
    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }
}

/// Pixel data ready to decode: one transmit or frame command's joined
/// payload, still compressed when it came compressed, and what its control
/// data said about it. Everything that can be checked without inflating or
/// decoding has been: the size is within [`MAX_IMAGE_DIMENSION`] and the
/// quota, uncompressed packed data has exactly the length its size demands,
/// and a frame's rectangle lies inside its image.
///
/// The result is the decoded `width` x `height` pixels — or, with
/// [`Self::compose`], the canvas they were composed onto, which is
/// [`Compose::canvas`] in size. An `a=c` carries no data at all: its
/// rectangle is read from a frame the image already has
/// ([`Compose::source`]), so the host decodes nothing and composes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeRequest {
    pub format: Format,
    /// `o=z`: the decoder inflates `data` first, off the socket thread. For
    /// packed formats the inflated length must be exactly
    /// [`Format::expected_len`]; a stream that inflates longer is `EINVAL`,
    /// so inflate with that as the cap.
    pub compression: Compression,
    /// The pixel size the command declared (packed formats) or the PNG
    /// header announced. The decoded pixels must have exactly this size.
    pub width: u32,
    pub height: u32,
    /// Packed pixels or a whole PNG file, base64-decoded and joined; empty
    /// for an `a=c`, whose rectangle is [`Compose::source`].
    pub data: Vec<u8>,
    /// For an animation frame: where the decoded pixels land. `None` for an
    /// image transmit or a query, whose result is the decoded pixels
    /// themselves.
    pub compose: Option<Compose>,
}

/// Straight RGBA8, `width * height * 4` bytes — what a placement draws.
/// Shared, not copied: the session record keeps one clone and the image
/// handle is minted from another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: bytes::Bytes,
}

/// How decoded pixels land on their canvas: `X` on `a=f`, `C` on `a=c`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposeMode {
    /// The default: straight-alpha "over", the decoded pixel over the
    /// canvas pixel (see [`blend_over`]).
    #[default]
    AlphaBlend,
    /// `X=1` / `C=1`: the decoded pixel replaces the canvas pixel, alpha
    /// included.
    Overwrite,
}

/// The composition step of an animation frame decode: a rectangle (the
/// request's `width` x `height`) lands at (`x`, `y`) on a canvas the size
/// of the image, which starts as `base`'s pixels or, without a base, as
/// `background` everywhere. The result is the whole canvas. The rectangle
/// is known to lie inside the canvas and a base to be canvas-sized;
/// [`Self::apply`] is a reference implementation a host may use as it is.
///
/// For `a=f` the rectangle is the decoded data (`x`, `y`, `s`, `v`), the
/// base is the frame named by `c` (or the frame being edited, `r`) and the
/// background is `Y`. For `a=c` the request carries no data: the rectangle
/// is [`Self::source`], `w` x `h` of frame `r` from (`X`, `Y`), which the
/// host reads straight from that frame's buffer, and the base is the
/// destination frame `c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compose {
    /// The frame to compose onto, exactly `canvas` in size; `None` fills
    /// the canvas with `background` first.
    pub base: Option<DecodedPixels>,
    /// For `a=c`: the frame the rectangle is read from, in place of the
    /// request's (empty) data. `None` for `a=f`, whose rectangle is the
    /// decoded data.
    pub source: Option<ComposeSource>,
    /// The `Y` key: 32-bit RGBA, `0xRRGGBBAA` ([`Self::background_rgba`]).
    /// Zero — transparent black — when none was given.
    pub background: u32,
    /// Where the rectangle's top-left pixel lands on the canvas.
    pub x: u32,
    pub y: u32,
    pub mode: ComposeMode,
    /// The canvas size: the image's `(width, height)`.
    pub canvas: (u32, u32),
}

/// The rectangle an `a=c` composes: `width` x `height` of `pixels` (a frame
/// of the image, shared with its record — a reference-count bump, never a
/// copy) from (`x`, `y`). The host composes it straight from that buffer;
/// nothing is cut out on the socket thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeSource {
    /// The source frame (`r`), the image's size.
    pub pixels: DecodedPixels,
    /// The rectangle's top-left on the source frame: `X`, `Y`.
    pub x: u32,
    pub y: u32,
    /// The rectangle's size, the same as the request's `width` x `height`:
    /// `w`, `h`.
    pub width: u32,
    pub height: u32,
}

/// Where [`Compose::apply`] reads the rows of its rectangle: a buffer, the
/// stride between its rows and the offset of the first.
struct RectRows<'a> {
    bytes: &'a [u8],
    stride: usize,
    start: usize,
}

impl Compose {
    /// The background as RGBA bytes.
    #[must_use]
    pub fn background_rgba(&self) -> [u8; 4] {
        self.background.to_be_bytes()
    }

    /// Compose the rectangle — `decoded`, straight RGBA8 of `width` x
    /// `height`, the request's size; or, with [`Self::source`], read from
    /// the source frame, when `decoded` must be empty — onto the canvas
    /// and return the canvas, `canvas.0 * canvas.1 * 4` bytes. A rectangle
    /// with no pixels (zero `width` or `height`) leaves the canvas as it
    /// starts: the base, or the background.
    ///
    /// # Errors
    /// `EINVAL` when `decoded` is not `width * height * 4` bytes (or not
    /// empty with a source), the rectangle leaves the canvas, the source is
    /// not the size it says or the rectangle leaves it, the base is not
    /// canvas-sized, or the canvas does not fit in memory.
    pub fn apply(&self, decoded: &[u8], width: u32, height: u32) -> Result<Vec<u8>, GraphicsError> {
        let (canvas_width, canvas_height) = self.canvas;
        let canvas_len = index(u64::from(canvas_width) * u64::from(canvas_height) * 4)?;
        let row_len = index(u64::from(width) * 4)?;
        let rows = index(u64::from(height))?;
        let rect = match &self.source {
            None => {
                if decoded.len() != row_len.checked_mul(rows).ok_or_else(too_large)? {
                    return Err(GraphicsError::invalid(format!(
                        "decoded frame is {} bytes, expected {width}x{height}x4",
                        decoded.len()
                    )));
                }
                RectRows {
                    bytes: decoded,
                    stride: row_len,
                    start: 0,
                }
            }
            Some(source) => {
                if !decoded.is_empty() {
                    return Err(GraphicsError::invalid(
                        "a compose from a frame carries no data",
                    ));
                }
                source.rows(width, height)?
            }
        };
        if u64::from(self.x) + u64::from(width) > u64::from(canvas_width)
            || u64::from(self.y) + u64::from(height) > u64::from(canvas_height)
        {
            return Err(GraphicsError::invalid(
                "frame rectangle is outside the image",
            ));
        }
        let mut canvas = match &self.base {
            Some(base) => {
                if (base.width, base.height) != self.canvas || base.rgba.len() != canvas_len {
                    return Err(GraphicsError::invalid(format!(
                        "base frame is {}x{} in {} bytes, expected {canvas_width}x{canvas_height}",
                        base.width,
                        base.height,
                        base.rgba.len()
                    )));
                }
                base.rgba.to_vec()
            }
            None => self.background_rgba().repeat(canvas_len / 4),
        };
        if row_len == 0 || rows == 0 {
            return Ok(canvas);
        }
        let stride = index(u64::from(canvas_width) * 4)?;
        let left = index(u64::from(self.x) * 4)?;
        let top = index(u64::from(self.y))?;
        for row in 0..rows {
            let from = rect.start + row * rect.stride;
            let source = &rect.bytes[from..from + row_len];
            let at = (top + row) * stride + left;
            let target = &mut canvas[at..at + row_len];
            match self.mode {
                ComposeMode::Overwrite => target.copy_from_slice(source),
                ComposeMode::AlphaBlend => {
                    for (under, over) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                        blend_over(under, over);
                    }
                }
            }
        }
        Ok(canvas)
    }
}

impl ComposeSource {
    /// The rows of the rectangle in the source frame's buffer, once the
    /// source is checked: it is the size it says, the rectangle is the
    /// request's `width` x `height` and lies inside it.
    fn rows(&self, width: u32, height: u32) -> Result<RectRows<'_>, GraphicsError> {
        if (self.width, self.height) != (width, height) {
            return Err(GraphicsError::invalid(format!(
                "source rectangle is {}x{}, expected {width}x{height}",
                self.width, self.height
            )));
        }
        let (source_width, source_height) = (self.pixels.width, self.pixels.height);
        let source_len = index(u64::from(source_width) * u64::from(source_height) * 4)?;
        if self.pixels.rgba.len() != source_len {
            return Err(GraphicsError::invalid(format!(
                "source frame is {source_width}x{source_height} in {} bytes",
                self.pixels.rgba.len()
            )));
        }
        if u64::from(self.x) + u64::from(width) > u64::from(source_width)
            || u64::from(self.y) + u64::from(height) > u64::from(source_height)
        {
            return Err(GraphicsError::invalid(
                "source rectangle is outside the source frame",
            ));
        }
        let stride = index(u64::from(source_width) * 4)?;
        Ok(RectRows {
            bytes: &self.pixels.rgba,
            stride,
            start: index(u64::from(self.y))? * stride + index(u64::from(self.x) * 4)?,
        })
    }
}

/// A buffer length or offset as `usize`, or the `EINVAL` of a canvas this
/// machine cannot hold.
fn index(value: u64) -> Result<usize, GraphicsError> {
    usize::try_from(value).map_err(|_| too_large())
}

fn too_large() -> GraphicsError {
    GraphicsError::invalid("canvas too large")
}

/// Straight-alpha "over": `over` onto `under`, both RGBA8 with straight
/// (not premultiplied) alpha, as kitty blends. The result's alpha is
/// `a_o = a_s + a_d (1 - a_s)` and each colour `(c_s a_s + c_d a_d (1 -
/// a_s)) / a_o`, rounded; a fully transparent result is transparent black.
pub fn blend_over(under: &mut [u8], over: &[u8]) {
    let over_alpha = u32::from(over[3]);
    if over_alpha == 255 {
        under[..4].copy_from_slice(&over[..4]);
        return;
    }
    if over_alpha == 0 {
        return;
    }
    let under_alpha = u32::from(under[3]);
    let inverse = 255 - over_alpha;
    // The result's alpha scaled by 255 squared.
    let alpha = over_alpha * 255 + under_alpha * inverse;
    if alpha == 0 {
        under[..4].fill(0);
        return;
    }
    for channel in 0..3 {
        let colour = u32::from(over[channel]) * over_alpha * 255
            + u32::from(under[channel]) * under_alpha * inverse;
        under[channel] = u8::try_from((colour + alpha / 2) / alpha).unwrap_or(u8::MAX);
    }
    under[3] = u8::try_from((alpha + 127) / 255).unwrap_or(u8::MAX);
}

pub type DecodeResult = Result<DecodedPixels, GraphicsError>;

/// One decode the host was asked for. The reply slot parked for it answers
/// when the host reports back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DecodeTicket(u64);

/// What the protocol needs from the process around it: pixel decoding off
/// the socket thread, and the local media a `t=f`, `t=t` or `t=s` transmit
/// names. Every method is synchronous. The host owns its threads and hands
/// completions back through whatever drives the processor.
pub trait GraphicsHost: std::fmt::Debug + Send + 'static {
    /// Start decoding `request`. The host decodes elsewhere and, when done,
    /// delivers `(ticket, result)` to the thread driving the processor,
    /// which calls `VtProcessor::graphics_decode_complete`. A result that
    /// can no longer be delivered — the connection is gone — is dropped.
    ///
    /// A request with [`DecodeRequest::compose`] is an animation frame: the
    /// decoded rectangle is composed onto its canvas ([`Compose::apply`])
    /// and the result is the canvas. One with a [`Compose::source`] is an
    /// `a=c`, which carries no data: the host decodes nothing and composes
    /// the rectangle of the source frame (`Compose::apply` with empty
    /// data). The state machine sends at most one such request per image
    /// at a time, so a frame's base and source are always frames that have
    /// landed.
    fn decode(&mut self, ticket: DecodeTicket, request: DecodeRequest);

    /// Read `size` bytes (zero: to the end) from `offset` of the file or
    /// shared-memory object `name`. Only a loopback peer may be allowed
    /// this, and every failure is the one `EBADF` reply.
    ///
    /// # Errors
    /// `EBADF`, always with the same detail.
    fn read_media(
        &mut self,
        medium: Medium,
        name: &[u8],
        offset: u32,
        size: u32,
    ) -> Result<Vec<u8>, GraphicsError>;
}

/// Replies in the order their commands arrived. A command whose answer waits
/// on a decode parks a slot, and everything queued behind it — later
/// graphics replies, DA1, size reports — waits with it, so the peer never
/// sees an answer overtake an earlier command's.
#[derive(Debug, Default)]
pub struct ReplyQueue {
    slots: VecDeque<ReplySlot>,
    next_ticket: u64,
}

#[derive(Debug)]
enum ReplySlot {
    /// Bytes ready to write. Adjacent ready bytes share one slot.
    Ready(Vec<u8>),
    /// A reply whose status is still being decided.
    Parked {
        ticket: u64,
        reply: Response,
        quiet: Quiet,
    },
}

impl ReplyQueue {
    /// Queue bytes that are ready now.
    pub fn push(&mut self, bytes: &[u8]) {
        match self.slots.back_mut() {
            Some(ReplySlot::Ready(buffer)) => buffer.extend_from_slice(bytes),
            _ => self.slots.push_back(ReplySlot::Ready(bytes.to_vec())),
        }
    }

    /// Queue a reply, unless `quiet` suppresses it.
    pub fn push_response(&mut self, response: &Response, quiet: Quiet) {
        if quiet.permits(response.is_ok()) {
            let mut bytes = Vec::new();
            response.write_to(&mut bytes);
            self.push(&bytes);
        }
    }

    /// Reserve the next slot for `reply`, whose status [`Self::resolve`]
    /// settles later.
    pub fn park(&mut self, reply: Response, quiet: Quiet) -> DecodeTicket {
        self.next_ticket += 1;
        self.slots.push_back(ReplySlot::Parked {
            ticket: self.next_ticket,
            reply,
            quiet,
        });
        DecodeTicket(self.next_ticket)
    }

    /// Settle a parked reply. `false` for a ticket this queue does not hold.
    /// A reply its quiet level suppresses leaves the queue altogether, so
    /// [`Self::has_ready`] never announces bytes a drain will not find.
    pub fn resolve(&mut self, ticket: DecodeTicket, status: Result<(), GraphicsError>) -> bool {
        self.resolve_with(ticket, status, Vec::new())
    }

    /// [`Self::resolve`] with keys to echo after the ids — `r=` for the
    /// frame a load created or edited, known only once it ran — each
    /// replacing the value the reply already carries for its key (the `r`
    /// the command gave), or appended.
    ///
    /// The ticket is found by a scan: the queue holds the replies of the
    /// decodes in flight and the frame commands queued behind them, a few
    /// dozen at most, and a ticket-to-index map would have to be shifted
    /// on every drain of the front, which runs far more often than this.
    pub fn resolve_with(
        &mut self,
        ticket: DecodeTicket,
        status: Result<(), GraphicsError>,
        extra: Vec<(u8, String)>,
    ) -> bool {
        let Some(index) = self.slots.iter().position(
            |slot| matches!(slot, ReplySlot::Parked { ticket: parked, .. } if *parked == ticket.0),
        ) else {
            return false;
        };
        let ReplySlot::Parked { reply, quiet, .. } = &mut self.slots[index] else {
            unreachable!("found by the Parked pattern");
        };
        reply.status = status;
        for (key, value) in extra {
            match reply.extra.iter_mut().find(|(held, _)| *held == key) {
                Some(slot) => slot.1 = value,
                None => reply.extra.push((key, value)),
            }
        }
        let mut bytes = Vec::new();
        if quiet.permits(reply.is_ok()) {
            reply.write_to(&mut bytes);
        }
        if bytes.is_empty() {
            self.slots.remove(index);
        } else {
            self.slots[index] = ReplySlot::Ready(bytes);
        }
        true
    }

    /// Whether bytes can be written now: the head of the queue is ready.
    #[inline]
    #[must_use]
    pub fn has_ready(&self) -> bool {
        matches!(self.slots.front(), Some(ReplySlot::Ready(_)))
    }

    /// Whether a parked reply is still waiting somewhere in the queue.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, ReplySlot::Parked { .. }))
    }

    /// Forget every reply still owed: bytes not yet written are dropped,
    /// and a parked reply resolves to nothing when its decode lands, so
    /// whatever queues behind it afterwards still flows. For a connection
    /// whose replies were switched off with answers outstanding.
    pub fn silence(&mut self) {
        self.slots
            .retain(|slot| matches!(slot, ReplySlot::Parked { .. }));
        for slot in &mut self.slots {
            if let ReplySlot::Parked { quiet, .. } = slot {
                *quiet = Quiet::SuppressAll;
            }
        }
    }

    /// Move every ready reply at the head of the queue into `out`, stopping
    /// at the first one still parked.
    pub fn drain_into(&mut self, out: &mut Vec<u8>) {
        while let Some(ReplySlot::Ready(_)) = self.slots.front() {
            let Some(ReplySlot::Ready(mut bytes)) = self.slots.pop_front() else {
                unreachable!("checked by the front pattern");
            };
            out.append(&mut bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parked_replies_hold_everything_queued_behind_them() {
        let mut queue = ReplyQueue::default();
        queue.push(b"A");
        let first = queue.park(Response::ok(1), Quiet::Verbose);
        queue.push(b"B");
        let second = queue.park(Response::ok(2), Quiet::SuppressOk);
        queue.push(b"C");
        let mut out = Vec::new();
        queue.drain_into(&mut out);
        assert_eq!(out, b"A");
        assert!(!queue.has_ready());
        assert!(queue.is_waiting());
        // The later decode finishes first; nothing moves until the earlier one does.
        assert!(queue.resolve(second, Ok(())));
        assert!(!queue.has_ready());
        assert!(queue.resolve(first, Err(GraphicsError::invalid("bad"))));
        assert!(queue.has_ready());
        queue.drain_into(&mut out);
        assert_eq!(
            out, b"A\x1b_Gi=1;EINVAL:bad\x1b\\BC",
            "the second's OK was suppressed by q=1, so B and C run together"
        );
        assert!(!queue.is_waiting());
        assert!(!queue.resolve(first, Ok(())), "a settled ticket is gone");
    }

    #[test]
    fn silencing_drops_ready_bytes_and_mutes_parked_replies_in_place() {
        let mut queue = ReplyQueue::default();
        queue.push(b"A");
        let parked = queue.park(Response::ok(1), Quiet::Verbose);
        queue.push(b"B");
        queue.silence();
        assert!(!queue.has_ready(), "nothing owed is written any more");
        assert!(queue.is_waiting(), "but the parked slot keeps its place");
        // What queues after the switch flows once the decode lands.
        queue.push(b"C");
        assert!(queue.resolve(parked, Err(GraphicsError::invalid("late"))));
        let mut out = Vec::new();
        queue.drain_into(&mut out);
        assert_eq!(out, b"C");
        assert!(!queue.is_waiting());
    }

    fn parse(control: &str) -> ControlData {
        parse_control(control.as_bytes()).unwrap_or_else(|err| panic!("{control}: {err}"))
    }

    fn apc(text: &str) -> (ControlData, Vec<u8>) {
        let (control, payload) = parse_apc(text.as_bytes())
            .expect("a graphics APC")
            .unwrap_or_else(|err| panic!("{text}: {err}"));
        (control, payload.to_vec())
    }

    #[test]
    fn non_graphics_apc_is_not_ours() {
        assert!(parse_apc(b"tmux;something").is_none());
        assert!(parse_apc(b"").is_none());
        assert!(parse_apc(b"g=1").is_none());
    }

    #[test]
    fn minimal_transmit_examples_from_the_spec() {
        // `f=24,s=10,v=20;<payload>`: RGB, 600 bytes.
        let (control, payload) = apc("Gf=24,s=10,v=20;AAAA");
        assert_eq!(control.action, Action::Transmit);
        assert_eq!(control.format, Format::Rgb);
        assert_eq!((control.width, control.height), (10, 20));
        assert_eq!(Format::Rgb.expected_len(10, 20), Some(600));
        assert_eq!(payload, b"AAAA");

        // `f=100;<payload>`: PNG needs no size.
        let (control, _) = apc("Gf=100;iVBORw0KGgo=");
        assert_eq!(control.format, Format::Png);

        // `f=24,s=10,v=20,o=z;<payload>`: compressed.
        let (control, _) = apc("Gf=24,s=10,v=20,o=z;eJxjYAAAAAIAAQ==");
        assert_eq!(control.compression, Compression::Zlib);

        // A payload-less command.
        let (control, payload) = apc("Ga=p,i=10");
        assert_eq!(control.action, Action::Put);
        assert_eq!(control.image_id, 10);
        assert!(payload.is_empty());
    }

    #[test]
    fn transmission_media_from_the_spec() {
        let (control, _) = apc("Gf=100,t=f;L3RtcC9pbWcucG5n");
        assert_eq!(control.medium, Medium::File);
        let (control, _) = apc("Gs=10,v=2,t=s,o=z;L2Zvbw==");
        assert_eq!(control.medium, Medium::SharedMemory);
        let (control, _) = apc("Gs=10,v=2,t=s,S=80,O=10;L2Zvbw==");
        assert_eq!((control.data_size, control.data_offset), (80, 10));
        let (control, _) = apc("Gf=100,t=t;L3RtcC90dHktZ3JhcGhpY3MtcHJvdG9jb2w=");
        assert_eq!(control.medium, Medium::TempFile);
    }

    #[test]
    fn query_and_feature_query_examples_from_the_spec() {
        let (control, payload) = apc("Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA");
        assert_eq!(control.action, Action::Query);
        assert_eq!(control.image_id, 31);
        assert_eq!(decode_payload(&payload).unwrap(), [0, 0, 0]);

        let control = parse("a=q,i=31,e=T,U=1,C=1");
        assert_eq!(control.feature_query, Some(Action::TransmitAndDisplay));
        assert!(control.unicode_placeholder);
        assert_eq!(control.cursor_movement, 1);

        let control = parse("a=q,i=31,e=f,r=1,X=1");
        assert_eq!(control.feature_query, Some(Action::Frame));
        assert_eq!((control.rows, control.x_offset), (1, 1));
    }

    #[test]
    fn display_keys_from_the_spec() {
        let control = parse("a=T,f=100,c=20,r=5,x=4,y=8,w=100,h=50,X=3,Y=2,z=-1,C=1,q=2");
        assert_eq!(control.action, Action::TransmitAndDisplay);
        assert_eq!((control.columns, control.rows), (20, 5));
        assert_eq!(
            (control.x, control.y, control.w, control.h),
            (4, 8, 100, 50)
        );
        assert_eq!((control.x_offset, control.y_offset), (3, 2));
        assert_eq!(control.z, -1);
        assert_eq!(control.cursor_movement, 1);
        assert_eq!(control.quiet, Quiet::SuppressAll);

        let control = parse("a=p,U=1,i=42,c=3,r=2");
        assert!(control.unicode_placeholder);

        let control = parse("a=p,i=5,p=2,P=1,Q=7,H=-3,V=4");
        assert_eq!((control.parent_image, control.parent_placement), (1, 7));
        assert_eq!((control.offset_columns, control.offset_rows), (-3, 4));
    }

    #[test]
    fn delete_examples_from_the_spec() {
        let control = parse("a=d");
        assert_eq!(
            control.delete_target().unwrap(),
            (DeleteTarget::AllVisible, false)
        );
        let control = parse("a=d,d=i,i=10");
        assert_eq!(
            control.delete_target().unwrap(),
            (DeleteTarget::ByImageId, false)
        );
        let control = parse("a=d,d=i,i=10,p=7");
        assert_eq!(control.placement_id, 7);
        let control = parse("a=d,d=Z,z=-1");
        assert_eq!(
            control.delete_target().unwrap(),
            (DeleteTarget::ZIndex, true)
        );
        assert_eq!(control.z, -1);
        let control = parse("a=d,d=p,x=3,y=4");
        assert_eq!(
            control.delete_target().unwrap(),
            (DeleteTarget::AtCell, false)
        );
        let control = parse("a=d,d=R,x=1,y=99");
        assert_eq!(
            control.delete_target().unwrap(),
            (DeleteTarget::IdRange, true)
        );
        assert!(parse("a=d,d=k").delete_target().is_err());
    }

    #[test]
    fn animation_examples_from_the_spec() {
        let control = parse("a=f,i=3,x=10,y=5,s=100,v=200,z=40,c=1,X=1,Y=4278190335");
        assert_eq!(control.action, Action::Frame);
        assert_eq!((control.x, control.y), (10, 5));
        assert_eq!((control.width, control.height), (100, 200));
        assert_eq!(control.z, 40);
        assert_eq!(control.y_offset, 4_278_190_335);

        let control = parse("a=a,i=3,c=7");
        assert_eq!(control.action, Action::Animate);
        assert_eq!(control.columns, 7);
        let control = parse("a=a,i=7,r=3,z=48");
        assert_eq!((control.rows, control.z), (3, 48));
        let control = parse("a=a,i=7,s=3,v=1");
        assert_eq!((control.width, control.height), (3, 1));

        let control = parse("a=c,i=1,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3");
        assert_eq!(control.action, Action::Compose);
        assert_eq!((control.rows, control.columns), (7, 9));
        assert_eq!((control.w, control.h), (23, 27));
        assert_eq!((control.x_offset, control.y_offset), (4, 8));
        assert_eq!((control.x, control.y), (1, 3));
    }

    #[test]
    fn lenient_about_what_the_spec_leaves_undefined() {
        // Leading, trailing and doubled commas; unknown keys; repeated keys.
        let control = parse(",a=T,,f=100,k=9,i=1,i=2,");
        assert_eq!(control.action, Action::TransmitAndDisplay);
        assert_eq!(control.image_id, 2);
        assert_eq!(
            control,
            parse("a=T,f=100,i=2"),
            "the unknown key left no trace"
        );
    }

    #[test]
    fn strict_about_values() {
        assert!(parse_control(b"a=x").is_err());
        assert!(parse_control(b"f=x").is_err());
        assert!(parse_control(b"t=q").is_err());
        assert!(parse_control(b"o=g").is_err());
        assert!(parse_control(b"q=3").is_err());
        assert!(parse_control(b"m=2").is_err());
        assert!(parse_control(b"U=2").is_err());
        assert!(parse_control(b"i=-1").is_err());
        assert!(parse_control(b"i=4294967296").is_err());
        assert!(parse_control(b"z=abc").is_err());
        assert!(parse_control(b"a").is_err());
        assert!(parse_control(b"ab=1").is_err());
        let control = parse("z=-2147483648");
        assert_eq!(control.z, i32::MIN);
        let control = parse("i=4294967295");
        assert_eq!(control.image_id, u32::MAX);
    }

    #[test]
    fn validation_is_a_separate_step() {
        assert!(parse("a=T,i=1,I=2").validate().is_err(), "i and I together");
        assert!(
            parse("a=T,f=24").validate().is_err(),
            "packed data without a size"
        );
        assert!(
            parse("a=T,f=24,m=1").validate().is_err(),
            "a chunked first piece carries the size too"
        );
        assert!(parse("a=T,f=100").validate().is_ok(), "PNG sizes itself");
        assert!(
            parse("a=q,i=1,e=T").validate().is_ok(),
            "feature queries carry no pixels"
        );
        assert!(parse("a=p,i=1").validate().is_ok());
        assert!(parse("f=24,s=10,v=20").validate().is_ok());
    }

    #[test]
    fn packed_dimensions_are_bounded_before_any_data_arrives() {
        assert!(parse("a=T,f=24,s=10000,v=10000").validate().is_ok());
        assert_eq!(
            parse("a=T,f=24,s=10001,v=1").validate().unwrap_err().detail,
            "image dimensions 10001x1 exceed 10000"
        );
        assert!(
            parse("a=T,f=32,s=1,v=10001,m=1").validate().is_err(),
            "the first chunk of a chunked upload is checked too"
        );
        assert!(
            parse("a=T,f=100,s=20000,v=20000").validate().is_ok(),
            "a PNG's size comes from its header, checked once whole"
        );
    }

    #[test]
    fn packed_data_needs_its_size_from_every_medium() {
        for medium in ["d", "f", "t", "s"] {
            assert_eq!(
                parse(&format!("a=T,f=24,t={medium}"))
                    .validate()
                    .unwrap_err()
                    .detail,
                "packed pixel data needs non-zero s and v",
                "t={medium}"
            );
            assert!(
                parse(&format!("a=T,f=24,t={medium},s=2,v=2"))
                    .validate()
                    .is_ok(),
                "t={medium}"
            );
            assert!(
                parse(&format!("a=T,f=100,t={medium}")).validate().is_ok(),
                "t={medium}"
            );
        }
    }

    #[test]
    fn unsupported_formats_parse_and_are_refused_as_unknown() {
        // The specification has clients probe for `f=8` support with a
        // query; kitty answers the probe, so the key must parse — and so
        // must any other number, which kitty answers the same way.
        let control = parse("a=q,i=1,f=8,s=1,v=1");
        assert_eq!(control.format, Format::Unsupported(8));
        assert_eq!(
            control.validate().unwrap_err(),
            GraphicsError::invalid("Unknown image format: 8")
        );
        assert_eq!(
            parse("a=T,i=1,f=16,s=1,v=1").validate().unwrap_err(),
            GraphicsError::invalid("Unknown image format: 16")
        );
        assert_eq!(
            parse("a=T,i=1,f=7,s=1,v=1").validate().unwrap_err(),
            GraphicsError::invalid("Unknown image format: 7"),
            "an undefined number is answered, not dropped as a parse error"
        );
        assert_eq!(
            parse("a=T,i=1,f=8,m=1").validate().unwrap_err().detail,
            "Unknown image format: 8",
            "refused on the first chunk, before any data is joined"
        );
        assert_eq!(Format::Unsupported(8).bytes_per_pixel(), None);
        assert_eq!(Format::Unsupported(16).expected_len(2, 2), None);
        assert!(
            parse("a=p,i=1,f=8").validate().is_ok(),
            "a format key on a command without pixels is ignored"
        );
        assert!(
            parse("a=q,i=1,e=T,f=8").validate().is_ok(),
            "a feature query transmits no pixels"
        );
        assert_eq!(
            parse("f=0,s=1,v=1").format,
            Format::Rgba,
            "f=0 names the default, as in kitty"
        );
        assert!(
            parse_control(b"f=x").is_err(),
            "a value that is not a number stays a parse error"
        );
    }

    #[test]
    fn the_e_key_is_ignored_outside_queries() {
        // kitty reads `e` only under `a=q`: a transmit carrying it is the
        // transmit it would be without, not an error.
        let transmit = parse("a=T,i=1,e=T,f=24,s=1,v=1");
        assert!(!transmit.is_feature_query());
        assert!(transmit.validate().is_ok());
        let put = parse("a=p,i=1,e=p");
        assert!(!put.is_feature_query());
        assert!(put.validate().is_ok());
        let query = parse("a=q,i=1,e=p");
        assert!(query.is_feature_query());
        assert!(query.validate().is_ok());
    }

    #[test]
    fn a_chunked_packed_upload_needs_its_dimensions_on_the_first_chunk() {
        // kitty's timing: the first chunk carries every key, and is refused
        // before any data is joined.
        assert_eq!(
            parse("a=T,i=1,f=24,m=1").validate().unwrap_err(),
            GraphicsError::invalid("packed pixel data needs non-zero s and v")
        );
        assert!(parse("a=T,i=1,f=24,s=1,v=1,m=1").validate().is_ok());
        assert!(
            parse("a=T,i=1,f=100,m=1").validate().is_ok(),
            "a PNG sizes itself"
        );
    }

    #[test]
    fn payload_decoding_tolerates_missing_padding() {
        assert_eq!(decode_payload(b"AAAA").unwrap(), [0, 0, 0]);
        assert_eq!(decode_payload(b"AA==").unwrap(), [0]);
        assert_eq!(decode_payload(b"AA").unwrap(), [0]);
        assert!(decode_payload(b"A").is_err());
        assert!(decode_payload(b"!!!!").is_err());
        assert_eq!(encode_payload(&[0, 0, 0]), "AAAA");
    }

    #[test]
    fn chunked_upload_example_from_the_spec() {
        // `s=100,v=30,m=1;...`, `m=1;...`, `m=0;...`
        let mut joined = Reassembler::default();
        let first = parse("a=T,f=24,s=100,v=30,m=1");
        assert_eq!(joined.feed(first, b"AAAA", 1 << 20).unwrap(), Fed::Pending);
        assert!(joined.in_flight());
        assert_eq!(
            joined.feed(parse("m=1"), b"AQID", 1 << 20).unwrap(),
            Fed::Pending
        );
        let Fed::Complete(control, data) = joined.feed(parse("m=0,q=2"), b"BAUG", 1 << 20).unwrap()
        else {
            panic!("final chunk completes the upload");
        };
        assert_eq!(control.action, Action::TransmitAndDisplay);
        assert_eq!((control.width, control.height), (100, 30));
        assert_eq!(
            control.quiet,
            Quiet::SuppressAll,
            "a later chunk's non-zero q sets the reply's quiet level"
        );
        assert_eq!(data, [0, 0, 0, 1, 2, 3, 4, 5, 6]);
        assert!(!joined.in_flight());
    }

    #[test]
    fn the_last_non_zero_quiet_level_of_any_chunk_wins() {
        let mut joined = Reassembler::default();
        joined
            .feed(parse("a=T,f=24,s=1,v=1,q=2,m=1"), b"AA", 64)
            .unwrap();
        assert_eq!(
            joined.pending_control().map(|c| c.quiet),
            Some(Quiet::SuppressAll)
        );
        // A middle chunk may lower it...
        joined.feed(parse("m=1,q=1"), b"AA", 64).unwrap();
        assert_eq!(
            joined.pending_control().map(|c| c.quiet),
            Some(Quiet::SuppressOk)
        );
        // ...and a chunk without `q` (or with `q=0`) keeps the last level.
        joined.feed(parse("m=1,q=0"), b"AA", 64).unwrap();
        let Fed::Complete(control, data) = joined.feed(parse("m=0"), b"AA", 64).unwrap() else {
            panic!("final chunk completes the upload");
        };
        assert_eq!(control.quiet, Quiet::SuppressOk);
        assert_eq!(data, [0, 0, 0, 0]);
    }

    #[test]
    fn any_direct_pixel_command_continues_the_upload_in_flight() {
        // kitty takes the next direct transmit, query or frame command for
        // the next chunk and ignores its other keys.
        let mut joined = Reassembler::default();
        assert!(
            !joined.continues(&parse("m=0")),
            "nothing to continue while idle"
        );
        joined
            .feed(parse("a=f,i=1,s=1,v=1,m=1"), b"AAAA", 64)
            .unwrap();
        assert!(joined.continues(&parse("a=f,m=0")));
        assert!(joined.continues(&parse("a=T,f=100,i=9,m=0")));
        assert!(joined.continues(&parse("a=q,i=3")));
        assert!(
            !joined.continues(&parse("a=q,i=3,e=T")),
            "a feature query transmits nothing"
        );
        assert!(
            !joined.continues(&parse("a=T,t=f,i=3")),
            "local media start a new load"
        );
        assert!(
            !joined.continues(&parse("a=d,d=i,i=1")),
            "a delete never continues an upload"
        );
        assert!(!joined.continues(&parse("a=p,i=1")));
    }

    #[test]
    fn a_continuation_s_own_keys_are_ignored() {
        let mut joined = Reassembler::default();
        joined
            .feed(parse("a=T,f=24,s=2,v=1,i=4,m=1"), b"AAAA", 64)
            .unwrap();
        let Fed::Complete(control, data) = joined
            .feed(parse("a=t,f=32,s=9,v=9,i=7"), b"AQID", 64)
            .unwrap()
        else {
            panic!("a chunk without m=1 completes the upload");
        };
        assert_eq!(control.action, Action::TransmitAndDisplay);
        assert_eq!((control.image_id, control.width), (4, 2));
        assert!(!control.more, "the joined command is whole");
        assert_eq!(data, [0, 0, 0, 1, 2, 3]);
        assert!(!joined.in_flight());
        assert_eq!(joined.abort(), None);
    }

    #[test]
    fn a_non_pixel_command_fed_mid_upload_replaces_it() {
        let mut joined = Reassembler::default();
        joined
            .feed(parse("a=T,f=24,s=1,v=1,m=1"), b"AAAA", 64)
            .unwrap();
        let Fed::Complete(control, data) = joined.feed(parse("a=p,i=1"), b"", 64).unwrap() else {
            panic!("an unchunked command completes at once");
        };
        assert_eq!(control.action, Action::Put);
        assert!(
            data.is_empty(),
            "nothing from the abandoned upload leaks in"
        );
        assert!(!joined.in_flight());
    }

    #[test]
    fn the_quota_bounds_a_chunked_upload() {
        let mut joined = Reassembler::default();
        joined.feed(parse("f=24,s=4,v=1,m=1"), b"AAAA", 5).unwrap();
        let err = joined.feed(parse("m=1"), b"AAAA", 5).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoSpace);
        assert!(!joined.in_flight(), "an overflow drops the upload");
        let err = joined
            .feed(parse("f=24,s=4,v=1"), b"AAAAAAAA", 5)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoSpace);
    }

    #[test]
    fn png_header_probe() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((640, 480)));
        assert_eq!(png_dimensions(&png[..20]), None);
        assert_eq!(png_dimensions(b"GIF89a"), None);
        let mut zero = png.clone();
        zero[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(png_dimensions(&zero), None);

        // Compressed PNG: inflate only the header.
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &png).unwrap();
        std::io::Write::write_all(&mut encoder, &[0u8; 4096]).unwrap();
        let compressed = encoder.finish().unwrap();
        let prefix = inflate_prefix(&compressed, 24).unwrap();
        assert_eq!(prefix.len(), 24);
        assert_eq!(png_dimensions(&prefix), Some((640, 480)));
        assert_eq!(
            inflate(&compressed, 1 << 16).unwrap().len(),
            png.len() + 4096
        );
        assert_eq!(
            inflate(&compressed, 100).unwrap_err().code,
            ErrorCode::NoSpace
        );
        assert!(inflate(b"not zlib", 100).is_err());
    }

    #[test]
    fn response_framing_matches_the_spec() {
        assert_eq!(Response::ok(31).to_bytes(), b"\x1b_Gi=31;OK\x1b\\");
        let mut with_number = Response::ok(99);
        with_number.image_number = Some(13);
        assert_eq!(with_number.to_bytes(), b"\x1b_Gi=99,I=13;OK\x1b\\");
        let mut placed = Response::ok(7);
        placed.placement_id = Some(3);
        assert_eq!(placed.to_bytes(), b"\x1b_Gi=7,p=3;OK\x1b\\");
        let not_found = Response::error(
            10,
            GraphicsError::new(ErrorCode::NotFound, "image not found"),
        );
        assert_eq!(
            not_found.to_bytes(),
            b"\x1b_Gi=10;ENOENT:image not found\x1b\\"
        );
        let mut features = Response::ok(31);
        features.extra = vec![
            (b'e', "T".to_string()),
            (b'C', "1".to_string()),
            (b'U', "1".to_string()),
        ];
        assert_eq!(features.to_bytes(), b"\x1b_Gi=31,e=T,C=1,U=1;OK\x1b\\");
        // Error details are printable ASCII only.
        let odd = Response::error(1, GraphicsError::invalid("bad\u{1b}byte\n"));
        assert_eq!(odd.to_bytes(), b"\x1b_Gi=1;EINVAL:bad?byte?\x1b\\");
        assert_eq!(
            GraphicsError::bad_file().to_string(),
            "EBADF:Failed to read image file"
        );
        assert_eq!(ErrorCode::NoData.as_str(), "ENODATA");
        assert_eq!(ErrorCode::TooBig.as_str(), "EFBIG");
    }

    #[test]
    fn quiet_levels_gate_replies() {
        assert!(Quiet::Verbose.permits(true) && Quiet::Verbose.permits(false));
        assert!(!Quiet::SuppressOk.permits(true) && Quiet::SuppressOk.permits(false));
        assert!(!Quiet::SuppressAll.permits(true) && !Quiet::SuppressAll.permits(false));
    }

    #[test]
    fn graphics_mode_capabilities() {
        assert!(GraphicsMode::Full.replies() && GraphicsMode::Full.displays());
        assert!(!GraphicsMode::DisplayOnly.replies() && GraphicsMode::DisplayOnly.displays());
        assert!(!GraphicsMode::Off.replies() && !GraphicsMode::Off.displays());
        assert_eq!(GraphicsMode::default(), GraphicsMode::Full);
        assert_eq!(
            serde_json::to_string(&GraphicsMode::DisplayOnly).unwrap(),
            "\"display_only\""
        );
    }

    // --- frame composition ---

    fn pixels(width: u32, height: u32, rgba: &[u8]) -> DecodedPixels {
        DecodedPixels {
            width,
            height,
            rgba: bytes::Bytes::copy_from_slice(rgba),
        }
    }

    #[test]
    fn a_frame_is_composed_onto_its_base_or_a_background() {
        // A 3x2 opaque blue base; a 2x1 rectangle of half-transparent red
        // then opaque green lands at (1, 1).
        let blue = [0, 0, 255, 255];
        let base = pixels(3, 2, &blue.repeat(6));
        let data = [255, 0, 0, 128, 0, 255, 0, 255];
        let mut compose = Compose {
            base: Some(base.clone()),
            source: None,
            background: 0x00ff_0088,
            x: 1,
            y: 1,
            mode: ComposeMode::AlphaBlend,
            canvas: (3, 2),
        };
        let blended = compose.apply(&data, 2, 1).unwrap();
        assert_eq!(
            &blended[..16],
            blue.repeat(4),
            "the top row and (0, 1) are the base"
        );
        assert_eq!(
            &blended[16..20],
            [128, 0, 127, 255],
            "red over blue, half and half"
        );
        assert_eq!(&blended[20..], [0, 255, 0, 255], "opaque pixels replace");
        assert_eq!(base.rgba.len(), 24, "the base is untouched");

        compose.mode = ComposeMode::Overwrite;
        let overwritten = compose.apply(&data, 2, 1).unwrap();
        assert_eq!(&overwritten[16..24], data, "alpha included");

        // Without a base the background fills the canvas: Y=0x00ff0088 is
        // translucent green, and blending onto it keeps its translucency.
        compose.base = None;
        compose.mode = ComposeMode::AlphaBlend;
        assert_eq!(compose.background_rgba(), [0, 255, 0, 0x88]);
        let on_background = compose.apply(&data, 2, 1).unwrap();
        assert_eq!(&on_background[..4], [0, 255, 0, 0x88]);
        assert_eq!(&on_background[20..], [0, 255, 0, 255]);
        let [r, g, b, a] = on_background[16..20] else {
            unreachable!()
        };
        assert!(
            r > 150 && g > 50 && b == 0 && a > 0x88 && a < 255,
            "{on_background:?}"
        );

        // Blending over transparent black keeps the colour and the alpha.
        compose.background = 0;
        let on_nothing = compose.apply(&data, 2, 1).unwrap();
        assert_eq!(&on_nothing[16..20], [255, 0, 0, 128]);
        assert_eq!(&on_nothing[..4], [0; 4]);
    }

    #[test]
    fn blend_over_handles_the_alpha_extremes() {
        let mut under = [10, 20, 30, 40];
        blend_over(&mut under, &[1, 2, 3, 0]);
        assert_eq!(
            under,
            [10, 20, 30, 40],
            "a transparent pixel changes nothing"
        );
        blend_over(&mut under, &[1, 2, 3, 255]);
        assert_eq!(under, [1, 2, 3, 255], "an opaque pixel replaces");
        let mut nothing = [9, 9, 9, 0];
        blend_over(&mut nothing, &[1, 2, 3, 0]);
        assert_eq!(nothing, [9, 9, 9, 0]);
        let mut clear = [0, 0, 0, 0];
        blend_over(&mut clear, &[200, 100, 50, 51]);
        assert_eq!(
            clear,
            [200, 100, 50, 51],
            "over nothing, the colour stays straight"
        );
    }

    #[test]
    fn a_compose_refuses_what_does_not_fit() {
        let compose = Compose {
            base: Some(pixels(2, 2, &[0; 16])),
            source: None,
            background: 0,
            x: 1,
            y: 1,
            mode: ComposeMode::Overwrite,
            canvas: (2, 2),
        };
        assert_eq!(
            compose.apply(&[0; 4], 1, 1).unwrap(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        let einval = |result: Result<Vec<u8>, GraphicsError>| {
            assert_eq!(result.unwrap_err().code, ErrorCode::Invalid);
        };
        einval(compose.apply(&[0; 3], 1, 1));
        einval(compose.apply(&[0; 8], 1, 1));
        einval(compose.apply(&[0; 8], 2, 1));
        einval(compose.apply(&[0; 8], 1, 2));
        let wrong_base = Compose {
            base: Some(pixels(1, 1, &[0; 4])),
            ..compose.clone()
        };
        einval(wrong_base.apply(&[0; 4], 1, 1));
        let short_base = Compose {
            base: Some(pixels(2, 2, &[0; 8])),
            ..compose
        };
        einval(short_base.apply(&[0; 4], 1, 1));
    }

    #[test]
    fn a_rectangle_with_no_pixels_leaves_the_canvas_as_it_starts() {
        let base = pixels(2, 2, &[7; 16]);
        let compose = Compose {
            base: Some(base.clone()),
            source: None,
            background: 0x1122_3344,
            x: 1,
            y: 1,
            mode: ComposeMode::Overwrite,
            canvas: (2, 2),
        };
        assert_eq!(compose.apply(&[], 0, 1).unwrap(), [7; 16]);
        assert_eq!(compose.apply(&[], 1, 0).unwrap(), [7; 16]);
        assert_eq!(compose.apply(&[], 0, 0).unwrap(), [7; 16]);
        let on_background = Compose {
            base: None,
            ..compose.clone()
        };
        assert_eq!(
            on_background.apply(&[], 0, 0).unwrap(),
            [0x11, 0x22, 0x33, 0x44].repeat(4)
        );
        // Data for a rectangle that has no pixels is still wrong.
        assert_eq!(
            compose.apply(&[0; 4], 0, 1).unwrap_err().code,
            ErrorCode::Invalid
        );
        // A rectangle with no pixels still has to lie inside the canvas.
        let outside = Compose { x: 3, ..compose };
        assert_eq!(
            outside.apply(&[], 0, 1).unwrap_err().code,
            ErrorCode::Invalid
        );
    }

    #[test]
    fn a_source_rectangle_is_composed_straight_from_its_frame() {
        // A 4x3 source whose pixel at (x, y) is [x, y, 0, 255]; the 2x2 at
        // (1, 1) lands at (2, 0) on a 4x2 canvas.
        let mut source_rgba = Vec::new();
        for y in 0..3u8 {
            for x in 0..4u8 {
                source_rgba.extend_from_slice(&[x, y, 0, 255]);
            }
        }
        let source = pixels(4, 3, &source_rgba);
        let compose = Compose {
            base: Some(pixels(4, 2, &[9; 32])),
            source: Some(ComposeSource {
                pixels: source.clone(),
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }),
            background: 0,
            x: 2,
            y: 0,
            mode: ComposeMode::Overwrite,
            canvas: (4, 2),
        };
        let canvas = compose.apply(&[], 2, 2).unwrap();
        assert_eq!(&canvas[..8], [9; 8], "the base shows left of the rectangle");
        assert_eq!(&canvas[8..16], [1, 1, 0, 255, 2, 1, 0, 255]);
        assert_eq!(&canvas[16..24], [9; 8]);
        assert_eq!(&canvas[24..], [1, 2, 0, 255, 2, 2, 0, 255]);
        assert_eq!(
            compose.source.as_ref().unwrap().pixels,
            source,
            "the source is read, not consumed"
        );
        // Blending reads the same rows.
        let blended = Compose {
            mode: ComposeMode::AlphaBlend,
            ..compose.clone()
        };
        assert_eq!(blended.apply(&[], 2, 2).unwrap(), canvas);

        let einval = |result: Result<Vec<u8>, GraphicsError>| {
            assert_eq!(result.unwrap_err().code, ErrorCode::Invalid);
        };
        // Data alongside a source, a source rectangle that is not the
        // request's size, one leaving the source, and a source frame
        // shorter than its size says.
        einval(compose.apply(&[0; 16], 2, 2));
        einval(compose.apply(&[], 2, 1));
        let past_the_edge = Compose {
            source: Some(ComposeSource {
                pixels: source.clone(),
                x: 3,
                y: 1,
                width: 2,
                height: 2,
            }),
            ..compose.clone()
        };
        einval(past_the_edge.apply(&[], 2, 2));
        let short = Compose {
            source: Some(ComposeSource {
                pixels: pixels(4, 3, &[0; 40]),
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }),
            ..compose.clone()
        };
        einval(short.apply(&[], 2, 2));
        // The rectangle must still land inside the canvas.
        let off_canvas = Compose { x: 3, ..compose };
        einval(off_canvas.apply(&[], 2, 2));
    }

    #[test]
    fn resolving_a_parked_reply_replaces_the_keys_it_already_carries() {
        let mut queue = ReplyQueue::default();
        let mut reply = Response::ok(1);
        reply.extra = vec![(b'r', "9".to_string())];
        let ticket = queue.park(reply, Quiet::Verbose);
        assert!(queue.resolve_with(
            ticket,
            Ok(()),
            vec![(b'r', "3".to_string()), (b'z', "1".to_string())]
        ));
        let mut out = Vec::new();
        queue.drain_into(&mut out);
        assert_eq!(out, b"\x1b_Gi=1,r=3,z=1;OK\x1b\\");
    }
}
