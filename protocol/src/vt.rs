use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use vtparse::{CsiParam, VTActor, VTParser};

use crate::graphics::placeholder::{
    PLACEHOLDER, PLACEHOLDER_UTF8, PlaceholderCell, RunBuilder, diacritic_number, is_combining_mark,
};
use crate::graphics::{self, GraphicsMode};
use crate::responders::unpack_dims;
use crate::sgr::{ExtendedColor, SgrExtras};
use crate::{json::deep_merge, link_url_host, sgr};
use smudgy_session_model::{
    InvisiblePolicy, LineFragments, LinkAction, LinkColor, LinkDecoration, LinkMenu, LinkMenuItem,
    LinkMenuTitle, LinkProtocol, LinkSelection, LinkSpan, LinkStyle, LinkTextStyle, LinkTooltip,
    LinkTooltipText, LinkVisibility, LinkVisibilityAction, LinkVisibilityExpire, Style, StyledLine,
    VtSpan, deceptive_invisible,
    inline_content::{Baseline, InlineObject, InlineOwner, Layer, MAX_INLINE_OBJECTS},
    push_escaped_char,
};

pub use sgr::{AnsiColor, Color};

/// The most bytes an OSC 8 URI may carry; longer links are ignored (the text
/// still displays, unlinked).
const MAX_OSC8_URI_LEN: usize = 4096;
/// The most named OSC 8 presets retained for one connection. Existing names
/// remain replaceable at the cap so a server can still restyle its presets.
const MAX_OSC8_PRESETS: usize = 1024;

/// The most Unicode placeholder cells noted on one pending line; the rest
/// stay text. A line can hold at most [`MAX_INLINE_OBJECTS`] runs anyway,
/// and this bounds what a line of nothing but placeholders costs.
const MAX_PLACEHOLDER_CELLS: usize = 4096;

/// A `U+10EEEE` printed into the pending line, with the ids its colours
/// carried at the time: the graphics image id's low 24 bits from the
/// foreground and the placement id from the underline colour.
#[derive(Clone, Copy, Debug)]
struct PlaceholderStamp {
    /// The byte position of the placeholder in `buf`.
    pos: usize,
    image_low: u32,
    placement_id: u32,
}

fn push_display_char(out: &mut String, c: char, prose: bool) {
    push_escaped_char(
        out,
        c,
        if prose {
            InvisiblePolicy::Prose
        } else {
            InvisiblePolicy::ActionTarget
        },
    );
}

#[cfg(test)]
fn sanitize_display_text(text: &str, prose: bool) -> Arc<str> {
    Arc::from(
        smudgy_session_model::escape_invisible_text(
            text,
            if prose {
                InvisiblePolicy::Prose
            } else {
                InvisiblePolicy::ActionTarget
            },
        )
        .as_ref(),
    )
}

#[derive(Default)]
struct TooltipAnsiActor {
    text: String,
    spans: Vec<VtSpan>,
    style: Style,
    span_start: usize,
    pending_cr: bool,
}

impl TooltipAnsiActor {
    fn close_span(&mut self) {
        if self.text.len() > self.span_start {
            self.spans.push(VtSpan {
                style: self.style,
                begin_pos: self.span_start,
                end_pos: self.text.len(),
            });
            self.span_start = self.text.len();
        }
    }

    fn set_style(&mut self, style: Style) {
        if style != self.style {
            self.close_span();
            self.style = style;
        }
    }

    fn push(&mut self, c: char) {
        if std::mem::take(&mut self.pending_cr) {
            self.text.push('\n');
        }
        push_display_char(&mut self.text, c, true);
    }

    fn finish(mut self) -> Option<LinkTooltipText> {
        if self.pending_cr {
            self.text.push('\n');
        }
        self.close_span();

        let begin = self.text.len() - self.text.trim_start().len();
        let end = self.text.trim_end().len();
        if begin >= end {
            return None;
        }
        let text: Arc<str> = Arc::from(&self.text[begin..end]);
        let spans: Vec<_> = self
            .spans
            .into_iter()
            .filter_map(|span| {
                let span_begin = span.begin_pos.max(begin);
                let span_end = span.end_pos.min(end);
                (span_begin < span_end).then_some(VtSpan {
                    style: span.style,
                    begin_pos: span_begin - begin,
                    end_pos: span_end - begin,
                })
            })
            .collect();
        Some(LinkTooltipText {
            text,
            spans: spans.into(),
        })
    }

    fn finish_fragment(mut self) -> StyledLine {
        if self.pending_cr {
            self.text.push('\n');
        }
        self.close_span();
        StyledLine::new(&self.text, self.spans)
    }
}

impl VTActor for TooltipAnsiActor {
    fn print(&mut self, c: char) {
        self.push(c);
    }

    fn execute_c0_or_c1(&mut self, control: u8) {
        match control {
            b'\r' => {
                if self.pending_cr {
                    self.text.push('\n');
                }
                self.pending_cr = true;
            }
            b'\n' => {
                self.pending_cr = false;
                self.text.push('\n');
            }
            _ => self.push(' '),
        }
    }

    fn dcs_hook(
        &mut self,
        _byte: u8,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
    ) {
    }

    fn dcs_put(&mut self, _byte: u8) {}

    fn dcs_unhook(&mut self) {}

    fn esc_dispatch(
        &mut self,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
        _byte: u8,
    ) {
    }

    fn csi_dispatch(&mut self, params: &[CsiParam], _parameters_truncated: bool, byte: u8) {
        if byte == b'm' {
            self.set_style(sgr::process(self.style, params));
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]]) {}

    fn apc_dispatch(&mut self, _data: Vec<u8>) {}
}

/// Parse authored tooltip copy as a safe SGR-only ANSI document. SGR updates
/// become semantic color spans; every other terminal sequence is discarded.
#[must_use]
pub fn parse_link_tooltip_text(text: &str) -> Option<LinkTooltipText> {
    let mut parser = VTParser::new();
    let mut actor = TooltipAnsiActor::default();
    for &byte in text.as_bytes() {
        parser.parse_byte(byte, &mut actor);
    }
    actor.finish()
}

/// Parses an editor or tester fragment as terminal text with SGR styles.
///
/// This function does not connect the fragment to a session. Trigger previews
/// with color filters use the same style representation as live socket input.
#[must_use]
pub fn parse_ansi_fragment(text: &str) -> StyledLine {
    let mut parser = VTParser::new();
    let mut actor = TooltipAnsiActor::default();
    for &byte in text.as_bytes() {
        parser.parse_byte(byte, &mut actor);
    }
    actor.finish_fragment()
}

/// Map an OSC 8 URI to its click action. The scheme allowlist is the trust
/// boundary: `http`/`https` open the browser (behind the per-server confirm),
/// a `send:` URI sends its percent-decoded command (same gate), and anything
/// else — `file:`, `javascript:`, unknown schemes — yields no link at all.
#[derive(Debug)]
struct ParsedLink {
    action: LinkAction,
    tooltip: LinkTooltip,
    style: Option<Arc<LinkStyle>>,
}

#[derive(Default)]
struct Osc8Config {
    tooltip: Option<LinkTooltipText>,
    disabled: bool,
    menu: Option<LinkMenu>,
    style: Option<Arc<LinkStyle>>,
    protocol: Option<LinkProtocol>,
}

fn link_color(value: &serde_json::Value) -> Option<LinkColor> {
    let parsed = crate::css_color::parse_css_color(value.as_str()?.trim())?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let alpha = (parsed.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(LinkColor {
        red: parsed.red,
        green: parsed.green,
        blue: parsed.blue,
        alpha,
    })
}

fn link_decoration(value: &serde_json::Value) -> Option<LinkDecoration> {
    if let Some(enabled) = value.as_bool() {
        return Some(if enabled {
            LinkDecoration::Solid
        } else {
            LinkDecoration::None
        });
    }
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "none" | "false" => Some(LinkDecoration::None),
        "solid" | "single" | "true" => Some(LinkDecoration::Solid),
        "double" => Some(LinkDecoration::Double),
        "dotted" => Some(LinkDecoration::Dotted),
        "dashed" => Some(LinkDecoration::Dashed),
        "wavy" => Some(LinkDecoration::Wavy),
        _ => None,
    }
}

fn style_field<'a>(
    style: &'a serde_json::Map<String, serde_json::Value>,
    full: &str,
    compact: &str,
) -> Option<&'a serde_json::Value> {
    style.get(full).or_else(|| style.get(compact))
}

fn link_background(value: &serde_json::Map<String, serde_json::Value>) -> Option<LinkColor> {
    // Mudlet accepts both its compact `bg` spelling and the full CSS property.
    // The full spelling is parsed last upstream, so it wins when both appear.
    value
        .get("background-color")
        .or_else(|| value.get("bg"))
        .and_then(link_color)
}

fn parse_link_text_style(value: &serde_json::Value) -> Option<LinkTextStyle> {
    let style = value.as_object()?;
    Some(LinkTextStyle {
        foreground: style_field(style, "color", "c").and_then(link_color),
        background: link_background(style),
        bold: style_field(style, "bold", "b").and_then(serde_json::Value::as_bool),
        italic: style_field(style, "italic", "i").and_then(serde_json::Value::as_bool),
        underline: style_field(style, "underline", "u").and_then(link_decoration),
        overline: style_field(style, "overline", "o").and_then(link_decoration),
        strikethrough: style_field(style, "strikethrough", "st").and_then(link_decoration),
        decoration_color: style_field(style, "text-decoration-color", "tdc").and_then(link_color),
    })
}

fn parse_link_style(value: &serde_json::Value) -> Option<Arc<LinkStyle>> {
    let style = value.as_object()?;
    let state = |full, compact| style_field(style, full, compact).and_then(parse_link_text_style);
    Some(Arc::new(LinkStyle {
        base: parse_link_text_style(value)?,
        active: state("active", "a"),
        hover: state("hover", "h"),
        focus_visible: state("focus-visible", "fv"),
        focus: state("focus", "f"),
        visited: state("visited", "vi"),
        selected: state("selected", "sl"),
        disabled: state("disabled", "d"),
        link: state("link", "l"),
        any_link: state("any-link", "al"),
    }))
}

fn parse_visibility(value: &serde_json::Value) -> Option<LinkVisibility> {
    let visibility = value.as_object()?;
    let action = match visibility.get("action")? {
        serde_json::Value::String(action) if action.eq_ignore_ascii_case("conceal") => {
            LinkVisibilityAction::Conceal
        }
        serde_json::Value::String(action) if action.eq_ignore_ascii_case("reveal") => {
            LinkVisibilityAction::Reveal
        }
        serde_json::Value::Array(actions)
            if actions.len() == 2
                && actions[0].as_str() == Some("reveal")
                && actions[1].as_str() == Some("conceal") =>
        {
            LinkVisibilityAction::RevealThenConceal
        }
        _ => return None,
    };
    let expire = visibility
        .get("expire")
        .and_then(serde_json::Value::as_object);
    let expire_bool = |name| {
        expire
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    Some(LinkVisibility {
        action,
        delay_ms: visibility.get("delay").and_then(serde_json::Value::as_u64),
        expire: LinkVisibilityExpire {
            input: expire_bool("input"),
            prompt: expire_bool("prompt"),
            output: expire_bool("output"),
            output_delay_ms: expire
                .and_then(|value| value.get("outputDelay"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(500),
        },
        whole_line: visibility
            .get("wholeline")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_selection(value: &serde_json::Value) -> Option<LinkSelection> {
    let selection = value.as_object()?;
    let group = sanitize_single_line_text(selection.get("group")?.as_str()?)?;
    let value = sanitize_single_line_text(selection.get("value")?.as_str()?)?;
    let flag = |name, default| {
        selection
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    };
    Some(LinkSelection {
        group,
        value,
        toggle: flag("toggle", true),
        selected: flag("selected", false),
        exclusive: flag("exclusive", true),
        disabled: flag("disabled", false),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc8QueryFieldKind {
    Config,
    Preset,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Osc8QueryField<'a> {
    raw: &'a str,
    kind: Osc8QueryFieldKind,
    value: Option<&'a str>,
}

fn encoded_equals_at(value: &str) -> Option<usize> {
    value.as_bytes().windows(3).position(|window| {
        window[0] == b'%' && window[1] == b'3' && matches!(window[2], b'd' | b'D')
    })
}

fn osc8_query_field(raw: &str) -> Osc8QueryField<'_> {
    let literal = raw.find('=').map(|index| (index, 1));
    let encoded = encoded_equals_at(raw).map(|index| (index, 3));
    let separator = match (literal, encoded) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (left, right) => left.or(right),
    };
    let (key, value) = separator.map_or((raw, None), |(index, width)| {
        (&raw[..index], Some(&raw[index + width..]))
    });
    let kind = match key {
        "config" => Osc8QueryFieldKind::Config,
        "preset" => Osc8QueryFieldKind::Preset,
        _ => Osc8QueryFieldKind::Other,
    };
    Osc8QueryField { raw, kind, value }
}

/// Return the byte length of one raw JSON object, including its closing brace.
/// OSC 8 examples commonly leave JSON unescaped, so `#` colors and `&` inside
/// strings must not be mistaken for URI delimiters.
fn raw_json_object_len(value: &str) -> Option<usize> {
    if !value.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn osc8_query_fields(query: &str) -> (Vec<Osc8QueryField<'_>>, Option<&str>) {
    let mut fields = Vec::new();
    let mut cursor = 0usize;
    let bytes = query.as_bytes();
    let mut fragment = None;

    while cursor < query.len() {
        if bytes[cursor] == b'#' {
            fragment = Some(&query[cursor + 1..]);
            break;
        }

        let provisional_end = query[cursor..]
            .bytes()
            .position(|byte| matches!(byte, b'&' | b'#'))
            .map_or(query.len(), |offset| cursor + offset);
        let probe = osc8_query_field(&query[cursor..provisional_end]);
        let mut field_end = provisional_end;

        if probe.kind == Osc8QueryFieldKind::Config
            && let Some(value) = probe.value
            && value.starts_with('{')
        {
            let value_start = cursor + probe.raw.len() - value.len();
            field_end = raw_json_object_len(&query[value_start..])
                .map_or(query.len(), |length| value_start + length);
            if field_end < query.len() && !matches!(bytes[field_end], b'&' | b'#') {
                field_end = query[field_end..]
                    .bytes()
                    .position(|byte| matches!(byte, b'&' | b'#'))
                    .map_or(query.len(), |offset| field_end + offset);
            }
        }

        fields.push(osc8_query_field(&query[cursor..field_end]));
        if field_end == query.len() {
            break;
        }
        match bytes[field_end] {
            b'&' => cursor = field_end + 1,
            b'#' => {
                fragment = Some(&query[field_end + 1..]);
                break;
            }
            _ => unreachable!("query scanner stops only at ASCII delimiters"),
        }
    }

    (fields, fragment)
}

/// Extract the first valid Mudlet-compatible `config` tooltip and remove all
/// reserved query fields from the actionable target. Smudgy advertises
/// `OSC_HYPERLINKS_TOOLTIP`, so the extension specification reserves this key;
/// an ordinary URL can retain one by percent-encoding the key itself.
fn osc8_config(uri: &str, presets: &HashMap<String, serde_json::Value>) -> (String, Osc8Config) {
    let Some((base, query)) = uri.split_once('?') else {
        return (uri.to_string(), Osc8Config::default());
    };
    let (fields, fragment) = osc8_query_fields(query);
    if !fields
        .iter()
        .any(|field| field.kind != Osc8QueryFieldKind::Other)
    {
        return (uri.to_string(), Osc8Config::default());
    }
    let override_config = fields.iter().find_map(|field| {
        (field.kind == Osc8QueryFieldKind::Config).then_some(())?;
        let decoded = percent_decode(field.value?);
        serde_json::from_str::<serde_json::Value>(&decoded).ok()
    });
    let preset = fields.iter().find_map(|field| {
        (field.kind == Osc8QueryFieldKind::Preset).then_some(())?;
        let name = percent_decode(field.value?);
        presets.get(&name).cloned()
    });
    let mut config = preset;
    if let Some(higher) = override_config {
        match &mut config {
            Some(base) => deep_merge(base, higher),
            None => config = Some(higher),
        }
    }

    let kept: Vec<_> = fields
        .iter()
        .filter(|field| field.kind == Osc8QueryFieldKind::Other)
        .map(|field| field.raw)
        .collect();
    let mut target = String::with_capacity(uri.len());
    target.push_str(base);
    if !kept.is_empty() {
        target.push('?');
        target.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        target.push('#');
        target.push_str(fragment);
    }
    let Some(config) = config else {
        return (target, Osc8Config::default());
    };
    let tooltip = config
        .get("tooltip")
        .or_else(|| config.get("t"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_link_tooltip_text);
    let disabled = config
        .get("disabled")
        .or_else(|| config.get("d"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let title = config
        .get("title")
        .or_else(|| config.get("ti"))
        .and_then(menu_title);
    let menu = config
        .get("menu")
        .or_else(|| config.get("m"))
        .and_then(|menu| parse_menu(menu, title));
    let style = config
        .get("style")
        .or_else(|| config.get("s"))
        .and_then(parse_link_style);
    let visibility = config
        .get("visibility")
        .or_else(|| config.get("v"))
        .and_then(parse_visibility);
    let selection = config
        .get("selection")
        .or_else(|| config.get("sel"))
        .and_then(parse_selection);
    let spoiler = config
        .get("spoiler")
        .or_else(|| config.get("sp"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let protocol =
        (visibility.is_some() || selection.is_some() || spoiler).then_some(LinkProtocol {
            visibility,
            selection,
            spoiler,
        });
    (
        target,
        Osc8Config {
            tooltip,
            disabled,
            menu,
            style,
            protocol,
        },
    )
}

fn menu_title(value: &serde_json::Value) -> Option<LinkMenuTitle> {
    let text = value
        .as_str()
        .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
        .and_then(sanitize_single_line_text)?;
    let style = value
        .get("style")
        .or_else(|| value.get("s"))
        .and_then(parse_link_text_style);
    Some(LinkMenuTitle { text, style })
}

fn parse_menu(value: &serde_json::Value, title: Option<LinkMenuTitle>) -> Option<LinkMenu> {
    let mut items = Vec::new();
    for value in value.as_array()? {
        if value.as_str() == Some("-") {
            items.push(LinkMenuItem::Separator);
            continue;
        }
        let Some((label, uri)) = value.as_object().and_then(|item| {
            item.iter()
                .find_map(|(label, uri)| uri.as_str().map(|uri| (label, uri)))
        }) else {
            continue;
        };
        let Some(label) = sanitize_single_line_text(label) else {
            continue;
        };
        let Some(action) = server_action_for_uri(uri) else {
            continue;
        };
        items.push(LinkMenuItem::Action { label, action });
    }
    (!items.is_empty()).then(|| LinkMenu {
        title,
        items: items.into(),
    })
}

/// Menu chrome stays one-line even though tooltip prose may be multiline.
fn sanitize_single_line_text(text: &str) -> Option<Arc<str>> {
    let mut cleaned = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            cleaned.push(' ');
        } else {
            push_display_char(&mut cleaned, c, true);
        }
    }
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| Arc::from(cleaned))
}

fn starts_with_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn server_action_for_uri(uri: &str) -> Option<LinkAction> {
    if link_url_host(uri).is_some() {
        return Some(LinkAction::OpenUrl(Arc::from(uri)));
    }
    if starts_with_ascii_case_insensitive(uri, "send:") {
        let command = percent_decode(&uri[5..]);
        return (!command.is_empty()).then(|| LinkAction::ServerSend(Arc::from(command)));
    }
    if starts_with_ascii_case_insensitive(uri, "prompt:") {
        return Some(LinkAction::Prompt(Arc::from(percent_decode(&uri[7..]))));
    }
    None
}

fn link_action_for_uri(
    uri: &str,
    presets: &HashMap<String, serde_json::Value>,
) -> Option<ParsedLink> {
    if uri.len() > MAX_OSC8_URI_LEN {
        log::debug!("OSC 8 URI over {MAX_OSC8_URI_LEN} bytes ignored");
        return None;
    }
    let (target, config) = osc8_config(uri, presets);
    let Some(primary) = server_action_for_uri(&target) else {
        log::debug!("OSC 8 URI with unsupported scheme ignored");
        return None;
    };
    let display_target = primary.tooltip_target();
    let tooltip = if config
        .protocol
        .as_ref()
        .is_some_and(|protocol| protocol.spoiler)
    {
        LinkTooltip::text(Arc::from("Click to reveal"), display_target.clone())
    } else if let Some(text) = config.tooltip {
        LinkTooltip::styled_text(text, display_target.clone())
    } else if config.menu.is_some() && !config.disabled {
        LinkTooltip::text(Arc::from("Right-click for menu"), display_target.clone())
    } else {
        LinkTooltip::text(display_target.clone()?, None)
    };
    let action = if config.disabled || config.menu.is_some() || config.protocol.is_some() {
        LinkAction::Configured {
            primary: Some(Box::new(primary)),
            disabled: config.disabled,
            primary_enabled: true,
            menu: config.menu,
            menu_on_left_click: false,
            protocol: config.protocol,
        }
    } else {
        primary
    };
    Some(ParsedLink {
        action,
        tooltip,
        style: config.style,
    })
}

/// Decode `%XX` escapes (RFC 3986); anything malformed passes through
/// verbatim. `+` is not space — that is form encoding, not URI encoding.
fn percent_decode(s: &str) -> String {
    fn hex_pair(bytes: &[u8]) -> Option<u8> {
        let hi = (*bytes.first()? as char).to_digit(16)?;
        let lo = (*bytes.get(1)? as char).to_digit(16)?;
        u8::try_from(hi * 16 + lo).ok()
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = bytes.get(i + 1..).and_then(hex_pair)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ordered terminal events emitted by the shared VT parser. Hosts decide how
/// to route them; the parser never depends on a native runtime or browser API.
#[derive(Clone, Debug)]
pub enum VtEvent {
    HandleIncomingLine(Arc<StyledLine>),
    HandleIncomingFragmentedLine {
        line: Arc<StyledLine>,
        completion_fragment: Arc<StyledLine>,
    },
    HandleIncomingPartialLine(Arc<StyledLine>),
    PromptBoundary,
    RetractIncomingPartialLine,
    RequestRepaint,
    IncomingPacketProcessed {
        connection_generation: u64,
        has_displayable_text: bool,
    },
}

/// Static event dispatch keeps the native parser hot path monomorphized.
pub trait VtSink: std::fmt::Debug {
    fn emit(&mut self, event: VtEvent);
}

/// The raw capture's state. One byte, so the per-byte raw push tests one
/// value for its common path (capture on, nothing pending).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawCapture {
    /// Nothing of the wire bytes is kept; `buf_raw` stays empty.
    Off,
    /// Every byte joins `buf_raw`.
    On,
    /// On, and a graphics APC was just cut out of `buf_raw`, leaving the
    /// `ESC` that ended it as the buffer's last byte; the next raw byte
    /// decides whether it goes too (a `\` completing `ESC \`, or anything
    /// superseding it) or begins the next sequence (see
    /// [`VtProcessor::settle_kept_esc`]). That byte is always on the
    /// per-byte path, since the `ESC` leaves the parser in its escape state,
    /// and it always arrives with capture on: a fall clears this with the
    /// buffer ([`VtProcessor::drop_raw`]), so the state is never stale.
    KeptEsc,
}

impl RawCapture {
    /// Whether wire bytes are being captured at all.
    fn is_on(self) -> bool {
        self != Self::Off
    }

    /// Capture rising, or staying on: a kept-back `ESC` keeps its verdict
    /// pending.
    fn on(self) -> Self {
        if self == Self::Off { Self::On } else { self }
    }
}

/// Laid out by hand (`repr(C)`): the fields the per-byte, per-run and
/// per-line paths touch come first, in the struct's first cache lines,
/// whatever graphics and link state the connection accumulates after them.
#[allow(clippy::struct_excessive_bools)] // Independent terminal capture and protocol flags.
#[derive(Debug)]
#[repr(C)]
pub struct VtProcessor<S: VtSink> {
    // --- Read for every byte, run or line. ---
    /// Whether wire bytes are captured into `buf_raw` (and whether a
    /// kept-back `ESC` awaits a verdict, see [`RawCapture`]). This is the
    /// `raw_wanted` value latched for the lines being accumulated: it FALLS
    /// at line boundaries but RISES only at read-batch boundaries, so an
    /// emitted line's raw form is always complete-or-absent — never a torn
    /// suffix — even though the flag flips concurrently with an in-flight
    /// parse run (see [`Self::refresh_capture_raw`]).
    capture: RawCapture,
    /// Whether `buf_raw` may hold bytes that are not valid UTF-8. Runs from the Ground-state
    /// bulk path arrive validated (or transcoder-decoded), so only the per-byte path can
    /// introduce doubt, and only with a high byte: an invalid sequence, or a valid one the
    /// state machine consumed itself (an OSC payload, an encoded C1). While this is false the
    /// committed line's raw text is taken from the buffer without a validation pass.
    raw_unverified: bool,
    /// Whether the graphics state holds a virtual placement, cached after
    /// every graphics command and mode change: only then can a `U+10EEEE`
    /// mean anything, and only then does the print path note one.
    placeholders_armed: bool,
    /// Whether the pending line holds graphics: placeholder cells stamped
    /// into it (`placeholder_cells`) or objects attached to it
    /// (`pending_objects`). The one flag the per-line path tests; the
    /// vectors it summarises are read only while it is set.
    line_has_graphics: bool,
    /// Whether the current read batch produced any non-empty terminal text.
    /// Telnet-only traffic and empty line terminators do not qualify.
    packet_has_displayable_text: bool,
    /// How this connection treats kitty graphics commands and the terminal
    /// queries clients send to detect them (DA1, pixel-size reports). `Off`
    /// for processors built without a server configuration (tests, benches,
    /// the web engine), so nothing is ever written back unasked.
    graphics_mode: GraphicsMode,
    link_preset_overflow_logged: bool,
    /// The SGR state the cursor style does not carry: the underline colour
    /// and whether the foreground was last set by palette index, which a
    /// Unicode placeholder cell encodes its graphics ids in.
    sgr_extras: SgrExtras,
    cursor_style: Style,
    /// A bare `\r` arrived: the next printable character overwrites the open
    /// line (carriage-return overprint — progress bars, spinners) instead of
    /// appending to it. Holds `buf_raw`'s length at the `\r`, so the restart
    /// can drop exactly the superseded frame's raw bytes while keeping any
    /// that arrived after it (escape sequences, the new frame's first char —
    /// the connection pushes raw bytes before the parser sees them). `\n`
    /// clears it, so CRLF stays an ordinary commit. Persists across reads
    /// like the rest of the parse state.
    pending_cr: Option<usize>,
    buf: String,
    buf_raw: Vec<u8>,
    span_info: Vec<VtSpan>,
    sink: S,
    // --- Read once per line, or more rarely. ---
    /// The active OSC 8 link, applied to every character until `ESC]8;;`.
    /// Like the cursor style, it survives line commits (a multi-line link)
    /// and carriage-return overprints.
    cursor_link: Option<ParsedLink>,
    /// Where in `buf` the active link's current range began.
    link_open_pos: usize,
    /// Completed link ranges of the pending line (closed OSC 8 links).
    link_info: Vec<LinkSpan>,
    /// How many lines this processor has committed: the sequence number of
    /// the pending line, stamped on the placements made on it, and the
    /// bottom of the screen positional graphics deletes are read against.
    line_seq: u64,
    /// Characters of the open logical line already flushed as partials
    /// (`buf` holds only the fragment since): the cursor's column is this
    /// many past the buffer.
    flushed_columns: usize,
    /// A line prefix emitted only because a socket read batch ended. This is
    /// deliberately separate from `buf`: the UI gets prompt-like immediacy,
    /// while a later newline still presents the assembled logical line to
    /// normal triggers.
    open_logical_line: LineFragments,
    /// Identifies the connection that owns this processor. Packet-completion
    /// markers carry it back to the runtime so a replaced socket cannot
    /// release the next connection's deferred profile send.
    connection_generation: u64,
    /// Whether the runtime still wants packet-completion markers — set while
    /// a deferred profile send awaits this connection's first displayable
    /// packet, cleared when it releases (or the connection ends). Owned by
    /// the runtime on the session thread and read here from the socket
    /// runtime; `None` (tests, benches) means always emit. The marker's
    /// useful lifetime ends at the release, so gating it here keeps
    /// steady-state batches off the runtime channel entirely.
    marker_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Whether any trigger currently carries a raw pattern — the only consumer
    /// of `StyledLine::raw`. Owned by the trigger manager on the session thread
    /// and read here from the socket runtime; `None` (tests, benches) means
    /// always capture.
    raw_wanted: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Objects the processor itself attached to the pending line — a
    /// graphics placement lands at the cursor as an inline object over
    /// placeholder columns. They leave with the text they sit in, so every
    /// partial, fragment and join carries them with the identity minted here.
    pending_objects: Vec<InlineObject>,
    /// The placeholder cells printed into the pending line, in order,
    /// stamped with their ids as they were printed; turned into runs when
    /// the line is built, cleared with an overprint.
    placeholder_cells: Vec<PlaceholderStamp>,
    /// Where in `buf_raw` the APC string now open began (its `ESC _`), from
    /// the parser's [`VTActor::apc_start`] until the string ends. A batch
    /// ending inside the string holds the bytes from here back for the next
    /// fragment, since a graphics string is cut out of the capture only
    /// when it ends; `0` once the string has straddled a boundary (or began
    /// before capture rose). `None` outside an APC.
    apc_raw_start: Option<usize>,
    // --- Graphics and link configuration. ---
    /// What this processor owes the server — query answers and the graphics
    /// protocol's replies — in command order, with slots parked for replies
    /// still waiting on a decode. The telnet bridge drains the ready head
    /// into its reply buffer after every feed, so answers reach the wire in
    /// stream order with the negotiation answers.
    replies: graphics::ReplyQueue,
    /// Decodes pixels and reads local media for graphics commands. `None`
    /// (tests, benches, the web engine, a host without graphics) parses
    /// graphics commands and drops them.
    graphics_host: Option<Box<dyn graphics::GraphicsHost>>,
    /// The main pane's character grid, packed with [`crate::responders::pack_dims`]
    /// and shared with the runtime, which updates it from UI reports. `None`
    /// (tests) reports the default grid.
    window_size: Option<Arc<std::sync::atomic::AtomicU32>>,
    /// The main pane's cell size, packed the same way, in fixed point
    /// ([`graphics::CELL_FIXED_SCALE`] units to the logical pixel). Zero
    /// until the UI has measured a font, which the size reports pass on as
    /// zero, exactly as terminals without pixel information do.
    cell_size: Option<Arc<std::sync::atomic::AtomicU32>>,
    /// The image table and the graphics command state machine, created by
    /// the first graphics command a displaying, hosted connection sees. A
    /// session that never receives one never allocates it.
    graphics: Option<Box<graphics::GraphicsState>>,
    /// Mudlet-compatible OSC 8 presets live for exactly one connection/
    /// processor lifetime and are never shared across sessions.
    link_presets: HashMap<String, serde_json::Value>,
    /// Test instrumentation: raw bytes moved to keep held-back bytes at the
    /// head of the buffer across flushes. A boundary that flushes nothing
    /// moves nothing, which is what keeps an APC open across many read
    /// batches linear; the test pins that by this count, not by timing.
    #[cfg(test)]
    raw_bytes_moved: usize,
}

const INPUT_BUFFER_CAPACITY: usize = 1024;

/// Our DA1 answer: a VT220-class terminal with no extensions claimed. The
/// graphics protocol's detection recipe sends its query and then DA1, and
/// treats a DA1 answer with no graphics answer as "unsupported", so this
/// must arrive whenever the protocol is on, and never when it is off.
const DA1_REPLY: &[u8] = b"\x1b[?62;c";

impl<S: VtSink> VtProcessor<S> {
    /// Access host-owned event storage after a parse run.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    #[must_use]
    pub fn new<T: Into<S>>(sink: T) -> Self {
        Self::with_connection_generation(sink, 0)
    }

    #[must_use]
    pub fn with_connection_generation<T: Into<S>>(sink: T, connection_generation: u64) -> Self {
        VtProcessor {
            cursor_style: Style::default(),
            buf: String::with_capacity(INPUT_BUFFER_CAPACITY),
            buf_raw: Vec::with_capacity(INPUT_BUFFER_CAPACITY),
            span_info: Vec::new(),
            link_info: Vec::new(),
            cursor_link: None,
            link_open_pos: 0,
            pending_cr: None,
            open_logical_line: LineFragments::None,
            sink: sink.into(),
            connection_generation,
            packet_has_displayable_text: false,
            marker_armed: None,
            raw_wanted: None,
            capture: RawCapture::On,
            raw_unverified: false,
            link_presets: HashMap::new(),
            link_preset_overflow_logged: false,
            graphics_mode: GraphicsMode::Off,
            replies: graphics::ReplyQueue::default(),
            graphics_host: None,
            window_size: None,
            cell_size: None,
            apc_raw_start: None,
            #[cfg(test)]
            raw_bytes_moved: 0,
            pending_objects: Vec::new(),
            graphics: None,
            sgr_extras: SgrExtras::default(),
            placeholders_armed: false,
            line_has_graphics: false,
            placeholder_cells: Vec::new(),
            line_seq: 0,
            flushed_columns: 0,
        }
    }

    /// Set how graphics commands and terminal queries are treated. The
    /// connection seeds this from the server configuration at connect time.
    /// Changed on a live processor: a mode that does not reply silences
    /// every answer still owed (bytes not yet written are dropped, and a
    /// reply parked for a decode resolves to nothing when it lands, so what
    /// queues behind it afterwards still flows), and a mode that does not
    /// display abandons any upload in flight, whose later chunks will be
    /// dropped unseen. The image table stays: placements already made keep
    /// drawing, a decode still in flight lands in its record, and a switch
    /// back finds every image where it was.
    pub fn set_graphics_mode(&mut self, mode: GraphicsMode) {
        if !mode.replies() {
            self.replies.silence();
        }
        if !mode.displays()
            && let Some(state) = self.graphics.as_deref_mut()
        {
            state.abandon_upload();
        }
        self.graphics_mode = mode;
        self.refresh_placeholders_armed();
    }

    /// Re-read whether Unicode placeholders can mean anything: a virtual
    /// placement exists and this connection displays graphics.
    fn refresh_placeholders_armed(&mut self) {
        self.placeholders_armed = self.graphics_mode.displays()
            && self
                .graphics
                .as_deref()
                .is_some_and(graphics::GraphicsState::has_virtual_placements);
    }

    /// The cursor's cell on the pending line, from zero: the characters
    /// printed so far (one cell each; wide characters are not reckoned),
    /// the fragments already flushed included.
    fn cursor_column(&self) -> u16 {
        u16::try_from(self.flushed_columns + self.buf.chars().count()).unwrap_or(u16::MAX)
    }

    #[must_use]
    pub fn graphics_mode(&self) -> GraphicsMode {
        self.graphics_mode
    }

    /// Share the runtime's grid and cell-size cells so size reports answer
    /// with what the UI last measured.
    pub fn set_terminal_geometry(
        &mut self,
        window_size: Arc<std::sync::atomic::AtomicU32>,
        cell_size: Arc<std::sync::atomic::AtomicU32>,
    ) {
        self.window_size = Some(window_size);
        self.cell_size = Some(cell_size);
    }

    /// Give this processor its graphics host. Transmits are decoded through
    /// it; without one every graphics command is parsed (for the raw
    /// capture) and dropped.
    pub fn set_graphics_host(&mut self, host: Box<dyn graphics::GraphicsHost>) {
        self.graphics_host = Some(host);
    }

    /// Run `run` against the graphics state (created on first use) when this
    /// connection displays graphics and has a host to decode them; `None`,
    /// running nothing, otherwise — the web engine, tests and benches parse
    /// graphics APCs only to keep them out of the raw capture. The command
    /// acts at the cursor, whose column is counted here (the pending line's
    /// characters) once per command.
    fn with_graphics<R>(
        &mut self,
        run: impl FnOnce(&mut graphics::GraphicsState, &mut graphics::GraphicsEnv<'_>) -> R,
    ) -> Option<R> {
        if !self.graphics_mode.displays() {
            return None;
        }
        let column = self.cursor_column();
        self.with_graphics_at(column, run)
    }

    /// [`Self::with_graphics`] for a command that acts at line column
    /// `column` rather than the cursor: a placeholder run, which acts at its
    /// own cells, so the cursor column is not counted for each.
    fn with_graphics_at<R>(
        &mut self,
        column: u16,
        run: impl FnOnce(&mut graphics::GraphicsState, &mut graphics::GraphicsEnv<'_>) -> R,
    ) -> Option<R> {
        if !self.graphics_mode.displays() {
            return None;
        }
        let cell = self.cell_fixed();
        let replies_enabled = self.graphics_mode.replies();
        let grid = self.grid();
        let Some(host) = self.graphics_host.as_deref_mut() else {
            log::debug!("graphics command parsed; no host to show it");
            return None;
        };
        // A pending overprint restarts the line before anything attaches
        // (`attach_inline_object`), taking the superseded frame's objects
        // with it, so the line a placement would join is empty.
        let objects_free = if self.pending_cr.is_some() {
            MAX_INLINE_OBJECTS
        } else {
            MAX_INLINE_OBJECTS.saturating_sub(self.pending_objects.len())
        };
        let mut env = graphics::GraphicsEnv {
            host,
            replies: &mut self.replies,
            cell,
            replies_enabled,
            objects_free,
            line_seq: self.line_seq,
            column,
            grid,
        };
        let state = self.graphics.get_or_insert_with(Box::default);
        Some(run(state, &mut env))
    }

    /// Run one whole graphics command and attach the placement it creates,
    /// if any, at the cursor: placeholder columns in the pending line, with
    /// text beside the image's last row — or, for an anchor (`C=1`, a
    /// negative z-index), an empty range at the cursor with the box as its
    /// outset, drawn under the text when the z-index is negative. Kept out
    /// of line and in the cold section: it is rare and large, and the
    /// callbacks around it are hot.
    #[cold]
    #[inline(never)]
    fn graphics_command(&mut self, control: graphics::ControlData, payload: &[u8]) {
        let outcome = self.with_graphics(|state, env| {
            let placement = state.command(env, control, payload);
            (placement, state.take_dirty())
        });
        self.refresh_placeholders_armed();
        let Some((placement, dirty)) = outcome else {
            return;
        };
        // An animation control or a frame delete changes what is drawn
        // with no decode to announce it.
        if dirty {
            self.sink.emit(VtEvent::RequestRepaint);
        }
        let Some(placement) = placement else {
            return;
        };
        let geometry = placement.geometry;
        let mut object = InlineObject::new(0..0, placement.clone(), InlineOwner::default());
        object.z = geometry.z;
        let columns = if placement.anchored {
            object.baseline = Baseline::Centered;
            object.paint_outset = graphics::anchor_outset(&geometry, self.cell_fixed());
            object.layer = if geometry.z < 0 {
                Layer::Under
            } else {
                Layer::Over
            };
            0
        } else {
            object.baseline = Baseline::LastRow;
            usize::from(geometry.columns)
        };
        // The state refused the placement already when the line was full
        // (`GraphicsEnv::objects_free`), so this attach cannot fail.
        if self.attach_object(columns, object).is_none() {
            log::debug!("graphics placement not shown: its line filled under it");
        }
    }

    /// Note a `U+10EEEE` about to be printed at byte `pos` of the pending
    /// line, with the ids its colours carry now: the image id's low 24 bits
    /// from the foreground (a palette index as itself, a direct colour as
    /// its 24 bits, a named colour as its index), the placement id from the
    /// underline colour likewise. Off the hot path: only a placeholder
    /// while a virtual placement exists gets here.
    #[cold]
    #[inline(never)]
    fn stamp_placeholder(&mut self, pos: usize) {
        if self.placeholder_cells.len() >= MAX_PLACEHOLDER_CELLS {
            return;
        }
        let rgb = |r: u8, g: u8, b: u8| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        let image_low = match self.sgr_extras.fg_index {
            Some(index) => u32::from(index),
            None => match self.cursor_style.fg {
                Color::Rgb { r, g, b } => rgb(r, g, b),
                Color::Ansi { color, bold } => u32::from(sgr::ansi_index(color, bold)),
                _ => 0,
            },
        };
        let placement_id = match self.sgr_extras.underline_color {
            Some(ExtendedColor::Indexed(index)) => u32::from(index),
            Some(ExtendedColor::Rgb { r, g, b }) => rgb(r, g, b),
            None => 0,
        };
        self.placeholder_cells.push(PlaceholderStamp {
            pos,
            image_low,
            placement_id,
        });
        self.line_has_graphics = true;
    }

    /// Note every placeholder in a run about to join the pending line at its
    /// end. Called only for a run with non-ASCII bytes while placeholders
    /// are armed; a placeholder's lead byte is rare enough in other text
    /// that the scan is a `memchr` and a four-byte compare per hit.
    fn stamp_placeholders_in(&mut self, run: &str) {
        let base = self.buf.len();
        let bytes = run.as_bytes();
        for at in memchr::memchr_iter(PLACEHOLDER_UTF8[0], bytes) {
            if bytes.get(at..at + PLACEHOLDER_UTF8.len()) == Some(&PLACEHOLDER_UTF8[..]) {
                self.stamp_placeholder(base + at);
            }
        }
    }

    /// Turn the placeholder cells of the pending line into runs and attach
    /// a placement over each one that names a virtual placement this
    /// connection holds (see [`graphics::placeholder`]). Runs that name
    /// nothing, or that the line has no room for, stay text. Called once
    /// per line that printed a placeholder while armed, before the line is
    /// built, so a partial flush splits a run at the batch boundary (each
    /// part is a run of its own, showing its own cells).
    #[cold]
    #[inline(never)]
    fn attach_placeholder_runs(&mut self) {
        let stamps = std::mem::take(&mut self.placeholder_cells);
        if !self.placeholders_armed {
            return;
        }
        let mut builder = RunBuilder::default();
        let mut runs = Vec::new();
        let (mut cursor, mut column) = (0, self.flushed_columns);
        for stamp in &stamps {
            // A stamp names where a placeholder was printed; anything that
            // rewrote the line since (an escaped invisible inside a link)
            // leaves it naming other text, which is skipped.
            if stamp.pos < cursor
                || !self.buf.is_char_boundary(stamp.pos)
                || !self.buf[stamp.pos..].starts_with(PLACEHOLDER)
            {
                continue;
            }
            column += self.buf[cursor..stamp.pos].chars().count();
            let cell = self.placeholder_cell(stamp, column);
            cursor = cell.range.end;
            column += 1;
            runs.extend(builder.push(cell));
        }
        runs.extend(builder.finish());
        let mut attached = false;
        for run in &runs {
            // The run acts at its own first cell; `placeholder_run` never
            // reads the column, so the cursor's is not counted for each run.
            let at = u16::try_from(run.first_cell_column).unwrap_or(u16::MAX);
            let placed = self.with_graphics_at(at, |state, env| state.placeholder_run(run, env));
            if let Some(Some((placement, range))) = placed {
                let mut object = InlineObject::new(range, placement, InlineOwner::default());
                object.baseline = Baseline::Centered;
                attached |= self.attach_over(object).is_some();
            }
        }
        if attached {
            // Objects are kept in line order; a run can start before an
            // object attached at the cursor earlier.
            self.pending_objects
                .sort_by_key(|object| object.range.start);
        }
    }

    /// Read the placeholder cell at a stamp: the placeholder and the
    /// combining marks after it, which kitty keeps in the cell with it and
    /// reads as the row, the column and the id's high byte (the first
    /// three; a mark outside the diacritic table means "unknown", like a
    /// missing one, but is the cell's all the same, so the next placeholder
    /// stays adjacent). Whose bytes the cell covers.
    fn placeholder_cell(&self, stamp: &PlaceholderStamp, column: usize) -> PlaceholderCell {
        let start = stamp.pos;
        let mut end = start + PLACEHOLDER.len_utf8();
        let mut values = [None; 3];
        for (index, c) in self.buf[end..].chars().enumerate() {
            if !is_combining_mark(c) {
                break;
            }
            if let Some(slot) = values.get_mut(index) {
                *slot = diacritic_number(c);
            }
            end += c.len_utf8();
        }
        PlaceholderCell {
            range: start..end,
            column,
            image_low: stamp.image_low,
            placement_id: stamp.placement_id,
            row: values[0],
            col: values[1],
            msb: values[2],
        }
    }

    /// A decode the host was asked for has finished: the pixels land in
    /// their record, its reply settles into its parked slot, whatever now
    /// heads the queue can be written, and the image's next frame commands
    /// (those that waited on the decode) run — through the host when one
    /// needs a decode. One repaint announces all of it. Called on the
    /// thread that drives this processor, between feeds — never inside
    /// one.
    pub fn graphics_decode_complete(
        &mut self,
        ticket: graphics::DecodeTicket,
        result: graphics::DecodeResult,
    ) {
        match (
            self.graphics.as_deref_mut(),
            self.graphics_host.as_deref_mut(),
        ) {
            (Some(state), Some(host)) => {
                state.decode_complete(host, &mut self.replies, ticket, result);
                // A control or frame delete that ran behind the decode is
                // announced by the repaint below, not left for the next
                // command to announce again.
                state.take_dirty();
            }
            _ => {
                if !self.replies.resolve(ticket, result.map(drop)) {
                    log::debug!("graphics decode {ticket:?} finished for a reply no longer parked");
                }
            }
        }
        self.sink.emit(VtEvent::RequestRepaint);
    }

    /// Move every reply that can be written now into `into`, in the order
    /// they were owed; a reply still waiting on a decode holds back those
    /// behind it.
    pub fn drain_replies(&mut self, into: &mut Vec<u8>) {
        self.replies.drain_into(into);
    }

    /// Whether a reply can be written now: one check of the queue's head,
    /// inlined into the telnet bridge's per-data-run call.
    #[inline]
    #[must_use]
    pub fn has_replies(&self) -> bool {
        self.replies.has_ready()
    }

    fn grid(&self) -> (u16, u16) {
        let (cols, rows) = self
            .window_size
            .as_ref()
            .map_or(crate::responders::DEFAULT_DIMS, |cell| {
                unpack_dims(cell.load(std::sync::atomic::Ordering::Relaxed))
            });
        (cols.max(1), rows.max(1))
    }

    /// The cell size as the UI last reported it, in fixed point
    /// ([`graphics::CELL_FIXED_SCALE`] units to the logical pixel); zero
    /// until it has.
    fn cell_fixed(&self) -> (u16, u16) {
        self.cell_size.as_ref().map_or((0, 0), |cell| {
            unpack_dims(cell.load(std::sync::atomic::Ordering::Relaxed))
        })
    }

    /// Answer the terminal queries the graphics protocol's clients rely on:
    /// DA1 (`CSI c`, `CSI 0 c`) and the XTWINOPS size reports `CSI 14 t`
    /// (text area in pixels), `CSI 16 t` (one cell in pixels) and `CSI 18 t`
    /// (text area in cells). Private-marker forms (`CSI > c`, `CSI ? c`) are
    /// other queries and stay unanswered. Everything is in logical pixels, the
    /// unit placements are sized in, so a client's arithmetic comes out right.
    ///
    /// The cell is the pane's advance (carried in fixed point, and used as
    /// such to lay placements out) truncated to whole pixels — or, before
    /// the pane has measured a font, the fallback cell placements are laid
    /// out with meanwhile, so what a client is told always matches what is
    /// drawn — and the text area is the grid times that cell rather than the
    /// pane's true pixel width: the three reports then agree exactly (`14 t`
    /// = `16 t` × `18 t`), so a client that derives the cell size from the
    /// ratio lays out the same as one that asks for it, and neither is ever
    /// told a text area wider than the real one (which is under a pixel per
    /// cell wider), so an image sized to fill it cannot overflow the grid.
    ///
    /// Out of line: queries are a handful per session, and the SGR arm
    /// beside this one runs for every escape in the stream.
    #[cold]
    #[inline(never)]
    fn answer_terminal_query(&mut self, params: &[CsiParam], byte: u8) {
        match byte {
            b'c' => {
                if params
                    .iter()
                    .all(|param| matches!(param, CsiParam::Integer(0)))
                {
                    self.replies.push(DA1_REPLY);
                }
            }
            b't' => {
                let Some(CsiParam::Integer(op)) = params.first() else {
                    return;
                };
                let (cols, rows) = self.grid();
                let (cell_w, cell_h) = graphics::cell_or_fallback(self.cell_fixed());
                let (cell_w, cell_h) = (
                    graphics::cell_whole_pixels(cell_w),
                    graphics::cell_whole_pixels(cell_h),
                );
                let report = match op {
                    14 => format!(
                        "\x1b[4;{};{}t",
                        u32::from(rows) * u32::from(cell_h),
                        u32::from(cols) * u32::from(cell_w)
                    ),
                    16 => format!("\x1b[6;{cell_h};{cell_w}t"),
                    18 => format!("\x1b[8;{rows};{cols}t"),
                    _ => return,
                };
                self.replies.push(report.as_bytes());
            }
            _ => {}
        }
    }

    /// Cut the graphics APC that just ended out of the raw capture: raw
    /// triggers match on escape codes, never on base64 image data, and a
    /// chunked image would otherwise swell one raw line by its whole size.
    /// `start` is where the string began in `buf_raw` (see
    /// [`Self::apc_raw_start`]); the string's own `ESC _ G` is searched for
    /// only if the parser never said. The connection pushes each raw byte
    /// before the parser sees it, so at dispatch the buffer ends with the
    /// byte that ended the string; everything from the start goes, and a
    /// terminator that begins the next sequence stays (see
    /// [`strip_graphics_apc`]).
    fn strip_graphics_apc_from_raw(&mut self, start: Option<usize>) {
        if self.capture.is_on() {
            let mut kept_esc = self.capture == RawCapture::KeptEsc;
            strip_graphics_apc(
                &mut self.buf_raw,
                &mut self.pending_cr,
                &mut kept_esc,
                start,
            );
            if kept_esc {
                self.capture = RawCapture::KeptEsc;
            }
        }
    }

    /// Where the raw bytes that must not flush with the pending line begin
    /// (see [`held_raw_start`]); `buf_raw.len()` when nothing is held, and
    /// always while capture is off, when the buffer is empty.
    fn held_raw_start(&self) -> usize {
        held_raw_start(&self.buf_raw, self.apc_raw_start)
    }

    /// Drop the pending line's raw bytes before `raw_end`, now flushed, and
    /// keep the rest — the bytes held back for the next fragment — at the
    /// head of the buffer, where an open APC's start now is. The held bytes
    /// move only here, and a boundary that flushes nothing never gets here:
    /// an APC that stays open across many read batches moves its bytes
    /// once, not once per batch, however long it grows.
    fn flush_raw(&mut self, raw_end: usize) {
        if raw_end >= self.buf_raw.len() {
            self.clear_raw();
        } else if raw_end > 0 {
            self.buf_raw.drain(..raw_end);
            // Doubt may have left with the flushed bytes.
            if self.raw_unverified {
                self.raw_unverified = self.buf_raw.iter().any(|&b| b >= 0x80);
            }
            #[cfg(test)]
            {
                self.raw_bytes_moved += self.buf_raw.len();
            }
        }
        if self.apc_raw_start.is_some() {
            self.apc_raw_start = Some(0);
        }
    }

    /// Capture has fallen: nothing of the wire bytes is kept from here on,
    /// the bytes held back for the next fragment included. While capture is
    /// off the buffer stays empty and no kept-back `ESC` awaits a verdict,
    /// so the next rise starts clean; an APC still open then is cut out
    /// from the rise on.
    fn drop_raw(&mut self) {
        self.clear_raw();
        if self.apc_raw_start.is_some() {
            self.apc_raw_start = Some(0);
        }
    }

    /// The generation stamped into the staleness-guarded actions this
    /// processor's connection emits — shared with the telnet bridge so
    /// protocol payloads carry the same socket identity as the packet
    /// markers.
    pub fn connection_generation(&self) -> u64 {
        self.connection_generation
    }

    /// Ties packet-completion markers to the given flag (the runtime's
    /// "a deferred profile send is pending" bit). While the flag is clear the
    /// batch boundary emits no marker; a marker skipped this way is never
    /// owed later, because the flag only rises when a new connection (with a
    /// fresh processor) installs a pending send.
    pub fn set_marker_armed_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.marker_armed = Some(flag);
    }

    /// Ties raw capture to the given flag (the trigger manager's "any raw
    /// pattern exists" bit). A drop takes effect at the next line boundary; a
    /// rise waits for the next read-batch boundary.
    pub fn set_raw_wanted_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.capture = if flag.load(std::sync::atomic::Ordering::Relaxed) {
            self.capture.on()
        } else {
            RawCapture::Off
        };
        self.raw_wanted = Some(flag);
    }

    /// Fall-only re-latch of `capture_raw`, called where the line buffers empty
    /// mid-run (line commit, prompt commit). The reader parses on the socket
    /// runtime while the trigger manager flips the flag from the session
    /// thread, and `TelnetBridge::on_data` hoists its per-byte raw push behind
    /// [`Self::capture_raw`] once per run: a mid-run FALL is safe under either
    /// hoisted branch (the push re-checks per byte, and a fallen commit
    /// attaches nothing), but a mid-run RISE would attach a torn raw suffix
    /// pushed only from the flip onward. Rises therefore wait for a batch
    /// boundary with no fragmented logical line open
    /// ([`Self::notify_end_of_buffer`]), where no run is in flight and the
    /// next line can be captured from its first byte. A fall drops whatever
    /// raw bytes were held for the next fragment ([`Self::drop_raw`]).
    fn refresh_capture_raw(&mut self) {
        if self.capture.is_on()
            && let Some(flag) = &self.raw_wanted
            && !flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            self.capture = RawCapture::Off;
            self.drop_raw();
        }
    }

    /// Whether raw bytes are currently being captured. The byte loop hoists
    /// its per-byte push behind this — sound across threads because the value
    /// can only fall mid-run (rises are deferred to batch boundaries), and a
    /// fall is safe under either hoisted branch.
    #[must_use]
    pub fn capture_raw(&self) -> bool {
        self.capture.is_on()
    }

    /// Close the active link's current range into `link_info` (empty ranges
    /// are dropped). `keep_active` retains the link across the boundary — a
    /// line commit inside a still-open link — while an explicit `ESC]8;;`
    /// ends it.
    fn close_link_range(&mut self, keep_active: bool) {
        if let Some(link) = &self.cursor_link {
            if self.buf.len() > self.link_open_pos {
                self.link_info.push(LinkSpan {
                    begin_pos: self.link_open_pos,
                    end_pos: self.buf.len(),
                    action: link.action.clone(),
                    tooltip: Some(link.tooltip.clone()),
                    style: link.style.clone(),
                });
            }
            if !keep_active {
                self.cursor_link = None;
            }
        }
        self.link_open_pos = self.buf.len();
    }

    /// Begin a link at the current buffer position, closing any open one (a
    /// second open without a close replaces it from that point).
    fn open_link(&mut self, link: ParsedLink) {
        self.close_link_range(false);
        self.cursor_link = Some(link);
        self.link_open_pos = self.buf.len();
    }

    /// A carriage-return overprint: discard the open frame — the local pending
    /// bytes, and (via [`VtEvent::RetractIncomingPartialLine`]) any
    /// prefix already flushed upstream as a partial — so the text after the
    /// `\r` replaces it. Raw bytes that arrived after the `\r` (up to and
    /// including the character triggering the restart) belong to the new
    /// frame and are kept. The cursor style survives, as on a real terminal.
    /// Out of line: an overprint is rare next to the runs and characters
    /// whose print paths test for it.
    #[cold]
    #[inline(never)]
    fn restart_open_line(&mut self, raw_mark: usize) {
        self.buf.clear();
        self.span_info.clear();
        // An active link survives the overprint like the cursor style does;
        // ranges already banked for the superseded frame are discarded.
        self.link_info.clear();
        self.link_open_pos = 0;
        // The superseded frame's objects go with its text: their ranges
        // meant nothing else, and nor do its placeholder cells.
        self.pending_objects.clear();
        self.placeholder_cells.clear();
        self.line_has_graphics = false;
        self.buf_raw.drain(..raw_mark.min(self.buf_raw.len()));
        self.open_logical_line.clear();
        self.flushed_columns = 0;
        self.sink.emit(VtEvent::RetractIncomingPartialLine);
    }

    fn change_style(&mut self, new_style: Style) {
        self.span_info.push(VtSpan {
            begin_pos: match self.span_info.last() {
                Some(span_info) => span_info.end_pos,
                None => 0,
            },
            end_pos: self.buf.len(),
            style: self.cursor_style,
        });

        self.cursor_style = new_style;
    }

    pub fn consume_into_pending_line(&mut self) -> StyledLine {
        let raw_end = self.buf_raw.len();
        self.consume_pending_line(raw_end)
    }

    /// [`Self::consume_into_pending_line`] with the raw text ending at
    /// `raw_end`: the bytes from there on are held back for the next
    /// fragment (see [`Self::held_raw_start`]) and stay in the buffer.
    fn consume_pending_line(&mut self, raw_end: usize) -> StyledLine {
        if self.line_has_graphics && !self.placeholder_cells.is_empty() {
            self.attach_placeholder_runs();
        }
        self.change_style(self.cursor_style);
        // A link still open at the boundary contributes its range so far and
        // stays active: its next range begins at 0 on the next line.
        self.close_link_range(true);
        let spans = self.span_info.drain(..).collect();
        let mut line = if self.capture.is_on() {
            StyledLine::new_with_raw_text(&self.buf, spans, Some(self.raw_text(raw_end)))
        } else {
            StyledLine::new(&self.buf, spans)
        };
        line.links = self.link_info.drain(..).collect();
        self.link_open_pos = 0;
        if self.line_has_graphics {
            self.take_line_graphics(&mut line);
        }
        line
    }

    /// The objects attached to the pending line leave with it (its
    /// placeholder cells were turned into them above). Out of line: a line
    /// carries graphics only when a placement landed on it.
    #[cold]
    #[inline(never)]
    fn take_line_graphics(&mut self, line: &mut StyledLine) {
        if !self.pending_objects.is_empty() {
            line.objects = Some(Arc::new(std::mem::take(&mut self.pending_objects)));
        }
        self.line_has_graphics = false;
    }

    /// Attach an inline object at the cursor: `columns` placeholder spaces
    /// join the pending line and the object covers them, exactly as a
    /// script-authored widget covers its text. Its identity is minted here,
    /// on the socket thread, and travels unchanged through every partial,
    /// fragment and join. A pending overprint restarts the line first, as a
    /// printed character would, so the per-line cap counts the frame the
    /// object joins, not the one it replaces. Nothing enters the raw
    /// capture: raw triggers match wire bytes, and a placement's were cut
    /// out with its APC. `baseline` says where text beside an object taller
    /// than its row sits (a placement leaves the cursor on its last row).
    /// `None` once the line holds the per-line cap.
    pub fn attach_inline_object(
        &mut self,
        columns: usize,
        resource: Arc<dyn std::any::Any + Send + Sync>,
        baseline: Baseline,
    ) -> Option<u64> {
        let mut object = InlineObject::new(0..0, resource, InlineOwner::default());
        object.baseline = baseline;
        self.attach_object(columns, object)
    }

    /// [`Self::attach_inline_object`] with the object built by the caller
    /// (its range is set here); zero `columns` attaches an anchor.
    fn attach_object(&mut self, columns: usize, mut object: InlineObject) -> Option<u64> {
        if let Some(raw_mark) = self.pending_cr.take() {
            self.restart_open_line(raw_mark);
        }
        if self.pending_objects.len() >= MAX_INLINE_OBJECTS {
            return None;
        }
        let start = self.buf.len();
        self.buf.extend(std::iter::repeat_n(' ', columns));
        object.range = start..self.buf.len();
        let id = object.id;
        self.pending_objects.push(object);
        self.line_has_graphics = true;
        Some(id)
    }

    /// Attach an object over text already in the pending line (its range
    /// is the object's): a placeholder run's placement. `None` at the
    /// per-line cap, like any attach.
    fn attach_over(&mut self, object: InlineObject) -> Option<u64> {
        if self.pending_objects.len() >= MAX_INLINE_OBJECTS {
            return None;
        }
        let id = object.id;
        self.pending_objects.push(object);
        self.line_has_graphics = true;
        Some(id)
    }

    /// Notifies that the end of a read batch of incoming data has been reached — the
    /// connection reader calls this once per socket wake (up to its byte budget), not once
    /// per read chunk.
    ///
    /// This finalizes any pending partial line and sends it, then requests a repaint.
    /// Sends are best-effort: the session runtime can tear down while the reader is
    /// mid-batch on the socket runtime, and the connection task then exits via its
    /// disconnect signal.
    pub fn notify_end_of_buffer(&mut self) {
        // A read batch ends wherever the socket did, including inside a
        // graphics APC or on a lone `ESC`: those bytes belong to the next
        // fragment's capture (where the string's end cuts it out, or the
        // sequence completes), not to this partial's raw text.
        let raw_end = self.held_raw_start();
        // A batch can carry terminal state without displayable characters
        // (for example, an SGR-only prompt). Preserve that cursor-style line
        // for prompt/color-only matching. A truly empty batch stays silent,
        // and so does one holding only string bytes.
        let has_pending_line = !self.buf.is_empty()
            || !self.span_info.is_empty()
            || !self.pending_objects.is_empty()
            || (self.capture.is_on() && raw_end > 0);
        if has_pending_line {
            let pending_line = Arc::new(self.consume_pending_line(raw_end));
            if !self.buf.is_empty() {
                self.packet_has_displayable_text = true;
            }
            self.open_logical_line.push(pending_line.clone());
            self.sink
                .emit(VtEvent::HandleIncomingPartialLine(pending_line));
            // The cursor's column past the flushed text is read only by
            // the graphics state, so it is counted only once one exists: a
            // line flushed before the session's first graphics command
            // reads its column from the fragments since.
            if self.graphics.is_some() {
                self.flushed_columns += self.buf.chars().count();
            }
            self.buf.clear();
            self.flush_raw(raw_end);
            self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
            self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
            // The frame a pending `\r` marked was just flushed upstream as a
            // partial; the restart's retraction covers it, and no local raw
            // bytes remain to drop (what stays is held for the next
            // fragment).
            if self.pending_cr.is_some() {
                self.pending_cr = Some(0);
            }
        }
        self.sink.emit(VtEvent::RequestRepaint);
        // The packet-completion marker exists only to release a deferred
        // profile send; once the runtime disarms the flag (send released, or
        // no send was pending), steady-state batches skip the channel send.
        let has_displayable_text = std::mem::take(&mut self.packet_has_displayable_text);
        if self
            .marker_armed
            .as_ref()
            .is_none_or(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            self.sink.emit(VtEvent::IncomingPacketProcessed {
                connection_generation: self.connection_generation,
                has_displayable_text,
            });
        }
        // A batch boundary with no fragmented logical line open is the one
        // place capture may RISE: no parse run is in flight, the next batch
        // starts a fresh line under the new value, and `TelnetBridge::on_data`
        // re-hoists it before any byte flows. Across a fragmented line the old
        // value stays latched, keeping raw text complete-or-absent.
        // A fall here drops what was held for the next fragment; a rise
        // finds the buffer empty, as it always is while capture is off.
        if !self.open_logical_line.has_fragments()
            && let Some(flag) = &self.raw_wanted
        {
            let wanted = flag.load(std::sync::atomic::Ordering::Relaxed);
            if self.capture.is_on() && !wanted {
                self.drop_raw();
            }
            debug_assert!(
                self.capture.is_on() || self.buf_raw.is_empty(),
                "raw bytes captured while capture was off"
            );
            self.capture = if wanted {
                self.capture.on()
            } else {
                RawCapture::Off
            };
        }
    }

    /// Commit the pending bytes as a **prompt**: emit them on the partial-line path (so
    /// `prompt:`-flagged triggers fire) and reset the buffers so the next bytes start a fresh line.
    ///
    /// Driven by the telnet layer when it decodes a prompt boundary (`IAC GA` / `IAC EOR`) — a
    /// precise, server-sent signal, unlike the partial-line-at-end-of-buffer heuristic in
    /// [`notify_end_of_buffer`](Self::notify_end_of_buffer). Clearing the buffers here is what stops
    /// that heuristic from re-emitting the same prompt at end of read. A bare
    /// boundary still emits one empty styled partial so a
    /// color-only prompt trigger can inspect the inherited cursor style. If a
    /// prior read-batch flush already emitted the prompt and no new terminal
    /// state arrived, only the boundary marker is sent. Sends are best-effort,
    /// like [`notify_end_of_buffer`](Self::notify_end_of_buffer)'s.
    pub fn commit_prompt(&mut self) {
        // A prompt boundary finalizes the line; a `\r` just before it must
        // not overwrite what follows.
        self.pending_cr = None;
        // A `GA` inside a graphics APC (or between an `ESC` and what it
        // begins) is as possible as a batch end there, and the bytes are
        // held back the same way.
        let raw_end = self.held_raw_start();
        let has_new_pending_line = !self.buf.is_empty()
            || !self.span_info.is_empty()
            || !self.pending_objects.is_empty()
            || (self.capture.is_on() && raw_end > 0);
        if !has_new_pending_line && self.open_logical_line.has_fragments() {
            self.open_logical_line.clear();
            self.sink.emit(VtEvent::PromptBoundary);
            self.refresh_capture_raw();
            // The prompt's row is final: what comes next is a new line.
            self.line_seq += 1;
            self.flushed_columns = 0;
            return;
        }
        let pending_line = Arc::new(self.consume_pending_line(raw_end));
        if !self.buf.is_empty() {
            self.packet_has_displayable_text = true;
        }
        self.sink
            .emit(VtEvent::HandleIncomingPartialLine(pending_line));
        self.sink.emit(VtEvent::PromptBoundary);
        // GA/EOR is an explicit semantic prompt boundary, unlike an arbitrary
        // read-batch flush. The prompt remains visually open, but it must not
        // become the prefix of the next normal trigger line.
        self.open_logical_line.clear();
        self.buf.clear();
        self.flush_raw(raw_end);
        self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
        self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
        self.refresh_capture_raw();
        self.line_seq += 1;
        self.flushed_columns = 0;
    }

    fn commit_line(&mut self) {
        let pending_line = Arc::new(self.consume_into_pending_line());
        self.line_seq += 1;
        self.flushed_columns = 0;
        if !pending_line.text.is_empty() {
            self.packet_has_displayable_text = true;
        }
        if self.open_logical_line.has_fragments() {
            let line = self
                .open_logical_line
                .take_joined_with(&pending_line)
                .expect("fragmented completion requires an open prefix");
            self.sink.emit(VtEvent::HandleIncomingFragmentedLine {
                line,
                completion_fragment: pending_line,
            });
        } else {
            self.sink.emit(VtEvent::HandleIncomingLine(pending_line));
        }
        self.buf.clear();
        self.clear_raw();
        self.refresh_capture_raw();
    }

    fn push_incoming_char(&mut self, ch: char) {
        self.buf.push(ch);
    }

    /// A deceptive invisible inside a link joins the line escaped.
    #[cold]
    #[inline(never)]
    fn push_escaped_link_char(&mut self, ch: char) {
        let mut escaped = String::new();
        push_display_char(&mut escaped, ch, true);
        self.buf.push_str(&escaped);
    }

    /// The Ground-state bulk `print`: one `push_str` for a run the byte loop already proved
    /// the parser would print unchanged (see `connection::plain_run`). Equivalent to `print`
    /// per character, including the overprint restart a pending `\r` demands and the
    /// per-character escaping of deceptive invisibles inside an open link.
    pub fn print_run(&mut self, run: &str) {
        if let Some(raw_mark) = self.pending_cr.take() {
            self.restart_open_line(raw_mark);
        }
        // Only a run inside a link, or one printed while placeholders are
        // armed, can need more than a `push_str`. Neither flag is set on the
        // common path, which stays two loads and the append; the rest lives
        // out of line so the byte loop this inlines into stays small.
        if self.cursor_link.is_some() || self.placeholders_armed {
            self.print_run_noted(run);
        } else {
            self.buf.push_str(run);
        }
    }

    /// [`Self::print_run`] inside a link or while placeholders are armed:
    /// only a run with non-ASCII bytes can need escaping (per character,
    /// through [`VTActor::print`]) or hold a placeholder to note.
    #[cold]
    #[inline(never)]
    fn print_run_noted(&mut self, run: &str) {
        if run.is_ascii() {
            self.buf.push_str(run);
        } else if self.cursor_link.is_some() {
            for c in run.chars() {
                self.print(c);
            }
        } else {
            self.stamp_placeholders_in(run);
            self.buf.push_str(run);
        }
    }

    pub fn push_raw_incoming_byte(&mut self, byte: u8) {
        // One compare for the common path: capture on, nothing pending.
        if self.capture == RawCapture::On {
            self.buf_raw.push(byte);
            self.raw_unverified |= byte >= 0x80;
        } else if self.capture == RawCapture::KeptEsc {
            self.push_raw_after_kept_esc(byte);
        }
    }

    /// [`Self::push_raw_incoming_byte`] for the byte after an `ESC` kept
    /// back from a stripped graphics APC: the byte settles the `ESC` first
    /// and joins the capture unless it completed the string's terminator.
    /// Out of line: this is the rare branch of a hot path.
    #[cold]
    #[inline(never)]
    fn push_raw_after_kept_esc(&mut self, byte: u8) {
        if self.settle_kept_esc(byte) {
            self.buf_raw.push(byte);
            self.raw_unverified |= byte >= 0x80;
        }
    }

    /// The verdict on the `ESC` kept back from a stripped graphics APC, in
    /// the raw byte after it. The `ESC` goes when the byte completes it (a
    /// `\`, or an 8-bit ST, which spell `ESC \` between them and go with
    /// it) or supersedes it (another `ESC`, or any C1 byte: the parser takes
    /// either for a fresh start, so the string's own `ESC` began nothing);
    /// it stays as the start of anything else. Whether the byte itself is
    /// to be captured.
    fn settle_kept_esc(&mut self, byte: u8) -> bool {
        self.capture = RawCapture::On;
        let completes = byte == b'\\' || byte == 0x9c;
        if completes || byte == 0x1b || (0x80..=0x9f).contains(&byte) {
            self.buf_raw.pop();
            // An overprint marked after that `ESC` must not reach past the
            // buffer's end, or the restart would drop the new frame's first
            // raw byte.
            if let Some(mark) = self.pending_cr.as_mut() {
                *mark = (*mark).min(self.buf_raw.len());
            }
        }
        !completes
    }

    /// Raw capture for a run the bulk path validated as UTF-8. A run carries no CR/LF (those
    /// are controls, which end a run), so nothing needs filtering out. Nor can a run follow
    /// an `ESC` kept back for ST: that `ESC` leaves the parser in its escape state, which
    /// only a byte on the per-byte path can end (CR/LF, the one kind that path does not
    /// push, keep the parser there), so [`Self::push_raw_incoming_byte`] has always settled
    /// `raw_skip_st` before the parser is back in Ground and a run can start — and a fall
    /// of capture, which would skip that push, clears the flag with the buffer.
    pub fn push_raw_incoming_run(&mut self, run: &str) {
        if self.capture.is_on() {
            debug_assert!(
                self.capture != RawCapture::KeptEsc,
                "a run cannot follow a kept-back ESC"
            );
            self.buf_raw.extend_from_slice(run.as_bytes());
        }
    }

    /// The pending raw bytes before `end` as text: borrowed straight from the buffer when
    /// every byte arrived verified, lossily re-decoded only when the per-byte path pushed a
    /// high byte. `end` is a hold-back point (see [`held_raw_start`]) or the buffer's
    /// length: it falls before a trailing ASCII `ESC`, at an open APC's `ESC _` (the
    /// `_` alone when capture rose between the two) or before the bytes of a character
    /// the batch cut short, never inside a multibyte character, so it never splits a
    /// validated run.
    fn raw_text(&self, end: usize) -> Cow<'_, str> {
        let raw = &self.buf_raw[..end];
        if self.raw_unverified {
            String::from_utf8_lossy(raw)
        } else {
            debug_assert!(std::str::from_utf8(raw).is_ok());
            // SAFETY: `raw_unverified` is set by every push of a byte >= 0x80 that did not
            // come through a validated run and is cleared only with the buffer, so while it
            // is false the buffer holds nothing but ASCII and validated UTF-8 runs, and
            // `end` (see above) is the buffer's length or the position of an ASCII byte:
            // a character boundary.
            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(raw) })
        }
    }

    /// Empty the raw buffer for the next line; nothing unverified remains,
    /// and no kept-back `ESC` (that state only ever describes the buffer's
    /// last byte).
    fn clear_raw(&mut self) {
        self.buf_raw.clear();
        self.raw_unverified = false;
        if self.capture == RawCapture::KeptEsc {
            self.capture = RawCapture::On;
        }
    }
}

// The raw-capture bookkeeping below works on the capture's own fields and
// lives outside the generic processor: compiled once here, not once per sink
// type beside the byte loop that each monomorphisation carries.

/// Whether `byte` begins a control sequence or string: `ESC`, or one of the
/// C1 introducers the parser honours (DCS, SOS, CSI, OSC, PM, APC). Such a
/// byte ends a string it arrives in and starts the next sequence at once.
/// Asked only of the byte that ended an APC string, where it is read on
/// its own (see [`strip_graphics_apc`]); at a batch boundary, where a
/// trailing C1 value is as likely a character's last byte, only `ESC` is
/// held back (see [`held_raw_start`]).
const fn introduces_sequence(byte: u8) -> bool {
    matches!(byte, 0x1b | 0x90 | 0x98 | 0x9b | 0x9d | 0x9e | 0x9f)
}

/// Cut a graphics APC out of `buf_raw` from `start` (or from its own
/// `ESC _ G` when the parser never said where it began), keeping a pending
/// overprint mark inside the buffer. The byte that ended the string is the
/// buffer's last: an 8-bit ST, CAN or SUB ends it and begins nothing, and
/// goes with it; an `ESC` or a C1 introducer ends it by beginning whatever
/// comes next (`ESC [ … m` straight after a chunk), so it stays — the `ESC`
/// provisionally, until the next byte says whether it was only half of
/// `ESC \` ([`VtProcessor::settle_kept_esc`]). That last byte is read for
/// what it is without doubt: the parser reads a string byte by byte, with
/// no UTF-8 decoding, so a C1 value there is the control it names and
/// everything before it in the string goes with the string; in text, where
/// the parser decodes UTF-8, a character's continuation byte with a C1
/// value (`œ` ends in `9C`) is taken as the character's and never reaches
/// the string machinery, so no character is ever cut here.
fn strip_graphics_apc(
    buf_raw: &mut Vec<u8>,
    pending_cr: &mut Option<usize>,
    raw_skip_st: &mut bool,
    start: Option<usize>,
) {
    let Some(at) = start.or_else(|| memchr::memmem::rfind(buf_raw, b"\x1b_G")) else {
        return;
    };
    let at = at.min(buf_raw.len());
    let introducer = buf_raw
        .last()
        .copied()
        .filter(|&byte| introduces_sequence(byte));
    buf_raw.truncate(at);
    if let Some(mark) = pending_cr.as_mut() {
        *mark = (*mark).min(at);
    }
    if let Some(byte) = introducer {
        buf_raw.push(byte);
        if byte == 0x1b {
            *raw_skip_st = true;
        }
    }
}

/// Where the raw bytes that must not flush with the pending line at a
/// boundary begin: those of an APC string still open (from
/// `apc_raw_start`; cut out of the capture when it ends, wherever that is)
/// or a lone trailing `ESC`, which belongs to whatever follows — the `\`
/// completing `ESC \` after a stripped graphics string (when it must stay
/// in the buffer for that `\` to find), or the sequence it begins, which
/// the next fragment then captures whole — or the bytes of a character the
/// batch cut short: a lead byte and fewer continuation bytes than it needs
/// (see [`incomplete_utf8_tail`]), which the parser holds in its UTF-8
/// state the same way and prints once the rest arrives. Flushed, they
/// would stand as a replacement character in this fragment and the rest as
/// another in the next (`語` arriving as `E8 AA` then `9E`), where the
/// joined text has the character whole. Of the control bytes only an
/// `ESC` is held: at the end of a batch, a byte with an 8-bit C1
/// introducer's value is far more likely the last byte of a character the
/// bulk path validated (`А` is `D0 90`, `語` is `E8 AA 9E`) than a
/// sequence's start, and holding it back would split the character —
/// behind `from_utf8_unchecked` when the run was verified — while `ESC`
/// (0x1B) is never part of one. Nothing is lost by not holding a C1 byte:
/// the sequence it begins is captured across the two fragments, whose
/// joined raw text is the same. Held bytes stay at the head of the buffer
/// across the flush, so a partial line's raw text is never torn and a
/// joined line's is the same as if nothing had split it. `buf_raw.len()`
/// when nothing is held.
fn held_raw_start(buf_raw: &[u8], apc_raw_start: Option<usize>) -> usize {
    match (apc_raw_start, buf_raw.last()) {
        (Some(at), _) => at.min(buf_raw.len()),
        (None, Some(&0x1b)) => buf_raw.len() - 1,
        (None, Some(&byte)) if byte >= 0x80 => buf_raw.len() - incomplete_utf8_tail(buf_raw),
        (None, _) => buf_raw.len(),
    }
}

/// How many bytes at the end of `buf_raw` begin a UTF-8 character that is
/// not complete — a lead byte and the continuation bytes so far, fewer than
/// its length and valid as far as they go, which is exactly what the
/// parser's UTF-8 state still holds — and so must not flush with the
/// fragment. Zero when the buffer ends in a complete character, in ASCII
/// or in bytes no character can begin with (a lone continuation byte, a
/// lead byte followed by a byte it cannot take): those the parser has
/// already replaced, and they flush as what they are. At most three, since
/// a character has at most four bytes.
fn incomplete_utf8_tail(buf_raw: &[u8]) -> usize {
    (1..=buf_raw.len().min(3))
        .find(|&len| {
            // Incomplete, and nothing else wrong with it: `from_utf8`
            // reports an unexpected end (no error length) at its start.
            matches!(
                std::str::from_utf8(&buf_raw[buf_raw.len() - len..]),
                Err(err) if err.valid_up_to() == 0 && err.error_len().is_none()
            )
        })
        .unwrap_or(0)
}

impl<S: VtSink> VTActor for VtProcessor<S> {
    fn print(&mut self, b: char) {
        if let Some(raw_mark) = self.pending_cr.take() {
            self.restart_open_line(raw_mark);
        }
        // One compare per printed character; the branch is taken only for
        // a graphics placeholder while a virtual placement exists.
        if b == PLACEHOLDER && self.placeholders_armed {
            self.stamp_placeholder(self.buf.len());
        }
        if self.cursor_link.is_some() && deceptive_invisible(b, InvisiblePolicy::Prose) {
            self.push_escaped_link_char(b);
        } else {
            self.push_incoming_char(b);
        }
    }

    fn execute_c0_or_c1(&mut self, control: u8) {
        match control {
            b'\n' => {
                self.pending_cr = None;
                self.commit_line();
            }
            b'\r' => self.pending_cr = Some(self.buf_raw.len()),
            _ => {}
        }
    }

    fn dcs_hook(
        &mut self,
        _byte: u8,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
    ) {
    }

    fn dcs_put(&mut self, _byte: u8) {}

    fn dcs_unhook(&mut self) {}

    fn esc_dispatch(
        &mut self,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
        _byte: u8,
    ) {
    }

    fn csi_dispatch(&mut self, params: &[CsiParam], _parameters_truncated: bool, byte: u8) {
        match byte {
            b'm' => {
                let new_style =
                    sgr::process_with_extras(self.cursor_style, params, &mut self.sgr_extras);
                self.change_style(new_style);
            }
            b'c' | b't' if self.graphics_mode.replies() => {
                self.answer_terminal_query(params, byte);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        // OSC 8 hyperlinks: `8 ; params ; URI`. vtparse splits the payload on
        // every `;`, but a URI may itself contain them — everything past the
        // second separator is the URI, rejoined. The params field (id=, …) is
        // accepted and unused. All other OSC selectors are ignored.
        if params.first() != Some(&&b"8"[..]) {
            return;
        }
        // A well-formed OSC 8 is `8 ; params ; URI`; anything shorter (a
        // truncated `ESC]8;` or bare `ESC]8`) is treated as a close so a
        // degenerate sequence can't leave a link open over later lines.
        if params.len() < 3 {
            self.close_link_range(false);
            return;
        }
        let uri = params[2..].join(&b';');
        if uri.is_empty() {
            self.close_link_range(false);
            return;
        }
        let uri = String::from_utf8_lossy(&uri);
        if starts_with_ascii_case_insensitive(&uri, "preset:") {
            self.close_link_range(false);
            if uri.len() > MAX_OSC8_URI_LEN {
                return;
            }
            let definition = &uri[7..];
            let (name, query) = definition.split_once('?').unwrap_or((definition, ""));
            let name = percent_decode(name);
            let (fields, _) = osc8_query_fields(query);
            let config = fields.iter().find_map(|field| {
                (field.kind == Osc8QueryFieldKind::Config).then_some(())?;
                let decoded = percent_decode(field.value?);
                serde_json::from_str::<serde_json::Value>(&decoded).ok()
            });
            if !name.is_empty()
                && !name.chars().any(char::is_control)
                && let Some(config @ serde_json::Value::Object(_)) = config
            {
                if self.link_presets.contains_key(&name)
                    || self.link_presets.len() < MAX_OSC8_PRESETS
                {
                    self.link_presets.insert(name, config);
                } else if !self.link_preset_overflow_logged {
                    log::warn!(
                        "OSC 8 preset limit of {MAX_OSC8_PRESETS} reached; new names ignored"
                    );
                    self.link_preset_overflow_logged = true;
                }
            }
            return;
        }
        match link_action_for_uri(&uri, &self.link_presets) {
            Some(link) => self.open_link(link),
            // Unsupported scheme: the text still displays, unlinked.
            None => self.close_link_range(false),
        }
    }

    fn osc_dispatch_truncated(&mut self, params: &[&[u8]]) {
        // A partial OSC 8 replacement/close is not actionable, and must not
        // leave the preceding active hyperlink spanning later output. Other
        // oversized OSC selectors remain ignored, exactly like their complete
        // forms, so an unrelated title/clipboard command cannot sever a link.
        if params.first() == Some(&&b"8"[..]) {
            self.close_link_range(false);
        }
    }

    fn apc_start(&mut self) {
        // The `ESC _` that opened the string reached the raw capture before
        // the parser saw it (and nothing did while capture is off, when the
        // buffer is empty): a graphics string is cut out from there. A
        // boundary between the two bytes keeps the `ESC` back for the `_`
        // (see `held_raw_start`); only capture rising between them leaves
        // fewer behind, and then the buffer holds nothing but the `_`.
        self.apc_raw_start = Some(self.buf_raw.len().saturating_sub(2));
    }

    fn apc_dispatch(&mut self, data: Vec<u8>) {
        let start = self.apc_raw_start.take();
        // Only graphics APCs are ours; any other APC string stays ignored,
        // raw capture included, exactly as before.
        let Some(parsed) = graphics::parse_apc(&data) else {
            return;
        };
        self.strip_graphics_apc_from_raw(start);
        match parsed {
            Ok((control, payload)) => self.graphics_command(control, payload),
            Err(err) => log::debug!("malformed graphics command ignored: {err}"),
        }
    }

    fn apc_dispatch_truncated(&mut self, prefix: &[u8]) {
        let start = self.apc_raw_start.take();
        // An oversized graphics chunk is a protocol violation (chunks are
        // capped at 4 KiB of payload); it still must not leave its prefix in
        // the raw capture, and it ends any upload it may have belonged to.
        // A session that has never run a graphics command has no upload to
        // end, and the refusal it would get is not worth allocating the
        // state for: the violation is dropped.
        if prefix.first() == Some(&b'G') {
            self.strip_graphics_apc_from_raw(start);
            if self.graphics.is_some() {
                self.with_graphics(|state, env| state.truncated_command(env, prefix));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnsiColor, Color, MAX_INLINE_OBJECTS, MAX_OSC8_PRESETS, MAX_OSC8_URI_LEN, VtEvent,
        VtProcessor, VtSink, held_raw_start, incomplete_utf8_tail, menu_title,
        parse_link_tooltip_text, sanitize_display_text,
    };
    use crate::graphics;
    use crate::link_url_host;
    use smudgy_session_model::inline_content::{Baseline, Layer};
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use vtparse::VTParser;

    #[derive(Debug)]
    struct TestSink(Sender<VtEvent>);

    impl From<Sender<VtEvent>> for TestSink {
        fn from(sender: Sender<VtEvent>) -> Self {
            Self(sender)
        }
    }

    impl VtSink for TestSink {
        fn emit(&mut self, event: VtEvent) {
            self.0.send(event).unwrap();
        }
    }

    struct Harness {
        parser: VTParser,
        processor: VtProcessor<TestSink>,
        rx: Receiver<VtEvent>,
    }

    fn harness() -> Harness {
        let (tx, rx) = channel();
        Harness {
            parser: VTParser::new(),
            processor: VtProcessor::new(tx),
            rx,
        }
    }

    impl Harness {
        /// Mirrors the connection's byte loop: raw bytes (minus CR/LF) are
        /// pushed before the parser sees each byte.
        fn feed(&mut self, bytes: &[u8]) {
            for &b in bytes {
                if b != b'\n' && b != b'\r' {
                    self.processor.push_raw_incoming_byte(b);
                }
                self.parser.parse_byte(b, &mut self.processor);
            }
        }

        fn actions(&mut self) -> Vec<VtEvent> {
            let mut out = Vec::new();
            while let Ok(action) = self.rx.try_recv() {
                out.push(action);
            }
            out
        }
    }

    /// The committed/partial line texts and where retractions fall between them.
    fn transcript(actions: &[VtEvent]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                VtEvent::HandleIncomingLine(line) => Some(format!("line:{}", line.text)),
                VtEvent::HandleIncomingFragmentedLine { line, .. } => {
                    Some(format!("line:{}", line.text))
                }
                VtEvent::HandleIncomingPartialLine(line) => Some(format!("partial:{}", line.text)),
                VtEvent::RetractIncomingPartialLine => Some("retract".to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bare_cr_overprints_the_open_line() {
        let mut h = harness();
        h.feed(b"10%\r20%\r100%\n");
        assert_eq!(
            transcript(&h.actions()),
            ["retract", "retract", "line:100%"]
        );
    }

    #[test]
    fn crlf_commits_normally() {
        let mut h = harness();
        h.feed(b"a\r\nb\r\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["line:a", "line:b"]);
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, VtEvent::HandleIncomingLine(_)))
                .count(),
            2,
            "CRLF-complete lines must retain the direct hot-path action"
        );
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, VtEvent::HandleIncomingFragmentedLine { .. }))
        );
    }

    #[test]
    fn read_batch_fragments_reassemble_once_at_the_real_line_boundary() {
        let mut h = harness();
        h.feed(b"\x1b[31m[AU");
        h.processor.notify_end_of_buffer();
        h.feed(b"TO");
        h.processor.notify_end_of_buffer();
        h.feed(b"LOOT]\x1b[0m rest\r\n");

        let actions = h.actions();
        assert_eq!(
            transcript(&actions),
            ["partial:[AU", "partial:TO", "line:[AUTOLOOT] rest"]
        );
        let (line, completion_fragment) = actions
            .iter()
            .find_map(|action| match action {
                VtEvent::HandleIncomingFragmentedLine {
                    line,
                    completion_fragment,
                } => Some((line, completion_fragment)),
                _ => None,
            })
            .expect("fragmented completion action");
        assert_eq!(line.text, "[AUTOLOOT] rest");
        assert_eq!(completion_fragment.text, "LOOT] rest");
        assert_eq!(line.raw(), Some("\x1b[31m[AUTOLOOT]\x1b[0m rest"));
        assert_eq!(line.spans.first().map(|span| span.begin_pos), Some(0));
        assert_eq!(
            line.spans.last().map(|span| span.end_pos),
            Some(line.text.len())
        );
        assert!(line.spans.iter().any(|span| matches!(
            span.style.fg,
            Color::Ansi {
                color: super::sgr::AnsiColor::Red,
                bold: false
            }
        ) && &line.text[span.begin_pos..span.end_pos]
            == "LOOT]"));
    }

    #[test]
    fn packet_marker_requires_non_empty_terminal_text() {
        let mut h = harness();

        h.feed(b"\r\n");
        h.processor.notify_end_of_buffer();
        assert!(h.actions().iter().any(|action| matches!(
            action,
            VtEvent::IncomingPacketProcessed {
                connection_generation: 0,
                has_displayable_text: false,
            }
        )));

        h.feed(b"Welcome\r\n");
        h.processor.notify_end_of_buffer();
        assert!(h.actions().iter().any(|action| matches!(
            action,
            VtEvent::IncomingPacketProcessed {
                connection_generation: 0,
                has_displayable_text: true,
            }
        )));

        // An unterminated prompt is displayable too: the batch boundary sends
        // it through the partial-line pipeline before the packet marker.
        h.feed(b"Login: ");
        h.processor.notify_end_of_buffer();
        assert!(h.actions().iter().any(|action| matches!(
            action,
            VtEvent::IncomingPacketProcessed {
                connection_generation: 0,
                has_displayable_text: true,
            }
        )));
    }

    #[test]
    fn sgr_only_batch_emits_one_empty_styled_partial() {
        let mut h = harness();
        h.feed(b"\x1b[31m");
        h.processor.notify_end_of_buffer();
        let actions = h.actions();
        let partial = actions
            .iter()
            .find_map(|action| match action {
                VtEvent::HandleIncomingPartialLine(line) => Some(line),
                _ => None,
            })
            .expect("SGR-only terminal state must reach prompt triggers");
        assert!(partial.text.is_empty());
        assert!(matches!(
            partial.spans.last().map(|span| span.style.fg),
            Some(Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            })
        ));

        h.processor.notify_end_of_buffer();
        assert!(
            !h.actions()
                .iter()
                .any(|action| matches!(action, VtEvent::HandleIncomingPartialLine(_))),
            "an actually empty follow-up batch must not duplicate the partial"
        );
    }

    #[test]
    fn bare_prompt_boundary_emits_the_inherited_cursor_style() {
        let mut h = harness();
        h.feed(b"\x1b[31m");
        h.processor.commit_prompt();
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:"]);
        let partial = actions
            .iter()
            .find_map(|action| match action {
                VtEvent::HandleIncomingPartialLine(line) => Some(line),
                _ => None,
            })
            .unwrap();
        assert!(matches!(
            partial.spans.last().map(|span| span.style.fg),
            Some(Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            })
        ));

        // A later bare GA/EOR is another explicit empty prompt under the
        // cursor style that survived the previous boundary.
        h.processor.commit_prompt();
        assert_eq!(transcript(&h.actions()), ["partial:"]);
    }

    #[test]
    fn packet_marker_stops_flowing_once_disarmed() {
        let mut h = harness();
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        h.processor.set_marker_armed_flag(armed.clone());

        h.feed(b"Welcome\r\n");
        h.processor.notify_end_of_buffer();
        assert!(
            h.actions()
                .iter()
                .any(|action| matches!(action, VtEvent::IncomingPacketProcessed { .. }))
        );

        // The runtime disarms after releasing the deferred send; from then on
        // batch boundaries stay off the marker channel entirely.
        armed.store(false, std::sync::atomic::Ordering::Relaxed);
        h.feed(b"steady state\r\n");
        h.processor.notify_end_of_buffer();
        assert!(
            !h.actions()
                .iter()
                .any(|action| matches!(action, VtEvent::IncomingPacketProcessed { .. }))
        );
    }

    #[test]
    fn newline_then_cr_line_endings_commit_normally() {
        // Some servers terminate with \n\r; the stray \r restarts an empty
        // frame, which retracts nothing upstream and loses no text.
        let mut h = harness();
        h.feed(b"a\n\rb\n");
        let transcript = transcript(&h.actions());
        assert_eq!(transcript[0], "line:a");
        assert_eq!(transcript.last().unwrap(), "line:b");
    }

    #[test]
    fn cursor_style_survives_an_overprint() {
        let mut h = harness();
        h.feed(b"\x1b[31mold\rnew\n");
        let actions = h.actions();
        let line = actions
            .iter()
            .find_map(|action| match action {
                VtEvent::HandleIncomingLine(line) => Some(line.clone()),
                _ => None,
            })
            .expect("a committed line");
        assert_eq!(line.text, "new");
        assert!(
            line.spans.iter().all(|span| matches!(
                span.style.fg,
                Color::Ansi {
                    color: super::sgr::AnsiColor::Red,
                    bold: false
                }
            )),
            "the SGR set before the overprint must still color the new frame: {:?}",
            line.spans
        );
        assert_eq!(line.raw(), Some("new"));
    }

    #[test]
    fn raw_capture_rises_at_batch_boundaries_and_falls_at_line_boundaries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = Arc::new(AtomicBool::new(false));
        let mut h = harness();
        h.processor.set_raw_wanted_flag(flag.clone());

        // No raw trigger registered: the wire bytes are not copied.
        h.feed(b"\x1b[31mplain\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "plain");
        assert_eq!(lines[0].raw(), None);

        // The flag rises mid-batch: no line captures yet — the reader may be
        // mid-run with the push branch hoisted off, so a rise waits for the
        // batch boundary and every line stays complete-or-absent (absent).
        h.feed(b"mid");
        flag.store(true, Ordering::Relaxed);
        h.feed(b"line\n\x1b[32mnext\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].raw(), None, "a rise never applies mid-batch");
        assert_eq!(lines[1].raw(), None, "a rise never applies mid-batch");

        // The batch boundary latches the rise; capture starts with the next batch.
        h.processor.notify_end_of_buffer();
        h.feed(b"\x1b[32mnow\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].raw(), Some("\x1b[32mnow\x1b[0m"));

        // A drop applies at the next line boundary: the line in flight when
        // the flag fell was fully pushed, so it still captures (complete), and
        // the one after it does not.
        flag.store(false, Ordering::Relaxed);
        h.feed(b"latched\nafter\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].raw(), Some("latched"));
        assert_eq!(lines[1].raw(), None);
    }

    #[test]
    fn overprint_after_a_flushed_partial_retracts_it() {
        let mut h = harness();
        h.feed(b"10%");
        h.processor.notify_end_of_buffer();
        h.feed(b"\r20%\n");
        assert_eq!(
            transcript(&h.actions()),
            ["partial:10%", "retract", "line:20%"]
        );
    }

    #[test]
    fn prompt_boundary_clears_a_pending_cr() {
        let mut h = harness();
        h.feed(b"> \r");
        h.processor.commit_prompt();
        h.feed(b"ok\n");
        assert_eq!(transcript(&h.actions()), ["partial:> ", "line:ok"]);
    }

    #[test]
    fn explicit_prompt_boundary_closes_a_previously_flushed_batch_fragment() {
        let mut h = harness();
        h.feed(b"> ");
        h.processor.notify_end_of_buffer();
        // GA/EOR may arrive in the next socket batch with no additional text.
        h.processor.commit_prompt();
        h.feed(b"ok\n");
        assert_eq!(transcript(&h.actions()), ["partial:> ", "line:ok"]);
    }

    use smudgy_session_model::{
        LinkAction, LinkColor, LinkDecoration, LinkMenuItem, LinkStyleState, LinkTooltip,
        LinkVisibilityAction, StyledLine,
    };

    fn committed_lines(actions: &[VtEvent]) -> Vec<std::sync::Arc<StyledLine>> {
        actions
            .iter()
            .filter_map(|action| match action {
                VtEvent::HandleIncomingLine(line)
                | VtEvent::HandleIncomingFragmentedLine { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn osc8_http_link_spans_the_enclosed_text() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\ done\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "click me done");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(lines[0].links[0].begin_pos, 0);
        assert_eq!(lines[0].links[0].end_pos, "click me".len());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com"))
        );
        assert_eq!(
            lines[0].links[0]
                .tooltip
                .as_ref()
                .and_then(LinkTooltip::display),
            Some((std::sync::Arc::from("https://example.com"), None))
        );
    }

    #[test]
    fn osc8_empty_send_and_hostless_urls_render_plain_text() {
        for uri in [
            "send:",
            "send:?config={\"tooltip\":\"empty\"}",
            "http://",
            "https://",
            "ftp://",
            "http:///path",
            "https://?query",
            "ftp://#fragment",
            "http://:80/path",
            "https://user@/path",
        ] {
            let mut h = harness();
            h.feed(format!("\x1b]8;;{uri}\x1b\\plain\x1b]8;;\x1b\\\n").as_bytes());
            let lines = committed_lines(&h.actions());
            assert_eq!(lines[0].text, "plain", "URI: {uri}");
            assert!(lines[0].links.is_empty(), "URI: {uri}");
        }
    }

    #[test]
    fn osc8_empty_prompt_matches_mudlet_clear_input_action() {
        let mut h = harness();
        // Mudlet's current OSC 8 implementation accepts `prompt:` and calls
        // sendCmdLine(""); this deliberately preserves that clear-input action.
        h.feed(b"\x1b]8;;prompt:\x1b\\clear input\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::Prompt(std::sync::Arc::from(""))
        );
    }

    #[test]
    fn osc8_minimal_host_check_keeps_unusual_valid_urls() {
        for uri in [
            "http://localhost",
            "HtTpS://[::1]:8443/path",
            "ftp://user:password@example.test/archive",
            "https://\u{4f8b}\u{3048}.test/path",
        ] {
            let mut h = harness();
            h.feed(format!("\x1b]8;;{uri}\x1b\\open\x1b]8;;\x1b\\\n").as_bytes());
            let lines = committed_lines(&h.actions());
            assert_eq!(lines[0].links.len(), 1, "URI: {uri}");
            assert_eq!(
                lines[0].links[0].action,
                LinkAction::OpenUrl(std::sync::Arc::from(uri)),
                "URI: {uri}"
            );
        }
    }

    #[test]
    fn osc8_web_urls_use_browser_host_parsing() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://evil.example\\@trusted.example/\x1b\\open\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].links.len(), 1);
        let LinkAction::OpenUrl(url) = &lines[0].links[0].action else {
            panic!("expected web link")
        };
        assert_eq!(
            link_url_host(url),
            Some("evil.example".to_string()),
            "validation and trust grants must identify the browser's destination host"
        );
    }

    #[test]
    fn osc8_url_parameter_matrix_matches_mudlet() {
        let cases = [
            (
                "\x1b]8;;https://example.com/?id=42&lang=en\x1b\\Link\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/?id=42&lang=en")),
            ),
            (
                "\x1b]8;;https://example.com/?config=%7B%22style%22%3A%7B%22color%22%3A%22red%22%7D%7D\x1b\\Styled\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/")),
            ),
            (
                "\x1b]8;;https://example.com/?page=1&preset=danger\x1b\\Link\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/?page=1")),
            ),
            (
                "\x1b]8;;https://example.com/?preset%3Ddefault&page=1\x1b\\Link\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/?page=1")),
            ),
            (
                "\x1b]8;;https://example.com/?config%3dvalue&foo=bar\x1b\\Link\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/?foo=bar")),
            ),
            (
                "\x1b]8;;https://example.com/?%63%6F%6E%66%69%67=value\x1b\\Link\x1b]8;;\x1b\\\n",
                LinkAction::OpenUrl(std::sync::Arc::from(
                    "https://example.com/?%63%6F%6E%66%69%67=value",
                )),
            ),
            (
                "\x1b]8;;send:attack?config=%7B%22style%22%3A%7B%22color%22%3A%22red%22%7D%7D\x1b\\Attack\x1b]8;;\x1b\\\n",
                LinkAction::ServerSend(std::sync::Arc::from("attack")),
            ),
        ];

        for (input, expected) in cases {
            let mut h = harness();
            h.feed(input.as_bytes());
            let lines = committed_lines(&h.actions());
            assert_eq!(lines.len(), 1, "input: {input:?}");
            assert_eq!(lines[0].links.len(), 1, "input: {input:?}");
            assert_eq!(lines[0].links[0].action, expected, "input: {input:?}");
        }
    }

    #[test]
    fn osc8_raw_json_keeps_hash_colors_ampersands_and_url_fragments_distinct() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;https://example.com/pay?x=1&config={\"style\":{\"color\":\"#0066ff\"},\"tooltip\":\"A & B\"}#receipt\x1b\\pay\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        assert_eq!(
            link.action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/pay?x=1#receipt"))
        );
        assert_eq!(
            link.style.as_ref().expect("raw JSON style").base.foreground,
            Some(LinkColor {
                red: 0,
                green: 102,
                blue: 255,
                alpha: 255,
            })
        );
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("A & B"),
                Some(std::sync::Arc::from("https://example.com/pay?x=1#receipt"))
            ))
        );
    }

    #[test]
    fn osc8_menu_title_text_matrix_rejects_invalid_or_empty_copy() {
        let cases = [
            (
                serde_json::json!("Lamb and Barley Stew"),
                Some("Lamb and Barley Stew"),
            ),
            (
                serde_json::json!({"text": "Style-less Title"}),
                Some("Style-less Title"),
            ),
            (
                serde_json::json!("Potion du Guerrier"),
                Some("Potion du Guerrier"),
            ),
            (
                serde_json::json!("Item <Rare> [+5] & More!"),
                Some("Item <Rare> [+5] & More!"),
            ),
            (serde_json::json!(""), None),
            (serde_json::json!({"text": ""}), None),
            (serde_json::json!({"style": {"color": "red"}}), None),
            (serde_json::json!(42), None),
            (serde_json::json!(true), None),
            (serde_json::json!(["a", "b"]), None),
        ];

        for (value, expected) in cases {
            assert_eq!(
                menu_title(&value).as_ref().map(|title| title.text.as_ref()),
                expected,
                "value: {value}"
            );
        }
    }

    #[test]
    fn osc8_menu_title_style_matrix_supports_css_and_decorations() {
        let title = menu_title(&serde_json::json!({
            "text": "Decorated Title",
            "style": {
                "color": "#ff0000",
                "background-color": "#660000",
                "bold": true,
                "italic": true,
                "underline": true,
                "strikethrough": true,
                "text-decoration-color": "#ff00ff"
            }
        }))
        .expect("styled title");
        let style = title.style.expect("title style");
        assert_eq!(
            style.foreground,
            Some(LinkColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            })
        );
        assert_eq!(
            style.background,
            Some(LinkColor {
                red: 102,
                green: 0,
                blue: 0,
                alpha: 255,
            })
        );
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(LinkDecoration::Solid));
        assert_eq!(style.strikethrough, Some(LinkDecoration::Solid));
        assert_eq!(
            style.decoration_color,
            Some(LinkColor {
                red: 255,
                green: 0,
                blue: 255,
                alpha: 255,
            })
        );

        for (name, expected) in [
            ("wavy", LinkDecoration::Wavy),
            ("dotted", LinkDecoration::Dotted),
            ("dashed", LinkDecoration::Dashed),
        ] {
            let title = menu_title(&serde_json::json!({
                "text": "Underline",
                "style": {"underline": name}
            }))
            .expect("underlined title");
            assert_eq!(
                title.style.expect("title style").underline,
                Some(expected),
                "underline: {name}"
            );
        }
    }

    #[test]
    fn osc8_parses_at_every_packet_boundary() {
        let input = b"before \x1b]8;;send:look?config={\"style\":{\"color\":\"#0066ff\"}}\x1b\\look\x1b]8;;\x1b\\ after\n";
        for split in 0..=input.len() {
            let mut h = harness();
            h.feed(&input[..split]);
            h.feed(&input[split..]);
            let lines = committed_lines(&h.actions());
            assert_eq!(lines.len(), 1, "split at byte {split}");
            assert_eq!(lines[0].text, "before look after", "split at byte {split}");
            assert_eq!(lines[0].links.len(), 1, "split at byte {split}");
            assert_eq!(
                lines[0].links[0].action,
                LinkAction::ServerSend(std::sync::Arc::from("look")),
                "split at byte {split}"
            );
        }
    }

    #[test]
    fn esc_terminated_osc8_does_not_swallow_later_output() {
        let mut h = harness();
        // vtparse terminates an OSC string on a bare ESC; the stream stays live.
        h.feed(b"\x1b]8;;send:hi\x1b[0mlinked\x1b]8;;\x1b[0m\n");
        let mut filler = Vec::new();
        for _ in 0..60 {
            filler.extend_from_slice(&[b'a'; 99]);
            filler.push(b'\n');
        }
        h.feed(&filler);
        h.feed(b"hello\n");
        let lines = committed_lines(&h.actions());
        assert!(lines.iter().any(|line| line.text == "hello"));
    }

    #[test]
    fn utf8_continuation_9d_does_not_swallow_later_output() {
        let mut h = harness();
        // U+1F49D ends in byte 0x9D, a UTF-8 continuation byte, followed by '8'.
        h.feed("\u{1F49D}8;; plain text\n".as_bytes());
        let mut filler = Vec::new();
        for _ in 0..60 {
            filler.extend_from_slice(&[b'b'; 99]);
            filler.push(b'\n');
        }
        h.feed(&filler);
        h.feed(b"world\n");
        let lines = committed_lines(&h.actions());
        assert!(lines.iter().any(|line| line.text == "world"));
    }

    #[test]
    fn osc8_basic_style_parses_full_and_compact_properties() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;send:look?config=%7B%22s%22%3A%7B%22c%22%3A%22red%22%2C%22bg%22%3A%22%2301020380%22%2C%22b%22%3Afalse%2C%22i%22%3Atrue%2C%22u%22%3Afalse%2C%22o%22%3A%22dotted%22%2C%22st%22%3A%22wavy%22%2C%22tdc%22%3A%22blue%22%7D%7D\x1b\\styled\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let style = &lines[0].links[0]
            .style
            .as_ref()
            .expect("authored style")
            .base;
        assert_eq!(
            style.foreground,
            Some(LinkColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            })
        );
        assert_eq!(
            style.background,
            Some(LinkColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 128,
            })
        );
        assert_eq!(style.bold, Some(false));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(LinkDecoration::None));
        assert_eq!(style.overline, Some(LinkDecoration::Dotted));
        assert_eq!(style.strikethrough, Some(LinkDecoration::Wavy));
        assert_eq!(
            style.decoration_color,
            Some(LinkColor {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 255,
            })
        );
    }

    #[test]
    fn osc8_empty_authored_style_is_retained() {
        let mut h = harness();
        h.feed(b"\x1b]8;;send:look?config=%7B%22style%22%3A%7B%7D%7D\x1b\\look\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert!(lines[0].links[0].style.is_some());
    }

    #[test]
    fn osc8_state_styles_parse_compact_names_and_resolve_priority() {
        let mut h = harness();
        h.feed(b"\x1b]8;;send:look?config={\"s\":{\"c\":\"white\",\"al\":{\"c\":\"blue\"},\"l\":{\"b\":true},\"vi\":{\"c\":\"purple\"},\"h\":{\"c\":\"red\"},\"a\":{\"c\":\"green\"}}}\x1b\\look\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        let style = lines[0].links[0].style.as_ref().expect("style");
        assert!(style.has_states());
        let visited_hover = style.resolve(LinkStyleState {
            visited: true,
            hover: true,
            ..LinkStyleState::default()
        });
        assert_eq!(
            visited_hover.foreground,
            Some(LinkColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            })
        );
        let active = style.resolve(LinkStyleState {
            active: true,
            hover: true,
            ..LinkStyleState::default()
        });
        assert_eq!(
            active.foreground,
            Some(LinkColor {
                red: 0,
                green: 128,
                blue: 0,
                alpha: 255,
            })
        );
        assert_eq!(active.bold, Some(true));
    }

    #[test]
    fn osc8_compact_protocol_behaviors_are_retained() {
        let mut h = harness();
        h.feed(b"\x1b]8;;send:vote?config={\"sp\":true,\"v\":{\"action\":[\"reveal\",\"conceal\"],\"delay\":25,\"wholeline\":true},\"sel\":{\"group\":\"poll\",\"value\":\"yes\",\"toggle\":false,\"selected\":true,\"exclusive\":false,\"disabled\":true}}\x1b\\yes\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        let protocol = link.action.protocol().expect("protocol behavior");
        assert!(protocol.spoiler);
        let visibility = protocol.visibility.as_ref().expect("visibility");
        assert_eq!(visibility.action, LinkVisibilityAction::RevealThenConceal);
        assert_eq!(visibility.delay_ms, Some(25));
        assert!(visibility.whole_line);
        let selection = protocol.selection.as_ref().expect("selection");
        assert_eq!(selection.group.as_ref(), "poll");
        assert_eq!(selection.value.as_ref(), "yes");
        assert!(!selection.toggle);
        assert!(selection.selected);
        assert!(!selection.exclusive);
        assert!(selection.disabled);
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Click to reveal"),
                Some(std::sync::Arc::from("send:vote"))
            ))
        );
    }

    #[test]
    fn osc8_presets_are_session_scoped_and_deep_merge_overrides() {
        let mut h = harness();
        h.feed(b"\x1b]8;;preset:danger?config={\"s\":{\"c\":\"red\",\"b\":true},\"t\":\"Danger\"}\x1b\\\x1b]8;;\x1b\\");
        h.feed(b"\x1b]8;;send:attack?preset=danger&config={\"s\":{\"c\":\"blue\"}}\x1b\\attack\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        let style = &link.style.as_ref().expect("preset style").base;
        assert_eq!(style.bold, Some(true));
        assert_eq!(
            style.foreground,
            Some(LinkColor {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 255,
            })
        );
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Danger"),
                Some(std::sync::Arc::from("send:attack"))
            ))
        );
    }

    #[test]
    fn osc8_preset_raw_json_keeps_ampersands_and_hash_colors() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;preset:shop?config={\"t\":\"Fish & Chips\",\"s\":{\"c\":\"#0066ff\"}}\x1b\\",
        );
        h.feed(b"\x1b]8;;send:buy?preset=shop\x1b\\buy\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Fish & Chips"),
                Some(std::sync::Arc::from("send:buy"))
            ))
        );
        assert_eq!(
            link.style.as_ref().expect("preset style").base.foreground,
            Some(LinkColor {
                red: 0,
                green: 102,
                blue: 255,
                alpha: 255,
            })
        );
    }

    #[test]
    fn osc8_preset_registry_caps_new_names_but_allows_replacement() {
        let mut h = harness();
        for index in 0..MAX_OSC8_PRESETS {
            h.feed(
                format!("\x1b]8;;preset:p{index}?config={{\"t\":\"original\"}}\x1b\\").as_bytes(),
            );
        }
        assert_eq!(h.processor.link_presets.len(), MAX_OSC8_PRESETS);

        h.feed(b"\x1b]8;;preset:overflow?config={\"t\":\"ignored\"}\x1b\\");
        h.feed(b"\x1b]8;;preset:overflow-again?config={\"t\":\"ignored\"}\x1b\\");
        assert_eq!(h.processor.link_presets.len(), MAX_OSC8_PRESETS);
        assert!(!h.processor.link_presets.contains_key("overflow"));
        assert!(h.processor.link_preset_overflow_logged);

        h.feed(b"\x1b]8;;preset:p0?config={\"t\":\"replacement\"}\x1b\\");
        assert_eq!(h.processor.link_presets.len(), MAX_OSC8_PRESETS);
        assert_eq!(
            h.processor.link_presets["p0"]["t"].as_str(),
            Some("replacement")
        );
    }

    #[test]
    fn osc8_link_copy_escapes_direction_controls() {
        let mut h = harness();
        h.feed("\x1b]8;;send:look\x1b\\safe\u{202e}txt\x1b]8;;\x1b\\\n".as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "safe\\u{202E}txt");
        assert_eq!(lines[0].links[0].end_pos, lines[0].text.len());
    }

    #[test]
    fn osc8_link_copy_preserves_emoji_shaping_controls() {
        let mut h = harness();
        let label = "🗝️ 👩‍💻";
        h.feed(format!("\x1b]8;;send:look\x1b\\{label}\x1b]8;;\x1b\\\n").as_bytes());
        let lines = committed_lines(&h.actions());

        assert_eq!(lines[0].text, label);
        assert_eq!(lines[0].links[0].end_pos, label.len());
        assert_eq!(
            sanitize_display_text("send:look\u{fe0f}", false).as_ref(),
            "send:look\\u{FE0F}",
            "destination disclosure must still expose invisible shaping controls"
        );
    }

    #[test]
    fn osc8_config_tooltip_strips_config_and_discloses_the_target() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;https://example.com/pay?x=1&config=%7B%22tooltip%22%3A%22Trusted%20checkout%22%7D#receipt\x1b\\pay\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        assert_eq!(
            link.action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/pay?x=1#receipt"))
        );
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Trusted checkout"),
                Some(std::sync::Arc::from("https://example.com/pay?x=1#receipt"))
            ))
        );
    }

    #[test]
    fn osc8_tooltip_preserves_json_lf_and_normalizes_crlf() {
        let cases = [
            (
                b"%7B%22tooltip%22%3A%22Line%20one%5CnLine%20two%22%7D".as_slice(),
                "Line one\nLine two",
            ),
            (
                b"%7B%22tooltip%22%3A%22Line%20one%5Cr%5CnLine%20two%22%7D".as_slice(),
                "Line one\nLine two",
            ),
        ];
        for (config, expected) in cases {
            let mut h = harness();
            let mut input = b"\x1b]8;;https://example.com/stats?config=".to_vec();
            input.extend_from_slice(config);
            input.extend_from_slice(b"\x1b\\stats\x1b]8;;\x1b\\\n");
            h.feed(&input);
            let lines = committed_lines(&h.actions());
            assert_eq!(
                lines[0].links[0]
                    .tooltip
                    .as_ref()
                    .and_then(LinkTooltip::display),
                Some((
                    std::sync::Arc::from(expected),
                    Some(std::sync::Arc::from("https://example.com/stats"))
                ))
            );
        }
    }

    #[test]
    fn tooltip_ansi_parser_keeps_sgr_spans_and_discards_active_controls() {
        let parsed =
            parse_link_tooltip_text("\x1b[38;2;255;128;0mEPIC\x1b[0m\nDamage: \x1b[1;31m42\x1b[0m")
                .expect("styled tooltip");
        assert_eq!(parsed.text.as_ref(), "EPIC\nDamage: 42");
        assert_eq!(
            &parsed.text[parsed.spans[0].begin_pos..parsed.spans[0].end_pos],
            "EPIC"
        );
        assert_eq!(
            parsed.spans[0].style.fg,
            Color::Rgb {
                r: 255,
                g: 128,
                b: 0
            }
        );
        assert!(parsed.spans.iter().any(|span| {
            &parsed.text[span.begin_pos..span.end_pos] == "42"
                && span.style.attributes.bold
                && span.style.fg
                    == Color::Ansi {
                        color: AnsiColor::Red,
                        bold: false,
                    }
        }));
        assert_eq!(parsed.spans.first().map(|span| span.begin_pos), Some(0));
        assert_eq!(
            parsed.spans.last().map(|span| span.end_pos),
            Some(parsed.text.len())
        );
        assert!(
            parsed
                .spans
                .windows(2)
                .all(|pair| pair[0].end_pos == pair[1].begin_pos)
        );

        let safe =
            parse_link_tooltip_text("safe\x1b]8;;https://example.com/hidden\x1b\\text\x1b[2J\0end")
                .expect("safe tooltip");
        assert_eq!(safe.text.as_ref(), "safetext end");
        assert!(!safe.text.contains('\x1b'));
    }

    #[test]
    fn osc8_compact_tooltip_key_is_supported() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;send:north?config=%7B%22t%22%3A%22Take%20the%20north%20exit%22%7D\x1b\\north\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        assert_eq!(
            link.action,
            LinkAction::ServerSend(std::sync::Arc::from("north"))
        );
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Take the north exit"),
                Some(std::sync::Arc::from("send:north"))
            ))
        );
    }

    #[test]
    fn osc8_reserved_config_is_stripped_even_when_malformed() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com/?x=1&config=not-json\x1b\\x\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/?x=1"))
        );
    }

    #[test]
    fn osc8_percent_encoded_config_key_remains_part_of_web_url() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com/?%63%6f%6e%66%69%67=value\x1b\\x\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from(
                "https://example.com/?%63%6f%6e%66%69%67=value"
            ))
        );
    }

    #[test]
    fn osc8_uri_may_contain_semicolons_and_bel_terminates() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com/a;b=1\x07x\x1b]8;;\x07\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/a;b=1"))
        );
    }

    #[test]
    fn osc8_link_continues_across_a_line_commit() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\one\ntwo\x1b]8;;\x1b\\!\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "one");
        assert_eq!(
            (lines[0].links[0].begin_pos, lines[0].links[0].end_pos),
            (0, 3)
        );
        assert_eq!(lines[1].text, "two!");
        assert_eq!(
            (lines[1].links[0].begin_pos, lines[1].links[0].end_pos),
            (0, 3),
            "the continuation restarts at column 0 and ends at the close"
        );
    }

    #[test]
    fn osc8_send_scheme_percent_decodes_into_server_send() {
        let mut h = harness();
        h.feed(b"\x1b]8;;send:say%20hello%2C%20world\x1b\\hi\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("say hello, world"))
        );
    }

    #[test]
    fn osc8_prompt_prefills_percent_decoded_text() {
        let mut h = harness();
        h.feed(b"\x1b]8;;prompt:cast%20fireball\x1b\\cast\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::Prompt(std::sync::Arc::from("cast fireball"))
        );
    }

    #[test]
    fn osc8_compact_disabled_link_keeps_tooltip_but_has_no_primary_action() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;send:north?config=%7B%22d%22%3Atrue%2C%22t%22%3A%22Unavailable%22%7D\x1b\\north\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        assert!(link.action.primary().is_none());
        assert!(link.action.menu().is_none());
        assert!(!link.action.opens_menu_on_left_click());
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Unavailable"),
                Some(std::sync::Arc::from("send:north"))
            ))
        );
    }

    #[test]
    fn osc8_compact_menu_supports_actions_separators_title_and_default_tooltip() {
        let mut h = harness();
        h.feed(
            b"\x1b]8;;send:look?config=%7B%22m%22%3A%5B%7B%22Look%22%3A%22send%3Alook%22%7D%2C%22-%22%2C%7B%22Cast%22%3A%22prompt%3Acast%2520fireball%22%7D%5D%2C%22ti%22%3A%22Actions%22%7D\x1b\\actions\x1b]8;;\x1b\\\n",
        );
        let lines = committed_lines(&h.actions());
        let link = &lines[0].links[0];
        let menu = link.action.menu().expect("an enabled menu");
        assert!(!link.action.opens_menu_on_left_click());
        assert_eq!(
            menu.title.as_ref().map(|title| title.text.as_ref()),
            Some("Actions")
        );
        assert_eq!(menu.items.len(), 3);
        assert!(matches!(menu.items[1], LinkMenuItem::Separator));
        assert!(matches!(
            &menu.items[2],
            LinkMenuItem::Action { label, action: LinkAction::Prompt(command) }
                if label.as_ref() == "Cast" && command.as_ref() == "cast fireball"
        ));
        assert_eq!(
            link.tooltip.as_ref().and_then(LinkTooltip::display),
            Some((
                std::sync::Arc::from("Right-click for menu"),
                Some(std::sync::Arc::from("send:look"))
            ))
        );
    }

    #[test]
    fn osc8_unsupported_schemes_render_plain_text() {
        let mut h = harness();
        h.feed(b"\x1b]8;;file:///etc/passwd\x1b\\name\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "name");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_oversized_uri_is_ignored() {
        let mut payload = b"\x1b]8;;https://example.com/".to_vec();
        payload.extend(std::iter::repeat_n(b'x', MAX_OSC8_URI_LEN));
        payload.extend_from_slice(b"\x1b\\text\x1b]8;;\x1b\\\n");
        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_dispatch_rejects_an_overlong_sequence_across_reads() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com/");
        h.feed(&vec![b'x'; MAX_OSC8_URI_LEN]);
        h.feed(b"still-discarded\x1b\\plain \x1b]8;;send:look\x1b\\look\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "plain look");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("look"))
        );
    }

    #[test]
    fn overlong_osc_selector_88_does_not_drop_the_next_link() {
        let mut payload = b"\x1b]88;;".to_vec();
        payload.extend(std::iter::repeat_n(b'x', MAX_OSC8_URI_LEN + 1));
        payload.extend_from_slice(b"\x1b\\plain ");
        payload.extend_from_slice(b"\x1b]8;;send:look\x1b\\look\x1b]8;;\x1b\\\n");

        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "plain look");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("look"))
        );
    }

    #[test]
    fn vtparse_osc_cap_rejects_truncated_uri_and_keeps_stream_live() {
        let mut payload = b"\x1b]8;;https://example.com/".to_vec();
        payload.extend(std::iter::repeat_n(b'x', 8192 + 1024));
        payload.extend_from_slice(b"\x1b\\plain ");
        payload.extend_from_slice(b"\x1b]8;;send:look\x1b\\look\x1b]8;;\x1b\\\n");

        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "plain look");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("look"))
        );
    }

    #[test]
    fn vtparse_osc_cap_does_not_dispatch_a_wrong_target_prefix() {
        const WRONG_PREFIX: &[u8] = b"send:quit";
        let params_len = 8192 - b"8".len() - WRONG_PREFIX.len();
        let mut payload = b"\x1b]8;".to_vec();
        payload.extend(std::iter::repeat_n(b'p', params_len));
        payload.extend_from_slice(b";send:quit-now\x1b\\text\x1b]8;;\x1b\\\n");

        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert!(
            lines[0].links.is_empty(),
            "a truncated send:quit prefix must never become live"
        );
    }

    #[test]
    fn vtparse_truncated_osc_closes_an_existing_link() {
        let mut payload = b"\x1b]8;;send:look\x1b\\linked".to_vec();
        payload.extend_from_slice(b"\x1b]8;");
        payload.extend(std::iter::repeat_n(b'p', 8192));
        payload.extend_from_slice(b";send:quit-now\x1b\\ plain\n");

        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "linked plain");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            &lines[0].text[lines[0].links[0].begin_pos..lines[0].links[0].end_pos],
            "linked"
        );
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("look"))
        );
    }

    #[test]
    fn oversized_non_osc8_command_does_not_sever_an_existing_link() {
        let mut payload = b"\x1b]8;;send:look\x1b\\before".to_vec();
        payload.extend_from_slice(b"\x1b]0;");
        payload.extend(std::iter::repeat_n(b'x', 8192));
        payload.extend_from_slice(b"\x1b\\after\x1b]8;;\x1b\\\n");

        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "beforeafter");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            &lines[0].text[lines[0].links[0].begin_pos..lines[0].links[0].end_pos],
            "beforeafter"
        );
    }

    #[test]
    fn osc8_uri_keeps_semicolons_beyond_vtparse_parameter_capacity() {
        let uri = format!("https://example.com/{}", ";segment".repeat(70));
        let payload = format!("\x1b]8;;{uri}\x1b\\open\x1b]8;;\x1b\\\n");

        let mut h = harness();
        h.feed(payload.as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from(uri))
        );
    }

    #[test]
    fn osc8_uri_at_byte_cap_is_accepted() {
        const PREFIX: &[u8] = b"https://example.com/";
        let mut payload = b"\x1b]8;;".to_vec();
        payload.extend_from_slice(PREFIX);
        payload.extend(std::iter::repeat_n(b'x', MAX_OSC8_URI_LEN - PREFIX.len()));
        payload.extend_from_slice(b"\x1b\\text\x1b]8;;\x1b\\\n");
        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert_eq!(lines[0].links.len(), 1);
    }

    #[test]
    fn osc8_multibyte_uri_does_not_panic() {
        // A URI of multibyte UTF-8 whose bytes straddle the scheme-prefix
        // lengths must not panic the scheme check; it is simply unlinked.
        let mut h = harness();
        h.feed("\x1b]8;;\u{e9}\u{e9}\u{e9}\u{e9}\x1b\\text\x1b]8;;\x1b\\\n".as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_link_survives_a_cr_overprint() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\old\rnew\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "new");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            (lines[0].links[0].begin_pos, lines[0].links[0].end_pos),
            (0, 3)
        );
    }
    // --- kitty graphics: wire layer only (no image table yet) ---

    fn raw_of(h: &mut Harness) -> (String, Option<String>) {
        let lines = committed_lines(&h.actions());
        assert_eq!(lines.len(), 1, "exactly one committed line expected");
        (lines[0].text.clone(), lines[0].raw().map(str::to_owned))
    }

    #[test]
    fn graphics_apc_is_cut_out_of_text_and_raw_capture() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\def\n");
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
    }

    #[test]
    fn graphics_apc_ended_by_8bit_st_is_cut_out_of_raw_capture() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x9cdef\n");
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
    }

    #[test]
    fn graphics_apc_without_st_backslash_keeps_the_following_escape() {
        // A client that ends the APC with a bare ESC followed by an SGR: the
        // ESC closes the APC and starts the CSI, which must survive in raw.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b[31mdef\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "abcdef");
        assert_eq!(lines[0].raw(), Some("abc\x1b[31mdef"));
    }

    #[test]
    fn oversized_graphics_chunk_is_cut_out_of_raw_capture() {
        let mut h = harness();
        let mut apc = b"abc\x1b_Ga=T,f=24,s=1,v=1,m=1;".to_vec();
        apc.extend(std::iter::repeat_n(b'A', 9000));
        apc.extend_from_slice(b"\x1b\\def\n");
        h.feed(&apc);
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
    }

    #[test]
    fn malformed_graphics_apc_is_still_cut_out_of_raw_capture() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=zz,f=1;AAAA\x1b\\def\n");
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
    }

    #[test]
    fn other_apc_strings_stay_in_raw_capture() {
        let mut h = harness();
        h.feed(b"abc\x1b_tmux;x\x1b\\def\n");
        assert_eq!(
            raw_of(&mut h),
            ("abcdef".into(), Some("abc\x1b_tmux;x\x1b\\def".into()))
        );
    }

    #[test]
    fn a_pending_overprint_mark_survives_raw_stripping() {
        let mut h = harness();
        h.feed(b"10%\r\x1b_Ga=p,i=1\x1b\\20%\n");
        assert_eq!(raw_of(&mut h), ("20%".into(), Some("20%".into())));
    }

    #[test]
    fn graphics_apc_leaves_a_following_backslash_character_alone() {
        // Only the `\` of `ESC \` is skipped: a literal backslash printed
        // after an 8-bit-ST-terminated APC is ordinary text.
        let mut h = harness();
        h.feed(b"a\x1b_Ga=p,i=1\x9c\\b\n");
        assert_eq!(raw_of(&mut h), ("a\\b".into(), Some("a\\b".into())));
    }

    // --- a graphics APC straddling read batches ---

    /// The partial lines' raw texts, then the joined line's.
    fn raw_fragments(actions: &[VtEvent]) -> (Vec<Option<String>>, Option<String>) {
        let partials = partial_lines(actions)
            .iter()
            .map(|line| line.raw().map(str::to_owned))
            .collect();
        let (line, _) = fragmented(actions);
        (partials, line.raw().map(str::to_owned))
    }

    #[test]
    fn a_graphics_apc_split_by_a_batch_end_stays_out_of_raw_capture() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1");
        h.processor.notify_end_of_buffer();
        h.feed(b"\x1b\\def\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:abc", "line:abcdef"]);
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abcdef".into())),
            "the open string's bytes wait for the fragment that ends it"
        );
        let (_, completion) = fragmented(&actions);
        assert_eq!(completion.raw(), Some("def"));
    }

    #[test]
    fn a_batch_ending_on_the_apc_s_closing_escape_keeps_it_back() {
        // The ESC ended the APC and may still be half of `ESC \`.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b");
        h.processor.notify_end_of_buffer();
        h.feed(b"\\def\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:abc", "line:abcdef"]);
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abcdef".into()))
        );
        // ...or the start of the next sequence, which then needs it.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b");
        h.processor.notify_end_of_buffer();
        h.feed(b"[31mdef\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:abc", "line:abcdef"]);
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abc\x1b[31mdef".into()))
        );
    }

    #[test]
    fn raw_capture_rising_inside_an_apc_skips_the_string_s_tail() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let wanted = Arc::new(AtomicBool::new(false));
        h.processor.set_raw_wanted_flag(wanted.clone());
        h.feed(b"abc\n\x1b_Ga=T,f=24,s=1,v=1,i=5;AA");
        // A trigger with a raw pattern appears while the upload is in
        // flight: capture rises at this batch boundary (no fragment is
        // open), inside the string.
        wanted.store(true, Ordering::Relaxed);
        h.processor.notify_end_of_buffer();
        h.feed(b"AA\x1b\\def\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].raw(), None, "captured nothing while unwanted");
        assert_eq!(
            (lines[1].text.as_str(), lines[1].raw()),
            (" def", Some("def")),
            "the placement's placeholder column is text, never raw"
        );
        // The upload itself is whole.
        let (_, request) = decodes.lock().unwrap().pop().expect("one decode");
        assert_eq!(request.data, [0, 0, 0]);
    }

    #[test]
    fn a_graphics_apc_across_three_batches_leaves_one_clean_join() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"abc\x1b_Ga=T,f=24,s=1,v=1,i=5;");
        h.processor.notify_end_of_buffer();
        h.feed(b"AA");
        h.processor.notify_end_of_buffer();
        h.feed(b"AA\x1b\\def\n");
        let actions = h.actions();
        assert_eq!(
            transcript(&actions),
            ["partial:abc", "line:abc def"],
            "a batch holding only string bytes flushes no partial; the \
             placement lands at the final chunk's cursor"
        );
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abcdef".into()))
        );
        let (_, request) = decodes.lock().unwrap().pop().expect("one decode");
        assert_eq!(request.data, [0, 0, 0]);
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert!(out.is_empty(), "the reply waits for the decode: {out:?}");
    }

    #[test]
    fn a_prompt_boundary_inside_an_apc_holds_its_bytes_back_too() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1");
        h.processor.commit_prompt();
        h.feed(b"\x1b\\def\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:abc", "line:def"]);
        assert_eq!(partial_lines(&actions)[0].raw(), Some("abc"));
        assert_eq!(committed_lines(&actions)[0].raw(), Some("def"));
    }

    #[test]
    fn another_apc_split_by_a_batch_end_keeps_all_its_bytes_in_raw() {
        let mut h = harness();
        h.feed(b"abc\x1b_tmux;x");
        h.processor.notify_end_of_buffer();
        h.feed(b"\x1b\\def\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (
                vec![Some("abc".into())],
                Some("abc\x1b_tmux;x\x1b\\def".into())
            )
        );
    }

    // --- held-back raw bytes against capture falls, boundaries and cost ---

    impl Harness {
        /// The connection's Ground-state bulk path: a run the byte loop
        /// validated as UTF-8 reaches the capture and the line in one
        /// piece, without the parser seeing it.
        fn feed_run(&mut self, run: &str) {
            assert!(self.parser.is_ground(), "a run only starts in Ground");
            self.processor.push_raw_incoming_run(run);
            self.processor.print_run(run);
        }
    }

    #[test]
    fn a_kept_back_escape_does_not_outlive_a_capture_fall() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let wanted = Arc::new(AtomicBool::new(true));
        let mut h = harness();
        h.processor.set_raw_wanted_flag(wanted.clone());
        // The string's closing ESC is kept back for its `\`...
        h.feed(b"abc\n\x1b_Ga=p,i=1\x1b");
        // ...but capture falls at this boundary, so it goes with the
        // buffer, and the `\` that follows is seen by nothing.
        wanted.store(false, Ordering::Relaxed);
        h.processor.notify_end_of_buffer();
        assert!(!h.processor.capture_raw() && h.processor.buf_raw.is_empty());
        h.feed(b"\\");
        wanted.store(true, Ordering::Relaxed);
        h.processor.notify_end_of_buffer();
        assert!(h.processor.capture_raw());
        // A validated run is captured whole: no stale verdict pops its
        // last byte (which would tear the character and, in release, feed
        // invalid UTF-8 to the unchecked raw text).
        h.feed_run("é");
        h.processor.notify_end_of_buffer();
        h.feed(b"\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[1].text.as_str(), lines[1].raw()), ("é", Some("é")));
    }

    #[test]
    fn held_apc_bytes_do_not_leak_into_raw_after_capture_falls_and_rises() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for prompt_boundary in [false, true] {
            let wanted = Arc::new(AtomicBool::new(true));
            let mut h = harness();
            h.processor.set_raw_wanted_flag(wanted.clone());
            h.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=5;AA");
            // Capture falls at a boundary inside the string: the bytes held
            // for the next fragment go with it, since nothing will strip
            // them while capture is off.
            wanted.store(false, Ordering::Relaxed);
            if prompt_boundary {
                h.processor.commit_prompt();
            } else {
                h.processor.notify_end_of_buffer();
            }
            assert!(
                h.processor.buf_raw.is_empty(),
                "prompt boundary: {prompt_boundary}"
            );
            h.feed(b"AA\x1b\\");
            wanted.store(true, Ordering::Relaxed);
            h.processor.notify_end_of_buffer();
            h.feed(b"def\n");
            let lines = committed_lines(&h.actions());
            assert_eq!(lines.len(), 1);
            assert_eq!(
                (lines[0].text.as_str(), lines[0].raw()),
                ("def", Some("def")),
                "prompt boundary: {prompt_boundary}"
            );
        }
    }

    #[test]
    fn an_apc_open_across_many_batches_moves_its_bytes_once() {
        // Linear by construction: a boundary that flushes nothing leaves
        // the buffer alone, so the string's bytes move to the head of the
        // buffer once (with the text before them flushed), however many
        // batches the string spans. A stray unterminated `ESC _` keeps
        // this up until an ESC or C1 byte arrives, so this is the cost of
        // every batch meanwhile.
        let mut h = harness();
        let open = b"\x1b_Gm=1;";
        h.feed(b"abc");
        h.feed(open);
        h.processor.notify_end_of_buffer();
        assert_eq!(h.processor.raw_bytes_moved, open.len());
        assert_eq!(h.processor.apc_raw_start, Some(0));
        let chunk = vec![b'A'; 16 * 1024];
        for _ in 0..32 {
            h.feed(&chunk);
            h.processor.notify_end_of_buffer();
            assert_eq!(h.processor.raw_bytes_moved, open.len());
            assert_eq!(h.processor.apc_raw_start, Some(0));
        }
        assert_eq!(h.processor.buf_raw.len(), open.len() + 32 * chunk.len());
        assert_eq!(transcript(&h.actions()), ["partial:abc"]);
        // The (oversized) string still ends cleanly.
        h.feed(b"\x1b\\def\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["line:abcdef"]);
        assert_eq!(fragmented(&actions).0.raw(), Some("abcdef"));
    }

    #[test]
    fn a_boundary_between_escape_and_underscore_keeps_the_escape_back() {
        // Every chunk of a chunked upload begins `ESC _`, and a read (or a
        // prompt boundary) can fall between the two bytes.
        for prompt_boundary in [false, true] {
            let mut h = harness();
            h.feed(b"abc\x1b");
            if prompt_boundary {
                h.processor.commit_prompt();
            } else {
                h.processor.notify_end_of_buffer();
            }
            h.feed(b"_Ga=p,i=1\x1b\\def\n");
            let actions = h.actions();
            assert_eq!(
                partial_lines(&actions)[0].raw(),
                Some("abc"),
                "prompt boundary: {prompt_boundary}"
            );
            let lines = committed_lines(&actions);
            let expected = if prompt_boundary { "def" } else { "abcdef" };
            assert_eq!(
                (lines[0].text.as_str(), lines[0].raw()),
                (expected, Some(expected)),
                "prompt boundary: {prompt_boundary}"
            );
        }
    }

    #[test]
    fn a_lone_trailing_escape_is_flushed_with_the_sequence_it_begins() {
        // A CSI...
        let mut h = harness();
        h.feed(b"abc\x1b");
        h.processor.notify_end_of_buffer();
        h.feed(b"[31mdef\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abc\x1b[31mdef".into()))
        );
        assert_eq!(fragmented(&actions).1.raw(), Some("\x1b[31mdef"));
        // ...an OSC...
        let mut h = harness();
        h.feed(b"abc\x1b");
        h.processor.notify_end_of_buffer();
        h.feed(b"]8;;http://x.y\x1b\\def\x1b]8;;\x1b\\\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (
                vec![Some("abc".into())],
                Some("abc\x1b]8;;http://x.y\x1b\\def\x1b]8;;\x1b\\".into())
            )
        );
        assert_eq!(fragmented(&actions).0.text, "abcdef");
        // ...or a stray `ESC \` with no string to end: wire bytes, kept.
        let mut h = harness();
        h.feed(b"abc\x1b");
        h.processor.notify_end_of_buffer();
        h.feed(b"\\def\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc".into())], Some("abc\x1b\\def".into()))
        );
    }

    #[test]
    fn a_flush_leaves_the_held_bytes_verified_on_their_own() {
        // The doubt a high byte cast on the buffer leaves with that byte,
        // so the fragment after it can borrow its raw text unchecked.
        let mut h = harness();
        h.feed("é\x1b_Ga=p,i=1".as_bytes());
        assert!(h.processor.raw_unverified);
        h.processor.notify_end_of_buffer();
        assert!(!h.processor.raw_unverified);
        h.feed(b"\x1b\\def\n");
        assert_eq!(
            raw_fragments(&h.actions()),
            (vec![Some("é".into())], Some("édef".into()))
        );
    }

    #[test]
    fn an_overprint_marked_after_the_kept_back_escape_is_clamped_when_it_goes() {
        // `ESC CR \`: the mark counted the kept-back ESC; once the `\`
        // takes it, the mark must not reach past the buffer, or the restart
        // would drop the new frame's first raw byte.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b\r\\def\n");
        assert_eq!(raw_of(&mut h), ("def".into(), Some("def".into())));
    }

    #[test]
    fn a_c1_introducer_ending_the_apc_begins_the_next_sequence() {
        // The CSI's own introducer ended the string: it is the next
        // sequence's and stays (a replacement character in the text form,
        // like any lone high byte).
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x9b31mdef\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "abcdef");
        assert_eq!(lines[0].raw(), Some("abc\u{FFFD}31mdef"));
        // Unlike an ESC it is not held back when a batch ends on it (a
        // trailing C1 value is usually a character's last byte, see
        // `held_raw_start`): it flushes with the partial, and the joined
        // line's raw text is the same.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x9b");
        h.processor.notify_end_of_buffer();
        assert!(h.processor.buf_raw.is_empty());
        h.feed(b"31mdef\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (
                vec![Some("abc\u{FFFD}".into())],
                Some("abc\u{FFFD}31mdef".into())
            )
        );
        assert_eq!(fragmented(&actions).1.raw(), Some("31mdef"));
    }

    #[test]
    fn a_second_escape_after_the_apc_s_own_supersedes_it() {
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b\x1b[31mdef\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "abcdef");
        assert_eq!(lines[0].raw(), Some("abc\x1b[31mdef"));
        // So does a C1 introducer...
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b\x9b31mdef\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "abcdef");
        assert_eq!(lines[0].raw(), Some("abc\u{FFFD}31mdef"));
        // ...while an 8-bit ST completes it, like `\`.
        let mut h = harness();
        h.feed(b"abc\x1b_Ga=p,i=1\x1b\x9cdef\n");
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
    }

    /// The connection's bulk path for a run of validated text: the raw
    /// capture first, then the print.
    fn feed_run(h: &mut Harness, run: &str) {
        h.processor.push_raw_incoming_run(run);
        h.processor.print_run(run);
    }

    /// Characters whose last UTF-8 byte has a C1 introducer's value: `Ð`
    /// is `C3 90`, `А` is `D0 90`, `П` is `D0 9F`, `ẞ` is `E1 BA 9E`, `語`
    /// is `E8 AA 9E`.
    const C1_TAILED: [&str; 5] = ["Ð", "А", "П", "ẞ", "語"];

    #[test]
    fn a_run_ending_in_a_c1_value_is_borrowed_whole_at_a_batch_end() {
        // The minimal case: a validated run is the whole capture, and its
        // raw text is borrowed unchecked. Holding its last byte back as an
        // introducer split it (a tripped assertion here; an invalid `&str`
        // in a release build).
        let mut h = harness();
        feed_run(&mut h, "Ð");
        h.processor.notify_end_of_buffer();
        assert!(h.processor.buf_raw.is_empty(), "nothing is held back");
        let partials = partial_lines(&h.actions());
        assert_eq!(partials.len(), 1);
        assert_eq!(
            (partials[0].text.as_str(), partials[0].raw()),
            ("Ð", Some("Ð"))
        );
    }

    #[test]
    fn a_boundary_after_a_character_ending_in_a_c1_value_keeps_it_whole() {
        // Through the bulk path (where the raw text is borrowed unchecked)
        // and the per-byte path (where it is re-decoded), at a batch end
        // and at a prompt boundary: the character flushes whole with the
        // partial, and what follows joins it as if nothing had split them.
        for text in C1_TAILED {
            for (bulk, prompt_boundary) in [(true, false), (true, true), (false, false)] {
                let case = format!("{text}, bulk: {bulk}, prompt boundary: {prompt_boundary}");
                let mut h = harness();
                feed_run(&mut h, "abc");
                if bulk {
                    feed_run(&mut h, text);
                } else {
                    h.feed(text.as_bytes());
                }
                if prompt_boundary {
                    h.processor.commit_prompt();
                } else {
                    h.processor.notify_end_of_buffer();
                }
                assert!(
                    h.processor.buf_raw.is_empty(),
                    "{case}: nothing is held back"
                );
                let partials = partial_lines(&h.actions());
                let whole = format!("abc{text}");
                assert_eq!(partials.len(), 1, "{case}");
                assert_eq!(
                    (partials[0].text.as_str(), partials[0].raw()),
                    (whole.as_str(), Some(whole.as_str())),
                    "{case}"
                );
                h.feed(b"def\n");
                let lines = committed_lines(&h.actions());
                let expected = if prompt_boundary {
                    "def".to_owned()
                } else {
                    format!("abc{text}def")
                };
                assert_eq!(
                    (lines[0].text.as_str(), lines[0].raw()),
                    (expected.as_str(), Some(expected.as_str())),
                    "{case}"
                );
            }
        }
    }

    /// A read batch, or a prompt, ending inside a character on the per-byte
    /// path: its bytes so far are held back for the fragment the rest
    /// completes, so neither fragment shows a replacement character and the
    /// joined line's raw text has the character whole, like its text. Two-,
    /// three- and four-byte characters, split after each byte.
    #[test]
    fn a_boundary_inside_a_character_holds_its_bytes_back_for_the_rest() {
        for text in ["é", "語", "🎉"] {
            let bytes = text.as_bytes();
            for split in 1..bytes.len() {
                for prompt_boundary in [false, true] {
                    let case = format!(
                        "{text} split after {split} of {} bytes, prompt boundary: {prompt_boundary}",
                        bytes.len()
                    );
                    let mut h = harness();
                    feed_run(&mut h, "ab");
                    h.feed(&bytes[..split]);
                    assert!(h.processor.raw_unverified, "{case}");
                    if prompt_boundary {
                        h.processor.commit_prompt();
                    } else {
                        h.processor.notify_end_of_buffer();
                    }
                    assert_eq!(
                        h.processor.buf_raw,
                        bytes[..split],
                        "{case}: the character's bytes so far are held back"
                    );
                    assert!(
                        h.processor.raw_unverified,
                        "{case}: the held bytes keep their doubt"
                    );
                    let partials = partial_lines(&h.actions());
                    assert_eq!(partials.len(), 1, "{case}");
                    assert_eq!(
                        (partials[0].text.as_str(), partials[0].raw()),
                        ("ab", Some("ab")),
                        "{case}: the partial ends before the character"
                    );
                    h.feed(&bytes[split..]);
                    h.feed(b"c\n");
                    let lines = committed_lines(&h.actions());
                    let expected = if prompt_boundary {
                        format!("{text}c")
                    } else {
                        format!("ab{text}c")
                    };
                    assert_eq!(
                        (lines[0].text.as_str(), lines[0].raw()),
                        (expected.as_str(), Some(expected.as_str())),
                        "{case}"
                    );
                    assert!(!h.processor.raw_unverified, "{case}: cleared with the line");
                }
            }
        }
    }

    /// A batch that brought nothing but the first bytes of a character
    /// emits no fragment (there is nothing to show) and keeps them for the
    /// next; a prompt boundary there closes the open line without them.
    #[test]
    fn a_batch_holding_only_the_start_of_a_character_flushes_nothing() {
        for prompt_boundary in [false, true] {
            let mut h = harness();
            feed_run(&mut h, "ab");
            h.processor.notify_end_of_buffer();
            h.feed(b"\xe8\xaa");
            if prompt_boundary {
                h.processor.commit_prompt();
            } else {
                h.processor.notify_end_of_buffer();
            }
            assert_eq!(h.processor.buf_raw, b"\xe8\xaa");
            let actions = h.actions();
            assert_eq!(partial_lines(&actions).len(), 1, "only the first batch's");
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, VtEvent::PromptBoundary))
                    .count(),
                usize::from(prompt_boundary)
            );
            h.feed(b"\x9ec\n");
            let lines = committed_lines(&h.actions());
            let expected = if prompt_boundary { "語c" } else { "ab語c" };
            assert_eq!(
                (lines[0].text.as_str(), lines[0].raw()),
                (expected, Some(expected))
            );
        }
    }

    /// A lone lead byte at the end of a batch is held like any start of a
    /// character; when the next byte does not continue it, the parser
    /// prints a replacement character for it and the raw text shows the
    /// same, in the fragment the verdict came in.
    #[test]
    fn a_lead_byte_the_next_batch_does_not_continue_flushes_as_a_replacement() {
        let mut h = harness();
        feed_run(&mut h, "ab");
        h.feed(b"\xe8");
        h.processor.notify_end_of_buffer();
        assert_eq!(h.processor.buf_raw, b"\xe8", "the lead byte is held");
        let partials = partial_lines(&h.actions());
        assert_eq!(
            (partials[0].text.as_str(), partials[0].raw()),
            ("ab", Some("ab"))
        );
        h.feed(b"c\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            (lines[0].text.as_str(), lines[0].raw()),
            ("ab\u{FFFD}c", Some("ab\u{FFFD}c"))
        );
        // Bytes no character can begin with are not held: the parser has
        // dealt with them already (a lone continuation byte in Ground is
        // the C1 control of its value, and prints nothing), and they flush
        // as what they are.
        let mut h = harness();
        feed_run(&mut h, "ab");
        h.feed(b"\x80");
        h.processor.notify_end_of_buffer();
        assert!(
            h.processor.buf_raw.is_empty(),
            "a lone continuation byte flushes"
        );
        let partials = partial_lines(&h.actions());
        assert_eq!(
            (partials[0].text.as_str(), partials[0].raw()),
            ("ab", Some("ab\u{FFFD}"))
        );
    }

    #[test]
    fn incomplete_utf8_tails_are_measured_as_the_parser_holds_them() {
        for (bytes, held) in [
            (&b""[..], 0),
            (b"a", 0),
            (b"ab\xc3", 1),
            (b"\xc3\xa9", 0),
            (b"\xe8", 1),
            (b"\xe8\xaa", 2),
            (b"\xe8\xaa\x9e", 0),
            (b"\xf0\x9f", 2),
            (b"\xf0\x9f\x8e", 3),
            (b"\xf0\x9f\x8e\x89", 0),
            (b"\xc3\xa9\xe8", 1),
            (b"\xe8\xaa\x9e\xf0\x9f", 2),
            // Nothing a character can begin with, or continue: replaced
            // by the parser already.
            (b"\x80", 0),
            (b"\xaa\x9e", 0),
            (b"\xff", 0),
            (b"\xc0", 0),
            (b"\xe0\x80", 0),
            (b"\xed\xa0", 0),
            (b"\xf4\x90", 0),
            (b"\xf5\x80", 0),
        ] {
            assert_eq!(incomplete_utf8_tail(bytes), held, "{bytes:?}");
            assert_eq!(held_raw_start(bytes, None), bytes.len() - held, "{bytes:?}");
        }
        // An open APC or a trailing `ESC` take precedence, as before.
        assert_eq!(held_raw_start(b"ab\x1b_Ga=p\xe8", Some(2)), 2);
        assert_eq!(held_raw_start(b"ab\xe8\x1b", None), 3);
    }

    #[test]
    fn a_character_ending_in_a_c1_value_after_a_stripped_apc_stays_whole() {
        // The `\` settled the string's kept-back `ESC`, the parser is back
        // in Ground and the character comes through the bulk path: its
        // last byte is the character's, not an introducer to hold back.
        for text in C1_TAILED {
            for prompt_boundary in [false, true] {
                let case = format!("{text}, prompt boundary: {prompt_boundary}");
                let mut h = harness();
                feed_run(&mut h, "abc");
                h.feed(b"\x1b_Ga=p,i=1\x1b\\");
                feed_run(&mut h, text);
                if prompt_boundary {
                    h.processor.commit_prompt();
                } else {
                    h.processor.notify_end_of_buffer();
                }
                assert!(
                    h.processor.buf_raw.is_empty(),
                    "{case}: nothing is held back"
                );
                let partials = partial_lines(&h.actions());
                let whole = format!("abc{text}");
                assert_eq!(partials.len(), 1, "{case}");
                assert_eq!(
                    (partials[0].text.as_str(), partials[0].raw()),
                    (whole.as_str(), Some(whole.as_str())),
                    "{case}"
                );
                h.feed(b"def\n");
                let lines = committed_lines(&h.actions());
                let expected = if prompt_boundary {
                    "def".to_owned()
                } else {
                    format!("abc{text}def")
                };
                assert_eq!(
                    (lines[0].text.as_str(), lines[0].raw()),
                    (expected.as_str(), Some(expected.as_str())),
                    "{case}"
                );
            }
        }
        // An APC ended by an 8-bit ST leaves the parser in Ground too; the
        // capture is unverified from the ST, and re-decodes whole.
        let mut h = harness();
        feed_run(&mut h, "abc");
        h.feed(b"\x1b_Ga=p,i=1\x9c");
        feed_run(&mut h, "語");
        h.processor.notify_end_of_buffer();
        h.feed(b"def\n");
        let actions = h.actions();
        assert_eq!(
            raw_fragments(&actions),
            (vec![Some("abc語".into())], Some("abc語def".into()))
        );
    }

    #[test]
    fn a_c1_value_inside_a_character_never_ends_an_apc() {
        // `œ` is `C5 9C`. In text the parser's UTF-8 decoding takes the
        // `9C` as the character's, so no string ends on it and nothing is
        // stripped around it...
        let mut h = harness();
        feed_run(&mut h, "abcœ");
        h.feed(b"\x1b_Ga=p,i=1\x1b\\");
        feed_run(&mut h, "œdef");
        h.feed(b"\n");
        assert_eq!(raw_of(&mut h), ("abcœœdef".into(), Some("abcœœdef".into())));
        // ...and the same through the per-byte path, where the parser
        // decodes the character itself.
        let mut h = harness();
        h.feed("abcœ\x1b_Ga=p,i=1\x1b\\œdef\n".as_bytes());
        assert_eq!(raw_of(&mut h), ("abcœœdef".into(), Some("abcœœdef".into())));
        // Inside the string the parser reads bytes one by one: the `C5` is
        // ignored there and the `9C` is ST, and both go with the string.
        let mut h = harness();
        feed_run(&mut h, "abc");
        h.feed(b"\x1b_Ga=p,i=1\xc5\x9cdef\n");
        assert_eq!(raw_of(&mut h), ("abcdef".into(), Some("abcdef".into())));
        // A C1 introducer ending the string after a character with a C1
        // tail: the introducer is the next sequence's, the character is
        // whole.
        let mut h = harness();
        feed_run(&mut h, "abc語");
        h.feed(b"\x1b_Ga=p,i=1\x9b31mdef\n");
        assert_eq!(
            raw_of(&mut h),
            ("abc語def".into(), Some("abc語\u{FFFD}31mdef".into()))
        );
    }

    #[test]
    fn an_esc_cutting_a_utf8_sequence_short_begins_its_sequence() {
        // The parser (the patched vtparse) prints one replacement
        // character for the truncated sequence and then reads the `ESC`
        // afresh. A CSI: the SGR applies to what follows.
        let mut h = harness();
        h.feed(b"\xe3\x1b[31mdef\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "\u{FFFD}def");
        assert_eq!(lines[0].raw(), Some("\u{FFFD}\x1b[31mdef"));
        assert!(lines[0].spans.iter().any(|span| matches!(
            span.style.fg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            }
        ) && &lines[0].text
            [span.begin_pos..span.end_pos]
            == "def"));
        // A graphics APC: cut out of the text and the raw capture like any
        // other, rather than printed as text.
        let mut h = harness();
        h.feed(b"\xe3\x1b_Ga=d\x1b\\x\r\n");
        assert_eq!(
            raw_of(&mut h),
            ("\u{FFFD}x".into(), Some("\u{FFFD}x".into()))
        );
        // Plain ASCII cutting a sequence short prints after the
        // replacement character, whatever the sequence's length.
        let mut h = harness();
        h.feed(b"\xe3abc\xf0\x9f\x98d\n");
        assert_eq!(
            raw_of(&mut h),
            (
                "\u{FFFD}abc\u{FFFD}d".into(),
                Some("\u{FFFD}abc\u{FFFD}d".into())
            )
        );
    }

    fn replies_after(h: &mut Harness, bytes: &[u8]) -> Vec<u8> {
        h.feed(bytes);
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        out
    }

    #[test]
    fn da1_is_answered_only_with_the_full_protocol_on() {
        let mut h = harness();
        assert!(
            replies_after(&mut h, b"\x1b[c").is_empty(),
            "off by default"
        );
        h.processor
            .set_graphics_mode(crate::graphics::GraphicsMode::DisplayOnly);
        assert!(replies_after(&mut h, b"\x1b[c").is_empty());
        h.processor
            .set_graphics_mode(crate::graphics::GraphicsMode::Full);
        assert_eq!(replies_after(&mut h, b"\x1b[c"), b"\x1b[?62;c");
        assert_eq!(replies_after(&mut h, b"\x1b[0c"), b"\x1b[?62;c");
        assert!(
            replies_after(&mut h, b"\x1b[>c").is_empty(),
            "DA2 is a different question"
        );
        assert!(replies_after(&mut h, b"\x1b[?c").is_empty());
        assert!(!h.processor.has_replies());
        // The query bytes never reach the text.
        h.feed(b"x\n");
        assert_eq!(raw_of(&mut h).0, "x");
    }

    #[test]
    fn size_reports_use_the_shared_geometry() {
        use crate::responders::pack_dims;
        use std::sync::atomic::AtomicU32;
        let mut h = harness();
        h.processor
            .set_graphics_mode(crate::graphics::GraphicsMode::Full);
        // No geometry shared: the default grid, and the 9x18 fallback cell
        // placements are laid out with until the pane measures a font, so
        // the reports describe what is drawn rather than saying zero.
        assert_eq!(replies_after(&mut h, b"\x1b[18t"), b"\x1b[8;24;80t");
        assert_eq!(replies_after(&mut h, b"\x1b[16t"), b"\x1b[6;18;9t");
        assert_eq!(replies_after(&mut h, b"\x1b[14t"), b"\x1b[4;432;720t");

        let window = Arc::new(AtomicU32::new(pack_dims(100, 40)));
        let cell = Arc::new(AtomicU32::new(pack_dims(
            graphics::cell_fixed(9.0),
            graphics::cell_fixed(19.0),
        )));
        h.processor
            .set_terminal_geometry(window.clone(), cell.clone());
        assert_eq!(replies_after(&mut h, b"\x1b[18t"), b"\x1b[8;40;100t");
        assert_eq!(replies_after(&mut h, b"\x1b[16t"), b"\x1b[6;19;9t");
        assert_eq!(replies_after(&mut h, b"\x1b[14t"), b"\x1b[4;760;900t");
        // A fractional cell reports its whole pixels: 9.6 px cells say 9, so
        // 100 columns report 900 px, inside the 960 px they really span.
        cell.store(
            pack_dims(graphics::cell_fixed(9.6), graphics::cell_fixed(19.2)),
            std::sync::atomic::Ordering::Relaxed,
        );
        assert_eq!(replies_after(&mut h, b"\x1b[16t"), b"\x1b[6;19;9t");
        assert_eq!(replies_after(&mut h, b"\x1b[14t"), b"\x1b[4;760;900t");
        // The runtime updates the cells in place; the next report sees it.
        window.store(pack_dims(80, 24), std::sync::atomic::Ordering::Relaxed);
        assert_eq!(replies_after(&mut h, b"\x1b[18t"), b"\x1b[8;24;80t");
        // Other XTWINOPS requests stay silent.
        assert!(replies_after(&mut h, b"\x1b[8;50;100t").is_empty());
        assert!(replies_after(&mut h, b"\x1b[t").is_empty());
        h.processor
            .set_graphics_mode(crate::graphics::GraphicsMode::Off);
        assert!(replies_after(&mut h, b"\x1b[16t").is_empty());
    }

    // --- decode completion and reply order ---

    type Decodes = Arc<std::sync::Mutex<Vec<(graphics::DecodeTicket, graphics::DecodeRequest)>>>;

    /// A host that only records what it was asked to decode; the test plays
    /// the completion back itself.
    #[derive(Debug)]
    struct RecordingHost(Decodes);

    impl graphics::GraphicsHost for RecordingHost {
        fn decode(&mut self, ticket: graphics::DecodeTicket, request: graphics::DecodeRequest) {
            self.0.lock().unwrap().push((ticket, request));
        }

        fn read_media(
            &mut self,
            _medium: graphics::Medium,
            _name: &[u8],
            _offset: u32,
            _size: u32,
        ) -> Result<Vec<u8>, graphics::GraphicsError> {
            Err(graphics::GraphicsError::bad_file())
        }
    }

    fn hosted(mode: graphics::GraphicsMode) -> (Harness, Decodes) {
        let mut h = harness();
        h.processor.set_graphics_mode(mode);
        let decodes: Decodes = Arc::default();
        h.processor
            .set_graphics_host(Box::new(RecordingHost(decodes.clone())));
        (h, decodes)
    }

    fn one_pixel() -> graphics::DecodedPixels {
        graphics::DecodedPixels {
            width: 1,
            height: 1,
            rgba: bytes::Bytes::from_static(&[0, 0, 0, 255]),
        }
    }

    #[test]
    fn a_query_answer_waits_behind_a_transmit_still_decoding() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=7;AAAA\x1b\\\x1b[c");
        assert!(
            !h.processor.has_replies(),
            "DA1's answer is queued behind the parked transmit reply"
        );
        let (ticket, request) = decodes.lock().unwrap().pop().expect("one decode requested");
        assert_eq!(
            request,
            graphics::DecodeRequest {
                format: graphics::Format::Rgb,
                compression: graphics::Compression::None,
                width: 1,
                height: 1,
                data: vec![0, 0, 0],
                compose: None,
            }
        );
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b_Gi=7;OK\x1b\\\x1b[?62;c");
        assert!(matches!(h.actions().last(), Some(VtEvent::RequestRepaint)));
    }

    #[test]
    fn a_frame_waits_for_its_root_and_a_control_asks_for_a_repaint() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(
            b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA\x1b\\\
              \x1b_Ga=f,f=24,s=1,v=1,i=7,z=50,c=1;AAAA\x1b\\\
              \x1b_Ga=a,i=7,s=3\x1b\\",
        );
        assert_eq!(
            decodes.lock().unwrap().len(),
            1,
            "the frame waits for the root"
        );
        assert!(
            !h.actions()
                .iter()
                .any(|action| matches!(action, VtEvent::RequestRepaint)),
            "a control still waiting changes nothing yet"
        );
        let (ticket, _) = decodes.lock().unwrap().remove(0);
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let (ticket, request) = decodes
            .lock()
            .unwrap()
            .pop()
            .expect("the frame goes to the host once the root landed");
        let compose = request.compose.expect("a frame decode");
        assert_eq!((compose.base, compose.canvas), (Some(one_pixel()), (1, 1)));
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let image = graphics_state(&h).unwrap().image(7).unwrap();
        assert_eq!(image.animation().state, graphics::AnimationState::Running);
        assert_eq!(image.frame(2).unwrap().gap_ms, 50);
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b_Gi=7;OK\x1b\\\x1b_Gi=7,r=2;OK\x1b\\");
        // A control on an idle image repaints at once, with no decode.
        h.actions();
        h.feed(b"\x1b_Ga=a,i=7,s=1\x1b\\");
        assert!(
            h.actions()
                .iter()
                .any(|action| matches!(action, VtEvent::RequestRepaint))
        );
        assert_eq!(image.animation().state, graphics::AnimationState::Stopped);
        assert!(decodes.lock().unwrap().is_empty());
    }

    #[test]
    fn a_frame_delete_queued_behind_a_decode_repaints_once_when_it_runs() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(
            b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA\x1b\\\
              \x1b_Ga=f,f=24,s=1,v=1,i=7;AAAA\x1b\\\
              \x1b_Ga=d,d=f,i=7,r=1\x1b\\",
        );
        let repaints = |actions: &[VtEvent]| {
            actions
                .iter()
                .filter(|action| matches!(action, VtEvent::RequestRepaint))
                .count()
        };
        assert_eq!(repaints(&h.actions()), 0, "everything waits for the root");
        let (ticket, _) = decodes.lock().unwrap().remove(0);
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        assert_eq!(repaints(&h.actions()), 1, "the root landed");
        let (ticket, _) = decodes.lock().unwrap().remove(0);
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        assert_eq!(
            repaints(&h.actions()),
            1,
            "the frame landed and the delete ran behind it: one repaint for both"
        );
        let image = graphics_state(&h).unwrap().image(7).unwrap();
        assert_eq!(
            image.frame_count(),
            1,
            "the root went, leaving frame 2 as the root"
        );
        h.feed(b"\x1b_Ga=p,i=99\x1b\\");
        assert_eq!(
            repaints(&h.actions()),
            0,
            "the delete's repaint is spent: an unrelated command announces nothing"
        );
        assert!(decodes.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_or_silenced_decode_settles_its_slot_in_place() {
        // `q=1` suppresses OK but still reports the failure, in order.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b[c\x1b_Ga=T,f=24,s=1,v=1,i=7,q=1;AAAA\x1b\\\x1b[16t");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(
            out, b"\x1b[?62;c",
            "what came before the transmit goes out at once"
        );
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Err(graphics::GraphicsError::invalid("no")));
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[?62;c\x1b_Gi=7;EINVAL:no\x1b\\\x1b[6;18;9t");

        // `q=2` says nothing either way, but the size report still waits.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=7,q=2;AAAA\x1b\\\x1b[16t");
        assert!(!h.processor.has_replies());
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[6;18;9t");
    }

    #[test]
    fn a_malformed_transmit_answers_at_once_and_asks_for_no_decode() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b[c\x1b_Ga=T,f=24,i=9;AAAA\x1b\\");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(
            out,
            b"\x1b[?62;c\x1b_Gi=9;EINVAL:packed pixel data needs non-zero s and v\x1b\\"
        );
        assert!(decodes.lock().unwrap().is_empty());
        // A command without an id gets no reply at all.
        h.feed(b"\x1b_Ga=T,f=24;AAAA\x1b\\");
        assert!(!h.processor.has_replies());
    }

    #[test]
    fn display_only_decodes_but_never_answers() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::DisplayOnly);
        h.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=7;AAAA\x1b\\\x1b[c");
        let (ticket, _) = decodes.lock().unwrap().pop().expect("still decoded");
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert!(out.is_empty(), "{out:?}");
        // Without a host nothing is decoded and nothing owed.
        let mut h = harness();
        h.processor.set_graphics_mode(graphics::GraphicsMode::Full);
        h.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=7;AAAA\x1b\\\x1b[c");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[?62;c");
    }

    #[test]
    fn a_query_without_an_id_is_refused_without_a_decode() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b_Ga=q,f=24,s=1,v=1;AAAA\x1b\\\x1b[c");
        assert!(decodes.lock().unwrap().is_empty(), "nothing to answer for");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[?62;c", "and nothing holds DA1 back");
        // An image number is no id: kitty refuses that query the same way.
        h.feed(b"\x1b_Ga=q,f=24,s=1,v=1,I=3;AAAA\x1b\\\x1b[c");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[?62;c");
        // With an id it is checked and answered at once, still without a
        // decode.
        h.feed(b"\x1b_Ga=q,f=24,s=1,v=1,i=3;AAAA\x1b\\\x1b[c");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b_Gi=3;OK\x1b\\\x1b[?62;c");
        assert!(decodes.lock().unwrap().is_empty());
    }

    #[test]
    fn an_indexed_format_probe_gets_kitty_s_answer() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"\x1b_Ga=q,i=31,s=1,v=1,f=8;AA==\x1b\\");
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b_Gi=31;EINVAL:Unknown image format: 8\x1b\\");
        assert!(decodes.lock().unwrap().is_empty());
    }

    // --- placements through the processor ---

    /// The placements a line's inline objects carry, downcast as the UI's
    /// resolver does.
    fn placements_of(line: &super::StyledLine) -> Vec<Arc<graphics::Placement>> {
        line.objects
            .iter()
            .flat_map(|objects| objects.iter())
            .map(|object| {
                object
                    .resource
                    .clone()
                    .downcast::<graphics::Placement>()
                    .expect("a graphics placement")
            })
            .collect()
    }

    fn graphics_state(h: &Harness) -> Option<&graphics::GraphicsState> {
        h.processor.graphics.as_deref()
    }

    /// `ESC _ G <keys> ; <base64 of len zero bytes> ESC \`.
    fn transmit(keys: &str, len: usize) -> Vec<u8> {
        format!(
            "\x1b_G{keys};{}\x1b\\",
            graphics::encode_payload(&vec![0; len])
        )
        .into_bytes()
    }

    /// Share a whole-pixel cell of `width` x `height` with the processor.
    fn with_cell(h: &mut Harness, width: u16, height: u16) {
        use crate::responders::pack_dims;
        use std::sync::atomic::AtomicU32;
        h.processor.set_terminal_geometry(
            Arc::new(AtomicU32::new(pack_dims(80, 24))),
            Arc::new(AtomicU32::new(pack_dims(
                width * graphics::CELL_FIXED_SCALE,
                height * graphics::CELL_FIXED_SCALE,
            ))),
        );
    }

    #[test]
    fn a_transmit_and_display_lands_as_an_inline_object_at_the_cursor() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        h.feed(b"ab");
        h.feed(&transmit("a=T,i=1,p=2,f=24,s=25,v=30", 25 * 30 * 3));
        h.feed(b"cd\n");
        let actions = h.actions();
        let line = &committed_lines(&actions)[0];
        assert_eq!(
            line.text, "ab   cd",
            "25 px over 10 px cells: three columns"
        );
        assert_eq!(line.raw(), Some("abcd"));
        let objects = line.objects.as_ref().expect("one object");
        assert_eq!(objects[0].range, 2..5);
        assert_eq!(objects[0].baseline, Baseline::LastRow);
        let placement = placements_of(line).pop().unwrap();
        assert_eq!((placement.id, placement.image.id), (2, 1));
        assert_eq!(
            (placement.geometry.columns, placement.geometry.rows),
            (3, 2)
        );
        assert_eq!(placement.image.data(), graphics::ImageData::Pending);
        // The decode lands: the record is ready and the reply goes out.
        let (ticket, request) = decodes.lock().unwrap().pop().unwrap();
        assert_eq!((request.width, request.height), (25, 30));
        h.processor.graphics_decode_complete(
            ticket,
            Ok(graphics::DecodedPixels {
                width: 25,
                height: 30,
                rgba: vec![0; 25 * 30 * 4].into(),
            }),
        );
        assert!(placement.pixels().is_some());
        assert!(matches!(h.actions().last(), Some(VtEvent::RequestRepaint)));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b_Gi=1,p=2;OK\x1b\\");
    }

    #[test]
    fn a_chunked_upload_lands_at_the_final_chunk_s_cursor() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(b"abc\x1b_Ga=T,i=1,f=24,s=2,v=1,m=1;AAAA\x1b\\def");
        h.feed(b"\x1b_Gm=0;AAAA\x1b\\ghi\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, "abcdef ghi");
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 6..7);
        assert_eq!(decodes.lock().unwrap()[0].1.data, [0; 6]);
    }

    #[test]
    fn placements_use_the_reported_cell_or_the_fallback() {
        let (mut h, _) = hosted(graphics::GraphicsMode::Full);
        h.feed(&transmit("a=T,f=24,s=18,v=36", 18 * 36 * 3));
        h.feed(b"|\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, "  |", "no cell reported yet: 9x18");
        assert_eq!(placements_of(line)[0].geometry.rows, 2);
        with_cell(&mut h, 6, 12);
        h.feed(&transmit("a=T,f=24,s=18,v=36", 18 * 36 * 3));
        h.feed(b"|\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, "   |");
        assert_eq!(placements_of(line)[0].geometry.rows, 3);
    }

    #[test]
    fn display_only_shows_placements_and_answers_nothing() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::DisplayOnly);
        h.feed(&transmit("a=T,i=1,f=24,s=1,v=1", 3));
        h.feed(b"\x1b_Ga=p,i=1\x1b\\\x1b_Ga=p,i=9\x1b\\\x1b_Ga=q,i=2,e=T\x1b\\\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(placements_of(line).len(), 2);
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn without_display_or_a_host_commands_are_parsed_and_dropped() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Off);
        h.feed(&transmit("a=T,i=1,f=24,s=1,v=1", 3));
        h.feed(b"x\x1b_Ga=q,i=2,e=T\x1b\\\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!((line.text.as_str(), line.objects.is_none()), ("x", true));
        assert!(decodes.lock().unwrap().is_empty());
        assert!(graphics_state(&h).is_none(), "no state is ever created");

        let mut h = harness();
        h.processor.set_graphics_mode(graphics::GraphicsMode::Full);
        h.feed(&transmit("a=T,i=1,f=24,s=1,v=1", 3));
        h.feed(b"x\x1b_Ga=q,i=2,e=T\x1b\\\x1b[c\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!((line.text.as_str(), line.objects.is_none()), ("x", true));
        assert!(graphics_state(&h).is_none());
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert_eq!(out, b"\x1b[?62;c", "a client sees no graphics support");
    }

    #[test]
    fn the_spec_s_detection_probe_gets_its_answers_in_order() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let out = replies_after(
            &mut h,
            b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\
              \x1b_Ga=q,i=32,e=T,U=1\x1b\\\
              \x1b_Ga=q,i=33,e=f\x1b\\\
              \x1b[c",
        );
        assert_eq!(
            out, b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32,e=T,U=1;OK\x1b\\\x1b_Gi=33,e=f;OK\x1b\\\x1b[?62;c",
            "U=1 is echoed: the client's check for placeholder support; e=f: for animation"
        );
        assert!(decodes.lock().unwrap().is_empty());
    }

    #[test]
    fn an_upload_cut_short_by_can_is_refused_rather_than_decoded() {
        // CAN ends the APC early, and vtparse dispatches what arrived.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let out = replies_after(&mut h, b"\x1b_Ga=T,i=4,f=24,s=2,v=1;AAAA\x18x\n");
        assert_eq!(
            out,
            b"\x1b_Gi=4;ENODATA:Insufficient image data: 3 < 6\x1b\\"
        );
        assert!(decodes.lock().unwrap().is_empty());
        assert_eq!(committed_lines(&h.actions())[0].text, "x");
    }

    #[test]
    fn an_oversized_command_on_a_session_without_graphics_state_allocates_none() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let mut bytes = b"\x1b_Ga=T,i=1,f=24,s=100,v=100;".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', 9000));
        bytes.extend_from_slice(b"\x1b\\x\n");
        let out = replies_after(&mut h, &bytes);
        assert!(out.is_empty(), "not worth a state to refuse: {out:?}");
        assert!(graphics_state(&h).is_none());
        assert!(decodes.lock().unwrap().is_empty());
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, "x");
        assert_eq!(line.raw(), Some("x"), "but still cut out of raw");
    }

    #[test]
    fn a_batch_of_transmits_is_placed_and_answered_whole() {
        // Decodes report back only between read batches: one batch of a
        // hundred icons must not lose any to a bound on decodes in flight.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let mut bytes = Vec::new();
        for id in 1..=100 {
            bytes.extend_from_slice(&transmit(&format!("a=T,i={id},f=24,s=1,v=1"), 3));
            bytes.push(b'\n');
        }
        h.feed(&bytes);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines.len(), 100);
        assert!(lines.iter().all(|line| placements_of(line).len() == 1));
        let requested: Vec<_> = std::mem::take(&mut *decodes.lock().unwrap());
        assert_eq!(requested.len(), 100);
        assert!(!h.processor.has_replies());
        for (ticket, _) in requested {
            h.processor
                .graphics_decode_complete(ticket, Ok(one_pixel()));
        }
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        let expected: Vec<u8> = (1..=100)
            .flat_map(|id| format!("\x1b_Gi={id};OK\x1b\\").into_bytes())
            .collect();
        assert_eq!(out, expected);
        assert!(
            lines
                .iter()
                .all(|line| placements_of(line)[0].pixels().is_some())
        );
    }

    #[test]
    fn a_put_on_a_full_line_is_refused_and_the_next_line_takes_it() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(&transmit("a=t,i=1,f=24,s=1,v=1", 3));
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        for _ in 0..MAX_INLINE_OBJECTS {
            h.processor
                .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
                .unwrap();
        }
        let out = replies_after(&mut h, b"\x1b_Ga=p,i=1,p=65\x1b\\\n");
        assert_eq!(
            out,
            b"\x1b_Gi=1,p=65;ENOSPC:too many placements on this line\x1b\\"
        );
        let full = &committed_lines(&h.actions())[0];
        assert_eq!(full.objects.as_ref().unwrap().len(), MAX_INLINE_OBJECTS);
        assert_eq!(full.text.len(), MAX_INLINE_OBJECTS, "no placeholder for it");
        let out = replies_after(&mut h, b"\x1b_Ga=p,i=1,p=66\x1b\\\n");
        assert_eq!(out, b"\x1b_Gi=1,p=66;OK\x1b\\");
        let next = &committed_lines(&h.actions())[0];
        assert_eq!(placements_of(next)[0].id, 66);
    }

    #[test]
    fn a_pending_overprint_makes_room_on_a_full_line() {
        // Sixty-four placements, `\r`, then a put: the overprint restarts the
        // line, so the put has the whole line and is not refused for the
        // frame it replaces.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        h.feed(&transmit("a=t,i=1,f=24,s=1,v=1", 3));
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        for _ in 0..MAX_INLINE_OBJECTS {
            h.processor
                .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
                .unwrap();
        }
        h.feed(b"\r");
        let out = replies_after(&mut h, b"\x1b_Ga=p,i=1,p=65\x1b\\\n");
        assert_eq!(out, b"\x1b_Gi=1,p=65;OK\x1b\\");
        let line = &committed_lines(&h.actions())[0];
        let placements = placements_of(line);
        assert_eq!(placements.len(), 1, "the superseded frame's objects went");
        assert_eq!(placements[0].id, 65);
        assert_eq!(line.text, " ", "one placeholder column");
    }

    #[test]
    fn switching_graphics_off_silences_what_is_owed_and_keeps_what_is_shown() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        // A placement whose image is still decoding, DA1 queued behind the
        // transmit's reply, and an upload half way through.
        h.feed(&transmit("a=T,i=1,f=24,s=1,v=1", 3));
        h.feed(b"\n\x1b[c\x1b_Ga=T,i=2,f=24,s=2,v=1,m=1;AAAA\x1b\\");
        assert!(graphics_state(&h).unwrap().upload_in_flight());
        assert!(!h.processor.has_replies());
        h.processor.set_graphics_mode(graphics::GraphicsMode::Off);
        assert!(
            !graphics_state(&h).unwrap().upload_in_flight(),
            "the upload is abandoned"
        );
        // Its final chunk is dropped unseen, and the command after it too.
        h.feed(b"\x1b_Gm=0;AAAA\x1b\\");
        h.feed(&transmit("a=T,i=3,f=24,s=1,v=1", 3));
        h.feed(b"x\n");
        let lines = committed_lines(&h.actions());
        let shown = placements_of(&lines[0])
            .pop()
            .expect("placed before the switch");
        assert_eq!(
            (lines[1].text.as_str(), lines[1].objects.is_none()),
            ("x", true)
        );
        assert_eq!(decodes.lock().unwrap().len(), 1, "only image 1 was decoded");
        // The decode in flight lands — the placement made before the switch
        // fills in — and nothing is written for it or for the DA1 behind it.
        let (ticket, _) = decodes.lock().unwrap().pop().unwrap();
        h.processor
            .graphics_decode_complete(ticket, Ok(one_pixel()));
        assert!(shown.pixels().is_some());
        assert!(!h.processor.has_replies());
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
        assert!(out.is_empty(), "{out:?}");
        let state = graphics_state(&h).unwrap();
        assert_eq!(state.decodes_in_flight(), 0);
        assert!(state.image(3).is_none(), "nothing stored while off");
        // Back on, the table is where it was and the queue flows.
        h.processor.set_graphics_mode(graphics::GraphicsMode::Full);
        let out = replies_after(&mut h, b"\x1b_Ga=p,i=1,p=4\x1b\\\x1b[c");
        assert_eq!(out, b"\x1b_Gi=1,p=4;OK\x1b\\\x1b[?62;c");
    }

    #[test]
    fn an_oversized_chunk_abandons_the_upload_and_is_refused() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        let mut bytes = b"\x1b_Ga=T,i=1,f=24,s=100,v=100,m=1;AAAA\x1b\\\x1b_Gm=1;".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', 9000));
        bytes.extend_from_slice(b"\x1b\\\x1b_Gm=0;AAAA\x1b\\done\n");
        let out = replies_after(&mut h, &bytes);
        assert_eq!(
            out,
            b"\x1b_Gi=1;EINVAL:graphics command exceeds the size limit\x1b\\"
        );
        assert!(decodes.lock().unwrap().is_empty());
        let state = graphics_state(&h).unwrap();
        assert!(!state.upload_in_flight() && state.image(1).is_none());
        assert_eq!(committed_lines(&h.actions())[0].text, "done");
    }

    #[test]
    fn a_delete_leaves_the_placement_s_box_in_its_line() {
        let (mut h, _) = hosted(graphics::GraphicsMode::Full);
        h.feed(&transmit("a=T,i=1,f=24,s=9,v=18", 9 * 18 * 3));
        h.feed(b"x\n");
        let line = &committed_lines(&h.actions())[0];
        h.feed(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        let placement = placements_of(line).pop().unwrap();
        assert!(placement.is_deleted() && placement.image.is_evicted());
        assert_eq!(line.text, " x", "nothing reflows");
    }

    // --- objects born on the socket thread ---

    fn objects_of(line: &super::StyledLine) -> Vec<(u64, std::ops::Range<usize>)> {
        line.objects.as_ref().map_or_else(Vec::new, |objects| {
            objects.iter().map(|o| (o.id, o.range.clone())).collect()
        })
    }

    fn partial_lines(actions: &[VtEvent]) -> Vec<Arc<super::StyledLine>> {
        actions
            .iter()
            .filter_map(|action| match action {
                VtEvent::HandleIncomingPartialLine(line) => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    fn fragmented(actions: &[VtEvent]) -> (Arc<super::StyledLine>, Arc<super::StyledLine>) {
        actions
            .iter()
            .find_map(|action| match action {
                VtEvent::HandleIncomingFragmentedLine {
                    line,
                    completion_fragment,
                } => Some((line.clone(), completion_fragment.clone())),
                _ => None,
            })
            .expect("a fragmented completion")
    }

    #[test]
    fn an_object_attached_mid_line_keeps_its_identity_across_a_batch_split() {
        let mut h = harness();
        h.feed(b"abc");
        let id = h
            .processor
            .attach_inline_object(3, Arc::new(()), Baseline::LastRow)
            .expect("under the cap");
        h.feed(b"def");
        h.processor.notify_end_of_buffer();
        h.feed(b"ghi\n");
        let actions = h.actions();
        assert_eq!(
            transcript(&actions),
            ["partial:abc   def", "line:abc   defghi"]
        );
        let partial = &partial_lines(&actions)[0];
        assert_eq!(objects_of(partial), [(id, 3..6)]);
        assert_eq!(
            partial.objects.as_ref().unwrap()[0].baseline,
            Baseline::LastRow,
            "a placement's text sits on its last row"
        );
        assert_eq!(
            partial.raw(),
            Some("abcdef"),
            "placeholders stay out of raw"
        );
        let (line, completion) = fragmented(&actions);
        assert_eq!(
            objects_of(&line),
            [(id, 3..6)],
            "the join keeps the object's id and range"
        );
        assert!(
            completion.objects.is_none(),
            "the completion fragment carries only the new text"
        );
        assert_eq!(line.raw(), Some("abcdefghi"));
    }

    #[test]
    fn an_object_in_a_prompt_snapshot_stays_with_the_prompt_row() {
        let mut h = harness();
        h.feed(b"HP> ");
        let id = h
            .processor
            .attach_inline_object(2, Arc::new(()), Baseline::LastRow)
            .unwrap();
        h.processor.commit_prompt();
        h.feed(b"look\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:HP>   ", "line:look"]);
        assert!(actions.iter().any(|a| matches!(a, VtEvent::PromptBoundary)));
        assert_eq!(objects_of(&partial_lines(&actions)[0]), [(id, 4..6)]);
        let committed = committed_lines(&actions);
        assert!(
            committed[0].objects.is_none(),
            "the next logical line starts without the prompt's object"
        );
    }

    #[test]
    fn a_zero_column_anchor_still_flushes_as_a_partial_line() {
        // A `C=1` placement is a zero-size anchor: no text, but a line.
        let mut h = harness();
        let id = h
            .processor
            .attach_inline_object(0, Arc::new(()), Baseline::LastRow)
            .unwrap();
        h.processor.notify_end_of_buffer();
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["partial:"]);
        assert_eq!(objects_of(&partial_lines(&actions)[0]), [(id, 0..0)]);
    }

    #[test]
    fn a_carriage_return_overprint_drops_the_superseded_frame_s_objects() {
        let mut h = harness();
        h.feed(b"10%");
        h.processor
            .attach_inline_object(2, Arc::new(()), Baseline::LastRow)
            .unwrap();
        h.feed(b"\r20%\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["retract", "line:20%"]);
        assert!(committed_lines(&actions)[0].objects.is_none());
        // A placement arriving after the `\r` restarts the frame itself.
        let mut h = harness();
        h.feed(b"10%\r");
        let id = h
            .processor
            .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
            .unwrap();
        h.feed(b"x\n");
        let actions = h.actions();
        assert_eq!(transcript(&actions), ["retract", "line: x"]);
        assert_eq!(objects_of(&committed_lines(&actions)[0]), [(id, 0..1)]);
    }

    #[test]
    fn the_per_line_cap_holds_per_fragment_and_the_join_keeps_the_newest() {
        let mut h = harness();
        for _ in 0..MAX_INLINE_OBJECTS {
            assert!(
                h.processor
                    .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
                    .is_some()
            );
        }
        assert!(
            h.processor
                .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
                .is_none(),
            "the cap refuses a 65th object on one fragment"
        );
        h.processor.notify_end_of_buffer();
        // A later batch starts a fresh count on the same logical line...
        let late = h
            .processor
            .attach_inline_object(1, Arc::new(()), Baseline::LastRow)
            .unwrap();
        h.feed(b"\n");
        let actions = h.actions();
        let first = objects_of(&partial_lines(&actions)[0])[0].0;
        let (line, _) = fragmented(&actions);
        let joined = objects_of(&line);
        // ...and the join trims the OLDEST back down to the cap.
        assert_eq!(joined.len(), MAX_INLINE_OBJECTS);
        assert_eq!(joined.last().map(|o| o.0), Some(late));
        assert!(joined.iter().all(|o| o.0 != first));
    }

    #[test]
    fn graphics_commands_do_not_disturb_styles_or_links() {
        let mut h = harness();
        h.feed(b"\x1b[31mred\x1b_Ga=T,f=100;iVBORw0KGgo=\x1b\\still red\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        let mut plain = harness();
        plain.feed(b"\x1b[31mredstill red\x1b[0m\n");
        let baseline = committed_lines(&plain.actions());
        assert_eq!(lines[0].text, "redstill red");
        assert_eq!(
            lines[0].spans, baseline[0].spans,
            "styling as if no APC arrived"
        );
        assert_eq!(lines[0].raw(), Some("\x1b[31mredstill red\x1b[0m"));
    }

    // --- anchors, layers and line sequence numbers ---

    /// Image 1, ready: 20x40 px, two 10x20 cells tall and two wide.
    fn ready_image(h: &mut Harness, decodes: &Decodes, id: u32, (width, height): (u32, u32)) {
        h.feed(&transmit(
            &format!("a=t,i={id},f=24,s={width},v={height}"),
            (width * height * 3) as usize,
        ));
        let (ticket, _) = decodes.lock().unwrap().pop().expect("a decode");
        h.processor.graphics_decode_complete(
            ticket,
            Ok(graphics::DecodedPixels {
                width,
                height,
                rgba: vec![0; (width * height * 4) as usize].into(),
            }),
        );
        let mut out = Vec::new();
        h.processor.drain_replies(&mut out);
    }

    #[test]
    fn cursor_keeping_and_negative_z_placements_attach_as_anchors() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        ready_image(&mut h, &decodes, 1, (20, 40));
        h.feed(b"ab\x1b_Ga=p,i=1,C=1\x1b\\cd\x1b_Ga=p,i=1,z=-3\x1b\\ef\x1b_Ga=p,i=1,z=2\x1b\\gh\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(
            line.text, "abcdef  gh",
            "anchors take no cells; z=2 takes its two"
        );
        let objects = line.objects.as_ref().unwrap();
        assert_eq!(objects.len(), 3);
        let kept = &objects[0];
        assert_eq!(kept.range, 2..2, "an empty range at the cursor");
        assert_eq!(kept.baseline, Baseline::Centered, "no riser");
        assert_eq!(
            kept.paint_outset, 40,
            "the box: 2x2 cells of 10x20 is 20 by 40"
        );
        assert_eq!((kept.layer, kept.z), (Layer::Over, 0));
        let under = &objects[1];
        assert_eq!(under.range, 4..4);
        assert_eq!((under.layer, under.z), (Layer::Under, -3));
        assert_eq!(under.baseline, Baseline::Centered);
        let over = &objects[2];
        assert_eq!(over.range, 6..8);
        assert_eq!((over.layer, over.z), (Layer::Over, 2));
        assert_eq!(over.baseline, Baseline::LastRow);
        let placements = placements_of(line);
        assert!(placements[0].anchored && placements[1].anchored && !placements[2].anchored);
        assert_eq!(placements[0].column, 2);
        assert_eq!(placements[2].column, 6);
    }

    #[test]
    fn placements_carry_the_sequence_number_of_their_line() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        ready_image(&mut h, &decodes, 1, (1, 1));
        h.feed(b"one\ntwo\n");
        h.feed(b"HP> ");
        h.processor.commit_prompt();
        h.feed(b"\x1b_Ga=p,i=1\x1b\\");
        h.processor.notify_end_of_buffer();
        h.feed(b"\x1b_Ga=p,i=1\x1b\\\n");
        let actions = h.actions();
        let (line, _) = fragmented(&actions);
        let placements = placements_of(&line);
        assert_eq!(placements.len(), 2);
        assert_eq!(
            (placements[0].line_seq, placements[1].line_seq),
            (3, 3),
            "two commits and a prompt before it; a partial flush is the same line"
        );
        assert_eq!((placements[0].column, placements[1].column), (0, 1));
        // A delete at the cursor: the second placement ended at the cursor,
        // so neither is under it; an anchor there is.
        h.feed(b"\x1b_Ga=p,i=1,C=1\x1b\\\x1b_Ga=d,d=c\x1b\\x\n");
        let line = &committed_lines(&h.actions())[0];
        let anchor = placements_of(line).pop().unwrap();
        assert!(anchor.is_deleted());
        assert!(!placements[0].is_deleted() && !placements[1].is_deleted());
        assert_eq!(anchor.line_seq, 4);
    }

    #[test]
    fn a_placement_after_a_partial_flush_sits_past_the_flushed_text() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        ready_image(&mut h, &decodes, 1, (10, 20));
        h.feed(b"abc");
        h.processor.notify_end_of_buffer();
        h.feed(b"de\x1b_Ga=p,i=1\x1b\\\n");
        let actions = h.actions();
        let (line, completion) = fragmented(&actions);
        assert_eq!(line.text, "abcde ");
        assert_eq!(
            placements_of(&completion).pop().unwrap().column,
            5,
            "three characters flushed and two in the buffer"
        );
        // Every flush adds its fragment.
        h.feed(b"xy");
        h.processor.notify_end_of_buffer();
        h.feed(b"z");
        h.processor.notify_end_of_buffer();
        h.feed(b"\x1b_Ga=p,i=1\x1b\\\n");
        let (line, completion) = fragmented(&h.actions());
        assert_eq!(line.text, "xyz ");
        assert_eq!(placements_of(&completion).pop().unwrap().column, 3);
    }

    // --- unicode placeholders ---

    const CELL: &str = "\u{10EEEE}";
    /// The diacritics for 0, 1 and 2.
    const D0: &str = "\u{0305}";
    const D1: &str = "\u{030D}";
    const D2: &str = "\u{030E}";

    /// Image `id` of `size`, ready, with a virtual placement of `keys`.
    fn virtual_image(h: &mut Harness, decodes: &Decodes, id: u32, size: (u32, u32), keys: &str) {
        ready_image(h, decodes, id, size);
        h.feed(format!("\x1b_Ga=p,U=1,i={id},q=2,{keys}\x1b\\").as_bytes());
    }

    fn sources(line: &super::StyledLine) -> Vec<graphics::SourceRect> {
        placements_of(line)
            .iter()
            .map(|p| p.geometry.source)
            .collect()
    }

    fn rect(x: u32, y: u32, width: u32, height: u32) -> graphics::SourceRect {
        graphics::SourceRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_spec_s_two_by_two_placeholder_shows_the_image_in_a_grid() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        // A 20x40 image fills a 2x2 box of 10x20 cells exactly.
        virtual_image(&mut h, &decodes, 42, (20, 40), "c=2,r=2");
        let first = format!("\x1b[38;5;42m{CELL}{D0}{D0}{CELL}{D0}{D1}\x1b[39m\n");
        let second = format!("\x1b[38;5;42m{CELL}{D1}{D0}{CELL}{D1}{D1}\x1b[39m\n");
        h.feed(first.as_bytes());
        h.feed(second.as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(lines.len(), 2);
        let texts = [
            format!("{CELL}{D0}{D0}{CELL}{D0}{D1}"),
            format!("{CELL}{D1}{D0}{CELL}{D1}{D1}"),
        ];
        for ((line, y), text) in lines.iter().zip([0, 20]).zip(&texts) {
            assert_eq!(&line.text, text, "placeholder text stays");
            let objects = line.objects.as_ref().expect("one run");
            assert_eq!(objects.len(), 1);
            assert_eq!(
                objects[0].range,
                0..16,
                "both cells: placeholder plus two diacritics each"
            );
            assert_eq!(objects[0].baseline, Baseline::Centered);
            assert_eq!((objects[0].layer, objects[0].z), (Layer::Over, 0));
            let placement = &placements_of(line)[0];
            assert!(placement.placeholder);
            assert_eq!(
                (placement.geometry.columns, placement.geometry.rows),
                (2, 1)
            );
            assert_eq!(placement.geometry.source, rect(0, y, 20, 20), "row {y}");
            assert_eq!(
                placement.geometry.fit,
                graphics::Fit::Box {
                    columns: 2,
                    rows: 1
                }
            );
            assert!(placement.pixels().is_some());
            assert_eq!(
                (placement.line_seq, placement.column),
                (u64::from(y != 0), 0)
            );
        }
        assert!(
            lines[0].raw().unwrap().contains(CELL),
            "and so does the raw text"
        );
        assert!(!h.processor.has_replies(), "q=2 on the virtual placement");
    }

    #[test]
    fn the_third_diacritic_names_the_image_id_s_high_byte() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42 + (2 << 24), (20, 40), "c=2,r=2");
        let line = format!("\x1b[38;5;42m{CELL}{D0}{D0}{D2}{CELL}{D0}{D1}{D2}\n");
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let placement = &placements_of(line)[0];
        assert_eq!(placement.image.id, 42 + (2 << 24));
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 0..20);
        // Without the high byte, image 42 does not exist: the cells stay text.
        let plain = format!("\x1b[38;5;42m{CELL}{D0}{D0}{CELL}{D0}{D1}\n");
        h.feed(plain.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        assert!(line.objects.is_none());
    }

    #[test]
    fn row_only_cells_continue_the_run_to_the_right() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        // 2 rows by 3 columns, one row diacritic per line.
        virtual_image(&mut h, &decodes, 42, (30, 40), "c=3,r=2");
        let first = format!("\x1b[38;5;42m{CELL}{D0}{CELL}{CELL}\n");
        let second = format!("\x1b[38;5;42m{CELL}{D1}{CELL}{CELL}\n");
        h.feed(first.as_bytes());
        h.feed(second.as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(sources(&lines[0]), [rect(0, 0, 30, 20)]);
        assert_eq!(sources(&lines[1]), [rect(0, 20, 30, 20)]);
        assert_eq!(lines[1].objects.as_ref().unwrap()[0].range, 0..14);
        assert_eq!(placements_of(&lines[1])[0].geometry.columns, 3);
    }

    #[test]
    fn runs_break_at_colour_changes_text_and_unknown_ids() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (30, 40), "c=3,r=2");
        virtual_image(&mut h, &decodes, 43, (30, 40), "c=3,r=2");
        // Two cells of 42, one of 43, text, then a cell of 42 again, and a
        // cell of an image that does not exist.
        let line = format!(
            "\x1b[38;5;42m{CELL}{D0}{CELL}\x1b[38;5;43m{CELL}{D1}{D2}\x1b[38;5;42m-{CELL}{D1}{D1}\x1b[38;5;99m{CELL}{D0}\x1b[m\n"
        );
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let objects = line.objects.as_ref().unwrap();
        let placements = placements_of(line);
        assert_eq!(
            placements.len(),
            3,
            "image 99 has no virtual placement: text"
        );
        assert_eq!(
            objects.iter().map(|o| o.range.clone()).collect::<Vec<_>>(),
            [0..10, 10..18, 19..27]
        );
        assert_eq!(
            placements.iter().map(|p| p.image.id).collect::<Vec<_>>(),
            [42, 43, 42]
        );
        assert_eq!(
            sources(line),
            [
                rect(0, 0, 20, 20),
                rect(20, 20, 10, 20),
                rect(10, 20, 10, 20)
            ]
        );
        assert_eq!(placements[2].column, 4, "after the dash");
    }

    #[test]
    fn the_underline_colour_names_the_virtual_placement() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (20, 40), "c=2,r=2,p=5");
        h.feed(b"\x1b_Ga=p,U=1,i=42,c=1,r=1,p=6,q=2\x1b\\");
        // Placement 5 is 2x2: row 1 exists. Placement 6 is 1x1: row 1 is
        // outside it. No underline colour: the newest, 6.
        let line = format!(
            "\x1b[38;5;42;58;5;5m{CELL}{D1}{D0}\x1b[58;5;6m{CELL}{D1}{D0}\x1b[59m{CELL}{D0}{D0}\x1b[58;2;0;0;7m{CELL}{D0}{D0}\x1b[m\n"
        );
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let placements = placements_of(line);
        assert_eq!(
            placements.len(),
            2,
            "placement 6 has no row 1; placement 7 does not exist"
        );
        assert_eq!(
            sources(line),
            [rect(0, 20, 10, 20), rect(0, 0, 20, 40)],
            "one cell of the 2x2 placement 5, then the whole image in the 1x1 placement 6"
        );
        let objects = line.objects.as_ref().unwrap();
        assert_eq!(
            objects.iter().map(|o| o.range.clone()).collect::<Vec<_>>(),
            [0..8, 16..24]
        );
    }

    #[test]
    fn direct_and_named_foreground_colours_carry_the_image_id_too() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 0x0001_0203, (10, 20), "c=1,r=1");
        virtual_image(&mut h, &decodes, 1, (10, 20), "c=1,r=1");
        virtual_image(&mut h, &decodes, 9, (10, 20), "c=1,r=1");
        // A 256-colour index that the palette maps to RGB still means itself.
        virtual_image(&mut h, &decodes, 200, (10, 20), "c=1,r=1");
        let line = format!(
            "\x1b[38;2;1;2;3m{CELL}{D0}{D0} \x1b[31m{CELL}{D0}{D0} \x1b[91m{CELL}{D0}{D0} \x1b[38;5;200m{CELL}{D0}{D0} \x1b[39m{CELL}{D0}{D0}\n"
        );
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(
            placements_of(line)
                .iter()
                .map(|p| p.image.id)
                .collect::<Vec<_>>(),
            [0x0001_0203, 1, 9, 200],
            "the default foreground is id 0: no image"
        );
    }

    #[test]
    fn placeholders_printed_before_a_virtual_placement_exists_stay_text() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        ready_image(&mut h, &decodes, 42, (10, 20));
        let cell = format!("\x1b[38;5;42m{CELL}{D0}{D0}");
        h.feed(cell.as_bytes());
        // The virtual placement arrives mid-line: the cell before it was
        // printed as text and stays so; one printed after it is a run.
        h.feed(b"\x1b_Ga=p,U=1,i=42,c=1,r=1,q=2\x1b\\");
        h.feed(cell.as_bytes());
        h.feed(b"\n");
        let line = &committed_lines(&h.actions())[0];
        let objects = line.objects.as_ref().unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].range, 8..16);
        // Without a host, or with graphics off, placeholders are text.
        h.processor.set_graphics_mode(graphics::GraphicsMode::Off);
        h.feed(cell.as_bytes());
        h.feed(b"\n");
        assert!(committed_lines(&h.actions())[0].objects.is_none());
        let mut plain = harness();
        plain.feed(cell.as_bytes());
        plain.feed(b"\n");
        let line = &committed_lines(&plain.actions())[0];
        assert!(line.objects.is_none());
        assert_eq!(line.text, format!("{CELL}{D0}{D0}"));
    }

    #[test]
    fn placeholders_in_a_bulk_run_are_found_like_printed_ones() {
        // The connection's byte loop hands Ground-state text over as runs
        // (`print_run`), which is where a placeholder line nearly always
        // arrives: right after its SGR.
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (20, 40), "c=2,r=2");
        h.feed(b"\x1b[38;5;42m");
        let run = format!("ab{CELL}{D1}{D0}{CELL}{D1}{D1}cd");
        h.processor.print_run(&run);
        h.feed(b"\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, run);
        let objects = line.objects.as_ref().expect("one run");
        assert_eq!(objects[0].range, 2..18);
        assert_eq!(sources(line), [rect(0, 20, 20, 20)]);
        assert_eq!(placements_of(line)[0].column, 2);
        // An ASCII run never looks; a non-ASCII run without placeholders
        // finds none.
        h.processor.print_run("plain text");
        h.processor.print_run("caf\u{e9}");
        h.feed(b"\n");
        assert!(committed_lines(&h.actions())[0].objects.is_none());
        // Inside a link the per-character path takes the run, and still
        // notes the placeholder.
        h.feed(b"\x1b]8;;https://example.com\x1b\\");
        h.processor.print_run(&run);
        h.feed(b"\x1b]8;;\x1b\\\n");
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 2..18);
    }

    #[test]
    fn a_run_split_by_a_batch_boundary_becomes_two_placements() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (20, 40), "c=2,r=2");
        h.feed(format!("\x1b[38;5;42m{CELL}{D0}{D0}").as_bytes());
        h.processor.notify_end_of_buffer();
        h.feed(format!("{CELL}{D0}{D1}\n").as_bytes());
        let actions = h.actions();
        let partial = &partial_lines(&actions)[0];
        assert_eq!(sources(partial), [rect(0, 0, 10, 20)]);
        let (line, completion) = fragmented(&actions);
        assert_eq!(sources(&completion), [rect(10, 0, 10, 20)]);
        assert_eq!(sources(&line), [rect(0, 0, 10, 20), rect(10, 0, 10, 20)]);
        assert_eq!(
            line.objects
                .as_ref()
                .unwrap()
                .iter()
                .map(|o| o.range.clone())
                .collect::<Vec<_>>(),
            [0..8, 8..16]
        );
    }

    #[test]
    fn three_combining_marks_belong_to_a_placeholder_cell_whatever_they_are() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42 + (1 << 24), (20, 20), "c=2,r=1");
        // The first cell's column mark is a combining acute, which names
        // nothing: the column is unknown (zero at a run's start), the id
        // byte after it still reads, and the next cell is still adjacent.
        let line = format!("\x1b[38;5;42m{CELL}{D0}\u{0301}{D1}{CELL}{D0}{D1}{D1}\n");
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let placements = placements_of(line);
        assert_eq!(placements.len(), 1, "one run of two cells");
        assert_eq!(placements[0].image.id, 42 + (1 << 24));
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 0..20);
        assert_eq!(sources(line), [rect(0, 0, 20, 20)]);
        // A fourth mark is the cell's too, and means nothing.
        let line = format!("\x1b[38;5;42m{CELL}{D0}{D0}{D1}{D0}{CELL}{D0}{D1}{D1}\n");
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(placements_of(line).len(), 1);
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 0..22);
    }

    #[test]
    fn style_changes_between_placeholder_cells_keep_the_run() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (30, 20), "c=3,r=1");
        // Bold, then a background, between the cells: a run reads only the
        // foreground and the underline colour, as kitty's does.
        let line = format!(
            "\x1b[38;5;42m{CELL}{D0}{D0}\x1b[1m{CELL}{D0}{D1}\x1b[48;5;3m{CELL}{D0}{D2}\x1b[m\n"
        );
        h.feed(line.as_bytes());
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(placements_of(line).len(), 1);
        assert_eq!(line.objects.as_ref().unwrap()[0].range, 0..24);
        assert_eq!(sources(line), [rect(0, 0, 30, 20)]);
    }

    #[test]
    fn a_carriage_return_overprint_drops_the_frame_s_placeholders() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (10, 20), "c=1,r=1");
        h.feed(format!("\x1b[38;5;42m{CELL}{D0}{D0}\rx\n").as_bytes());
        let line = &committed_lines(&h.actions())[0];
        assert_eq!(line.text, "x");
        assert!(line.objects.is_none());
        // Objects stay in line order when a run starts before an object
        // attached at the cursor earlier.
        h.feed(format!("{CELL}{D0}{D0}\x1b_Ga=p,i=42\x1b\\\n").as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let objects = line.objects.as_ref().unwrap();
        assert_eq!(
            objects.iter().map(|o| o.range.clone()).collect::<Vec<_>>(),
            [0..8, 8..9]
        );
    }

    #[test]
    fn deleting_the_virtual_placement_takes_the_runs_in_the_lines() {
        let (mut h, decodes) = hosted(graphics::GraphicsMode::Full);
        with_cell(&mut h, 10, 20);
        virtual_image(&mut h, &decodes, 42, (10, 20), "c=1,r=1,p=3");
        h.feed(format!("\x1b[38;5;42m{CELL}{D0}{D0}\n").as_bytes());
        let line = &committed_lines(&h.actions())[0];
        let run = placements_of(line).pop().unwrap();
        h.feed(b"\x1b_Ga=d,d=i,i=42,p=3\x1b\\");
        assert!(run.is_deleted());
        assert_eq!(
            line.text,
            format!("{CELL}{D0}{D0}"),
            "the text is the client's"
        );
        // With the virtual placement gone the next line is text again.
        h.feed(format!("{CELL}{D0}{D0}\n").as_bytes());
        assert!(committed_lines(&h.actions())[0].objects.is_none());
    }
}
