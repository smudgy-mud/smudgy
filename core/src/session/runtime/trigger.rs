use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexSet};

use crate::models::matchers::{
    MatcherColor, MatcherColorMatch, MatcherHsv, MatcherHsvRange, MatcherRole,
    MatcherTextAttribute, TriggerMatcherSource,
};

use super::{
    ActionQueue, ScriptAction,
    captures::{CapturePattern, CapturePayload, CaptureView},
    matcher::{PatternMatch, PatternSet},
    origin::{
        AutomationBody, AutomationDelta, AutomationKind, AutomationSummary, IsolateId, Origin,
    },
};

/// One automation's introspectable state for the `session.triggers`/`session.aliases`
/// registries. Mirrors what the JS handle exposes: its `enabled` flag and its
/// read-back `pattern` (the first pattern's source). Refreshed by the [`Manager`] on every
/// add/enable/remove so the synchronous introspection ops can read it without crossing into
/// the (non-`OpState`) [`Manager`].
#[derive(Clone, Debug)]
pub struct AutomationEntry {
    pub enabled: bool,
    pub pattern: String,
    pub priority: i32,
    pub fallthrough: bool,
}

/// `name -> entry` within one `(IsolateId, Origin)` namespace.
type AutomationNamespace = HashMap<String, AutomationEntry>;

/// A registration-time handle for one `(IsolateId, Origin)` automation namespace. The
/// `Manager` interns each distinct pair once; per-line work then compares and hashes this
/// small copyable id instead of the owned isolate/origin values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NamespaceId(u32);

/// The immutable identity of one registered automation, created when it is registered and
/// shared (`Arc`) by its registry entry, by every action it queues, and by the fire
/// accounting those actions perform. Queuing a fire copies one `Arc` rather than the
/// isolate, origin, and name separately.
///
/// `slot` caches where the fire counter lives in the registry: the `(generation, index)` of
/// the last successful lookup. A fire whose cached generation matches the registry's current
/// generation reads the counter at that index directly; any registry mutation (add, replace,
/// remove) bumps the generation, so a stale slot falls back to the name lookup — which is
/// what keeps the existing dispatch-time semantics after a replacement or removal: a queued
/// fire charges whatever definition holds the name when it runs, not the definition that
/// queued it.
#[derive(Debug)]
pub struct AutomationIdentity {
    pub isolate: IsolateId,
    pub origin: Origin,
    pub name: Arc<String>,
    /// Whether the entry lives in the alias `Vec` (matched on outgoing input) vs the trigger
    /// `Vec` (matched on incoming lines).
    pub is_alias: bool,
    pub namespace: NamespaceId,
    slot_generation: AtomicU64,
    slot_index: AtomicUsize,
}

/// The registry generation no slot cache can ever hold: a fresh identity always looks up.
const NO_GENERATION: u64 = 0;

/// Process-wide source of registry generations, so a generation is never reused across
/// `Manager` instances (an engine rebuild replaces the manager while queued actions from the
/// old one may still hold cached slots).
static NEXT_REGISTRY_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_registry_generation() -> u64 {
    NEXT_REGISTRY_GENERATION.fetch_add(1, Ordering::Relaxed)
}

impl AutomationIdentity {
    fn new(
        isolate: IsolateId,
        origin: Origin,
        name: &str,
        is_alias: bool,
        namespace: NamespaceId,
    ) -> Self {
        Self {
            isolate,
            origin,
            name: Arc::new(name.to_owned()),
            is_alias,
            namespace,
            slot_generation: AtomicU64::new(NO_GENERATION),
            slot_index: AtomicUsize::new(0),
        }
    }

    /// The cached registry index, if it was recorded under `generation`. Only the session
    /// thread touches the slot, so the two reads cannot tear.
    fn cached_slot(&self, generation: u64) -> Option<usize> {
        (self.slot_generation.load(Ordering::Relaxed) == generation)
            .then(|| self.slot_index.load(Ordering::Relaxed))
    }

    fn cache_slot(&self, generation: u64, index: usize) {
        self.slot_index.store(index, Ordering::Relaxed);
        self.slot_generation.store(generation, Ordering::Relaxed);
    }
}

/// Stop state for one alias/trigger dispatch, partitioned by creator namespace so one package
/// cannot suppress another package's (or the user's) automations. Nearly every line fires
/// into a single namespace, so the first scope is stored inline; only a second namespace on
/// the same line touches the heap for the overflow list.
#[derive(Default)]
struct FallthroughScopes {
    first: Option<(NamespaceId, Arc<AtomicBool>)>,
    rest: Vec<(NamespaceId, Arc<AtomicBool>)>,
}

impl FallthroughScopes {
    fn new() -> Self {
        Self::default()
    }

    /// The stop flag shared by every automation of `namespace` on this line, created on
    /// first use. Each logical line owns fresh flags: a queued action retains its line's flag,
    /// so flags are never reset and reused across lines.
    fn scope(&mut self, namespace: NamespaceId) -> Arc<AtomicBool> {
        match &self.first {
            None => {
                let stopped = Arc::new(AtomicBool::new(false));
                self.first = Some((namespace, stopped.clone()));
                stopped
            }
            Some((id, stopped)) if *id == namespace => stopped.clone(),
            Some(_) => {
                if let Some((_, stopped)) = self.rest.iter().find(|(id, _)| *id == namespace) {
                    return stopped.clone();
                }
                let stopped = Arc::new(AtomicBool::new(false));
                self.rest.push((namespace, stopped.clone()));
                stopped
            }
        }
    }
}

/// Per-line working storage the incoming-line paths reuse across lines: the pattern-set
/// hits and the fired-trigger list. Taken out of the `Manager` for one line's matching and
/// put back before it returns, so no handler runs while it is borrowed; the containers keep
/// their capacity, so a steady stream of lines allocates nothing here.
#[derive(Debug, Default)]
struct LineScratch {
    matches: Vec<PatternMatch>,
    fired: Vec<usize>,
}

/// The identity of the alias whose expansion produced the line currently being matched.
/// Carried through every nested outgoing-line pass so that alias's own sent text is
/// excluded from re-matching it (unless the alias opts in via `allow_self_match`), and so
/// the depth-limit bail can name the looping alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasSender {
    pub isolate: IsolateId,
    pub origin: Origin,
    pub name: Arc<String>,
}

/// The introspection mirror the `get`/`list`/`exists` ops read. Keyed by
/// `(IsolateId, Origin)` exactly like the [`Manager`]'s own indices, so a caller only ever
/// sees its OWN `(isolate, origin)` automations (origin-scoped). Shared (the same
/// `Rc`) into every isolate's ops at construction; the [`Manager`] owns the write side and
/// keeps it consistent with its `Vec`s.
#[derive(Default, Debug)]
pub struct AutomationRegistry {
    pub aliases: HashMap<(IsolateId, Origin), AutomationNamespace>,
    pub triggers: HashMap<(IsolateId, Origin), AutomationNamespace>,
}

/// The shared introspection mirror handed to both the [`Manager`] (writer) and the ops
/// (readers). A fresh one is built per engine, so a reload clears it.
pub type SharedAutomationRegistry = Rc<RefCell<AutomationRegistry>>;
use crate::session::{
    runtime::{
        RuntimeAction,
        script_engine::{FunctionId, ScriptId},
    },
    styled_line::{Blink, Color, Style, StyledLine, Underline},
};

// The bools are independent dirty/recording gates, not an encodable state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct Manager {
    spawned_actions: ActionQueue,
    triggers: Vec<Trigger>,
    aliases: Vec<Trigger>,
    trigger_regex_set_map: Vec<usize>, // Maps index in PatternSet to index in triggers
    trigger_regex_patterns_map: Vec<usize>,
    trigger_regex_set: PatternSet,
    raw_trigger_regex_set_map: Vec<usize>,
    raw_trigger_regex_patterns_map: Vec<usize>,
    raw_trigger_regex_set: PatternSet,
    prompt_trigger_regex_set_map: Vec<usize>,
    prompt_trigger_regex_patterns_map: Vec<usize>,
    prompt_trigger_regex_set: PatternSet,
    prompt_raw_trigger_regex_set_map: Vec<usize>,
    prompt_raw_trigger_regex_patterns_map: Vec<usize>,
    prompt_raw_trigger_regex_set: PatternSet,
    alias_regex_set_map: Vec<usize>,
    alias_regex_patterns_map: Vec<usize>,
    alias_regex_set: PatternSet,
    // Keyed by `(IsolateId, Origin)`: the isolate dimension (see `PACKAGE-ISOLATES.md`) lets
    // the *same* `(origin, name)` automation coexist across isolates — e.g. a package loaded
    // both in `Main` and in its own sandbox registers two namespaces instead of clobbering
    // via upsert.
    trigger_indices: HashMap<(IsolateId, Origin), HashMap<String, usize>>,
    alias_indices: HashMap<(IsolateId, Origin), HashMap<String, usize>>,
    /// Reused per-line matching storage (see [`LineScratch`]).
    line_scratch: LineScratch,
    /// Interned `(IsolateId, Origin)` namespaces (see [`NamespaceId`]). Grows only at
    /// registration; a namespace keeps its id for the manager's life.
    namespaces: HashMap<(IsolateId, Origin), NamespaceId>,
    /// The registry generation (see [`AutomationIdentity`]): bumped on every mutation of
    /// `triggers`/`aliases` or their index maps, so cached fire-counter slots taken under an
    /// older generation fall back to the name lookup.
    registry_generation: u64,
    /// Indices into `triggers` of every trigger that declares a `line_limit`. A side list so the
    /// per-incoming-line `count_tested_lines` self-limit tick visits only the (rare) line-limited
    /// triggers instead of scanning all of them — keeping the common no-line-limit profile O(1)
    /// per line rather than O(trigger-count). Recomputed in
    /// [`rebuild_trigger_regex_set`](Self::rebuild_trigger_regex_set), the same dirty-gated point
    /// the trigger `PatternSet`s rebuild, so it never holds stale indices.
    line_limited_triggers: Vec<usize>,
    trigger_regex_set_dirty: bool,
    alias_regex_set_dirty: bool,
    command_separator: Arc<String>,
    /// Controls whether SGR bold promotes a normal ANSI foreground color to
    /// its bright variant during matching with a color filter. The
    /// session thread caches this setting here. The unfiltered trigger path
    /// does not read it.
    bold_is_bright: bool,
    /// While ≥1 window is subscribed (the runtime sets this from the automation broadcast's
    /// receiver count, so it covers any number of windows), each add/enable on a
    /// script-created (non-`User`) automation records an [`AutomationDelta`] here; the
    /// runtime flushes them at its queue-drain point. Empty and unrecorded otherwise.
    recording: bool,
    automation_deltas: Vec<AutomationDelta>,
    /// Introspection mirror shared with the `get`/`list`/`exists` ops. The `Manager`
    /// is the sole writer; it refreshes the entry on every add/enable/remove so a synchronous
    /// op read sees the live `enabled`/`pattern`.
    automation_registry: SharedAutomationRegistry,
    /// Whether any trigger (enabled or not) carries a raw pattern. Shared with the
    /// connection's [`VtProcessor`], which captures `StyledLine::raw` — a per-line
    /// lossy copy of the wire bytes whose only consumer is raw matching — only while
    /// this is set. Kept true across enable/disable so a disabled raw trigger's
    /// re-enable never races the capture of in-flight lines.
    raw_wanted: Arc<AtomicBool>,
    /// Set by every alias mutation; gates the [`Self::command_names_update`]
    /// recompute so the common no-Command profile pays one bool per mutation.
    command_names_dirty: bool,
    /// The last command-name list handed to the UI, for change detection —
    /// unrelated alias churn must not re-send an identical list.
    last_command_names: Vec<Arc<String>>,
}

/// Feature-gated observation handle for trigger benchmarks.
///
/// The runtime action queue remains an internal implementation detail; benches
/// only need to count, clear, and test whether trigger actions were emitted.
#[cfg(feature = "bench-api")]
#[derive(Clone, Debug)]
pub struct BenchActionQueue(ActionQueue);

#[cfg(feature = "bench-api")]
impl BenchActionQueue {
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// A single regex capture group from a trigger/alias match.
///
/// Captures are carried as an **ordered** `Vec<MatchCapture>`: position in the vec *is*
/// the group number (`captures[0]` is the whole match, `captures[1..]` the parenthesized
/// groups in pattern order). `name` is `Some` only for named groups (`(?<name>…)`). A
/// group that did not participate in the match has an empty `value`.
///
/// `MatchCapture` also carries host-routed interop deliveries (event/watch/procedure
/// captures), whose names are the fixed literals `event`/`payload`/`path`/`snapshot`/
/// `sender`. `name` is a `Cow` to serve both producers: regex aliases own their
/// dynamic, author-written group names; interop deliveries borrow their literals with no
/// per-delivery allocation. Incoming triggers use source ranges instead.
#[derive(Debug, Clone)]
pub struct MatchCapture {
    /// The named-group name (`(?<name>…)`) or an interop capture's literal name; `None`
    /// for an unnamed group.
    pub name: Option<std::borrow::Cow<'static, str>>,
    /// The matched text, or empty when the group did not participate.
    pub value: String,
}

/// Expands a bash-style inline template against borrowed capture values in a
/// single left-to-right tokenizing pass.
///
/// Grammar (see the `JSDoc` in `js/smudgy.js` for the user-facing contract):
/// - `${N}` / `${name}` — braced reference; `N` is a (multi-digit) group number,
///   `name` an identifier resolving a named group.
/// - `$N` — a **single** digit group reference (so `$10` is group 1 then a literal `0`;
///   use `${10}` for group ten).
/// - `$name` — an identifier (`[A-Za-z_][A-Za-z0-9_]*`) resolving a named group.
/// - `$$` — a literal `$`.
/// - A `$` not starting any of the above is emitted literally.
///
/// Unknown / empty / non-participating groups expand to the empty string.
#[must_use]
pub(crate) fn expand_template_view(template: &str, captures: CaptureView<'_>) -> String {
    let lookup_index = |idx: usize| -> &str { captures.get(idx).map_or("", |c| c.value) };
    let lookup_name = |name: &str| -> &str {
        captures
            .iter()
            .find(|c| c.name == Some(name))
            .map_or("", |c| c.value)
    };

    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Advance one full UTF-8 char (templates may contain non-ASCII text).
            let ch = template[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // We are at a `$`.
        let next = bytes.get(i + 1).copied();
        match next {
            Some(b'$') => {
                out.push('$');
                i += 2;
            }
            Some(b'{') => {
                // `${...}` — scan to the closing brace.
                if let Some(rel_close) = template[i + 2..].find('}') {
                    let inner = &template[i + 2..i + 2 + rel_close];
                    if inner.chars().all(|c| c.is_ascii_digit()) && !inner.is_empty() {
                        if let Ok(idx) = inner.parse::<usize>() {
                            out.push_str(lookup_index(idx));
                        }
                    } else {
                        out.push_str(lookup_name(inner));
                    }
                    i += 2 + rel_close + 1;
                } else {
                    // No closing brace: emit the `$` literally and continue past it.
                    out.push('$');
                    i += 1;
                }
            }
            Some(b'0'..=b'9') => {
                // Single-digit group reference.
                let digit = (next.unwrap() - b'0') as usize;
                out.push_str(lookup_index(digit));
                i += 2;
            }
            Some(c) if c == b'_' || c.is_ascii_alphabetic() => {
                // `$identifier` — consume the identifier run.
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric())
                {
                    end += 1;
                }
                let name = &template[start..end];
                out.push_str(lookup_name(name));
                i = end;
            }
            _ => {
                // Lone `$` (end of string or followed by something inert): literal.
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
fn expand_template(template: &str, captures: &[MatchCapture]) -> String {
    expand_template_view(template, CaptureView::Owned(captures))
}

/// Splits an outgoing chunk into commands: always on '\n', additionally on
/// `separator` when it is non-empty.
#[must_use]
pub fn split_commands<'a>(text: &'a str, separator: &str) -> Vec<&'a str> {
    if separator.is_empty() {
        text.split('\n').collect()
    } else {
        text.split('\n')
            .flat_map(|chunk| chunk.split(separator))
            .collect()
    }
}

#[derive(Clone, Copy)]
enum TriggerMatchType {
    Normal,
    Raw,
}

enum ColorQualification {
    Unfiltered,
    Matched(usize),
}

const ATTR_BOLD: u16 = 1 << 0;
const ATTR_FAINT: u16 = 1 << 1;
const ATTR_ITALIC: u16 = 1 << 2;
const ATTR_UNDERLINE: u16 = 1 << 3;
const ATTR_DOUBLE_UNDERLINE: u16 = 1 << 4;
const ATTR_SLOW_BLINK: u16 = 1 << 5;
const ATTR_FAST_BLINK: u16 = 1 << 6;
const ATTR_CROSSED_OUT: u16 = 1 << 7;
const ATTR_REVERSE: u16 = 1 << 8;

#[derive(Debug, Clone, Copy)]
struct CompiledHsvRange {
    hue_from: u16,
    hue_to: u16,
    saturation_min: u8,
    saturation_max: u8,
    value_min: u8,
    value_max: u8,
}

impl From<MatcherHsvRange> for CompiledHsvRange {
    fn from(range: MatcherHsvRange) -> Self {
        let range = range.rgb_canonicalized();
        let (hue_from, hue_to) = range.directed_hue_bounds();
        let (saturation_min, saturation_max) = range.saturation_bounds();
        let (value_min, value_max) = range.value_bounds();
        Self {
            hue_from,
            hue_to,
            saturation_min,
            saturation_max,
            value_min,
            value_max,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CompiledMatcherColor {
    Exact(Color),
    HsvRange(CompiledHsvRange),
}

impl CompiledMatcherColor {
    fn compile(color: MatcherColor) -> Self {
        match color {
            MatcherColor::Truecolor {
                range: Some(range), ..
            } => Self::HsvRange(range.into()),
            _ => Self::Exact(matcher_terminal_color(color)),
        }
    }

    #[inline]
    fn matches(self, actual: Color) -> bool {
        match self {
            Self::Exact(expected) => actual == expected,
            Self::HsvRange(range) => {
                let Color::Rgb { r, g, b } = actual else {
                    return false;
                };
                let hsv = MatcherHsv::from_rgb(r, g, b);
                let hue_matches = if hsv.saturation == 0 {
                    // Colors with zero saturation have no defined hue.
                    // Saturation and value must still match their separate
                    // bounds.
                    true
                } else {
                    let hue = if hsv.hue < range.hue_from {
                        hsv.hue + MatcherHsv::HUE_PERIOD
                    } else {
                        hsv.hue
                    };
                    (range.hue_from..=range.hue_to).contains(&hue)
                };
                hue_matches
                    && (range.saturation_min..=range.saturation_max).contains(&hsv.saturation)
                    && (range.value_min..=range.value_max).contains(&hsv.value)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CompiledColorMatch {
    foreground: Option<CompiledMatcherColor>,
    background: Option<CompiledMatcherColor>,
    required_attributes: u16,
}

/// One displayed-text row supplied by the scripting facade. Unlike the
/// persisted matcher sidecar, the source and its style predicate cannot drift
/// apart: they cross the native boundary as one value.
#[derive(Debug, Clone)]
pub(crate) struct ScriptTriggerPattern {
    pub(crate) source: String,
    pub(crate) style: Option<MatcherColorMatch>,
}

/// Trigger-local regexes and style predicates compiled before a script create
/// op reserves a singleton identity or stores a V8 function. Dispatch can
/// therefore install this value without another fallible compilation step.
#[derive(Debug, Clone)]
pub struct PreparedScriptTriggerPatterns {
    patterns: Vec<Regex>,
    pattern_colors: Vec<Option<CompiledColorMatch>>,
    raw_patterns: Vec<Regex>,
    anti_patterns: RegexSet,
    colored_anti_pattern_set: Option<RegexSet>,
    colored_anti_patterns: Vec<(Regex, CompiledColorMatch)>,
}

impl CompiledColorMatch {
    fn compile(matcher: &MatcherColorMatch) -> Self {
        let mut required_attributes = 0;
        for attribute in &matcher.attributes {
            required_attributes |= matcher_attribute_bit(*attribute);
        }
        Self {
            foreground: matcher.foreground.map(CompiledMatcherColor::compile),
            background: matcher.background.map(CompiledMatcherColor::compile),
            required_attributes,
        }
    }

    const fn is_unconstrained(self) -> bool {
        self.foreground.is_none() && self.background.is_none() && self.required_attributes == 0
    }
}

const fn matcher_attribute_bit(attribute: MatcherTextAttribute) -> u16 {
    match attribute {
        MatcherTextAttribute::Bold => ATTR_BOLD,
        MatcherTextAttribute::Faint => ATTR_FAINT,
        MatcherTextAttribute::Italic => ATTR_ITALIC,
        MatcherTextAttribute::Underline => ATTR_UNDERLINE,
        MatcherTextAttribute::DoubleUnderline => ATTR_DOUBLE_UNDERLINE,
        MatcherTextAttribute::SlowBlink => ATTR_SLOW_BLINK,
        MatcherTextAttribute::FastBlink => ATTR_FAST_BLINK,
        MatcherTextAttribute::CrossedOut => ATTR_CROSSED_OUT,
        MatcherTextAttribute::Reverse => ATTR_REVERSE,
    }
}

/// Validate the persisted and script paths at their shared compilation point.
/// `MatcherColor::Ansi` is an external-data shape, so do not let a forged
/// index reach `ansi_color` and silently alias through its modulo conversion.
fn validate_color_match(matcher: &MatcherColorMatch, path: &str) -> Result<()> {
    for (channel, color) in [
        ("foreground", matcher.foreground),
        ("background", matcher.background),
    ] {
        if let Some(MatcherColor::Ansi { index }) = color
            && index > 15
        {
            bail!("{path}.{channel} ANSI index must be between 0 and 15");
        }
    }

    let mut seen = 0_u16;
    for attribute in &matcher.attributes {
        let bit = matcher_attribute_bit(*attribute);
        if seen & bit != 0 {
            bail!("{path}.attributes contains duplicate {attribute:?}");
        }
        seen |= bit;
    }
    if seen & (ATTR_UNDERLINE | ATTR_DOUBLE_UNDERLINE) == ATTR_UNDERLINE | ATTR_DOUBLE_UNDERLINE {
        bail!("{path}.attributes cannot require both single and double underline");
    }
    if seen & (ATTR_SLOW_BLINK | ATTR_FAST_BLINK) == ATTR_SLOW_BLINK | ATTR_FAST_BLINK {
        bail!("{path}.attributes cannot require both slow and fast blink");
    }
    Ok(())
}

const fn ansi_color(index: u8) -> crate::session::connection::vt_processor::AnsiColor {
    use crate::session::connection::vt_processor::AnsiColor;
    match index % 8 {
        0 => AnsiColor::Black,
        1 => AnsiColor::Red,
        2 => AnsiColor::Green,
        3 => AnsiColor::Yellow,
        4 => AnsiColor::Blue,
        5 => AnsiColor::Magenta,
        6 => AnsiColor::Cyan,
        _ => AnsiColor::White,
    }
}

/// Converts a persisted matcher color to the representation used by
/// [`StyledLine`]. It preserves ANSI slots and xterm indices 0 through 15. It
/// converts other xterm colors and truecolor values to RGB.
fn matcher_terminal_color(color: MatcherColor) -> Color {
    match color {
        MatcherColor::Ansi { index } => Color::Ansi {
            color: ansi_color(index),
            bold: index >= 8,
        },
        MatcherColor::Xterm { index } if index < 16 => Color::Ansi {
            color: ansi_color(index),
            bold: index >= 8,
        },
        MatcherColor::Xterm { index } if index < 232 => {
            let n = index - 16;
            let component = |level: u8| if level == 0 { 0 } else { 55 + 40 * level };
            Color::Rgb {
                r: component(n / 36),
                g: component((n % 36) / 6),
                b: component(n % 6),
            }
        }
        MatcherColor::Xterm { index } => {
            let value = 8 + 10 * (index - 232);
            Color::Rgb {
                r: value,
                g: value,
                b: value,
            }
        }
        MatcherColor::Truecolor { r, g, b, .. } => Color::Rgb { r, g, b },
    }
}

#[inline]
const fn effective_foreground(style: Style, bold_is_bright: bool) -> Color {
    if !bold_is_bright || !style.attributes.bold {
        return style.fg;
    }
    match style.fg {
        Color::Ansi { color, bold: false } => Color::Ansi { color, bold: true },
        Color::DefaultForeground { bold: false } => Color::DefaultForeground { bold: true },
        foreground => foreground,
    }
}

#[inline]
fn style_matches(style: Style, matcher: CompiledColorMatch, bold_is_bright: bool) -> bool {
    if matcher
        .foreground
        .is_some_and(|color| !color.matches(effective_foreground(style, bold_is_bright)))
        || matcher
            .background
            .is_some_and(|color| !color.matches(style.bg))
    {
        return false;
    }

    let required = matcher.required_attributes;
    required == 0
        || (required & ATTR_BOLD == 0 || style.attributes.bold)
            && (required & ATTR_FAINT == 0 || style.attributes.faint)
            && (required & ATTR_ITALIC == 0 || style.attributes.italic)
            && (required & ATTR_UNDERLINE == 0 || style.attributes.underline == Underline::Single)
            && (required & ATTR_DOUBLE_UNDERLINE == 0
                || style.attributes.underline == Underline::Double)
            && (required & ATTR_SLOW_BLINK == 0 || style.attributes.blink == Blink::Slow)
            && (required & ATTR_FAST_BLINK == 0 || style.attributes.blink == Blink::Fast)
            && (required & ATTR_CROSSED_OUT == 0 || style.attributes.crossed_out)
            && (required & ATTR_REVERSE == 0 || style.attributes.reverse)
}

/// Returns the start byte of the first regex match whose style at that byte
/// satisfies the filter. Regex matches and VT spans have the same order. A
/// single monotonic span cursor keeps the search at O(matches + spans). An
/// empty regex defines a color-only matcher. It scans spans directly and
/// avoids a regex match at each character boundary.
fn color_matched_start(
    regex: &Regex,
    subject: &str,
    line: &StyledLine,
    matcher: CompiledColorMatch,
    bold_is_bright: bool,
) -> Option<usize> {
    // An empty regex requires no text search. Scan the spans directly in
    // O(spans). This avoids a regex match at each character boundary.
    if regex.as_str().is_empty() {
        if subject.is_empty() {
            // The VT processor records the empty line's cursor style in the
            // final zero-width span. Earlier zero-width spans record
            // superseded transitions at the same position.
            return line
                .spans
                .last()
                .filter(|span| style_matches(span.style, matcher, bold_is_bright))
                .map(|_| 0);
        }
        return line
            .spans
            .iter()
            // The VT processor records a zero-width span when one style
            // transition replaces another before more text arrives. The span
            // preserves cursor history but contains no text. Ignore it so a
            // color-only matcher checks only styles that apply to text.
            .find(|span| {
                span.begin_pos < span.end_pos && style_matches(span.style, matcher, bold_is_bright)
            })
            .map(|span| span.begin_pos.min(subject.len()));
    }

    let mut span_index = 0;
    let mut cached_span_index = usize::MAX;
    let mut cached_style_matches = false;
    regex.find_iter(subject).find_map(|matched| {
        let start = matched.start();
        while line
            .spans
            .get(span_index)
            .is_some_and(|span| span.end_pos <= start)
        {
            span_index += 1;
        }
        let span = line.spans.get(span_index)?;
        if span.begin_pos > start || start >= span.end_pos {
            return None;
        }
        if cached_span_index != span_index {
            cached_span_index = span_index;
            cached_style_matches = style_matches(span.style, matcher, bold_is_bright);
        }
        cached_style_matches.then_some(start)
    })
}

impl PreparedScriptTriggerPatterns {
    /// Validates and compiles a script-created trigger before the op mutates
    /// singleton or function registries. Empty predicates first normalize to
    /// an unfiltered row, then empty unfiltered rows are discarded.
    pub(crate) fn prepare(
        patterns: Vec<ScriptTriggerPattern>,
        raw_patterns: Vec<String>,
        anti_patterns: Vec<ScriptTriggerPattern>,
    ) -> Result<Self> {
        // Historically, an explicitly supplied empty normal/raw pattern was
        // accepted as an inert trigger. Preserve that registration contract;
        // it is distinct from supplying no positive leaves at all.
        let has_inert_plain_positive = patterns
            .iter()
            .any(|row| row.source.is_empty() && row.style.is_none())
            || raw_patterns.iter().any(String::is_empty);
        let prepared = Self::compile(patterns, raw_patterns, anti_patterns)?;
        if prepared.patterns.is_empty()
            && prepared.raw_patterns.is_empty()
            && !has_inert_plain_positive
        {
            bail!("a trigger requires at least one normal or raw positive pattern");
        }
        Ok(prepared)
    }

    /// Shared compiler for persisted rows and script-created paired rows.
    fn compile(
        pattern_rows: Vec<ScriptTriggerPattern>,
        raw_pattern_sources: Vec<String>,
        anti_rows: Vec<ScriptTriggerPattern>,
    ) -> Result<Self> {
        let mut patterns = Vec::with_capacity(pattern_rows.len());
        let mut pattern_colors = Vec::with_capacity(pattern_rows.len());
        for (index, row) in pattern_rows.into_iter().enumerate() {
            let color = match row.style.as_ref() {
                Some(style) => {
                    validate_color_match(style, &format!("normal[{index}].style"))?;
                    let compiled = CompiledColorMatch::compile(style);
                    (!compiled.is_unconstrained()).then_some(compiled)
                }
                None => None,
            };

            // Empty unfiltered rows are inert. Normalize the predicate first:
            // otherwise `style: {}` would retain an empty regex and silently
            // become a match-all trigger.
            if row.source.is_empty() && color.is_none() {
                continue;
            }
            let regex = Regex::new(&row.source)
                .with_context(|| format!("normal[{index}] contains an invalid regex"))?;
            patterns.push(regex);
            pattern_colors.push(color);
        }
        if pattern_colors.iter().all(Option::is_none) {
            pattern_colors.clear();
        }

        let raw_patterns = raw_pattern_sources
            .into_iter()
            .enumerate()
            .filter(|(_, source)| !source.is_empty())
            .map(|(index, source)| {
                Regex::new(&source)
                    .with_context(|| format!("raw[{index}] contains an invalid regex"))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut plain_anti_patterns = Vec::new();
        let mut colored_anti_patterns = Vec::new();
        for (index, row) in anti_rows.into_iter().enumerate() {
            let color = match row.style.as_ref() {
                Some(style) => {
                    validate_color_match(style, &format!("anti[{index}].style"))?;
                    let compiled = CompiledColorMatch::compile(style);
                    (!compiled.is_unconstrained()).then_some(compiled)
                }
                None => None,
            };
            if row.source.is_empty() && color.is_none() {
                continue;
            }
            let regex = Regex::new(&row.source)
                .with_context(|| format!("anti[{index}] contains an invalid regex"))?;
            if let Some(color) = color {
                colored_anti_patterns.push((regex, color));
            } else {
                plain_anti_patterns.push(regex.as_str().to_string());
            }
        }
        let anti_patterns = RegexSet::new(plain_anti_patterns)
            .context("plain anti-pattern set contains an invalid regex")?;
        let colored_anti_pattern_set = (!colored_anti_patterns.is_empty())
            .then(|| {
                RegexSet::new(
                    colored_anti_patterns
                        .iter()
                        .map(|(regex, _)| regex.as_str()),
                )
            })
            .transpose()
            .context("styled anti-pattern set contains an invalid regex")?;

        Ok(Self {
            patterns,
            pattern_colors,
            raw_patterns,
            anti_patterns,
            colored_anti_pattern_set,
            colored_anti_patterns,
        })
    }
}

pub struct PushTriggerParams<'a> {
    pub isolate: IsolateId,
    pub origin: Origin,
    pub name: &'a Arc<String>,
    pub patterns: &'a Arc<Vec<String>>,
    pub raw_patterns: &'a Arc<Vec<String>>,
    pub anti_patterns: &'a Arc<Vec<String>>,
    /// Matcher rows from the editor. The stored regex vectors are authoritative.
    /// The runtime uses these color filters only if compiling the rows
    /// reproduces every stored vector exactly.
    pub matchers: Option<&'a [TriggerMatcherSource]>,
    pub action: ScriptAction,
    pub prompt: bool,
    pub enabled: bool,
    pub priority: i32,
    pub fallthrough: bool,
    pub fire_limit: Option<u32>,
    pub line_limit: Option<u32>,
    /// Display-only body source for the read-only detail pane: the JS/TS eval string, or a
    /// function's `toString()`. `None` for plaintext (the command is recoverable from
    /// `action`) or when no source was supplied.
    pub source: Option<Arc<str>>,
}

impl Manager {
    pub(crate) fn new(
        spawned_actions: ActionQueue,
        command_separator: Arc<String>,
        automation_registry: SharedAutomationRegistry,
    ) -> Self {
        let triggers = Vec::new();
        let aliases = Vec::new();
        let trigger_indices = HashMap::new();
        let alias_indices = HashMap::new();
        let trigger_regex_set = PatternSet::empty();
        let raw_trigger_regex_set = PatternSet::empty();
        let prompt_trigger_regex_set = PatternSet::empty();
        let prompt_raw_trigger_regex_set = PatternSet::empty();
        let alias_regex_set = PatternSet::empty();

        Self {
            alias_regex_set,
            trigger_regex_set,
            raw_trigger_regex_set,
            prompt_trigger_regex_set,
            prompt_raw_trigger_regex_set,
            alias_regex_set_map: Vec::new(),
            trigger_regex_set_map: Vec::new(),
            raw_trigger_regex_set_map: Vec::new(),
            prompt_trigger_regex_set_map: Vec::new(),
            prompt_raw_trigger_regex_set_map: Vec::new(),
            alias_regex_patterns_map: Vec::new(),
            trigger_regex_patterns_map: Vec::new(),
            raw_trigger_regex_patterns_map: Vec::new(),
            prompt_trigger_regex_patterns_map: Vec::new(),
            prompt_raw_trigger_regex_patterns_map: Vec::new(),
            aliases,
            triggers,
            alias_indices,
            trigger_indices,
            line_scratch: LineScratch::default(),
            namespaces: HashMap::new(),
            registry_generation: next_registry_generation(),
            line_limited_triggers: Vec::new(),
            spawned_actions,
            trigger_regex_set_dirty: true,
            alias_regex_set_dirty: true,
            command_separator,
            bold_is_bright: crate::models::settings::TerminalBoldMode::default()
                .uses_bright_palette(),
            recording: false,
            automation_deltas: Vec::new(),
            automation_registry,
            raw_wanted: Arc::new(AtomicBool::new(false)),
            command_names_dirty: false,
            last_command_names: Vec::new(),
        }
    }

    /// The enabled Command-alias names for tab completion, when they changed
    /// since the last take; `None` means no change. Dirty-gated on alias
    /// mutations and compared against the last take, so unrelated alias churn
    /// never re-sends an identical list.
    pub fn command_names_update(&mut self) -> Option<Arc<Vec<Arc<String>>>> {
        if !self.command_names_dirty {
            return None;
        }
        self.command_names_dirty = false;
        let mut names: Vec<Arc<String>> = self
            .aliases
            .iter()
            .filter(|alias| alias.enabled)
            .filter_map(|alias| alias.command.as_ref().map(|c| Arc::new(c.name.clone())))
            .collect();
        names.sort();
        names.dedup();
        if names == self.last_command_names {
            return None;
        }
        self.last_command_names.clone_from(&names);
        Some(Arc::new(names))
    }

    /// Construct the real trigger manager plus its feature-gated queue
    /// observation handle for workspace benchmarks.
    #[cfg(feature = "bench-api")]
    #[must_use]
    pub fn new_for_bench(
        command_separator: Arc<String>,
        automation_registry: SharedAutomationRegistry,
    ) -> (Self, BenchActionQueue) {
        let spawned_actions: ActionQueue = Rc::new(RefCell::default());
        let queue = BenchActionQueue(spawned_actions.clone());
        (
            Self::new(spawned_actions, command_separator, automation_registry),
            queue,
        )
    }

    /// The shared "any trigger has a raw pattern" flag, for wiring into the
    /// connection's raw-byte capture.
    #[must_use]
    pub fn raw_wanted_flag(&self) -> Arc<AtomicBool> {
        self.raw_wanted.clone()
    }

    /// Continue writing to a predecessor manager's flag cell instead of this
    /// manager's own. A reload rebuilds the manager but keeps the connection —
    /// and with it the `VtProcessor`'s clone of the old cell — alive; adopting
    /// keeps that clone live. Syncs the cell to this manager's (empty) trigger
    /// set; the reloading modules' re-registrations raise it again.
    pub fn adopt_raw_wanted_flag(&mut self, flag: Arc<AtomicBool>) {
        self.raw_wanted = flag;
        self.refresh_raw_wanted();
    }

    /// Recompute [`Self::raw_wanted`] after a trigger mutation. Runs eagerly (not at the
    /// dirty-gated `PatternSet` rebuild) so capture starts before the next line arrives,
    /// not after it.
    fn refresh_raw_wanted(&self) {
        let wanted = self
            .triggers
            .iter()
            .any(|trigger| !trigger.raw_patterns.is_empty());
        self.raw_wanted.store(wanted, Ordering::Relaxed);
    }

    /// Set by the runtime from the automation broadcast's receiver count: record deltas
    /// while ≥1 window is subscribed. Turning recording off drops any buffered deltas (the
    /// next subscriber gets a fresh reset first).
    pub fn set_recording(&mut self, on: bool) {
        if !on && self.recording {
            self.automation_deltas.clear();
        }
        self.recording = on;
    }

    /// Whether any automations window is subscribed (gates delta recording).
    fn is_watched(&self) -> bool {
        self.recording
    }

    /// The current full set of script-created (non-`User`) automations, for the reset a
    /// window receives when it starts watching. User/disk automations are shown from disk
    /// and scripts can't touch the user namespace, so they're excluded.
    pub fn automation_reset(&self) -> Vec<AutomationSummary> {
        let aliases = self
            .aliases
            .iter()
            .filter(|item| item.origin != Origin::User)
            .map(|item| Self::summary(AutomationKind::Alias, item));
        let triggers = self
            .triggers
            .iter()
            .filter(|item| item.origin != Origin::User)
            .map(|item| Self::summary(AutomationKind::Trigger, item));
        aliases.chain(triggers).collect()
    }

    /// Whether there are buffered deltas to flush (checked at the runtime drain point).
    pub fn has_automation_deltas(&self) -> bool {
        !self.automation_deltas.is_empty()
    }

    /// Drains the buffered deltas for the runtime to emit.
    pub fn take_automation_deltas(&mut self) -> Vec<AutomationDelta> {
        std::mem::take(&mut self.automation_deltas)
    }

    fn summary(kind: AutomationKind, item: &Trigger) -> AutomationSummary {
        AutomationSummary {
            kind,
            origin: item.origin.clone(),
            name: item.name.clone(),
            enabled: item.enabled,
            pattern: Self::pattern_display(item),
            body: Self::body_display(item),
        }
    }

    /// The match pattern(s) joined into one display string: regex sources for the match
    /// patterns first, then the raw patterns, ` | `-separated. Empty when there are none.
    fn pattern_display(item: &Trigger) -> Arc<str> {
        let mut out = String::new();
        for re in item.patterns.iter().chain(item.raw_patterns.iter()) {
            if !out.is_empty() {
                out.push_str(" | ");
            }
            out.push_str(re.as_str());
        }
        Arc::from(out)
    }

    /// What the automation does, for the read-only detail pane. Prefers the captured `source`
    /// (eval string / function `toString()`); for plaintext the command is recovered from the
    /// `ScriptAction` itself.
    fn body_display(item: &Trigger) -> AutomationBody {
        match &item.script {
            ScriptAction::SendRaw(s) | ScriptAction::SendSimple(s) => AutomationBody::Command(
                item.source.clone().unwrap_or_else(|| Arc::from(s.as_str())),
            ),
            ScriptAction::EvalJavascript(_) | ScriptAction::CallJavascriptFunction(_) => {
                AutomationBody::Script(item.source.clone())
            }
            ScriptAction::Noop => AutomationBody::Noop,
        }
    }

    /// Replaces the separator used to split plaintext alias/trigger bodies
    /// into commands. Used by the `ApplySettings` handler for live updates.
    pub fn set_command_separator(&mut self, separator: Arc<String>) {
        self.command_separator = separator;
    }

    /// Sets the effective ANSI foreground brightness policy for SGR bold.
    /// Matching with a color filter reads this cached value. This avoids a
    /// settings load in the trigger hot path.
    pub fn set_bold_is_bright(&mut self, bold_is_bright: bool) {
        self.bold_is_bright = bold_is_bright;
    }

    /// The pattern source the JS `.pattern` handle reads back: the first pattern's regex
    /// source, or empty when an automation has none.
    fn pattern_of(item: &Trigger) -> String {
        item.patterns
            .first()
            .map_or_else(String::new, |re| re.as_str().to_string())
    }

    /// Mirror one automation into the shared introspection registry. `kind` selects the
    /// alias/trigger map; the entry is keyed by `(isolate, origin)` then name.
    fn registry_upsert(&self, kind: AutomationKind, item: &Trigger) {
        let entry = AutomationEntry {
            enabled: item.enabled,
            pattern: Self::pattern_of(item),
            priority: item.priority,
            fallthrough: item.fallthrough,
        };
        let key = (item.isolate.clone(), item.origin.clone());
        let mut registry = self.automation_registry.borrow_mut();
        // The introspection mirror tracks only aliases/triggers; hotkeys are keyed for
        // origin-scoping but live in dispatch's own `HotkeyId` map, never reaching this helper.
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        map.entry(key).or_default().insert(item.name.clone(), entry);
    }

    /// Drop one automation from the shared introspection registry (on remove).
    fn registry_remove(
        &self,
        kind: AutomationKind,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
    ) {
        let mut registry = self.automation_registry.borrow_mut();
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        if let Some(namespace) = map.get_mut(&(isolate.clone(), origin.clone())) {
            namespace.remove(name);
        }
    }

    /// Flip one automation's `enabled` in the shared introspection registry (on enable/disable).
    fn registry_set_enabled(
        &self,
        kind: AutomationKind,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut registry = self.automation_registry.borrow_mut();
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        if let Some(entry) = map
            .get_mut(&(isolate.clone(), origin.clone()))
            .and_then(|namespace| namespace.get_mut(name))
        {
            entry.enabled = enabled;
        }
    }

    /// Give a registering automation its shared identity (see [`AutomationIdentity`]),
    /// interning its `(isolate, origin)` namespace on first sight.
    fn assign_identity(&mut self, item: &mut Trigger) {
        let key = (item.isolate.clone(), item.origin.clone());
        let next = NamespaceId(u32::try_from(self.namespaces.len()).expect("namespace count"));
        let namespace = *self.namespaces.entry(key).or_insert(next);
        item.identity = Some(Arc::new(AutomationIdentity::new(
            item.isolate.clone(),
            item.origin.clone(),
            &item.name,
            item.is_alias,
            namespace,
        )));
    }

    fn add_or_update_alias(&mut self, mut alias: Trigger) {
        debug!(
            "Adding or updating alias: {:?}, {:?}, {:?}",
            alias.origin, alias.name, alias.patterns
        );
        self.assign_identity(&mut alias);
        self.registry_upsert(AutomationKind::Alias, &alias);
        let delta = (self.is_watched() && alias.origin != Origin::User)
            .then(|| AutomationDelta::Upserted(Self::summary(AutomationKind::Alias, &alias)));
        // Keyed by (isolate, origin, name): re-creating the same alias in the same isolate
        // upserts in place, while a same-named alias from a different origin OR a different
        // isolate coexists.
        let key = (alias.isolate.clone(), alias.origin.clone());
        if let Some(index) = self
            .alias_indices
            .get(&key)
            .and_then(|by_name| by_name.get(&alias.name))
            .copied()
        {
            *self.aliases.get_mut(index).unwrap() = alias;
        } else {
            let index = self.aliases.len();
            self.alias_indices
                .entry(key)
                .or_default()
                .insert(alias.name.clone(), index);
            self.aliases.push(alias);
        }
        self.registry_generation = next_registry_generation();
        // Defer the (expensive) PatternSet rebuild to the next outgoing line,
        // exactly like triggers do via `trigger_regex_set_dirty`. Rebuilding
        // eagerly on every insert made loading N aliases O(N²) — and since each
        // rebuild recompiles the aho-corasick automaton + regexes (far slower in
        // debug builds), a large profile/package alias set could stall the
        // runtime for tens of seconds at session start, delaying `Connect`.
        self.alias_regex_set_dirty = true;
        self.command_names_dirty = true;
        if let Some(delta) = delta {
            self.automation_deltas.push(delta);
        }
    }

    fn add_or_update_trigger(&mut self, mut trigger: Trigger) {
        trace!(
            "Adding or updating trigger: {:?}, {:?}",
            trigger.name, trigger.patterns
        );
        self.assign_identity(&mut trigger);
        self.registry_upsert(AutomationKind::Trigger, &trigger);
        let delta = (self.is_watched() && trigger.origin != Origin::User)
            .then(|| AutomationDelta::Upserted(Self::summary(AutomationKind::Trigger, &trigger)));
        let key = (trigger.isolate.clone(), trigger.origin.clone());
        if let Some(index) = self
            .trigger_indices
            .get(&key)
            .and_then(|by_name| by_name.get(&trigger.name))
            .copied()
        {
            *self.triggers.get_mut(index).unwrap() = trigger;
        } else {
            let index = self.triggers.len();
            self.trigger_indices
                .entry(key)
                .or_default()
                .insert(trigger.name.clone(), index);
            self.triggers.push(trigger);
        }
        self.registry_generation = next_registry_generation();

        self.trigger_regex_set_dirty = true;
        self.refresh_raw_wanted();
        if let Some(delta) = delta {
            self.automation_deltas.push(delta);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_javascript_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: &Arc<String>,
        patterns: &Arc<Vec<String>>,
        script_id: ScriptId,
        priority: i32,
        fallthrough: bool,
        allow_self_match: bool,
        fire_limit: Option<u32>,
        source: Option<Arc<str>>,
        command: Option<crate::models::matchers::CommandSpec>,
    ) -> Result<()> {
        self.add_or_update_alias(
            Trigger::new_alias(
                isolate,
                origin,
                name.to_string(),
                patterns.iter(),
                ScriptAction::EvalJavascript(script_id),
                priority,
                fallthrough,
                fire_limit,
            )?
            .with_source(source)
            .with_command(command)
            .with_allow_self_match(allow_self_match),
        );
        Ok(())
    }

    pub fn push_trigger(&mut self, params: PushTriggerParams) -> Result<()> {
        self.add_or_update_trigger(
            Trigger::new(
                params.isolate,
                params.origin,
                params.name.to_string(),
                params.patterns.iter(),
                params.raw_patterns.iter(),
                params.anti_patterns.iter(),
                params.matchers,
                params.action,
                params.prompt,
                params.enabled,
                params.priority,
                params.fallthrough,
                params.fire_limit,
                params.line_limit,
            )?
            .with_source(params.source),
        );
        Ok(())
    }

    /// Installs a script trigger whose trigger-local regexes and predicates
    /// were prepared synchronously by the registration op.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_script_trigger(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: String,
        prepared: PreparedScriptTriggerPatterns,
        action: ScriptAction,
        prompt: bool,
        enabled: bool,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
        source: Option<Arc<str>>,
    ) {
        self.add_or_update_trigger(
            Trigger::from_prepared(
                isolate,
                origin,
                name,
                prepared,
                action,
                prompt,
                enabled,
                priority,
                fallthrough,
                fire_limit,
                line_limit,
            )
            .with_source(source),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_javascript_function_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        function_id: FunctionId,
        priority: i32,
        fallthrough: bool,
        allow_self_match: bool,
        fire_limit: Option<u32>,
        source: Option<Arc<str>>,
        command: Option<crate::models::matchers::CommandSpec>,
    ) -> Result<()> {
        self.add_or_update_alias(
            Trigger::new_alias(
                isolate,
                origin,
                name.to_string(),
                patterns.iter(),
                ScriptAction::CallJavascriptFunction(function_id),
                priority,
                fallthrough,
                fire_limit,
            )?
            .with_source(source)
            .with_command(command)
            .with_allow_self_match(allow_self_match),
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_simple_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        script: Arc<String>,
        priority: i32,
        fallthrough: bool,
        allow_self_match: bool,
        fire_limit: Option<u32>,
        command: Option<crate::models::matchers::CommandSpec>,
    ) -> Result<()> {
        self.add_or_update_alias(
            Trigger::new_alias(
                isolate,
                origin,
                name.to_string(),
                patterns.iter(),
                ScriptAction::SendSimple(script),
                priority,
                fallthrough,
                fire_limit,
            )?
            .with_command(command)
            .with_allow_self_match(allow_self_match),
        );
        Ok(())
    }

    pub fn enable_alias(
        &mut self,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut changed = false;
        if let Some(index) = self
            .alias_indices
            .get(&(isolate.clone(), origin.clone()))
            .and_then(|by_name| by_name.get(name))
            .copied()
            && let Some(alias) = self.aliases.get_mut(index)
        {
            trace!(
                "{} alias: {:?}, {:?}",
                if enabled { "Enabling" } else { "Disabling" },
                alias.name,
                alias.patterns
            );
            alias.enabled = enabled;
            changed = true;
        }
        if changed {
            self.command_names_dirty = true;
            self.registry_set_enabled(AutomationKind::Alias, isolate, origin, name, enabled);
        }
        if changed && self.is_watched() && *origin != Origin::User {
            self.automation_deltas
                .push(AutomationDelta::EnabledChanged {
                    kind: AutomationKind::Alias,
                    origin: origin.clone(),
                    name: name.to_string(),
                    enabled,
                });
        }
    }

    pub fn enable_trigger(
        &mut self,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut changed = false;
        if let Some(index) = self
            .trigger_indices
            .get(&(isolate.clone(), origin.clone()))
            .and_then(|by_name| by_name.get(name))
            .copied()
            && let Some(trigger) = self.triggers.get_mut(index)
        {
            trace!(
                "{} trigger: {:?}, {:?}",
                if enabled { "Enabling" } else { "Disabling" },
                trigger.name,
                trigger.patterns
            );
            trigger.enabled = enabled;
            changed = true;
        }
        if changed {
            self.registry_set_enabled(AutomationKind::Trigger, isolate, origin, name, enabled);
        }
        if changed && self.is_watched() && *origin != Origin::User {
            self.automation_deltas
                .push(AutomationDelta::EnabledChanged {
                    kind: AutomationKind::Trigger,
                    origin: origin.clone(),
                    name: name.to_string(),
                    enabled,
                });
        }
    }

    /// Remove an alias by its `(isolate, origin, name)` key: drop it from the `Vec`,
    /// rebuild the name→index map and the alias `PatternSet` (so its matcher slot is actually
    /// freed — leaving `enabled=false` would keep it resident), drop its introspection-registry
    /// entry, and emit a [`AutomationDelta::Removed`] for the watching UI. A no-op if the key
    /// is unknown (e.g. a double `delete()`).
    pub fn remove_alias(&mut self, isolate: &IsolateId, origin: &Origin, name: &str) {
        if Self::remove_named(
            &mut self.aliases,
            &mut self.alias_indices,
            isolate,
            origin,
            name,
        ) {
            self.registry_generation = next_registry_generation();
            self.alias_regex_set_dirty = true;
            self.command_names_dirty = true;
            self.registry_remove(AutomationKind::Alias, isolate, origin, name);
            if self.is_watched() && *origin != Origin::User {
                self.automation_deltas.push(AutomationDelta::Removed {
                    kind: AutomationKind::Alias,
                    origin: origin.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    /// Remove a trigger by its `(isolate, origin, name)` key: the trigger counterpart of
    /// [`remove_alias`](Self::remove_alias). Marks every trigger `PatternSet` dirty so the slot
    /// is freed across the normal/raw/prompt tiers.
    pub fn remove_trigger(&mut self, isolate: &IsolateId, origin: &Origin, name: &str) {
        if Self::remove_named(
            &mut self.triggers,
            &mut self.trigger_indices,
            isolate,
            origin,
            name,
        ) {
            self.registry_generation = next_registry_generation();
            self.trigger_regex_set_dirty = true;
            self.refresh_raw_wanted();
            self.registry_remove(AutomationKind::Trigger, isolate, origin, name);
            if self.is_watched() && *origin != Origin::User {
                self.automation_deltas.push(AutomationDelta::Removed {
                    kind: AutomationKind::Trigger,
                    origin: origin.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    /// Remove `name` from a `Vec<Trigger>` + its `(isolate, origin) -> name -> index` map,
    /// keeping the remaining indices consistent. `Vec::remove` shifts every later element down
    /// one, so after the removal we rebuild the map from the surviving entries (cheap relative to
    /// the `PatternSet` recompile the caller defers anyway). Returns whether anything was removed.
    fn remove_named(
        items: &mut Vec<Trigger>,
        indices: &mut HashMap<(IsolateId, Origin), HashMap<String, usize>>,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
    ) -> bool {
        let key = (isolate.clone(), origin.clone());
        let Some(index) = indices
            .get(&key)
            .and_then(|by_name| by_name.get(name))
            .copied()
        else {
            return false;
        };
        items.remove(index);
        // The `Vec::remove` shifted later items down one, invalidating every stored index past
        // `index`. Rebuild the whole name→index map from the surviving `Vec` order.
        indices.clear();
        for (i, item) in items.iter().enumerate() {
            indices
                .entry((item.isolate.clone(), item.origin.clone()))
                .or_default()
                .insert(item.name.clone(), i);
        }
        true
    }

    ///
    /// Builds pattern sets for triggers, raw triggers, prompt triggers, and raw prompt triggers
    ///
    /// This could be heavily DRY-ed up, but it just needs to create, for each type of trigger:
    ///  - a `PatternSet` to test when that type of trigger is being tested
    ///  - a `Vec<usize>` to map the indices of the `PatternSet` to the indices of the triggers
    ///  - a `Vec<usize>` to map the indices of the `PatternSet` to the indices of the patterns
    fn rebuild_trigger_regex_set(&mut self) {
        let start = std::time::Instant::now();

        let mut priority_order: Vec<usize> = (0..self.triggers.len()).collect();
        // `sort_by` is stable: equal-priority automations retain their registration order.
        priority_order.sort_by(|&a, &b| self.triggers[b].priority.cmp(&self.triggers[a].priority));

        self.trigger_regex_set = PatternSet::build(priority_order.iter().flat_map(|&i| {
            self.triggers[i]
                .patterns
                .iter()
                .map(|pattern| pattern.as_str())
        }))
        .unwrap();

        self.trigger_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.patterns.len());
                for _ in 0..trigger.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.trigger_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.triggers[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.raw_trigger_regex_set = PatternSet::build(priority_order.iter().flat_map(|&i| {
            self.triggers[i]
                .raw_patterns
                .iter()
                .map(|pattern| pattern.as_str())
        }))
        .unwrap();
        self.raw_trigger_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.raw_patterns.len());
                for _ in 0..trigger.raw_patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.raw_trigger_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.triggers[i]
                    .raw_patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.prompt_trigger_regex_set = PatternSet::build(
            priority_order
                .iter()
                .filter(|&&i| self.triggers[i].fire_on_prompts())
                .flat_map(|&i| {
                    self.triggers[i]
                        .patterns
                        .iter()
                        .map(|pattern| pattern.as_str())
                }),
        )
        .unwrap();
        self.prompt_trigger_regex_set_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.patterns.len());
                for _ in 0..trigger.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.prompt_trigger_regex_patterns_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                self.triggers[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.prompt_raw_trigger_regex_set = PatternSet::build(
            priority_order
                .iter()
                .filter(|&&i| self.triggers[i].fire_on_prompts())
                .flat_map(|&i| {
                    self.triggers[i]
                        .raw_patterns
                        .iter()
                        .map(|pattern| pattern.as_str())
                }),
        )
        .unwrap();
        self.prompt_raw_trigger_regex_set_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.raw_patterns.len());
                for _ in 0..trigger.raw_patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.prompt_raw_trigger_regex_patterns_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                self.triggers[i]
                    .raw_patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        // The only triggers `count_tested_lines` must visit per line; recomputed here, the
        // dirty-gated rebuild point, so it tracks the trigger `Vec` without per-mutation upkeep.
        self.rebuild_line_limited_triggers();

        debug!("Time to rebuild trigger regex sets: {:?}", start.elapsed());
    }

    /// Recompute [`line_limited_triggers`](Self::line_limited_triggers) from the current trigger
    /// `Vec`: the indices whose `line_limit` is set. See the field docs for why this is a side
    /// list rather than a per-line scan.
    fn rebuild_line_limited_triggers(&mut self) {
        self.line_limited_triggers = self
            .triggers
            .iter()
            .enumerate()
            .filter(|(_, trigger)| trigger.line_limit.is_some())
            .map(|(i, _)| i)
            .collect();
    }

    fn rebuild_alias_regex_set(&mut self) {
        let mut priority_order: Vec<usize> = (0..self.aliases.len()).collect();
        priority_order.sort_by(|&a, &b| self.aliases[b].priority.cmp(&self.aliases[a].priority));

        self.alias_regex_set = PatternSet::build(priority_order.iter().flat_map(|&i| {
            self.aliases[i]
                .patterns
                .iter()
                .map(|pattern| pattern.as_str())
        }))
        .unwrap();
        self.alias_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let alias = &self.aliases[i];
                let mut v = Vec::with_capacity(alias.patterns.len());
                for _ in 0..alias.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.alias_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.aliases[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();
    }

    /// Match one subject string against one `PatternSet` tier and queue the matched
    /// automations' actions.
    ///
    /// `fired` carries the indices (into `triggers`) that have already queued a
    /// `RunAutomation` for the current line. The incoming-line paths share one list across
    /// their raw and normal passes, so an automation matching in both fires **once per
    /// line** — raw first, which is the documented precedence — rather than once per pass.
    /// Each automation queued here is recorded into the list. `matches` is the caller's
    /// scratch for the pattern-set hits; it is overwritten here.
    #[allow(clippy::too_many_arguments)]
    fn process_line_inner(
        &self,
        line: &str,
        styled_line: Option<&Arc<StyledLine>>,
        depth: u32,
        sender: Option<&AliasSender>,
        pattern_set: &PatternSet,
        triggers: &[Trigger],
        regex_set_to_triggers_map: &[usize],
        regex_set_to_patterns_map: &[usize],
        match_type: TriggerMatchType,
        is_captured: Option<Arc<AtomicBool>>,
        fallthrough_scopes: &mut FallthroughScopes,
        fired: &mut Vec<usize>,
        matches: &mut Vec<PatternMatch>,
    ) -> Result<()> {
        if depth > 100 {
            match sender {
                Some(sender) => bail!(
                    "Script processor bailing, depth limit reached while expanding alias \"{}\". Does it trigger itself?",
                    sender.name
                ),
                None => bail!(
                    "Script processor bailing, depth limit reached. Do you have an alias that triggers itself?"
                ),
            }
        }
        // Time the match only when debug logging is compiled in: `log_enabled!(Debug)`
        // const-folds to `false` under `release_max_level_info` (release/bench), so the timer is
        // a dead `None` and the whole block — both clock reads — is optimized away.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);
        pattern_set.matches_into(line, matches);
        if let Some(start) = timer {
            debug!("Time to test pattern matches: {:?}", start.elapsed());
        }

        if !matches.is_empty() {
            for match_indices in matches.chunk_by(|a, b| {
                regex_set_to_triggers_map.get(a.index).unwrap()
                    == regex_set_to_triggers_map.get(b.index).unwrap()
            }) {
                let first_match_idx = match_indices[0].index;
                let trigger_idx = *regex_set_to_triggers_map.get(first_match_idx).unwrap();
                let trigger = triggers.get(trigger_idx).unwrap();

                if !trigger.enabled
                    || fired.contains(&trigger_idx)
                    || trigger.anti_matches(line, styled_line.map(Arc::as_ref), self.bold_is_bright)
                {
                    continue;
                }

                // Preserve the fast path for unfiltered triggers. It reads only
                // the first matching pattern. A candidate with a color
                // filter can inspect later regex matches and their styled spans.
                let qualified_match = if matches!(match_type, TriggerMatchType::Raw)
                    || trigger.pattern_colors.is_empty()
                {
                    regex_set_to_patterns_map
                        .get(first_match_idx)
                        .copied()
                        .map(|pattern_idx| (&match_indices[0], pattern_idx, None))
                } else {
                    match_indices.iter().find_map(|hit| {
                        let match_idx = hit.index;
                        let pattern_idx = *regex_set_to_patterns_map.get(match_idx)?;
                        trigger
                            .pattern_color_match_start(
                                line,
                                styled_line.map(Arc::as_ref),
                                pattern_idx,
                                self.bold_is_bright,
                            )
                            .map(|qualification| match qualification {
                                ColorQualification::Unfiltered => (hit, pattern_idx, None),
                                ColorQualification::Matched(start) => {
                                    (hit, pattern_idx, Some(start))
                                }
                            })
                    })
                };
                let Some((hit, pattern_idx, match_start)) = qualified_match else {
                    continue;
                };

                // An alias's own sent text does not re-match it unless it opts
                // in: the direct self-recursion loop never starts, instead of
                // spinning until the depth limit.
                if trigger.is_alias
                    && !trigger.allow_self_match
                    && sender.is_some_and(|sender| {
                        sender.isolate == trigger.isolate
                            && sender.origin == trigger.origin
                            && *sender.name == trigger.name
                    })
                {
                    continue;
                }

                debug!(
                    "Trigger matched: {:?}, /{}/",
                    trigger.name(),
                    pattern_set.patterns().get(hit.index).unwrap()
                );

                let stopped = fallthrough_scopes.scope(trigger.identity().namespace);
                trigger.run(
                    line,
                    styled_line,
                    match_type,
                    pattern_idx,
                    match_start,
                    hit.literal.clone().filter(|_| match_start.is_none()),
                    &is_captured,
                    stopped,
                    &self.spawned_actions,
                    depth + 1,
                )?;
                fired.push(trigger_idx);
            }
        }
        Ok(())
    }

    /// Queue the auto-removal of a self-limited automation, routed by whether it is an alias or a
    /// trigger (the same split the dispatch handlers use). Best-effort: the action lands at the
    /// back of the spawned-action queue and the `Manager` applies it on its own thread.
    fn queue_self_removal(&self, item: &Trigger) {
        let action = if item.is_alias {
            RuntimeAction::RemoveAlias(
                item.isolate.clone(),
                item.origin.clone(),
                Arc::new(item.name.clone()),
            )
        } else {
            RuntimeAction::RemoveTrigger(
                item.isolate.clone(),
                item.origin.clone(),
                Arc::new(item.name.clone()),
            )
        };
        self.spawned_actions.borrow_mut().push_back(action);
    }

    /// Bump `lines_tested` on every enabled trigger that declares a `lineLimit`, queueing each
    /// one's removal as it reaches the limit. Trigger-only (called from the incoming-line paths).
    /// Iterates only [`line_limited_triggers`](Self::line_limited_triggers), so unlimited
    /// triggers — the common case — cost nothing per line rather than an O(trigger-count) scan.
    /// Counts one tested line per incoming line regardless of how many tiers (raw/normal)
    /// evaluate it.
    fn count_tested_lines(&self) {
        for &idx in &self.line_limited_triggers {
            let trigger = &self.triggers[idx];
            // `line_limited_triggers` only holds `line_limit.is_some()` indices; the self-limit
            // arithmetic still needs the concrete bound.
            let Some(limit) = trigger.line_limit else {
                continue;
            };
            if !trigger.enabled {
                continue;
            }
            let tested = trigger.lines_tested.get() + 1;
            trigger.lines_tested.set(tested);
            if tested >= limit && trigger.fire_limit.is_none_or(|fl| trigger.fires.get() < fl) {
                self.queue_self_removal(trigger);
            }
        }
    }

    /// Match one outgoing line against the alias set. `sender` names the alias whose
    /// expansion produced the line, when one did (typed input passes `None`), and `depth`
    /// counts nested expansions toward the loop-bail limit.
    pub fn process_outgoing_line(
        &mut self,
        line: &str,
        depth: u32,
        sender: Option<&AliasSender>,
    ) -> Result<()> {
        // Lazily rebuild the alias PatternSet here (mirrors how
        // `process_incoming_line` rebuilds the trigger set) so alias inserts at
        // load time stay O(1) and we pay one rebuild on the first command.
        if self.alias_regex_set_dirty {
            self.rebuild_alias_regex_set();
            self.alias_regex_set_dirty = false;
        }
        self.process_nested_outgoing_line(line, depth, sender)
    }

    pub fn process_nested_outgoing_line(
        &self,
        line: &str,
        depth: u32,
        sender: Option<&AliasSender>,
    ) -> Result<()> {
        let is_captured = Arc::new(AtomicBool::new(false));
        let mut fallthrough_scopes = FallthroughScopes::new();
        // Aliases evaluate in a single pass, so the fired list is inert here; it exists
        // for the two-pass incoming paths.
        let mut fired = Vec::new();
        let mut matches = Vec::new();

        self.process_line_inner(
            line,
            None,
            depth,
            sender,
            &self.alias_regex_set,
            &self.aliases,
            &self.alias_regex_set_map,
            &self.alias_regex_patterns_map,
            TriggerMatchType::Normal,
            Some(is_captured.clone()),
            &mut fallthrough_scopes,
            &mut fired,
            &mut matches,
        )?;

        self.spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::SendRawUnless(
                is_captured,
                Arc::new(line.to_string()),
            ));
        Ok(())
    }

    /// Execute a matched plaintext command template. This happens at dispatch time (rather than
    /// match-discovery time) so a prior automation can stop this invocation before it captures or
    /// sends anything. Each separated command begins its own alias frame; `sender` is the alias
    /// whose body is being expanded (`None` for a trigger body).
    pub(crate) fn run_simple_automation(
        &self,
        script: &str,
        captures: CaptureView<'_>,
        depth: u32,
        sender: Option<&AliasSender>,
    ) -> Result<()> {
        let evaluated = expand_template_view(script, captures);
        for line in split_commands(&evaluated, &self.command_separator) {
            self.process_nested_outgoing_line(line, depth, sender)?;
        }
        Ok(())
    }

    /// Count an invocation only after it actually begins running. A match skipped by an earlier
    /// `fallthrough(false)` therefore consumes neither `fireLimit` nor its one-shot lifetime.
    ///
    /// The counter is found through the identity's cached slot when the registry has not
    /// changed since it was cached; otherwise by the `(isolate, origin, name)` lookup, whose
    /// result is cached for the next fire.
    pub(crate) fn record_fire(&self, identity: &AutomationIdentity) {
        let items = if identity.is_alias {
            &self.aliases
        } else {
            &self.triggers
        };
        let index = if let Some(index) = identity.cached_slot(self.registry_generation) {
            index
        } else {
            let indices = if identity.is_alias {
                &self.alias_indices
            } else {
                &self.trigger_indices
            };
            let Some(&index) = indices
                .get(&(identity.isolate.clone(), identity.origin.clone()))
                .and_then(|namespace| namespace.get(identity.name.as_str()))
            else {
                return;
            };
            identity.cache_slot(self.registry_generation, index);
            index
        };
        let Some(item) = items.get(index) else {
            return;
        };

        let fires = item.fires.get() + 1;
        item.fires.set(fires);
        if item.fire_limit.is_some_and(|limit| fires >= limit) {
            self.queue_self_removal(item);
        }
    }

    /// Match `line` against the complete-line trigger sets, queuing the matched triggers'
    /// actions. Does **not** enqueue [`RuntimeAction::CompleteLineTriggersProcessed`] — the
    /// caller owns that, so it can splice a post-trigger `sys:receive` emit between the trigger
    /// cascade and the line's transform/route step (see the `HandleIncomingLine` dispatch arm).
    pub fn process_incoming_line(&mut self, line: &Arc<StyledLine>) -> Result<()> {
        trace!("Processing incoming line: {line:?}");
        if self.trigger_regex_set_dirty {
            self.rebuild_trigger_regex_set();
            self.trigger_regex_set_dirty = false;
        }

        // Zero-cost unless debug logging is compiled in; see `process_line_inner`.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);

        let mut fallthrough_scopes = FallthroughScopes::new();
        // Shared across the raw and normal passes (the same per-line lifetime as
        // `fallthrough_scopes`): a trigger matching in both fires once, on the raw pass.
        // The scratch is taken out for this line and returned below, before any handler runs.
        let mut scratch = std::mem::take(&mut self.line_scratch);
        scratch.fired.clear();
        let result = self.process_incoming_line_with(line, &mut fallthrough_scopes, &mut scratch);
        self.line_scratch = scratch;
        result?;

        // Self-limit: one tested-line tick per incoming complete line for every
        // `lineLimit` trigger (no-op for the common unlimited case).
        self.count_tested_lines();

        if let Some(start) = timer {
            debug!(
                "Time to match and dispatch triggers on incoming line: {:?}",
                start.elapsed()
            );
        }

        Ok(())
    }

    /// The raw-then-normal passes of [`Self::process_incoming_line`] over the caller's scratch.
    fn process_incoming_line_with(
        &self,
        line: &Arc<StyledLine>,
        fallthrough_scopes: &mut FallthroughScopes,
        scratch: &mut LineScratch,
    ) -> Result<()> {
        if let Some(raw) = line.raw() {
            debug!("Processing raw line: {raw:?}");
            self.process_line_inner(
                raw,
                Some(line),
                0,
                None,
                &self.raw_trigger_regex_set,
                &self.triggers,
                &self.raw_trigger_regex_set_map,
                &self.raw_trigger_regex_patterns_map,
                TriggerMatchType::Raw,
                None,
                fallthrough_scopes,
                &mut scratch.fired,
                &mut scratch.matches,
            )?;
        }

        self.process_line_inner(
            line,
            Some(line),
            0,
            None,
            &self.trigger_regex_set,
            &self.triggers,
            &self.trigger_regex_set_map,
            &self.trigger_regex_patterns_map,
            TriggerMatchType::Normal,
            None,
            fallthrough_scopes,
            &mut scratch.fired,
            &mut scratch.matches,
        )
    }

    pub fn process_partial_line(&mut self, line: Arc<StyledLine>) -> Result<()> {
        trace!("Processing incoming partial line: {line:?}");

        // A prompt can be the first server output after scripts register (or
        // reload). Do not require an unrelated complete line to rebuild the
        // prompt PatternSets first.
        if self.trigger_regex_set_dirty {
            self.rebuild_trigger_regex_set();
            self.trigger_regex_set_dirty = false;
        }

        // Zero-cost unless debug logging is compiled in; see `process_line_inner`.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);

        let mut fallthrough_scopes = FallthroughScopes::new();
        // The prompt path has the same two-pass shape as `process_incoming_line` and gets
        // the same one-fire-per-line treatment. Each call starts its list empty, so a partial
        // line and its later completed line count as different lines.
        let mut scratch = std::mem::take(&mut self.line_scratch);
        scratch.fired.clear();
        let result = self.process_partial_line_with(&line, &mut fallthrough_scopes, &mut scratch);
        self.line_scratch = scratch;
        result?;

        if let Some(start) = timer {
            debug!(
                "Time to match and dispatch triggers on incoming partial line: {:?}",
                start.elapsed()
            );
        }

        self.spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::PartialLineTriggersProcessed(line));
        Ok(())
    }
}

impl Manager {
    /// The raw-then-normal prompt passes of [`Self::process_partial_line`] over the caller's
    /// scratch.
    fn process_partial_line_with(
        &self,
        line: &Arc<StyledLine>,
        fallthrough_scopes: &mut FallthroughScopes,
        scratch: &mut LineScratch,
    ) -> Result<()> {
        if let Some(raw) = line.raw() {
            self.process_line_inner(
                raw,
                Some(line),
                0,
                None,
                &self.prompt_raw_trigger_regex_set,
                &self.triggers,
                &self.prompt_raw_trigger_regex_set_map,
                &self.prompt_raw_trigger_regex_patterns_map,
                TriggerMatchType::Raw,
                None,
                fallthrough_scopes,
                &mut scratch.fired,
                &mut scratch.matches,
            )?;
        }

        self.process_line_inner(
            line,
            Some(line),
            0,
            None,
            &self.prompt_trigger_regex_set,
            &self.triggers,
            &self.prompt_trigger_regex_set_map,
            &self.prompt_trigger_regex_patterns_map,
            TriggerMatchType::Normal,
            None,
            fallthrough_scopes,
            &mut scratch.fired,
            &mut scratch.matches,
        )
    }
}

#[derive(Debug)]
struct Trigger {
    /// The isolate this automation was registered in. Source of truth for both the
    /// `(IsolateId, Origin)` registry key and the isolate stamped into the v8-routed
    /// actions [`run`](Trigger::run) emits (its `ScriptId`/`FunctionId` index *this*
    /// isolate's registries).
    isolate: IsolateId,
    origin: Origin,
    name: String,
    /// The shared identity every queued action carries (see [`AutomationIdentity`]).
    /// Assigned by the `Manager` at registration; every entry in its registries has one.
    identity: Option<Arc<AutomationIdentity>>,
    patterns: Vec<CapturePattern>,
    pattern_colors: Vec<Option<CompiledColorMatch>>,
    raw_patterns: Vec<CapturePattern>,
    anti_patterns: RegexSet,
    colored_anti_pattern_set: Option<RegexSet>,
    colored_anti_patterns: Vec<(Regex, CompiledColorMatch)>,
    script: ScriptAction,
    prompt: bool,
    enabled: bool,
    /// Higher values are evaluated first; equal values retain registration order.
    priority: i32,
    /// Whether later matches in this automation's creator scope may run.
    fallthrough: bool,
    /// Alias-only: whether text this alias sends may match this same alias again. `false`
    /// excludes the alias from matching its own expansion output (see
    /// [`Manager::process_line_inner`]); inert for triggers.
    allow_self_match: bool,
    /// Whether this entry lives in the alias `Vec` (matched on outgoing input) vs the trigger
    /// `Vec` (matched on incoming lines). Drives the `RemoveAlias`/`RemoveTrigger` self-limit
    /// removal kind. `Trigger` is reused for both by construction; this is the discriminant.
    is_alias: bool,
    /// Self-limit: auto-remove after this many fires. `None` ⇒ unbounded; `Some(1)` ⇒
    /// one-shot.
    fire_limit: Option<u32>,
    /// Self-limit (trigger-only): auto-remove after this many tested lines. Aliases match
    /// input rather than server lines, so this is always `None` for them.
    line_limit: Option<u32>,
    /// Times this automation has fired. `Cell` so the matcher can bump it through the `&self`
    /// processing path without a `&mut Manager`.
    fires: Cell<u32>,
    /// Times this trigger has been evaluated against an incoming line (only tracked when
    /// `line_limit` is set, to avoid per-line cost for the common unlimited case).
    lines_tested: Cell<u32>,
    /// Display-only body source for the automations window's read-only detail pane: the
    /// JS/TS eval string, or a function's `toString()` passed in good faith from JS-land.
    /// `None` for plaintext bodies (recoverable from `script`) or when none was supplied.
    /// Never executed — purely what the UI renders.
    source: Option<Arc<str>>,
    /// A Command alias's argument parser (alias-only, from the editor's sidecar). When
    /// set, the stored regex is only a prefilter: [`Trigger::run`] hands the line to the
    /// parser, which decides firing and produces the captures.
    command: Option<crate::models::matchers::CommandSpec>,
}

impl Trigger {
    fn anti_matches(
        &self,
        subject: &str,
        styled_line: Option<&StyledLine>,
        bold_is_bright: bool,
    ) -> bool {
        // Most automations declare no anti-patterns; an empty set still costs a search
        // dispatch, so answer it here.
        if !self.anti_patterns.is_empty() && self.anti_patterns.is_match(subject) {
            return true;
        }
        let Some(colored_anti_pattern_set) = &self.colored_anti_pattern_set else {
            return false;
        };
        let Some(styled_line) = styled_line else {
            return false;
        };
        // Keep the common text-miss path allocation-free. `matches()` builds
        // a candidate bitset, so pay for it only after the cheaper boolean
        // prefilter says at least one colored anti regex matched.
        if !colored_anti_pattern_set.is_match(&styled_line.text) {
            return false;
        }
        colored_anti_pattern_set
            .matches(&styled_line.text)
            .iter()
            .any(|index| {
                let (regex, color) = &self.colored_anti_patterns[index];
                color_matched_start(
                    regex,
                    &styled_line.text,
                    styled_line,
                    *color,
                    bold_is_bright,
                )
                .is_some()
            })
    }

    /// Returns `Some(Unfiltered)` for an unfiltered candidate. Returns
    /// `Some(Matched(start))` for the first text match that satisfies the color
    /// filter. Returns `None` when the filter rejects the candidate.
    fn pattern_color_match_start(
        &self,
        subject: &str,
        styled_line: Option<&StyledLine>,
        pattern_index: usize,
        bold_is_bright: bool,
    ) -> Option<ColorQualification> {
        let Some(color) = self
            .pattern_colors
            .get(pattern_index)
            .and_then(Option::as_ref)
        else {
            return Some(ColorQualification::Unfiltered);
        };
        let styled_line = styled_line?;
        self.patterns
            .get(pattern_index)
            .and_then(|regex| {
                color_matched_start(regex, subject, styled_line, *color, bold_is_bright)
            })
            .map(ColorQualification::Matched)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new<
        TIterPattern,
        TIterRawPattern,
        TIterAntiPattern,
        TPatternStr,
        TRawPatternStr,
        TAntiPatternStr,
    >(
        isolate: IsolateId,
        origin: Origin,
        name: String,
        patterns: TIterPattern,
        raw_patterns: TIterRawPattern,
        anti_patterns: TIterAntiPattern,
        matchers: Option<&[TriggerMatcherSource]>,
        script: ScriptAction,
        prompt: bool,
        enabled: bool,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
    ) -> Result<Self>
    where
        TPatternStr: AsRef<str>,
        TRawPatternStr: AsRef<str>,
        TAntiPatternStr: AsRef<str>,
        TIterPattern: Iterator<Item = TPatternStr>,
        TIterRawPattern: Iterator<Item = TRawPatternStr>,
        TIterAntiPattern: Iterator<Item = TAntiPatternStr>,
    {
        let pattern_sources: Vec<_> = patterns.map(|p| p.as_ref().to_string()).collect();
        let raw_pattern_sources: Vec<_> = raw_patterns.map(|p| p.as_ref().to_string()).collect();
        let anti_pattern_sources: Vec<_> = anti_patterns.map(|p| p.as_ref().to_string()).collect();

        let fresh_colors = matchers.and_then(|matchers| {
            let derived = crate::models::matchers::trigger_patterns(matchers).ok()?;
            let same = derived.patterns == pattern_sources
                && derived.raw_patterns == raw_pattern_sources
                && derived.anti_patterns == anti_pattern_sources;
            same.then_some(matchers)
        });
        let source_pattern_colors = fresh_colors.map_or_else(
            || vec![None; pattern_sources.len()],
            |matchers| {
                matchers
                    .iter()
                    .filter(|matcher| matcher.role == MatcherRole::Match)
                    .map(|matcher| matcher.color.clone())
                    .collect()
            },
        );
        let anti_colors: Vec<_> = fresh_colors.map_or_else(
            || vec![None; anti_pattern_sources.len()],
            |matchers| {
                matchers
                    .iter()
                    .filter(|matcher| matcher.role == MatcherRole::Anti)
                    .map(|matcher| matcher.color.clone())
                    .collect()
            },
        );

        let pattern_rows = pattern_sources
            .into_iter()
            .zip(source_pattern_colors)
            .map(|(source, style)| ScriptTriggerPattern { source, style })
            .collect();
        let anti_rows = anti_pattern_sources
            .into_iter()
            .zip(anti_colors)
            .map(|(source, style)| ScriptTriggerPattern { source, style })
            .collect();
        let prepared =
            PreparedScriptTriggerPatterns::compile(pattern_rows, raw_pattern_sources, anti_rows)?;

        Ok(Self::from_prepared(
            isolate,
            origin,
            name,
            prepared,
            script,
            prompt,
            enabled,
            priority,
            fallthrough,
            fire_limit,
            line_limit,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn from_prepared(
        isolate: IsolateId,
        origin: Origin,
        name: String,
        prepared: PreparedScriptTriggerPatterns,
        script: ScriptAction,
        prompt: bool,
        enabled: bool,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
    ) -> Self {
        let PreparedScriptTriggerPatterns {
            patterns,
            pattern_colors,
            raw_patterns,
            anti_patterns,
            colored_anti_pattern_set,
            colored_anti_patterns,
        } = prepared;

        let patterns = patterns.into_iter().map(CapturePattern::new).collect();
        let raw_patterns = raw_patterns.into_iter().map(CapturePattern::new).collect();
        Self {
            identity: None,
            isolate,
            origin,
            name,
            patterns,
            pattern_colors,
            raw_patterns,
            anti_patterns,
            colored_anti_pattern_set,
            colored_anti_patterns,
            script,
            prompt,
            enabled,
            priority,
            fallthrough,
            allow_self_match: false,
            is_alias: false,
            fire_limit,
            line_limit,
            fires: Cell::new(0),
            lines_tested: Cell::new(0),
            source: None,
            command: None,
        }
    }

    pub fn new_alias<TIterPattern, TPatternStr>(
        isolate: IsolateId,
        origin: Origin,
        name: String,
        patterns: TIterPattern,
        script: ScriptAction,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
    ) -> Result<Self>
    where
        TPatternStr: AsRef<str>,
        TIterPattern: Iterator<Item = TPatternStr>,
    {
        let mut alias = Self::new(
            isolate,
            origin,
            name,
            patterns,
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
            None,
            script,
            false,
            true,
            priority,
            fallthrough,
            fire_limit,
            // Aliases match input, not server lines, so `lineLimit` is ignored for them.
            None,
        )?;
        alias.is_alias = true;
        Ok(alias)
    }

    /// Attaches the display-only body source (see [`Trigger::source`]). Chained off `new`/
    /// `new_alias` at the push sites that have it.
    #[must_use]
    fn with_source(mut self, source: Option<Arc<str>>) -> Self {
        self.source = source;
        self
    }

    /// Attaches a Command alias's argument parser (see [`Trigger::command`]).
    #[must_use]
    fn with_command(mut self, command: Option<crate::models::matchers::CommandSpec>) -> Self {
        self.command = command;
        self
    }

    /// Sets whether this alias's own sent text may match it again (see
    /// [`Trigger::allow_self_match`]).
    #[must_use]
    fn with_allow_self_match(mut self, allow_self_match: bool) -> Self {
        self.allow_self_match = allow_self_match;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        line: &str,
        styled_line: Option<&Arc<StyledLine>>,
        match_type: TriggerMatchType,
        pattern_idx: usize,
        match_start: Option<usize>,
        literal_range: Option<std::ops::Range<usize>>,
        is_captured: &Option<Arc<AtomicBool>>,
        stopped: Arc<AtomicBool>,
        spawned_actions: &ActionQueue,
        depth: u32,
    ) -> Result<()> {
        // A Command alias's stored regex is only a prefilter: the parser decides
        // whether it actually fires, and produces the captures when it does.
        if let Some(command) = &self.command {
            return self.run_command(command, line, is_captured, stopped, spawned_actions, depth);
        }

        let pattern = match match_type {
            TriggerMatchType::Normal => self.patterns.get(pattern_idx).unwrap(),
            TriggerMatchType::Raw => self.raw_patterns.get(pattern_idx).unwrap(),
        };
        let captures = if let Some(styled_line) = styled_line {
            pattern.capture_line(
                styled_line,
                matches!(match_type, TriggerMatchType::Raw),
                match_start,
                literal_range,
            )
        } else {
            // Ordered captures: position is the group number (index 0 = whole match), `name` set
            // only for named groups. The list is shared by the JS handlers (numeric/named
            // `matches` object) and the inline `SendSimple` template expansion.
            let captures = match_start.map_or_else(
                || pattern.captures(line),
                |start| {
                    pattern.captures_at(line, start).filter(|captures| {
                        captures.get(0).is_some_and(|whole| whole.start() == start)
                    })
                },
            );
            let captures: Arc<Vec<MatchCapture>> = Arc::new(
                pattern
                    .capture_names()
                    .zip(
                        captures
                            .expect("a selected trigger match must still capture")
                            .iter(),
                    )
                    .map(|(name, value)| MatchCapture {
                        name: name.map(|n| std::borrow::Cow::Owned(n.to_string())),
                        value: value.map_or_else(String::new, |m| m.as_str().to_string()),
                    })
                    .collect(),
            );

            CapturePayload::Owned(captures)
        };

        spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::RunAutomation {
                identity: self.identity().clone(),
                script: self.script.clone(),
                matches: captures,
                depth,
                is_captured: is_captured.clone(),
                stopped,
                fallthrough: self.fallthrough,
            });
        Ok(())
    }

    /// The Command path of [`Trigger::run`]: tokenize and assign per the spec.
    /// `Fired` queues the automation with the parser's captures (index 0 is the
    /// trimmed input, then one capture per argument in declaration order — an
    /// absent optional is empty, the same convention as a non-participating
    /// regex group). A missing required argument queues the usage echo (D10).
    /// Every other miss queues nothing, which leaves `is_captured` unset so
    /// `SendRawUnless` sends the line the user actually typed.
    fn run_command(
        &self,
        command: &crate::models::matchers::CommandSpec,
        line: &str,
        is_captured: &Option<Arc<AtomicBool>>,
        stopped: Arc<AtomicBool>,
        spawned_actions: &ActionQueue,
        depth: u32,
    ) -> Result<()> {
        use crate::models::matchers::{CommandMiss, CommandOutcome, assign, usage_line};

        match assign(line, &command.name, &command.args, command.parse) {
            CommandOutcome::Fired { args } => {
                let mut captures = Vec::with_capacity(args.len() + 1);
                captures.push(MatchCapture {
                    name: None,
                    value: line.trim().to_string(),
                });
                for (name, value) in args {
                    captures.push(MatchCapture {
                        name: Some(std::borrow::Cow::Owned(name)),
                        value: value.unwrap_or_default(),
                    });
                }
                spawned_actions
                    .borrow_mut()
                    .push_back(RuntimeAction::RunAutomation {
                        identity: self.identity().clone(),
                        script: self.script.clone(),
                        matches: CapturePayload::Owned(Arc::new(captures)),
                        depth,
                        is_captured: is_captured.clone(),
                        stopped,
                        fallthrough: self.fallthrough,
                    });
            }
            CommandOutcome::NotFired(CommandMiss::MissingRequired { .. }) => {
                spawned_actions
                    .borrow_mut()
                    .push_back(RuntimeAction::EchoUsage {
                        text: Arc::new(format!(
                            "Usage: {}",
                            usage_line(&command.name, &command.args)
                        )),
                        is_captured: is_captured.clone(),
                        stopped,
                        fallthrough: self.fallthrough,
                    });
            }
            CommandOutcome::NotFired(_) => {}
        }
        Ok(())
    }

    /// The shared identity assigned at registration. Only registered entries are matched, so
    /// a missing identity is a registration-path bug.
    fn identity(&self) -> &Arc<AutomationIdentity> {
        self.identity
            .as_ref()
            .expect("an automation in the registry has its identity assigned")
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fire_on_prompts(&self) -> bool {
        self.prompt
    }
}

#[cfg(test)]
mod tests {
    use super::{MatchCapture, expand_template, split_commands};

    mod color_matching {
        use std::rc::Rc;
        use std::sync::Arc;

        use regex::Regex;

        use super::super::{
            CompiledColorMatch, CompiledMatcherColor, Manager, PreparedScriptTriggerPatterns,
            PushTriggerParams, ScriptAction, ScriptTriggerPattern, Trigger,
            color_matched_start as compiled_color_matched_start, effective_foreground,
        };
        use crate::models::matchers::{
            MatcherColor, MatcherColorMatch, MatcherHsv, MatcherHsvRange, MatcherRole,
            MatcherSyntax, MatcherTextAttribute, TriggerMatcherSource,
        };
        use crate::session::connection::vt_processor::{
            AnsiColor, VtProcessor, parse_ansi_fragment,
        };
        use crate::session::runtime::origin::{IsolateId, Origin};
        use crate::session::runtime::{ActionQueue, RuntimeAction};
        use crate::session::styled_line::{Color, Style, StyledLine, TextAttributes, VtSpan};
        use tokio::sync::mpsc::unbounded_channel;
        use vtparse::VTParser;

        const RED: Color = Color::Ansi {
            color: AnsiColor::Red,
            bold: false,
        };
        const BLUE: Color = Color::Ansi {
            color: AnsiColor::Blue,
            bold: false,
        };
        const CYAN: Color = Color::Ansi {
            color: AnsiColor::Cyan,
            bold: false,
        };
        const BRIGHT_CYAN: Color = Color::Ansi {
            color: AnsiColor::Cyan,
            bold: true,
        };

        fn color_matched_start(
            regex: &Regex,
            subject: &str,
            line: &StyledLine,
            matcher: &MatcherColorMatch,
            bold_is_bright: bool,
        ) -> Option<usize> {
            compiled_color_matched_start(
                regex,
                subject,
                line,
                CompiledColorMatch::compile(matcher),
                bold_is_bright,
            )
        }

        fn line() -> StyledLine {
            StyledLine::new(
                "red red",
                vec![
                    VtSpan {
                        style: Style::default(),
                        begin_pos: 0,
                        end_pos: 4,
                    },
                    VtSpan {
                        style: Style {
                            fg: RED,
                            bg: BLUE,
                            attributes: TextAttributes {
                                bold: true,
                                ..TextAttributes::default()
                            },
                        },
                        begin_pos: 4,
                        end_pos: 7,
                    },
                ],
            )
        }

        fn red_bold_on_blue() -> MatcherColorMatch {
            MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 1 }),
                background: Some(MatcherColor::Ansi { index: 4 }),
                attributes: vec![MatcherTextAttribute::Bold],
            }
        }

        #[test]
        fn iterates_matches_until_a_start_style_fits() {
            let line = line();
            assert_eq!(
                color_matched_start(
                    &Regex::new("red").unwrap(),
                    &line.text,
                    &line,
                    &red_bold_on_blue(),
                    false,
                ),
                Some(4),
            );
            assert!(
                color_matched_start(
                    &Regex::new("^red").unwrap(),
                    &line.text,
                    &line,
                    &red_bold_on_blue(),
                    false,
                )
                .is_none()
            );
        }

        #[test]
        fn empty_pattern_becomes_a_color_only_scan() {
            let line = line();
            assert_eq!(
                color_matched_start(
                    &Regex::new("").unwrap(),
                    &line.text,
                    &line,
                    &red_bold_on_blue(),
                    false,
                ),
                Some(4),
            );
        }

        #[test]
        fn color_only_scan_ignores_zero_width_vt_style_transitions() {
            let bold = TextAttributes {
                bold: true,
                ..TextAttributes::default()
            };
            let line = StyledLine::new(
                "cyan",
                vec![
                    // A live VtProcessor emits these spans when the cursor
                    // inherits dim cyan and receives SGR 1 before text.
                    VtSpan {
                        style: Style {
                            fg: CYAN,
                            ..Style::default()
                        },
                        begin_pos: 0,
                        end_pos: 0,
                    },
                    VtSpan {
                        style: Style {
                            fg: CYAN,
                            attributes: bold,
                            ..Style::default()
                        },
                        begin_pos: 0,
                        end_pos: 4,
                    },
                ],
            );
            let dim = MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 6 }),
                ..Default::default()
            };
            let bright = MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 14 }),
                ..Default::default()
            };
            let empty = Regex::new("").unwrap();

            assert!(color_matched_start(&empty, &line.text, &line, &dim, true).is_none());
            assert_eq!(
                color_matched_start(&empty, &line.text, &line, &bright, true),
                Some(0),
            );

            let empty_line = StyledLine::new(
                "",
                vec![
                    VtSpan {
                        style: Style {
                            fg: CYAN,
                            ..Style::default()
                        },
                        begin_pos: 0,
                        end_pos: 0,
                    },
                    VtSpan {
                        style: Style {
                            fg: CYAN,
                            attributes: bold,
                            ..Style::default()
                        },
                        begin_pos: 0,
                        end_pos: 0,
                    },
                ],
            );
            assert!(
                color_matched_start(&empty, &empty_line.text, &empty_line, &dim, true).is_none()
            );
            assert_eq!(
                color_matched_start(&empty, &empty_line.text, &empty_line, &bright, true),
                Some(0),
            );
        }

        #[test]
        fn any_channel_does_not_constrain_that_channel() {
            let line = line();
            let filter = MatcherColorMatch {
                foreground: None,
                background: Some(MatcherColor::Ansi { index: 4 }),
                attributes: Vec::new(),
            };
            assert!(
                color_matched_start(
                    &Regex::new("red").unwrap(),
                    &line.text,
                    &line,
                    &filter,
                    false,
                )
                .is_some()
            );
        }

        #[test]
        fn xterm_and_truecolor_compare_by_terminal_rgb() {
            let regex = Regex::new("target").unwrap();
            let rgb = Color::Rgb {
                r: 95,
                g: 135,
                b: 175,
            };
            let line = StyledLine::new(
                "target",
                vec![VtSpan {
                    style: Style {
                        fg: rgb,
                        ..Style::default()
                    },
                    begin_pos: 0,
                    end_pos: 6,
                }],
            );
            for foreground in [
                MatcherColor::Xterm { index: 67 },
                MatcherColor::Truecolor {
                    r: 95,
                    g: 135,
                    b: 175,
                    range: None,
                },
            ] {
                let filter = MatcherColorMatch {
                    foreground: Some(foreground),
                    ..Default::default()
                };
                assert_eq!(
                    color_matched_start(&regex, &line.text, &line, &filter, false,),
                    Some(0),
                );
            }
        }

        #[test]
        fn truecolor_range_requires_all_hsv_components_inside_the_box() {
            let range = MatcherHsvRange {
                first: MatcherHsv {
                    hue: 100,
                    saturation: 100,
                    value: 100,
                },
                second: MatcherHsv {
                    hue: 140,
                    saturation: 200,
                    value: 200,
                },
                wrap_hue: false,
            };
            let matcher = CompiledMatcherColor::compile(MatcherColor::Truecolor {
                r: 0,
                g: 0,
                b: 0,
                range: Some(range),
            });
            let rgb = |hsv: MatcherHsv| {
                let (r, g, b) = hsv.to_rgb();
                Color::Rgb { r, g, b }
            };

            assert!(matcher.matches(rgb(MatcherHsv {
                hue: 120,
                saturation: 150,
                value: 150,
            })));
            assert!(!matcher.matches(rgb(MatcherHsv {
                hue: 160,
                saturation: 150,
                value: 150,
            })));
            assert!(!matcher.matches(rgb(MatcherHsv {
                hue: 120,
                saturation: 220,
                value: 150,
            })));
            assert!(!matcher.matches(rgb(MatcherHsv {
                hue: 120,
                saturation: 150,
                value: 220,
            })));
            assert!(!matcher.matches(CYAN));

            let gray = MatcherHsv {
                hue: 217,
                saturation: 0,
                value: 128,
            };
            let gray_matcher = CompiledMatcherColor::compile(MatcherColor::Truecolor {
                r: 128,
                g: 128,
                b: 128,
                range: Some(MatcherHsvRange {
                    first: gray,
                    second: gray,
                    wrap_hue: false,
                }),
            });
            assert!(gray_matcher.matches(Color::Rgb {
                r: 128,
                g: 128,
                b: 128,
            }));

            let wrapped = CompiledMatcherColor::compile(MatcherColor::Truecolor {
                r: 255,
                g: 0,
                b: 42,
                range: Some(MatcherHsvRange {
                    first: MatcherHsv {
                        hue: 350,
                        saturation: 255,
                        value: 255,
                    },
                    second: MatcherHsv {
                        hue: 10,
                        saturation: 255,
                        value: 255,
                    },
                    wrap_hue: true,
                }),
            });
            assert!(wrapped.matches(rgb(MatcherHsv {
                hue: 355,
                saturation: 255,
                value: 255,
            })));
            assert!(wrapped.matches(rgb(MatcherHsv {
                hue: 5,
                saturation: 255,
                value: 255,
            })));
            assert!(!wrapped.matches(rgb(MatcherHsv {
                hue: 180,
                saturation: 255,
                value: 255,
            })));
        }

        #[test]
        fn compiled_hue_range_supports_broad_and_equal_directed_arcs() {
            let endpoint = |hue| MatcherHsv {
                hue,
                saturation: 255,
                value: 255,
            };
            let compile = |from, to| {
                CompiledMatcherColor::compile(MatcherColor::Truecolor {
                    r: 0,
                    g: 0,
                    b: 0,
                    range: Some(MatcherHsvRange::from_to(from, to)),
                })
            };
            let rgb = |hsv: MatcherHsv| {
                let (r, g, b) = hsv.to_rgb();
                Color::Rgb { r, g, b }
            };

            let broad = compile(endpoint(10), endpoint(350));
            assert!(broad.matches(rgb(endpoint(10))));
            assert!(broad.matches(rgb(endpoint(180))));
            assert!(broad.matches(rgb(endpoint(350))));
            assert!(!broad.matches(rgb(endpoint(5))));
            assert!(!broad.matches(rgb(endpoint(355))));

            let equal = compile(endpoint(45), endpoint(45));
            assert!(equal.matches(rgb(endpoint(45))));
            assert!(!equal.matches(rgb(endpoint(44))));
            assert!(!equal.matches(rgb(endpoint(46))));
        }

        #[test]
        fn compiled_hue_range_preserves_direction_when_quantization_crosses_zero() {
            let from = MatcherHsv {
                hue: 359,
                saturation: 10,
                value: 30,
            };
            let to = MatcherHsv {
                hue: 1,
                saturation: 255,
                value: 255,
            };
            let matcher = CompiledMatcherColor::compile(MatcherColor::Truecolor {
                r: 0,
                g: 0,
                b: 0,
                range: Some(MatcherHsvRange::from_to(from, to)),
            });
            let CompiledMatcherColor::HsvRange(compiled) = matcher else {
                panic!("expected a compiled HSV range");
            };
            assert_eq!((compiled.hue_from, compiled.hue_to), (0, 1));

            let rgb = |hsv: MatcherHsv| {
                let (r, g, b) = hsv.to_rgb();
                Color::Rgb { r, g, b }
            };
            assert!(matcher.matches(rgb(from)));
            assert!(matcher.matches(rgb(to)));
            assert!(!matcher.matches(rgb(MatcherHsv {
                hue: 180,
                saturation: 255,
                value: 255,
            })));
        }

        #[test]
        fn bold_brightness_promotes_only_normal_ansi_foregrounds() {
            let bold = TextAttributes {
                bold: true,
                ..TextAttributes::default()
            };
            assert_eq!(
                effective_foreground(
                    Style {
                        fg: CYAN,
                        bg: CYAN,
                        attributes: bold,
                    },
                    true,
                ),
                BRIGHT_CYAN,
            );
            assert_eq!(
                effective_foreground(
                    Style {
                        fg: CYAN,
                        attributes: bold,
                        ..Style::default()
                    },
                    false,
                ),
                CYAN,
            );
            assert_eq!(
                effective_foreground(
                    Style {
                        fg: BRIGHT_CYAN,
                        ..Style::default()
                    },
                    true,
                ),
                BRIGHT_CYAN,
            );
            assert_eq!(
                effective_foreground(
                    Style {
                        fg: Color::DefaultForeground { bold: false },
                        attributes: bold,
                        ..Style::default()
                    },
                    true,
                ),
                Color::DefaultForeground { bold: true },
            );
            let rgb = Color::Rgb { r: 1, g: 2, b: 3 };
            assert_eq!(
                effective_foreground(
                    Style {
                        fg: rgb,
                        attributes: bold,
                        ..Style::default()
                    },
                    true,
                ),
                rgb,
            );
        }

        #[test]
        fn bold_is_bright_distinguishes_effective_dim_and_bright_cyan() {
            let line = parse_ansi_fragment("\u{1b}[1;36;46mcyan");
            let regex = Regex::new("cyan").unwrap();
            let dim = MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 6 }),
                ..Default::default()
            };
            let bright = MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 14 }),
                ..Default::default()
            };
            let bright_and_bold = MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 14 }),
                attributes: vec![MatcherTextAttribute::Bold],
                ..Default::default()
            };
            let bright_background = MatcherColorMatch {
                background: Some(MatcherColor::Ansi { index: 8 + 6 }),
                ..Default::default()
            };
            let dim_background = MatcherColorMatch {
                background: Some(MatcherColor::Ansi { index: 6 }),
                ..Default::default()
            };

            assert_eq!(
                color_matched_start(&regex, &line.text, &line, &dim, false),
                Some(0),
            );
            assert!(color_matched_start(&regex, &line.text, &line, &bright, false).is_none());
            assert!(color_matched_start(&regex, &line.text, &line, &dim, true).is_none());
            assert_eq!(
                color_matched_start(&regex, &line.text, &line, &bright, true),
                Some(0),
            );
            assert_eq!(
                color_matched_start(&regex, &line.text, &line, &bright_and_bold, true),
                Some(0),
            );
            assert!(
                color_matched_start(&regex, &line.text, &line, &bright_background, true).is_none()
            );
            assert_eq!(
                color_matched_start(&regex, &line.text, &line, &dim_background, true),
                Some(0),
            );
        }

        #[test]
        fn manager_uses_live_bold_brightness_policy() {
            let queue: ActionQueue = Rc::default();
            let mut manager = Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
            let name = Arc::new("bright cyan".to_string());
            let patterns = Arc::new(vec!["cyan".to_string()]);
            let empty = Arc::new(Vec::<String>::new());
            let matchers = [TriggerMatcherSource {
                role: MatcherRole::Match,
                syntax: MatcherSyntax::Regex,
                source: "cyan".to_string(),
                anchor_start: true,
                anchor_end: true,
                color: Some(MatcherColorMatch {
                    foreground: Some(MatcherColor::Ansi { index: 14 }),
                    ..Default::default()
                }),
            }];
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &name,
                    patterns: &patterns,
                    raw_patterns: &empty,
                    anti_patterns: &empty,
                    matchers: Some(&matchers),
                    action: ScriptAction::Noop,
                    prompt: false,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
            let line = Arc::new(parse_ansi_fragment("\u{1b}[1;36mcyan"));

            manager.set_bold_is_bright(true);
            manager.process_incoming_line(&line).unwrap();
            assert!(
                queue
                    .borrow_mut()
                    .drain(..)
                    .any(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
            );

            manager.set_bold_is_bright(false);
            manager.process_incoming_line(&line).unwrap();
            assert!(
                !queue
                    .borrow_mut()
                    .drain(..)
                    .any(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
            );
        }

        #[test]
        fn manager_ansi_cyan_matrix_respects_bold_brightness_policy() {
            fn fires_line(manager: &mut Manager, queue: &ActionQueue, line: StyledLine) -> bool {
                let line = Arc::new(line);
                manager.process_incoming_line(&line).unwrap();
                queue
                    .borrow_mut()
                    .drain(..)
                    .any(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
            }

            fn fires(manager: &mut Manager, queue: &ActionQueue, sgr: &str) -> bool {
                fires_line(manager, queue, parse_ansi_fragment(sgr))
            }

            fn live_lines(bytes: &[u8]) -> Vec<Arc<StyledLine>> {
                let (tx, mut rx) = unbounded_channel();
                let mut processor = VtProcessor::new(tx);
                let mut parser = VTParser::new();
                for &byte in bytes {
                    if byte != b'\r' && byte != b'\n' {
                        processor.push_raw_incoming_byte(byte);
                    }
                    parser.parse_byte(byte, &mut processor);
                }
                let mut lines = Vec::new();
                while let Ok(action) = rx.try_recv() {
                    if let RuntimeAction::HandleIncomingLine(line) = action {
                        lines.push(line);
                    }
                }
                lines
            }

            fn manager_for(index: u8) -> (Manager, ActionQueue) {
                let queue: ActionQueue = Rc::default();
                let mut manager =
                    Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
                let name = Arc::new(format!("ansi {index}"));
                // Use the color-only form that the editor persists. Its text
                // pattern is empty. The color filter is its only condition.
                let patterns = Arc::new(vec![String::new()]);
                let empty = Arc::new(Vec::<String>::new());
                let matchers = [TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: String::new(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index }),
                        ..Default::default()
                    }),
                }];
                manager
                    .push_trigger(PushTriggerParams {
                        isolate: IsolateId::Main,
                        origin: Origin::User,
                        name: &name,
                        patterns: &patterns,
                        raw_patterns: &empty,
                        anti_patterns: &empty,
                        matchers: Some(&matchers),
                        action: ScriptAction::Noop,
                        prompt: false,
                        enabled: true,
                        priority: 0,
                        fallthrough: true,
                        fire_limit: None,
                        line_limit: None,
                        source: None,
                    })
                    .unwrap();
                (manager, queue)
            }

            let (mut dim, dim_queue) = manager_for(6);
            dim.set_bold_is_bright(false);
            assert!(fires(&mut dim, &dim_queue, "\u{1b}[36mcyan"));
            assert!(fires(&mut dim, &dim_queue, "\u{1b}[1;36mcyan"));
            assert!(!fires(&mut dim, &dim_queue, "\u{1b}[96mcyan"));
            dim.set_bold_is_bright(true);
            assert!(fires(&mut dim, &dim_queue, "\u{1b}[36mcyan"));
            assert!(!fires(&mut dim, &dim_queue, "\u{1b}[1;36mcyan"));
            assert!(!fires(&mut dim, &dim_queue, "\u{1b}[96mcyan"));
            let lines = live_lines(b"\x1b[36mheader\r\n\x1b[1m\x1b[36mcyan\r\n");
            assert_eq!(lines.len(), 2);
            assert!(!fires_line(&mut dim, &dim_queue, (*lines[1]).clone()));

            let (mut bright, bright_queue) = manager_for(14);
            bright.set_bold_is_bright(false);
            assert!(!fires(&mut bright, &bright_queue, "\u{1b}[36mcyan"));
            assert!(!fires(&mut bright, &bright_queue, "\u{1b}[1;36mcyan"));
            assert!(fires(&mut bright, &bright_queue, "\u{1b}[96mcyan"));
            bright.set_bold_is_bright(true);
            assert!(!fires(&mut bright, &bright_queue, "\u{1b}[36mcyan"));
            assert!(fires(&mut bright, &bright_queue, "\u{1b}[1;36mcyan"));
            assert!(fires(&mut bright, &bright_queue, "\u{1b}[96mcyan"));
        }

        #[test]
        fn blank_colored_anti_pattern_scans_for_a_qualifying_style() {
            let matchers = [
                TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: "go".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: None,
                },
                TriggerMatcherSource {
                    role: MatcherRole::Anti,
                    syntax: MatcherSyntax::Regex,
                    source: String::new(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                },
            ];
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "colored anti".to_string(),
                ["go"].into_iter(),
                std::iter::empty::<&str>(),
                [""].into_iter(),
                Some(&matchers),
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let line = StyledLine::new(
                "go red",
                vec![
                    VtSpan {
                        style: Style::default(),
                        begin_pos: 0,
                        end_pos: 3,
                    },
                    VtSpan {
                        style: Style {
                            fg: RED,
                            ..Style::default()
                        },
                        begin_pos: 3,
                        end_pos: 6,
                    },
                ],
            );
            assert!(trigger.anti_matches(&line.text, Some(&line), false));
        }

        #[test]
        fn colored_anti_prefilter_uses_plain_styled_text() {
            let matchers = [
                TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: "go".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: None,
                },
                TriggerMatcherSource {
                    role: MatcherRole::Anti,
                    syntax: MatcherSyntax::Regex,
                    source: "^red$".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                },
            ];
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "colored anti".to_string(),
                ["go"].into_iter(),
                std::iter::empty::<&str>(),
                ["^red$"].into_iter(),
                Some(&matchers),
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let raw = "\u{1b}[31mred";
            let line = parse_ansi_fragment(raw);

            assert!(trigger.anti_matches(raw, Some(&line), false));
        }

        #[test]
        fn colored_anti_text_miss_does_not_qualify_a_matching_style() {
            let matchers = [
                TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: "go".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: None,
                },
                TriggerMatcherSource {
                    role: MatcherRole::Anti,
                    syntax: MatcherSyntax::Regex,
                    source: "stop".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                },
            ];
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "colored anti miss".to_string(),
                ["go"].into_iter(),
                std::iter::empty::<&str>(),
                ["stop"].into_iter(),
                Some(&matchers),
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let line = StyledLine::new(
                "go",
                vec![VtSpan {
                    style: Style {
                        fg: RED,
                        ..Style::default()
                    },
                    begin_pos: 0,
                    end_pos: 2,
                }],
            );

            assert!(!trigger.anti_matches(&line.text, Some(&line), false));
        }

        #[test]
        fn colored_anti_checks_each_pattern_after_the_prefilter_matches() {
            let matchers = [
                TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: "go".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: None,
                },
                TriggerMatcherSource {
                    role: MatcherRole::Anti,
                    syntax: MatcherSyntax::Regex,
                    source: "stop".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                },
                TriggerMatcherSource {
                    role: MatcherRole::Anti,
                    syntax: MatcherSyntax::Regex,
                    source: "stop".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 4 }),
                        ..Default::default()
                    }),
                },
            ];
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "colored anti".to_string(),
                ["go"].into_iter(),
                std::iter::empty::<&str>(),
                ["stop", "stop"].into_iter(),
                Some(&matchers),
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let blue_line = StyledLine::new(
                "go stop",
                vec![
                    VtSpan {
                        style: Style::default(),
                        begin_pos: 0,
                        end_pos: 3,
                    },
                    VtSpan {
                        style: Style {
                            fg: BLUE,
                            ..Style::default()
                        },
                        begin_pos: 3,
                        end_pos: 7,
                    },
                ],
            );
            let plain_line = StyledLine::new(
                "go stop",
                vec![VtSpan {
                    style: Style::default(),
                    begin_pos: 0,
                    end_pos: 7,
                }],
            );

            assert!(trigger.anti_matches(&blue_line.text, Some(&blue_line), false));
            assert!(!trigger.anti_matches(&plain_line.text, Some(&plain_line), false));
        }

        #[test]
        fn uncolored_trigger_omits_the_colored_anti_prefilter() {
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "plain anti".to_string(),
                ["go"].into_iter(),
                std::iter::empty::<&str>(),
                ["stop"].into_iter(),
                None,
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();

            assert!(trigger.colored_anti_pattern_set.is_none());
            assert!(trigger.anti_matches("go stop", None, false));
        }

        #[test]
        fn unconstrained_persisted_filter_does_not_turn_blank_pattern_into_match_all() {
            let matchers = [TriggerMatcherSource {
                role: MatcherRole::Match,
                syntax: MatcherSyntax::Regex,
                source: String::new(),
                anchor_start: true,
                anchor_end: true,
                color: Some(MatcherColorMatch::default()),
            }];
            let trigger = Trigger::new(
                IsolateId::Main,
                Origin::User,
                "any style".to_string(),
                [""].into_iter(),
                std::iter::empty::<&str>(),
                std::iter::empty::<&str>(),
                Some(&matchers),
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
            )
            .unwrap();

            assert!(trigger.patterns.is_empty());
            assert!(trigger.pattern_colors.is_empty());
        }

        #[test]
        fn script_preparation_normalizes_empty_style_before_filtering_empty_rows() {
            let empty_style = MatcherColorMatch::default();
            let prepared = PreparedScriptTriggerPatterns::prepare(
                vec![ScriptTriggerPattern {
                    source: "go".to_string(),
                    style: Some(empty_style.clone()),
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
            assert_eq!(prepared.patterns[0].as_str(), "go");
            assert!(prepared.pattern_colors.is_empty());

            let error = PreparedScriptTriggerPatterns::prepare(
                vec![ScriptTriggerPattern {
                    source: String::new(),
                    style: Some(empty_style),
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("at least one normal or raw"));

            let prepared = PreparedScriptTriggerPatterns::prepare(
                vec![ScriptTriggerPattern {
                    source: String::new(),
                    style: None,
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
            assert!(prepared.patterns.is_empty());

            let prepared =
                PreparedScriptTriggerPatterns::prepare(Vec::new(), vec![String::new()], Vec::new())
                    .unwrap();
            assert!(prepared.raw_patterns.is_empty());

            let error = PreparedScriptTriggerPatterns::prepare(
                Vec::new(),
                Vec::new(),
                vec![ScriptTriggerPattern {
                    source: "stop".to_string(),
                    style: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                }],
            )
            .unwrap_err();
            assert!(error.to_string().contains("at least one normal or raw"));
        }

        #[test]
        fn persisted_rows_reject_ansi_aliasing_and_conflicting_attributes() {
            let invalid_match = |color: MatcherColorMatch| {
                let matchers = [TriggerMatcherSource {
                    role: MatcherRole::Match,
                    syntax: MatcherSyntax::Regex,
                    source: "go".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    color: Some(color),
                }];
                Trigger::new(
                    IsolateId::Main,
                    Origin::User,
                    "invalid persisted color".to_string(),
                    ["go"].into_iter(),
                    std::iter::empty::<&str>(),
                    std::iter::empty::<&str>(),
                    Some(&matchers),
                    ScriptAction::Noop,
                    false,
                    true,
                    0,
                    true,
                    None,
                    None,
                )
                .unwrap_err()
                .to_string()
            };

            let error = invalid_match(MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 16 }),
                ..Default::default()
            });
            assert!(error.contains("between 0 and 15"), "{error}");

            let error = invalid_match(MatcherColorMatch {
                attributes: vec![
                    MatcherTextAttribute::Underline,
                    MatcherTextAttribute::DoubleUnderline,
                ],
                ..Default::default()
            });
            assert!(error.contains("single and double underline"), "{error}");
        }

        #[test]
        fn script_preparation_validates_all_regexes_before_installation() {
            for (normal, raw, anti, expected) in [
                (
                    vec![ScriptTriggerPattern {
                        source: "[".to_string(),
                        style: None,
                    }],
                    Vec::new(),
                    Vec::new(),
                    "normal[0]",
                ),
                (Vec::new(), vec!["[".to_string()], Vec::new(), "raw[0]"),
                (
                    vec![ScriptTriggerPattern {
                        source: "go".to_string(),
                        style: None,
                    }],
                    Vec::new(),
                    vec![ScriptTriggerPattern {
                        source: "[".to_string(),
                        style: None,
                    }],
                    "anti[0]",
                ),
            ] {
                let error = PreparedScriptTriggerPatterns::prepare(normal, raw, anti).unwrap_err();
                assert!(error.to_string().contains(expected), "{error:#}");
            }
        }

        #[test]
        fn captures_come_from_the_first_color_fitting_match() {
            let queue: ActionQueue = Rc::default();
            let mut manager = Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
            let name = Arc::new("colored".to_string());
            let patterns = Arc::new(vec![r"(?<number>red\d)".to_string()]);
            let empty = Arc::new(Vec::<String>::new());
            let matchers = [TriggerMatcherSource {
                role: MatcherRole::Match,
                syntax: MatcherSyntax::Regex,
                source: r"(?<number>red\d)".to_string(),
                anchor_start: true,
                anchor_end: true,
                color: Some(MatcherColorMatch {
                    foreground: Some(MatcherColor::Ansi { index: 1 }),
                    ..Default::default()
                }),
            }];
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &name,
                    patterns: &patterns,
                    raw_patterns: &empty,
                    anti_patterns: &empty,
                    matchers: Some(&matchers),
                    action: ScriptAction::Noop,
                    prompt: false,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
            let line = Arc::new(StyledLine::new(
                "red1 red2",
                vec![
                    VtSpan {
                        style: Style::default(),
                        begin_pos: 0,
                        end_pos: 5,
                    },
                    VtSpan {
                        style: Style {
                            fg: RED,
                            ..Style::default()
                        },
                        begin_pos: 5,
                        end_pos: 9,
                    },
                ],
            ));
            manager.process_incoming_line(&line).unwrap();

            let captures = queue
                .borrow_mut()
                .drain(..)
                .find_map(|action| match action {
                    RuntimeAction::RunAutomation { matches, .. } => Some(matches),
                    _ => None,
                })
                .expect("colored trigger should fire");
            assert_eq!(captures.get(0).unwrap().value, "red2");
            assert_eq!(captures.get(1).unwrap().value, "red2");
        }

        #[test]
        fn prepared_color_only_trigger_captures_an_empty_whole_match() {
            let queue: ActionQueue = Rc::default();
            let mut manager = Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
            let prepared = PreparedScriptTriggerPatterns::prepare(
                vec![ScriptTriggerPattern {
                    source: String::new(),
                    style: Some(MatcherColorMatch {
                        foreground: Some(MatcherColor::Ansi { index: 1 }),
                        ..Default::default()
                    }),
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
            manager.push_script_trigger(
                IsolateId::Main,
                Origin::User,
                "red run".to_string(),
                prepared,
                ScriptAction::Noop,
                false,
                true,
                0,
                true,
                None,
                None,
                None,
            );
            let line = Arc::new(StyledLine::new(
                "plain red",
                vec![
                    VtSpan {
                        style: Style::default(),
                        begin_pos: 0,
                        end_pos: 6,
                    },
                    VtSpan {
                        style: Style {
                            fg: RED,
                            ..Style::default()
                        },
                        begin_pos: 6,
                        end_pos: 9,
                    },
                ],
            ));

            manager.process_incoming_line(&line).unwrap();
            let captures = queue
                .borrow_mut()
                .drain(..)
                .find_map(|action| match action {
                    RuntimeAction::RunAutomation { matches, .. } => Some(matches),
                    _ => None,
                })
                .expect("color-only trigger should fire");
            assert_eq!(captures.len(), 1);
            assert_eq!(captures.get(0).unwrap().value, "");
        }
    }

    mod raw_wanted {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        use super::super::{Manager, PushTriggerParams, ScriptAction};
        use crate::session::runtime::origin::{IsolateId, Origin};

        fn manager() -> Manager {
            Manager::new(
                std::rc::Rc::default(),
                Arc::new(";".to_string()),
                std::rc::Rc::default(),
            )
        }

        fn push(manager: &mut Manager, name: &str, raw_patterns: Vec<String>) {
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &Arc::new(name.to_string()),
                    patterns: &Arc::new(vec!["plain".to_string()]),
                    raw_patterns: &Arc::new(raw_patterns),
                    anti_patterns: &Arc::new(Vec::new()),
                    matchers: None,
                    action: ScriptAction::SendSimple(Arc::new("ok".to_string())),
                    prompt: false,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
        }

        #[test]
        fn flag_tracks_raw_pattern_existence_across_mutations() {
            let mut m = manager();
            let flag = m.raw_wanted_flag();
            assert!(!flag.load(Ordering::Relaxed), "empty manager wants no raw");

            push(&mut m, "plain-only", Vec::new());
            assert!(
                !flag.load(Ordering::Relaxed),
                "plain triggers don't ask for raw"
            );

            push(&mut m, "raw", vec!["\\x1b\\[31m".to_string()]);
            assert!(
                flag.load(Ordering::Relaxed),
                "a raw pattern raises the flag"
            );

            m.remove_trigger(&IsolateId::Main, &Origin::User, "raw");
            assert!(
                !flag.load(Ordering::Relaxed),
                "removing the last raw trigger lowers it"
            );

            // An upsert that drops the raw pattern lowers it too.
            push(&mut m, "raw2", vec!["raw".to_string()]);
            assert!(flag.load(Ordering::Relaxed));
            push(&mut m, "raw2", Vec::new());
            assert!(!flag.load(Ordering::Relaxed), "upsert away the raw pattern");
        }

        #[test]
        fn adopted_flag_cell_keeps_feeding_the_old_clone() {
            let mut old = manager();
            push(&mut old, "raw", vec!["raw".to_string()]);
            let connection_clone = old.raw_wanted_flag();
            assert!(connection_clone.load(Ordering::Relaxed));

            // Reload: a fresh manager adopts the old cell; the connection's
            // clone immediately reflects the empty new manager…
            let mut fresh = manager();
            fresh.adopt_raw_wanted_flag(old.raw_wanted_flag());
            assert!(!connection_clone.load(Ordering::Relaxed));

            // …and re-registration into the NEW manager raises the OLD clone.
            push(&mut fresh, "raw", vec!["raw".to_string()]);
            assert!(connection_clone.load(Ordering::Relaxed));
        }
    }

    mod one_fire_per_line {
        use std::rc::Rc;
        use std::sync::Arc;

        use super::super::{ActionQueue, Manager, PushTriggerParams, ScriptAction};
        use crate::session::runtime::RuntimeAction;
        use crate::session::runtime::origin::{IsolateId, Origin};
        use crate::session::styled_line::StyledLine;

        fn manager() -> (Manager, ActionQueue) {
            let queue: ActionQueue = Rc::default();
            (
                Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default()),
                queue,
            )
        }

        fn push(
            manager: &mut Manager,
            name: &str,
            patterns: Vec<String>,
            raw_patterns: Vec<String>,
            prompt: bool,
        ) {
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &Arc::new(name.to_string()),
                    patterns: &Arc::new(patterns),
                    raw_patterns: &Arc::new(raw_patterns),
                    anti_patterns: &Arc::new(Vec::new()),
                    matchers: None,
                    action: ScriptAction::SendSimple(Arc::new("ok".to_string())),
                    prompt,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
        }

        fn drain_fires(queue: &ActionQueue) -> Vec<RuntimeAction> {
            queue
                .borrow_mut()
                .drain(..)
                .filter(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
                .collect()
        }

        fn line_with_raw(text: &str, raw: &str) -> Arc<StyledLine> {
            Arc::new(StyledLine::new_with_raw(
                text,
                Vec::new(),
                Some(raw.as_bytes()),
            ))
        }

        /// Whether the queued automation's captures came from a pattern with a named
        /// `hp` group — the raw patterns below have one, the normal patterns none, so
        /// this identifies which pass produced the fire.
        fn captured_hp(action: &RuntimeAction) -> bool {
            let RuntimeAction::RunAutomation { matches, .. } = action else {
                return false;
            };
            matches.iter().any(|capture| capture.name == Some("hp"))
        }

        #[test]
        fn double_match_fires_once_and_raw_wins() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                "both",
                vec!["42hp".to_string()],
                vec![r"\x1b\[31m(?<hp>\d+)hp".to_string()],
                false,
            );

            manager
                .process_incoming_line(&line_with_raw("42hp", "\x1b[31m42hp"))
                .unwrap();

            let fires = drain_fires(&queue);
            assert_eq!(fires.len(), 1, "one fire per trigger per line");
            assert!(
                captured_hp(&fires[0]),
                "raw runs first, so raw wins the fire"
            );
        }

        #[test]
        fn normal_pass_still_fires_when_raw_missed() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                "both",
                vec!["42hp".to_string()],
                vec![r"\x1b\[99m".to_string()],
                false,
            );

            manager
                .process_incoming_line(&line_with_raw("42hp", "\x1b[31m42hp"))
                .unwrap();

            let fires = drain_fires(&queue);
            assert_eq!(fires.len(), 1);
            assert!(
                !captured_hp(&fires[0]),
                "the normal pattern produced the fire"
            );
        }

        #[test]
        fn distinct_triggers_are_not_suppressed_by_each_other() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                "raw-only",
                Vec::new(),
                vec![r"\x1b\[31m(?<hp>\d+)hp".to_string()],
                false,
            );
            push(
                &mut manager,
                "normal-only",
                vec!["42hp".to_string()],
                Vec::new(),
                false,
            );

            manager
                .process_incoming_line(&line_with_raw("42hp", "\x1b[31m42hp"))
                .unwrap();

            assert_eq!(
                drain_fires(&queue).len(),
                2,
                "the fired list is per trigger, not global"
            );
        }

        #[test]
        fn first_prompt_after_registration_rebuilds_and_fires_once_across_its_passes() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                "prompt-both",
                vec!["42hp".to_string()],
                vec![r"\x1b\[31m(?<hp>\d+)hp".to_string()],
                true,
            );

            manager
                .process_partial_line(line_with_raw("42hp", "\x1b[31m42hp"))
                .unwrap();

            let fires = drain_fires(&queue);
            assert_eq!(fires.len(), 1, "one fire per trigger per prompt line");
            assert!(captured_hp(&fires[0]), "raw wins on the prompt path too");
        }

        /// The JS-side flag helper bakes a flagged `RegExp`'s flags in as a
        /// non-capturing `(?i:...)` wrapper. Positional capture materialization in
        /// `Trigger::run` — index in the vec is the group number — must see the same
        /// numbering as the unwrapped source.
        #[test]
        fn inline_flag_wrapper_is_transparent_to_group_numbering() {
            let (mut manager, queue) = manager();
            manager
                .push_simple_alias(
                    super::super::super::origin::IsolateId::Main,
                    super::super::super::origin::Origin::User,
                    std::sync::Arc::new("num".to_string()),
                    std::sync::Arc::new(vec![r"(?i:^num (\w+) (?<who>\w+)$)".to_string()]),
                    std::sync::Arc::new("say $1 $who".to_string()),
                    0,
                    true,
                    false,
                    None,
                    None,
                )
                .unwrap();

            manager
                .process_outgoing_line("NUM One Two", 0, None)
                .unwrap();

            let captures = queue
                .borrow_mut()
                .drain(..)
                .find_map(|action| match action {
                    crate::session::runtime::RuntimeAction::RunAutomation { matches, .. } => {
                        Some(matches)
                    }
                    _ => None,
                })
                .expect("the (?i:) alias fires on differently-cased input");
            assert_eq!(captures.get(0).unwrap().value, "NUM One Two");
            assert_eq!(captures.get(1).unwrap().value, "One");
            assert!(captures.get(1).unwrap().name.is_none());
            assert_eq!(captures.get(2).unwrap().name, Some("who"));
            assert_eq!(captures.get(2).unwrap().value, "Two");
        }

        #[test]
        fn consecutive_lines_each_get_their_own_fire() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                "both",
                vec!["42hp".to_string()],
                vec![r"\x1b\[31m(?<hp>\d+)hp".to_string()],
                false,
            );

            let line = line_with_raw("42hp", "\x1b[31m42hp");
            manager.process_incoming_line(&line).unwrap();
            manager.process_incoming_line(&line).unwrap();

            assert_eq!(
                drain_fires(&queue).len(),
                2,
                "the fired list must not leak across lines"
            );
        }
    }

    mod command_aliases {
        use std::rc::Rc;
        use std::sync::Arc;

        use super::super::{ActionQueue, Manager};
        use crate::models::matchers::{
            ArgKind, ArgSpec, CommandSpec, ParseMode, command_prefilter,
        };
        use crate::session::runtime::RuntimeAction;
        use crate::session::runtime::origin::{IsolateId, Origin};

        fn manager_with_command(spec: CommandSpec) -> (Manager, ActionQueue) {
            let queue: ActionQueue = Rc::default();
            let mut manager = Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
            let prefilter = command_prefilter(&spec.name);
            manager
                .push_simple_alias(
                    IsolateId::Main,
                    Origin::User,
                    Arc::new("cmd".to_string()),
                    Arc::new(vec![prefilter]),
                    Arc::new("say hi to $person".to_string()),
                    0,
                    true,
                    false,
                    None,
                    Some(spec),
                )
                .unwrap();
            (manager, queue)
        }

        fn greet_spec(kind: ArgKind) -> CommandSpec {
            CommandSpec {
                name: "greet".to_string(),
                args: vec![ArgSpec {
                    name: "person".to_string(),
                    kind,
                }],
                parse: ParseMode::All,
            }
        }

        fn drain(queue: &ActionQueue) -> Vec<RuntimeAction> {
            queue.borrow_mut().drain(..).collect()
        }

        fn count_runs(actions: &[RuntimeAction]) -> usize {
            actions
                .iter()
                .filter(|a| matches!(a, RuntimeAction::RunAutomation { .. }))
                .count()
        }

        #[test]
        fn fired_command_produces_parser_captures() {
            let (mut manager, queue) = manager_with_command(greet_spec(ArgKind::Required));
            manager
                .process_outgoing_line(r#"greet "big ugly troll""#, 0, None)
                .unwrap();

            let actions = drain(&queue);
            let matches = actions
                .iter()
                .find_map(|a| match a {
                    RuntimeAction::RunAutomation { matches, .. } => Some(matches),
                    _ => None,
                })
                .expect("the command fires");
            assert_eq!(matches.get(0).unwrap().value, r#"greet "big ugly troll""#);
            assert_eq!(matches.get(1).unwrap().name, Some("person"));
            assert_eq!(matches.get(1).unwrap().value, "big ugly troll");
        }

        #[test]
        fn missing_required_echoes_usage_and_captures() {
            let (mut manager, queue) = manager_with_command(greet_spec(ArgKind::Required));
            manager.process_outgoing_line("greet", 0, None).unwrap();

            let actions = drain(&queue);
            assert_eq!(count_runs(&actions), 0, "no fire on a missing required arg");
            let usage = actions
                .iter()
                .find_map(|a| match a {
                    RuntimeAction::EchoUsage {
                        text, is_captured, ..
                    } => Some((text.clone(), is_captured.clone())),
                    _ => None,
                })
                .expect("the usage echo is queued");
            assert_eq!(usage.0.as_str(), "Usage: greet <person>");
            assert!(usage.1.is_some(), "the echo can mark the input captured");
        }

        #[test]
        fn unclaimed_tokens_fall_through() {
            let (mut manager, queue) = manager_with_command(greet_spec(ArgKind::Required));
            manager
                .process_outgoing_line("greet Mira Bob", 0, None)
                .unwrap();

            let actions = drain(&queue);
            assert_eq!(count_runs(&actions), 0);
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, RuntimeAction::EchoUsage { .. })),
                "unclaimed tokens are not an arity failure"
            );
            // The trailing SendRawUnless still carries the typed line.
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, RuntimeAction::SendRawUnless(_, line) if line.as_str() == "greet Mira Bob")),
            );
        }

        #[test]
        fn command_names_update_tracks_enabled_command_aliases() {
            let (mut manager, _queue) = manager_with_command(greet_spec(ArgKind::Optional));
            let names = manager
                .command_names_update()
                .expect("the first take sees the new alias");
            assert_eq!(
                names.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
                ["greet"]
            );
            assert!(
                manager.command_names_update().is_none(),
                "no change, no re-send"
            );

            manager.enable_alias(&IsolateId::Main, &Origin::User, "cmd", false);
            let names = manager
                .command_names_update()
                .expect("a disable changes the list");
            assert!(names.is_empty());
        }

        #[test]
        fn stale_sidecar_first_word_mismatch_cannot_fire() {
            // A hand-edited pattern (`^greetz...`) with a sidecar naming `greet`:
            // the prefilter matches `greetz hi`, but the parser re-checks the
            // first word and refuses, so the line falls through.
            let queue: ActionQueue = Rc::default();
            let mut manager = Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default());
            manager
                .push_simple_alias(
                    IsolateId::Main,
                    Origin::User,
                    Arc::new("cmd".to_string()),
                    Arc::new(vec![command_prefilter("greetz")]),
                    Arc::new("say hi".to_string()),
                    0,
                    true,
                    false,
                    None,
                    Some(greet_spec(ArgKind::Optional)),
                )
                .unwrap();
            manager.process_outgoing_line("greetz hi", 0, None).unwrap();
            assert_eq!(count_runs(&drain(&queue)), 0);
        }
    }

    mod alias_matching_guards {
        use std::rc::Rc;
        use std::sync::Arc;

        use super::super::{ActionQueue, AliasSender, Manager};
        use crate::session::runtime::RuntimeAction;
        use crate::session::runtime::origin::{IsolateId, Origin};

        fn manager() -> (Manager, ActionQueue) {
            let queue: ActionQueue = Rc::default();
            (
                Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default()),
                queue,
            )
        }

        fn push(
            manager: &mut Manager,
            name: &str,
            pattern: &str,
            body: &str,
            allow_self_match: bool,
        ) {
            manager
                .push_simple_alias(
                    IsolateId::Main,
                    Origin::User,
                    Arc::new(name.to_string()),
                    Arc::new(vec![pattern.to_string()]),
                    Arc::new(body.to_string()),
                    0,
                    true,
                    allow_self_match,
                    None,
                    None,
                )
                .unwrap();
        }

        fn drain_runs(queue: &ActionQueue) -> usize {
            queue
                .borrow_mut()
                .drain(..)
                .filter(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
                .count()
        }

        fn sender(name: &str) -> AliasSender {
            AliasSender {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name.to_string()),
            }
        }

        #[test]
        fn empty_pattern_alias_never_matches() {
            let (mut manager, queue) = manager();
            push(&mut manager, "empty", "", "say hi", false);

            manager
                .process_outgoing_line("anything at all", 0, None)
                .unwrap();

            let actions: Vec<RuntimeAction> = queue.borrow_mut().drain(..).collect();
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, RuntimeAction::RunAutomation { .. })),
                "an empty pattern must not match every command"
            );
            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    RuntimeAction::SendRawUnless(_, line) if line.as_str() == "anything at all"
                )),
                "the typed line still falls through to the wire"
            );
        }

        #[test]
        fn own_output_skips_the_sending_alias_by_default() {
            let (mut manager, queue) = manager();
            push(&mut manager, "loop", "^spin$", "spin", false);

            manager.process_outgoing_line("spin", 0, None).unwrap();
            assert_eq!(drain_runs(&queue), 1, "typed input matches normally");

            manager
                .process_outgoing_line("spin", 1, Some(&sender("loop")))
                .unwrap();
            assert_eq!(
                drain_runs(&queue),
                0,
                "the alias's own output must not re-match it"
            );
        }

        #[test]
        fn allow_self_match_opts_back_in() {
            let (mut manager, queue) = manager();
            push(&mut manager, "walk", "^walk$", "walk", true);

            manager
                .process_outgoing_line("walk", 1, Some(&sender("walk")))
                .unwrap();
            assert_eq!(
                drain_runs(&queue),
                1,
                "an opted-in alias may match its own output"
            );
        }

        #[test]
        fn another_alias_output_still_matches() {
            let (mut manager, queue) = manager();
            push(&mut manager, "target", "^spin$", "say hi", false);

            manager
                .process_outgoing_line("spin", 1, Some(&sender("other")))
                .unwrap();
            assert_eq!(
                drain_runs(&queue),
                1,
                "only the SENDING alias is excluded from its output"
            );
        }

        #[test]
        fn depth_bail_names_the_looping_alias() {
            let (mut manager, _queue) = manager();
            push(&mut manager, "loop", "^spin$", "spin", true);

            let err = manager
                .process_outgoing_line("spin", 101, Some(&sender("loop")))
                .unwrap_err();
            assert!(
                err.to_string().contains("\"loop\""),
                "the bail should name the alias: {err}"
            );
        }
    }

    mod empty_trigger_patterns {
        use std::rc::Rc;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        use super::super::{ActionQueue, Manager, PushTriggerParams, ScriptAction};
        use crate::session::runtime::RuntimeAction;
        use crate::session::runtime::origin::{IsolateId, Origin};
        use crate::session::styled_line::StyledLine;

        fn manager() -> (Manager, ActionQueue) {
            let queue: ActionQueue = Rc::default();
            (
                Manager::new(queue.clone(), Arc::new(";".to_string()), Rc::default()),
                queue,
            )
        }

        fn push(
            manager: &mut Manager,
            patterns: Vec<String>,
            raw_patterns: Vec<String>,
            anti_patterns: Vec<String>,
        ) {
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &Arc::new("guarded".to_string()),
                    patterns: &Arc::new(patterns),
                    raw_patterns: &Arc::new(raw_patterns),
                    anti_patterns: &Arc::new(anti_patterns),
                    matchers: None,
                    action: ScriptAction::SendSimple(Arc::new("ok".to_string())),
                    prompt: false,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
        }

        fn drain_runs(queue: &ActionQueue) -> usize {
            queue
                .borrow_mut()
                .drain(..)
                .filter(|action| matches!(action, RuntimeAction::RunAutomation { .. }))
                .count()
        }

        fn line_with_raw(text: &str, raw: &str) -> Arc<StyledLine> {
            Arc::new(StyledLine::new_with_raw(
                text,
                Vec::new(),
                Some(raw.as_bytes()),
            ))
        }

        #[test]
        fn empty_trigger_pattern_never_fires() {
            let (mut manager, queue) = manager();
            push(&mut manager, vec![String::new()], Vec::new(), Vec::new());

            manager
                .process_incoming_line(&line_with_raw("anything at all", "anything at all"))
                .unwrap();

            assert_eq!(
                drain_runs(&queue),
                0,
                "an empty pattern must not match every line"
            );
        }

        #[test]
        fn empty_raw_pattern_neither_fires_nor_requests_raw_capture() {
            let (mut manager, queue) = manager();
            push(&mut manager, Vec::new(), vec![String::new()], Vec::new());

            assert!(
                !manager.raw_wanted_flag().load(Ordering::Relaxed),
                "a dropped empty raw pattern must not enable raw capture"
            );

            manager
                .process_incoming_line(&line_with_raw("42hp", "\x1b[31m42hp"))
                .unwrap();
            assert_eq!(drain_runs(&queue), 0);
        }

        #[test]
        fn empty_exception_does_not_block() {
            let (mut manager, queue) = manager();
            push(
                &mut manager,
                vec!["42hp".to_string()],
                Vec::new(),
                vec![String::new()],
            );

            manager
                .process_incoming_line(&line_with_raw("42hp", "42hp"))
                .unwrap();

            assert_eq!(
                drain_runs(&queue),
                1,
                "an empty exception must not veto every line"
            );
        }
    }

    /// Build captures from a list of `(name, value)` pairs; position is the group number.
    fn caps(items: &[(Option<&str>, &str)]) -> Vec<MatchCapture> {
        items
            .iter()
            .map(|(name, value)| MatchCapture {
                name: name.map(|n| std::borrow::Cow::Owned(n.to_string())),
                value: (*value).to_string(),
            })
            .collect()
    }

    #[test]
    fn template_expands_double_digit_groups_without_clobber() {
        // 11 groups (index 0 = whole match, 1..=10 = groups). `${10}` must resolve group ten,
        // and bare `$1` must resolve group one even when followed by another digit.
        let captures = caps(&[
            (None, "WHOLE"),
            (None, "g1"),
            (None, "g2"),
            (None, "g3"),
            (None, "g4"),
            (None, "g5"),
            (None, "g6"),
            (None, "g7"),
            (None, "g8"),
            (None, "g9"),
            (None, "g10"),
        ]);
        // `${10}` is group ten; `${1}` is group one — no collision.
        assert_eq!(expand_template("x ${10} ${1}", &captures), "x g10 g1");
        // Bare `$10` is group one followed by a literal `0` (single-digit rule).
        assert_eq!(expand_template("$10", &captures), "g10");
        // ^ group one ("g1") + literal "0" == "g1" + "0" == "g10". Make the distinction explicit
        // with a group whose value is unambiguous.
        let captures2 = caps(&[(None, "WHOLE"), (None, "ONE")]);
        assert_eq!(expand_template("$10", &captures2), "ONE0");
    }

    #[test]
    fn template_dollar_escape_and_named_groups() {
        let captures = caps(&[(None, "WHOLE"), (None, "g1"), (Some("name"), "NAMED")]);
        assert_eq!(
            expand_template("x $1 $$ ${name}", &captures),
            "x g1 $ NAMED"
        );
        // `$name` identifier form resolves the same named group.
        assert_eq!(expand_template("$name", &captures), "NAMED");
    }

    #[test]
    fn template_unknown_and_empty_groups_expand_empty() {
        let captures = caps(&[(None, "WHOLE"), (None, "")]);
        // Out-of-range index, unknown name, and an empty group all expand to "".
        assert_eq!(expand_template("[${9}]", &captures), "[]");
        assert_eq!(expand_template("[${missing}]", &captures), "[]");
        assert_eq!(expand_template("[$1]", &captures), "[]");
    }

    #[test]
    fn template_lone_and_malformed_dollar_is_literal() {
        let captures = caps(&[(None, "WHOLE")]);
        // Trailing `$`, `$` before a space, and an unterminated `${` are all literal `$`.
        assert_eq!(expand_template("end$", &captures), "end$");
        assert_eq!(expand_template("a $ b", &captures), "a $ b");
        assert_eq!(expand_template("${oops", &captures), "${oops");
    }

    #[test]
    fn template_whole_match_is_index_zero() {
        let captures = caps(&[(None, "the whole thing"), (None, "g1")]);
        assert_eq!(expand_template("[$0]", &captures), "[the whole thing]");
        assert_eq!(expand_template("[${0}]", &captures), "[the whole thing]");
    }

    #[test]
    fn default_separator_splits_commands() {
        assert_eq!(
            split_commands("north;south;east", ";"),
            vec!["north", "south", "east"]
        );
    }

    #[test]
    fn multi_char_separator_splits_and_preserves_single_occurrences() {
        assert_eq!(
            split_commands("say hi; you;;north", ";;"),
            vec!["say hi; you", "north"]
        );
    }

    #[test]
    fn empty_separator_only_splits_on_newlines() {
        assert_eq!(
            split_commands("say a;b\nnorth", ""),
            vec!["say a;b", "north"]
        );
    }

    #[test]
    fn newline_always_splits() {
        assert_eq!(
            split_commands("north\nsouth;east", ";"),
            vec!["north", "south", "east"]
        );
    }
}
