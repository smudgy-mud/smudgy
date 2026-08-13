use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use vtparse::{CsiParam, VTActor, VTParser};

use crate::json::deep_merge;
use crate::models::server::link_url_host;
use crate::session::{
    runtime::RuntimeAction,
    styled_line::{
        InvisiblePolicy, LinkAction, LinkColor, LinkDecoration, LinkMenu, LinkMenuItem,
        LinkMenuTitle, LinkProtocol, LinkSelection, LinkSpan, LinkStyle, LinkTextStyle,
        LinkTooltip, LinkTooltipText, LinkVisibility, LinkVisibilityAction, LinkVisibilityExpire,
        Style, StyledLine, VtSpan, deceptive_invisible, push_escaped_char,
    },
};

mod sgr;
pub use sgr::{AnsiColor, Color};
// Expose the SGR interpreter to the `smudgy_bench` crate without widening the normal public
// API: `mod sgr` stays private; the function becomes reachable only under the feature.
#[cfg(feature = "bench-api")]
pub use sgr::process as sgr_process;

/// The most bytes an OSC 8 URI may carry; longer links are ignored (the text
/// still displays, unlinked).
const MAX_OSC8_URI_LEN: usize = 4096;
/// The most named OSC 8 presets retained for one connection. Existing names
/// remain replaceable at the cap so a server can still restyle its presets.
const MAX_OSC8_PRESETS: usize = 1024;

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
        crate::session::styled_line::escape_invisible_text(
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

struct TooltipAnsiActor {
    text: String,
    spans: Vec<VtSpan>,
    style: Style,
    span_start: usize,
    pending_cr: bool,
}

impl Default for TooltipAnsiActor {
    fn default() -> Self {
        Self {
            text: String::new(),
            spans: Vec::new(),
            style: Style::default(),
            span_start: 0,
            pending_cr: false,
        }
    }
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
    let canonical = smudgy_cloud::canonicalize_css_color(value.as_str()?.trim())?;
    let hex = canonical.strip_prefix('#')?;
    let byte = |start| u8::from_str_radix(hex.get(start..start + 2)?, 16).ok();
    Some(LinkColor {
        red: byte(0)?,
        green: byte(2)?,
        blue: byte(4)?,
        alpha: if hex.len() == 8 { byte(6)? } else { u8::MAX },
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

#[derive(Debug)]
pub struct VtProcessor {
    cursor_style: Style,
    buf: String,
    buf_raw: Vec<u8>,
    span_info: Vec<VtSpan>,
    /// Completed link ranges of the pending line (closed OSC 8 links).
    link_info: Vec<LinkSpan>,
    /// The active OSC 8 link, applied to every character until `ESC]8;;`.
    /// Like the cursor style, it survives line commits (a multi-line link)
    /// and carriage-return overprints.
    cursor_link: Option<ParsedLink>,
    /// Where in `buf` the active link's current range began.
    link_open_pos: usize,
    /// A bare `\r` arrived: the next printable character overwrites the open
    /// line (carriage-return overprint — progress bars, spinners) instead of
    /// appending to it. Holds `buf_raw`'s length at the `\r`, so the restart
    /// can drop exactly the superseded frame's raw bytes while keeping any
    /// that arrived after it (escape sequences, the new frame's first char —
    /// the connection pushes raw bytes before the parser sees them). `\n`
    /// clears it, so CRLF stays an ordinary commit. Persists across reads
    /// like the rest of the parse state.
    pending_cr: Option<usize>,
    session_runtime_tx: UnboundedSender<RuntimeAction>,
    /// Identifies the connection that owns this processor. Packet-completion
    /// markers carry it back to the runtime so a replaced socket cannot
    /// release the next connection's deferred profile send.
    connection_generation: u64,
    /// Whether the current read batch produced any non-empty terminal text.
    /// Telnet-only traffic and empty line terminators do not qualify.
    packet_has_displayable_text: bool,
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
    /// The `raw_wanted` value latched for the lines being accumulated: it FALLS
    /// at line boundaries but RISES only at read-batch boundaries, so an emitted
    /// line's raw form is always complete-or-absent — never a torn suffix — even
    /// though the flag flips concurrently with an in-flight parse run (see
    /// [`Self::refresh_capture_raw`]).
    capture_raw: bool,
    /// Mudlet-compatible OSC 8 presets live for exactly one connection/
    /// processor lifetime and are never shared across sessions.
    link_presets: HashMap<String, serde_json::Value>,
    link_preset_overflow_logged: bool,
}

const INPUT_BUFFER_CAPACITY: usize = 1024;

impl VtProcessor {
    #[must_use]
    pub fn new(session_runtime_tx: UnboundedSender<RuntimeAction>) -> Self {
        Self::with_connection_generation(session_runtime_tx, 0)
    }

    #[must_use]
    pub(super) fn with_connection_generation(
        session_runtime_tx: UnboundedSender<RuntimeAction>,
        connection_generation: u64,
    ) -> Self {
        VtProcessor {
            cursor_style: Style::default(),
            buf: String::with_capacity(INPUT_BUFFER_CAPACITY),
            buf_raw: Vec::with_capacity(INPUT_BUFFER_CAPACITY),
            span_info: Vec::new(),
            link_info: Vec::new(),
            cursor_link: None,
            link_open_pos: 0,
            pending_cr: None,
            session_runtime_tx,
            connection_generation,
            packet_has_displayable_text: false,
            marker_armed: None,
            raw_wanted: None,
            capture_raw: true,
            link_presets: HashMap::new(),
            link_preset_overflow_logged: false,
        }
    }

    /// Ties packet-completion markers to the given flag (the runtime's
    /// "a deferred profile send is pending" bit). While the flag is clear the
    /// batch boundary emits no marker; a marker skipped this way is never
    /// owed later, because the flag only rises when a new connection (with a
    /// fresh processor) installs a pending send.
    pub(super) fn set_marker_armed_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.marker_armed = Some(flag);
    }

    /// Ties raw capture to the given flag (the trigger manager's "any raw
    /// pattern exists" bit). A drop takes effect at the next line boundary; a
    /// rise waits for the next read-batch boundary.
    pub fn set_raw_wanted_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.capture_raw = flag.load(std::sync::atomic::Ordering::Relaxed);
        self.raw_wanted = Some(flag);
    }

    /// Fall-only re-latch of `capture_raw`, called where the line buffers empty
    /// mid-run (line commit, prompt commit). The reader parses on the socket
    /// runtime while the trigger manager flips the flag from the session
    /// thread, and `TelnetBridge::on_data` hoists its per-byte raw push behind
    /// [`Self::capture_raw`] once per run: a mid-run FALL is safe under either
    /// hoisted branch (the push re-checks per byte, and a fallen commit
    /// attaches nothing), but a mid-run RISE would attach a torn raw suffix
    /// pushed only from the flip onward. Rises therefore wait for the batch
    /// boundary ([`Self::notify_end_of_buffer`]), where no run is in flight
    /// and the buffers are empty.
    fn refresh_capture_raw(&mut self) {
        if let Some(flag) = &self.raw_wanted {
            self.capture_raw = self.capture_raw && flag.load(std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Whether raw bytes are currently being captured. The byte loop hoists
    /// its per-byte push behind this — sound across threads because the value
    /// can only fall mid-run (rises are deferred to batch boundaries), and a
    /// fall is safe under either hoisted branch.
    #[must_use]
    pub fn capture_raw(&self) -> bool {
        self.capture_raw
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
    /// bytes, and (via [`RuntimeAction::RetractIncomingPartialLine`]) any
    /// prefix already flushed upstream as a partial — so the text after the
    /// `\r` replaces it. Raw bytes that arrived after the `\r` (up to and
    /// including the character triggering the restart) belong to the new
    /// frame and are kept. The cursor style survives, as on a real terminal.
    fn restart_open_line(&mut self, raw_mark: usize) {
        self.buf.clear();
        self.span_info.clear();
        // An active link survives the overprint like the cursor style does;
        // ranges already banked for the superseded frame are discarded.
        self.link_info.clear();
        self.link_open_pos = 0;
        self.buf_raw.drain(..raw_mark.min(self.buf_raw.len()));
        self.session_runtime_tx
            .send(RuntimeAction::RetractIncomingPartialLine)
            .unwrap();
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
        self.change_style(self.cursor_style);
        // A link still open at the boundary contributes its range so far and
        // stays active: its next range begins at 0 on the next line.
        self.close_link_range(true);
        let mut line = StyledLine::new_with_raw(
            &self.buf,
            self.span_info.drain(..).collect(),
            self.capture_raw.then_some(self.buf_raw.as_slice()),
        );
        line.links = self.link_info.drain(..).collect();
        self.link_open_pos = 0;
        line
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
        let pending_line = Arc::new(self.consume_into_pending_line());
        if !self.buf.is_empty() {
            self.packet_has_displayable_text = true;
            self.session_runtime_tx
                .send(RuntimeAction::HandleIncomingPartialLine(pending_line))
                .ok();
            self.buf.clear();
            self.buf_raw.clear();
            self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
            self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
            // The frame a pending `\r` marked was just flushed upstream as a
            // partial; the restart's retraction covers it, and no local raw
            // bytes remain to drop.
            if self.pending_cr.is_some() {
                self.pending_cr = Some(0);
            }
        }
        self.session_runtime_tx
            .send(RuntimeAction::RequestRepaint)
            .ok();
        // The packet-completion marker exists only to release a deferred
        // profile send; once the runtime disarms the flag (send released, or
        // no send was pending), steady-state batches skip the channel send.
        let has_displayable_text = std::mem::take(&mut self.packet_has_displayable_text);
        if self
            .marker_armed
            .as_ref()
            .is_none_or(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            self.session_runtime_tx
                .send(RuntimeAction::IncomingPacketProcessed {
                    connection_generation: self.connection_generation,
                    has_displayable_text,
                })
                .ok();
        }
        // The batch boundary is the one place capture may RISE: no parse run is
        // in flight (the reader calls this between batches) and the line buffers
        // are empty, so the next batch starts a fresh line under the new value
        // and `TelnetBridge::on_data` re-hoists it before any byte flows.
        if let Some(flag) = &self.raw_wanted {
            self.capture_raw = flag.load(std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Commit the pending bytes as a **prompt**: emit them on the partial-line path (so
    /// `prompt:`-flagged triggers fire) and reset the buffers so the next bytes start a fresh line.
    ///
    /// Driven by the telnet layer when it decodes a prompt boundary (`IAC GA` / `IAC EOR`) — a
    /// precise, server-sent signal, unlike the partial-line-at-end-of-buffer heuristic in
    /// [`notify_end_of_buffer`](Self::notify_end_of_buffer). Clearing the buffers here is what stops
    /// that heuristic from re-emitting the same prompt at end of read. A no-op when nothing is
    /// pending (e.g. a bare `IAC GA` with no preceding text). The send is best-effort, like
    /// [`notify_end_of_buffer`](Self::notify_end_of_buffer)'s.
    pub fn commit_prompt(&mut self) {
        // A prompt boundary finalizes the line; a `\r` just before it must
        // not overwrite what follows.
        self.pending_cr = None;
        if self.buf.is_empty() {
            self.session_runtime_tx
                .send(RuntimeAction::PromptBoundary)
                .ok();
            return;
        }
        let pending_line = Arc::new(self.consume_into_pending_line());
        self.packet_has_displayable_text = true;
        self.session_runtime_tx
            .send(RuntimeAction::HandleIncomingPartialLine(pending_line))
            .ok();
        self.session_runtime_tx
            .send(RuntimeAction::PromptBoundary)
            .ok();
        self.buf.clear();
        self.buf_raw.clear();
        self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
        self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
        self.refresh_capture_raw();
    }

    fn commit_line(&mut self) {
        let pending_line = Arc::new(self.consume_into_pending_line());
        if !pending_line.text.is_empty() {
            self.packet_has_displayable_text = true;
        }
        self.session_runtime_tx
            .send(RuntimeAction::HandleIncomingLine(pending_line))
            .ok();
        self.buf.clear();
        self.buf_raw.clear();
        self.refresh_capture_raw();
    }

    fn push_incoming_char(&mut self, ch: char) {
        self.buf.push(ch);
    }

    pub fn push_raw_incoming_byte(&mut self, byte: u8) {
        if self.capture_raw {
            self.buf_raw.push(byte);
        }
    }
}

impl VTActor for VtProcessor {
    fn print(&mut self, b: char) {
        if let Some(raw_mark) = self.pending_cr.take() {
            self.restart_open_line(raw_mark);
        }
        if self.cursor_link.is_some() && deceptive_invisible(b, InvisiblePolicy::Prose) {
            let mut escaped = String::new();
            push_display_char(&mut escaped, b, true);
            self.buf.push_str(&escaped);
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
        if byte == b'm' {
            let new_style = sgr::process(self.cursor_style, params);
            self.change_style(new_style);
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

    fn apc_dispatch(&mut self, _data: Vec<u8>) {}
}

#[cfg(test)]
mod tests {
    use super::{
        AnsiColor, Color, MAX_OSC8_PRESETS, MAX_OSC8_URI_LEN, VtProcessor, menu_title,
        parse_link_tooltip_text, sanitize_display_text,
    };
    use crate::models::server::link_url_host;
    use crate::session::runtime::RuntimeAction;
    use std::sync::Arc;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use vtparse::VTParser;

    struct Harness {
        parser: VTParser,
        processor: VtProcessor,
        rx: UnboundedReceiver<RuntimeAction>,
    }

    fn harness() -> Harness {
        let (tx, rx) = unbounded_channel();
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

        fn actions(&mut self) -> Vec<RuntimeAction> {
            let mut out = Vec::new();
            while let Ok(action) = self.rx.try_recv() {
                out.push(action);
            }
            out
        }
    }

    /// The committed/partial line texts and where retractions fall between them.
    fn transcript(actions: &[RuntimeAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                RuntimeAction::HandleIncomingLine(line) => Some(format!("line:{}", line.text)),
                RuntimeAction::HandleIncomingPartialLine(line) => {
                    Some(format!("partial:{}", line.text))
                }
                RuntimeAction::RetractIncomingPartialLine => Some("retract".to_string()),
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
        assert_eq!(transcript(&h.actions()), ["line:a", "line:b"]);
    }

    #[test]
    fn packet_marker_requires_non_empty_terminal_text() {
        let mut h = harness();

        h.feed(b"\r\n");
        h.processor.notify_end_of_buffer();
        assert!(h.actions().iter().any(|action| matches!(
            action,
            RuntimeAction::IncomingPacketProcessed {
                connection_generation: 0,
                has_displayable_text: false,
            }
        )));

        h.feed(b"Welcome\r\n");
        h.processor.notify_end_of_buffer();
        assert!(h.actions().iter().any(|action| matches!(
            action,
            RuntimeAction::IncomingPacketProcessed {
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
            RuntimeAction::IncomingPacketProcessed {
                connection_generation: 0,
                has_displayable_text: true,
            }
        )));
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
                .any(|action| matches!(action, RuntimeAction::IncomingPacketProcessed { .. }))
        );

        // The runtime disarms after releasing the deferred send; from then on
        // batch boundaries stay off the marker channel entirely.
        armed.store(false, std::sync::atomic::Ordering::Relaxed);
        h.feed(b"steady state\r\n");
        h.processor.notify_end_of_buffer();
        assert!(
            !h.actions()
                .iter()
                .any(|action| matches!(action, RuntimeAction::IncomingPacketProcessed { .. }))
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
                RuntimeAction::HandleIncomingLine(line) => Some(line.clone()),
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

    use crate::session::styled_line::{
        LinkAction, LinkColor, LinkDecoration, LinkMenuItem, LinkStyleState, LinkTooltip,
        LinkVisibilityAction, StyledLine,
    };

    fn committed_lines(actions: &[RuntimeAction]) -> Vec<std::sync::Arc<StyledLine>> {
        actions
            .iter()
            .filter_map(|action| match action {
                RuntimeAction::HandleIncomingLine(line) => Some(line.clone()),
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
}
