use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, Ordering};

use super::connection::vt_processor;

pub use vt_processor::Color;

/// The underline form carried by terminal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
}

/// The blink rate carried by terminal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Blink {
    #[default]
    None,
    Slow,
    Fast,
}

/// Non-color attributes applied to one terminal text run.
#[allow(clippy::struct_excessive_bools)] // These are independent ECMA-48 toggles, not one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextAttributes {
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underline: Underline,
    pub blink: Blink,
    pub crossed_out: bool,
    pub reverse: bool,
}

impl TextAttributes {
    pub const DEFAULT: Self = Self {
        bold: false,
        faint: false,
        italic: false,
        underline: Underline::None,
        blink: Blink::None,
        crossed_out: false,
        reverse: false,
    };
}

/// A partial [`TextAttributes`]: the write-path twin of the complete readback
/// struct. Script style options arrive as any subset of the attributes; a
/// `Some` field overrides the base the update is applied over, a `None` field
/// leaves the base's value untouched.
#[allow(clippy::struct_excessive_bools)] // Mirrors `TextAttributes` field for field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextAttributesUpdate {
    pub bold: Option<bool>,
    pub faint: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<Underline>,
    pub blink: Option<Blink>,
    pub crossed_out: Option<bool>,
    pub reverse: Option<bool>,
}

impl TextAttributesUpdate {
    pub const UNSET: Self = Self {
        bold: None,
        faint: None,
        italic: None,
        underline: None,
        blink: None,
        crossed_out: None,
        reverse: None,
    };

    #[must_use]
    pub fn apply_to(self, base: TextAttributes) -> TextAttributes {
        TextAttributes {
            bold: self.bold.unwrap_or(base.bold),
            faint: self.faint.unwrap_or(base.faint),
            italic: self.italic.unwrap_or(base.italic),
            underline: self.underline.unwrap_or(base.underline),
            blink: self.blink.unwrap_or(base.blink),
            crossed_out: self.crossed_out.unwrap_or(base.crossed_out),
            reverse: self.reverse.unwrap_or(base.reverse),
        }
    }
}

/// A complete attribute set as an update overrides every field — the lossless
/// round-trip for a read-back span passed straight back to a styling method.
impl From<TextAttributes> for TextAttributesUpdate {
    fn from(value: TextAttributes) -> Self {
        Self {
            bold: Some(value.bold),
            faint: Some(value.faint),
            italic: Some(value.italic),
            underline: Some(value.underline),
            blink: Some(value.blink),
            crossed_out: Some(value.crossed_out),
            reverse: Some(value.reverse),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: vt_processor::Color,
    pub bg: vt_processor::Color,
    pub attributes: TextAttributes,
}

impl Style {
    pub const DEFAULT: Self = Self {
        fg: Color::DefaultForeground { bold: false },
        bg: Color::DefaultBackground,
        attributes: TextAttributes::DEFAULT,
    };
}

impl Default for Style {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A partial [`Style`]: the channels a script write actually set. An unset
/// channel keeps whatever the style being written over already carries —
/// existing span styles for a highlight, the splice-point or delivery default
/// for inserted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleUpdate {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attributes: TextAttributesUpdate,
}

impl StyleUpdate {
    pub const UNSET: Self = Self {
        fg: None,
        bg: None,
        attributes: TextAttributesUpdate::UNSET,
    };

    #[must_use]
    pub fn apply_to(self, base: Style) -> Style {
        Style {
            fg: self.fg.unwrap_or(base.fg),
            bg: self.bg.unwrap_or(base.bg),
            attributes: self.attributes.apply_to(base.attributes),
        }
    }

    #[must_use]
    pub fn is_unset(&self) -> bool {
        *self == Self::UNSET
    }
}

/// A complete style as an update overrides every channel.
impl From<Style> for StyleUpdate {
    fn from(value: Style) -> Self {
        Self {
            fg: Some(value.fg),
            bg: Some(value.bg),
            attributes: value.attributes.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtSpan {
    pub style: Style,
    pub begin_pos: usize,
    pub end_pos: usize,
}

/// RGBA color authored by an OSC 8 link configuration. This stays separate
/// from terminal [`Color`]: OSC colors may carry alpha and must not be mapped
/// through the active ANSI palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

/// Line decoration requested by Mudlet's OSC 8 styling extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkDecoration {
    #[default]
    None,
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// Optional overrides in one OSC 8 style object. `Option<bool>` is
/// intentional: an authored `false` must be able to turn off an SGR attribute
/// already active where the link begins.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkTextStyle {
    pub foreground: Option<LinkColor>,
    pub background: Option<LinkColor>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<LinkDecoration>,
    pub overline: Option<LinkDecoration>,
    pub strikethrough: Option<LinkDecoration>,
    pub decoration_color: Option<LinkColor>,
}

impl LinkTextStyle {
    /// Overlay the properties explicitly authored by `higher` onto this
    /// style. OSC pseudo-classes compose property-by-property rather than
    /// replacing the complete base object.
    #[must_use]
    pub fn overlay(&self, higher: &Self) -> Self {
        Self {
            foreground: higher.foreground.or(self.foreground),
            background: higher.background.or(self.background),
            bold: higher.bold.or(self.bold),
            italic: higher.italic.or(self.italic),
            underline: higher.underline.or(self.underline),
            overline: higher.overline.or(self.overline),
            strikethrough: higher.strikethrough.or(self.strikethrough),
            decoration_color: higher.decoration_color.or(self.decoration_color),
        }
    }
}

/// Authored visual style for a link. The wrapper exists even when every
/// property is absent: `{ "style": {} }` is still an authored style and must
/// suppress Smudgy's fallback underline/wash.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkStyle {
    pub base: LinkTextStyle,
    pub active: Option<LinkTextStyle>,
    pub hover: Option<LinkTextStyle>,
    pub focus_visible: Option<LinkTextStyle>,
    pub focus: Option<LinkTextStyle>,
    pub visited: Option<LinkTextStyle>,
    pub selected: Option<LinkTextStyle>,
    pub disabled: Option<LinkTextStyle>,
    pub link: Option<LinkTextStyle>,
    pub any_link: Option<LinkTextStyle>,
}

/// Stateful OSC styling inputs, resolved by the terminal pane in Mudlet's
/// documented low-to-high priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkStyleState {
    pub active: bool,
    pub hover: bool,
    pub focus_visible: bool,
    pub focus: bool,
    pub visited: bool,
    pub selected: bool,
    pub disabled: bool,
}

impl LinkStyle {
    #[must_use]
    pub fn resolve(&self, state: LinkStyleState) -> LinkTextStyle {
        let mut resolved = self.base.clone();
        // Lowest to highest priority: any-link, link, disabled, selected,
        // visited, focus, focus-visible, hover, active.
        for style in [
            self.any_link.as_ref(),
            (!state.visited).then_some(self.link.as_ref()).flatten(),
            state.disabled.then_some(self.disabled.as_ref()).flatten(),
            state.selected.then_some(self.selected.as_ref()).flatten(),
            state.visited.then_some(self.visited.as_ref()).flatten(),
            state.focus.then_some(self.focus.as_ref()).flatten(),
            state
                .focus_visible
                .then_some(self.focus_visible.as_ref())
                .flatten(),
            state.hover.then_some(self.hover.as_ref()).flatten(),
            state.active.then_some(self.active.as_ref()).flatten(),
        ]
        .into_iter()
        .flatten()
        {
            resolved = resolved.overlay(style);
        }
        resolved
    }

    #[must_use]
    pub fn has_states(&self) -> bool {
        self.active.is_some()
            || self.hover.is_some()
            || self.focus_visible.is_some()
            || self.focus.is_some()
            || self.visited.is_some()
            || self.selected.is_some()
            || self.disabled.is_some()
            || self.link.is_some()
            || self.any_link.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkVisibilityAction {
    Conceal,
    Reveal,
    RevealThenConceal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkVisibilityExpire {
    pub input: bool,
    pub prompt: bool,
    pub output: bool,
    pub output_delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkVisibility {
    pub action: LinkVisibilityAction,
    /// An explicitly supplied delay. `None` is distinct from `Some(0)`: reveal
    /// links without a delay wait for an expiry trigger, while a zero delay
    /// reveals immediately.
    pub delay_ms: Option<u64>,
    pub expire: LinkVisibilityExpire,
    pub whole_line: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSelection {
    pub group: Arc<str>,
    pub value: Arc<str>,
    pub toggle: bool,
    pub selected: bool,
    pub exclusive: bool,
    pub disabled: bool,
}

/// Mudlet-compatible behavior carried only by server-authored OSC links.
/// Script links deliberately bypass the wire-size cap and do not acquire
/// protocol-only state by accident.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkProtocol {
    pub visibility: Option<LinkVisibility>,
    pub selection: Option<LinkSelection>,
    pub spoiler: bool,
}

/// Liveness anchor for a callback link. Every line referencing the callback
/// holds a clone of the `Arc`; the registry holds only a `Weak`, so a
/// callback's registry entry (and its `v8::Global`) can be reclaimed exactly
/// when the last line referencing it leaves every buffer — main scrollback,
/// panes, the recent-lines ring, cross-session copies. Deliberately plain
/// data: a scrollback line must never own a v8 handle, or its eventual drop
/// on the UI thread would abort the process. All tokens compare equal (the
/// derived unit equality) — link identity lives in the `(session, isolate,
/// id)` address beside it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LinkToken(());

/// What a click on a linked range of a line does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// A link-styled range with optional secondary behavior. `primary: None`
    /// is useful for tooltip-only script text; `disabled` preserves hover copy
    /// while suppressing both the primary action and context menu.
    Configured {
        primary: Option<Box<LinkAction>>,
        /// Protocol-level disabled state: blocks both primary activation and
        /// the context menu.
        disabled: bool,
        /// Script-level activation switch: blocks only the left-click path;
        /// an attached menu remains available from right-click.
        primary_enabled: bool,
        menu: Option<LinkMenu>,
        /// A null-primary script menu may use an ordinary left click as a
        /// second way to open the context menu.
        menu_on_left_click: bool,
        /// Protocol-only state (selection, visibility and spoiler behavior).
        /// Script-authored configured links always carry `None` here.
        protocol: Option<LinkProtocol>,
    },
    /// Send this command on the clicked pane's session, as if typed (alias
    /// processing and command splitting apply). Serialized into the line, so
    /// it works for as long as the line is on screen.
    Send(Arc<str>),
    /// Open this http(s)/ftp URL in the system browser. Minted only by the VT
    /// layer from a server's OSC 8 hyperlink — scripts have no wire form for
    /// it — so activation is gated by the per-server link-trust policy.
    OpenUrl(Arc<str>),
    /// Send this command, but the link came from the **server** (an OSC 8
    /// `send:` URI), not from a script: activation is gated by the same
    /// per-server trust policy as [`LinkAction::OpenUrl`].
    ServerSend(Arc<str>),
    /// Prefill the main command input without submitting it. Minted by an OSC
    /// 8 `prompt:` URI.
    Prompt(Arc<str>),
    /// Run a script callback in the engine that created the fragment. The
    /// line carries only this address — the function itself stays in its
    /// isolate's registry, so a click after that engine is gone is a no-op.
    Callback {
        /// The session whose engine holds the callback (fragments can be
        /// echoed into another session's pane).
        session: super::SessionId,
        /// The creating isolate instantiation, in widget-routing-token form
        /// (`IsolateId::to_widget_token`).
        isolate_token: Arc<str>,
        /// The slot in that instantiation's link-callback registry (`u64` so the
        /// monotonic ids can never wrap into aliasing another callback).
        id: u64,
        /// Keeps the registry entry alive while any line carries this action
        /// (see [`LinkToken`]).
        token: Arc<LinkToken>,
    },
}

impl LinkAction {
    /// The action a normal left click may execute. Disabled and tooltip-only
    /// configured links return `None`.
    #[must_use]
    pub fn primary(&self) -> Option<&Self> {
        match self {
            Self::Configured { disabled: true, .. }
            | Self::Configured {
                primary_enabled: false,
                ..
            }
            | Self::Configured { primary: None, .. } => None,
            Self::Configured {
                primary: Some(primary),
                ..
            } => primary.primary(),
            _ => Some(self),
        }
    }

    /// The underlying primary target, even when activation is disabled. Used
    /// only for honest target disclosure.
    #[must_use]
    pub fn disclosed_target(&self) -> Option<&Self> {
        match self {
            Self::Configured {
                primary: Some(primary),
                ..
            } => primary.disclosed_target(),
            Self::Configured { primary: None, .. } => None,
            _ => Some(self),
        }
    }

    /// Complete, unescaped text for the action this link would perform.
    /// Configured wrappers are unwrapped; callbacks and actionless links
    /// disclose nothing. Callers must escape this before rendering it.
    #[must_use]
    pub fn disclosure_target_text(&self) -> Option<Cow<'_, str>> {
        let target = self.disclosed_target()?;
        match target {
            Self::Send(command) | Self::OpenUrl(command) => Some(Cow::Borrowed(command.as_ref())),
            Self::ServerSend(command) => Some(Cow::Owned(format!("send:{command}"))),
            Self::Prompt(command) => Some(Cow::Owned(format!("prompt:{command}"))),
            Self::Callback { .. } | Self::Configured { .. } => None,
        }
    }

    /// Safe, prefix-bounded tooltip copy for the action this link would
    /// perform. Invisible target bytes are written explicitly so two actions
    /// cannot look identical while sending or opening different data.
    #[must_use]
    pub fn tooltip_target(&self) -> Option<Arc<str>> {
        let target = self.disclosed_target()?;
        match target {
            Self::Send(command) | Self::OpenUrl(command) => {
                Some(match tooltip_action_target(command) {
                    Cow::Borrowed(_) => Arc::clone(command),
                    Cow::Owned(display) => Arc::from(display),
                })
            }
            Self::ServerSend(command) => Some(Arc::from(
                tooltip_action_target(&format!("send:{command}")).as_ref(),
            )),
            Self::Prompt(command) => Some(Arc::from(
                tooltip_action_target(&format!("prompt:{command}")).as_ref(),
            )),
            Self::Callback { .. } | Self::Configured { .. } => None,
        }
    }

    /// The enabled context menu, if one was supplied.
    #[must_use]
    pub fn menu(&self) -> Option<&LinkMenu> {
        match self {
            Self::Configured {
                disabled: false,
                menu,
                ..
            } => menu.as_ref(),
            _ => None,
        }
    }

    /// Whether a normal left click should open the menu instead of dispatching
    /// a primary action. Right-click menu availability is governed by
    /// [`Self::menu`] independently.
    #[must_use]
    pub fn opens_menu_on_left_click(&self) -> bool {
        matches!(
            self,
            Self::Configured {
                disabled: false,
                primary_enabled: true,
                menu: Some(_),
                menu_on_left_click: true,
                ..
            }
        )
    }

    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.primary().is_some()
            || self.menu().is_some()
            || self.protocol().is_some_and(|protocol| protocol.spoiler)
    }

    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Configured { disabled: true, .. })
    }

    #[must_use]
    pub fn protocol(&self) -> Option<&LinkProtocol> {
        match self {
            Self::Configured { protocol, .. } => protocol.as_ref(),
            _ => None,
        }
    }
}

/// Bound tooltip shaping without limiting the underlying script-authored
/// action. The ellipsis makes the disclosure truncation explicit.
fn tooltip_action_target(target: &str) -> Cow<'_, str> {
    const MAX_CHARS: usize = 512;

    if let Some((cutoff, _)) = target.char_indices().nth(MAX_CHARS) {
        let mut escaped =
            escape_invisible_text(&target[..cutoff], InvisiblePolicy::ActionTarget).into_owned();
        escaped.push('\u{2026}');
        return Cow::Owned(escaped);
    }
    escape_invisible_text(target, InvisiblePolicy::ActionTarget)
}

/// A right-click menu attached to a link-styled range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMenu {
    pub title: Option<LinkMenuTitle>,
    pub items: Arc<[LinkMenuItem]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMenuTitle {
    pub text: Arc<str>,
    pub style: Option<LinkTextStyle>,
}

/// One context-menu row. OSC menu labels and script labels are always rendered
/// as plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkMenuItem {
    Separator,
    Action { label: Arc<str>, action: LinkAction },
}

/// Safe, display-ready tooltip copy. ANSI control sequences have already been
/// reduced to plain text plus semantic terminal color spans; no executable
/// terminal controls remain in `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTooltipText {
    pub text: Arc<str>,
    pub spans: Arc<[VtSpan]>,
}

impl LinkTooltipText {
    #[must_use]
    pub fn plain(text: Arc<str>) -> Self {
        let len = text.len();
        Self {
            text,
            spans: Arc::from([VtSpan {
                style: Style::default(),
                begin_pos: 0,
                end_pos: len,
            }]),
        }
    }
}

const TOOLTIP_IDLE: u8 = 0;
const TOOLTIP_LOADING: u8 = 1;
const TOOLTIP_READY: u8 = 2;
const TOOLTIP_FAILED: u8 = 3;

/// Shared result cell for a script-authored tooltip callback. The UI atomically
/// claims the first hover, the owning isolate resolves the callback (which may
/// be async), and every copy of the linked line observes the cached result.
#[derive(Debug, Default)]
pub struct LinkTooltipState {
    status: AtomicU8,
    text: RwLock<Option<LinkTooltipText>>,
}

impl LinkTooltipState {
    /// Claim the one lazy resolution attempt made for this tooltip.
    #[must_use]
    pub fn begin_request(&self) -> bool {
        self.status
            .compare_exchange(
                TOOLTIP_IDLE,
                TOOLTIP_LOADING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Publish a callback result. `None` is a terminal failure; the target
    /// fallback (when one exists) remains available to the UI.
    pub fn resolve(&self, text: Option<LinkTooltipText>) {
        let ready = text.is_some();
        *self
            .text
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = text;
        self.status.store(
            if ready { TOOLTIP_READY } else { TOOLTIP_FAILED },
            Ordering::Release,
        );
    }

    #[must_use]
    pub fn text(&self) -> Option<LinkTooltipText> {
        if self.status.load(Ordering::Acquire) != TOOLTIP_READY {
            return None;
        }
        self.text
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the tooltip callback has been dispatched but has not resolved.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.status.load(Ordering::Acquire) == TOOLTIP_LOADING
    }
}

/// The callback address and shared result cell for a lazy script tooltip.
#[derive(Debug, Clone)]
pub struct LinkTooltipCallback {
    pub session: super::SessionId,
    pub isolate_token: Arc<str>,
    pub id: u64,
    /// Keeps the registry entry alive while any line carries this tooltip
    /// (see [`LinkToken`]).
    pub token: Arc<LinkToken>,
    pub state: Arc<LinkTooltipState>,
}

impl PartialEq for LinkTooltipCallback {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.isolate_token == other.isolate_token
            && self.id == other.id
    }
}

impl Eq for LinkTooltipCallback {}

/// The primary hover copy for a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTooltipSource {
    Text(LinkTooltipText),
    Callback(LinkTooltipCallback),
}

/// Hover copy attached to a linked range. `target` is present only when the
/// primary copy is custom; the UI renders the real action target beneath it in
/// muted text so author-provided help cannot conceal a deceptive destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTooltip {
    pub source: LinkTooltipSource,
    pub target: Option<Arc<str>>,
}

impl LinkTooltip {
    #[must_use]
    pub fn text(text: Arc<str>, target: Option<Arc<str>>) -> Self {
        Self {
            source: LinkTooltipSource::Text(LinkTooltipText::plain(text)),
            target,
        }
    }

    #[must_use]
    pub fn styled_text(text: LinkTooltipText, target: Option<Arc<str>>) -> Self {
        Self {
            source: LinkTooltipSource::Text(text),
            target,
        }
    }

    #[must_use]
    pub fn callback(callback: LinkTooltipCallback, target: Option<Arc<str>>) -> Self {
        Self {
            source: LinkTooltipSource::Callback(callback),
            target,
        }
    }

    /// The current renderable `(primary, secondary target)` pair. Before a
    /// callback resolves, a known target is the safe fallback; a callback-only
    /// link simply has no tooltip yet.
    #[must_use]
    pub fn display(&self) -> Option<(Arc<str>, Option<Arc<str>>)> {
        self.display_styled()
            .map(|(primary, secondary)| (primary.text, secondary))
    }

    /// The current renderable styled primary copy and plain secondary target.
    #[must_use]
    pub fn display_styled(&self) -> Option<(LinkTooltipText, Option<Arc<str>>)> {
        match &self.source {
            LinkTooltipSource::Text(text) => {
                Some((text.clone(), self.secondary_for(text.text.as_ref())))
            }
            LinkTooltipSource::Callback(callback) => callback
                .state
                .text()
                .map(|text| {
                    let secondary = self.secondary_for(text.text.as_ref());
                    (text, secondary)
                })
                .or_else(|| {
                    self.target
                        .clone()
                        .map(|target| (LinkTooltipText::plain(target), None))
                }),
        }
    }

    fn secondary_for(&self, primary: &str) -> Option<Arc<str>> {
        self.target
            .as_ref()
            .filter(|target| target.as_ref() != primary)
            .cloned()
    }

    /// Claim this dynamic tooltip's first resolution and return the request to
    /// route home. Static and already-requested tooltips return `None`.
    #[must_use]
    pub fn request(&self) -> Option<LinkTooltipCallback> {
        let LinkTooltipSource::Callback(callback) = &self.source else {
            return None;
        };
        callback.state.begin_request().then(|| callback.clone())
    }

    /// Whether this tooltip is currently waiting for its async callback.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(
            &self.source,
            LinkTooltipSource::Callback(callback) if callback.state.is_loading()
        )
    }
}

/// One clickable byte range of a line. Kept in a list parallel to the style
/// spans (not on [`VtSpan`]) so the hot ingest path and the span-surgery code
/// stay link-free; link ranges may cross style-span boundaries and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub begin_pos: usize,
    pub end_pos: usize,
    pub action: LinkAction,
    pub tooltip: Option<LinkTooltip>,
    /// `None` means no OSC-authored style and enables Smudgy's fallback link
    /// affordance. `Some`, including an empty style, follows authored OSC
    /// semantics exactly.
    pub style: Option<Arc<LinkStyle>>,
}

/// Link metadata on one styled-text run before its byte range is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLink {
    pub action: LinkAction,
    pub tooltip: Option<LinkTooltip>,
    pub style: Option<Arc<LinkStyle>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub text: String,
    pub spans: Vec<VtSpan>,
    /// Clickable ranges, sorted and non-overlapping (usually empty — an empty
    /// vec does not allocate). Unlike `spans`, these need not cover the text.
    pub links: Vec<LinkSpan>,
    /// The line's pre-VT wire form (escape sequences included, CR/LF excluded),
    /// captured only while some trigger carries a raw pattern — raw matching is
    /// this field's sole consumer, and the lossy copy is pure overhead for the
    /// (overwhelmingly common) profiles without one.
    raw: Option<String>,
}

/// Cold-path accumulator for a logical line provisionally emitted in pieces.
/// One fragment reuses its existing `Arc`; only a second fragment allocates a
/// side vector. Joining precomputes every destination capacity and copies each
/// byte/span/link exactly once.
#[derive(Debug, Default)]
pub(crate) enum LineFragments {
    #[default]
    None,
    One(Arc<StyledLine>),
    Many(Vec<Arc<StyledLine>>),
}

impl LineFragments {
    #[cold]
    pub(crate) fn push(&mut self, fragment: Arc<StyledLine>) {
        match self {
            Self::None => *self = Self::One(fragment),
            Self::One(_) => {
                let Self::One(first) = std::mem::take(self) else {
                    unreachable!();
                };
                let mut fragments = Vec::with_capacity(4);
                fragments.push(first);
                fragments.push(fragment);
                *self = Self::Many(fragments);
            }
            Self::Many(fragments) => fragments.push(fragment),
        }
    }

    /// Consume all accumulated fragments. A single fragment is returned
    /// unchanged; multiple fragments are flattened once.
    #[cold]
    pub(crate) fn take_joined(&mut self) -> Option<Arc<StyledLine>> {
        match std::mem::take(self) {
            Self::None => None,
            Self::One(line) => Some(line),
            Self::Many(fragments) => Some(Arc::new(StyledLine::concatenate_fragments(
                &fragments, None,
            ))),
        }
    }

    /// Consume the provisional prefix and concatenate it with `completion` in
    /// one exact-capacity pass. Returns `None` for the ordinary unfragmented
    /// path.
    #[cold]
    pub(crate) fn take_joined_with(
        &mut self,
        completion: &Arc<StyledLine>,
    ) -> Option<Arc<StyledLine>> {
        match std::mem::take(self) {
            Self::None => None,
            Self::One(prefix) => Some(Arc::new(StyledLine::concatenate_fragments(
                &[prefix],
                Some(completion),
            ))),
            Self::Many(fragments) => Some(Arc::new(StyledLine::concatenate_fragments(
                &fragments,
                Some(completion),
            ))),
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::None;
    }

    #[inline]
    pub(crate) fn has_fragments(&self) -> bool {
        matches!(self, Self::One(_) | Self::Many(_))
    }
}

/// Clamp `pos` into `text` and snap it down to a char boundary. The line-edit
/// methods run every script-supplied byte offset through this: a mid-code-point
/// span or link boundary would panic the first `&text[a..b]` slice it meets
/// (here or in the renderer), and scripts address lines by raw byte offsets.
/// Public so out-of-crate byte-offset consumers (the bench corpus slicer)
/// share one definition.
#[must_use]
pub fn floor_char_boundary(text: &str, pos: usize) -> usize {
    let mut pos = pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

impl StyledLine {
    #[must_use]
    pub fn new(text: &str, span_info: Vec<VtSpan>) -> Self {
        Self {
            text: String::from(text),
            spans: span_info,
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn new_with_raw(text: &str, span_info: Vec<VtSpan>, raw: Option<&[u8]>) -> Self {
        Self {
            text: String::from(text),
            spans: span_info,
            links: Vec::new(),
            raw: raw.map(|raw| String::from_utf8_lossy(raw).into_owned()),
        }
    }

    /// [`Self::new_with_raw`] for valid raw text. Borrowed text is copied; an owned lossy
    /// decode is moved into the line without a second allocation or copy.
    #[must_use]
    pub fn new_with_raw_text(
        text: &str,
        span_info: Vec<VtSpan>,
        raw: Option<Cow<'_, str>>,
    ) -> Self {
        Self {
            text: String::from(text),
            spans: span_info,
            links: Vec::new(),
            raw: raw.map(Cow::into_owned),
        }
    }

    #[must_use]
    pub fn append(&self, other_line: &StyledLine) -> Self {
        Self {
            text: format!("{}{}", self.text, other_line.text),
            spans: self
                .spans
                .clone()
                .into_iter()
                .chain(other_line.spans.iter().map(|span| VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos + self.text.len(),
                    end_pos: span.end_pos + self.text.len(),
                }))
                .collect(),
            links: self
                .links
                .iter()
                .cloned()
                .chain(other_line.links.iter().map(|link| LinkSpan {
                    begin_pos: link.begin_pos + self.text.len(),
                    end_pos: link.end_pos + self.text.len(),
                    action: link.action.clone(),
                    tooltip: link.tooltip.clone(),
                    style: link.style.clone(),
                }))
                .collect(),
            raw: match self.raw {
                Some(ref raw) => {
                    let mut combined = raw.clone();
                    match other_line.raw {
                        Some(ref other_raw) => {
                            combined.push_str(other_raw);
                            Some(combined)
                        }
                        None => Some(combined),
                    }
                }
                None => other_line.raw.clone(),
            },
        }
    }

    /// Concatenate a fragmented logical line after one metadata-only sizing
    /// pass. This is the cold-path sibling of [`Self::append`]: transport-batch
    /// fragments may be numerous, so folding `append` over them would repeatedly
    /// copy the complete prefix and become quadratic.
    ///
    /// `has_raw` follows `append`'s union semantics. The VT producer keeps raw
    /// capture latched across an open logical line, so production fragments are
    /// in practice either all raw or all cooked.
    #[cold]
    fn concatenate_fragments(fragments: &[Arc<Self>], completion: Option<&Arc<Self>>) -> Self {
        let mut text_len = 0;
        let mut span_len = 0;
        let mut link_len = 0;
        let mut raw_len = 0;
        let mut has_raw = false;
        for fragment in fragments
            .iter()
            .map(Arc::as_ref)
            .chain(completion.map(Arc::as_ref))
        {
            text_len += fragment.text.len();
            span_len += fragment.spans.len();
            link_len += fragment.links.len();
            raw_len += fragment.raw().map_or(0, str::len);
            has_raw |= fragment.raw().is_some();
        }

        let mut combined = Self {
            text: String::with_capacity(text_len),
            spans: Vec::with_capacity(span_len),
            links: Vec::with_capacity(link_len),
            raw: has_raw.then(|| String::with_capacity(raw_len)),
        };

        for fragment in fragments
            .iter()
            .map(Arc::as_ref)
            .chain(completion.map(Arc::as_ref))
        {
            let offset = combined.text.len();
            combined.text.push_str(&fragment.text);
            combined
                .spans
                .extend(fragment.spans.iter().map(|span| VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos + offset,
                    end_pos: span.end_pos + offset,
                }));
            combined
                .links
                .extend(fragment.links.iter().map(|link| LinkSpan {
                    begin_pos: link.begin_pos + offset,
                    end_pos: link.end_pos + offset,
                    action: link.action.clone(),
                    tooltip: link.tooltip.clone(),
                    style: link.style.clone(),
                }));
            if let (Some(raw), Some(fragment_raw)) = (&mut combined.raw, &fragment.raw) {
                raw.push_str(fragment_raw);
            }
        }

        debug_assert_eq!(combined.text.len(), text_len);
        debug_assert_eq!(combined.spans.len(), span_len);
        debug_assert_eq!(combined.links.len(), link_len);
        debug_assert_eq!(combined.raw.as_ref().map_or(0, String::len), raw_len);
        combined
    }

    /// Re-map the link spans across a splice that replaces `text[begin..end]` with
    /// `insert_len` new (link-free) bytes: the piece of a link before the replaced
    /// region survives in place, the piece after it shifts by the length delta, and
    /// bytes overlapping the region drop their link — the same interval rules as the
    /// style-span remap inside [`Self::insert`] (kept in lockstep; only the
    /// no-re-cover rule differs), so `begin`/`end` must arrive with the same clamping
    /// `insert`/`remove` apply.
    fn remap_links(&self, begin: usize, end: usize, insert_len: usize) -> Vec<LinkSpan> {
        if self.links.is_empty() {
            return Vec::new();
        }
        let shift = insert_len as i64 - (end - begin) as i64;
        let mut links = Vec::with_capacity(self.links.len());
        for link in &self.links {
            if link.begin_pos < begin {
                let clipped_end = link.end_pos.min(begin);
                if clipped_end > link.begin_pos {
                    links.push(LinkSpan {
                        begin_pos: link.begin_pos,
                        end_pos: clipped_end,
                        action: link.action.clone(),
                        tooltip: link.tooltip.clone(),
                        style: link.style.clone(),
                    });
                }
            }
            if link.end_pos > end {
                let after_begin = link.begin_pos.max(end);
                let begin_pos = ((after_begin as i64) + shift).max(0) as usize;
                let end_pos = ((link.end_pos as i64) + shift).max(0) as usize;
                if end_pos > begin_pos {
                    links.push(LinkSpan {
                        begin_pos,
                        end_pos,
                        action: link.action.clone(),
                        tooltip: link.tooltip.clone(),
                        style: link.style.clone(),
                    });
                }
            }
        }
        links
    }

    #[must_use]
    pub fn insert(&self, str: &str, begin: usize, end: usize, style: Style) -> Self {
        // Clamp bounds into the text and onto char boundaries
        let begin = floor_char_boundary(&self.text, begin);
        let end = floor_char_boundary(&self.text, end).max(begin);

        // Create new text by inserting the string
        let mut new_text = String::new();
        new_text.push_str(&self.text[..begin]);
        new_text.push_str(str);
        new_text.push_str(&self.text[end..]);

        let insert_len = str.len();
        let removed_len = end - begin;
        let shift = insert_len as i32 - removed_len as i32;

        let mut new_spans = Vec::new();

        // Re-map each existing span across the splice that replaces `text[begin..end]`
        // (length `removed_len`) with `str` (length `insert_len`, shifting everything past
        // `end` by `shift`). A span contributes at most two pieces: the part strictly
        // before `begin`, kept in place, and the part strictly after `end`, shifted right
        // by `shift`. Bytes overlapping the replaced region are dropped — the inserted-text
        // span below covers them. The result is non-overlapping and gap-free, which the
        // renderer relies on: it tiles the on-screen line by slicing `text[begin..end]` per
        // span, so overlapping spans would duplicate text (and overrun byte offsets on copy).
        for span in &self.spans {
            // Portion of the span before the replaced region, unchanged.
            if span.begin_pos < begin {
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos,
                    end_pos: span.end_pos.min(begin),
                });
            }
            // Portion of the span after the replaced region, shifted by the length delta.
            // `after_begin` maps to `begin + insert_len` for a span that spans the region,
            // sitting flush against the inserted-text span below.
            if span.end_pos > end {
                let after_begin = span.begin_pos.max(end);
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: ((after_begin as i32) + shift).max(0) as usize,
                    end_pos: ((span.end_pos as i32) + shift).max(0) as usize,
                });
            }
        }

        // Add span for the inserted text if it's not empty
        if !str.is_empty() {
            new_spans.push(VtSpan {
                style,
                begin_pos: begin,
                end_pos: begin + insert_len,
            });
        }

        // Sort spans by begin position
        new_spans.sort_by_key(|span| span.begin_pos);

        Self {
            text: new_text,
            spans: new_spans,
            links: self.remap_links(begin, end, insert_len),
            raw: self.raw.clone(),
        }
    }

    #[must_use]
    pub fn highlight(&self, begin: usize, end: usize, update: StyleUpdate) -> Self {
        // Clamp bounds into the text and onto char boundaries
        let begin = floor_char_boundary(&self.text, begin);
        let end = floor_char_boundary(&self.text, end).max(begin);

        // An empty range, or an update that sets nothing, changes nothing.
        if begin >= end || update.is_unset() {
            return self.clone();
        }

        // Restyled pieces collect separately so adjacent equal results merge with
        // each other (a full update over many spans collapses back to one span)
        // without ever merging into a span the range did not touch.
        fn push_restyled(
            restyled: &mut Vec<VtSpan>,
            style: Style,
            begin_pos: usize,
            end_pos: usize,
        ) {
            if begin_pos >= end_pos {
                return;
            }
            if let Some(last) = restyled.last_mut()
                && last.end_pos == begin_pos
                && last.style == style
            {
                last.end_pos = end_pos;
                return;
            }
            restyled.push(VtSpan {
                style,
                begin_pos,
                end_pos,
            });
        }

        // The base an uncovered stretch of the range takes: spans normally tile
        // the line, but edited lines can carry gaps, and those render as the
        // terminal defaults — the same fallback the splice path uses.
        let gap_base = Style::DEFAULT;

        let mut new_spans = Vec::new();
        let mut restyled = Vec::new();
        let mut cursor = begin;

        // Keep the parts of spans outside the range (split at its boundaries);
        // apply the update over each part inside, span by span, so everything
        // the update leaves unset keeps the value that part already had.
        for span in &self.spans {
            if span.end_pos <= begin || span.begin_pos >= end {
                new_spans.push(*span);
                continue;
            }
            if span.begin_pos < begin {
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos,
                    end_pos: begin,
                });
            }
            let overlap_begin = span.begin_pos.max(begin);
            let overlap_end = span.end_pos.min(end);
            if cursor < overlap_begin {
                push_restyled(
                    &mut restyled,
                    update.apply_to(gap_base),
                    cursor,
                    overlap_begin,
                );
            }
            push_restyled(
                &mut restyled,
                update.apply_to(span.style),
                overlap_begin,
                overlap_end,
            );
            cursor = overlap_end;
            if span.end_pos > end {
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: end,
                    end_pos: span.end_pos,
                });
            }
        }
        if cursor < end {
            push_restyled(&mut restyled, update.apply_to(gap_base), cursor, end);
        }

        new_spans.append(&mut restyled);

        // Sort spans by begin position to maintain order
        new_spans.sort_by_key(|span| span.begin_pos);

        Self {
            text: self.text.clone(),
            spans: new_spans,
            // A recolor moves no bytes, so the link ranges are untouched.
            links: self.links.clone(),
            raw: self.raw.clone(),
        }
    }

    /// Replace the link coverage of `[begin, end)` in place, without touching
    /// text or styling: existing links are trimmed to the outside of the range
    /// (one spanning it splits, keeping its outside pieces — the same interval
    /// rules a splice applies), and `link`, if given, covers the range as one
    /// new span. `None` bare-strips the range. In-place so a linkify applied
    /// after a restyle swaps the links vec instead of recloning the line.
    pub fn relink(&mut self, begin: usize, end: usize, link: Option<StyledLink>) {
        let begin = floor_char_boundary(&self.text, begin);
        let end = floor_char_boundary(&self.text, end).max(begin);
        if begin >= end {
            return;
        }
        // A zero length delta makes the splice remap a pure trim.
        let mut links = self.remap_links(begin, end, end - begin);
        if let Some(link) = link {
            // The trimmed vec is already sorted; place the one new span.
            // Deliberately no merging with equal-action neighbors: unlike a
            // splice's runs, an adjacent link here is a distinct registration.
            let at = links.partition_point(|existing| existing.begin_pos < begin);
            links.insert(
                at,
                LinkSpan {
                    begin_pos: begin,
                    end_pos: end,
                    action: link.action,
                    tooltip: link.tooltip,
                    style: link.style,
                },
            );
        }
        self.links = links;
    }

    #[must_use]
    pub fn remove(&self, begin: usize, end: usize) -> Self {
        let text = self.text.as_str();
        let begin = floor_char_boundary(text, begin);
        let end = floor_char_boundary(text, end).max(begin);

        let shift = end - begin;

        let new_spans = self
            .spans
            .iter()
            .filter_map(|span| {
                if span.begin_pos >= begin && span.end_pos <= end {
                    // Span is completely within removal range - remove it
                    None
                } else if span.begin_pos >= end {
                    // Span is completely after removal range - shift it left
                    Some(VtSpan {
                        begin_pos: span.begin_pos - shift,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else if span.end_pos <= begin {
                    // Span is completely before removal range - keep it unchanged
                    Some(*span)
                } else if span.begin_pos < begin && span.end_pos > end {
                    // Span encompasses removal range - shrink it
                    Some(VtSpan {
                        begin_pos: span.begin_pos,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else if span.begin_pos < begin && span.end_pos > begin {
                    // Span starts before and ends within removal range - truncate to before part
                    Some(VtSpan {
                        begin_pos: span.begin_pos,
                        end_pos: begin,
                        style: span.style,
                    })
                } else if span.begin_pos < end && span.end_pos > end {
                    // Span starts within and ends after removal range - keep the after part, shifted
                    Some(VtSpan {
                        begin_pos: begin,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else {
                    // Should not reach here, but keep the span as fallback
                    Some(*span)
                }
            })
            .collect();

        Self {
            text: text[..begin].to_string() + &text[end..],
            spans: new_spans,
            links: self.remap_links(begin, end, 0),
            raw: self.raw.clone(),
        }
    }

    /// Build a line from styled runs: each run contributes its text with its style and
    /// optional link, in order. Spans are laid down flush against each other (adjacent
    /// same-style runs merge; adjacent same-action link runs merge), so the result tiles
    /// the text non-overlapping and gap-free by construction — the invariant the
    /// renderer relies on. Empty runs contribute nothing; an empty run set yields the
    /// same single empty span an empty echo produces.
    #[must_use]
    pub fn from_styled_runs(
        runs: &[(&str, Style, Option<LinkAction>)],
        empty_style: Style,
    ) -> Self {
        let runs: Vec<_> = runs
            .iter()
            .map(|(text, style, action)| {
                (
                    *text,
                    *style,
                    action.clone().map(|action| StyledLink {
                        action,
                        tooltip: None,
                        style: None,
                    }),
                )
            })
            .collect();
        Self::from_linked_runs(&runs, empty_style)
    }

    /// Build a line like [`Self::from_styled_runs`], retaining optional hover
    /// metadata on each linked run.
    #[must_use]
    pub fn from_linked_runs(
        runs: &[(&str, Style, Option<StyledLink>)],
        empty_style: Style,
    ) -> Self {
        let mut text = String::with_capacity(runs.iter().map(|(t, _, _)| t.len()).sum());
        let mut spans: Vec<VtSpan> = Vec::with_capacity(runs.len());
        let mut links: Vec<LinkSpan> = Vec::new();
        for (run_text, style, link) in runs {
            if run_text.is_empty() {
                continue;
            }
            let begin = text.len();
            text.push_str(run_text);
            match spans.last_mut() {
                Some(prev) if prev.style == *style && prev.end_pos == begin => {
                    prev.end_pos = text.len();
                }
                _ => spans.push(VtSpan {
                    style: *style,
                    begin_pos: begin,
                    end_pos: text.len(),
                }),
            }
            if let Some(link) = link {
                match links.last_mut() {
                    Some(prev)
                        if prev.action == link.action
                            && prev.tooltip == link.tooltip
                            && prev.style == link.style
                            && prev.end_pos == begin =>
                    {
                        prev.end_pos = text.len();
                    }
                    _ => links.push(LinkSpan {
                        begin_pos: begin,
                        end_pos: text.len(),
                        action: link.action.clone(),
                        tooltip: link.tooltip.clone(),
                        style: link.style.clone(),
                    }),
                }
            }
        }
        if spans.is_empty() {
            spans.push(VtSpan {
                style: empty_style,
                begin_pos: 0,
                end_pos: 0,
            });
        }
        Self {
            text,
            spans,
            links,
            raw: None,
        }
    }

    /// Build a whole line of one role color from locally-produced text
    /// (echoes, notices, sent-command display). The text is display-bound and
    /// never came through the VT parser, so control characters (a stray ESC,
    /// a `\r` tail from CRLF-split input) are stripped here — `\t` survives.
    fn from_role_str(text: &str, fg: Color) -> Self {
        let text = sanitize_display_text(text);
        Self {
            spans: vec![VtSpan {
                begin_pos: 0,
                end_pos: text.len(),
                style: Style {
                    fg,
                    bg: Color::DefaultBackground,
                    ..Style::DEFAULT
                },
            }],
            text: text.into_owned(),
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn from_echo_str(text: &str) -> Self {
        Self::from_role_str(text, Color::Echo)
    }

    #[must_use]
    pub fn from_warn_str(text: &str) -> Self {
        Self::from_role_str(text, Color::Warn)
    }

    #[must_use]
    pub fn from_output_str(text: &str) -> Self {
        Self::from_role_str(text, Color::Output)
    }

    #[must_use]
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }
}

/// Strip control characters from display-bound text that bypasses the VT
/// parser (echoes, notices, styled-echo runs). `\t` survives — scripts echo
/// tabs for alignment — everything else in the control ranges (a stray ESC,
/// `\r` tails, C1 bytes) is dirty data with no display meaning. Borrows when
/// the text is already clean, which is the overwhelmingly common case.
#[must_use]
pub fn sanitize_display_text(text: &str) -> Cow<'_, str> {
    let dirty = |c: char| c.is_control() && c != '\t';
    if text.chars().any(dirty) {
        Cow::Owned(text.chars().filter(|&c| !dirty(c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// How invisible Unicode is handled in user-visible copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvisiblePolicy {
    /// Labels, menu titles, and tooltip prose. Joiners, variation selectors,
    /// directional marks, and tag characters reach the shaper so authored
    /// grapheme sequences and bidirectional labels remain intact.
    Prose,
    /// Commands and URLs. Every invisible byte is disclosed explicitly
    /// because it changes the action even when the rendered text looks equal.
    ActionTarget,
}

/// True for an invisible character that can make rendered text conceal its
/// underlying bytes. Ordinary controls are handled separately by
/// [`push_escaped_char`] so callers auditing source text do not flag every
/// newline.
#[must_use]
pub fn deceptive_invisible(c: char, policy: InvisiblePolicy) -> bool {
    let shaping_control = matches!(
        c,
        '\u{061c}'
            | '\u{180b}'..='\u{180d}'
            | '\u{200c}'..='\u{200f}'
            | '\u{fe00}'..='\u{fe0f}'
    ) || ('\u{e0000}'..='\u{e007f}').contains(&c)
        || ('\u{e0100}'..='\u{e01ef}').contains(&c);
    if policy == InvisiblePolicy::Prose && shaping_control {
        return false;
    }
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'
            | '\u{1160}'
            | '\u{17b4}'
            | '\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

fn requires_explicit_escape(c: char, policy: InvisiblePolicy) -> bool {
    c.is_control()
        || deceptive_invisible(c, policy)
        || (policy == InvisiblePolicy::ActionTarget && matches!(c, '\\' | '\u{2028}' | '\u{2029}'))
}

/// Append one display-safe character under `policy`. Action targets also
/// escape literal backslashes so escaped Unicode notation cannot be forged by
/// ordinary target text.
pub fn push_escaped_char(out: &mut String, c: char, policy: InvisiblePolicy) {
    if policy == InvisiblePolicy::ActionTarget && c == '\\' {
        out.push_str("\\\\");
    } else if requires_explicit_escape(c, policy) {
        write!(out, "\\u{{{:X}}}", u32::from(c)).expect("writing to a String cannot fail");
    } else {
        out.push(c);
    }
}

/// Escape control and deceptive invisible characters while borrowing already
/// safe text. Action targets additionally escape literal backslashes and
/// Unicode line/paragraph separators to keep disclosures injective and on one
/// visual line.
#[must_use]
pub fn escape_invisible_text(text: &str, policy: InvisiblePolicy) -> Cow<'_, str> {
    if !text.chars().any(|c| requires_explicit_escape(c, policy)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        push_escaped_char(&mut out, c, policy);
    }
    Cow::Owned(out)
}

impl std::ops::Deref for StyledLine {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.text.as_str()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_text_constructor_keeps_the_owned_decode_allocation() {
        let raw = String::from_utf8_lossy(b"raw\xfe text").into_owned();
        let allocation = raw.as_ptr();
        let line = super::StyledLine::new_with_raw_text(
            "display",
            Vec::new(),
            Some(std::borrow::Cow::Owned(raw)),
        );
        let captured = line.raw().expect("raw capture");
        assert_eq!(captured, "raw\u{fffd} text");
        assert_eq!(captured.as_ptr(), allocation);
    }

    use super::*;
    use crate::session::connection::vt_processor::AnsiColor;

    fn create_test_style(fg_color: AnsiColor, bold: bool) -> Style {
        Style {
            fg: Color::Ansi {
                color: fg_color,
                bold,
            },
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        }
    }

    fn create_test_line() -> StyledLine {
        StyledLine::new(
            "Hello World Test",
            vec![
                VtSpan {
                    style: create_test_style(AnsiColor::Red, false),
                    begin_pos: 0,
                    end_pos: 5, // "Hello"
                },
                VtSpan {
                    style: create_test_style(AnsiColor::Green, false),
                    begin_pos: 6,
                    end_pos: 11, // "World"
                },
                VtSpan {
                    style: create_test_style(AnsiColor::Blue, false),
                    begin_pos: 12,
                    end_pos: 16, // "Test"
                },
            ],
        )
    }

    #[test]
    fn test_insert_at_beginning() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("START ", 0, 0, new_style);

        assert_eq!(result.text, "START Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the new span is at the beginning
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 6);
        assert_eq!(result.spans[0].style, new_style);

        // Check that existing spans are shifted
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 11);
    }

    #[test]
    fn test_insert_at_end() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert(" END", 16, 16, new_style);

        assert_eq!(result.text, "Hello World Test END");
        assert_eq!(result.spans.len(), 4);

        // Check that existing spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);

        // Check that the new span is at the end
        assert_eq!(result.spans[3].begin_pos, 16);
        assert_eq!(result.spans[3].end_pos, 20);
        assert_eq!(result.spans[3].style, new_style);
    }

    #[test]
    fn test_insert_in_middle() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert(" MIDDLE", 6, 6, new_style);

        assert_eq!(result.text, "Hello  MIDDLEWorld Test");
        assert_eq!(result.spans.len(), 4);

        // Check spans before insertion point
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);

        // Check inserted span
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 13);
        assert_eq!(result.spans[1].style, new_style);

        // Check spans after insertion point are shifted
        assert_eq!(result.spans[2].begin_pos, 13);
        assert_eq!(result.spans[2].end_pos, 18);
    }

    #[test]
    fn test_insert_with_replacement() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("REPLACEMENT", 6, 11, new_style); // Replace "World"

        assert_eq!(result.text, "Hello REPLACEMENT Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the replaced span is gone and new span is there
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 17);
        assert_eq!(result.spans[1].style, new_style);
    }

    #[test]
    fn test_insert_empty_string() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("", 6, 6, new_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3); // No new span added for empty string

        // Check that spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_insert_bounds_checking() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("OVERFLOW", 100, 100, new_style);

        assert_eq!(result.text, "Hello World TestOVERFLOW");
        assert_eq!(result.spans.len(), 4);

        // Check that the new span is at the actual end
        assert_eq!(result.spans[3].begin_pos, 16);
        assert_eq!(result.spans[3].end_pos, 24);
    }

    #[test]
    fn test_highlight_at_beginning() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(0, 3, highlight_style.into());

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the highlight span is first
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 3);
        assert_eq!(result.spans[0].style, highlight_style);

        // Check that the original span is truncated
        assert_eq!(result.spans[1].begin_pos, 3);
        assert_eq!(result.spans[1].end_pos, 5);
    }

    #[test]
    fn test_highlight_at_end() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(14, 16, highlight_style.into());

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the original span is truncated
        assert_eq!(result.spans[2].begin_pos, 12);
        assert_eq!(result.spans[2].end_pos, 14);

        // Check that the highlight span is last
        assert_eq!(result.spans[3].begin_pos, 14);
        assert_eq!(result.spans[3].end_pos, 16);
        assert_eq!(result.spans[3].style, highlight_style);
    }

    #[test]
    fn test_highlight_spanning_multiple_spans() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(3, 9, highlight_style.into()); // Spans across "Hello" and "World"

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the first span is truncated
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 3);

        // Check that the highlight span is in the middle
        assert_eq!(result.spans[1].begin_pos, 3);
        assert_eq!(result.spans[1].end_pos, 9);
        assert_eq!(result.spans[1].style, highlight_style);

        // Check that the second span is truncated
        assert_eq!(result.spans[2].begin_pos, 9);
        assert_eq!(result.spans[2].end_pos, 11);
    }

    #[test]
    fn test_highlight_encompassing_span() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(4, 8, highlight_style.into()); // Encompasses part of "Hello" and space

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the original span is split
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 4);

        // Check that the highlight span is in the middle
        assert_eq!(result.spans[1].begin_pos, 4);
        assert_eq!(result.spans[1].end_pos, 8);
        assert_eq!(result.spans[1].style, highlight_style);

        // Check that the original span continues after
        assert_eq!(result.spans[2].begin_pos, 8);
        assert_eq!(result.spans[2].end_pos, 11);
    }

    #[test]
    fn test_highlight_empty_range() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(5, 5, highlight_style.into());

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3); // No change in spans

        // Check that spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_highlight_bounds_checking() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(10, 100, highlight_style.into());

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the highlight goes to the end of the text
        assert_eq!(result.spans[2].begin_pos, 10);
        assert_eq!(result.spans[2].end_pos, 16);
        assert_eq!(result.spans[2].style, highlight_style);
    }

    #[test]
    fn test_highlight_partial_update_keeps_unset_channels_per_span() {
        let line = create_test_line();
        let bg = Color::Ansi {
            color: AnsiColor::Yellow,
            bold: false,
        };
        // Only the background is set: every span keeps its own foreground, so
        // the range does NOT collapse to one span.
        let update = StyleUpdate {
            bg: Some(bg),
            ..StyleUpdate::UNSET
        };
        let result = line.highlight(3, 9, update);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 6);
        // [0,3) red kept, [3,5) red over yellow, [5,6) gap defaults over
        // yellow, [6,9) green over yellow, [9,11) green kept, [12,16) kept.
        assert_eq!(
            result.spans[0].style,
            create_test_style(AnsiColor::Red, false)
        );
        assert_eq!((result.spans[1].begin_pos, result.spans[1].end_pos), (3, 5));
        assert_eq!(
            result.spans[1].style,
            Style {
                bg,
                ..create_test_style(AnsiColor::Red, false)
            }
        );
        assert_eq!((result.spans[2].begin_pos, result.spans[2].end_pos), (5, 6));
        assert_eq!(
            result.spans[2].style,
            Style {
                fg: Color::DefaultForeground { bold: false },
                bg,
                ..Style::DEFAULT
            }
        );
        assert_eq!((result.spans[3].begin_pos, result.spans[3].end_pos), (6, 9));
        assert_eq!(
            result.spans[3].style,
            Style {
                bg,
                ..create_test_style(AnsiColor::Green, false)
            }
        );
        assert_eq!(
            (result.spans[4].begin_pos, result.spans[4].end_pos),
            (9, 11)
        );
        assert_eq!(
            result.spans[4].style,
            create_test_style(AnsiColor::Green, false)
        );
    }

    #[test]
    fn test_highlight_partial_attributes_merge_per_field() {
        let mut line = create_test_line();
        // Give "Hello" italics so a bold-only update must preserve them.
        line.spans[0].style.attributes.italic = true;
        let update = StyleUpdate {
            attributes: TextAttributesUpdate {
                bold: Some(true),
                ..TextAttributesUpdate::UNSET
            },
            ..StyleUpdate::UNSET
        };
        let result = line.highlight(0, 5, update);

        assert!(result.spans[0].style.attributes.bold);
        assert!(result.spans[0].style.attributes.italic);
        assert_eq!(
            result.spans[0].style.fg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            }
        );
        assert_eq!(result.spans[0].style.bg, Color::DefaultBackground);
    }

    #[test]
    fn test_highlight_unset_update_is_a_noop() {
        let line = create_test_line();
        let result = line.highlight(0, 16, StyleUpdate::UNSET);
        assert_eq!(result.spans, line.spans);
        assert_eq!(result.text, line.text);
    }

    #[test]
    fn test_line_edit_offsets_snap_to_char_boundaries() {
        // "h\u{e9}llo": boundaries at 0, 1, 3, 4, 5, 6 (the e-acute spans
        // bytes 1..3). Offset 2 splits it and must snap down to 1 everywhere
        // a script-supplied offset lands, or the first text slice panics.
        let line = StyledLine::new(
            "h\u{e9}llo",
            vec![VtSpan {
                style: create_test_style(AnsiColor::Red, false),
                begin_pos: 0,
                end_pos: 6,
            }],
        );

        let highlighted = line.highlight(0, 2, create_test_style(AnsiColor::Yellow, true).into());
        assert_eq!(
            (highlighted.spans[0].begin_pos, highlighted.spans[0].end_pos),
            (0, 1)
        );
        for span in &highlighted.spans {
            assert!(highlighted.text.is_char_boundary(span.begin_pos));
            assert!(highlighted.text.is_char_boundary(span.end_pos));
        }

        let mut linked = line.clone();
        linked.relink(
            0,
            2,
            Some(StyledLink {
                action: LinkAction::Send(std::sync::Arc::from("x")),
                tooltip: None,
                style: None,
            }),
        );
        assert_eq!((linked.links[0].begin_pos, linked.links[0].end_pos), (0, 1));

        // Both offsets snap down, so a mid-char insertion point lands before
        // the split character, and a range that collapses removes nothing.
        assert_eq!(line.insert("X", 2, 2, Style::DEFAULT).text, "hX\u{e9}llo");
        assert_eq!(line.remove(1, 2).text, "h\u{e9}llo");
        assert_eq!(line.remove(1, 3).text, "hllo");
    }

    #[test]
    fn test_remove_at_beginning() {
        let line = create_test_line();
        let result = line.remove(0, 6); // Remove "Hello "

        assert_eq!(result.text, "World Test");
        assert_eq!(result.spans.len(), 2);

        // Check that the first span is removed and others are shifted
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_at_end() {
        let line = create_test_line();
        let result = line.remove(12, 16); // Remove "Test"

        assert_eq!(result.text, "Hello World ");
        assert_eq!(result.spans.len(), 2);

        // Check that the last span is removed
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 11);
    }

    #[test]
    fn test_remove_in_middle() {
        let line = create_test_line();
        let result = line.remove(6, 12); // Remove "World "

        assert_eq!(result.text, "Hello Test");
        assert_eq!(result.spans.len(), 2);

        // Check that the middle span is removed and others are shifted
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_partial_span() {
        let line = create_test_line();
        let result = line.remove(2, 8); // Remove "llo Wo"

        assert_eq!(result.text, "Herld Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the first span is truncated
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 2);

        // Check that the second span (from "World") is truncated and shifted
        assert_eq!(result.spans[1].begin_pos, 2);
        assert_eq!(result.spans[1].end_pos, 5);

        // Check that the third span (from "Test") is shifted
        assert_eq!(result.spans[2].begin_pos, 6);
        assert_eq!(result.spans[2].end_pos, 10);
    }

    #[test]
    fn test_remove_empty_range() {
        let line = create_test_line();
        let result = line.remove(5, 5);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3);

        // Check that nothing changes
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_remove_bounds_checking() {
        let line = create_test_line();
        let result = line.remove(10, 100);

        assert_eq!(result.text, "Hello Worl");
        assert_eq!(result.spans.len(), 2);

        // Check that removal goes to the end of the text
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_entire_text() {
        let line = create_test_line();
        let result = line.remove(0, 100);

        assert_eq!(result.text, "");
        assert_eq!(result.spans.len(), 0);
    }

    /// The renderer tiles the on-screen line by concatenating `text[span]` for every
    /// span in order, so a fully-covered line's spans must reproduce its text exactly.
    /// Overlapping spans duplicate text (the on-screen corruption); gaps drop it.
    fn assert_spans_tile_text(line: &StyledLine) {
        let mut rendered = String::new();
        let mut cursor = 0;
        for span in &line.spans {
            assert!(
                span.begin_pos <= span.end_pos,
                "inverted span {:?} in {:?}",
                span,
                line.spans
            );
            assert!(
                span.begin_pos >= cursor,
                "overlapping/unsorted spans {:?}",
                line.spans
            );
            assert_eq!(
                span.begin_pos, cursor,
                "gap before span {span:?} in {:?}",
                line.spans
            );
            rendered.push_str(&line.text[span.begin_pos..span.end_pos]);
            cursor = span.end_pos;
        }
        assert_eq!(cursor, line.text.len(), "spans do not reach end of text");
        assert_eq!(
            rendered, line.text,
            "spans do not tile text: {:?}",
            line.spans
        );
    }

    #[test]
    fn from_styled_runs_tiles_and_merges() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        let line = StyledLine::from_styled_runs(
            &[
                ("plain ", red, None),
                ("", green, None), // empty runs contribute nothing
                ("more", red, None),
                (" green", green, None),
            ],
            red,
        );
        assert_eq!(line.text, "plain more green");
        // The two adjacent red runs merge into one span.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].begin_pos, 0);
        assert_eq!(line.spans[0].end_pos, 10);
        assert_eq!(line.spans[0].style, red);
        assert_eq!(line.spans[1].begin_pos, 10);
        assert_eq!(line.spans[1].end_pos, 16);
        assert_eq!(line.spans[1].style, green);
        assert_spans_tile_text(&line);
    }

    #[test]
    fn from_styled_runs_empty_matches_empty_echo() {
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::from_styled_runs(&[], style);
        let echo = StyledLine::from_echo_str("");
        assert_eq!(line.text, "");
        assert_eq!(line.spans.len(), echo.spans.len());
        assert_eq!(line.spans[0].begin_pos, 0);
        assert_eq!(line.spans[0].end_pos, 0);
        assert_eq!(line.spans[0].style, style);
    }

    #[test]
    fn from_styled_runs_non_ascii_offsets_are_bytes() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        let line = StyledLine::from_styled_runs(
            &[("caf\u{e9}", red, None), ("\u{1F600}!", green, None)],
            red,
        );
        assert_eq!(line.text, "caf\u{e9}\u{1F600}!");
        assert_eq!(line.spans[0].end_pos, 5); // "café" is 5 bytes
        assert_eq!(line.spans[1].end_pos, 10); // + 4-byte emoji + '!'
        assert_spans_tile_text(&line);
    }

    fn send_link(cmd: &str) -> LinkAction {
        LinkAction::Send(Arc::from(cmd))
    }

    #[test]
    fn authored_link_style_is_shared_across_clones_and_line_surgery() {
        let text_style = create_test_style(AnsiColor::White, false);
        let link_style = Arc::new(LinkStyle::default());
        let styled_link = StyledLink {
            action: send_link("look"),
            tooltip: None,
            style: Some(Arc::clone(&link_style)),
        };
        let line =
            StyledLine::from_linked_runs(&[("link", text_style, Some(styled_link))], text_style);

        assert!(std::mem::size_of::<Option<Arc<LinkStyle>>>() < std::mem::size_of::<LinkStyle>());
        assert!(Arc::ptr_eq(
            line.links[0].style.as_ref().expect("authored style"),
            &link_style
        ));

        let split = line.insert("-", 1, 3, text_style);
        assert_eq!(split.links.len(), 2);
        assert!(
            split
                .links
                .iter()
                .all(|link| Arc::ptr_eq(link.style.as_ref().expect("authored style"), &link_style))
        );
    }

    #[test]
    fn from_styled_runs_links_merge_across_style_boundaries() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        // One link over two differently-styled runs: 2 style spans, 1 link span.
        let line = StyledLine::from_styled_runs(
            &[
                ("go ", red, None),
                ("nor", red, Some(send_link("north"))),
                ("th", green, Some(send_link("north"))),
            ],
            red,
        );
        assert_eq!(line.text, "go north");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(
            line.links,
            vec![LinkSpan {
                begin_pos: 3,
                end_pos: 8,
                action: send_link("north"),
                tooltip: None,
                style: None,
            }]
        );
    }

    #[test]
    fn links_remap_across_insert_and_remove() {
        let style = create_test_style(AnsiColor::White, false);
        let mut line = StyledLine::from_styled_runs(
            &[
                ("a ", style, None),
                ("link", style, Some(send_link("go"))),
                (" z", style, None),
            ],
            style,
        );
        assert_eq!(
            line.links,
            vec![LinkSpan {
                begin_pos: 2,
                end_pos: 6,
                action: send_link("go"),
                tooltip: None,
                style: None,
            }]
        );

        // Insert before the link: it shifts right.
        line = line.insert("XX", 0, 0, style);
        assert_eq!(line.text, "XXa link z");
        assert_eq!(line.links[0].begin_pos, 4);
        assert_eq!(line.links[0].end_pos, 8);

        // Replace the middle of the link ("in"): head and tail survive linked, the
        // inserted bytes are link-free.
        let split = line.insert("-", 5, 7, style);
        assert_eq!(split.text, "XXa l-k z");
        assert_eq!(
            split.links,
            vec![
                LinkSpan {
                    begin_pos: 4,
                    end_pos: 5,
                    action: send_link("go"),
                    tooltip: None,
                    style: None,
                },
                LinkSpan {
                    begin_pos: 6,
                    end_pos: 7,
                    action: send_link("go"),
                    tooltip: None,
                    style: None,
                },
            ]
        );

        // Remove a range covering the whole link: it disappears.
        let gone = line.remove(3, 9);
        assert_eq!(gone.text, "XXaz");
        assert!(gone.links.is_empty());

        // A recolor leaves links untouched.
        let recolored = line.highlight(0, 10, create_test_style(AnsiColor::Red, true).into());
        assert_eq!(recolored.links, line.links);

        // Append shifts the appended line's links.
        let tail = StyledLine::from_styled_runs(&[("tail", style, Some(send_link("t")))], style);
        let joined = line.append(&tail);
        assert_eq!(joined.links.len(), 2);
        assert_eq!(joined.links[1].begin_pos, line.text.len());
        assert_eq!(joined.links[1].end_pos, line.text.len() + 4);
    }

    #[test]
    fn test_replace_midline_spans_tile_text() {
        // Regression: a single-span server line whose `line.replace` wraps a mid-line
        // term rendered `You hold <a roasted turkey le<a roasted turkey leg> roasted
        // turkey leg> high...` because the encompassing span split into overlapping
        // ranges. The text was always correct; the spans were not.
        let text = "You hold a roasted turkey leg high for everyone to see.";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let begin = text.find("a roasted turkey leg").unwrap();
        let end = begin + "a roasted turkey leg".len();
        let result = line.insert("<a roasted turkey leg>", begin, end, style);

        assert_eq!(
            result.text,
            "You hold <a roasted turkey leg> high for everyone to see."
        );
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_whole_line_tiles() {
        let text = "a roasted turkey leg";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let result = line.insert("<a roasted turkey leg>", 0, text.len(), style);

        assert_eq!(result.text, "<a roasted turkey leg>");
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_across_span_boundary_tiles() {
        // A replacement that starts inside one span and ends inside the next: the head of
        // the first span and the surviving tail of the second must both be kept (the tail
        // was previously dropped, leaving a gap that erased trailing text on screen).
        let red = create_test_style(AnsiColor::Red, false);
        let green = create_test_style(AnsiColor::Green, false);
        let yellow = create_test_style(AnsiColor::Yellow, true);
        let line = StyledLine::new(
            "HelloWorld",
            vec![
                VtSpan {
                    style: red,
                    begin_pos: 0,
                    end_pos: 5,
                },
                VtSpan {
                    style: green,
                    begin_pos: 5,
                    end_pos: 10,
                },
            ],
        );

        let result = line.insert("XX", 3, 7, yellow); // replace "loWo"

        assert_eq!(result.text, "HelXXrld");
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_shorter_than_match_tiles() {
        // Replacement shorter than the match (negative shift) must still tile.
        let text = "You hold a roasted turkey leg high.";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let begin = text.find("a roasted turkey leg").unwrap();
        let end = begin + "a roasted turkey leg".len();
        let result = line.insert("leg", begin, end, style);

        assert_eq!(result.text, "You hold leg high.");
        assert_spans_tile_text(&result);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::{
        Cow, InvisiblePolicy, LinkAction, StyledLine, escape_invisible_text, sanitize_display_text,
    };
    use std::sync::Arc;

    #[test]
    fn clean_text_borrows() {
        assert!(matches!(
            sanitize_display_text("hello\tworld"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn control_chars_are_stripped_but_tab_survives() {
        assert_eq!(sanitize_display_text("a\u{1b}[31mb\rc\td"), "a[31mbc\td");
        assert_eq!(sanitize_display_text("nul\0del\u{7f}c1\u{9b}"), "nuldelc1");
    }

    #[test]
    fn role_constructors_sanitize_and_the_span_still_tiles() {
        let line = StyledLine::from_echo_str("dirty\r\u{7f}");
        assert_eq!(line.text, "dirty");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].begin_pos, 0);
        assert_eq!(line.spans[0].end_pos, line.text.len());
    }

    #[test]
    fn invisible_policies_preserve_prose_shaping_but_disclose_action_bytes() {
        let emoji = "\u{1f469}\u{200d}\u{1f4bb}\u{fe0f}\u{e0067}\u{e0100}";
        assert!(matches!(
            escape_invisible_text(emoji, InvisiblePolicy::Prose),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            escape_invisible_text(emoji, InvisiblePolicy::ActionTarget),
            "\u{1f469}\\u{200D}\u{1f4bb}\\u{FE0F}\\u{E0067}\\u{E0100}"
        );
        assert_eq!(
            escape_invisible_text("a\u{202e}b\n", InvisiblePolicy::Prose),
            "a\\u{202E}b\\u{A}"
        );
        let directional_marks = "\u{061c}\u{200e}\u{200f}";
        assert!(matches!(
            escape_invisible_text(directional_marks, InvisiblePolicy::Prose),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            escape_invisible_text(directional_marks, InvisiblePolicy::ActionTarget),
            "\\u{61C}\\u{200E}\\u{200F}"
        );
    }

    #[test]
    fn action_target_escaping_is_injective_and_single_line() {
        let directional_override =
            escape_invisible_text("actual:\u{202e}", InvisiblePolicy::ActionTarget);
        let forged_notation =
            escape_invisible_text("actual:\\u{202E}", InvisiblePolicy::ActionTarget);
        assert_eq!(directional_override, "actual:\\u{202E}");
        assert_eq!(forged_notation, "actual:\\\\u{202E}");
        assert_ne!(directional_override, forged_notation);

        assert_eq!(
            escape_invisible_text("a\u{2028}b\u{2029}c", InvisiblePolicy::ActionTarget),
            "a\\u{2028}b\\u{2029}c"
        );
        assert!(matches!(
            escape_invisible_text("literal\\slash\u{2028}", InvisiblePolicy::Prose),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn link_action_targets_unwrap_escape_and_bound_only_tooltips() {
        let action = LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("look\u{200d}")))),
            disabled: true,
            primary_enabled: false,
            menu: None,
            menu_on_left_click: false,
            protocol: None,
        };
        assert_eq!(
            action.tooltip_target().as_deref(),
            Some("send:look\\u{200D}")
        );

        let command: Arc<str> = Arc::from("x".repeat(600));
        let action = LinkAction::Send(command.clone());
        let displayed = action.tooltip_target().expect("send target");
        let disclosure = action
            .disclosure_target_text()
            .expect("send disclosure target");
        assert_eq!(command.chars().count(), 600, "the action is not capped");
        assert_eq!(disclosure.chars().count(), 600);
        assert_eq!(displayed.chars().count(), 513);
        assert!(displayed.ends_with('\u{2026}'));
    }
}

#[cfg(test)]
mod tooltip_state_tests {
    use super::{LinkTooltipState, LinkTooltipText};
    use std::sync::Arc;

    #[test]
    fn async_tooltip_reports_loading_only_while_awaiting_resolution() {
        let state = LinkTooltipState::default();
        assert!(!state.is_loading());
        assert!(state.begin_request());
        assert!(state.is_loading());
        assert!(!state.begin_request());

        state.resolve(Some(LinkTooltipText::plain(Arc::from("ready"))));
        assert!(!state.is_loading());
        assert_eq!(
            state.text().expect("resolved tooltip").text.as_ref(),
            "ready"
        );
    }
}
