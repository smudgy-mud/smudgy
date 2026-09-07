//! The "What it reads" disclosure of the alias, trigger, and hotkey editors: the drafted state
//! exposures (the definition's `state` list, `core::models::state_exposure`), the browser over
//! the live session store that toggles them, and the row that adds a path by hand. The draft
//! mirrors the open definition while the editor is open; `save_open` validates it through
//! [`resolve_all`] and writes it back. The disclosure keeps the shape of "When it runs": a text
//! link while hidden, forced open (and not re-hideable) while any exposure exists, with a hide
//! link only at defaults.

use iced::alignment::Vertical;
use iced::widget::{Column, Space, button, checkbox, column, container, row, text, text_input};
use iced::{Font, Length, Padding};
use smudgy_cloud::Node;
use smudgy_core::models::ScriptLang;
use smudgy_core::models::state_exposure::{
    KnownGlobal, StateExposure, StateExposureError, resolve_all,
};
use smudgy_core::session::runtime::catalogue::{CatalogueKind, CatalogueSnapshot, ProducerView};
use smudgy_core::session::runtime::{ProducerKey, StorePath};

use crate::assets::fonts;
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::widgets::dropdown::Dropdown;

use super::editors::{field_row, text_link, tip};
use super::highlight::ExposedPath;
use super::store_inspector::{RowLabel, TreeRows, json_rows};
use super::{AutomationsWindow, EditNode, EditorState, Elem, Message, Pane, common};

const MONO: Font = fonts::GEIST_MONO_VF;

/// Rows one filter shows before eliding the rest (the elision row says how many).
const FILTER_RESULT_CAP: usize = 200;

/// Nodes one filter pass visits before it stops looking, so typing stays cheap over a large
/// published tree; matches beyond the budget are simply not found.
const FILTER_VISIT_BUDGET: usize = 20_000;

/// The vertical gap between the module's sections. The section column has no spacing of its
/// own: the Exposed section carries the gap as bottom padding only while it renders (its
/// zero-height placeholder stays mounted for tree stability, and column spacing would leave
/// it as a blank band above Browse whenever nothing is exposed), and the sections below
/// Browse carry it as top padding.
const SECTION_GAP: f32 = 12.0;

/// `content` with the section gap above it.
fn gap_above(content: Elem<'_>) -> Elem<'_> {
    container(content)
        .padding(Padding {
            top: SECTION_GAP,
            ..Padding::ZERO
        })
        .into()
}

/// One exposed root as drafted in the editor: the persisted exposure plus the rename field's
/// visibility, which the on-disk form has no room for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateExposureDraft {
    /// The producer address in display form (`user`, `gmcp`, `smudgy://owner/name`).
    pub producer: String,
    /// The handle name for the `user` and package producers; `None` for a platform producer.
    pub handle: Option<String>,
    /// The "Use a different name" field: `None` while the link shows, `Some(text)` once
    /// revealed. Blank text is not an override (the Command-word rule): the default name holds
    /// until something is typed.
    pub name_field: Option<String>,
    /// Paths beneath the root in the author's spelling; `""` is the root itself.
    pub paths: Vec<String>,
}

impl StateExposureDraft {
    pub fn from_exposure(exposure: &StateExposure) -> Self {
        Self {
            producer: exposure.producer.clone(),
            handle: exposure.handle.clone(),
            name_field: exposure.name_override.clone(),
            paths: exposure.paths.clone(),
        }
    }

    /// The persisted form: a trimmed, non-blank rename field is the `as` override, unless it
    /// spells the default name exactly, in which case `as` is omitted (§4). JavaScript
    /// spelling is exact, so another casing of the default (`Foo` for a handle `foo`) is a
    /// rename.
    pub fn to_exposure(&self) -> StateExposure {
        let mut exposure = StateExposure {
            producer: self.producer.clone(),
            handle: self.handle.clone(),
            name_override: None,
            paths: self.paths.clone(),
        };
        let default = exposure.default_name();
        exposure.name_override = self
            .name_field
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != default)
            .map(str::to_owned);
        exposure
    }

    /// The name the action spells: the override when one is typed, else the default.
    pub fn name(&self) -> String {
        self.to_exposure().name()
    }

    pub fn default_name(&self) -> String {
        self.to_exposure().default_name()
    }

    /// Whether this draft is the root at `producer` + `handle` (both compared folded).
    pub fn is_root(&self, producer: &str, handle: Option<&str>) -> bool {
        producers_equal(&self.producer, producer)
            && match (&self.handle, handle) {
                (None, None) => true,
                (Some(mine), Some(other)) => mine.trim().eq_ignore_ascii_case(other.trim()),
                _ => false,
            }
    }

    /// The position of `path` among this root's paths, matched segment-wise under the fold.
    pub fn path_index(&self, path: &str) -> Option<usize> {
        self.paths.iter().position(|kept| paths_equal(kept, path))
    }
}

/// The Add-a-path row's drafts: the chosen producer, the handle and path being typed, the
/// producer picker's open state and keyboard cursor, and the last rejected path's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateAddDraft {
    /// Whether the row is showing; it starts behind its "advanced" link.
    pub revealed: bool,
    pub producer: String,
    pub handle: String,
    pub path: String,
    pub picker_open: bool,
    pub cursor: usize,
    /// The handle picker: open, and its keyboard cursor among the known handles.
    pub handle_picker_open: bool,
    pub handle_cursor: usize,
    pub error: Option<String>,
}

impl Default for StateAddDraft {
    fn default() -> Self {
        Self {
            revealed: false,
            producer: "gmcp".to_string(),
            handle: String::new(),
            path: String::new(),
            picker_open: false,
            cursor: 0,
            handle_picker_open: false,
            handle_cursor: 0,
            error: None,
        }
    }
}

/// One entry of the producer picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerChoice {
    /// The address in display form: what an exposure persists.
    pub key: String,
    /// What the picker shows: the address itself, or a package's name.
    pub label: String,
    /// The full address when `label` abbreviates it (packages).
    pub detail: Option<String>,
    /// Whether an exposure under this producer needs a handle.
    pub takes_handle: bool,
}

/// A per-root diagnostic under an Exposed heading. The name errors block a save; the
/// shadowing notes are advisory (§3.2: shadowing is the trade the author asked for) and
/// apply to the action language that can see the collision: the JavaScript notes to a
/// script body, the capture note to Send text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateNote {
    /// An earlier exposure already uses this name under the fold.
    NameTaken(String),
    /// The name is not an identifier.
    BadName(String),
    /// The name is a JavaScript reserved word, which no body could spell.
    ReservedName(String),
    /// The name is an inline API member, unreachable inside this script.
    ShadowsSmudgy(String),
    /// The name is an ECMAScript global, unreachable inside this script.
    ShadowsJavaScript(String),
    /// The name is a Deno or web-platform global, unreachable inside this script.
    ShadowsDeno(String),
    /// The name is also a named capture; bare `$name` in Send text means the capture.
    ShadowsCapture(String),
}

impl StateNote {
    pub fn blocks_save(&self) -> bool {
        matches!(
            self,
            Self::NameTaken(_) | Self::BadName(_) | Self::ReservedName(_)
        )
    }

    pub fn message(&self) -> String {
        match self {
            Self::NameTaken(name) => {
                crate::i18n::t!("editor-state-name-taken", "name" => name.as_str())
            }
            Self::BadName(name) => {
                crate::i18n::t!("editor-state-bad-name", "name" => name.as_str())
            }
            Self::ReservedName(name) => {
                crate::i18n::t!("editor-state-reserved-name", "name" => name.as_str())
            }
            Self::ShadowsSmudgy(name) => {
                crate::i18n::t!("editor-state-name-shadows-smudgy", "name" => name.as_str())
            }
            Self::ShadowsJavaScript(name) => {
                crate::i18n::t!("editor-state-name-shadows-javascript", "name" => name.as_str())
            }
            Self::ShadowsDeno(name) => {
                crate::i18n::t!("editor-state-name-shadows-deno", "name" => name.as_str())
            }
            Self::ShadowsCapture(name) => {
                crate::i18n::t!("editor-state-name-shadows-capture", "name" => name.as_str())
            }
        }
    }
}

/// Whether two producer addresses name the same producer: parsed and folded when both parse,
/// folded text otherwise.
fn producers_equal(a: &str, b: &str) -> bool {
    match (ProducerKey::parse(a), ProducerKey::parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    }
}

/// Whether two path spellings address the same store node: segment-wise under the fold when
/// both parse, folded text otherwise.
fn paths_equal(a: &str, b: &str) -> bool {
    match (StorePath::parse(a), StorePath::parse(b)) {
        (Ok(a), Ok(b)) => {
            a.segments().len() == b.segments().len()
                && a.segments()
                    .iter()
                    .zip(b.segments())
                    .all(|(x, y)| x.eq_ignore_ascii_case(y))
        }
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    }
}

/// Whether exposures under `producer` name a handle (everything but the platform producers).
fn takes_handle(producer: &str) -> bool {
    !matches!(ProducerKey::parse(producer), Some(ProducerKey::Platform(_)))
}

/// The name an exposure at `producer` + `handle` gets when nothing renames it.
fn default_name_for(producer: &str, handle: Option<&str>) -> String {
    StateExposure {
        producer: producer.to_string(),
        handle: handle.map(str::to_owned),
        name_override: None,
        paths: Vec::new(),
    }
    .default_name()
}

/// The reference an action spells for `path` under `name`: the name alone for the root,
/// `name.path` otherwise (a bracket-quoted first segment attaches without the dot). A path
/// that does not parse renders as typed, so the Exposed list shows the author what to fix;
/// the rail, whose badges insert references, skips such a path (`rail_entries`).
pub fn reference(name: &str, path: &str) -> String {
    match StorePath::parse(path) {
        Ok(parsed) => join_reference(name, &parsed.to_string()),
        Err(_) => join_reference(name, path.trim()),
    }
}

/// `name` joined with a path's display form.
fn join_reference(name: &str, display: &str) -> String {
    if display.is_empty() {
        name.to_string()
    } else if display.starts_with('[') {
        format!("{name}{display}")
    } else {
        format!("{name}.{display}")
    }
}

/// One reference in the action language's vocabulary: `$gmcp.Char.Vitals.hp` for a text
/// body, braced (`${gmcp["Some-Pkg"].Msg}`) when it spells a bracket key, which the bare
/// form does not take; `gmcp.Char.Vitals.hp` for JavaScript.
fn render_state_reference(reference: String, language: ScriptLang) -> String {
    match language {
        ScriptLang::Plaintext if reference.contains('[') => format!("${{{reference}}}"),
        ScriptLang::Plaintext => format!("${reference}"),
        ScriptLang::JS | ScriptLang::TS => reference,
    }
}

/// The State values rail's badges, one per exposed path in draft order: the reference in
/// the action language's vocabulary (the twin of the capture rail's `render_references`),
/// with the live value when `snapshot` holds one. A path that does not parse gets no badge:
/// it has no reference to insert, and the highlighter exposes nothing under it
/// (`AutomationsWindow::exposed_paths` applies the same filter). Reference and value are
/// built in the one pass, so the two columns cannot disagree about which paths count.
pub(super) fn rail_entries(
    exposures: &[StateExposureDraft],
    language: ScriptLang,
    snapshot: Option<&CatalogueSnapshot>,
) -> Vec<(String, Option<String>)> {
    let mut entries = Vec::new();
    for draft in exposures {
        let name = draft.name();
        for path in &draft.paths {
            let Ok(parsed) = StorePath::parse(path) else {
                continue;
            };
            let reference =
                render_state_reference(join_reference(&name, &parsed.to_string()), language);
            let value = snapshot.and_then(|snapshot| state_value_preview(snapshot, draft, path));
            entries.push((reference, value));
        }
    }
    entries
}

/// How many characters of a live value a rail badge shows before eliding the rest.
const VALUE_PREVIEW_CHARS: usize = 24;

/// The live value at `path` beneath the draft's root in `snapshot`, rendered for a rail
/// badge: scalars in their JSON spelling, `{…}` and `[…]` for containers, a long value
/// elided. `None` when the snapshot holds no node there.
pub(super) fn state_value_preview(
    snapshot: &CatalogueSnapshot,
    draft: &StateExposureDraft,
    path: &str,
) -> Option<String> {
    let view = snapshot
        .producers
        .iter()
        .find(|view| producers_equal(&view.producer, &draft.producer))?;
    let mut node = &view.tree;
    if let Some(handle) = &draft.handle {
        node = node.get(handle.trim())?;
    }
    for segment in StorePath::parse(path).ok()?.segments() {
        node = node.get(segment)?;
    }
    Some(value_preview(node))
}

fn value_preview(node: &Node) -> String {
    match node {
        Node::Object(_) => "{…}".to_string(),
        Node::Array(_) => "[…]".to_string(),
        _ => {
            let text = node.to_string();
            match text.char_indices().nth(VALUE_PREVIEW_CHARS) {
                Some((at, _)) => format!("{}…", &text[..at]),
                None => text,
            }
        }
    }
}

/// How a producer is named in the editor: the address itself for `user` and the platform
/// producers; a package's name, with the full address as the detail (shown on hover).
fn producer_display(producer: &str) -> (String, Option<String>) {
    match ProducerKey::parse(producer) {
        Some(key) => match &key {
            ProducerKey::Package { name, .. } => (name.clone(), Some(key.to_string())),
            _ => (key.to_string(), None),
        },
        None => (producer.to_string(), None),
    }
}

/// Producers list `user` first, then the platform producers, then packages; each group by
/// address.
fn producer_rank(producer: &str) -> (u8, String) {
    match ProducerKey::parse(producer) {
        Some(ProducerKey::User) => (0, String::new()),
        Some(ProducerKey::Platform(platform)) => (1, platform.as_str().to_string()),
        Some(key @ ProducerKey::Package { .. }) => (2, key.to_string()),
        None => (3, producer.to_string()),
    }
}

/// The message for one unusable exposure. The name and path failures, the ones the editor's
/// own controls can produce, get the notes' wording; the rest (an unknown producer, a handle
/// missing or misplaced, no paths) only come from a file edited by hand or written by a
/// later version, and carry the runtime's warning inside the catalog's frame.
/// The diagnostics for exposure `names` in draft order, given the open editor's capture
/// names and action language. A name that is not an identifier, or is a reserved word,
/// blocks the save; a duplicate marks the later entry, as [`resolve_all`] does. The
/// shadowing notes are advisory and appear only for a name the body could otherwise reach,
/// in the language that would see the collision: a script body for the inline API members
/// and the ECMAScript and Deno globals (static lists in core), Send text for this
/// automation's own captures. No open editor means no advisory note.
fn state_notes_for(
    names: &[String],
    captures: &[String],
    language: Option<ScriptLang>,
) -> Vec<Vec<StateNote>> {
    let script = matches!(language, Some(ScriptLang::JS | ScriptLang::TS));
    let send_text = language == Some(ScriptLang::Plaintext);
    let mut seen: Vec<String> = Vec::with_capacity(names.len());
    names
        .iter()
        .map(|name| {
            let mut notes = Vec::new();
            if !StateExposure::is_identifier(name) {
                notes.push(StateNote::BadName(name.clone()));
            } else if StateExposure::is_reserved(name) {
                notes.push(StateNote::ReservedName(name.clone()));
            }
            let folded = name.to_ascii_lowercase();
            if seen.contains(&folded) {
                notes.push(StateNote::NameTaken(name.clone()));
            } else {
                seen.push(folded);
            }
            if script {
                match StateExposure::known_global(name) {
                    Some(KnownGlobal::Smudgy) => {
                        notes.push(StateNote::ShadowsSmudgy(name.clone()));
                    }
                    Some(KnownGlobal::JavaScript) => {
                        notes.push(StateNote::ShadowsJavaScript(name.clone()));
                    }
                    Some(KnownGlobal::Deno) => notes.push(StateNote::ShadowsDeno(name.clone())),
                    None => {}
                }
            }
            if send_text && captures.contains(name) {
                notes.push(StateNote::ShadowsCapture(name.clone()));
            }
            notes
        })
        .collect()
}

fn exposure_error_message(error: &StateExposureError) -> String {
    match error {
        StateExposureError::DuplicateName { name } => {
            crate::i18n::t!("editor-state-name-taken", "name" => name.as_str())
        }
        StateExposureError::InvalidName { name } => {
            crate::i18n::t!("editor-state-bad-name", "name" => name.as_str())
        }
        StateExposureError::ReservedName { name } => {
            crate::i18n::t!("editor-state-reserved-name", "name" => name.as_str())
        }
        StateExposureError::InvalidPath { path, .. } => {
            crate::i18n::t!("editor-state-bad-path", "path" => path.as_str())
        }
        StateExposureError::UnknownProducer { .. }
        | StateExposureError::MissingHandle { .. }
        | StateExposureError::UnexpectedHandle { .. }
        | StateExposureError::InvalidHandle { .. }
        | StateExposureError::NoPaths => {
            let detail = error.to_string();
            crate::i18n::t!("editor-state-invalid", "error" => detail.as_str())
        }
    }
}

/// The picker row style shared with the Parsing picker: the selection tinted with the accent,
/// the keyboard cursor and hover with a faint wash.
fn picker_row_style(
    selected: bool,
    at_cursor: bool,
) -> impl Fn(&Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |theme: &Theme, status| {
        let background = if selected {
            Some(theme.styles.general.accent.scale_alpha(0.35))
        } else if at_cursor || status == iced::widget::button::Status::Hovered {
            Some(theme.styles.text.normal.scale_alpha(0.06))
        } else {
            None
        };
        iced::widget::button::Style {
            background: background.map(iced::Background::Color),
            border: iced::Border::default().rounded(4.0),
            text_color: theme.styles.text.normal,
            ..Default::default()
        }
    }
}

fn picker_surface_style(theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(theme.styles.modal.body_background),
        border: theme.styles.modal.body_border,
        shadow: theme.styles.modal.shadow,
        ..Default::default()
    }
}

/// Visit every addressable node beneath `node` whose `producer_label path` spelling contains
/// `needle` (both folded), depth first in publish order, until the visit budget runs out.
/// Arrays match as a whole and are not descended: no path spells an element.
fn collect_matches<'a>(
    node: &'a Node,
    path: &mut Vec<String>,
    needle: &str,
    producer_label: &str,
    budget: &mut usize,
    visit: &mut dyn FnMut(&[String], &'a Node),
) {
    let Node::Object(object) = node else {
        return;
    };
    for (key, child) in object.iter() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if key.is_empty() {
            continue;
        }
        path.push(key.to_string());
        let spelled = StorePath::from_segments(path.iter().map(String::as_str))
            .map_or_else(|_| path.join("."), |p| p.to_string());
        if format!("{producer_label} {spelled}")
            .to_ascii_lowercase()
            .contains(needle)
        {
            visit(path.as_slice(), child);
        }
        collect_matches(child, path, needle, producer_label, budget, visit);
        path.pop();
    }
}

impl AutomationsWindow {
    /// Whether the disclosure shows its module: clicked open, or forced open by an exposure
    /// (which also disables re-hiding, the order module's rule).
    pub(super) fn state_disclosure_open(&self) -> bool {
        self.state_revealed || !self.state_exposures.is_empty()
    }

    /// Whether this window wants the runtime's catalogue broadcast: the store pane always,
    /// an editor while its disclosure is open (so the browser and the rail can show live
    /// values). Narrow otherwise, so nothing is built while no window is subscribed.
    pub(super) fn catalogue_subscribed(&self) -> bool {
        match &self.pane {
            Pane::StoreInspector => true,
            Pane::Editor(_) => self.state_disclosure_open(),
            _ => false,
        }
    }

    /// Seeds the draft from a definition's exposures (empty for a new automation) and resets
    /// the disclosure, the browser, and the Add-a-path row. An entry with no paths exposes
    /// nothing and has no row to remove it by, so it is dropped here with a warning, as the
    /// runtime drops it at registration; every other unusable entry keeps its rows, so the
    /// author can see it and remove it.
    pub(super) fn reset_state_draft(&mut self, exposures: &[StateExposure]) {
        self.state_exposures = Vec::with_capacity(exposures.len());
        for exposure in exposures {
            if exposure.paths.is_empty() {
                log::warn!(
                    "state exposure {:?} ignored: no paths are exposed",
                    exposure.name()
                );
                continue;
            }
            self.state_exposures
                .push(StateExposureDraft::from_exposure(exposure));
        }
        self.state_revealed = false;
        self.state_filter.clear();
        self.state_toggled.clear();
        self.state_add = StateAddDraft::default();
    }

    fn state_root_index(&self, producer: &str, handle: Option<&str>) -> Option<usize> {
        self.state_exposures
            .iter()
            .position(|draft| draft.is_root(producer, handle))
    }

    /// Adds `path` under the root (creating the root), or nothing when it is already there.
    fn insert_state_path(&mut self, producer: &str, handle: Option<&str>, path: &str) {
        let path = path.trim();
        match self.state_root_index(producer, handle) {
            Some(index) => {
                let draft = &mut self.state_exposures[index];
                if draft.path_index(path).is_none() {
                    draft.paths.push(path.to_string());
                }
            }
            None => self.state_exposures.push(StateExposureDraft {
                producer: producer.to_string(),
                handle: handle.map(|handle| handle.trim().to_string()),
                name_field: None,
                paths: vec![path.to_string()],
            }),
        }
    }

    /// Removes `path` from its root, and the root once no path is left.
    pub(super) fn remove_state_exposure(
        &mut self,
        producer: &str,
        handle: Option<&str>,
        path: &str,
    ) {
        let Some(index) = self.state_root_index(producer, handle) else {
            return;
        };
        let draft = &mut self.state_exposures[index];
        if let Some(at) = draft.path_index(path) {
            draft.paths.remove(at);
        }
        if draft.paths.is_empty() {
            self.state_exposures.remove(index);
        }
    }

    /// The browser's trailing toggle: exposes `path` under the root, or stops exposing it.
    pub(super) fn toggle_state_exposure(
        &mut self,
        producer: &str,
        handle: Option<&str>,
        path: &str,
    ) {
        let exposed = self
            .state_root_index(producer, handle)
            .is_some_and(|index| self.state_exposures[index].path_index(path).is_some());
        if exposed {
            self.remove_state_exposure(producer, handle, path);
        } else {
            self.insert_state_path(producer, handle, path);
        }
    }

    /// The Add-a-path row's Enter: a path that parses is added under the drafted root and the
    /// field clears; one that does not leaves its message under the row. A root that needs a
    /// handle adds nothing until one is typed (the empty field is the prompt).
    pub(super) fn add_state_path(&mut self, producer: &str, handle: Option<&str>, path: &str) {
        let path = path.trim();
        if let Err(error) = StorePath::parse(path) {
            log::debug!("rejected state path {path:?}: {error}");
            self.state_add.error = Some(crate::i18n::t!("editor-state-bad-path", "path" => path));
            return;
        }
        let handle = handle.map(str::trim).filter(|handle| !handle.is_empty());
        if takes_handle(producer) && handle.is_none() {
            return;
        }
        self.insert_state_path(
            producer,
            if takes_handle(producer) { handle } else { None },
            path,
        );
        self.state_add.error = None;
        self.state_add.path.clear();
    }

    /// Opens a root's rename field (blank: the default name holds).
    pub(super) fn reveal_state_name(&mut self, producer: &str, handle: Option<&str>) {
        if let Some(index) = self.state_root_index(producer, handle) {
            let draft = &mut self.state_exposures[index];
            if draft.name_field.is_none() {
                draft.name_field = Some(String::new());
            }
        }
    }

    pub(super) fn set_state_name(&mut self, producer: &str, handle: Option<&str>, name: String) {
        if let Some(index) = self.state_root_index(producer, handle) {
            self.state_exposures[index].name_field = Some(name);
        }
    }

    /// The named captures the open matcher provides (aliases and triggers; hotkeys capture
    /// nothing), for the capture-collision note.
    fn open_capture_names(&self) -> Vec<String> {
        let captures = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Alias(_),
                ..
            }) => self.alias_captures(),
            Pane::Editor(EditorState {
                node: EditNode::Trigger { rows, .. },
                ..
            }) => Self::trigger_captures(rows),
            _ => Vec::new(),
        };
        captures.into_iter().flatten().collect()
    }

    /// The diagnostics under each Exposed heading, index-aligned with the draft (§3.2). A
    /// duplicate name marks the later exposure, as [`resolve_all`] does.
    pub(super) fn state_notes(&self) -> Vec<Vec<StateNote>> {
        let names: Vec<String> = self
            .state_exposures
            .iter()
            .map(StateExposureDraft::name)
            .collect();
        state_notes_for(
            &names,
            &self.open_capture_names(),
            self.open_action_language(),
        )
    }

    /// The draft in its persisted form, validated the way the runtime validates it (§4): the
    /// first failure's message is the editor's error-bar text.
    pub(super) fn validate_state_exposures(&self) -> Result<Vec<StateExposure>, String> {
        let exposures: Vec<StateExposure> = self
            .state_exposures
            .iter()
            .map(StateExposureDraft::to_exposure)
            .collect();
        match resolve_all(&exposures) {
            Ok(_) => Ok(exposures),
            Err((_, error)) => Err(exposure_error_message(&error)),
        }
    }

    /// The exposed paths as the send-text highlighter resolves references against them,
    /// name and segments folded. An unparsable path (flagged under Exposed) covers nothing.
    pub(super) fn exposed_paths(&self) -> Vec<ExposedPath> {
        let mut paths = Vec::new();
        for draft in &self.state_exposures {
            let name = draft.name();
            for path in &draft.paths {
                if let Ok(parsed) = StorePath::parse(path) {
                    paths.push(ExposedPath::new(&name, parsed.segments()));
                }
            }
        }
        paths
    }

    /// The State values rail's badges for the open draft, read against the live catalogue.
    pub(super) fn state_rail_entries(&self, language: ScriptLang) -> Vec<(String, Option<String>)> {
        rail_entries(&self.state_exposures, language, self.catalogue.as_deref())
    }

    /// The producer picker's entries: `user` and the platform producers always, then every
    /// package the live store or the lockfile knows, so a package handle can be added before
    /// its state arrives or with no session at all.
    pub(super) fn state_producer_choices(&self) -> Vec<ProducerChoice> {
        let mut keys: Vec<String> = ["user", "gmcp", "msdp", "mssp"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut push_package = |key: ProducerKey| {
            let key = key.to_string();
            if !keys.contains(&key) {
                keys.push(key);
            }
        };
        if let Some(snapshot) = &self.catalogue {
            for view in &snapshot.producers {
                if let Some(key @ ProducerKey::Package { .. }) = ProducerKey::parse(&view.producer)
                {
                    push_package(key);
                }
            }
        }
        for package in &self.installed_packages {
            let coordinates = package
                .specifier
                .split('@')
                .next()
                .unwrap_or(&package.specifier);
            if let Some(key @ ProducerKey::Package { .. }) = ProducerKey::parse(coordinates) {
                push_package(key);
            }
        }
        keys.into_iter()
            .map(|key| {
                let (label, detail) = producer_display(&key);
                ProducerChoice {
                    takes_handle: takes_handle(&key),
                    key,
                    label,
                    detail,
                }
            })
            .collect()
    }

    /// The index of the drafted producer among the picker's choices.
    fn state_producer_position(&self, choices: &[ProducerChoice]) -> usize {
        choices
            .iter()
            .position(|choice| producers_equal(&choice.key, &self.state_add.producer))
            .unwrap_or(0)
    }

    pub(super) fn open_state_producer_picker(&mut self) {
        let choices = self.state_producer_choices();
        self.state_add.cursor = self.state_producer_position(&choices);
        self.state_add.picker_open = true;
    }

    pub(super) fn move_state_producer_cursor(&mut self, delta: i32) {
        let len = self.state_producer_choices().len() as i32;
        if len == 0 {
            return;
        }
        let cursor = self.state_add.cursor as i32 + delta;
        self.state_add.cursor = cursor.rem_euclid(len) as usize;
    }

    /// Picks a producer for the Add-a-path row. A handle belongs to one producer, so the
    /// switch clears it (a producer without handles has none to keep).
    pub(super) fn set_state_producer(&mut self, producer: String) {
        self.state_add.handle.clear();
        self.state_add.handle_picker_open = false;
        self.state_add.producer = producer;
        self.state_add.picker_open = false;
        self.state_add.error = None;
    }

    /// The handles the catalogue knows under `producer`: the keys at its root and its
    /// declared state handles (set or not), first-published spelling, deduped under the
    /// fold. Empty without a live session, when the row falls back to typing the handle.
    fn state_handle_choices(&self, producer: &str) -> Vec<String> {
        let mut handles: Vec<String> = Vec::new();
        let Some(snapshot) = &self.catalogue else {
            return handles;
        };
        let mut push = |handle: &str| {
            if !handle.is_empty()
                && !handles
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(handle))
            {
                handles.push(handle.to_string());
            }
        };
        for view in &snapshot.producers {
            if producers_equal(&view.producer, producer)
                && let Some(object) = view.tree.as_object()
            {
                for key in object.keys() {
                    push(key);
                }
            }
        }
        for entry in &snapshot.entries {
            if matches!(entry.kind, CatalogueKind::State)
                && producers_equal(&entry.producer, producer)
            {
                push(&entry.name);
            }
        }
        handles
    }

    /// The index of the drafted handle among the picker's choices.
    fn state_handle_position(&self, choices: &[String]) -> usize {
        choices
            .iter()
            .position(|choice| choice.eq_ignore_ascii_case(self.state_add.handle.trim()))
            .unwrap_or(0)
    }

    pub(super) fn open_state_handle_picker(&mut self) {
        let choices = self.state_handle_choices(&self.state_add.producer);
        self.state_add.handle_cursor = self.state_handle_position(&choices);
        self.state_add.handle_picker_open = true;
    }

    pub(super) fn move_state_handle_cursor(&mut self, delta: i32) {
        let len = self.state_handle_choices(&self.state_add.producer).len() as i32;
        if len == 0 {
            return;
        }
        let cursor = self.state_add.handle_cursor as i32 + delta;
        self.state_add.handle_cursor = cursor.rem_euclid(len) as usize;
    }

    // ---- views -----------------------------------------------------------------

    /// The "What it reads" module behind its disclosure: hidden as a text link (its grid
    /// label rendered empty) until clicked, forced open (and not re-hideable) while any
    /// exposure exists, with a hide link when open on none. Everything inside mounts in one
    /// always-present container, so the action editor below keeps its tree position.
    pub(super) fn state_module(&self) -> Elem<'_> {
        if !self.state_disclosure_open() {
            return field_row("", self.state_reveal_link());
        }
        let notes = self.state_notes();
        // The Add-a-path row is the advanced path (a value that has not arrived yet, or
        // authoring offline): a link until asked for.
        let add: Elem<'_> = if self.state_add.revealed {
            self.state_add_row()
        } else {
            text_link(
                crate::i18n::t!("editor-state-add-reveal"),
                Message::RevealStateAdd,
            )
        };
        let mut inner = column![
            container(self.state_exposed_section(&notes)),
            self.state_browse_section(),
            gap_above(add),
        ];
        if self.state_exposures.is_empty() {
            inner = inner.push(gap_above(text_link(
                crate::i18n::t!("editor-hide-state"),
                Message::HideState,
            )));
        }
        field_row(
            crate::i18n::ts!("editor-what-it-reads"),
            container(inner).width(Length::Fill).into(),
        )
    }

    /// The disclosure's reveal link on its own, for the row it shares with the order
    /// disclosure's link while both are hidden.
    pub(super) fn state_reveal_link(&self) -> Elem<'_> {
        text_link(crate::i18n::t!("editor-reveal-state"), Message::RevealState)
    }

    /// The Exposed list: one block per root, grouped under its name, with the section gap
    /// beneath it; a zero-height placeholder while nothing is exposed, so the sections below
    /// keep their tree position.
    fn state_exposed_section<'a>(&'a self, notes: &[Vec<StateNote>]) -> Elem<'a> {
        if self.state_exposures.is_empty() {
            return container(Space::new().height(0)).into();
        }
        let mut section = column![common::section_label(crate::i18n::ts!(
            "editor-state-exposed"
        ))]
        .spacing(6.0)
        .padding(Padding {
            bottom: SECTION_GAP,
            ..Padding::ZERO
        });
        for (index, draft) in self.state_exposures.iter().enumerate() {
            let notes = notes.get(index).map_or(&[][..], Vec::as_slice);
            section = section.push(self.state_exposed_root(draft, notes));
        }
        section.into()
    }

    /// One root's block: the name heading with its producer and rename control, the notes
    /// beneath, then one row per path with its remove link.
    fn state_exposed_root<'a>(
        &'a self,
        draft: &'a StateExposureDraft,
        notes: &[StateNote],
    ) -> Elem<'a> {
        let name = draft.name();
        let producer = draft.producer.clone();
        let handle = draft.handle.clone();
        let mut head = row![
            text(name.clone())
                .size(13.0)
                .font(MONO)
                .style(common::capture_accent)
        ]
        .spacing(10.0)
        .align_y(Vertical::Center);
        // A handle carries its producer beside it; a platform producer's name says it all.
        if draft.handle.is_some() {
            let (label, detail) = producer_display(&draft.producer);
            let label: Elem<'a> = text(label).size(12.0).style(common::muted).into();
            head = head.push(match detail {
                Some(detail) => tip(label, detail),
                None => label,
            });
        }
        match &draft.name_field {
            None => {
                head = head.push(text_link(
                    crate::i18n::t!("editor-state-rename"),
                    Message::RevealStateName {
                        producer: producer.clone(),
                        handle: handle.clone(),
                    },
                ));
            }
            Some(value) => {
                let (producer, handle) = (producer.clone(), handle.clone());
                head = head.push(
                    row![
                        text(crate::i18n::ts!("editor-state-name"))
                            .size(12.0)
                            .style(common::muted),
                        text_input(&draft.default_name(), value)
                            .on_input(move |name| Message::SetStateName {
                                producer: producer.clone(),
                                handle: handle.clone(),
                                name,
                            })
                            .font(MONO)
                            .size(13.0)
                            .width(Length::Fixed(200.0)),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            }
        }
        let mut block = column![head].spacing(4.0);
        for note in notes {
            block = block.push(
                text(note.message())
                    .size(12.0)
                    .style(if note.blocks_save() {
                        common::danger
                    } else {
                        common::warning
                    }),
            );
        }
        let mut paths = Column::new().spacing(2.0);
        for path in &draft.paths {
            let mut line = row![
                text(reference(&name, path)).size(12.0).font(MONO),
                text_link(
                    crate::i18n::t!("editor-state-remove"),
                    Message::RemoveStateExposure {
                        producer: producer.clone(),
                        handle: handle.clone(),
                        path: path.clone(),
                    },
                ),
            ]
            .spacing(10.0)
            .align_y(Vertical::Center);
            if StorePath::parse(path).is_err() {
                line = line.push(
                    text(crate::i18n::t!("editor-state-bad-path", "path" => path.as_str()))
                        .size(12.0)
                        .style(common::danger),
                );
            }
            paths = paths.push(line);
        }
        block = block.push(container(paths).padding(Padding {
            left: 14.0,
            ..Padding::ZERO
        }));
        block.into()
    }

    /// The Browse section: the filter box over the live store, grouped by producer.
    fn state_browse_section(&self) -> Elem<'_> {
        let filter = text_input(
            crate::i18n::ts!("editor-state-filter-placeholder"),
            &self.state_filter,
        )
        .on_input(Message::SetStateFilter)
        .size(13.0)
        .width(Length::Fixed(260.0));
        let body: Elem<'_> = match &self.catalogue {
            None => text(crate::i18n::t!("store-waiting"))
                .size(13.0)
                .style(common::muted)
                .into(),
            Some(snapshot) => {
                let producers = Self::state_producers(snapshot);
                if producers.is_empty() {
                    text(crate::i18n::t!("editor-state-empty"))
                        .size(13.0)
                        .style(common::muted)
                        .into()
                } else if self.state_filter.trim().is_empty() {
                    self.state_tree(&producers)
                } else {
                    self.state_filtered(&producers)
                }
            }
        };
        column![
            common::section_label(crate::i18n::ts!("editor-state-browse")),
            filter,
            body,
        ]
        .spacing(8.0)
        .into()
    }

    /// The producers the browser lists, in rank order: those with state. A producer has
    /// state when its tree holds a key or the catalogue carries a state handle for it
    /// (declared by `createState`, set or not). A package that publishes only events or
    /// procedures, a platform producer before its first message, and the `user` producer
    /// before any handle exists are left out.
    fn state_producers(snapshot: &CatalogueSnapshot) -> Vec<&ProducerView> {
        let mut producers: Vec<&ProducerView> = snapshot
            .producers
            .iter()
            .filter(|view| {
                view.tree
                    .as_object()
                    .is_some_and(|object| !object.is_empty())
                    || snapshot.entries.iter().any(|entry| {
                        matches!(entry.kind, CatalogueKind::State)
                            && producers_equal(&entry.producer, &view.producer)
                    })
            })
            .collect();
        producers.sort_by_cached_key(|view| producer_rank(&view.producer));
        producers
    }

    /// The store tree per producer with an expose toggle trailing every addressable row. The
    /// producer's root row is its heading: handles are the rows beneath `user` and a package,
    /// a platform producer shows its tree directly. Every node starts collapsed.
    fn state_tree<'a>(&'a self, producers: &[&'a ProducerView]) -> Elem<'a> {
        let mut rows = Column::new().spacing(2.0);
        for &view in producers {
            let producer: &'a str = &view.producer;
            let tree = TreeRows {
                toggled: &self.state_toggled,
                // Every producer starts collapsed: the browser lists what is available and
                // opens on demand, unlike the Store tab's two levels.
                default_expand_depth: 0,
                on_toggle: Message::ToggleStateNode,
                trailing: Some(Box::new(move |path: &[String]| {
                    self.state_row_toggle(producer, path)
                })),
            };
            let (label, detail) = producer_display(producer);
            let head: Elem<'a> = text(label).size(12.0).font(MONO).into();
            let head = match detail {
                Some(detail) => tip(head, detail),
                None => head,
            };
            rows = json_rows(
                &tree,
                rows,
                producer,
                String::new(),
                RowLabel::Head(head),
                &view.tree,
                0,
                &mut Vec::new(),
                true,
            );
        }
        rows.into()
    }

    /// The trailing toggle for the row at `path` beneath `producer`'s root, or `None` where
    /// nothing can be exposed (the root of a handle-bearing producer: exposures name a handle).
    fn state_row_toggle<'a>(&'a self, producer: &str, path: &[String]) -> Option<Elem<'a>> {
        let (handle, sub): (Option<&str>, &[String]) = if takes_handle(producer) {
            let (first, rest) = path.split_first()?;
            (Some(first.as_str()), rest)
        } else {
            (None, path)
        };
        let sub_path = StorePath::from_segments(sub.iter().map(String::as_str))
            .ok()?
            .to_string();
        let root = self
            .state_exposures
            .iter()
            .find(|draft| draft.is_root(producer, handle));
        let exposed = root.is_some_and(|draft| draft.path_index(&sub_path).is_some());
        let name = root.map_or_else(
            || default_name_for(producer, handle),
            StateExposureDraft::name,
        );
        let reference = reference(&name, &sub_path);
        let tooltip = if exposed {
            crate::i18n::t!("editor-state-unexpose-tooltip", "path" => reference.as_str())
        } else {
            crate::i18n::t!("editor-state-expose-tooltip", "path" => reference.as_str())
        };
        let message = Message::ToggleStateExposure {
            producer: producer.to_string(),
            handle: handle.map(str::to_owned),
            path: sub_path,
        };
        Some(tip(
            checkbox(exposed)
                .on_toggle(move |_| message.clone())
                .size(14.0)
                .into(),
            tooltip,
        ))
    }

    /// The filter's results: every addressable node whose spelled path contains the filter,
    /// as flat rows with the full path, so a match deep in a collapsed subtree is one click
    /// from exposed.
    fn state_filtered<'a>(&'a self, producers: &[&'a ProducerView]) -> Elem<'a> {
        let needle = self.state_filter.trim().to_ascii_lowercase();
        let mut budget = FILTER_VISIT_BUDGET;
        let mut matches: Vec<(&'a str, String, Vec<String>, &'a Node)> = Vec::new();
        let mut hidden = 0usize;
        for &view in producers {
            let producer: &'a str = &view.producer;
            let (label, _) = producer_display(producer);
            let mut path = Vec::new();
            collect_matches(
                &view.tree,
                &mut path,
                &needle,
                &label,
                &mut budget,
                &mut |path, node| {
                    if matches.len() >= FILTER_RESULT_CAP {
                        hidden += 1;
                    } else {
                        matches.push((producer, label.clone(), path.to_vec(), node));
                    }
                },
            );
        }
        let mut rows = Column::new().spacing(2.0);
        for (producer, label, path, node) in &matches {
            rows = rows.push(self.state_filter_row(producer, label, path, node));
        }
        if hidden > 0 {
            rows = rows.push(
                text(crate::i18n::t!("store-more-hidden", "count" => hidden))
                    .size(12.0)
                    .style(common::faint),
            );
        }
        rows.into()
    }

    /// One filter result: producer, full path, the value for a scalar, and the toggle.
    fn state_filter_row<'a>(
        &'a self,
        producer: &str,
        producer_label: &str,
        path: &[String],
        node: &'a Node,
    ) -> Elem<'a> {
        let spelled = StorePath::from_segments(path.iter().map(String::as_str))
            .map_or_else(|_| path.join("."), |p| p.to_string());
        let mut line = row![
            text(producer_label.to_string())
                .size(11.0)
                .style(common::faint),
            text(spelled).size(12.0).font(MONO),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);
        let summary = match node {
            Node::Object(object) => Some(format!("{{{}}}", object.len())),
            Node::Array(array) => Some(format!("[{}]", array.items().len())),
            _ => None,
        };
        line = line.push(match summary {
            Some(summary) => text(summary).size(11.0).style(common::faint),
            None => text(node.to_string())
                .size(12.0)
                .font(MONO)
                .style(common::muted),
        });
        if let Some(toggle) = self.state_row_toggle(producer, path) {
            line = line.push(toggle);
        }
        line.into()
    }

    /// The Add-a-path row: the producer picker, a handle field when the producer takes one
    /// (its slot stays mounted either way so the path field keeps its tree position), and the
    /// path field, submitted on Enter.
    fn state_add_row(&self) -> Elem<'_> {
        let draft = &self.state_add;
        let takes = takes_handle(&draft.producer);
        let handle = draft.handle.trim();
        let handle_slot: Elem<'_> = if takes {
            let choices = self.state_handle_choices(&draft.producer);
            if choices.is_empty() {
                // Nothing published under this producer yet (or no session): type it.
                text_input(crate::i18n::ts!("editor-state-handle"), &draft.handle)
                    .on_input(Message::SetStateHandleDraft)
                    .font(MONO)
                    .size(13.0)
                    .width(Length::Fixed(140.0))
                    .into()
            } else {
                self.state_handle_picker(&choices)
            }
        } else {
            Space::new().height(0).into()
        };
        let mut path_input = text_input(crate::i18n::ts!("editor-state-add-path"), &draft.path)
            .on_input(Message::SetStatePathDraft)
            .font(MONO)
            .size(13.0)
            .width(Length::Fixed(260.0));
        if !takes || !handle.is_empty() {
            path_input = path_input.on_submit(Message::AddStatePath {
                producer: draft.producer.clone(),
                handle: takes.then(|| handle.to_string()),
                path: draft.path.clone(),
            });
        }
        let controls = row![
            self.state_producer_picker(),
            container(handle_slot),
            path_input,
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);
        let mut block = column![controls].spacing(4.0);
        if let Some(error) = &draft.error {
            block = block.push(text(error.clone()).size(12.0).style(common::danger));
        }
        block.into()
    }

    /// The handle picker for a producer whose handles the catalogue knows: the same overlay
    /// dropdown as the producer picker, listing `choices`; the anchor shows the drafted
    /// handle or the field's placeholder.
    fn state_handle_picker<'a>(&'a self, choices: &[String]) -> Elem<'a> {
        let current = self.state_handle_position(choices);
        let handle = self.state_add.handle.trim();
        let label: Elem<'a> = if handle.is_empty() {
            text(crate::i18n::ts!("editor-state-handle"))
                .size(13.0)
                .style(common::muted)
                .into()
        } else {
            text(handle.to_string()).size(13.0).font(MONO).into()
        };
        let anchor_row = row![label, text("\u{25BE}").size(10.0).style(common::muted)]
            .spacing(8.0)
            .align_y(Vertical::Center);
        let anchor = button(anchor_row)
            .style(button_style::subtle)
            .padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 10.0,
                right: 10.0,
            })
            .on_press(if self.state_add.handle_picker_open {
                Message::CloseStateHandlePicker
            } else {
                Message::OpenStateHandlePicker
            });

        let content: Option<Elem<'a>> = self.state_add.handle_picker_open.then(|| {
            let mut list = Column::new().spacing(2.0);
            for (index, choice) in choices.iter().enumerate() {
                list = list.push(
                    button(text(choice.clone()).size(13.0).font(MONO))
                        .width(Length::Fill)
                        .style(picker_row_style(
                            index == current,
                            index == self.state_add.handle_cursor,
                        ))
                        .padding(Padding {
                            top: 6.0,
                            bottom: 6.0,
                            left: 10.0,
                            right: 10.0,
                        })
                        .on_press(Message::SetStateHandle(choice.clone())),
                );
            }
            container(list)
                .width(Length::Fixed(240.0))
                .padding(6.0)
                .style(picker_surface_style)
                .into()
        });

        let at_cursor = choices
            .get(
                self.state_add
                    .handle_cursor
                    .min(choices.len().saturating_sub(1)),
            )
            .cloned();
        Dropdown::new(anchor, content, Message::CloseStateHandlePicker)
            .on_key(move |key| match key {
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowUp) => {
                    Some(Message::MoveStateHandleCursor(-1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown) => {
                    Some(Message::MoveStateHandleCursor(1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter) => {
                    at_cursor.clone().map(Message::SetStateHandle)
                }
                _ => None,
            })
            .into()
    }

    /// The producer picker: the reusable overlay dropdown, so the list escapes the pane's
    /// scrollable; Escape, click-outside, and a pick dismiss it, up/down/Enter drive the
    /// keyboard cursor.
    fn state_producer_picker(&self) -> Elem<'_> {
        let choices = self.state_producer_choices();
        let current = self.state_producer_position(&choices);
        let (label, detail) = producer_display(&self.state_add.producer);
        let mut anchor_row = row![text(label).size(13.0).font(MONO)]
            .spacing(8.0)
            .align_y(Vertical::Center);
        if let Some(detail) = detail {
            anchor_row = anchor_row.push(text(detail).size(11.0).style(common::faint));
        }
        anchor_row = anchor_row.push(text("\u{25BE}").size(10.0).style(common::muted));
        let anchor = button(anchor_row)
            .style(button_style::subtle)
            .padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 10.0,
                right: 10.0,
            })
            .on_press(if self.state_add.picker_open {
                Message::CloseStateProducerPicker
            } else {
                Message::OpenStateProducerPicker
            });

        let content: Option<Elem<'_>> = self.state_add.picker_open.then(|| {
            let mut list = Column::new().spacing(2.0);
            for (index, choice) in choices.iter().enumerate() {
                let mut inner = row![text(choice.label.clone()).size(13.0).font(MONO)]
                    .spacing(8.0)
                    .align_y(Vertical::Center);
                if let Some(detail) = &choice.detail {
                    inner = inner.push(text(detail.clone()).size(11.0).style(common::faint));
                }
                list = list.push(
                    button(inner)
                        .width(Length::Fill)
                        .style(picker_row_style(
                            index == current,
                            index == self.state_add.cursor,
                        ))
                        .padding(Padding {
                            top: 6.0,
                            bottom: 6.0,
                            left: 10.0,
                            right: 10.0,
                        })
                        .on_press(Message::SetStateProducer(choice.key.clone())),
                );
            }
            container(list)
                .width(Length::Fixed(360.0))
                .padding(6.0)
                .style(picker_surface_style)
                .into()
        });

        let at_cursor = choices
            .get(self.state_add.cursor.min(choices.len().saturating_sub(1)))
            .map(|choice| choice.key.clone());
        Dropdown::new(anchor, content, Message::CloseStateProducerPicker)
            .on_key(move |key| match key {
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowUp) => {
                    Some(Message::MoveStateProducerCursor(-1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown) => {
                    Some(Message::MoveStateProducerCursor(1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter) => {
                    at_cursor.clone().map(Message::SetStateProducer)
                }
                _ => None,
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_spell_the_root_bare_and_paths_dotted() {
        assert_eq!(reference("gmcp", ""), "gmcp");
        assert_eq!(reference("gmcp", "Char.Vitals.hp"), "gmcp.Char.Vitals.hp");
        assert_eq!(reference("prompt", "hp"), "prompt.hp");
        assert_eq!(
            reference("gmcp", "[\"Some-Pkg\"].Msg"),
            "gmcp[\"Some-Pkg\"].Msg"
        );
        // An unparsable path still renders so the author can see what to fix.
        assert_eq!(reference("gmcp", "Char..Vitals"), "gmcp.Char..Vitals");
    }

    #[test]
    fn roots_and_paths_match_under_the_fold() {
        let draft = StateExposureDraft {
            producer: "smudgy://Kapusniak/Arctic-Prompt".to_string(),
            handle: Some("Prompt".to_string()),
            name_field: None,
            paths: vec!["Char.Vitals".to_string()],
        };
        assert!(draft.is_root("kapusniak/arctic-prompt", Some("prompt")));
        assert!(!draft.is_root("smudgy://kapusniak/arctic-prompt", None));
        assert!(!draft.is_root("user", Some("prompt")));
        assert_eq!(draft.path_index("char.vitals"), Some(0));
        assert_eq!(draft.path_index("Char.Vitals.hp"), None);
        // The default name keeps the handle's own spelling.
        assert_eq!(draft.name(), "Prompt");
    }

    #[test]
    fn a_blank_rename_field_is_not_an_override() {
        let mut draft = StateExposureDraft {
            producer: "user".to_string(),
            handle: Some("foo".to_string()),
            name_field: Some("   ".to_string()),
            paths: vec![String::new()],
        };
        assert_eq!(draft.to_exposure().name_override, None);
        assert_eq!(draft.name(), "foo");
        draft.name_field = Some(" stats ".to_string());
        assert_eq!(draft.to_exposure().name_override.as_deref(), Some("stats"));
        assert_eq!(draft.name(), "stats");
        // The default name typed back is not an override either; JavaScript spelling is
        // exact, so another casing of it is.
        draft.name_field = Some("foo".to_string());
        assert_eq!(draft.to_exposure().name_override, None);
        assert_eq!(draft.name(), "foo");
        draft.name_field = Some("Foo".to_string());
        assert_eq!(draft.to_exposure().name_override.as_deref(), Some("Foo"));
        let platform = StateExposureDraft {
            producer: "gmcp".to_string(),
            handle: None,
            name_field: Some(" gmcp ".to_string()),
            paths: vec![String::new()],
        };
        assert_eq!(platform.to_exposure().name_override, None);
        assert_eq!(platform.default_name(), "gmcp");
    }

    #[test]
    fn notes_block_reserved_words_and_flag_known_globals_per_language() {
        let names: Vec<String> = ["class", "stats", "gmcp", "Math", "prompt", "9x", "target"]
            .into_iter()
            .map(String::from)
            .collect();
        let captures = ["target".to_string()];

        // A script body sees the three shadowing buckets and no capture collision.
        let script = state_notes_for(&names, &captures, Some(ScriptLang::JS));
        assert_eq!(script[0], vec![StateNote::ReservedName("class".into())]);
        assert!(script[0][0].blocks_save());
        assert!(
            script[1].is_empty(),
            "an ordinary name gets no note: {:?}",
            script[1]
        );
        assert_eq!(script[2], vec![StateNote::ShadowsSmudgy("gmcp".into())]);
        assert_eq!(script[3], vec![StateNote::ShadowsJavaScript("Math".into())]);
        assert_eq!(script[4], vec![StateNote::ShadowsDeno("prompt".into())]);
        assert!(!script[4][0].blocks_save());
        assert_eq!(script[5], vec![StateNote::BadName("9x".into())]);
        assert!(script[6].is_empty());
        assert_eq!(
            state_notes_for(&names, &captures, Some(ScriptLang::TS)),
            script
        );

        // Send text sees only the capture collision; nothing shadows in a text body.
        let send_text = state_notes_for(&names, &captures, Some(ScriptLang::Plaintext));
        assert_eq!(send_text[0], script[0]);
        assert!(send_text[2].is_empty());
        assert!(send_text[3].is_empty());
        assert!(send_text[4].is_empty());
        assert_eq!(
            send_text[6],
            vec![StateNote::ShadowsCapture("target".into())]
        );

        // No open editor: the blocking notes only.
        let none = state_notes_for(&names, &captures, None);
        assert_eq!(none[0], script[0]);
        assert_eq!(none[5], script[5]);
        for index in [1, 2, 3, 4, 6] {
            assert!(none[index].is_empty(), "{index}: {:?}", none[index]);
        }

        // A duplicate under the fold marks the later entry.
        let dup = state_notes_for(&["hp".to_string(), "HP".to_string()], &[], None);
        assert!(dup[0].is_empty());
        assert_eq!(dup[1], vec![StateNote::NameTaken("HP".into())]);
    }

    #[test]
    fn producers_display_packages_by_name_and_rank_user_first() {
        assert_eq!(
            producer_display("smudgy://kapusniak/arctic-prompt"),
            (
                "arctic-prompt".to_string(),
                Some("smudgy://kapusniak/arctic-prompt".to_string())
            )
        );
        assert_eq!(producer_display("GMCP"), ("gmcp".to_string(), None));
        let mut producers = vec!["smudgy://a/b", "mssp", "user", "gmcp", "msdp"];
        producers.sort_by_cached_key(|producer| producer_rank(producer));
        assert_eq!(producers, ["user", "gmcp", "msdp", "mssp", "smudgy://a/b"]);
    }

    fn draft(
        producer: &str,
        handle: Option<&str>,
        name: Option<&str>,
        paths: &[&str],
    ) -> StateExposureDraft {
        StateExposureDraft {
            producer: producer.to_string(),
            handle: handle.map(str::to_string),
            name_field: name.map(str::to_string),
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn state_references_render_per_tab_and_brace_bracket_keys() {
        let drafts = vec![
            draft(
                "gmcp",
                None,
                None,
                &["Char.Vitals.hp", "", "[\"Some-Pkg\"].Msg"],
            ),
            draft("user", Some("foo"), Some("stats"), &["bar"]),
        ];
        // With no snapshot every badge is a bare reference.
        let references = |language: ScriptLang| -> Vec<String> {
            rail_entries(&drafts, language, None)
                .into_iter()
                .map(|(reference, value)| {
                    assert_eq!(value, None, "{reference}");
                    reference
                })
                .collect()
        };
        assert_eq!(
            references(ScriptLang::Plaintext),
            [
                "$gmcp.Char.Vitals.hp",
                "$gmcp",
                "${gmcp[\"Some-Pkg\"].Msg}",
                "$stats.bar",
            ]
        );
        let script = [
            "gmcp.Char.Vitals.hp",
            "gmcp",
            "gmcp[\"Some-Pkg\"].Msg",
            "stats.bar",
        ];
        assert_eq!(references(ScriptLang::JS), script);
        assert_eq!(references(ScriptLang::TS), script);
        assert!(rail_entries(&[], ScriptLang::Plaintext, None).is_empty());
    }

    /// A path that does not parse is flagged under Exposed and inserts nothing, so it gets
    /// no badge; the badges around it keep their own values, and the highlighter's exposed
    /// paths count the same entries the rail shows.
    #[test]
    fn unparsable_paths_get_no_badge_and_leave_the_values_aligned() {
        let snapshot = CatalogueSnapshot {
            producers: vec![ProducerView {
                producer: "gmcp".to_string(),
                tree: Node::from(serde_json::json!({
                    "Char": { "Vitals": { "hp": 100, "sp": 50 } }
                })),
                entries: 0,
                bytes: 0,
            }],
            entries: Vec::new(),
        };
        let drafts = vec![draft(
            "gmcp",
            None,
            None,
            &["Char..Vitals", "Char.Vitals.hp", "Char.Vitals.sp"],
        )];
        assert_eq!(
            rail_entries(&drafts, ScriptLang::Plaintext, Some(&snapshot)),
            [
                ("$gmcp.Char.Vitals.hp".to_string(), Some("100".to_string())),
                ("$gmcp.Char.Vitals.sp".to_string(), Some("50".to_string())),
            ]
        );

        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "unparsable-path-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.state_exposures = drafts;
        assert_eq!(
            window.exposed_paths().len(),
            window.state_rail_entries(ScriptLang::Plaintext).len()
        );
        // A root whose only path is unparsable exposes no name: no badge, and the
        // highlighter treats `$gmcp` as it would any unknown capture.
        window.state_exposures = vec![draft("gmcp", None, None, &["Char..Vitals"])];
        assert!(window.exposed_paths().is_empty());
        assert!(window.state_rail_entries(ScriptLang::Plaintext).is_empty());
    }

    #[test]
    fn value_previews_walk_the_snapshot_under_the_fold() {
        let snapshot = CatalogueSnapshot {
            producers: vec![
                ProducerView {
                    producer: "gmcp".to_string(),
                    tree: Node::from(serde_json::json!({
                        "Char": {
                            "Vitals": {
                                "hp": 100,
                                "name": "Bob",
                                "alive": true,
                                "tags": [1, 2],
                                "gone": null,
                                "long": "x".repeat(40),
                            }
                        }
                    })),
                    entries: 0,
                    bytes: 0,
                },
                ProducerView {
                    producer: "smudgy://kapusniak/arctic-prompt".to_string(),
                    tree: Node::from(serde_json::json!({ "prompt": { "hp": 7 } })),
                    entries: 0,
                    bytes: 0,
                },
            ],
            entries: Vec::new(),
        };
        let gmcp = draft("GMCP", None, None, &[]);
        let preview =
            |draft: &StateExposureDraft, path: &str| state_value_preview(&snapshot, draft, path);
        // Scalars in their JSON spelling, containers summarized, the root included.
        assert_eq!(preview(&gmcp, "char.vitals.HP").as_deref(), Some("100"));
        assert_eq!(
            preview(&gmcp, "Char.Vitals.name").as_deref(),
            Some("\"Bob\"")
        );
        assert_eq!(preview(&gmcp, "Char.Vitals.alive").as_deref(), Some("true"));
        assert_eq!(preview(&gmcp, "Char.Vitals.gone").as_deref(), Some("null"));
        assert_eq!(preview(&gmcp, "Char.Vitals.tags").as_deref(), Some("[…]"));
        assert_eq!(preview(&gmcp, "Char.Vitals").as_deref(), Some("{…}"));
        assert_eq!(preview(&gmcp, "").as_deref(), Some("{…}"));
        let long = preview(&gmcp, "Char.Vitals.long").unwrap();
        assert_eq!(long.chars().count(), VALUE_PREVIEW_CHARS + 1);
        assert!(long.ends_with('…'));
        // Nothing at the path, an unparsable path, or an unknown producer.
        assert_eq!(preview(&gmcp, "Room.Info"), None);
        assert_eq!(preview(&gmcp, "Char..Vitals"), None);
        assert_eq!(preview(&draft("msdp", None, None, &[]), ""), None);
        // A handle root walks the handle first, producer and handle folded.
        let prompt = draft(
            "smudgy://Kapusniak/Arctic-Prompt",
            Some("Prompt"),
            None,
            &[],
        );
        assert_eq!(preview(&prompt, "hp").as_deref(), Some("7"));
        assert_eq!(preview(&prompt, "").as_deref(), Some("{…}"));
        assert_eq!(
            preview(
                &draft("smudgy://kapusniak/arctic-prompt", Some("nope"), None, &[]),
                ""
            ),
            None
        );
    }
}
