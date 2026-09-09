//! Data model for the Automations window: the script tree, the status model,
//! and the (client-side) package dependency graph derivations described in
//! `docs/new-automations-window.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use smudgy_cloud::DependencyKind;
use smudgy_core::models::shared_packages::LockedPackage;
use smudgy_core::models::{self, aliases, automation_transaction, packages, triggers};
use smudgy_core::session::runtime::{
    AutomationBody, AutomationDelta, AutomationKind, AutomationSummary, Origin,
};

use crate::update::Update;

use super::{AutomationsWindow, Event, Message};

/// Outcome of committing this window's automation tree against its loaded baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutomationSaveStatus {
    Saved,
    /// Another writer changed the persisted state after this window loaded it. Nothing was
    /// written; the caller restores its in-memory edit and the window reloads.
    Conflict,
}

/// Live script-created automations for the open session, kept in sync from the per-session
/// automation broadcast and keyed by creator [`Origin`]. The sidebar nests each creator's
/// aliases/triggers under its module/package node. Disk-authored automations are not here
/// (they come from the on-disk model); only `Module`/`Package`-origin ones are streamed.
#[derive(Default)]
pub struct LiveAutomations {
    by_origin: HashMap<Origin, CreatorAutomations>,
}

/// One creator's script-created aliases/triggers (name → live detail).
#[derive(Default)]
pub struct CreatorAutomations {
    pub aliases: BTreeMap<String, LiveAutomation>,
    pub triggers: BTreeMap<String, LiveAutomation>,
}

/// A single script-created automation's live state, mirrored from the runtime's introspection
/// stream — its on/off flag plus the read-only pattern/body shown in the detail pane.
#[derive(Clone)]
pub struct LiveAutomation {
    pub enabled: bool,
    /// Match pattern(s), joined for display. Empty when it has none.
    pub pattern: Arc<str>,
    /// What it does (command text, script source, or none). Display-only.
    pub body: AutomationBody,
}

impl LiveAutomations {
    /// Replace all state from a fresh snapshot (sent when the window subscribes, and after a
    /// session reload).
    pub fn reset(&mut self, summaries: &[AutomationSummary]) {
        self.by_origin.clear();
        for summary in summaries {
            self.upsert(summary);
        }
    }

    /// Apply a batch of incremental changes on top of the current state.
    pub fn apply(&mut self, deltas: &[AutomationDelta]) {
        for delta in deltas {
            match delta {
                AutomationDelta::Upserted(summary) => self.upsert(summary),
                AutomationDelta::EnabledChanged {
                    kind,
                    origin,
                    name,
                    enabled,
                } => {
                    if let Some(creator) = self.by_origin.get_mut(origin)
                        && let Some(slot) = creator.map_mut(*kind).get_mut(name)
                    {
                        slot.enabled = *enabled;
                    }
                }
                AutomationDelta::Removed { kind, origin, name } => {
                    if let Some(creator) = self.by_origin.get_mut(origin) {
                        creator.map_mut(*kind).remove(name);
                    }
                }
            }
        }
    }

    fn upsert(&mut self, summary: &AutomationSummary) {
        let creator = self.by_origin.entry(summary.origin.clone()).or_default();
        creator.map_mut(summary.kind).insert(
            summary.name.clone(),
            LiveAutomation {
                enabled: summary.enabled,
                pattern: summary.pattern.clone(),
                body: summary.body.clone(),
            },
        );
    }

    /// A local module's automations, keyed by its `modules/`-relative subpath.
    pub fn module(&self, subpath: &str) -> Option<&CreatorAutomations> {
        self.by_origin.get(&Origin::Module {
            subpath: subpath.to_string(),
        })
    }

    /// An installed package's automations, matched by owner/name across any resolved version.
    pub fn package(&self, owner: &str, name: &str) -> Option<&CreatorAutomations> {
        self.by_origin
            .iter()
            .find_map(|(origin, creator)| match origin {
                Origin::Package {
                    owner: o, name: n, ..
                } if o == owner && n == name => Some(creator),
                _ => None,
            })
    }
}

impl CreatorAutomations {
    fn map_mut(&mut self, kind: AutomationKind) -> &mut BTreeMap<String, LiveAutomation> {
        match kind {
            AutomationKind::Alias => &mut self.aliases,
            AutomationKind::Trigger => &mut self.triggers,
            // Hotkeys are not streamed as automation deltas (they live in the runtime's own
            // `HotkeyId` map, not the trigger introspection mirror), so this is never reached.
            AutomationKind::Hotkey => unreachable!("hotkeys are not tracked as automation deltas"),
        }
    }
}

/// A leaf automation or a folder, mirroring the on-disk model. Folders hold a
/// nested map of children (the tree is built from each script's `package` path).
#[derive(Debug, Clone)]
pub enum Script {
    Alias(models::aliases::AliasDefinition),
    Hotkey(models::hotkeys::HotkeyDefinition),
    Trigger(models::triggers::TriggerDefinition),
    /// A folder of nested children. Folder enable state lives in the package
    /// tree (`packages.json`), not here; the placeholder `bool` mirrors the
    /// loader's shape and is intentionally never read.
    Folder(#[allow(dead_code)] bool, BTreeMap<String, Script>),
}

impl Script {
    /// The folder (package path) this script lives under, if any.
    pub fn folder_name(&self) -> Option<&str> {
        match self {
            Script::Alias(a) => a.package.as_deref(),
            Script::Hotkey(h) => h.package.as_deref(),
            Script::Trigger(t) => t.package.as_deref(),
            Script::Folder(_, _) => None,
        }
    }

    fn set_folder_name(&mut self, folder: Option<String>) {
        match self {
            Script::Alias(alias) => alias.package = folder,
            Script::Hotkey(hotkey) => hotkey.package = folder,
            Script::Trigger(trigger) => trigger.package = folder,
            Script::Folder(_, _) => {}
        }
    }

    /// This node's own `enabled` flag (folders report `true`).
    pub fn own_enabled(&self) -> bool {
        match self {
            Script::Alias(a) => a.enabled,
            Script::Hotkey(h) => h.enabled,
            Script::Trigger(t) => t.enabled,
            Script::Folder(_, _) => true,
        }
    }
}

/// Identifies a script by its (folder, name) pair for tree selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptKey {
    pub folder_name: Option<String>,
    pub script_name: String,
}

/// A node's at-a-glance health. Drives the colored status dot everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Informational/non-runnable content with no enable state (for example an import-only file).
    Neutral,
    /// Enabled & healthy (green).
    Ok,
    /// Enabled but broken — e.g. a pattern won't compile (red).
    Error,
    /// Needs attention but not broken (orange/amber) — e.g. an installed package whose newest
    /// version is held back because it demands more permissions than were granted (the update is
    /// blocked until the user reviews + grants it, `PACKAGE-ISOLATES-CONSENT-TRUST.md`).
    Warning,
    /// Turned off; won't run (grey).
    Disabled,
}

/// The per-row matcher role in the unified trigger row list (`MatcherRole` in
/// the persisted sidecar; `Anti` renders as "Exceptions" in the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// Must match the line for the trigger to fire.
    Match,
    /// A veto: any match prevents the trigger from firing.
    Anti,
    /// Matched against the raw line, color codes included (regex only).
    Raw,
}

impl PatternKind {
    fn role(self) -> matchers::MatcherRole {
        match self {
            PatternKind::Match => matchers::MatcherRole::Match,
            PatternKind::Anti => matchers::MatcherRole::Anti,
            PatternKind::Raw => matchers::MatcherRole::Raw,
        }
    }

    fn from_role(role: matchers::MatcherRole) -> Self {
        match role {
            matchers::MatcherRole::Match => PatternKind::Match,
            matchers::MatcherRole::Anti => PatternKind::Anti,
            matchers::MatcherRole::Raw => PatternKind::Raw,
        }
    }
}

use smudgy_core::models::matchers::{
    self, AliasMatcherSource, ArgKind, ArgSpec, CmdMode, MatcherSyntax, ParseMode,
    TriggerMatcherSource,
};

/// A `MatcherSyntax` wrapper carrying the pick-list `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxChoice(pub MatcherSyntax);

impl SyntaxChoice {
    pub const ALL: [SyntaxChoice; 2] = [
        SyntaxChoice(MatcherSyntax::Pattern),
        SyntaxChoice(MatcherSyntax::Regex),
    ];
}

impl std::fmt::Display for SyntaxChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            MatcherSyntax::Pattern => crate::i18n::ts!("editor-syntax-pattern"),
            MatcherSyntax::Regex => crate::i18n::ts!("editor-syntax-regex"),
        })
    }
}

/// The selected tab for one foreground or background color channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherColorKind {
    Any,
    Ansi,
    Xterm,
    Truecolor,
    ColorRange,
}

/// Identifies the color-range endpoint that an editor control changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRangeEndpoint {
    First,
    Second,
}

impl ColorRangeEndpoint {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }
}

/// Identifies an RGB input in the exact truecolor editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruecolorComponent {
    Red,
    Green,
    Blue,
}

impl TruecolorComponent {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
        }
    }
}

impl MatcherColorKind {
    pub const ALL: [Self; 5] = [
        Self::Any,
        Self::Ansi,
        Self::Xterm,
        Self::Truecolor,
        Self::ColorRange,
    ];

    pub fn of(color: Option<matchers::MatcherColor>) -> Self {
        match color {
            None => Self::Any,
            Some(matchers::MatcherColor::Ansi { .. }) => Self::Ansi,
            Some(matchers::MatcherColor::Xterm { .. }) => Self::Xterm,
            Some(matchers::MatcherColor::Truecolor { range: None, .. }) => Self::Truecolor,
            Some(matchers::MatcherColor::Truecolor { range: Some(_), .. }) => Self::ColorRange,
        }
    }
}

/// Editable text for one exact truecolor selection.
///
/// These strings preserve incomplete or invalid input while the user types.
/// The row matcher changes only when all values are valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTruecolorDraft {
    pub hex: String,
    pub rgb: [String; 3],
    /// The last complete value. Tab changes restore the matcher from this
    /// value without discarding incomplete text.
    pub last_valid: [u8; 3],
}

impl ExactTruecolorDraft {
    #[must_use]
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            hex: format!("#{r:02x}{g:02x}{b:02x}"),
            rgb: [r.to_string(), g.to_string(), b.to_string()],
            last_valid: [r, g, b],
        }
    }
}

impl Default for ExactTruecolorDraft {
    fn default() -> Self {
        Self::from_rgb(255, 255, 255)
    }
}

/// Editable color text for one terminal color channel.
///
/// The editor stores a separate draft for foreground and background. A
/// channel-tab change does not discard a partial value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelColorDraft {
    pub exact_truecolor: ExactTruecolorDraft,
    pub color_range_hex: [String; 2],
    /// The last complete range. Tab changes restore the matcher from this
    /// value without discarding incomplete endpoint text.
    pub color_range_last_valid: matchers::MatcherHsvRange,
}

impl ChannelColorDraft {
    #[must_use]
    fn from_color(color: Option<matchers::MatcherColor>) -> Self {
        let color_range_last_valid = color_range_value(color);
        Self {
            exact_truecolor: exact_truecolor_draft(color),
            color_range_hex: color_range_hexes(color_range_last_valid),
            color_range_last_valid,
        }
    }
}

impl Default for ChannelColorDraft {
    fn default() -> Self {
        Self::from_color(None)
    }
}

/// Parses six hexadecimal RGB digits with an optional leading `#`.
#[must_use]
pub fn parse_matcher_hex(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let packed = u32::from_str_radix(value, 16).ok()?;
    let [_, r, g, b] = packed.to_be_bytes();
    Some((r, g, b))
}

/// An `ArgKind` wrapper carrying the pick-list `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgKindChoice(pub ArgKind);

impl std::fmt::Display for ArgKindChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            ArgKind::Required => crate::i18n::ts!("editor-arg-required"),
            ArgKind::Optional => crate::i18n::ts!("editor-arg-optional"),
            ArgKind::Rest => crate::i18n::ts!("editor-arg-rest"),
        })
    }
}

/// A `ParseMode` wrapper carrying the pick-list `Display` (label only; the
/// prototype's two-line example rows are the deferred overlay-picker design).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseModeChoice(pub ParseMode);

impl ParseModeChoice {
    pub const ALL: [ParseModeChoice; 5] = [
        ParseModeChoice(ParseMode::Spaces),
        ParseModeChoice(ParseMode::Quotes),
        ParseModeChoice(ParseMode::Braces),
        ParseModeChoice(ParseMode::All),
        ParseModeChoice(ParseMode::Raw),
    ];
}

impl std::fmt::Display for ParseModeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            ParseMode::Spaces => crate::i18n::ts!("editor-parse-spaces"),
            ParseMode::Quotes => crate::i18n::ts!("editor-parse-quotes"),
            ParseMode::Braces => crate::i18n::ts!("editor-parse-braces"),
            ParseMode::All => crate::i18n::ts!("editor-parse-all"),
            ParseMode::Raw => crate::i18n::ts!("editor-parse-raw"),
        })
    }
}

/// The alias editor's matcher kind (the three type cards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasKind {
    Command,
    Pattern,
    Regex,
}

/// The trigger pane's three matcher cards (README §4): a create control while
/// no matcher exists, a kind+role selector while exactly one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerCard {
    Pattern,
    Regex,
    Raw,
}

impl TriggerCard {
    /// The `(syntax, role)` a card stands for.
    pub fn shape(self) -> (MatcherSyntax, PatternKind) {
        match self {
            TriggerCard::Pattern => (MatcherSyntax::Pattern, PatternKind::Match),
            TriggerCard::Regex => (MatcherSyntax::Regex, PatternKind::Match),
            TriggerCard::Raw => (MatcherSyntax::Regex, PatternKind::Raw),
        }
    }

    /// The card describing an existing matcher row, for the selector state.
    pub fn of_row(row: &TriggerRow) -> Self {
        if row.role == PatternKind::Raw {
            TriggerCard::Raw
        } else if row.syntax == MatcherSyntax::Pattern {
            TriggerCard::Pattern
        } else {
            TriggerCard::Regex
        }
    }
}

/// The alias editor's matcher draft: the selected kind plus every kind's
/// buffers, so switching type cards never destroys work. Lives on the window
/// (like the hotkey capture state) and is seeded on open/create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasMatcherDraft {
    pub kind: AliasKind,
    /// A stored command word that differs from the alias's name — legacy data
    /// from when the editor offered a separate command field. `None` — the
    /// ordinary case — inherits the name. Displayed while present, and cleared
    /// by any edit to the Name field: the name is the command.
    pub command_override: Option<String>,
    pub args: Vec<ArgSpec>,
    pub parse: ParseMode,
    pub cmd_mode: CmdMode,
    pub pattern_source: String,
    pub anchor_start: bool,
    pub anchor_end: bool,
    pub regex_source: String,
    /// A stale sidecar degraded this matcher to Regex on load — the stored
    /// pattern was edited by hand, and the hand edit wins.
    pub degraded: bool,
}

impl Default for AliasMatcherDraft {
    fn default() -> Self {
        Self {
            // Command is the default for NEW aliases; opening an existing one
            // reseeds via `from_definition`.
            kind: AliasKind::Command,
            command_override: None,
            args: Vec::new(),
            parse: ParseMode::All,
            cmd_mode: CmdMode::Simple,
            pattern_source: String::new(),
            anchor_start: true,
            anchor_end: true,
            regex_source: String::new(),
            degraded: false,
        }
    }
}

impl AliasMatcherDraft {
    /// Seed the draft from a stored definition. The sidecar drives the editor
    /// only while it still compiles to the stored pattern; on a mismatch (a
    /// hand-edited `pattern`, or a lying package sidecar) the matcher degrades
    /// to Regex showing the stored pattern verbatim — the hand edit wins.
    pub fn from_definition(alias: &aliases::AliasDefinition, alias_name: &str) -> Self {
        let mut draft = Self {
            kind: AliasKind::Regex,
            regex_source: alias.pattern.clone(),
            ..Self::default()
        };
        let Some(source) = &alias.matcher else {
            return draft;
        };
        let fresh = matchers::alias_pattern(source, alias_name)
            .is_ok_and(|derived| derived == alias.pattern);
        if !fresh {
            draft.degraded = true;
            return draft;
        }
        match source {
            AliasMatcherSource::Command {
                name,
                args,
                parse,
                mode,
            } => {
                draft.kind = AliasKind::Command;
                draft.command_override.clone_from(name);
                draft.args.clone_from(args);
                draft.parse = *parse;
                draft.cmd_mode = *mode;
            }
            AliasMatcherSource::Pattern {
                source,
                anchor_start,
                anchor_end,
            } => {
                draft.kind = AliasKind::Pattern;
                draft.pattern_source.clone_from(source);
                draft.anchor_start = *anchor_start;
                draft.anchor_end = *anchor_end;
            }
        }
        draft
    }

    /// The sidecar this draft saves (`None` for the Regex kind — an absent
    /// sidecar IS the Regex kind).
    pub fn to_matcher(&self) -> Option<AliasMatcherSource> {
        match self.kind {
            AliasKind::Regex => None,
            AliasKind::Command => Some(AliasMatcherSource::Command {
                // A revealed-but-blank override is no override: clearing the
                // field collapses back to inheriting the name.
                name: self
                    .command_override
                    .as_deref()
                    .map(str::trim)
                    .filter(|word| !word.is_empty())
                    .map(str::to_string),
                args: self.args.clone(),
                parse: self.parse,
                mode: self.cmd_mode,
            }),
            AliasKind::Pattern => Some(AliasMatcherSource::Pattern {
                source: self.pattern_source.clone(),
                anchor_start: self.anchor_start,
                anchor_end: self.anchor_end,
            }),
        }
    }

    /// The word a Command draft matches on: its override, or the alias's own
    /// name. Both are trimmed, and a blank override inherits.
    pub fn command_word<'a>(&'a self, alias_name: &'a str) -> &'a str {
        self.command_override
            .as_deref()
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .unwrap_or_else(|| alias_name.trim())
    }

    /// The stored pattern this draft saves, or a display-ready error.
    pub fn to_pattern(&self, alias_name: &str) -> Result<String, String> {
        if self.kind == AliasKind::Command {
            let word = self.command_word(alias_name);
            if word.is_empty() {
                return Err(crate::i18n::t!("editor-command-name-empty"));
            }
            // The parser compares the first whitespace-delimited token, so a
            // word with a space in it can never match anything.
            if word.contains(char::is_whitespace) {
                return Err(crate::i18n::t!("editor-command-name-spaces"));
            }
        }
        match self.to_matcher() {
            None => Ok(self.regex_source.clone()),
            Some(matcher) => matchers::alias_pattern(&matcher, alias_name)
                .map_err(|errors| pattern_error_text(&errors[0])),
        }
    }

    /// README §2 invariants on the argument rows, applied after every args
    /// mutation: `Rest` only in the last position (earlier ones demote to
    /// `Optional`), and Simple mode forces every argument `Optional` with the
    /// last one `Rest`.
    pub fn normalize_args(&mut self) {
        let len = self.args.len();
        for (i, arg) in self.args.iter_mut().enumerate() {
            if arg.kind == ArgKind::Rest && i + 1 != len {
                arg.kind = ArgKind::Optional;
            }
        }
        if self.cmd_mode == CmdMode::Simple {
            for (i, arg) in self.args.iter_mut().enumerate() {
                arg.kind = if i + 1 == len {
                    ArgKind::Rest
                } else {
                    ArgKind::Optional
                };
            }
        }
    }
}

/// A display string for one pattern-compilation error.
pub fn pattern_error_text(error: &matchers::PatternError) -> String {
    match error {
        matchers::PatternError::NumberedHole { body } => {
            crate::i18n::t!("editor-numbered-hole", "body" => body.clone())
        }
        matchers::PatternError::UnknownHoleType { body } => {
            crate::i18n::t!("editor-unknown-hole-type", "body" => body.clone())
        }
        matchers::PatternError::Engine { message } => {
            crate::i18n::t!("editor-invalid-regex", "error" => message.clone())
        }
    }
}

/// One trigger matcher row in the editor: role, syntax, source, and the
/// Pattern-syntax anchor checkboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRow {
    pub role: PatternKind,
    pub syntax: MatcherSyntax,
    pub source: String,
    pub anchor_start: bool,
    pub anchor_end: bool,
    /// An optional color filter. Only normal and exception rows expose this
    /// filter. Raw rows continue to match escape bytes directly.
    pub color: Option<matchers::MatcherColorMatch>,
    /// Identifies the color channel that the tabbed editor displays. This
    /// field stores editor-only state.
    pub color_channel: matchers::MatcherColorChannel,
    /// Stores editable color text separately for foreground and background.
    pub color_drafts: [ChannelColorDraft; 2],
    /// The filter most recently disabled with the editor checkbox. This is
    /// transient UI state: save/load continues to persist only [`Self::color`].
    pub(super) remembered_color: Option<matchers::MatcherColorMatch>,
}

impl TriggerRow {
    pub fn new(role: PatternKind) -> Self {
        Self {
            role,
            syntax: MatcherSyntax::Regex,
            source: String::new(),
            anchor_start: true,
            anchor_end: true,
            color: None,
            color_channel: matchers::MatcherColorChannel::Foreground,
            color_drafts: [ChannelColorDraft::default(), ChannelColorDraft::default()],
            remembered_color: None,
        }
    }

    /// Enables or disables the row's color filter without discarding an
    /// authored filter during an in-editor off/on round trip.
    pub fn set_color_enabled(&mut self, enabled: bool) {
        if enabled {
            if self.color.is_none() {
                self.color = Some(self.remembered_color.take().unwrap_or_else(|| {
                    matchers::MatcherColorMatch {
                        foreground: Some(matchers::MatcherColor::Ansi { index: 7 }),
                        ..Default::default()
                    }
                }));
            }
        } else if let Some(color) = self.color.take() {
            self.remembered_color = Some(color);
        }
    }

    #[must_use]
    pub const fn color_channel_index(channel: matchers::MatcherColorChannel) -> usize {
        match channel {
            matchers::MatcherColorChannel::Foreground => 0,
            matchers::MatcherColorChannel::Background => 1,
        }
    }

    #[must_use]
    pub fn color_draft(&self, channel: matchers::MatcherColorChannel) -> &ChannelColorDraft {
        &self.color_drafts[Self::color_channel_index(channel)]
    }

    pub fn color_draft_mut(
        &mut self,
        channel: matchers::MatcherColorChannel,
    ) -> &mut ChannelColorDraft {
        &mut self.color_drafts[Self::color_channel_index(channel)]
    }

    fn color_draft_error(&self) -> Option<String> {
        let filter = self.color.as_ref()?;
        if self.role != PatternKind::Raw
            && self.source.is_empty()
            && filter.foreground.is_none()
            && filter.background.is_none()
            && filter.attributes.is_empty()
        {
            return Some(crate::i18n::t!("editor-color-needs-constraint"));
        }
        for (channel, color) in [
            (matchers::MatcherColorChannel::Foreground, filter.foreground),
            (matchers::MatcherColorChannel::Background, filter.background),
        ] {
            let draft = self.color_draft(channel);
            let error = match color {
                Some(matchers::MatcherColor::Truecolor { range: None, .. })
                    if parse_matcher_hex(&draft.exact_truecolor.hex).is_none() =>
                {
                    Some(crate::i18n::t!("editor-color-invalid-hex"))
                }
                Some(matchers::MatcherColor::Truecolor { range: None, .. })
                    if !draft
                        .exact_truecolor
                        .rgb
                        .iter()
                        .all(|value| value.parse::<u8>().is_ok()) =>
                {
                    Some(crate::i18n::t!("editor-color-invalid-rgb"))
                }
                Some(matchers::MatcherColor::Truecolor { range: Some(_), .. })
                    if draft
                        .color_range_hex
                        .iter()
                        .any(|value| parse_matcher_hex(value).is_none()) =>
                {
                    Some(crate::i18n::t!("editor-color-invalid-hex"))
                }
                _ => None,
            };
            if let Some(error) = error {
                let channel = crate::i18n::translate_static(match channel {
                    matchers::MatcherColorChannel::Foreground => "editor-color-foreground",
                    matchers::MatcherColorChannel::Background => "editor-color-background",
                });
                return Some(format!("{channel}: {error}"));
            }
        }
        None
    }

    /// The stored regex this row derives to, or a display-ready error.
    pub fn compiled(&self) -> Result<String, String> {
        if let Some(error) = self.color_draft_error() {
            return Err(error);
        }
        if self.source.is_empty() && self.color.is_some() && self.role != PatternKind::Raw {
            return Ok(String::new());
        }
        match self.syntax {
            MatcherSyntax::Pattern => {
                let compiled =
                    matchers::compile_pattern(&self.source, self.anchor_start, self.anchor_end);
                match compiled.errors.first() {
                    None => Ok(compiled.source),
                    Some(error) => Err(pattern_error_text(error)),
                }
            }
            MatcherSyntax::Regex => {
                let source = if self.role == PatternKind::Raw {
                    matchers::translate_esc(&self.source)
                } else {
                    self.source.clone()
                };
                regex::Regex::new(&source).map_err(
                    |e| crate::i18n::t!("editor-invalid-regex", "error" => e.to_string()),
                )?;
                Ok(source)
            }
        }
    }
}

/// Builds the editor's row list from a stored trigger. A fresh sidecar drives
/// the rows; an absent or stale one (the stored vectors were hand-edited)
/// degrades every row to Regex syntax showing the stored regexes verbatim.
pub fn trigger_rows(trigger: &triggers::TriggerDefinition) -> Vec<TriggerRow> {
    if let Some(sidecar) = &trigger.matchers {
        let fresh = matchers::trigger_patterns(sidecar).is_ok_and(|derived| {
            let stored = |v: &Option<Vec<String>>| v.clone().unwrap_or_default();
            derived.patterns == stored(&trigger.patterns)
                && derived.anti_patterns == stored(&trigger.anti_patterns)
                && derived.raw_patterns == stored(&trigger.raw_patterns)
        });
        if fresh {
            return sidecar
                .iter()
                .map(|m| TriggerRow {
                    role: PatternKind::from_role(m.role),
                    syntax: m.syntax,
                    source: m.source.clone(),
                    anchor_start: m.anchor_start,
                    anchor_end: m.anchor_end,
                    color: m.color.clone(),
                    color_channel: matchers::MatcherColorChannel::Foreground,
                    color_drafts: [
                        ChannelColorDraft::from_color(
                            m.color.as_ref().and_then(|filter| filter.foreground),
                        ),
                        ChannelColorDraft::from_color(
                            m.color.as_ref().and_then(|filter| filter.background),
                        ),
                    ],
                    remembered_color: None,
                })
                .collect();
        }
    }
    let mut rows = Vec::new();
    if let Some(patterns) = &trigger.patterns {
        rows.extend(patterns.iter().map(|p| TriggerRow {
            source: p.clone(),
            ..TriggerRow::new(PatternKind::Match)
        }));
    }
    if let Some(anti) = &trigger.anti_patterns {
        rows.extend(anti.iter().map(|p| TriggerRow {
            source: p.clone(),
            ..TriggerRow::new(PatternKind::Anti)
        }));
    }
    if let Some(raw) = &trigger.raw_patterns {
        rows.extend(raw.iter().map(|p| TriggerRow {
            source: p.clone(),
            ..TriggerRow::new(PatternKind::Raw)
        }));
    }
    // A trigger with no matchers opens at the unselected-cards state (README §4)
    // rather than with a blank row pre-created.
    rows
}

fn exact_truecolor_draft(color: Option<matchers::MatcherColor>) -> ExactTruecolorDraft {
    let Some(matchers::MatcherColor::Truecolor { r, g, b, .. }) = color else {
        return ExactTruecolorDraft::default();
    };
    ExactTruecolorDraft::from_rgb(r, g, b)
}

fn color_range_value(color: Option<matchers::MatcherColor>) -> matchers::MatcherHsvRange {
    let (r, g, b, range) = match color {
        Some(matchers::MatcherColor::Truecolor { r, g, b, range }) => (r, g, b, range),
        _ => (255, 255, 255, None),
    };
    let point = matchers::MatcherHsv::from_rgb(r, g, b);
    range
        .unwrap_or_else(|| matchers::MatcherHsvRange::from_to(point, point))
        .rgb_canonicalized()
}

fn color_range_hexes(range: matchers::MatcherHsvRange) -> [String; 2] {
    let (from, to) = range.directed_endpoints();
    let hex = |hsv: matchers::MatcherHsv| {
        let (r, g, b) = hsv.to_rgb();
        format!("#{r:02x}{g:02x}{b:02x}")
    };
    [hex(from), hex(to)]
}

/// Rebuilds a trigger's three pattern vectors (and its sidecar) from the row
/// list — the save path. The sidecar is written only when it carries real
/// authoring intent: a Pattern-syntax row, or a Raw row whose stored form
/// differs from what was typed (`\e` translation); pure hand-written-regex
/// triggers stay sidecar-free, exactly like files from older clients.
///
/// # Errors
///
/// Returns the first failing row's `(index, display error)`.
pub fn rows_into_trigger(
    rows: &[TriggerRow],
    trigger: &mut triggers::TriggerDefinition,
) -> Result<(), (usize, String)> {
    let mut patterns = Vec::new();
    let mut anti_patterns = Vec::new();
    let mut raw_patterns = Vec::new();
    let mut wants_sidecar = false;

    for (i, row) in rows.iter().enumerate() {
        if row.source.trim().is_empty() && row.color.is_none() {
            continue;
        }
        let compiled = row.compiled().map_err(|e| (i, e))?;
        wants_sidecar |=
            row.syntax == MatcherSyntax::Pattern || compiled != row.source || row.color.is_some();
        match row.role {
            PatternKind::Match => patterns.push(compiled),
            PatternKind::Anti => anti_patterns.push(compiled),
            PatternKind::Raw => raw_patterns.push(compiled),
        }
    }

    let some = |v: Vec<String>| if v.is_empty() { None } else { Some(v) };
    trigger.patterns = some(patterns);
    trigger.anti_patterns = some(anti_patterns);
    trigger.raw_patterns = some(raw_patterns);
    trigger.matchers = wants_sidecar.then(|| {
        rows.iter()
            .filter(|row| !row.source.trim().is_empty() || row.color.is_some())
            .map(|row| TriggerMatcherSource {
                role: row.role.role(),
                syntax: row.syntax,
                source: row.source.clone(),
                anchor_start: row.anchor_start,
                anchor_end: row.anchor_end,
                color: row.color.clone(),
            })
            .collect()
    });
    Ok(())
}

/// The client-side package dependency graph, derived from the lockfile plus
/// each installed package's resolved `dependencies`. Specifiers are the keys.
#[derive(Debug, Clone, Default)]
pub struct PackageGraph {
    /// `specifier -> (range, dep specifiers)` — a package's declared requires.
    pub requires: HashMap<String, Vec<DepEdge>>,
    /// Direct-install intent: the user installed it at top level.
    pub direct: HashSet<String>,
    /// Owned (authored) packages — always directly enabled (their own source).
    pub owned: HashSet<String>,
    /// The user's direct enable intent for controllable packages.
    pub intent: HashMap<String, bool>,
    /// Resolved version per specifier (best-effort, from the last resolve).
    pub resolved: HashMap<String, String>,
    /// Runtime-effective roots from the current lock snapshot. Catalog edges describe
    /// relationships; they cannot activate packages that were not committed to the lock.
    pub effective_roots: Option<HashSet<String>>,
    pub requirement_issues: HashMap<String, String>,
}

/// One edge in the requires graph: the dependency specifier + its declared range.
#[derive(Debug, Clone)]
pub struct DepEdge {
    pub specifier: String,
    pub range: String,
    pub kind: DependencyKind,
}

impl PackageGraph {
    /// Packages whose `requires` include `id`. Ordinary code dependencies do
    /// not force their target to run as a separate root.
    pub fn required_by(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .requires
            .iter()
            .filter(|(_, edges)| {
                edges
                    .iter()
                    .any(|edge| edge.specifier == id && edge.kind == DependencyKind::Requires)
            })
            .map(|(parent, _)| parent.clone())
            .collect();
        out.sort();
        out
    }

    /// Packages with any declared relationship to `id`, including imported
    /// code dependencies and separately-running `requires` roots.
    fn parents_of(&self, id: &str) -> Vec<String> {
        let mut out = self
            .requires
            .iter()
            .filter(|(_, edges)| edges.iter().any(|edge| edge.specifier == id))
            .map(|(parent, _)| parent.clone())
            .collect::<Vec<_>>();
        out.sort();
        out
    }

    /// An import-only package: not directly installed or owned, and reached only through ordinary
    /// code-dependency edges.
    pub fn is_dep_only(&self, id: &str) -> bool {
        !self.direct.contains(id)
            && !self.owned.contains(id)
            && self.required_by(id).is_empty()
            && !self.parents_of(id).is_empty()
    }

    /// A package runs as a root when its direct intent is active or any recursively active parent
    /// declares it through `requires`. Ordinary dependencies execute inside their parent's isolate
    /// and do not become roots here.
    pub fn effectively_enabled(&self, id: &str) -> bool {
        if let Some(roots) = &self.effective_roots {
            return roots.contains(id);
        }
        fn visit(graph: &PackageGraph, id: &str, visiting: &mut HashSet<String>) -> bool {
            if (graph.direct.contains(id) || graph.owned.contains(id))
                && graph.intent.get(id).copied().unwrap_or(false)
            {
                return true;
            }
            if !visiting.insert(id.to_string()) {
                return false;
            }
            let enabled = graph
                .required_by(id)
                .iter()
                .any(|parent| visit(graph, parent, visiting));
            visiting.remove(id);
            enabled
        }

        visit(self, id, &mut HashSet::new())
    }

    /// An automatically installed package whose only ownership is `required_by` has no direct
    /// activation control. Explicit installs and authored packages do.
    pub fn controllable(&self, id: &str) -> bool {
        self.direct.contains(id) || self.owned.contains(id)
    }

    /// Enabled parents that currently cause `id` to run (for the "Required by …" note).
    pub fn enabled_dependents(&self, id: &str) -> Vec<String> {
        self.required_by(id)
            .into_iter()
            .filter(|parent| self.effectively_enabled(parent))
            .collect()
    }

    /// Whether a dependency row shown UNDER `parent` should read as live. Such a row
    /// exists because `parent` pulls `child` in, so it follows the parent's context:
    /// it greys once `parent` is no longer effectively enabled, rather than reporting
    /// `child`'s global state (which stays on for a separately-installed `child` that
    /// runs on its own — that belongs to `child`'s own row, not this edge). The
    /// operative term is the parent for both ordinary imports and `requires`: an active requiring
    /// parent is what activates an automatically installed required root.
    pub fn dep_edge_active(&self, parent: &str, child: &str) -> bool {
        let Some(edge) = self
            .requires
            .get(parent)
            .and_then(|edges| edges.iter().find(|edge| edge.specifier == child))
        else {
            return false;
        };
        self.effectively_enabled(parent)
            && (edge.kind == DependencyKind::Dependency || self.effectively_enabled(child))
    }
}

impl AutomationsWindow {
    /// Loads aliases/triggers/hotkeys into the nested script tree.
    pub(super) fn load_scripts_message(&self) -> Message {
        let snapshot = match automation_transaction::load(&self.server_name) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Message::ScriptsLoaded {
                    scripts: BTreeMap::new(),
                    load: None,
                    errors: Arc::new(vec![error.to_string()]),
                };
            }
        };

        let aliases = snapshot
            .aliases
            .clone()
            .into_iter()
            .map(|(name, alias)| (name, Script::Alias(alias)));
        let hotkeys = snapshot
            .hotkeys
            .clone()
            .into_iter()
            .map(|(name, hotkey)| (name, Script::Hotkey(hotkey)));
        // Inner triggers are saved nested under their outer; the tree holds them as leaves
        // beside it, each carrying `outer` and the root's folder, and re-nests them on save.
        let triggers = flatten_inner_triggers(snapshot.triggers.clone())
            .into_iter()
            .map(|(name, trigger)| (name, Script::Trigger(trigger)));

        let combined: Vec<(String, Script)> =
            aliases.into_iter().chain(hotkeys).chain(triggers).collect();

        let mut scripts = BTreeMap::new();
        for (name, script) in combined {
            match upsert_script_folder(&mut scripts, script.folder_name()) {
                Ok(folder) => {
                    folder.insert(name, script);
                }
                Err(error) => {
                    return Message::ScriptsLoaded {
                        scripts: BTreeMap::new(),
                        load: None,
                        errors: Arc::new(vec![error]),
                    };
                }
            }
        }
        Message::ScriptsLoaded {
            scripts,
            load: Some(snapshot),
            errors: Arc::new(Vec::new()),
        }
    }

    /// Adds the folder-tree's folders (incl. empty ones) into the script map so
    /// they render with no scripts inside. Idempotent.
    pub(super) fn merge_folders(&mut self) {
        fn collect_leaves(scripts: &BTreeMap<String, Script>, leaves: &mut Vec<(String, Script)>) {
            for (name, script) in scripts {
                match script {
                    Script::Folder(_, children) => collect_leaves(children, leaves),
                    script => leaves.push((name.clone(), script.clone())),
                }
            }
        }

        // Rewrite only unambiguous case variants to the packages.json spelling. Exact legacy
        // case-only siblings remain distinct; a lone `combat` script under stored `Combat` is
        // canonicalized so the sidebar, editor, and runtime all address the same folder.
        let mut leaves = Vec::new();
        collect_leaves(&self.scripts, &mut leaves);
        let mut scripts = BTreeMap::new();
        for (name, mut script) in leaves {
            if let Some(folder) = script.folder_name()
                && let Some(canonical) = packages::canonical_folder_path(&self.packages, folder)
            {
                script.set_folder_name(Some(canonical));
            }
            if let Ok(folder) = upsert_script_folder(&mut scripts, script.folder_name()) {
                folder.insert(name, script);
            }
        }
        self.scripts = scripts;
        for path in packages::collect_folder_paths(&self.packages) {
            let _ = upsert_script_folder(&mut self.scripts, Some(&path));
        }
    }

    pub(super) fn serialize_scripts(
        &mut self,
    ) -> Result<AutomationSaveStatus, Box<dyn std::error::Error>> {
        if self.folder_state_error.is_some() {
            return Err(crate::i18n::t!("automation-folder-state-unavailable").into());
        }
        let expected = self
            .automation_snapshot
            .clone()
            .ok_or_else(|| crate::i18n::t!("automation-state-baseline-unavailable"))?;
        let mut aliases_map = std::collections::HashMap::new();
        let mut hotkeys_map = std::collections::HashMap::new();
        let mut triggers_map = std::collections::HashMap::new();
        collect_scripts(
            &self.scripts,
            &mut aliases_map,
            &mut hotkeys_map,
            &mut triggers_map,
        );
        let snapshot = automation_transaction::AutomationStateSnapshot::new(
            self.packages.clone(),
            aliases_map,
            hotkeys_map,
            triggers_map,
        );
        let outcome =
            automation_transaction::commit_if_unchanged(&self.server_name, &expected, &snapshot)
                .map_err(|error| {
                    crate::i18n::t!(
                        "automation-save-state-failed",
                        "error" => error.to_string()
                    )
                })?;
        match outcome {
            automation_transaction::CommitOutcome::Applied => {
                self.automation_snapshot = Some(snapshot);
                Ok(AutomationSaveStatus::Saved)
            }
            automation_transaction::CommitOutcome::Conflict => Ok(AutomationSaveStatus::Conflict),
        }
    }

    /// Present a save that lost its compare-and-set race: nothing was written, so tell the user
    /// and reload the authoritative tree. Callers restore their in-memory edit first.
    pub(super) fn automation_save_conflict_task(&mut self) -> iced::Task<Message> {
        iced::Task::batch([
            self.show_toast(crate::i18n::t!("automation-save-state-conflict")),
            iced::Task::done(self.load_scripts_message()),
        ])
    }

    pub(super) fn automation_save_conflict(&mut self) -> Update<Message, Event> {
        Update::with_task(self.automation_save_conflict_task())
    }

    pub(super) fn script_exists(&self, name: &str) -> bool {
        fn rec(scripts: &BTreeMap<String, Script>, name: &str) -> bool {
            for (script_name, script) in scripts {
                // Case-insensitive: these names become files on disk, and
                // Windows/macOS filesystems treat `Combat` and `combat` as one.
                if models::naming::names_conflict(script_name, name) {
                    return true;
                }
                if let Script::Folder(_, children) = script
                    && rec(children, name)
                {
                    return true;
                }
            }
            false
        }
        rec(&self.scripts, name)
    }

    pub(super) fn remove_script_by_name(&mut self, name: &str) {
        fn rec(scripts: &mut BTreeMap<String, Script>, name: &str) -> bool {
            if scripts.remove(name).is_some() {
                return true;
            }
            for script in scripts.values_mut() {
                if let Script::Folder(_, children) = script
                    && rec(children, name)
                {
                    return true;
                }
            }
            false
        }
        rec(&mut self.scripts, name);
    }

    /// Looks up a leaf script by its (folder, name) key.
    pub(super) fn find_script(&self, key: &ScriptKey) -> Option<Script> {
        fn rec(scripts: &BTreeMap<String, Script>, name: &str) -> Option<Script> {
            for (script_name, script) in scripts {
                if script_name == name && !matches!(script, Script::Folder(_, _)) {
                    return Some(script.clone());
                }
                if let Script::Folder(_, children) = script
                    && let Some(found) = rec(children, name)
                {
                    return Some(found);
                }
            }
            None
        }
        rec(&self.scripts, &key.script_name)
    }

    /// Every folder path in the tree (each `Script::Folder`, nested as a
    /// `/`-joined path), sorted with parents before children. This is the set of
    /// destinations the "move to folder" affordances offer — drawn from the live
    /// script tree (not `packages.json`) so it includes folders that exist only
    /// because a script's `package` field points at them.
    pub(super) fn all_folder_paths(&self) -> Vec<String> {
        fn rec(scripts: &BTreeMap<String, Script>, prefix: &str, out: &mut Vec<String>) {
            for (name, script) in scripts {
                if let Script::Folder(_, children) = script {
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    out.push(path.clone());
                    rec(children, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.scripts, "", &mut out);
        out.sort();
        out
    }

    /// The effective status of a leaf script (its own enable + folder enable + a
    /// compile error). Folders/modules report Ok unless disabled.
    pub(super) fn script_status(&self, script: &Script) -> NodeStatus {
        let folder_enabled = script.folder_name().is_none_or(|path| {
            packages::is_package_effectively_enabled_for(path, &self.packages, &self.profile_name)
        });
        if !script.own_enabled() || !folder_enabled {
            return NodeStatus::Disabled;
        }
        if let Script::Trigger(t) = script {
            // The reach cannot cover anything: a saved trigger that never fires.
            if t.outer.is_some() && !t.reach.can_fire() {
                return NodeStatus::Error;
            }
            if self
                .outer_chain(t.outer.as_deref())
                .iter()
                .any(|outer| !outer.enabled)
            {
                return NodeStatus::Disabled;
            }
        }
        if script_has_error(script) {
            return NodeStatus::Error;
        }
        NodeStatus::Ok
    }

    /// The stored definition of the trigger named `name`, wherever it sits in the tree.
    pub(super) fn trigger_definition(
        &self,
        name: &str,
    ) -> Option<models::triggers::TriggerDefinition> {
        match self.find_script(&ScriptKey {
            folder_name: None,
            script_name: name.to_string(),
        }) {
            Some(Script::Trigger(t)) => Some(t),
            _ => None,
        }
    }

    /// The triggers `name` is inside, nearest first, starting from `outer` (a trigger's
    /// own `outer` field). Stops at a missing link or the depth limit.
    pub(super) fn outer_chain(
        &self,
        outer: Option<&str>,
    ) -> Vec<models::triggers::TriggerDefinition> {
        let mut chain = Vec::new();
        let mut at = outer.map(str::to_string);
        while let Some(name) = at.take() {
            let Some(definition) = self.trigger_definition(&name) else {
                break;
            };
            at = definition.outer.clone();
            chain.push(definition);
            if chain.len() > models::triggers::MAX_INNER_DEPTH {
                break;
            }
        }
        chain
    }

    /// The names of the triggers this one is inside, nearest first.
    pub(super) fn outer_names(&self, outer: Option<&str>) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = outer.map(str::to_string);
        while let Some(name) = at.take() {
            at = self.trigger_definition(&name).and_then(|t| t.outer);
            names.push(name);
            if names.len() > models::triggers::MAX_INNER_DEPTH {
                break;
            }
        }
        names
    }

    /// The triggers directly inside `name`, in dispatch order: priority first, then name.
    pub(super) fn inner_triggers_of(
        &self,
        name: &str,
    ) -> Vec<(String, models::triggers::TriggerDefinition)> {
        fn rec(
            scripts: &BTreeMap<String, Script>,
            outer: &str,
            out: &mut Vec<(String, models::triggers::TriggerDefinition)>,
        ) {
            for (script_name, script) in scripts {
                match script {
                    Script::Trigger(t) if t.outer.as_deref() == Some(outer) => {
                        out.push((script_name.clone(), t.clone()));
                    }
                    Script::Folder(_, children) => rec(children, outer, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.scripts, name, &mut out);
        out.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Every trigger inside `name`, at any depth.
    pub(super) fn triggers_inside(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![name.to_string()];
        while let Some(outer) = pending.pop() {
            for (inner, _) in self.inner_triggers_of(&outer) {
                if !out.contains(&inner) {
                    out.push(inner.clone());
                    pending.push(inner);
                }
            }
        }
        out
    }

    /// Update every trigger directly inside `old` to sit inside `new`, and give the whole
    /// subtree the folder `package`: what a rename or a folder move of an outer needs.
    pub(super) fn retarget_inner_triggers(&mut self, old: &str, new: &str, package: Option<&str>) {
        let inside = self.triggers_inside(old);
        fn rec(
            scripts: &mut BTreeMap<String, Script>,
            old: &str,
            new: &str,
            inside: &[String],
            package: Option<&str>,
        ) {
            for (script_name, script) in scripts.iter_mut() {
                match script {
                    Script::Trigger(t) if inside.contains(script_name) => {
                        if t.outer.as_deref() == Some(old) {
                            t.outer = Some(new.to_string());
                        }
                        t.package = package.map(str::to_string);
                    }
                    Script::Folder(_, children) => rec(children, old, new, inside, package),
                    _ => {}
                }
            }
        }
        rec(&mut self.scripts, old, new, &inside, package);
        // Folder placement lives in the tree structure too: re-home the moved subtree.
        for name in inside {
            let key = ScriptKey {
                folder_name: None,
                script_name: name.clone(),
            };
            if let Some(script) = self.find_script(&key) {
                self.remove_script_by_name(&name);
                if let Ok(folder) = upsert_script_folder(&mut self.scripts, script.folder_name()) {
                    folder.insert(name, script);
                }
            }
        }
    }
}

/// Whether a script carries an obvious compile error (a regex that won't build).
pub fn script_has_error(script: &Script) -> bool {
    match script {
        Script::Alias(a) if a.language != models::ScriptLang::Plaintext => false,
        Script::Alias(a) => regex::Regex::new(&a.pattern).is_err() && !a.pattern.is_empty(),
        Script::Trigger(t) => {
            let bad = |v: &Option<Vec<String>>| {
                v.as_ref().is_some_and(|patterns| {
                    patterns
                        .iter()
                        .any(|p| !p.is_empty() && regex::Regex::new(p).is_err())
                })
            };
            bad(&t.patterns) || bad(&t.anti_patterns) || bad(&t.raw_patterns)
        }
        Script::Hotkey(_) | Script::Folder(_, _) => false,
    }
}

/// Walks the tree collecting leaves of each type into flat maps for serialization. Inner
/// triggers come out nested under their outer, the shape the file keeps.
fn collect_scripts(
    scripts: &BTreeMap<String, Script>,
    aliases: &mut std::collections::HashMap<String, models::aliases::AliasDefinition>,
    hotkeys: &mut std::collections::HashMap<String, models::hotkeys::HotkeyDefinition>,
    triggers: &mut std::collections::HashMap<String, models::triggers::TriggerDefinition>,
) {
    fn walk(
        scripts: &BTreeMap<String, Script>,
        aliases: &mut std::collections::HashMap<String, models::aliases::AliasDefinition>,
        hotkeys: &mut std::collections::HashMap<String, models::hotkeys::HotkeyDefinition>,
        triggers: &mut std::collections::HashMap<String, models::triggers::TriggerDefinition>,
    ) {
        for (name, script) in scripts {
            match script {
                Script::Alias(a) => {
                    aliases.insert(name.clone(), a.clone());
                }
                Script::Hotkey(h) => {
                    hotkeys.insert(name.clone(), h.clone());
                }
                Script::Trigger(t) => {
                    triggers.insert(name.clone(), t.clone());
                }
                Script::Folder(_, children) => walk(children, aliases, hotkeys, triggers),
            }
        }
    }
    walk(scripts, aliases, hotkeys, triggers);
    let flat = std::mem::take(triggers);
    *triggers = nest_inner_triggers(flat);
}

/// Unpack every trigger's `inner` map into flat entries beside it: each inner trigger
/// carries `outer` and the root's folder, so the folder tree places it where its root is.
pub fn flatten_inner_triggers(
    triggers: std::collections::HashMap<String, models::triggers::TriggerDefinition>,
) -> std::collections::HashMap<String, models::triggers::TriggerDefinition> {
    fn visit(
        name: String,
        mut definition: models::triggers::TriggerDefinition,
        outer: Option<&str>,
        package: Option<&str>,
        out: &mut std::collections::HashMap<String, models::triggers::TriggerDefinition>,
    ) {
        let inner = definition.inner.take();
        if outer.is_some() {
            definition.outer = outer.map(str::to_string);
            definition.package = package.map(str::to_string);
        }
        let package = definition.package.clone();
        // A name used twice keeps the entry seen first; the loader drops the same one.
        if out.contains_key(&name) {
            return;
        }
        out.insert(name.clone(), definition);
        for (inner_name, inner_definition) in inner.into_iter().flatten() {
            visit(
                inner_name,
                inner_definition,
                Some(&name),
                package.as_deref(),
                out,
            );
        }
    }
    let mut out = std::collections::HashMap::new();
    for (name, definition) in triggers {
        visit(name, definition, None, None, &mut out);
    }
    out
}

/// The inverse of [`flatten_inner_triggers`]: move every entry with an `outer` into that
/// trigger's `inner` map, innermost first, clearing the runtime-only fields the file does
/// not keep. An entry whose outer is missing stays top-level rather than vanishing.
pub fn nest_inner_triggers(
    mut flat: std::collections::HashMap<String, models::triggers::TriggerDefinition>,
) -> std::collections::HashMap<String, models::triggers::TriggerDefinition> {
    fn depth(
        flat: &std::collections::HashMap<String, models::triggers::TriggerDefinition>,
        name: &str,
    ) -> usize {
        let mut depth = 0;
        let mut at = flat.get(name).and_then(|t| t.outer.as_deref());
        while let Some(outer) = at {
            depth += 1;
            if depth > models::triggers::MAX_INNER_DEPTH + 1 {
                break;
            }
            at = flat.get(outer).and_then(|t| t.outer.as_deref());
        }
        depth
    }
    let mut order: Vec<(usize, String)> = flat
        .iter()
        .filter(|(_, t)| t.outer.is_some())
        .map(|(name, _)| (depth(&flat, name), name.clone()))
        .collect();
    // Deepest first, so a moved entry already holds its own inner map.
    order.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, name) in order {
        let Some(mut definition) = flat.remove(&name) else {
            continue;
        };
        let outer = definition.outer.take().unwrap_or_default();
        definition.package = None;
        match flat.get_mut(&outer) {
            Some(outer_definition) => {
                outer_definition
                    .inner
                    .get_or_insert_with(BTreeMap::new)
                    .insert(name, definition);
            }
            None => {
                flat.insert(name, definition);
            }
        }
    }
    flat
}

/// Ensures the folder chain `folder_name` exists in `scripts`, returning the
/// innermost folder's child map.
pub fn upsert_script_folder<'a>(
    scripts: &'a mut BTreeMap<String, Script>,
    folder_name: Option<&str>,
) -> Result<&'a mut BTreeMap<String, Script>, String> {
    let mut current = scripts;
    if let Some(folder_name) = folder_name {
        for (i, folder) in folder_name.split('/').enumerate() {
            match current.get(folder) {
                Some(Script::Folder(_, _)) => {}
                Some(_) => {
                    return Err(crate::i18n::t!(
                        "automation-script-folder-invalid",
                        "path" => folder_name.split('/').take(i).collect::<Vec<_>>().join("/")
                    ));
                }
                None => {
                    current.insert(folder.to_string(), Script::Folder(false, BTreeMap::new()));
                }
            }
            current = match current.get_mut(folder) {
                Some(Script::Folder(_, children)) => children,
                _ => return Err(crate::i18n::t!("automation-script-folder-create-failed")),
            };
        }
    }
    Ok(current)
}

/// Parses a `smudgy://owner/name` specifier into `(owner, name)`.
pub fn parse_specifier(specifier: &str) -> Option<(String, String)> {
    let rest = specifier.strip_prefix("smudgy://")?;
    let (owner, name) = rest.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// A short display label for an installed-package specifier (the trailing name).
pub fn package_display_name(specifier: &str) -> &str {
    specifier.rsplit('/').next().unwrap_or(specifier)
}

/// The specifier a `LockedPackage` would carry for a given owner/name.
pub fn specifier_for(owner: &str, name: &str) -> String {
    format!("smudgy://{owner}/{name}")
}

/// Whether `owner/name` is present in the lockfile list.
pub fn is_installed(installed: &[LockedPackage], owner: &str, name: &str) -> bool {
    let specifier = specifier_for(owner, name);
    installed.iter().any(|p| p.specifier == specifier)
}

/// Required-parameter completeness per profile for the open profile-scoped package. Computing
/// it reads the value store and the keyring under the server state lock, so it lives here rather
/// than in the Settings tab's `view`, which only reads it. Kept current by
/// [`AutomationsWindow::sync_profile_param_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileParamStatus {
    /// The package the entries describe (`smudgy://owner/name`).
    pub specifier: String,
    /// The declared parameter keys the entries were computed for. A manifest save that adds or
    /// removes a parameter re-seeds the editor with a different list and invalidates the cache.
    pub param_keys: Vec<String>,
    /// Missing required keys per profile, in profile inventory order.
    pub missing: Vec<(String, Vec<String>)>,
}

impl ProfileParamStatus {
    /// The required keys `profile` still lacks, or `None` when the profile was not in the
    /// inventory the entries were computed for.
    #[must_use]
    pub fn missing_for(&self, profile: &str) -> Option<&[String]> {
        self.missing
            .iter()
            .find(|(name, _)| name == profile)
            .map(|(_, missing)| missing.as_slice())
    }

    fn matches(&self, config: &super::packages::ParamConfig, profile_names: &[String]) -> bool {
        self.specifier == config.specifier
            && self
                .param_keys
                .iter()
                .eq(config.params.iter().map(|param| &param.key))
            && self.missing.len() == profile_names.len()
            && self
                .missing
                .iter()
                .zip(profile_names)
                .all(|((name, _), profile)| name == profile)
    }
}

impl AutomationsWindow {
    /// Keeps [`Self::profile_param_status`] aligned with the open parameter editor. Runs after
    /// every update: the cache is dropped when no available profile-scoped editor is open,
    /// recomputed when the editor's package, parameter list, or the profile inventory changed
    /// (a package opening, a manifest save, a reload), and recomputed unconditionally when
    /// `values_written` reports a message that stored or cleared parameter values without changing
    /// that identity.
    pub(super) fn sync_profile_param_status(&mut self, values_written: bool) {
        let Some(config) = self.param_config.as_ref().filter(|config| {
            config.available
                && config.parameter_scope
                    == smudgy_core::models::shared_packages::ParameterScope::Profile
        }) else {
            self.profile_param_status = None;
            return;
        };
        let current = self
            .profile_param_status
            .as_ref()
            .is_some_and(|status| status.matches(config, &self.profile_names));
        if current && !values_written {
            return;
        }
        let missing = self
            .profile_names
            .iter()
            .map(|profile| {
                let missing = smudgy_core::models::shared_packages::missing_required_params_scoped(
                    &self.server_name,
                    smudgy_core::models::shared_packages::ParamValueScope::Profile(profile),
                    &config.specifier,
                    &config.params,
                );
                (profile.clone(), missing)
            })
            .collect();
        self.profile_param_status = Some(ProfileParamStatus {
            specifier: config.specifier.clone(),
            param_keys: config
                .params
                .iter()
                .map(|param| param.key.clone())
                .collect(),
            missing,
        });
    }

    /// Messages whose handling can store or clear parameter values for the open package while
    /// leaving the editor's identity unchanged, so the completeness cache must be recomputed.
    pub(super) fn writes_parameter_values(message: &Message) -> bool {
        matches!(
            message,
            Message::ParamConfigSave
                | Message::ParamConfigClearSecret(_)
                | Message::SetParameterScope(_)
                | Message::ConfirmGlobalParameterSource
                | Message::ConfirmCopySettings
                | Message::SelectParameterProfile(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{DepEdge, PackageGraph, Script, upsert_script_folder};

    #[test]
    fn script_folder_tree_preserves_legacy_case_variants() {
        let mut scripts = std::collections::BTreeMap::new();
        upsert_script_folder(&mut scripts, Some("combat/healing")).unwrap();
        upsert_script_folder(&mut scripts, Some("Combat/Healing")).unwrap();

        assert_eq!(scripts.len(), 2);
        assert!(matches!(scripts.get("combat"), Some(Script::Folder(_, _))));
        assert!(matches!(scripts.get("Combat"), Some(Script::Folder(_, _))));
    }

    mod matcher_drafts {
        use super::super::*;

        fn alias(pattern: &str, matcher: Option<AliasMatcherSource>) -> aliases::AliasDefinition {
            aliases::AliasDefinition {
                pattern: pattern.to_string(),
                script: None,
                package: None,
                enabled: true,
                priority: 0,
                fallthrough: true,
                allow_self_match: false,
                language: smudgy_core::models::ScriptLang::Plaintext,
                matcher,
                state: Vec::new(),
            }
        }

        #[test]
        fn fresh_pattern_sidecar_drives_the_draft_and_round_trips() {
            let source = AliasMatcherSource::Pattern {
                source: "greet {person}".to_string(),
                anchor_start: true,
                anchor_end: true,
            };
            let stored = matchers::alias_pattern(&source, "greet").unwrap();
            let draft =
                AliasMatcherDraft::from_definition(&alias(&stored, Some(source.clone())), "greet");
            assert_eq!(draft.kind, AliasKind::Pattern);
            assert_eq!(draft.pattern_source, "greet {person}");
            assert!(!draft.degraded);
            // save(compile(sidecar)) == stored, and the sidecar re-emerges.
            assert_eq!(draft.to_pattern("greet").unwrap(), stored);
            assert_eq!(draft.to_matcher(), Some(source));
        }

        #[test]
        fn stale_sidecar_degrades_to_regex_showing_the_hand_edit() {
            let source = AliasMatcherSource::Pattern {
                source: "greet {person}".to_string(),
                anchor_start: true,
                anchor_end: true,
            };
            let draft = AliasMatcherDraft::from_definition(
                &alias(r"^greetz\s+(.*)$", Some(source)),
                "greet",
            );
            assert_eq!(draft.kind, AliasKind::Regex);
            assert!(draft.degraded);
            assert_eq!(draft.regex_source, r"^greetz\s+(.*)$");
            // Saving keeps the hand edit and drops the lying sidecar.
            assert_eq!(draft.to_pattern("greet").unwrap(), r"^greetz\s+(.*)$");
            assert_eq!(draft.to_matcher(), None);
        }

        #[test]
        fn the_alias_name_is_the_command_until_an_override_is_pinned() {
            let draft = AliasMatcherDraft {
                kind: AliasKind::Command,
                ..AliasMatcherDraft::default()
            };
            // Renaming the alias renames the command, prefilter and all.
            assert_eq!(draft.command_word("obe"), "obe");
            assert_eq!(draft.to_pattern("obe").unwrap(), r"^obe(?:\s|$)");
            assert_eq!(draft.to_pattern("bash").unwrap(), r"^bash(?:\s|$)");
            assert!(matches!(
                draft.to_matcher(),
                Some(AliasMatcherSource::Command { name: None, .. })
            ));

            // A saved override (legacy data) still wins until the name is
            // edited again.
            let pinned = AliasMatcherDraft {
                command_override: Some("*".to_string()),
                ..draft.clone()
            };
            assert_eq!(pinned.command_word("star-emote"), "*");
            assert_eq!(pinned.to_pattern("star-emote").unwrap(), r"^\*(?:\s|$)");

            // A blank override is no override: it still inherits, and it
            // still saves as an absent field.
            let blank = AliasMatcherDraft {
                command_override: Some("  ".to_string()),
                ..draft.clone()
            };
            assert_eq!(blank.command_word("obe"), "obe");
            assert!(matches!(
                blank.to_matcher(),
                Some(AliasMatcherSource::Command { name: None, .. })
            ));
        }

        #[test]
        fn a_name_that_cannot_be_a_command_word_blocks_the_save() {
            let draft = AliasMatcherDraft {
                kind: AliasKind::Command,
                ..AliasMatcherDraft::default()
            };
            // Names may contain spaces; command words are one token, so the
            // save refuses until the alias gets a one-word name.
            assert!(draft.to_pattern("guild tell").is_err());
            assert!(draft.to_pattern("   ").is_err());

            // A legacy saved override still rescues such a name until the name
            // field is next edited (which clears the override).
            let rescued = AliasMatcherDraft {
                command_override: Some("gt".to_string()),
                ..draft
            };
            assert_eq!(rescued.to_pattern("guild tell").unwrap(), r"^gt(?:\s|$)");
        }

        #[test]
        fn simple_mode_normalizes_args_to_optional_then_rest() {
            let mut draft = AliasMatcherDraft {
                kind: AliasKind::Command,
                command_override: None,
                args: vec![
                    ArgSpec {
                        name: "a".to_string(),
                        kind: ArgKind::Required,
                    },
                    ArgSpec {
                        name: "b".to_string(),
                        kind: ArgKind::Required,
                    },
                ],
                ..AliasMatcherDraft::default()
            };
            draft.normalize_args();
            assert_eq!(draft.args[0].kind, ArgKind::Optional);
            assert_eq!(draft.args[1].kind, ArgKind::Rest);

            // Advanced mode: only the rest-only-last invariant applies.
            draft.cmd_mode = CmdMode::Advanced;
            draft.args[0].kind = ArgKind::Rest;
            draft.normalize_args();
            assert_eq!(draft.args[0].kind, ArgKind::Optional);
            assert_eq!(draft.args[1].kind, ArgKind::Rest);
        }

        #[test]
        fn trigger_rows_round_trip_through_the_sidecar() {
            let rows = vec![
                TriggerRow {
                    role: PatternKind::Match,
                    syntax: MatcherSyntax::Pattern,
                    source: "You are {state}.".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    ..TriggerRow::new(PatternKind::Match)
                },
                TriggerRow {
                    role: PatternKind::Raw,
                    syntax: MatcherSyntax::Regex,
                    source: r"\e\[31m".to_string(),
                    anchor_start: true,
                    anchor_end: true,
                    ..TriggerRow::new(PatternKind::Raw)
                },
            ];
            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&rows, &mut trigger).unwrap();
            assert_eq!(
                trigger.patterns.as_deref(),
                Some(&[r"^You\s+are\s+(?<state>.*?)\.$".to_string()][..])
            );
            // The raw row stores the translated form; the sidecar keeps `\e`.
            assert_eq!(
                trigger.raw_patterns.as_deref(),
                Some(&[r"\x1b\[31m".to_string()][..])
            );
            assert!(trigger.matchers.is_some());
            assert_eq!(trigger_rows(&trigger), rows);
        }

        #[test]
        fn pure_regex_rows_stay_sidecar_free() {
            let rows = vec![TriggerRow {
                source: "^ready$".to_string(),
                ..TriggerRow::new(PatternKind::Match)
            }];
            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&rows, &mut trigger).unwrap();
            assert!(
                trigger.matchers.is_none(),
                "no authoring intent, no sidecar"
            );
            assert_eq!(
                trigger.patterns.as_deref(),
                Some(&["^ready$".to_string()][..])
            );
        }

        #[test]
        fn blank_color_row_is_persisted_as_a_color_only_matcher() {
            let rows = vec![TriggerRow {
                color: Some(matchers::MatcherColorMatch {
                    foreground: Some(matchers::MatcherColor::Ansi { index: 1 }),
                    ..Default::default()
                }),
                ..TriggerRow::new(PatternKind::Match)
            }];
            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&rows, &mut trigger).unwrap();
            assert_eq!(trigger.patterns.as_deref(), Some(&[String::new()][..]));
            assert!(trigger.matchers.is_some());
            assert_eq!(trigger_rows(&trigger), rows);
        }

        #[test]
        fn color_toggle_restores_the_filter_without_persisting_editor_memory() {
            let filter = matchers::MatcherColorMatch {
                foreground: Some(matchers::MatcherColor::Ansi { index: 2 }),
                background: Some(matchers::MatcherColor::Xterm { index: 196 }),
                attributes: vec![
                    matchers::MatcherTextAttribute::Bold,
                    matchers::MatcherTextAttribute::Italic,
                ],
            };
            let mut row = TriggerRow {
                source: "ready".to_string(),
                color: Some(filter.clone()),
                ..TriggerRow::new(PatternKind::Match)
            };

            row.set_color_enabled(false);
            assert!(row.color.is_none());
            assert_eq!(row.remembered_color.as_ref(), Some(&filter));

            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&[row.clone()], &mut trigger).unwrap();
            assert!(
                trigger.matchers.is_none(),
                "transient toggle memory must not force a persisted sidecar"
            );
            assert_eq!(
                trigger.patterns.as_deref(),
                Some(&["ready".to_string()][..])
            );

            row.set_color_enabled(true);
            assert_eq!(row.color.as_ref(), Some(&filter));
            assert!(row.remembered_color.is_none());
            rows_into_trigger(&[row.clone()], &mut trigger).unwrap();
            assert_eq!(trigger_rows(&trigger), vec![row]);
        }

        #[test]
        fn first_color_enable_keeps_the_ansi_white_default() {
            let mut row = TriggerRow::new(PatternKind::Match);
            row.set_color_enabled(true);
            assert_eq!(
                row.color,
                Some(matchers::MatcherColorMatch {
                    foreground: Some(matchers::MatcherColor::Ansi { index: 7 }),
                    ..Default::default()
                })
            );
        }

        #[test]
        fn color_only_row_requires_a_surviving_constraint() {
            let mut row = TriggerRow {
                color: Some(matchers::MatcherColorMatch::default()),
                ..TriggerRow::new(PatternKind::Match)
            };
            assert_eq!(
                row.compiled().unwrap_err(),
                crate::i18n::t!("editor-color-needs-constraint")
            );

            let mut trigger = triggers::TriggerDefinition::default();
            let (index, error) = rows_into_trigger(&[row.clone()], &mut trigger).unwrap_err();
            assert_eq!(index, 0);
            assert_eq!(error, crate::i18n::t!("editor-color-needs-constraint"));

            row.color
                .as_mut()
                .unwrap()
                .attributes
                .push(matchers::MatcherTextAttribute::Bold);
            assert_eq!(row.compiled().unwrap(), "");
            rows_into_trigger(&[row.clone()], &mut trigger).unwrap();
            assert_eq!(trigger.patterns.as_deref(), Some(&[String::new()][..]));
            assert_eq!(trigger_rows(&trigger), vec![row]);
        }

        #[test]
        fn whitespace_colored_regex_stays_verbatim_and_keeps_a_fresh_sidecar() {
            let rows = vec![TriggerRow {
                source: " ".to_string(),
                color: Some(matchers::MatcherColorMatch {
                    foreground: Some(matchers::MatcherColor::Ansi { index: 1 }),
                    ..Default::default()
                }),
                ..TriggerRow::new(PatternKind::Match)
            }];
            let mut trigger = triggers::TriggerDefinition::default();

            rows_into_trigger(&rows, &mut trigger).unwrap();

            assert_eq!(trigger.patterns.as_deref(), Some(&[" ".to_string()][..]));
            let sidecar = trigger.matchers.as_ref().expect("colored regex sidecar");
            assert_eq!(sidecar[0].syntax, MatcherSyntax::Regex);
            assert_eq!(sidecar[0].source, " ");
            let derived = matchers::trigger_patterns(sidecar).expect("fresh sidecar");
            assert_eq!(
                derived.patterns,
                trigger.patterns.clone().unwrap_or_default()
            );
            assert_eq!(
                derived.anti_patterns,
                trigger.anti_patterns.clone().unwrap_or_default()
            );
            assert_eq!(
                derived.raw_patterns,
                trigger.raw_patterns.clone().unwrap_or_default()
            );
            assert_eq!(trigger_rows(&trigger), rows);
        }

        #[test]
        fn truecolor_range_round_trips_with_both_endpoint_buffers() {
            let range = matchers::MatcherHsvRange::from_to(
                matchers::MatcherHsv {
                    hue: 12,
                    saturation: 210,
                    value: 180,
                },
                matchers::MatcherHsv {
                    hue: 42,
                    saturation: 120,
                    value: 240,
                },
            );
            let (r, g, b) = range.first.to_rgb();
            let endpoint_hex = |endpoint: matchers::MatcherHsv| {
                let (r, g, b) = endpoint.to_rgb();
                format!("#{r:02x}{g:02x}{b:02x}")
            };
            let rows = vec![TriggerRow {
                source: "target".to_string(),
                color: Some(matchers::MatcherColorMatch {
                    foreground: Some(matchers::MatcherColor::Truecolor {
                        r,
                        g,
                        b,
                        range: Some(range),
                    }),
                    ..Default::default()
                }),
                color_drafts: [
                    ChannelColorDraft {
                        exact_truecolor: ExactTruecolorDraft::from_rgb(r, g, b),
                        color_range_hex: [endpoint_hex(range.first), endpoint_hex(range.second)],
                        color_range_last_valid: range,
                    },
                    ChannelColorDraft::default(),
                ],
                ..TriggerRow::new(PatternKind::Match)
            }];
            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&rows, &mut trigger).unwrap();

            assert_eq!(trigger_rows(&trigger), rows);
        }

        #[test]
        fn exact_truecolor_and_color_range_use_separate_tabs() {
            let exact = matchers::MatcherColor::Truecolor {
                r: 10,
                g: 20,
                b: 30,
                range: None,
            };
            let point = matchers::MatcherHsv::from_rgb(10, 20, 30);
            let range = matchers::MatcherColor::Truecolor {
                r: 10,
                g: 20,
                b: 30,
                range: Some(matchers::MatcherHsvRange::from_to(point, point)),
            };

            assert_eq!(
                MatcherColorKind::of(Some(exact)),
                MatcherColorKind::Truecolor
            );
            assert_eq!(
                MatcherColorKind::of(Some(range)),
                MatcherColorKind::ColorRange
            );
            assert_eq!(ExactTruecolorDraft::from_rgb(10, 20, 30).hex, "#0a141e");
        }

        #[test]
        fn configured_truecolor_channels_require_valid_drafts() {
            let exact = matchers::MatcherColor::Truecolor {
                r: 10,
                g: 20,
                b: 30,
                range: None,
            };
            let point = matchers::MatcherHsv::from_rgb(40, 50, 60);
            let range = matchers::MatcherColor::Truecolor {
                r: 40,
                g: 50,
                b: 60,
                range: Some(matchers::MatcherHsvRange::from_to(point, point)),
            };
            let mut row = TriggerRow {
                source: "target".to_string(),
                color: Some(matchers::MatcherColorMatch {
                    foreground: Some(exact),
                    background: Some(range),
                    attributes: Vec::new(),
                }),
                color_drafts: [
                    ChannelColorDraft::from_color(Some(exact)),
                    ChannelColorDraft::from_color(Some(range)),
                ],
                ..TriggerRow::new(PatternKind::Match)
            };
            assert!(row.compiled().is_ok());

            row.color_drafts[0].exact_truecolor.hex = "#123".to_string();
            assert!(row.compiled().is_err());
            row.color_drafts[0] = ChannelColorDraft::from_color(Some(exact));
            row.color_drafts[0].exact_truecolor.rgb[1] = "300".to_string();
            assert!(row.compiled().is_err());
            row.color_drafts[0] = ChannelColorDraft::from_color(Some(exact));
            row.color_drafts[1].color_range_hex[1] = "#abcd".to_string();
            assert!(row.compiled().is_err());
        }

        #[test]
        fn hand_edited_vectors_degrade_sidecar_rows_to_regex() {
            let rows = vec![TriggerRow {
                role: PatternKind::Match,
                syntax: MatcherSyntax::Pattern,
                source: "You are {state}.".to_string(),
                anchor_start: true,
                anchor_end: true,
                ..TriggerRow::new(PatternKind::Match)
            }];
            let mut trigger = triggers::TriggerDefinition::default();
            rows_into_trigger(&rows, &mut trigger).unwrap();
            // A hand edit to the stored vector makes the sidecar stale.
            trigger.patterns = Some(vec!["^edited$".to_string()]);
            let degraded = trigger_rows(&trigger);
            assert_eq!(degraded.len(), 1);
            assert_eq!(degraded[0].syntax, MatcherSyntax::Regex);
            assert_eq!(degraded[0].source, "^edited$");
        }

        #[test]
        fn invalid_rows_report_their_index() {
            let rows = vec![
                TriggerRow {
                    source: "fine".to_string(),
                    ..TriggerRow::new(PatternKind::Match)
                },
                TriggerRow {
                    source: "[unclosed".to_string(),
                    ..TriggerRow::new(PatternKind::Match)
                },
            ];
            let mut trigger = triggers::TriggerDefinition::default();
            let (index, _) = rows_into_trigger(&rows, &mut trigger).unwrap_err();
            assert_eq!(index, 1);
        }
    }

    /// A directly-installed package: present in the lockfile (so in `direct`) with its own
    /// enable intent — exactly what `rebuild_graph` seeds for each installed entry.
    fn install(graph: &mut PackageGraph, spec: &str, enabled: bool) {
        graph.direct.insert(spec.to_string());
        graph.intent.insert(spec.to_string(), enabled);
    }

    /// Record that `parent` imports `child` into its own isolate.
    fn imports(graph: &mut PackageGraph, parent: &str, child: &str) {
        graph
            .requires
            .entry(parent.to_string())
            .or_default()
            .push(DepEdge {
                specifier: child.to_string(),
                range: String::new(),
                kind: smudgy_cloud::DependencyKind::Dependency,
            });
    }

    /// Record that `parent` needs `child` to run as a separate root.
    fn requires(graph: &mut PackageGraph, parent: &str, child: &str) {
        graph
            .requires
            .entry(parent.to_string())
            .or_default()
            .push(DepEdge {
                specifier: child.to_string(),
                range: String::new(),
                kind: smudgy_cloud::DependencyKind::Requires,
            });
    }

    #[test]
    fn dep_edge_row_greys_when_parent_disabled_but_dep_keeps_its_own_status() {
        // P imports D, and D is also separately installed (its own enabled lockfile entry).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        install(&mut graph, "d", true);
        imports(&mut graph, "p", "d");

        assert!(
            graph.dep_edge_active("p", "d"),
            "both on: D's row under P is live"
        );

        // Disable P. D keeps its own enabled entry, so it still runs on its own...
        graph.intent.insert("p".to_string(), false);
        assert!(
            graph.effectively_enabled("d"),
            "D still runs via its own install — its own row stays green",
        );
        // ...but its row UNDER P greys: the import via P is no longer active. This is the bug fix.
        assert!(!graph.dep_edge_active("p", "d"));
    }

    #[test]
    fn dep_edge_row_stays_live_via_another_enabled_requirer() {
        // P and Q both import D (D separately installed too).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        install(&mut graph, "q", true);
        install(&mut graph, "d", true);
        imports(&mut graph, "p", "d");
        imports(&mut graph, "q", "d");

        // Disable only P: D's row under P greys, but under still-enabled Q it stays live.
        graph.intent.insert("p".to_string(), false);
        assert!(
            !graph.dep_edge_active("p", "d"),
            "the import via the disabled P is dead"
        );
        assert!(graph.dep_edge_active("q", "d"), "Q still pulls D in");
        assert!(graph.effectively_enabled("d"));
    }

    #[test]
    fn dep_edge_row_follows_parent_for_a_pure_import_dep() {
        // P imports L, which has NO lockfile entry of its own (a pure transitive import).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        imports(&mut graph, "p", "l");

        assert!(graph.is_dep_only("l"));
        assert!(
            graph.dep_edge_active("p", "l"),
            "L is live while P pulls it in"
        );

        // Disable P: nothing else needs L, so both its global state and its row go inactive.
        graph.intent.insert("p".to_string(), false);
        assert!(!graph.effectively_enabled("l"));
        assert!(!graph.dep_edge_active("p", "l"));
    }

    #[test]
    fn requires_edges_follow_the_active_parent_chain() {
        let mut graph = PackageGraph::default();
        install(&mut graph, "parent", true);
        imports(&mut graph, "parent", "library");
        requires(&mut graph, "parent", "worker");

        assert!(!graph.effectively_enabled("library"));
        assert!(graph.effectively_enabled("worker"));
        assert!(graph.dep_edge_active("parent", "library"));
        assert!(graph.dep_edge_active("parent", "worker"));
        assert!(graph.required_by("library").is_empty());
        assert_eq!(graph.required_by("worker"), ["parent"]);
        assert!(!graph.controllable("worker"));

        graph.intent.insert("parent".to_string(), false);
        assert!(!graph.effectively_enabled("worker"));
        assert!(!graph.dep_edge_active("parent", "worker"));

        install(&mut graph, "worker", true);
        assert!(graph.effectively_enabled("worker"));
        assert!(graph.controllable("worker"));
    }

    mod inner_triggers {
        use std::collections::{BTreeMap, HashMap};

        use smudgy_core::models::triggers::{InnerReach, LineReach, TriggerDefinition};

        use super::super::{flatten_inner_triggers, nest_inner_triggers};

        fn trigger(pattern: &str) -> TriggerDefinition {
            TriggerDefinition {
                patterns: Some(vec![pattern.to_string()]),
                ..TriggerDefinition::default()
            }
        }

        #[test]
        fn nested_file_flattens_to_leaves_and_nests_back_unchanged() {
            let mut deeper = BTreeMap::new();
            deeper.insert("deep".to_string(), trigger("^deep$"));
            let mut inner = BTreeMap::new();
            let mut item = trigger("^ item$");
            item.reach.within_lines = LineReach::Lines(40);
            item.inner = Some(deeper);
            inner.insert("item".to_string(), item);
            let mut heading = trigger("^Items:$");
            heading.package = Some("inventory".to_string());
            heading.inner = Some(inner);
            let mut file = HashMap::new();
            file.insert("heading".to_string(), heading.clone());
            file.insert("other".to_string(), trigger("^other$"));

            let flat = flatten_inner_triggers(file.clone());
            assert_eq!(flat.len(), 4);
            let item = &flat["item"];
            assert_eq!(item.outer.as_deref(), Some("heading"));
            assert_eq!(
                item.package.as_deref(),
                Some("inventory"),
                "inherits the root's folder"
            );
            assert!(item.inner.is_none());
            assert_eq!(flat["deep"].outer.as_deref(), Some("item"));
            assert_eq!(flat["deep"].package.as_deref(), Some("inventory"));
            assert!(flat["heading"].outer.is_none());
            assert!(flat["heading"].inner.is_none());

            let nested = nest_inner_triggers(flat);
            assert_eq!(nested, file, "the round trip is exact");
        }

        #[test]
        fn a_leaf_whose_outer_is_gone_stays_top_level() {
            let mut orphan = trigger("^x$");
            orphan.outer = Some("nobody".to_string());
            let mut flat = HashMap::new();
            flat.insert("orphan".to_string(), orphan);
            let nested = nest_inner_triggers(flat);
            assert!(nested["orphan"].outer.is_none());
            assert_eq!(nested["orphan"].reach, InnerReach::DEFAULT);
        }
    }
}
