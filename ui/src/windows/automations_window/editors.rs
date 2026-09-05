//! The script editors (alias / trigger / hotkey), the folder editor, and the
//! module pane — both the update-side logic and the views.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use iced::alignment::Vertical;
use iced::keyboard::Key;
use iced::widget::Id;
use iced::widget::{
    Column, Space, button, checkbox, column, container, pick_list, radio, row, text, text_editor,
    text_input,
};
use iced::{Element, Font, Length, Padding, Task};

use smudgy_core::models::matchers::{
    self, ArgKind, CmdMode, CommandOutcome, CommandSpec, MatcherColor, MatcherColorChannel,
    MatcherColorMatch, MatcherHsv, MatcherHsvRange, MatcherSyntax, MatcherTextAttribute,
};
use smudgy_core::models::modules::ModuleFileWriteOutcome;
use smudgy_core::models::profile_activation::ProfileActivation;
use smudgy_core::models::server;
use smudgy_core::models::shared_packages::LockedPackage;
use smudgy_core::models::{ScriptLang, aliases, hotkeys, naming, packages, triggers};
use smudgy_core::session::runtime::AutomationKind;

use crate::assets::{bootstrap_icons, fonts};
use crate::components::color_picker::ColorPicker;
use crate::keymap::{self as hotkey_helpers, MaybePhysicalKey};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::update::Update;
use crate::widgets::dropdown::Dropdown;
use crate::widgets::hotkey_input::HotkeyInput;
use crate::widgets::wrap_row::wrap_row;

use super::code_editor;
use super::common;
use super::highlight;
use super::keyboard_control::{
    KeyAction, KeyboardControl, activation, grid_selection, linear_selection, publish_selection,
};
use super::model::{
    AliasKind, AliasMatcherDraft, ArgKindChoice, AutomationSaveStatus, ColorRangeEndpoint,
    MatcherColorKind, NodeStatus, ParseModeChoice, PatternKind, Script, ScriptKey, SyntaxChoice,
    TriggerCard, TriggerRow, TruecolorComponent, parse_matcher_hex, pattern_error_text,
    rows_into_trigger, trigger_rows, upsert_script_folder,
};
use super::{
    AutomationsWindow, EditNode, EditorMode, EditorState, Elem, Event, FolderState, Message,
    ModuleMode, ModuleState, ModuleTab, Pane, Selection, matcher_hsv_to_picker,
    matcher_truecolor_range,
};

const LABEL_WIDTH: f32 = 92.0;

/// A destination choice for the editor's folder picker: top level, or a folder
/// path. Wraps `Option<String>` so it satisfies the `Clone + Display + PartialEq`
/// `pick_list` requires, with `None`/top level rendered as a friendly sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FolderChoice {
    TopLevel,
    Folder(String),
}

impl FolderChoice {
    fn from_package(package: Option<&str>) -> Self {
        match package {
            Some(path) if !path.is_empty() => FolderChoice::Folder(path.to_string()),
            _ => FolderChoice::TopLevel,
        }
    }

    fn into_package(self) -> Option<String> {
        match self {
            FolderChoice::TopLevel => None,
            FolderChoice::Folder(path) => Some(path),
        }
    }
}

impl std::fmt::Display for FolderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FolderChoice::TopLevel => f.write_str(&crate::i18n::t!("editor-top-level")),
            FolderChoice::Folder(path) => f.write_str(path),
        }
    }
}

/// Logs `msg` and returns an empty update (used for non-fatal save failures).
fn warn_none(msg: String) -> Update<Message, Event> {
    log::warn!("{msg}");
    Update::none()
}

fn action_tab_key_action(
    key: &Key,
    language: ScriptLang,
    script_language: ScriptLang,
) -> KeyAction<Message> {
    let current = usize::from(language != ScriptLang::Plaintext);
    publish_selection(linear_selection(key, current, 2), |index| {
        Message::SetBehavior(if index == 0 {
            ScriptLang::Plaintext
        } else {
            script_language
        })
    })
}

fn activation_control_key_action(key: &Key, repeat: bool, message: Message) -> KeyAction<Message> {
    activation(key, repeat, message)
}

fn keyboard_activation_control<'a>(content: Elem<'a>, id: Id, message: Message) -> Elem<'a> {
    let focus_id = id.clone();
    KeyboardControl::new(
        content,
        id,
        move || Message::FocusColorControl(focus_id.clone()),
        move |key, repeat| activation_control_key_action(key, repeat, message.clone()),
    )
    .into()
}

// ============================================================================
// Update-side: open / create / save / delete
// ============================================================================

impl AutomationsWindow {
    pub(super) fn open_script(&mut self, key: ScriptKey) -> Update<Message, Event> {
        let Some(script) = self.find_script(&key) else {
            return Update::none();
        };
        self.clear_selection();
        self.selection = Selection::Script(key.clone());
        self.test_input.clear();
        self.order_revealed = false;
        self.try_it_open = false;
        self.parsing_open = false;

        let editor_task = match &script {
            Script::Alias(a) => self.seed_action_buffers(
                a.language,
                a.script.as_deref(),
                code_editor::CodeDocument::Alias,
            ),
            Script::Trigger(t) => self.seed_action_buffers(
                t.language,
                t.script.as_deref(),
                code_editor::CodeDocument::Trigger,
            ),
            Script::Hotkey(h) => {
                self.action_script_lang = if h.language == ScriptLang::TS {
                    ScriptLang::TS
                } else {
                    ScriptLang::JS
                };
                let text = h.script.as_deref().unwrap_or_default();
                if h.language == ScriptLang::Plaintext {
                    self.hotkey_text_content = text_editor::Content::with_text(text);
                    Task::none()
                } else {
                    self.hotkey_text_content = text_editor::Content::new();
                    self.bind_code_editor(
                        text,
                        code_editor::script_language(h.language),
                        code_editor::CodeDocument::Hotkey,
                    )
                }
            }
            Script::Folder(_, _) => return Update::none(),
        };

        let node = match script {
            Script::Alias(a) => {
                self.alias_draft = AliasMatcherDraft::from_definition(&a, &key.script_name);
                if self.alias_draft.degraded {
                    log::info!(
                        "alias {}: stored pattern no longer matches its sidecar; showing as regex",
                        key.script_name
                    );
                }
                self.alias_pattern_content =
                    text_editor::Content::with_text(&self.alias_draft.pattern_source);
                self.alias_regex_content =
                    text_editor::Content::with_text(&self.alias_draft.regex_source);
                EditNode::Alias(a)
            }
            Script::Hotkey(h) => {
                self.hotkey_state = hotkey_definition_to_keys(&h);
                EditNode::Hotkey(h)
            }
            Script::Trigger(t) => {
                let rows = trigger_rows(&t);
                self.trigger_row_contents = rows
                    .iter()
                    .map(|row| text_editor::Content::with_text(&row.source))
                    .collect();
                EditNode::Trigger {
                    enabled: t.enabled,
                    language: t.language,
                    prompt: t.prompt,
                    priority: t.priority,
                    fallthrough: t.fallthrough,
                    package: t.package.clone(),
                    rows,
                }
            }
            Script::Folder(_, _) => return Update::none(),
        };
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Edit,
            original_name: Some(key.script_name.clone()),
            name: key.script_name,
            node,
            error: None,
        });
        // The inactive action tab starts with a generated example body.
        let generated_task = self.refresh_generated_actions();
        Update::with_task(Task::batch([editor_task, generated_task]))
    }

    /// Seeds the two action drafts from a stored automation: the stored body
    /// lands in its language's tab, pinned (saved work is never regenerated
    /// over — a file-backed body, `script: None`, pins as empty for the same
    /// reason); the other tab starts unpinned so it can carry a generated
    /// example until edited.
    fn seed_action_buffers(
        &mut self,
        language: ScriptLang,
        body: Option<&str>,
        kind: code_editor::CodeDocument,
    ) -> Task<Message> {
        self.action_script_lang = if language == ScriptLang::TS {
            ScriptLang::TS
        } else {
            ScriptLang::JS
        };
        let stored = body.unwrap_or_default();
        if language == ScriptLang::Plaintext {
            self.send_text_content = text_editor::Content::with_text(stored);
            self.action_text_pinned = true;
            self.action_script_pinned = false;
            self.bind_code_editor(
                "",
                code_editor::script_language(self.action_script_lang),
                kind,
            )
        } else {
            self.send_text_content = text_editor::Content::new();
            self.action_script_pinned = true;
            self.action_text_pinned = false;
            self.bind_code_editor(stored, code_editor::script_language(language), kind)
        }
    }

    /// Resets both action drafts for a create pane: unpinned, so they carry
    /// generated example bodies until the user edits one.
    fn reset_action_buffers(&mut self) {
        self.send_text_content = text_editor::Content::new();
        self.action_text_pinned = false;
        self.action_script_pinned = false;
        self.action_script_lang = ScriptLang::JS;
    }

    /// Regenerates any unpinned action draft from the live matcher
    /// (`matching-logic.md` §8): until the user edits a draft, its example
    /// body tracks the captures, so it can never reference one that doesn't
    /// exist. Any edit pins the draft and ends the tracking.
    pub(super) fn refresh_generated_actions(&mut self) -> Task<Message> {
        let computed = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Alias(_),
                ..
            }) => {
                let kind = if self.alias_draft.kind == AliasKind::Pattern {
                    ExampleKind::AliasEmote
                } else {
                    ExampleKind::AliasSay
                };
                Some((
                    kind,
                    self.alias_capture_references(ScriptLang::Plaintext),
                    self.alias_capture_references(ScriptLang::JS),
                ))
            }
            Pane::Editor(EditorState {
                node: EditNode::Trigger { rows, .. },
                ..
            }) => Some((
                ExampleKind::Trigger,
                Self::trigger_capture_references(rows, ScriptLang::Plaintext),
                Self::trigger_capture_references(rows, ScriptLang::JS),
            )),
            _ => None,
        };
        let Some((kind, text_references, script_references)) = computed else {
            return Task::none();
        };
        if !self.action_text_pinned {
            self.send_text_content = text_editor::Content::with_text(&generated_body(
                kind,
                text_references.first().map(String::as_str),
                false,
            ));
        }
        if !self.action_script_pinned {
            let generated =
                generated_body(kind, script_references.first().map(String::as_str), true);
            let kind = match &self.pane {
                Pane::Editor(EditorState {
                    node: EditNode::Alias(_),
                    ..
                }) => code_editor::CodeDocument::Alias,
                Pane::Editor(EditorState {
                    node: EditNode::Trigger { .. },
                    ..
                }) => code_editor::CodeDocument::Trigger,
                _ => return Task::none(),
            };
            return self.bind_code_editor(
                &generated,
                code_editor::script_language(self.action_script_lang),
                kind,
            );
        }
        Task::none()
    }

    pub(super) fn new_alias(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.reset_action_buffers();
        self.test_input.clear();
        self.order_revealed = false;
        self.try_it_open = false;
        self.parsing_open = false;
        // Command is the default kind for new aliases.
        self.alias_draft = AliasMatcherDraft::default();
        self.alias_pattern_content = text_editor::Content::new();
        self.alias_regex_content = text_editor::Content::new();
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: String::new(),
            node: EditNode::Alias(aliases::AliasDefinition {
                pattern: String::new(),
                script: None,
                package: self.current_folder(),
                enabled: true,
                priority: 0,
                fallthrough: true,
                allow_self_match: false,
                language: ScriptLang::Plaintext,
                matcher: None,
            }),
            error: None,
        });
        Update::with_task(self.refresh_generated_actions())
    }

    pub(super) fn new_trigger(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.reset_action_buffers();
        self.test_input.clear();
        self.order_revealed = false;
        self.try_it_open = false;
        self.parsing_open = false;
        // No rows yet: the pane opens at the unselected-cards state.
        self.trigger_row_contents = Vec::new();
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: String::new(),
            node: EditNode::Trigger {
                enabled: true,
                language: ScriptLang::Plaintext,
                prompt: false,
                priority: 0,
                fallthrough: true,
                package: self.current_folder(),
                rows: Vec::new(),
            },
            error: None,
        });
        Update::with_task(self.refresh_generated_actions())
    }

    pub(super) fn new_hotkey(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.action_script_lang = ScriptLang::JS;
        self.hotkey_text_content = text_editor::Content::new();
        self.hotkey_state.clear();
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: String::new(),
            node: EditNode::Hotkey(hotkeys::HotkeyDefinition {
                key: String::new(),
                modifiers: vec![],
                script: None,
                package: self.current_folder(),
                language: ScriptLang::Plaintext,
                enabled: true,
            }),
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_folder(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.pane = Pane::Folder(FolderState {
            mode: EditorMode::Create,
            original_path: None,
            path: self
                .current_folder()
                .map(|p| format!("{p}/"))
                .unwrap_or_default(),
            activation: ProfileActivation::All,
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_module(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.module_source_baseline = None;
        self.selection = Selection::None;
        let text = "// A local module for shared helpers and automation setup.\n";
        self.pane = Pane::Module(ModuleState {
            mode: ModuleMode::Create,
            subpath: String::new(),
            path: None,
            name: String::new(),
            tab: ModuleTab::Source,
            activation: ProfileActivation::All,
            activation_touched: false,
            error: None,
        });
        Update::with_task(self.bind_code_editor(
            text,
            code_editor::path_language("new.ts"),
            code_editor::CodeDocument::StandaloneModule,
        ))
    }

    pub(super) fn open_folder(&mut self, path: String) -> Update<Message, Event> {
        self.clear_selection();
        let folder_paths = packages::collect_folder_paths(&self.packages);
        let stored_path = packages::canonical_folder_path(&self.packages, &path);
        let ambiguous_case = stored_path.is_none()
            && folder_paths
                .iter()
                .any(|existing| naming::names_conflict(existing, &path));
        let activation = packages::folder_activation(
            &self.packages,
            stored_path.as_deref().unwrap_or(path.as_str()),
        );
        let needs_legacy_repair =
            self.folder_state_error.is_none() && stored_path.is_none() && !ambiguous_case;
        let mut repaired = false;
        let mut repair_conflict = false;
        let mut repair_error =
            ambiguous_case.then(|| crate::i18n::t!("automation-folder-case-ambiguous"));
        if needs_legacy_repair {
            // Script package fields predate explicit folder rows. Preserve their historical
            // enabled behavior by materializing the missing All row as soon as the user opens it;
            // otherwise the pane would say All while runtime correctly fails the missing path
            // closed, and the already-selected Enable Everywhere action could not be pressed.
            let previous = self.packages.clone();
            packages::insert_folder(&mut self.packages, &path);
            match self.serialize_scripts() {
                Ok(AutomationSaveStatus::Saved) => repaired = true,
                Ok(AutomationSaveStatus::Conflict) => {
                    self.packages = previous;
                    repair_conflict = true;
                }
                Err(error) => {
                    self.packages = previous;
                    repair_error = Some(error.to_string());
                }
            }
        }
        self.selection = Selection::Folder(path.clone());
        self.pane = Pane::Folder(FolderState {
            mode: EditorMode::Edit,
            original_path: Some(path.clone()),
            path,
            activation,
            error: repair_error,
        });
        let task = if repair_conflict {
            self.automation_save_conflict_task()
        } else {
            Task::none()
        };
        Update::new(
            task,
            repaired.then_some(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn open_module(&mut self, subpath: String) -> Update<Message, Event> {
        self.clear_selection();
        let path = self
            .modules
            .iter()
            .find(|m| m.subpath == subpath)
            .map(|m| m.path.clone());
        self.selection = Selection::Module(subpath.clone());
        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    self.module_source_baseline = Some(content.clone());
                    let language = code_editor::path_language(&subpath);
                    let tab = if smudgy_core::models::modules::is_script_module(&subpath) {
                        ModuleTab::Settings
                    } else {
                        ModuleTab::Source
                    };
                    let activation =
                        smudgy_core::models::modules::activation(&self.module_settings, &subpath);
                    self.pane = Pane::Module(ModuleState {
                        mode: ModuleMode::View,
                        subpath,
                        path: Some(path),
                        name: String::new(),
                        tab,
                        activation,
                        activation_touched: false,
                        error: None,
                    });
                    return Update::with_task(self.bind_code_editor(
                        &content,
                        language,
                        code_editor::CodeDocument::StandaloneModule,
                    ));
                }
                Err(e) => {
                    self.pane = Pane::Error(Arc::new(vec![crate::i18n::t!(
                        "editor-failed-read",
                        "path" => subpath,
                        "error" => e.to_string()
                    )]));
                }
            }
        }
        Update::none()
    }

    /// The currently-selected folder, used to pre-place a new item.
    fn current_folder(&self) -> Option<String> {
        match &self.selection {
            Selection::Folder(path) => Some(path.clone()),
            Selection::Script(key) => key.folder_name.clone(),
            _ => None,
        }
    }

    /// Toggle the enable state of the node open in the editor (alias/trigger/
    /// hotkey/folder) — the single enable switch.
    pub(super) fn toggle_open_enabled(&mut self) -> Update<Message, Event> {
        let original_name = match &mut self.pane {
            Pane::Editor(state) => {
                let now = match &mut state.node {
                    EditNode::Alias(a) => {
                        a.enabled = !a.enabled;
                        a.enabled
                    }
                    EditNode::Hotkey(h) => {
                        h.enabled = !h.enabled;
                        h.enabled
                    }
                    EditNode::Trigger { enabled, .. } => {
                        *enabled = !*enabled;
                        *enabled
                    }
                };
                if state.mode == EditorMode::Create {
                    self.dirty = true;
                    return Update::none();
                }
                let Some(name) = state.original_name.clone() else {
                    return Update::none();
                };
                (name, now)
            }
            Pane::Folder(_) => return self.toggle_folder_enabled(),
            _ => return Update::none(),
        };
        let (name, enabled) = original_name;
        match self.persist_script_metadata(&name, move |script| match script {
            Script::Alias(alias) => alias.enabled = enabled,
            Script::Hotkey(hotkey) => hotkey.enabled = enabled,
            Script::Trigger(trigger) => trigger.enabled = enabled,
            Script::Folder(_, _) => {}
        }) {
            Ok(update) => update,
            Err(error) => {
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(alias) => alias.enabled = !enabled,
                        EditNode::Hotkey(hotkey) => hotkey.enabled = !enabled,
                        EditNode::Trigger {
                            enabled: current, ..
                        } => *current = !enabled,
                    }
                    state.error = Some(error);
                }
                Update::none()
            }
        }
    }

    /// Move the open script into `folder` (`None` = top level). In edit mode this
    /// re-homes and persists immediately without serializing either action draft. In create
    /// mode it only records the choice; it's applied when the user clicks Create.
    /// The palette's "Move to…" group routes here too: the selected script is the
    /// one open in the editor, so this single handler drives both surfaces.
    pub(super) fn set_script_folder(&mut self, folder: Option<String>) -> Update<Message, Event> {
        // Normalize an empty path to top level so a stray "" never becomes a folder.
        let folder = folder.filter(|p| !p.is_empty());
        let (original_name, previous_folder) = match &mut self.pane {
            Pane::Editor(state) => {
                let previous = match &mut state.node {
                    EditNode::Alias(alias) => std::mem::replace(&mut alias.package, folder.clone()),
                    EditNode::Hotkey(hotkey) => {
                        std::mem::replace(&mut hotkey.package, folder.clone())
                    }
                    EditNode::Trigger { package, .. } => std::mem::replace(package, folder.clone()),
                };
                if state.mode == EditorMode::Create {
                    self.dirty = true;
                    return Update::none();
                }
                (state.original_name.clone(), previous)
            }
            _ => return Update::none(),
        };
        let Some(name) = original_name else {
            return Update::none();
        };
        match self.persist_script_metadata(&name, move |script| match script {
            Script::Alias(alias) => alias.package = folder.clone(),
            Script::Hotkey(hotkey) => hotkey.package = folder.clone(),
            Script::Trigger(trigger) => trigger.package = folder,
            Script::Folder(_, _) => {}
        }) {
            Ok(update) => update,
            Err(error) => {
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(alias) => alias.package = previous_folder.clone(),
                        EditNode::Hotkey(hotkey) => hotkey.package = previous_folder.clone(),
                        EditNode::Trigger { package, .. } => *package = previous_folder,
                    }
                    state.error = Some(error);
                }
                Update::none()
            }
        }
    }

    /// Persists one immediate metadata mutation against the stored script while
    /// leaving all open authoring drafts and the dirty flag untouched.
    fn persist_script_metadata(
        &mut self,
        name: &str,
        update: impl FnOnce(&mut Script),
    ) -> Result<Update<Message, Event>, String> {
        let key = ScriptKey {
            folder_name: self.find_script_folder(name),
            script_name: name.to_owned(),
        };
        let Some(mut script) = self.find_script(&key) else {
            return Err(crate::i18n::t!("editor-script-missing", "name" => name));
        };
        update(&mut script);
        let folder_name = script.folder_name().map(str::to_owned);
        let previous_scripts = self.scripts.clone();
        self.remove_script_by_name(name);
        let folder = match upsert_script_folder(&mut self.scripts, folder_name.as_deref()) {
            Ok(folder) => folder,
            Err(error) => {
                self.scripts = previous_scripts;
                return Err(error);
            }
        };
        folder.insert(name.to_owned(), script);
        match self.serialize_scripts() {
            Ok(AutomationSaveStatus::Saved) => {}
            Ok(AutomationSaveStatus::Conflict) => {
                self.scripts = previous_scripts;
                return Ok(self.automation_save_conflict());
            }
            Err(error) => {
                self.scripts = previous_scripts;
                return Err(crate::i18n::t!(
                    "editor-failed-save",
                    "error" => error.to_string()
                ));
            }
        }
        self.selection = Selection::Script(ScriptKey {
            folder_name,
            script_name: name.to_owned(),
        });
        let toast = self.show_toast(crate::i18n::t!("editor-saved", "name" => name));
        Ok(Update::new(
            toast,
            Some(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            }),
        ))
    }

    fn toggle_folder_enabled(&mut self) -> Update<Message, Event> {
        let enabled = match &self.pane {
            Pane::Folder(state) => state.activation.is_enabled_for(&self.profile_name),
            _ => return Update::none(),
        };
        self.set_open_activation(if enabled {
            ProfileActivation::None
        } else {
            ProfileActivation::All
        })
    }

    pub(super) fn toggle_open_activation_profile(
        &mut self,
        profile_name: String,
    ) -> Update<Message, Event> {
        if !self.open_activation_storage_available() {
            return Update::none();
        }
        let Some(current) = self.open_activation() else {
            return Update::none();
        };
        // Canonicalizing a selected set against the profiles known at open time would turn
        // "every profile I could see" into `All`, silently enabling a profile created since. Read
        // the inventory again right before the write and refuse it when the read fails.
        if let Err(reason) = self.refresh_profile_inventory() {
            match &mut self.pane {
                Pane::Folder(state) => state.error = Some(reason),
                Pane::Module(state) => state.error = Some(reason),
                Pane::InstalledPackage | Pane::OwnedPackage => self.manage_feedback = Some(reason),
                _ => {}
            }
            return Update::none();
        }
        let known = self.profile_names.iter().cloned().collect::<BTreeSet<_>>();
        let enabled = current.is_enabled_for(&profile_name);
        self.set_open_activation(current.with_profile(&profile_name, !enabled, &known))
    }

    /// Replaces the profile inventory with a fresh strict read of the server's profiles.
    ///
    /// # Errors
    /// Returns the user-facing reason when any profile cannot be read; the previous inventory is
    /// kept for display but marked incomplete so no activation write canonicalizes against it.
    pub(super) fn refresh_profile_inventory(&mut self) -> Result<(), String> {
        match Self::load_profile_choices(&self.server_name) {
            Ok(profile_names) => {
                self.profile_names = profile_names;
                self.profile_inventory_complete = true;
                Ok(())
            }
            Err(error) => {
                self.profile_inventory_complete = false;
                log::warn!(
                    "Failed to load the complete profile inventory for {}: {error}",
                    self.server_name
                );
                Err(crate::i18n::t!("activation-profile-inventory-error"))
            }
        }
    }

    fn open_activation(&self) -> Option<ProfileActivation> {
        match &self.pane {
            Pane::Folder(state) => Some(state.activation.clone()),
            Pane::Module(state) => Some(state.activation.clone()),
            Pane::InstalledPackage => self.installed_open.as_deref().map(|package| {
                let governing = self.governing_specifier(&package.specifier);
                self.installed_packages
                    .iter()
                    .find(|locked| locked.specifier == governing)
                    .map_or_else(|| package.activation(), LockedPackage::activation)
            }),
            Pane::OwnedPackage => self.local_package.as_ref().map(|package| {
                let specifier = self.local_own_spec(&package.name);
                self.installed_packages
                    .iter()
                    .find(|locked| locked.specifier == specifier)
                    .map_or(ProfileActivation::None, LockedPackage::activation)
            }),
            _ => None,
        }
    }

    /// Whether the open pane's activation storage accepts writes. Derived from
    /// [`Self::open_activation_storage_error`] so the controls the view enables are exactly the
    /// writes the model performs.
    pub(super) fn open_activation_storage_available(&self) -> bool {
        self.open_activation_storage_error().is_none()
    }

    /// The single reason activation writes are refused for the open pane, shown by the view in
    /// place of enabled controls. Every activation write consults this same predicate.
    pub(super) fn open_activation_storage_error(&self) -> Option<String> {
        match &self.pane {
            Pane::Folder(_) if self.folder_state_error.is_some() => {
                Some(crate::i18n::t!("activation-folder-state-error"))
            }
            // A failed folder write leaves its explanation in the pane; the activation rows must
            // not advertise a write that would fail the same way.
            Pane::Folder(state) if state.error.is_some() => {
                Some(crate::i18n::t!("activation-folder-error-blocked"))
            }
            Pane::Module(_) if self.module_state_error.is_some() => {
                Some(crate::i18n::t!("activation-module-state-error"))
            }
            Pane::InstalledPackage | Pane::OwnedPackage => self
                .local_package_state_error
                .clone()
                .or_else(|| self.installed_package_state_error.clone()),
            _ => None,
        }
    }

    /// Unsaved executable/configuration drafts must not be confused with the code and values that
    /// a newly enabled profile will actually load. Disabling remains available as a safety action.
    fn activation_enable_block_reason(&self) -> Option<String> {
        match &self.pane {
            Pane::Module(state) if state.mode != ModuleMode::Create && self.dirty => {
                Some(crate::i18n::t!("module-save-before-activation"))
            }
            Pane::OwnedPackage
                if self.dirty
                    || self.manifest_dirty
                    || self
                        .param_config
                        .as_ref()
                        .is_some_and(|config| !config.touched.is_empty()) =>
            {
                Some(crate::i18n::t!("package-save-before-activation"))
            }
            Pane::InstalledPackage
                if self
                    .param_config
                    .as_ref()
                    .is_some_and(|config| !config.touched.is_empty()) =>
            {
                Some(crate::i18n::t!("package-save-before-activation"))
            }
            _ => None,
        }
    }

    fn activation_change_enables_more_profiles(
        &self,
        current: &ProfileActivation,
        requested: &ProfileActivation,
    ) -> bool {
        let enables_known_profile = self
            .profile_names
            .iter()
            .any(|profile| !current.is_enabled_for(profile) && requested.is_enabled_for(profile));
        let enables_future_profiles = !matches!(current, ProfileActivation::All)
            && matches!(requested, ProfileActivation::All);
        enables_known_profile || enables_future_profiles
    }

    pub(super) fn set_open_activation(
        &mut self,
        activation: ProfileActivation,
    ) -> Update<Message, Event> {
        if !self.open_activation_storage_available() {
            return Update::none();
        }
        if let Some(reason) = self.activation_enable_block_reason()
            && self.open_activation().is_some_and(|current| {
                self.activation_change_enables_more_profiles(&current, &activation)
            })
        {
            match &mut self.pane {
                Pane::Module(state) => state.error = Some(reason),
                Pane::InstalledPackage | Pane::OwnedPackage => self.manage_feedback = Some(reason),
                _ => {}
            }
            return Update::none();
        }
        if matches!(&self.pane, Pane::Folder(_)) {
            let (previous_activation, path) = match &mut self.pane {
                Pane::Folder(state) => {
                    let previous_activation = state.activation.clone();
                    state.activation = activation.clone();
                    (previous_activation, state.original_path.clone())
                }
                _ => unreachable!("folder pane was checked above"),
            };
            let Some(path) = path else {
                self.dirty = true;
                return Update::none();
            };
            let previous_packages = self.packages.clone();
            if !packages::set_folder_activation(&mut self.packages, &path, activation.clone()) {
                // Older script files can refer to a folder before it has an explicit
                // `packages.json` row. Materialize that row on the first activation edit instead
                // of pretending a mutation of the missing node succeeded.
                packages::insert_folder(&mut self.packages, &path);
                if !packages::set_folder_activation(&mut self.packages, &path, activation) {
                    self.packages = previous_packages;
                    if let Pane::Folder(state) = &mut self.pane {
                        state.activation = previous_activation;
                        state.error = Some(crate::i18n::t!("automation-folder-case-ambiguous"));
                    }
                    return Update::none();
                }
            }
            match self.serialize_scripts() {
                Ok(AutomationSaveStatus::Saved) => {}
                Ok(AutomationSaveStatus::Conflict) => {
                    self.packages = previous_packages;
                    if let Pane::Folder(state) = &mut self.pane {
                        state.activation = previous_activation;
                    }
                    return self.automation_save_conflict();
                }
                Err(error) => {
                    self.packages = previous_packages;
                    if let Pane::Folder(state) = &mut self.pane {
                        state.activation = previous_activation;
                    }
                    return warn_none(crate::i18n::t!(
                        "editor-failed-save-folders",
                        "error" => error.to_string()
                    ));
                }
            }
            return Update::with_event(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            });
        }

        match &mut self.pane {
            Pane::Module(state) => {
                let previous_activation = state.activation.clone();
                state.activation = activation.clone();
                if state.mode == ModuleMode::Create {
                    state.activation_touched = true;
                    self.dirty = true;
                    return Update::none();
                }
                if let Err(error) = smudgy_core::models::modules::set_activation(
                    &self.server_name,
                    &state.subpath,
                    activation,
                ) {
                    state.activation = previous_activation;
                    state.error = Some(error.to_string());
                    return Update::none();
                }
                state.error = None;
                match smudgy_core::models::modules::load_settings(&self.server_name) {
                    Ok(settings) => self.module_settings = settings,
                    Err(error) => {
                        self.module_state_error = Some(error.to_string());
                        state.error = Some(error.to_string());
                    }
                }
                Update::with_event(Event::ScriptsChanged {
                    server_name: self.server_name.clone(),
                })
            }
            Pane::InstalledPackage | Pane::OwnedPackage => {
                self.set_open_package_activation(activation)
            }
            _ => Update::none(),
        }
    }

    pub(super) fn save_open(&mut self) -> Update<Message, Event> {
        let Pane::Editor(state) = &mut self.pane else {
            return Update::none();
        };
        state.error = None;
        let name = state.name.trim().to_string();
        if name.is_empty() {
            state.error = Some(crate::i18n::t!("editor-name-empty"));
            return Update::none();
        }
        if let Err(message) = naming::validate_name(&name) {
            state.error = Some(message);
            return Update::none();
        }

        let mode = state.mode;
        let original_name = state.original_name.clone();
        // Conflict check.
        let conflicts = match mode {
            EditorMode::Create => self.script_exists(&name),
            EditorMode::Edit => {
                // A pure case change (e.g. `combat` → `Combat`) is the same file
                // on a case-insensitive filesystem, so it is not a conflict.
                let renamed = original_name
                    .as_deref()
                    .is_none_or(|original| !naming::names_conflict(original, &name));
                renamed && self.script_exists(&name)
            }
        };
        if conflicts {
            if let Pane::Editor(state) = &mut self.pane {
                state.error = Some(crate::i18n::t!("editor-name-in-use"));
            }
            return Update::none();
        }

        // The alias matcher persists from the draft: the compiled pattern plus
        // the authoring sidecar (absent for the Regex kind). A compile error
        // blocks the save with its message.
        let alias_matcher = if matches!(
            &self.pane,
            Pane::Editor(EditorState {
                node: EditNode::Alias(_),
                ..
            })
        ) {
            match self.alias_draft.to_pattern(&name) {
                Ok(pattern) => Some((pattern, self.alias_draft.to_matcher())),
                Err(message) => {
                    if let Pane::Editor(state) = &mut self.pane {
                        state.error = Some(message);
                    }
                    return Update::none();
                }
            }
        } else {
            None
        };

        // The body comes from whichever action tab is active: the send-text
        // draft for a Plaintext action, the script draft otherwise. Hotkeys
        // use their dedicated plaintext buffer when applicable. Every JS/TS
        // body comes from the upstream code editor's authoritative buffer.
        let body = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Alias(a),
                ..
            }) if a.language == ScriptLang::Plaintext => self.send_text_content.text(),
            Pane::Editor(EditorState {
                node:
                    EditNode::Trigger {
                        language: ScriptLang::Plaintext,
                        ..
                    },
                ..
            }) => self.send_text_content.text(),
            Pane::Editor(EditorState {
                node: EditNode::Hotkey(h),
                ..
            }) if h.language == ScriptLang::Plaintext => self.hotkey_text_content.text(),
            _ => self.code_editor_text(),
        };
        let saved_code = matches!(
            &self.pane,
            Pane::Editor(EditorState {
                node: EditNode::Alias(aliases::AliasDefinition {
                    language: ScriptLang::JS | ScriptLang::TS,
                    ..
                }),
                ..
            }) | Pane::Editor(EditorState {
                node: EditNode::Trigger {
                    language: ScriptLang::JS | ScriptLang::TS,
                    ..
                },
                ..
            }) | Pane::Editor(EditorState {
                node: EditNode::Hotkey(hotkeys::HotkeyDefinition {
                    language: ScriptLang::JS | ScriptLang::TS,
                    ..
                }),
                ..
            })
        );
        let saved_dual_action = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Alias(alias),
                ..
            }) => Some((alias.language, code_editor::CodeDocument::Alias)),
            Pane::Editor(EditorState {
                node: EditNode::Trigger { language, .. },
                ..
            }) => Some((*language, code_editor::CodeDocument::Trigger)),
            _ => None,
        };
        let body = if saved_code {
            body
        } else {
            body.trim_end_matches('\n').to_string()
        };
        let persisted_script = persisted_script(body);
        let final_script = match &self.pane {
            Pane::Editor(EditorState { node, .. }) => match node {
                EditNode::Alias(a) => {
                    let (pattern, matcher) =
                        alias_matcher.expect("computed above for the alias arm");
                    Script::Alias(aliases::AliasDefinition {
                        script: persisted_script,
                        pattern,
                        matcher,
                        ..a.clone()
                    })
                }
                EditNode::Hotkey(h) => {
                    let mut h = h.clone();
                    if !self.hotkey_state.is_empty() {
                        hotkey_helpers::set_key_and_modifiers_from_maybe_physical(
                            &mut h,
                            self.hotkey_state.clone(),
                        );
                    }
                    Script::Hotkey(hotkeys::HotkeyDefinition {
                        script: persisted_script,
                        ..h
                    })
                }
                EditNode::Trigger {
                    enabled,
                    language,
                    prompt,
                    priority,
                    fallthrough,
                    package,
                    rows,
                } => {
                    let mut t = triggers::TriggerDefinition {
                        patterns: None,
                        raw_patterns: None,
                        anti_patterns: None,
                        script: persisted_script,
                        package: package.clone(),
                        language: *language,
                        enabled: *enabled,
                        prompt: *prompt,
                        priority: *priority,
                        fallthrough: *fallthrough,
                        matchers: None,
                    };
                    if let Err((i, message)) = rows_into_trigger(rows, &mut t) {
                        let message = crate::i18n::t!(
                            "editor-row-error", "row" => (i + 1).to_string(), "error" => message
                        );
                        if let Pane::Editor(state) = &mut self.pane {
                            state.error = Some(message);
                        }
                        return Update::none();
                    }
                    Script::Trigger(t)
                }
            },
            _ => return Update::none(),
        };

        // Drop the old entry first so the re-insert below re-homes the script.
        // This covers a rename (name changed) *and* a move (only the `package`
        // folder changed): in both cases the script lives under the old key/
        // folder in `self.scripts` and must be removed, or it would end up
        // duplicated under both the old and new folder. `remove_script_by_name`
        // finds it by name anywhere in the tree, so an unchanged save is a
        // harmless remove-then-reinsert in place.
        let previous_scripts = self.scripts.clone();
        if mode == EditorMode::Edit
            && let Some(orig) = &original_name
        {
            self.remove_script_by_name(orig);
        }
        match upsert_script_folder(&mut self.scripts, final_script.folder_name()) {
            Ok(folder) => {
                folder.insert(name.clone(), final_script);
            }
            Err(e) => {
                self.scripts = previous_scripts;
                if let Pane::Editor(state) = &mut self.pane {
                    state.error = Some(e);
                }
                return Update::none();
            }
        }
        match self.serialize_scripts() {
            Ok(AutomationSaveStatus::Saved) => {}
            Ok(AutomationSaveStatus::Conflict) => {
                self.scripts = previous_scripts;
                return self.automation_save_conflict();
            }
            Err(e) => {
                self.scripts = previous_scripts;
                if let Pane::Editor(state) = &mut self.pane {
                    state.error =
                        Some(crate::i18n::t!("editor-failed-save", "error" => e.to_string()));
                }
                return Update::none();
            }
        }
        // Reflect the saved state in the pane.
        if let Pane::Editor(state) = &mut self.pane {
            state.mode = EditorMode::Edit;
            state.original_name = Some(name.clone());
        }
        self.selection = Selection::Script(ScriptKey {
            folder_name: self.find_script_folder(&name),
            script_name: name.clone(),
        });
        self.dirty = false;
        let released = self.release_pending_navigation();
        if saved_code {
            self.mark_code_editor_saved();
        }
        if self.language_project_context_matches(&code_editor::LanguageProjectContext::Inline) {
            self.language_project_target_context =
                Some(code_editor::LanguageProjectContext::Inline);
            self.refresh_language_project();
        }
        // Alias and trigger action tabs are alternative persisted bodies. Once
        // one is saved, discard the opposite draft so it cannot reappear as
        // stale user-authored content after another tab switch.
        let discard_task = match saved_dual_action {
            Some((ScriptLang::Plaintext, kind)) => {
                self.action_text_pinned = true;
                self.action_script_pinned = false;
                self.bind_code_editor(
                    "",
                    code_editor::script_language(self.action_script_lang),
                    kind,
                )
            }
            Some((ScriptLang::JS | ScriptLang::TS, _)) => {
                self.send_text_content = text_editor::Content::new();
                self.action_text_pinned = false;
                self.action_script_pinned = true;
                Task::none()
            }
            None => Task::none(),
        };
        let toast = self.show_toast(crate::i18n::t!("editor-saved", "name" => name));
        Update::new(
            Task::batch([discard_task, toast, released]),
            Some(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// The folder path a saved script ended up in (for re-selection).
    fn find_script_folder(&self, name: &str) -> Option<String> {
        fn rec(
            scripts: &BTreeMap<String, Script>,
            name: &str,
            prefix: Option<&str>,
        ) -> Option<String> {
            for (n, script) in scripts {
                if n == name && !matches!(script, Script::Folder(_, _)) {
                    return Some(prefix.map(str::to_string).unwrap_or_default());
                }
                if let Script::Folder(_, children) = script {
                    let child_prefix = match prefix {
                        Some(p) => format!("{p}/{n}"),
                        None => n.clone(),
                    };
                    if let Some(found) = rec(children, name, Some(&child_prefix)) {
                        return Some(found);
                    }
                }
            }
            None
        }
        rec(&self.scripts, name, None).filter(|p| !p.is_empty())
    }

    pub(super) fn delete_open(&mut self) -> Update<Message, Event> {
        let original = match &self.pane {
            Pane::Editor(EditorState {
                mode: EditorMode::Edit,
                original_name: Some(name),
                ..
            }) => name.clone(),
            _ => return Update::none(),
        };
        let previous_scripts = self.scripts.clone();
        self.remove_script_by_name(&original);
        match self.serialize_scripts() {
            Ok(AutomationSaveStatus::Saved) => {}
            Ok(AutomationSaveStatus::Conflict) => {
                self.scripts = previous_scripts;
                return self.automation_save_conflict();
            }
            Err(e) => {
                self.scripts = previous_scripts;
                self.pane = Pane::Error(Arc::new(vec![crate::i18n::t!(
                    "editor-failed-save-delete",
                    "error" => e.to_string()
                )]));
                return Update::none();
            }
        }
        self.dirty = false;
        self.clear_code_editor();
        if self.language_project_context_matches(&code_editor::LanguageProjectContext::Inline) {
            self.language_project_target_context =
                Some(code_editor::LanguageProjectContext::Inline);
            self.refresh_language_project();
        }
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let toast = self.show_toast(crate::i18n::t!("editor-deleted", "name" => original));
        Update::new(
            toast,
            Some(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- folder save / delete ---------------------------------------------

    pub(super) fn save_folder(&mut self) -> Update<Message, Event> {
        let (mode, original_path, path, activation) = match &self.pane {
            Pane::Folder(state) => (
                state.mode,
                state.original_path.clone(),
                state.path.trim_matches('/').to_string(),
                state.activation.clone(),
            ),
            _ => return Update::none(),
        };
        if let Pane::Folder(state) = &mut self.pane {
            state.error = None;
        }
        if let Err(message) = naming::validate_folder_path(&path) {
            if let Pane::Folder(state) = &mut self.pane {
                state.error = Some(message);
            }
            return Update::none();
        }
        match mode {
            EditorMode::Create => {
                if packages::folder_destination_conflicts(&self.packages, &path, None) {
                    if let Pane::Folder(state) = &mut self.pane {
                        state.error = Some(crate::i18n::t!(
                            "editor-folder-path-conflict",
                            "path" => &path
                        ));
                    }
                    return Update::none();
                }
                let previous_packages = self.packages.clone();
                packages::insert_folder(&mut self.packages, &path);
                packages::set_folder_activation(&mut self.packages, &path, activation.clone());
                match self.serialize_scripts() {
                    Ok(AutomationSaveStatus::Saved) => {}
                    Ok(AutomationSaveStatus::Conflict) => {
                        self.packages = previous_packages;
                        return self.automation_save_conflict();
                    }
                    Err(e) => {
                        self.packages = previous_packages;
                        if let Pane::Folder(state) = &mut self.pane {
                            state.error = Some(crate::i18n::t!(
                                "editor-failed-save-folders",
                                "error" => e.to_string()
                            ));
                        }
                        return Update::none();
                    }
                }
                self.merge_folders();
                self.selection = Selection::Folder(path.clone());
                self.pane = Pane::Folder(FolderState {
                    mode: EditorMode::Edit,
                    original_path: Some(path.clone()),
                    path,
                    activation,
                    error: None,
                });
                // The path and activation drafts are now persisted; the editor is clean.
                self.dirty = false;
                let released = self.release_pending_navigation();
                let toast = self.show_toast(crate::i18n::t!("editor-folder-created"));
                Update::with_task(Task::batch([toast, released]))
            }
            EditorMode::Edit => {
                let Some(old_path) = original_path else {
                    return Update::none();
                };
                if old_path == path {
                    // Retyping the same path changed nothing on disk, so there is no draft left.
                    self.dirty = false;
                    return Update::with_task(self.release_pending_navigation());
                }
                if packages::folder_destination_conflicts(&self.packages, &path, Some(&old_path)) {
                    if let Pane::Folder(state) = &mut self.pane {
                        state.error = Some(crate::i18n::t!(
                            "editor-folder-path-conflict",
                            "path" => &path
                        ));
                    }
                    return Update::none();
                }
                let previous_packages = self.packages.clone();
                let previous_scripts = self.scripts.clone();
                if !packages::rename_folder(&mut self.packages, &old_path, &path) {
                    if let Pane::Folder(state) = &mut self.pane {
                        state.error = Some(crate::i18n::t!(
                            "editor-folder-missing",
                            "path" => &old_path
                        ));
                    }
                    return Update::none();
                }
                self.rename_script_packages(&old_path, &path);
                match self.serialize_scripts() {
                    Ok(AutomationSaveStatus::Saved) => {}
                    Ok(AutomationSaveStatus::Conflict) => {
                        self.packages = previous_packages;
                        self.scripts = previous_scripts;
                        return self.automation_save_conflict();
                    }
                    Err(e) => {
                        self.packages = previous_packages;
                        self.scripts = previous_scripts;
                        return warn_none(crate::i18n::t!(
                            "editor-failed-save-folders",
                            "error" => e.to_string()
                        ));
                    }
                }
                self.selection = Selection::Folder(path.clone());
                self.pane = Pane::Folder(FolderState {
                    mode: EditorMode::Edit,
                    original_path: Some(path.clone()),
                    path,
                    activation,
                    error: None,
                });
                self.dirty = false;
                let released = self.release_pending_navigation();
                Update::new(
                    Task::batch([Task_batch_reload(self), released]),
                    Some(Event::UserAutomationsChanged {
                        server_name: self.server_name.clone(),
                    }),
                )
            }
        }
    }

    pub(super) fn delete_folder(&mut self, delete_scripts: bool) -> Update<Message, Event> {
        let path = match &self.pane {
            Pane::Folder(FolderState {
                mode: EditorMode::Edit,
                original_path: Some(path),
                ..
            }) => path.clone(),
            _ => return Update::none(),
        };
        let previous_packages = self.packages.clone();
        let previous_scripts = self.scripts.clone();
        if !packages::remove_folder(&mut self.packages, &path) {
            return warn_none(crate::i18n::t!("editor-folder-missing", "path" => &path));
        }
        if delete_scripts {
            for name in self.scripts_under(&path) {
                self.remove_script_by_name(&name);
            }
        } else {
            let parent = packages::parent_path(&path);
            self.reparent_scripts(&path, parent);
        }
        self.confirm_folder_delete = false;
        match self.serialize_scripts() {
            Ok(AutomationSaveStatus::Saved) => {}
            Ok(AutomationSaveStatus::Conflict) => {
                self.packages = previous_packages;
                self.scripts = previous_scripts;
                return self.automation_save_conflict();
            }
            Err(e) => {
                self.packages = previous_packages;
                self.scripts = previous_scripts;
                return warn_none(
                    crate::i18n::t!("editor-failed-save-folders", "error" => e.to_string()),
                );
            }
        }
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        Update::new(
            Task_batch_reload(self),
            Some(Event::UserAutomationsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- module save / create ---------------------------------------------

    pub(super) fn save_module(&mut self) -> Update<Message, Event> {
        let subpath = match &self.pane {
            Pane::Module(ModuleState {
                subpath,
                path: Some(_),
                ..
            }) => subpath.clone(),
            _ => return Update::none(),
        };
        if !self.code_editor_is_modified() {
            self.dirty = false;
            return Update::with_task(self.release_pending_navigation());
        }
        let Some(expected) = self.module_source_baseline.as_deref() else {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(crate::i18n::t!("editor-file-changed-outside"));
            }
            return Update::none();
        };
        let content = self.code_editor_text();
        match smudgy_core::models::modules::save_module_if_unchanged(
            &self.server_name,
            &subpath,
            expected,
            &content,
        ) {
            Ok(ModuleFileWriteOutcome::Saved) => {}
            Ok(ModuleFileWriteOutcome::Conflict) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.error = Some(crate::i18n::t!("editor-file-changed-outside"));
                }
                return Update::none();
            }
            Err(e) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.error = Some(crate::i18n::t!(
                        "editor-failed-save-module",
                        "error" => e.to_string()
                    ));
                }
                return Update::none();
            }
        }
        if let Pane::Module(state) = &mut self.pane {
            state.error = None;
        }
        self.module_source_baseline = Some(content);
        self.dirty = false;
        let released = self.release_pending_navigation();
        self.mark_code_editor_saved();
        self.refresh_language_project();
        let toast = self.show_toast(crate::i18n::t!("editor-module-saved"));
        Update::new(
            Task::batch([toast, released]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn create_module(&mut self) -> Update<Message, Event> {
        let (name, activation) = match &self.pane {
            Pane::Module(state) => (state.name.trim().to_string(), state.activation.clone()),
            _ => return Update::none(),
        };
        if let Err(message) = naming::validate_module_subpath(&name) {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(message);
            }
            return Update::none();
        }
        if !smudgy_core::models::modules::is_script_module(&name) {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(crate::i18n::t!("module-script-extension-required"));
            }
            return Update::none();
        }
        let dir = match server::load_server(&self.server_name) {
            Ok(server) => server.path.join("modules"),
            Err(e) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.error = Some(crate::i18n::t!(
                        "editor-failed-modules-dir",
                        "error" => e.to_string()
                    ));
                }
                return Update::none();
            }
        };
        let target = dir.join(&name);
        let content = self.code_editor_text();
        if let Err(error) = smudgy_core::models::modules::create_module(
            &self.server_name,
            &name.replace('\\', "/"),
            &content,
            activation.clone(),
        ) {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(crate::i18n::t!(
                    "editor-failed-create-module",
                    "error" => error.to_string()
                ));
            }
            return Update::none();
        }
        self.dirty = false;
        self.module_source_baseline = Some(content);
        let released = self.release_pending_navigation();
        self.mark_code_editor_saved();
        self.selection = Selection::Module(name.clone());
        self.pane = Pane::Module(ModuleState {
            mode: ModuleMode::View,
            subpath: name.clone(),
            path: Some(target),
            name: String::new(),
            tab: ModuleTab::Settings,
            activation,
            activation_touched: false,
            error: None,
        });
        self.refresh_language_project();
        let toast = self.show_toast(crate::i18n::t!("editor-module-created", "name" => &name));
        Update::new(
            Task_batch_module_reload(Task::batch([toast, released])),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }
    // ---- tree mutation helpers (folder rename/delete) ---------------------

    fn scripts_under(&self, folder: &str) -> Vec<String> {
        let mut names = Vec::new();
        collect_scripts_under(&self.scripts, folder, &mut names);
        names
    }

    fn rename_script_packages(&mut self, old: &str, new: &str) {
        for_each_script_mut(&mut self.scripts, &mut |script| {
            if let Some(pkg) = script_package_field(script) {
                let updated = pkg.as_deref().and_then(|path| {
                    folder_relative_suffix(path, old).map(|suffix| {
                        if suffix.is_empty() {
                            new.to_owned()
                        } else {
                            format!("{new}/{suffix}")
                        }
                    })
                });
                if let Some(updated) = updated {
                    *pkg = Some(updated);
                }
            }
        });
    }

    fn reparent_scripts(&mut self, folder: &str, target: Option<String>) {
        for_each_script_mut(&mut self.scripts, &mut |script| {
            if let Some(pkg) = script_package_field(script) {
                let under = pkg
                    .as_deref()
                    .is_some_and(|path| folder_relative_suffix(path, folder).is_some());
                if under {
                    *pkg = target.clone();
                }
            }
        });
    }
}

// ---- free helpers ----------------------------------------------------------

fn hotkey_definition_to_keys(hotkey: &hotkeys::HotkeyDefinition) -> Vec<MaybePhysicalKey> {
    use iced::keyboard::{Key, key::Named};
    let mut keys = Vec::new();
    for modifier in &hotkey.modifiers {
        let modifier_key = match modifier.as_str() {
            "CTRL" => MaybePhysicalKey::Key(Key::Named(Named::Control)),
            "ALT" => MaybePhysicalKey::Key(Key::Named(Named::Alt)),
            "SHIFT" => MaybePhysicalKey::Key(Key::Named(Named::Shift)),
            "SUPER" => MaybePhysicalKey::Key(Key::Named(Named::Super)),
            _ => continue,
        };
        keys.push(modifier_key);
    }
    keys.push(hotkey_helpers::hotkey_to_maybe_physical_key(hotkey));
    keys
}

fn script_package_field(script: &mut Script) -> Option<&mut Option<String>> {
    match script {
        Script::Alias(a) => Some(&mut a.package),
        Script::Hotkey(h) => Some(&mut h.package),
        Script::Trigger(t) => Some(&mut t.package),
        Script::Folder(_, _) => None,
    }
}

fn for_each_script_mut(scripts: &mut BTreeMap<String, Script>, f: &mut impl FnMut(&mut Script)) {
    for script in scripts.values_mut() {
        if let Script::Folder(_, children) = script {
            for_each_script_mut(children, f);
        } else {
            f(script);
        }
    }
}

fn collect_scripts_under(scripts: &BTreeMap<String, Script>, folder: &str, out: &mut Vec<String>) {
    for (name, script) in scripts {
        if let Script::Folder(_, children) = script {
            collect_scripts_under(children, folder, out);
        } else {
            let pkg = script.folder_name();
            if pkg.is_some_and(|path| folder_relative_suffix(path, folder).is_some()) {
                out.push(name.clone());
            }
        }
    }
}

/// Returns the segment-wise suffix of `path` below the exact stored `folder`. Unambiguous legacy
/// case variants are canonicalized when the script tree merges with packages.json; exact matching
/// here preserves still-distinct legacy case-only sibling folders during rename and delete.
fn folder_relative_suffix(path: &str, folder: &str) -> Option<String> {
    let path_components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let folder_components = folder
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if path_components.len() < folder_components.len()
        || !path_components
            .iter()
            .zip(&folder_components)
            .all(|(path, folder)| path == folder)
    {
        return None;
    }
    Some(path_components[folder_components.len()..].join("/"))
}

#[allow(non_snake_case)]
fn Task_batch_reload(window: &AutomationsWindow) -> iced::Task<Message> {
    iced::Task::batch([
        iced::Task::done(window.load_scripts_message()),
        iced::Task::done(Message::LoadFolders),
    ])
}

#[allow(non_snake_case)]
fn Task_batch_module_reload(toast: iced::Task<Message>) -> iced::Task<Message> {
    iced::Task::batch([iced::Task::done(Message::LoadModules), toast])
}

// ============================================================================
// View-side
// ============================================================================

impl AutomationsWindow {
    /// A scene header: leading dot · large title · subtitle · right-aligned actions.
    pub(super) fn scene_header<'a>(
        &self,
        status: Option<NodeStatus>,
        title: &str,
        subtitle: Option<String>,
        actions: Option<Elem<'a>>,
    ) -> Elem<'a> {
        self.scene_header_impl(status, title, subtitle, actions, None)
    }

    /// Like [`scene_header`], but with a right-aligned control on the subtitle
    /// line (the folder picker). Placing it there keeps it directly beneath the
    /// header actions without deepening the header — the subtitle row already
    /// exists, so panes with and without the aside stay the same height.
    pub(super) fn scene_header_with_aside<'a>(
        &self,
        status: Option<NodeStatus>,
        title: &str,
        subtitle: Option<String>,
        actions: Option<Elem<'a>>,
        subtitle_aside: Elem<'a>,
    ) -> Elem<'a> {
        self.scene_header_impl(status, title, subtitle, actions, Some(subtitle_aside))
    }

    fn scene_header_impl<'a>(
        &self,
        status: Option<NodeStatus>,
        title: &str,
        subtitle: Option<String>,
        actions: Option<Elem<'a>>,
        subtitle_aside: Option<Elem<'a>>,
    ) -> Elem<'a> {
        let mut title_row = row![].spacing(10.0).align_y(Vertical::Center);
        if let Some(status) = status {
            title_row = title_row.push(common::status_dot(status));
        }
        title_row = title_row.push(text(title.to_string()).size(30.0).font(Font {
            weight: iced::font::Weight::Light,
            ..fonts::GEIST_VF
        }));
        title_row = title_row.push(iced::widget::space::horizontal());
        if let Some(actions) = actions {
            title_row = title_row.push(actions);
        }
        let mut header = column![title_row].spacing(4.0);
        if let Some(aside) = subtitle_aside {
            // Subtitle text on the left, the aside control right-aligned so it
            // sits beneath the header actions.
            let mut sub_row = row![].spacing(10.0).align_y(Vertical::Center);
            if let Some(subtitle) = subtitle {
                sub_row = sub_row.push(text(subtitle).size(13.0).style(common::muted));
            }
            sub_row = sub_row.push(iced::widget::space::horizontal());
            sub_row = sub_row.push(aside);
            header = header.push(sub_row);
        } else if let Some(subtitle) = subtitle {
            header = header.push(text(subtitle).size(13.0).style(common::muted));
        }
        column![header, iced::widget::rule::horizontal(1.0),]
            .spacing(12.0)
            .into()
    }

    /// The sticky save bar shown for dirty editors / create panes. A
    /// `delete_link` label renders the destructive affordance as the deck's
    /// red underlined text link (with `Cancel` beside `Save`); `None` keeps
    /// the plain `Delete` button the other panes use.
    pub(super) fn save_bar<'a>(
        &self,
        create: bool,
        can_delete: bool,
        save_label: &str,
        delete_link: Option<&str>,
    ) -> Option<Elem<'a>> {
        if !create && !self.dirty && !can_delete {
            return None;
        }
        let mut bar = row![]
            .spacing(12.0)
            .align_y(Vertical::Center)
            .padding(Padding {
                top: 12.0,
                bottom: 4.0,
                left: 0.0,
                right: 0.0,
            });
        if can_delete {
            bar = bar.push(match delete_link {
                Some(label) => danger_link(label.to_string(), Message::Delete),
                None => button(text(crate::i18n::t!("editor-delete")).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Delete)
                    .into(),
            });
        }
        if self.dirty {
            bar = bar.push(text("\u{25CF}").size(9.0).style(common::accent));
            bar = bar.push(
                text(crate::i18n::t!("editor-unsaved"))
                    .size(13.0)
                    .style(common::muted),
            );
            bar = bar.push(iced::widget::space::horizontal());
            let cancel = if delete_link.is_some() {
                crate::i18n::t!("action-cancel")
            } else {
                crate::i18n::t!("editor-discard")
            };
            bar = bar.push(
                button(text(cancel).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
            );
            bar = bar.push(
                button(text(save_label.to_string()).size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::Save),
            );
        }
        Some(container(bar).width(Length::Fill).into())
    }

    /// The "When it runs" module behind its disclosure: hidden as a text link
    /// (its grid label rendered empty) until clicked, forced open — and not
    /// re-hideable — while any value is non-default (`prompt` included), with
    /// a hide link when open on pure defaults.
    fn order_module<'a>(
        &self,
        priority: i32,
        fallthrough: bool,
        prompt: Option<bool>,
        allow_self_match: Option<bool>,
        trigger: bool,
    ) -> Elem<'a> {
        let non_default =
            priority != 0 || !fallthrough || prompt == Some(true) || allow_self_match == Some(true);
        if !non_default && !self.order_revealed {
            let label = if trigger {
                crate::i18n::t!("editor-reveal-order-triggers")
            } else {
                crate::i18n::t!("editor-reveal-order-aliases")
            };
            return field_row("", text_link(label, Message::RevealOrder));
        }

        // The priority stepper: a collapsed-border [-|value|+] segment.
        let stepper = container(
            row![
                button(text("-").size(14.0))
                    .style(button_style::toolbar)
                    .on_press(Message::AdjustPriority(-1))
                    .padding(Padding {
                        top: 2.0,
                        bottom: 2.0,
                        left: 10.0,
                        right: 10.0,
                    }),
                container(text(priority.to_string()).size(13.0))
                    .width(Length::Fixed(40.0))
                    .align_x(iced::alignment::Horizontal::Center),
                button(text("+").size(14.0))
                    .style(button_style::toolbar)
                    .on_press(Message::AdjustPriority(1))
                    .padding(Padding {
                        top: 2.0,
                        bottom: 2.0,
                        left: 10.0,
                        right: 10.0,
                    }),
            ]
            .align_y(Vertical::Center),
        )
        .style(common::outline_box_style);

        let priority_row = row![
            text(crate::i18n::ts!("editor-priority"))
                .size(13.0)
                .style(common::muted),
            stepper,
            text(if trigger {
                crate::i18n::ts!("editor-priority-note-triggers")
            } else {
                crate::i18n::ts!("editor-priority-note-aliases")
            })
            .size(12.0)
            .style(common::muted),
        ]
        .spacing(10.0)
        .align_y(Vertical::Center);

        let continue_row = checkbox(fallthrough)
            .label(if trigger {
                crate::i18n::ts!("editor-continue-triggers")
            } else {
                crate::i18n::ts!("editor-continue-aliases")
            })
            .on_toggle(|_| Message::ToggleFallthrough)
            .size(14.0)
            .text_size(13.0);

        let mut inner = column![priority_row, continue_row].spacing(10.0);
        if let Some(allow_self_match) = allow_self_match {
            inner = inner.push(
                checkbox(allow_self_match)
                    .label(crate::i18n::ts!("editor-allow-self-match"))
                    .on_toggle(|_| Message::ToggleAllowSelfMatch)
                    .size(14.0)
                    .text_size(13.0),
            );
        }
        if let Some(prompt) = prompt {
            inner = inner.push(
                column![
                    checkbox(prompt)
                        .label(crate::i18n::ts!("editor-prompt"))
                        .on_toggle(|_| Message::TogglePrompt)
                        .size(14.0)
                        .text_size(13.0),
                    container(
                        text(crate::i18n::ts!("editor-prompt-note"))
                            .size(12.0)
                            .style(common::muted),
                    )
                    .padding(Padding {
                        top: 0.0,
                        bottom: 0.0,
                        left: 22.0,
                        right: 0.0,
                    }),
                ]
                .spacing(2.0),
            );
        }
        if !non_default {
            inner = inner.push(text_link(
                crate::i18n::t!("editor-hide-order"),
                Message::HideOrder,
            ));
        }
        field_row(crate::i18n::ts!("editor-when-it-runs"), inner.into())
    }

    /// The Matched-values rail: one clickable badge per capture the current
    /// matcher provides, inserting its reference at the caret in the action
    /// body. Absent entirely when nothing is captured.
    fn matched_values_rail<'a>(&self, references: Vec<String>) -> Option<Elem<'a>> {
        if references.is_empty() {
            return None;
        }
        let mut rail = row![].spacing(6.0).align_y(Vertical::Center);
        for reference in references {
            rail = rail.push(
                button(
                    text(reference.clone())
                        .size(12.0)
                        .font(fonts::GEIST_MONO_VF),
                )
                .style(capture_badge_style)
                .on_press(Message::InsertReference(reference))
                .padding([3, 8]),
            );
        }
        Some(
            column![
                common::section_label(crate::i18n::ts!("editor-matched-values")),
                rail,
            ]
            .spacing(4.0)
            .into(),
        )
    }

    /// The capture references the open alias's draft provides, rendered in the
    /// action language's vocabulary (`$name` for text, `matches.name` for JS).
    fn alias_capture_references(&self, language: ScriptLang) -> Vec<String> {
        let draft = &self.alias_draft;
        let captures: Vec<Option<String>> = match draft.kind {
            AliasKind::Command => draft
                .args
                .iter()
                .map(|arg| Some(arg.name.clone()))
                .collect(),
            AliasKind::Pattern => {
                let compiled = matchers::compile_pattern(
                    &draft.pattern_source,
                    draft.anchor_start,
                    draft.anchor_end,
                );
                if compiled.errors.is_empty() {
                    compiled.captures
                } else {
                    Vec::new()
                }
            }
            AliasKind::Regex => regex::Regex::new(&draft.regex_source)
                .map(|re| {
                    re.capture_names()
                        .skip(1)
                        .map(|n| n.map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        };
        render_references(&captures, language)
    }

    /// The capture references a trigger's Match/Raw rows provide (the union,
    /// in row order).
    fn trigger_capture_references(rows: &[TriggerRow], language: ScriptLang) -> Vec<String> {
        let mut captures: Vec<Option<String>> = Vec::new();
        for row in rows {
            if row.role == PatternKind::Anti || row.source.trim().is_empty() {
                continue;
            }
            let Ok(source) = row.compiled() else { continue };
            let Ok(re) = regex::Regex::new(&source) else {
                continue;
            };
            for name in re.capture_names().skip(1) {
                let name = name.map(str::to_string);
                if name.is_some() && captures.contains(&name) {
                    continue;
                }
                captures.push(name);
            }
        }
        render_references(&captures, language)
    }

    /// The "Folder" control in a script editor: a `pick_list` of every folder
    /// (plus "(top level)"). Picking a destination emits [`Message::SetScriptFolder`],
    /// which moves the script (immediately in edit mode, on Create otherwise).
    fn folder_picker<'a>(&self, current: Option<&str>) -> Elem<'a> {
        let selected = FolderChoice::from_package(current);
        let mut options = vec![FolderChoice::TopLevel];
        options.extend(
            self.all_folder_paths()
                .into_iter()
                .map(FolderChoice::Folder),
        );
        // The current folder is normally already a real tree folder, but guard so
        // the picker never shows a blank selection if it somehow isn't listed.
        if !options.contains(&selected) {
            options.push(selected.clone());
        }
        pick_list(options, Some(selected), |choice: FolderChoice| {
            Message::SetScriptFolder(choice.into_package())
        })
        .text_size(13.0)
        .padding(Padding {
            top: 3.0,
            bottom: 3.0,
            left: 8.0,
            right: 6.0,
        })
        .into()
    }

    /// The action module: the `Send text | Run JavaScript` tab strip fused to
    /// the body editor it labels — the tab IS the field's label. Each tab has
    /// its own draft, so switching never destroys work; a TS automation opens
    /// under (and saves from) the JavaScript tab as TS.
    fn action_tab_strip<'a>(
        &'a self,
        language: ScriptLang,
        control_name: &'static str,
    ) -> Elem<'a> {
        let text_active = language == ScriptLang::Plaintext;
        let script_language = self.action_script_lang;
        let strip = row![
            common::tab(
                crate::i18n::ts!("editor-tab-send-text"),
                text_active,
                Message::SetBehavior(ScriptLang::Plaintext),
            ),
            common::tab(
                crate::i18n::ts!("editor-tab-run-js"),
                !text_active,
                Message::SetBehavior(self.action_script_lang),
            ),
        ]
        .spacing(16.0);

        let id = Id::from(format!("automation-action-tabs:{control_name}"));
        let focus_id = id.clone();
        KeyboardControl::new(
            strip,
            id,
            move || Message::FocusColorControl(focus_id.clone()),
            move |key, _repeat| action_tab_key_action(key, language, script_language),
        )
        .focus_color(iced::Color::TRANSPARENT)
        .into()
    }

    fn action_module<'a>(
        &'a self,
        language: ScriptLang,
        references: Vec<String>,
        control_name: &'static str,
    ) -> Elem<'a> {
        let text_active = language == ScriptLang::Plaintext;
        let strip = self.action_tab_strip(language, control_name);

        let editor: Elem<'a> = if text_active {
            let mut known = references;
            known.push("$0".to_string());
            text_editor(&self.send_text_content)
                .highlight_with::<highlight::PatternHighlighter>(
                    highlight::FieldSyntax::SendText { known },
                    token_format,
                )
                .font(fonts::GEIST_MONO_VF)
                .size(13.0)
                .padding(10.0)
                .on_action(Message::SendTextAction)
                .height(Length::Fill)
                .min_height(120.0)
                .into()
        } else {
            self.code_editor_view(Length::Fill)
        };
        column![
            strip,
            container(editor)
                .height(Length::Fill)
                .style(common::code_surface_style)
        ]
        .spacing(0.0)
        .into()
    }

    /// The hotkey action editor uses the same tab contract as aliases and triggers.
    fn hotkey_action_module<'a>(&'a self, language: ScriptLang) -> Elem<'a> {
        let strip = self.action_tab_strip(language, "hotkey");
        let editor: Elem<'a> = if language == ScriptLang::Plaintext {
            text_editor(&self.hotkey_text_content)
                .font(fonts::GEIST_MONO_VF)
                .size(13.0)
                .padding(10.0)
                .on_action(Message::HotkeyTextAction)
                .height(Length::Fill)
                .min_height(120.0)
                .into()
        } else {
            self.code_editor_view(Length::Fill)
        };
        column![
            strip,
            container(editor)
                .height(Length::Fill)
                .style(common::code_surface_style)
        ]
        .spacing(0.0)
        .into()
    }

    fn field_label<'a>(label: &str) -> Elem<'a> {
        container(text(label.to_string()).size(13.0).style(common::muted))
            .width(Length::Fixed(LABEL_WIDTH))
            .align_y(Vertical::Center)
            .height(Length::Fixed(34.0))
            .into()
    }

    /// `viewport_height` is the scroll viewport the pane renders into; the action editor grows
    /// into whatever of it the rest of the pane leaves.
    pub(super) fn view_editor<'a>(
        &'a self,
        state: &'a EditorState,
        viewport_height: f32,
    ) -> Elem<'a> {
        match &state.node {
            EditNode::Alias(alias) => self.view_alias_editor(state, alias, viewport_height),
            EditNode::Hotkey(hotkey) => self.view_hotkey_editor(state, hotkey, viewport_height),
            EditNode::Trigger {
                enabled,
                language,
                prompt,
                priority,
                fallthrough,
                rows,
                ..
            } => self.view_trigger_editor(
                state,
                *enabled,
                *language,
                *prompt,
                *priority,
                *fallthrough,
                rows,
                viewport_height,
            ),
        }
    }

    fn editor_status(create: bool, enabled: bool, has_error: bool) -> NodeStatus {
        if !enabled {
            NodeStatus::Disabled
        } else if has_error && !create {
            NodeStatus::Error
        } else {
            NodeStatus::Ok
        }
    }

    fn header_actions<'a>(&self, badge_label: &str, enabled: bool) -> Elem<'a> {
        row![
            common::badge(badge_label.to_string()),
            common::pill_switch(enabled, false, Some(Message::ToggleEnabled)),
        ]
        .spacing(14.0)
        .align_y(Vertical::Center)
        .into()
    }

    /// The right-aligned "Folder" placement picker shown on a script editor's
    /// subtitle line, directly beneath the header's enable switch. Living on the
    /// existing subtitle row keeps the header the same height as panes without a
    /// picker, with the dropdown sized to match the switch above it.
    fn folder_aside<'a>(&self, folder: Option<&str>) -> Elem<'a> {
        row![
            text(crate::i18n::t!("editor-folder"))
                .size(13.0)
                .style(common::muted),
            self.folder_picker(folder),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center)
        .into()
    }

    fn view_alias_editor<'a>(
        &'a self,
        state: &'a EditorState,
        alias: &'a aliases::AliasDefinition,
        viewport_height: f32,
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let badge_label = if alias.language == ScriptLang::Plaintext {
            crate::i18n::ts!("editor-text")
        } else {
            "JavaScript"
        };
        let title = if create {
            crate::i18n::ts!("editor-new-alias")
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(
            create,
            crate::i18n::ts!("automation-alias"),
            alias.package.as_deref(),
        );
        let status = Self::editor_status(create, alias.enabled, false);

        let mut body = column![self.scene_header_with_aside(
            Some(status),
            title,
            Some(subtitle),
            Some(self.header_actions(badge_label, alias.enabled)),
            self.folder_aside(alias.package.as_deref()),
        ),]
        .spacing(16.0);

        body = body.push(
            text(crate::i18n::ts!("editor-deck-alias"))
                .size(13.0)
                .style(common::muted),
        );

        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }

        body = body.push(field_row(
            crate::i18n::ts!("editor-name"),
            text_input(crate::i18n::ts!("editor-example-alias-name"), &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));
        body = body.push(field_row(
            crate::i18n::ts!("editor-match-input-as"),
            self.alias_kind_cards(),
        ));
        match self.alias_draft.kind {
            AliasKind::Command => {
                body = self.alias_command_fields(body, state.name.trim());
            }
            AliasKind::Pattern => {
                body = body.push(field_row(
                    crate::i18n::ts!("editor-pattern"),
                    matcher_field(
                        &self.alias_pattern_content,
                        crate::i18n::ts!("editor-example-alias-simple"),
                        highlight::FieldSyntax::Pattern,
                        (!self.alias_draft.anchor_start, !self.alias_draft.anchor_end),
                        true,
                        Message::AliasPatternAction,
                    ),
                ));
                body = body.push(field_row(
                    "",
                    row![
                        checkbox(!self.alias_draft.anchor_start)
                            .label(crate::i18n::ts!("editor-allow-before"))
                            .on_toggle(|_| Message::ToggleAnchorStart)
                            .size(14.0)
                            .text_size(13.0),
                        checkbox(!self.alias_draft.anchor_end)
                            .label(crate::i18n::ts!("editor-allow-after"))
                            .on_toggle(|_| Message::ToggleAnchorEnd)
                            .size(14.0)
                            .text_size(13.0),
                    ]
                    .spacing(16.0)
                    .into(),
                ));
                if let Some(warning) = self.alias_pattern_warning() {
                    body = body.push(field_row(
                        "",
                        text(warning).size(12.0).style(common::warning).into(),
                    ));
                }
            }
            AliasKind::Regex => {
                body = body.push(field_row(
                    crate::i18n::ts!("editor-regex"),
                    matcher_field(
                        &self.alias_regex_content,
                        crate::i18n::ts!("editor-example-alias-regex"),
                        highlight::FieldSyntax::Regex,
                        regex_loose_sides(&self.alias_draft.regex_source),
                        false,
                        Message::AliasRegexAction,
                    ),
                ));
            }
        }
        body = body.push(self.tester_box(true, false));
        body = body.push(self.order_module(
            alias.priority,
            alias.fallthrough,
            None,
            Some(alias.allow_self_match),
            false,
        ));
        let references = self.alias_capture_references(alias.language);
        if let Some(rail) = self.matched_values_rail(references.clone()) {
            body = body.push(rail);
        }
        let editor = self.action_module(alias.language, references, "alias");
        let bar = self.save_bar(
            create,
            !create,
            if create {
                crate::i18n::ts!("editor-create-alias")
            } else {
                crate::i18n::ts!("action-save")
            },
            Some(crate::i18n::ts!("editor-delete-this-alias")),
        );
        pane_scroll_growing(body, editor, bar, ACTION_EDITOR_MIN_HEIGHT, viewport_height)
    }

    fn view_hotkey_editor<'a>(
        &'a self,
        state: &'a EditorState,
        hotkey: &'a hotkeys::HotkeyDefinition,
        viewport_height: f32,
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let badge_label = if hotkey.language == ScriptLang::Plaintext {
            crate::i18n::ts!("editor-text")
        } else {
            "JavaScript"
        };
        let title = if create {
            crate::i18n::ts!("editor-new-hotkey")
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(
            create,
            crate::i18n::ts!("automation-hotkey"),
            hotkey.package.as_deref(),
        );
        let status = Self::editor_status(create, hotkey.enabled, false);

        let mut body = column![self.scene_header_with_aside(
            Some(status),
            title,
            Some(subtitle),
            Some(self.header_actions(badge_label, hotkey.enabled)),
            self.folder_aside(hotkey.package.as_deref()),
        )]
        .spacing(16.0);
        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }
        body = body.push(field_row(
            crate::i18n::ts!("editor-name"),
            text_input(crate::i18n::ts!("editor-example-hotkey-name"), &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));
        body = body.push(field_row(
            crate::i18n::ts!("editor-shortcut"),
            Element::new(
                HotkeyInput::new(&self.hotkey_state, true)
                    .id(iced::widget::Id::new("automation-hotkey-shortcut"))
                    .height(Length::Fixed(34.0))
                    .on_action(Message::MarkHotkeyState),
            ),
        ));
        let editor = self.hotkey_action_module(hotkey.language);
        let bar = self.save_bar(
            create,
            !create,
            if create {
                crate::i18n::ts!("editor-create-hotkey")
            } else {
                crate::i18n::ts!("action-save")
            },
            None,
        );
        pane_scroll_growing(body, editor, bar, ACTION_EDITOR_MIN_HEIGHT, viewport_height)
    }

    #[allow(clippy::too_many_arguments)]
    fn view_trigger_editor<'a>(
        &'a self,
        state: &'a EditorState,
        enabled: bool,
        language: ScriptLang,
        prompt: bool,
        priority: i32,
        fallthrough: bool,
        rows: &'a [TriggerRow],
        viewport_height: f32,
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let title = if create {
            crate::i18n::ts!("editor-new-trigger")
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(
            create,
            crate::i18n::ts!("automation-trigger"),
            trigger_package(state),
        );
        let any_invalid = rows.iter().any(|row| {
            (!row.source.trim().is_empty() || row.color.is_some()) && row.compiled().is_err()
        });
        let status = Self::editor_status(create, enabled, any_invalid);
        let badge_label = if language == ScriptLang::Plaintext {
            crate::i18n::ts!("editor-text")
        } else {
            "JavaScript"
        };

        let mut body = column![self.scene_header_with_aside(
            Some(status),
            title,
            Some(subtitle),
            Some(self.header_actions(badge_label, enabled)),
            self.folder_aside(trigger_package(state)),
        )]
        .spacing(16.0);

        // Keep this slot mounted while a pattern crosses the valid/invalid
        // boundary. Inserting the banner ahead of the form used to shift the
        // focused text input to a different iced tree position, resetting its
        // state after the first character that made the regex invalid.
        let error = state
            .error
            .as_deref()
            .or_else(|| any_invalid.then(|| crate::i18n::ts!("editor-patterns-invalid")));

        body = body.push(
            text(crate::i18n::ts!("editor-deck-trigger"))
                .size(13.0)
                .style(common::muted),
        );
        body = body.push(error_slot(error));

        body = body.push(field_row(
            crate::i18n::ts!("editor-name"),
            text_input(crate::i18n::ts!("editor-example-trigger-name"), &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));

        // The matcher module (README §4): unselected cards at zero matchers,
        // the same cards as a selector plus one full-width field at one,
        // compact role-grouped rows at two or more. Exceptions and Raw render
        // as labeled groups only when populated; every adder is a text link.
        let match_ids: Vec<usize> = row_ids_with_role(rows, PatternKind::Match);
        let anti_ids: Vec<usize> = row_ids_with_role(rows, PatternKind::Anti);
        let raw_ids: Vec<usize> = row_ids_with_role(rows, PatternKind::Raw);
        let matcher_count = match_ids.len() + raw_ids.len();

        let mut matchers = Column::new().spacing(12.0);
        match matcher_count {
            0 => {
                matchers = matchers.push(self.trigger_cards(None));
            }
            1 => {
                let index = match_ids
                    .first()
                    .or_else(|| raw_ids.first())
                    .copied()
                    .expect("exactly one matcher row");
                let trigger_row = &rows[index];
                matchers =
                    matchers.push(self.trigger_cards(Some(TriggerCard::of_row(trigger_row))));
                if let Some(content) = self.trigger_row_contents.get(index) {
                    matchers = matchers.push(
                        row![
                            trigger_row_field(index, trigger_row, content),
                            dot_with_tooltip(self.row_status(trigger_row), trigger_row.role),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                    );
                }
                if trigger_row.syntax == MatcherSyntax::Pattern {
                    matchers = matchers.push(anchors_row(index, trigger_row));
                }
                if trigger_row.role != PatternKind::Raw {
                    matchers =
                        matchers.push(matcher_color_controls(self.window_id, index, trigger_row));
                }
                if trigger_row.role == PatternKind::Raw && trigger_row.source.trim().is_empty() {
                    matchers = matchers.push(raw_hint());
                }
                // "Another" means another of what you have.
                matchers = matchers.push(if trigger_row.role == PatternKind::Raw {
                    Elem::from(
                        row![
                            text_link(
                                crate::i18n::t!("editor-add-raw-another"),
                                Message::AddRawRow
                            ),
                            text_link(crate::i18n::t!("editor-add-normal"), Message::AddPattern),
                        ]
                        .spacing(16.0),
                    )
                } else {
                    text_link(crate::i18n::t!("editor-add-pattern"), Message::AddPattern)
                });
            }
            _ => {
                // The Matches group carries no header.
                let mut group = Column::new().spacing(6.0);
                for (nth, &index) in match_ids.iter().enumerate() {
                    group = group.push(self.trigger_compact_row(
                        index,
                        &rows[index],
                        nth,
                        match_ids.len(),
                    ));
                }
                group = group.push(if match_ids.is_empty() {
                    text_link(crate::i18n::t!("editor-add-normal"), Message::AddPattern)
                } else {
                    text_link(crate::i18n::t!("editor-add-pattern"), Message::AddPattern)
                });
                matchers = matchers.push(group);
            }
        }

        if !anti_ids.is_empty() {
            let mut group = Column::new().spacing(6.0).push(group_header(
                crate::i18n::ts!("editor-group-exceptions"),
                crate::i18n::ts!("editor-group-exceptions-note"),
                |theme: &Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.error),
                },
            ));
            for (nth, &index) in anti_ids.iter().enumerate() {
                group =
                    group.push(self.trigger_compact_row(index, &rows[index], nth, anti_ids.len()));
            }
            group = group.push(text_link(
                crate::i18n::t!("editor-add-exception-another"),
                Message::AddExceptionRow,
            ));
            matchers = matchers.push(group);
        }

        // At one matcher the lone raw row already renders under the cards; the
        // labeled Raw group appears once the compact layout takes over.
        if !raw_ids.is_empty() && matcher_count >= 2 {
            let mut group = Column::new().spacing(6.0).push(group_header(
                crate::i18n::ts!("editor-group-raw"),
                crate::i18n::ts!("editor-group-raw-note"),
                |_theme: &Theme| iced::widget::text::Style {
                    color: Some(common::KIND_RAW),
                },
            ));
            for (nth, &index) in raw_ids.iter().enumerate() {
                group =
                    group.push(self.trigger_compact_row(index, &rows[index], nth, raw_ids.len()));
            }
            group = group.push(text_link(
                crate::i18n::t!("editor-add-raw-another"),
                Message::AddRawRow,
            ));
            matchers = matchers.push(group);
        }

        // Disclosure links for the groups that do not exist yet.
        let mut disclosures = row![].spacing(16.0);
        let mut any_disclosure = false;
        if anti_ids.is_empty() {
            disclosures = disclosures.push(tip(
                text_link(
                    crate::i18n::t!("editor-add-exception"),
                    Message::AddExceptionRow,
                ),
                crate::i18n::t!("editor-add-exception-tip"),
            ));
            any_disclosure = true;
        }
        if raw_ids.is_empty() {
            disclosures = disclosures.push(tip(
                text_link(crate::i18n::t!("editor-match-raw"), Message::AddRawRow),
                crate::i18n::t!("editor-match-raw-tip"),
            ));
            any_disclosure = true;
        }
        if any_disclosure {
            matchers = matchers.push(disclosures);
        }

        body = body.push(field_row(
            crate::i18n::ts!("editor-patterns"),
            matchers.into(),
        ));

        let has_raw = rows
            .iter()
            .any(|row| row.role == PatternKind::Raw && !row.source.trim().is_empty());
        body = body.push(self.tester_box(false, has_raw));
        body = body.push(self.order_module(priority, fallthrough, Some(prompt), None, true));
        let references = Self::trigger_capture_references(rows, language);
        if let Some(rail) = self.matched_values_rail(references.clone()) {
            body = body.push(rail);
        }
        let editor = self.action_module(language, references, "trigger");
        let bar = self.save_bar(
            create,
            !create,
            if create {
                crate::i18n::ts!("editor-create-trigger")
            } else {
                crate::i18n::ts!("action-save")
            },
            Some(crate::i18n::ts!("editor-delete-this-trigger")),
        );
        pane_scroll_growing(body, editor, bar, ACTION_EDITOR_MIN_HEIGHT, viewport_height)
    }

    /// The three alias type cards, styled per the kind palette. Selection is
    /// the draft's kind; every kind's buffers survive a switch.
    fn alias_kind_cards<'a>(&self) -> Elem<'a> {
        let selected = self.alias_draft.kind;
        row![
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-command"),
                example: crate::i18n::ts!("editor-card-example-command"),
                badge: None,
                hue: common::KIND_COMMAND,
                selected: selected == AliasKind::Command,
                message: Message::SetAliasKind(AliasKind::Command),
            }),
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-pattern"),
                example: crate::i18n::ts!("editor-example-alias-simple"),
                badge: None,
                hue: common::KIND_PATTERN,
                selected: selected == AliasKind::Pattern,
                message: Message::SetAliasKind(AliasKind::Pattern),
            }),
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-regex"),
                example: crate::i18n::ts!("editor-example-alias-regex"),
                badge: Some(crate::i18n::ts!("editor-badge-advanced")),
                hue: common::KIND_REGEX,
                selected: selected == AliasKind::Regex,
                message: Message::SetAliasKind(AliasKind::Regex),
            }),
        ]
        .spacing(12.0)
        .into()
    }

    /// The trigger pane's three matcher cards: a create control while no
    /// matcher exists, a selector for the single matcher's kind+role while
    /// exactly one does.
    fn trigger_cards<'a>(&self, selected: Option<TriggerCard>) -> Elem<'a> {
        row![
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-pattern"),
                example: crate::i18n::ts!("editor-example-trigger-pattern"),
                badge: None,
                hue: common::KIND_PATTERN,
                selected: selected == Some(TriggerCard::Pattern),
                message: Message::SetTriggerCard(TriggerCard::Pattern),
            }),
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-regex"),
                example: crate::i18n::ts!("editor-example-trigger-regex"),
                badge: Some(crate::i18n::ts!("editor-badge-advanced")),
                hue: common::KIND_REGEX,
                selected: selected == Some(TriggerCard::Regex),
                message: Message::SetTriggerCard(TriggerCard::Regex),
            }),
            kind_card(KindCard {
                title: crate::i18n::ts!("editor-kind-raw"),
                example: crate::i18n::ts!("editor-example-trigger-raw"),
                badge: Some(crate::i18n::ts!("editor-badge-wizardry")),
                hue: common::KIND_RAW,
                selected: selected == Some(TriggerCard::Raw),
                message: Message::SetTriggerCard(TriggerCard::Raw),
            }),
        ]
        .spacing(12.0)
        .into()
    }

    /// A row's status dot value against the current test line.
    fn row_status(&self, trigger_row: &TriggerRow) -> NodeStatus {
        if trigger_row.source.trim().is_empty()
            && (trigger_row.color.is_none() || trigger_row.role == PatternKind::Raw)
        {
            return NodeStatus::Disabled;
        }
        let raw_subject = raw_of(&self.test_input);
        let styled =
            smudgy_core::session::connection::vt_processor::parse_ansi_fragment(&raw_subject);
        let subject = if trigger_row.role == PatternKind::Raw {
            raw_subject
        } else {
            styled.text.clone()
        };
        match trigger_row.compiled().map(|s| regex::Regex::new(&s)) {
            Err(_) | Ok(Err(_)) => NodeStatus::Error,
            Ok(Ok(re))
                if !self.test_input.is_empty()
                    && trigger_row.color.as_ref().map_or_else(
                        || re.is_match(&subject),
                        |color| preview_has_color_matched_start(&re, &subject, &styled, color),
                    ) =>
            {
                // A matching exception is what BLOCKS the trigger.
                if trigger_row.role == PatternKind::Anti {
                    NodeStatus::Error
                } else {
                    NodeStatus::Ok
                }
            }
            Ok(Ok(_)) => NodeStatus::Disabled,
        }
    }

    /// One compact matcher row (the 2+ layout): kind control, source field,
    /// status dot, reorder scoped within the role group, and remove — behind
    /// a 3px left bar in the row's kind hue, with the Pattern anchors (and
    /// the raw teaching hint) on following lines.
    fn trigger_compact_row<'a>(
        &'a self,
        index: usize,
        trigger_row: &'a TriggerRow,
        nth: usize,
        group_len: usize,
    ) -> Elem<'a> {
        let Some(content) = self.trigger_row_contents.get(index) else {
            return Column::new().into();
        };
        let kind_control: Elem<'a> = if trigger_row.role == PatternKind::Raw {
            // Raw is always regex; the label is fixed.
            container(
                text(crate::i18n::ts!("editor-syntax-regex"))
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 8.0,
                right: 8.0,
            })
            .into()
        } else {
            pick_list(
                SyntaxChoice::ALL.to_vec(),
                Some(SyntaxChoice(trigger_row.syntax)),
                move |choice| Message::SetRowSyntax(index, choice.0),
            )
            .text_size(13.0)
            .into()
        };

        let mut lines = Column::new().spacing(4.0).push(
            row![
                kind_control,
                trigger_row_field(index, trigger_row, content),
                dot_with_tooltip(self.row_status(trigger_row), trigger_row.role),
                icon_button(
                    bootstrap_icons::CHEVRON_UP,
                    crate::i18n::t!("editor-move-up"),
                    (nth > 0).then_some(Message::MoveRowUp(index)),
                ),
                icon_button(
                    bootstrap_icons::CHEVRON_DOWN,
                    crate::i18n::t!("editor-move-down"),
                    (nth + 1 < group_len).then_some(Message::MoveRowDown(index)),
                ),
                icon_button(
                    bootstrap_icons::TRASH_3,
                    crate::i18n::t!("editor-remove-line"),
                    Some(Message::RemovePattern(index)),
                ),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        if trigger_row.syntax == MatcherSyntax::Pattern {
            lines = lines.push(anchors_row(index, trigger_row));
        }
        if trigger_row.role != PatternKind::Raw {
            lines = lines.push(matcher_color_controls(self.window_id, index, trigger_row));
        }
        if trigger_row.role == PatternKind::Raw && trigger_row.source.trim().is_empty() {
            lines = lines.push(raw_hint());
        }

        let hue = row_kind_hue(trigger_row);
        // As in `matcher_field`: the bar's Fill height must stay internal, or
        // `Row::push`'s size-enclosing would make the whole row report Fill
        // and collapse the group it sits in.
        row![
            container(Space::new())
                .width(Length::Fixed(3.0))
                .height(Length::Fill)
                .style(move |_theme: &Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(hue)),
                    border: iced::Border::default().rounded(2.0),
                    ..Default::default()
                }),
            lines,
        ]
        .spacing(8.0)
        .height(Length::Shrink)
        .into()
    }

    /// The Command kind's field block: the command word beside the mode radio,
    /// the argument rows, the generated usage line, and (Advanced only) the
    /// parsing picker.
    ///
    /// The alias's name *is* the command, so the Command row is read-only:
    /// the Name field above is where the word is edited. A definition saved
    /// with a different stored word (legacy data) shows that word until the
    /// name is next edited, which clears it.
    fn alias_command_fields<'a>(
        &'a self,
        mut body: Column<'a, Message, Theme>,
        alias_name: &'a str,
    ) -> Column<'a, Message, Theme> {
        let draft = &self.alias_draft;
        let mode_radios = row![
            radio(
                crate::i18n::ts!("editor-cmd-simple"),
                CmdMode::Simple,
                Some(draft.cmd_mode),
                Message::SetCmdMode,
            )
            .size(14.0)
            .text_size(13.0),
            radio(
                crate::i18n::ts!("editor-cmd-advanced"),
                CmdMode::Advanced,
                Some(draft.cmd_mode),
                Message::SetCmdMode,
            )
            .size(14.0)
            .text_size(13.0),
        ]
        .spacing(12.0)
        .align_y(Vertical::Center);

        let command_word = draft.command_word(alias_name);
        let word_display = if command_word.is_empty() {
            text(crate::i18n::t!("editor-command-name-empty"))
                .size(13.0)
                .style(common::muted)
        } else {
            text(command_word.to_string())
                .size(14.0)
                .font(fonts::GEIST_MONO_VF)
        };
        body = body.push(field_row(
            crate::i18n::ts!("editor-command"),
            row![word_display, mode_radios]
                .spacing(12.0)
                .align_y(Vertical::Center)
                .into(),
        ));

        let mut args = Column::new().spacing(6.0);
        let last = draft.args.len().saturating_sub(1);
        for (i, arg) in draft.args.iter().enumerate() {
            let mut arg_row = row![
                text_input(crate::i18n::ts!("editor-example-arg-name"), &arg.name)
                    .on_input(move |v| Message::SetArgName(i, v))
                    .size(14.0)
                    .width(Length::Fill),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center);
            if draft.cmd_mode == CmdMode::Advanced {
                // Rest of line is offered only on the last row.
                let options: Vec<ArgKindChoice> = if i == last {
                    vec![
                        ArgKindChoice(ArgKind::Required),
                        ArgKindChoice(ArgKind::Optional),
                        ArgKindChoice(ArgKind::Rest),
                    ]
                } else {
                    vec![
                        ArgKindChoice(ArgKind::Required),
                        ArgKindChoice(ArgKind::Optional),
                    ]
                };
                arg_row = arg_row.push(
                    pick_list(options, Some(ArgKindChoice(arg.kind)), move |choice| {
                        Message::SetArgKind(i, choice.0)
                    })
                    .text_size(13.0),
                );
            }
            arg_row = arg_row.push(
                button(
                    text(bootstrap_icons::TRASH_3)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(14.0),
                )
                .style(button_style::secondary)
                .on_press(Message::RemoveArg(i))
                .padding(8),
            );
            args = args.push(arg_row);
        }
        args = args.push(
            button(
                row![
                    text(bootstrap_icons::PLUS_LG)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(12.0),
                    text(crate::i18n::t!("editor-add-argument")).size(13.0),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center),
            )
            .style(button_style::secondary)
            .on_press(Message::AddArg),
        );
        body = body.push(field_row(crate::i18n::ts!("editor-arguments"), args.into()));

        // The Usage row (label included) is omitted while the command word is
        // empty — an unnamed new alias has nothing to show yet.
        let word = draft.command_word(alias_name);
        if !word.is_empty() {
            body = body.push(field_row(
                crate::i18n::ts!("editor-usage"),
                column![
                    text(matchers::usage_line(word, &draft.args))
                        .size(13.0)
                        .font(fonts::GEIST_MONO_VF),
                    text(crate::i18n::t!("editor-command-completion-note"))
                        .size(12.0)
                        .style(common::muted),
                ]
                .spacing(4.0)
                .into(),
            ));
        }
        if draft.cmd_mode == CmdMode::Advanced {
            body = body.push(field_row(
                crate::i18n::ts!("editor-parsing"),
                self.parsing_picker(),
            ));
        }
        body
    }

    /// The Parsing picker (D7): a custom overlay dropdown, because the rows
    /// are two lines — the label over `example → GETS → result` — and that
    /// pairing is the whole point of the control. The floating list escapes
    /// the scrollable; Escape/click-outside/selection dismiss it, and
    /// up/down/enter drive the keyboard cursor.
    fn parsing_picker<'a>(&self) -> Elem<'a> {
        use smudgy_core::models::matchers::ParseMode;

        let current = self.alias_draft.parse;
        let (current_label, current_example, _) = parse_mode_strings(current);
        let anchor = button(
            row![
                text(current_label).size(13.0),
                text(current_example)
                    .size(12.0)
                    .font(fonts::GEIST_MONO_VF)
                    .style(common::faint),
                text("\u{25BE}").size(10.0).style(common::muted),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        )
        .style(button_style::subtle)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 10.0,
            right: 10.0,
        })
        .on_press(if self.parsing_open {
            Message::CloseParsingPicker
        } else {
            Message::OpenParsingPicker
        });

        let content: Option<Elem<'a>> = self.parsing_open.then(|| {
            let mut list = Column::new().spacing(2.0);
            for (index, choice) in ParseModeChoice::ALL.iter().enumerate() {
                let (label, example, gets) = parse_mode_strings(choice.0);
                let selected = choice.0 == current;
                let at_cursor = index == self.parsing_cursor;
                let inner = column![
                    text(label).size(13.0),
                    row![
                        text(example)
                            .size(12.0)
                            .font(fonts::GEIST_MONO_VF)
                            .style(common::muted),
                        common::section_label(crate::i18n::ts!("editor-gets")),
                        text(gets).size(12.0).font(fonts::GEIST_MONO_VF),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                ]
                .spacing(2.0);
                list = list.push(
                    button(inner)
                        .width(Length::Fill)
                        .style(move |theme: &Theme, status| {
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
                        })
                        .padding(Padding {
                            top: 6.0,
                            bottom: 6.0,
                            left: 10.0,
                            right: 10.0,
                        })
                        .on_press(Message::SetParseMode(choice.0)),
                );
            }
            container(list)
                .width(Length::Fixed(460.0))
                .padding(6.0)
                .style(|theme: &Theme| iced::widget::container::Style {
                    background: Some(theme.styles.modal.body_background),
                    border: theme.styles.modal.body_border,
                    shadow: theme.styles.modal.shadow,
                    ..Default::default()
                })
                .into()
        });

        let cursor_mode: ParseMode =
            ParseModeChoice::ALL[self.parsing_cursor.min(ParseModeChoice::ALL.len() - 1)].0;
        Dropdown::new(anchor, content, Message::CloseParsingPicker)
            .on_key(move |key| match key {
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowUp) => {
                    Some(Message::MoveParsingCursor(-1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown) => {
                    Some(Message::MoveParsingCursor(1))
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter) => {
                    Some(Message::SetParseMode(cursor_mode))
                }
                _ => None,
            })
            .into()
    }

    /// The non-blocking matches-every-line warning for the Pattern kind.
    fn alias_pattern_warning(&self) -> Option<String> {
        let draft = &self.alias_draft;
        let compiled =
            matchers::compile_pattern(&draft.pattern_source, draft.anchor_start, draft.anchor_end);
        (compiled.errors.is_empty()
            && compiled
                .warnings
                .contains(&matchers::PatternWarning::MatchesEveryLine)
            && !draft.pattern_source.trim().is_empty())
        .then(|| crate::i18n::t!("editor-matches-every-line"))
    }

    /// The Try-it module: a collapsed accordion whose header is a call to
    /// action; expanded, the test field, its verdict, and (triggers, when a
    /// raw row exists) the byte view of the simulated raw line.
    fn tester_box<'a>(&self, alias: bool, show_bytes: bool) -> Elem<'a> {
        let header = |chevron: &'static str, label: String| {
            button(
                row![text(chevron).size(11.0), text(label).size(13.0)]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
            )
            .style(button_style::quiet_link)
            .padding(0)
            .width(Length::Fill)
            .on_press(Message::ToggleTryIt)
        };

        if !self.try_it_open {
            let label = if alias {
                crate::i18n::t!("editor-try-alias-cta")
            } else {
                crate::i18n::t!("editor-try-trigger-cta")
            };
            let body = container(header("\u{25B8}", label))
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style);
            return field_row("", body.into());
        }

        let mut inner = column![header("\u{25BE}", crate::i18n::t!("editor-try-it"))].spacing(8.0);
        if alias {
            inner = inner.push(
                row![
                    text("\u{276F}")
                        .size(13.0)
                        .font(fonts::GEIST_MONO_VF)
                        .style(common::capture_accent),
                    text_input(
                        crate::i18n::ts!("editor-test-placeholder-alias"),
                        &self.test_input
                    )
                    .on_input(Message::SetTestInput)
                    .size(13.0),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        } else {
            inner = inner.push(common::section_label(crate::i18n::ts!("editor-game-sent")));
            inner = inner.push(
                text_input(
                    crate::i18n::ts!("editor-test-placeholder-trigger"),
                    &self.test_input,
                )
                .on_input(Message::SetTestInput)
                .size(13.0),
            );
            if show_bytes && !self.test_input.is_empty() {
                let bytes = raw_of(&self.test_input).replace('\x1b', "\u{241B}");
                inner = inner.push(
                    text(format!(
                        "{} {bytes}",
                        crate::i18n::t!("editor-try-bytes-prefix")
                    ))
                    .size(11.0)
                    .font(fonts::GEIST_MONO_VF)
                    .style(common::faint),
                );
            }
        }
        let (verdict, status): (String, NodeStatus) = if alias {
            self.alias_draft_verdict()
        } else {
            self.trigger_verdict()
        };
        inner = inner.push(container(
            row![
                common::status_dot(status),
                text(verdict).size(12.0).style(verdict_style(status)),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
        ));
        let body = container(inner)
            .padding(12.0)
            .width(Length::Fill)
            .style(common::banner_style);
        field_row("", body.into())
    }

    /// The alias tester's verdict, per the draft's kind.
    fn alias_draft_verdict(&self) -> (String, NodeStatus) {
        let draft = &self.alias_draft;
        let sample = &self.test_input;
        let alias_name = match &self.pane {
            Pane::Editor(state) => state.name.trim(),
            _ => "",
        };
        match draft.kind {
            AliasKind::Regex => alias_verdict(&draft.regex_source, sample),
            AliasKind::Pattern => {
                if draft.pattern_source.trim().is_empty() {
                    return (
                        crate::i18n::t!("editor-verdict-no-pattern"),
                        NodeStatus::Disabled,
                    );
                }
                let compiled = matchers::compile_pattern(
                    &draft.pattern_source,
                    draft.anchor_start,
                    draft.anchor_end,
                );
                if let Some(error) = compiled.errors.first() {
                    return (
                        crate::i18n::t!(
                            "editor-verdict-compile-error", "error" => pattern_error_text(error)
                        ),
                        NodeStatus::Error,
                    );
                }
                if sample.is_empty() {
                    return (
                        crate::i18n::t!("editor-enter-command"),
                        NodeStatus::Disabled,
                    );
                }
                match compiled.regex {
                    Some(re) if re.is_match(sample) => {
                        (crate::i18n::t!("editor-would-fire"), NodeStatus::Ok)
                    }
                    _ => (crate::i18n::t!("editor-no-match"), NodeStatus::Disabled),
                }
            }
            AliasKind::Command => {
                let name = draft.command_word(alias_name);
                if name.is_empty() {
                    return (
                        crate::i18n::t!("editor-verdict-no-command"),
                        NodeStatus::Disabled,
                    );
                }
                // The parser matches the first whitespace-delimited token, so
                // say why a spaced word can never fire rather than reporting
                // an ordinary miss.
                if name.contains(char::is_whitespace) {
                    return (
                        crate::i18n::t!("editor-verdict-command-spaces"),
                        NodeStatus::Error,
                    );
                }
                if sample.is_empty() {
                    return (
                        crate::i18n::t!("editor-enter-command"),
                        NodeStatus::Disabled,
                    );
                }
                let spec = CommandSpec {
                    name: name.to_string(),
                    args: draft.args.clone(),
                    parse: draft.parse,
                };
                match matchers::assign(sample, &spec.name, &spec.args, spec.parse) {
                    CommandOutcome::Fired { .. } => {
                        (crate::i18n::t!("editor-would-fire"), NodeStatus::Ok)
                    }
                    CommandOutcome::NotFired(miss) => command_miss_verdict(name, &miss),
                }
            }
        }
    }

    /// The trigger tester's verdict: exceptions veto against each phase's own
    /// subject, then raw rows in order, then normal rows — first hit wins,
    /// one fire per line (the runtime's semantics, told truthfully).
    fn trigger_verdict(&self) -> (String, NodeStatus) {
        let rows = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Trigger { rows, .. },
                ..
            }) => rows,
            _ => {
                return (crate::i18n::t!("editor-no-match"), NodeStatus::Disabled);
            }
        };
        let line = &self.test_input;
        let filled: Vec<(usize, &TriggerRow)> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                !row.source.trim().is_empty()
                    || (row.color.is_some() && row.role != PatternKind::Raw)
            })
            .collect();
        if line.is_empty() {
            return (crate::i18n::t!("editor-enter-line"), NodeStatus::Disabled);
        }
        if !filled.iter().any(|(_, row)| row.role != PatternKind::Anti) {
            return (
                crate::i18n::t!("editor-verdict-no-matchers"),
                NodeStatus::Disabled,
            );
        }

        let raw_subject = raw_of(line);
        let styled =
            smudgy_core::session::connection::vt_processor::parse_ansi_fragment(&raw_subject);
        let plain_subject = styled.text.as_str();
        let compile = |row: &TriggerRow| -> Result<regex::Regex, String> {
            let source = row.compiled()?;
            regex::Regex::new(&source)
                .map_err(|e| crate::i18n::t!("editor-invalid-regex", "error" => e.to_string()))
        };

        // Any compile error surfaces first, as a failing verdict.
        for (_, row) in &filled {
            if let Err(message) = row.compiled() {
                return (
                    crate::i18n::t!("editor-verdict-compile-error", "error" => message),
                    NodeStatus::Error,
                );
            }
        }

        let blocked_in = |subject: &str| -> Option<usize> {
            filled
                .iter()
                .filter(|(_, row)| row.role == PatternKind::Anti)
                .enumerate()
                .find_map(|(nth, (_, row))| {
                    let regex = compile(row).ok()?;
                    let matches = row.color.as_ref().map_or_else(
                        || regex.is_match(subject),
                        |filter| {
                            preview_has_color_matched_start(&regex, plain_subject, &styled, filter)
                        },
                    );
                    matches.then_some(nth + 1)
                })
        };

        let mut first_block = None;
        for role in [PatternKind::Raw, PatternKind::Match] {
            let subject = if role == PatternKind::Raw {
                raw_subject.as_str()
            } else {
                plain_subject
            };
            let phase: Vec<&TriggerRow> = filled
                .iter()
                .filter(|(_, row)| row.role == role)
                .map(|(_, row)| *row)
                .collect();
            if phase.is_empty() {
                continue;
            }
            if let Some(nth) = blocked_in(subject) {
                first_block.get_or_insert(nth);
                continue;
            }
            for (nth, row) in phase.iter().enumerate() {
                if compile(row).is_ok_and(|regex| {
                    row.color.as_ref().map_or_else(
                        || regex.is_match(subject),
                        |filter| {
                            preview_has_color_matched_start(&regex, plain_subject, &styled, filter)
                        },
                    )
                }) {
                    let key = if role == PatternKind::Raw {
                        crate::i18n::t!("editor-fires-on-raw", "n" => (nth + 1).to_string())
                    } else {
                        crate::i18n::t!("editor-fires-on-match", "n" => (nth + 1).to_string())
                    };
                    return (key, NodeStatus::Ok);
                }
            }
        }
        if let Some(nth) = first_block {
            return (
                crate::i18n::t!("editor-blocked-by", "n" => nth.to_string()),
                NodeStatus::Error,
            );
        }
        (crate::i18n::t!("editor-no-match"), NodeStatus::Disabled)
    }

    // ---- folder + module views --------------------------------------------

    pub(super) fn activation_controls<'a>(
        &'a self,
        activation: &ProfileActivation,
        inherited_notices: BTreeMap<String, String>,
    ) -> Elem<'a> {
        let all_active = matches!(activation, ProfileActivation::All);
        let none_active = matches!(activation, ProfileActivation::None);
        let storage_error = self.open_activation_storage_error();
        let storage_available = storage_error.is_none();
        let enable_block_reason = self.activation_enable_block_reason();
        let mut profiles = Column::new().spacing(8.0);
        if let Some(error) = storage_error {
            profiles = profiles.push(text(error).size(12.0).style(common::danger));
        }
        if let Some(reason) = enable_block_reason.as_ref() {
            profiles = profiles.push(text(reason.clone()).size(12.0).style(common::danger));
        }
        if !self.profile_inventory_complete {
            profiles = profiles.push(
                text(crate::i18n::t!("activation-profile-inventory-error"))
                    .size(12.0)
                    .style(common::danger),
            );
        }
        for profile_name in self.profile_names.iter().map(String::as_str) {
            let name = profile_name.to_string();
            let pointer_name = name.clone();
            let current = profile_name == self.profile_name;
            let profile_enabled = activation.is_enabled_for(profile_name);
            let can_toggle_profile = storage_available
                && self.profile_inventory_complete
                && (profile_enabled || enable_block_reason.is_none());
            let profile_checkbox = checkbox(profile_enabled)
                .label(name.clone())
                .size(14.0)
                .text_size(13.0);
            let profile_checkbox: Elem<'a> = if can_toggle_profile {
                profile_checkbox
                    .on_toggle(move |_| Message::ToggleActivationProfile(pointer_name.clone()))
                    .into()
            } else {
                profile_checkbox.into()
            };
            let profile_checkbox = if can_toggle_profile {
                keyboard_activation_control(
                    profile_checkbox,
                    Id::from(format!("automation-activation-profile:{profile_name}")),
                    Message::ToggleActivationProfile(name),
                )
            } else {
                profile_checkbox
            };
            let mut profile_row = row![profile_checkbox]
                .spacing(8.0)
                .align_y(Vertical::Center);
            if current {
                profile_row =
                    profile_row.push(common::badge(crate::i18n::t!("activation-current-profile")));
            }
            profiles = profiles.push(profile_row);
            if let Some(notice) = inherited_notices.get(profile_name) {
                profiles = profiles.push(
                    container(text(notice.clone()).size(12.0).style(common::muted)).padding(
                        Padding {
                            top: 0.0,
                            right: 0.0,
                            bottom: 4.0,
                            left: 22.0,
                        },
                    ),
                );
            }
        }
        if self.profile_inventory_complete && self.profile_names.is_empty() {
            profiles = profiles.push(
                text(crate::i18n::t!("activation-create-profile"))
                    .size(12.0)
                    .style(common::muted),
            );
        }

        let enabled_count = self
            .profile_names
            .iter()
            .filter(|profile| activation.is_enabled_for(profile))
            .count();
        let summary = if !self.profile_inventory_complete {
            crate::i18n::t!("activation-profile-list-unavailable")
        } else if all_active {
            crate::i18n::t!("activation-every-profile")
        } else if none_active {
            crate::i18n::t!("activation-no-profile")
        } else {
            crate::i18n::t!(
                "activation-profile-count",
                "enabled" => enabled_count,
                "total" => self.profile_names.len()
            )
        };
        let enable_button: Elem<'a> =
            button(text(crate::i18n::t!("activation-enable-everywhere")).size(13.0))
                .style(button_style::secondary)
                .on_press_maybe(
                    (storage_available && enable_block_reason.is_none() && !all_active)
                        .then_some(Message::EnableEverywhere),
                )
                .into();
        let enable_button = if all_active || !storage_available || enable_block_reason.is_some() {
            enable_button
        } else {
            keyboard_activation_control(
                enable_button,
                Id::from("automation-activation-enable-everywhere"),
                Message::EnableEverywhere,
            )
        };
        let disable_button: Elem<'a> =
            button(text(crate::i18n::t!("activation-disable-everywhere")).size(13.0))
                .style(button_style::secondary)
                .on_press_maybe(
                    (storage_available && !none_active).then_some(Message::DisableEverywhere),
                )
                .into();
        let disable_button = if none_active || !storage_available {
            disable_button
        } else {
            keyboard_activation_control(
                disable_button,
                Id::from("automation-activation-disable-everywhere"),
                Message::DisableEverywhere,
            )
        };
        let body = column![
            row![
                text(crate::i18n::t!("activation-title"))
                    .size(14.0)
                    .font(Font {
                        weight: iced::font::Weight::Semibold,
                        ..fonts::GEIST_VF
                    }),
                iced::widget::space::horizontal(),
                text(summary).size(12.0).style(common::muted),
            ]
            .align_y(Vertical::Center),
            row![enable_button, disable_button].spacing(8.0),
            profiles,
        ]
        .spacing(10.0);
        container(body)
            .width(Length::Fill)
            .padding(16.0)
            .style(common::card_style)
            .into()
    }

    pub(super) fn view_folder_editor<'a>(&'a self, state: &'a FolderState) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let count = if let Some(path) = &state.original_path {
            self.folder_child_rows(path).len()
        } else {
            0
        };
        let title = if create {
            crate::i18n::t!("editor-new-folder")
        } else {
            state
                .original_path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or(crate::i18n::ts!("editor-folder"))
                .to_string()
        };
        let subtitle = if create {
            crate::i18n::t!("editor-folder")
        } else {
            crate::i18n::t!("editor-folder-summary", "count" => count)
        };
        let directly_enabled = state.activation.is_enabled_for(&self.profile_name);
        let effectively_enabled = state
            .original_path
            .as_deref()
            .map_or(directly_enabled, |path| {
                packages::is_package_effectively_enabled_for(
                    path,
                    &self.packages,
                    &self.profile_name,
                )
            });
        let status = if self.folder_state_error.is_some() {
            NodeStatus::Error
        } else if create || effectively_enabled {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };

        let mut body =
            column![self.scene_header(Some(status), &title, Some(subtitle), None)].spacing(16.0);

        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }
        body = body.push(field_row(
            crate::i18n::ts!("editor-path"),
            text_input(crate::i18n::ts!("editor-example-folder-path"), &state.path)
                .on_input(Message::SetFolderPath)
                .size(14.0)
                .into(),
        ));
        let hint = if !create && !effectively_enabled {
            crate::i18n::ts!("editor-folder-disabled-help")
        } else {
            crate::i18n::ts!("editor-folder-help")
        };
        body = body.push(text(hint).size(12.0).style(common::muted));
        let inherited_notices = state
            .original_path
            .as_deref()
            .map_or_else(BTreeMap::new, |path| {
                self.profile_names
                .iter()
                .filter(|profile| state.activation.is_enabled_for(profile))
                .filter_map(|profile| {
                    packages::disabled_ancestor_for(path, &self.packages, profile).map(|ancestor| {
                        (
                            profile.clone(),
                            crate::i18n::t!("activation-folder-masked", "ancestor" => ancestor),
                        )
                    })
                })
                .collect()
            });
        body = body.push(self.activation_controls(&state.activation, inherited_notices));

        // Contents.
        if let Some(path) = &state.original_path {
            let rows = self.folder_child_rows(path);
            if !rows.is_empty() {
                let mut contents = Column::new()
                    .spacing(4.0)
                    .push(common::section_label(crate::i18n::ts!("editor-contents")));
                for (status, kind_icon, name, msg) in rows {
                    contents = contents.push(
                        button(
                            row![
                                common::status_dot(status),
                                text(kind_icon).font(fonts::BOOTSTRAP_ICONS).size(14.0),
                                text(name).size(13.0),
                            ]
                            .spacing(8.0)
                            .align_y(Vertical::Center),
                        )
                        .style(button_style::list_item)
                        .on_press(msg)
                        .width(Length::Fill),
                    );
                }
                body = body.push(contents);
            }
        }

        // Footer: delete confirm or the save bar.
        if self.confirm_folder_delete {
            body = body.push(
                container(
                    row![
                        text(crate::i18n::t!("editor-delete-folder-question"))
                            .size(13.0)
                            .align_y(Vertical::Center),
                        iced::widget::space::horizontal(),
                        button(text(crate::i18n::t!("editor-move-scripts-parent")).size(13.0))
                            .style(button_style::secondary)
                            .on_press(Message::ConfirmDeleteFolder(false)),
                        button(text(crate::i18n::t!("editor-delete-scripts-too")).size(13.0))
                            .style(button_style::secondary)
                            .on_press(Message::ConfirmDeleteFolder(true)),
                        button(text(crate::i18n::t!("action-cancel")).size(13.0))
                            .style(button_style::secondary)
                            .on_press(Message::CancelDeleteFolder),
                    ]
                    .spacing(10.0)
                    .align_y(Vertical::Center),
                )
                .padding(12.0)
                .style(common::banner_style),
            );
        } else {
            let mut bar = row![]
                .spacing(12.0)
                .align_y(Vertical::Center)
                .padding(Padding {
                    top: 12.0,
                    bottom: 4.0,
                    left: 0.0,
                    right: 0.0,
                });
            if !create {
                bar = bar.push(
                    button(text(crate::i18n::t!("action-delete")).size(13.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            state
                                .error
                                .is_none()
                                .then_some(Message::RequestDeleteFolder),
                        ),
                );
            }
            bar = bar.push(iced::widget::space::horizontal());
            bar = bar.push(
                button(text(crate::i18n::t!("editor-discard")).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
            );
            bar = bar.push(
                button(
                    text(if create {
                        crate::i18n::t!("editor-create-folder")
                    } else {
                        crate::i18n::t!("action-save")
                    })
                    .size(13.0),
                )
                .style(button_style::primary)
                .on_press(Message::SaveFolder),
            );
            body = body.push(bar);
        }
        pane_scroll(body)
    }

    /// (status, icon, name, open-message) for each child of `folder`.
    fn folder_child_rows(&self, folder: &str) -> Vec<(NodeStatus, &'static str, String, Message)> {
        let mut out = Vec::new();
        // Find the folder's child map.
        let mut current = &self.scripts;
        for segment in folder.split('/') {
            let matched = if let Some(exact) = current.get(segment) {
                Some(exact)
            } else {
                let mut matches = current
                    .iter()
                    .filter(|(key, _)| naming::names_conflict(key, segment))
                    .map(|(_, script)| script);
                let first = matches.next();
                matches.next().is_none().then_some(first).flatten()
            };
            match matched {
                Some(Script::Folder(_, children)) => current = children,
                _ => return out,
            }
        }
        for (name, script) in current {
            let (icon, msg, status) = match script {
                Script::Folder(_, _) => {
                    let path = format!("{folder}/{name}");
                    (
                        bootstrap_icons::FOLDER_PLUS,
                        Message::SelectFolder(path.clone()),
                        if packages::is_package_effectively_enabled_for(
                            &path,
                            &self.packages,
                            &self.profile_name,
                        ) {
                            NodeStatus::Ok
                        } else {
                            NodeStatus::Disabled
                        },
                    )
                }
                other => {
                    let icon = match other {
                        Script::Alias(_) => bootstrap_icons::AT,
                        Script::Trigger(_) => bootstrap_icons::LIGHTNING,
                        Script::Hotkey(_) => bootstrap_icons::DPAD,
                        Script::Folder(_, _) => bootstrap_icons::FOLDER_PLUS,
                    };
                    (
                        icon,
                        Message::SelectScript(ScriptKey {
                            folder_name: other.folder_name().map(str::to_string),
                            script_name: name.clone(),
                        }),
                        self.script_status(other),
                    )
                }
            };
            out.push((status, icon, name.clone(), msg));
        }
        out
    }

    /// `viewport_height` is the scroll viewport the pane renders into; on the Source tab the
    /// editor grows into whatever of it the rest of the pane leaves.
    pub(super) fn view_module<'a>(
        &'a self,
        state: &'a ModuleState,
        viewport_height: f32,
    ) -> Elem<'a> {
        let create = state.mode == ModuleMode::Create;
        let executable = create || smudgy_core::models::modules::is_script_module(&state.subpath);
        let title = if create {
            crate::i18n::t!("editor-new-module")
        } else {
            state.subpath.clone()
        };
        let subtitle = crate::i18n::t!("editor-module-help");
        let enabled = executable && state.activation.is_enabled_for(&self.profile_name);
        let mut body = column![self.scene_header(
            executable.then_some(if self.module_state_error.is_some() {
                NodeStatus::Error
            } else if enabled {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            }),
            &title,
            Some(subtitle),
            None
        )]
        .spacing(16.0);
        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }
        if create {
            body = body.push(field_row(
                crate::i18n::ts!("editor-name"),
                text_input(crate::i18n::ts!("editor-example-module-path"), &state.name)
                    .on_input(Message::SetNewModuleName)
                    .size(14.0)
                    .into(),
            ));
            body = body.push(self.activation_controls(&state.activation, BTreeMap::new()));
        }

        if !create {
            let source_tab_label =
                common::unsaved_tab_label(crate::i18n::t!("module-tab-source"), self.dirty);
            let tabs = row![
                module_tab_button(
                    state.tab,
                    ModuleTab::Settings,
                    crate::i18n::ts!("module-tab-settings"),
                ),
                module_tab_button(state.tab, ModuleTab::Source, source_tab_label),
            ]
            .spacing(16.0);
            let current = usize::from(state.tab == ModuleTab::Source);
            let id = Id::from(format!("module-tabs:{}", state.subpath));
            let focus_id = id.clone();
            body = body.push(
                KeyboardControl::new(
                    tabs,
                    id,
                    move || Message::FocusColorControl(focus_id.clone()),
                    move |key, _repeat| {
                        publish_selection(linear_selection(key, current, 2), |index| {
                            Message::SelectModuleTab(if index == 0 {
                                ModuleTab::Settings
                            } else {
                                ModuleTab::Source
                            })
                        })
                    },
                )
                .focus_color(iced::Color::TRANSPARENT),
            );
        }

        if state.tab == ModuleTab::Settings {
            if executable {
                body = body.push(self.activation_controls(&state.activation, BTreeMap::new()));
            } else {
                body = body.push(
                    container(
                        text(crate::i18n::t!("module-import-only-help"))
                            .size(12.0)
                            .style(common::muted),
                    )
                    .padding(12.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            if executable && state.subpath.contains('/') {
                body = body.push(
                    text(crate::i18n::t!("module-nested-load-help"))
                        .size(12.0)
                        .style(common::muted),
                );
            }
            if let Some(automations) = self.live.module(&state.subpath) {
                let total = automations.aliases.len() + automations.triggers.len();
                if total > 0 {
                    let creator_id = format!("module:{}", state.subpath);
                    let mut created = Column::new()
                        .spacing(4.0)
                        .push(common::section_label(crate::i18n::ts!(
                            "module-created-automations"
                        )))
                        .push(
                            text(crate::i18n::t!(
                                "automations-created-count",
                                "count" => total as i64
                            ))
                            .size(12.0)
                            .style(common::muted),
                        );
                    for (name, automation) in &automations.aliases {
                        created = created.push(module_automation_row(
                            &creator_id,
                            AutomationKind::Alias,
                            name,
                            automation.enabled,
                        ));
                    }
                    for (name, automation) in &automations.triggers {
                        created = created.push(module_automation_row(
                            &creator_id,
                            AutomationKind::Trigger,
                            name,
                            automation.enabled,
                        ));
                    }
                    body = body.push(created);
                }
            }
        }
        let source_editor = (create || state.tab == ModuleTab::Source).then(|| {
            column![
                common::section_label(crate::i18n::ts!("editor-source")),
                container(self.code_editor_view(Length::Fill))
                    .height(Length::Fill)
                    .style(common::code_surface_style),
            ]
            .spacing(6.0)
            .height(Length::Fill)
            .into()
        });

        let mut bar = row![]
            .spacing(12.0)
            .align_y(Vertical::Center)
            .padding(Padding {
                top: 12.0,
                bottom: 4.0,
                left: 0.0,
                right: 0.0,
            });
        bar = bar.push(iced::widget::space::horizontal());
        if create || state.tab == ModuleTab::Source {
            bar = bar.push(
                button(text(crate::i18n::t!("editor-discard")).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
            );
        }
        if create {
            bar = bar.push(
                button(text(crate::i18n::t!("editor-create-module")).size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::CreateModule),
            );
        } else if state.tab == ModuleTab::Source {
            bar = bar.push(
                button(text(crate::i18n::t!("action-save")).size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::SaveModule),
            );
        }
        match source_editor {
            Some(editor) => pane_scroll_growing(
                body,
                editor,
                Some(bar.into()),
                MODULE_EDITOR_MIN_HEIGHT,
                viewport_height,
            ),
            None => {
                body = body.push(bar);
                pane_scroll(body)
            }
        }
    }
}

// ---- view helpers ----------------------------------------------------------

fn module_tab_button<'a>(active: ModuleTab, tab: ModuleTab, label: impl Into<String>) -> Elem<'a> {
    common::tab(label, active == tab, Message::SelectModuleTab(tab))
}

fn module_automation_row<'a>(
    creator_id: &str,
    kind: AutomationKind,
    name: &str,
    enabled: bool,
) -> Elem<'a> {
    let (icon, label) = match kind {
        AutomationKind::Alias => (bootstrap_icons::AT, crate::i18n::ts!("automations-aliases")),
        AutomationKind::Trigger => (
            bootstrap_icons::LIGHTNING,
            crate::i18n::ts!("automations-triggers"),
        ),
        AutomationKind::Hotkey => (
            bootstrap_icons::DPAD,
            crate::i18n::ts!("automations-hotkeys"),
        ),
    };
    button(
        row![
            common::status_dot(if enabled {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            }),
            text(icon).font(fonts::BOOTSTRAP_ICONS).size(14.0),
            text(name.to_string()).size(13.0),
            iced::widget::space::horizontal(),
            common::badge(label),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center),
    )
    .style(button_style::list_item)
    .on_press(Message::SelectCreatorAutomation {
        creator_id: creator_id.to_string(),
        kind,
        name: name.to_string(),
    })
    .width(Length::Fill)
    .into()
}

fn subtitle_for(create: bool, kind: &str, package: Option<&str>) -> String {
    if create {
        kind.to_string()
    } else if let Some(folder) = package {
        crate::i18n::t!("editor-kind-in-folder", "kind" => kind, "folder" => folder)
    } else {
        crate::i18n::t!("editor-kind-top-level", "kind" => kind)
    }
}

fn trigger_package(state: &EditorState) -> Option<&str> {
    match &state.node {
        EditNode::Trigger { package, .. } => package.as_deref(),
        _ => None,
    }
}

fn field_row<'a>(label: &str, control: Elem<'a>) -> Elem<'a> {
    row![AutomationsWindow::field_label(label), control]
        .spacing(12.0)
        .align_y(Vertical::Center)
        .into()
}

/// An underlined text link (D8): quiet at rest, full-strength on hover. The
/// underline rule and both colors come from the theme crate so every link in
/// these panes reads the same.
fn text_link<'a>(label: String, message: Message) -> Elem<'a> {
    button(button_style::underlined(text(label).size(12.0)))
        .style(button_style::quiet_link)
        .padding(0)
        .on_press(message)
        .into()
}

/// The destructive underlined link (the `Delete this alias/trigger` footer).
fn danger_link<'a>(label: String, message: Message) -> Elem<'a> {
    button(button_style::underlined(text(label).size(13.0)))
        .style(button_style::danger_link)
        .padding(0)
        .on_press(message)
        .into()
}

// ---- one-line matcher fields ------------------------------------------------

/// Applies an action to a one-line field's buffer: Enter is dropped and
/// pasted newlines flatten to spaces, so the buffer never grows a second line.
pub(super) fn perform_single_line(content: &mut text_editor::Content, action: text_editor::Action) {
    use text_editor::{Action, Edit};
    let action = match action {
        Action::Edit(Edit::Enter) => return,
        Action::Edit(Edit::Paste(pasted)) if pasted.contains('\n') || pasted.contains('\r') => {
            Action::Edit(Edit::Paste(Arc::new(
                pasted.replace("\r\n", "\n").replace(['\r', '\n'], " "),
            )))
        }
        other => other,
    };
    content.perform(action);
}

/// A one-line buffer's text, without the trailing newline `Content::text`
/// always appends.
pub(super) fn single_line_text(content: &text_editor::Content) -> String {
    let mut text = content.text();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    text
}

/// Which sides of a regex source are unanchored (fixtures §10): left iff no
/// leading `^`, right iff no unescaped trailing `$`; neither while empty.
fn regex_loose_sides(source: &str) -> (bool, bool) {
    let source = source.trim();
    if source.is_empty() {
        return (false, false);
    }
    let anchored_end =
        source.ends_with('$') && (source.len() == 1 || !source[..source.len() - 1].ends_with('\\'));
    (!source.starts_with('^'), !anchored_end)
}

/// A small tooltip chip.
fn tip<'a>(content: Elem<'a>, label: String) -> Elem<'a> {
    iced::widget::tooltip(
        content,
        container(text(label).size(11.0))
            .padding(6.0)
            .style(common::banner_style),
        iced::widget::tooltip::Position::Top,
    )
    .into()
}

/// A `. . .` gutter cell (visual-contract §6): the literal spaced string in
/// the mono font on a faint wash, flush against the field inside the
/// composite's single border.
///
/// The cell is ALWAYS in the composite's widget tree; a hidden side keeps
/// its slot and collapses to nothing (empty text, no padding — NOT a
/// `Fixed(0)` width, which `Row::push` treats as void and silently drops
/// from the child list, defeating the whole point). Mounting and unmounting
/// the cell would shift the editor's position among the row's children
/// whenever typing flips the anchor derivation (the first unanchored
/// character, adding or removing `^`/`$`), and iced's positional tree diff
/// would then rebuild the editor's state mid-keystroke — dropping focus and
/// swallowing everything typed after it. A collapsed cell has no hoverable
/// area, so a hidden side's tooltip can never fire.
fn gutter_cell<'a>(shown: bool, tooltip_label: String) -> Elem<'a> {
    let cell = container(
        text(if shown { ". . ." } else { "" })
            .size(11.0)
            .font(fonts::GEIST_MONO_VF)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.32)),
            }),
    )
    .padding(if shown {
        Padding {
            top: 0.0,
            bottom: 0.0,
            left: 8.0,
            right: 8.0,
        }
    } else {
        Padding::ZERO
    })
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .style(move |theme: &Theme| iced::widget::container::Style {
        background: shown
            .then(|| iced::Background::Color(theme.styles.text.normal.scale_alpha(0.04))),
        ..Default::default()
    });
    tip(cell.into(), tooltip_label)
}

/// The color a highlighted run takes (visual-contract §1). The island run is
/// specified as ink on a wash; a highlighter `Format` has no background
/// channel, so the island borrows the Regex kind hue instead — the
/// in-language way to mark "this run is raw regex".
fn token_format(
    token: &highlight::Token,
    theme: &Theme,
) -> iced::advanced::text::highlighter::Format<Font> {
    use highlight::Token;
    let color = match token {
        Token::Hole | Token::GroupOpen | Token::Escape | Token::KnownRef => common::KIND_PATTERN,
        Token::Wildcard => common::KIND_PATTERN.scale_alpha(0.65),
        Token::Island => common::KIND_REGEX,
        Token::UnknownRef => theme.styles.text.error,
    };
    iced::advanced::text::highlighter::Format {
        color: Some(color),
        font: None,
    }
}

/// One matcher source field: `[gutter | editor | gutter]` composed inside a
/// single bordered container (README §5.3) — the editor is chromeless, the
/// composite owns the one border, and the `. . .` gutters appear per `loose`.
/// The editor is a real `text_editor` with highlighted runs and a true caret;
/// Enter is swallowed at the key-binding layer and again in the update path.
fn matcher_field<'a>(
    content: &'a text_editor::Content,
    placeholder: &'a str,
    syntax: highlight::FieldSyntax,
    loose: (bool, bool),
    pattern_tips: bool,
    on_action: impl Fn(text_editor::Action) -> Message + 'a,
) -> Elem<'a> {
    let editor = text_editor(content)
        .placeholder(placeholder)
        .size(13.0)
        .padding(8.0)
        .font(fonts::GEIST_MONO_VF)
        .class(crate::theme::TextEditorClass::Inline)
        .key_binding(|key_press| {
            match text_editor::Binding::from_key_press(key_press) {
                // Swallow the break: captured, but edits nothing.
                Some(text_editor::Binding::Enter) => {
                    Some(text_editor::Binding::Sequence(Vec::new()))
                }
                other => other,
            }
        })
        .highlight_with::<highlight::PatternHighlighter>(syntax, token_format)
        .on_action(on_action);

    let (left_tip, right_tip) = if pattern_tips {
        (
            crate::i18n::t!("editor-gutter-before-pattern"),
            crate::i18n::t!("editor-gutter-after-pattern"),
        )
    } else {
        (
            crate::i18n::t!("editor-gutter-before-regex"),
            crate::i18n::t!("editor-gutter-after-regex"),
        )
    };
    // Both gutters stay mounted in every anchor state (see [`gutter_cell`]) so
    // the editor's tree position — and with it its focus — survives typing.
    //
    // The explicit Shrink heights keep the gutters' `Fill` INTERNAL to the
    // composite: `Row::push` encloses child size hints (one Fill-height child
    // makes the row report Fill), and `Container::new` derives its fluidity
    // from its content's hint — without the pins the whole field would report
    // Fill height, and a Fill-height field next to no fixed-size sibling (the
    // trigger matcher module) measures against nothing and collapses the
    // section.
    let inner = row![
        gutter_cell(loose.0, left_tip),
        editor,
        gutter_cell(loose.1, right_tip),
    ]
    .height(Length::Shrink);
    container(inner)
        .width(Length::Fill)
        .height(Length::Shrink)
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(
                theme.styles.general.container_background,
            )),
            border: iced::Border {
                color: theme.styles.general.border,
                width: 1.0,
                radius: 5.0.into(),
            },
            ..Default::default()
        })
        .into()
}

// ---- trigger matcher rows ---------------------------------------------------

/// Indexes of the rows carrying `role`, in list order.
fn row_ids_with_role(rows: &[TriggerRow], role: PatternKind) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.role == role)
        .map(|(i, _)| i)
        .collect()
}

/// A trigger row's kind hue (visual-contract §1): the card kind that made it —
/// raw rows the Raw hue, otherwise the syntax's hue. Role stays out of the hue
/// channel; it is position plus a wash.
fn row_kind_hue(trigger_row: &TriggerRow) -> iced::Color {
    if trigger_row.role == PatternKind::Raw {
        common::KIND_RAW
    } else if trigger_row.syntax == MatcherSyntax::Pattern {
        common::KIND_PATTERN
    } else {
        common::KIND_REGEX
    }
}

/// The source field for one trigger row: placeholder, grammar, gutters, and
/// role wash all derived from the row.
fn trigger_row_field<'a>(
    index: usize,
    trigger_row: &'a TriggerRow,
    content: &'a text_editor::Content,
) -> Elem<'a> {
    let pattern = trigger_row.syntax == MatcherSyntax::Pattern;
    matcher_field(
        content,
        if pattern {
            crate::i18n::ts!("editor-example-trigger-pattern")
        } else if trigger_row.role == PatternKind::Raw {
            crate::i18n::ts!("editor-example-trigger-raw")
        } else {
            crate::i18n::ts!("editor-example-trigger-regex")
        },
        if pattern {
            highlight::FieldSyntax::Pattern
        } else {
            highlight::FieldSyntax::Regex
        },
        if pattern {
            (!trigger_row.anchor_start, !trigger_row.anchor_end)
        } else {
            regex_loose_sides(&trigger_row.source)
        },
        pattern,
        move |action| Message::RowSourceAction(index, action),
    )
}

/// The anchor checkboxes a Pattern-syntax row carries on its second line.
fn anchors_row<'a>(index: usize, trigger_row: &TriggerRow) -> Elem<'a> {
    row![
        checkbox(!trigger_row.anchor_start)
            .label(crate::i18n::ts!("editor-allow-before"))
            .on_toggle(move |_| Message::ToggleRowAnchorStart(index))
            .size(14.0)
            .text_size(12.0),
        checkbox(!trigger_row.anchor_end)
            .label(crate::i18n::ts!("editor-allow-after"))
            .on_toggle(move |_| Message::ToggleRowAnchorEnd(index))
            .size(14.0)
            .text_size(12.0),
    ]
    .spacing(16.0)
    .into()
}

/// Returns the inline validation message for an unconstrained color-only row.
fn color_filter_constraint_error(trigger_row: &TriggerRow) -> Option<&'static str> {
    let filter = trigger_row.color.as_ref()?;
    (trigger_row.source.is_empty()
        && filter.foreground.is_none()
        && filter.background.is_none()
        && filter.attributes.is_empty())
    .then(|| crate::i18n::ts!("editor-color-needs-constraint"))
}

const COLOR_CHANNELS: [MatcherColorChannel; 2] = [
    MatcherColorChannel::Foreground,
    MatcherColorChannel::Background,
];

fn color_control_id(window_id: iced::window::Id, row_index: usize, control: &str) -> Id {
    Id::from(format!(
        "automation-window-{window_id}-trigger-color-row-{row_index}-{control}"
    ))
}

fn color_attribute_control_id(
    window_id: iced::window::Id,
    row_index: usize,
    attribute: MatcherTextAttribute,
) -> Id {
    let name = match attribute {
        MatcherTextAttribute::Bold => "attribute-bold",
        MatcherTextAttribute::Faint => "attribute-faint",
        MatcherTextAttribute::Italic => "attribute-italic",
        MatcherTextAttribute::Underline => "attribute-underline",
        MatcherTextAttribute::DoubleUnderline => "attribute-double-underline",
        MatcherTextAttribute::SlowBlink => "attribute-slow-blink",
        MatcherTextAttribute::FastBlink => "attribute-fast-blink",
        MatcherTextAttribute::CrossedOut => "attribute-crossed-out",
        MatcherTextAttribute::Reverse => "attribute-reverse",
    };
    color_control_id(window_id, row_index, name)
}

fn color_keyboard_control<'a>(
    content: Elem<'a>,
    id: Id,
    focus_color: iced::Color,
    on_key: impl Fn(&Key, bool) -> KeyAction<Message> + 'a,
) -> Elem<'a> {
    let focus_id = id.clone();
    KeyboardControl::new(
        content,
        id,
        move || Message::FocusColorControl(focus_id.clone()),
        on_key,
    )
    .focus_color(focus_color)
    .into()
}

/// Builds the color filter below a normal or exception matcher.
///
/// Foreground and background use independent tabs. Each channel retains its
/// selection when the user selects the other tab. The choices are Any, ANSI,
/// xterm, an exact truecolor, and a color range. Every selection other than
/// Any must match.
fn matcher_color_controls<'a>(
    window_id: iced::window::Id,
    index: usize,
    trigger_row: &'a TriggerRow,
) -> Elem<'a> {
    let enabled = trigger_row.color.is_some();
    let focus_color = crate::prefs::app_theme().styles.general.accent;
    let heading_content: Elem<'a> = row![
        checkbox(enabled)
            .label(crate::i18n::ts!("editor-match-color"))
            .on_toggle(move |value| Message::ToggleRowColor(index, value))
            .size(14.0)
            .text_size(12.0),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center)
    .into();
    let heading = color_keyboard_control(
        heading_content,
        color_control_id(window_id, index, "match"),
        focus_color,
        move |key, repeat| activation(key, repeat, Message::ToggleRowColor(index, !enabled)),
    );

    let Some(filter) = &trigger_row.color else {
        return heading;
    };

    let channel_tab = |label: &'static str, channel: MatcherColorChannel| {
        selector_tab(
            label,
            trigger_row.color_channel == channel,
            Message::SelectRowColorChannel(index, channel),
        )
    };
    let channels_content: Elem<'a> = row![
        channel_tab(
            crate::i18n::ts!("editor-color-foreground"),
            MatcherColorChannel::Foreground,
        ),
        channel_tab(
            crate::i18n::ts!("editor-color-background"),
            MatcherColorChannel::Background,
        ),
    ]
    .spacing(12.0)
    .into();
    let selected_channel = TriggerRow::color_channel_index(trigger_row.color_channel);
    let channels = color_keyboard_control(
        channels_content,
        color_control_id(window_id, index, "channel"),
        focus_color,
        move |key, _repeat| {
            publish_selection(
                linear_selection(key, selected_channel, COLOR_CHANNELS.len()),
                |selected| Message::SelectRowColorChannel(index, COLOR_CHANNELS[selected]),
            )
        },
    );

    let selected_color = match trigger_row.color_channel {
        MatcherColorChannel::Foreground => filter.foreground,
        MatcherColorChannel::Background => filter.background,
    };
    let selected_kind = MatcherColorKind::of(selected_color);
    let color_draft = trigger_row.color_draft(trigger_row.color_channel);
    let kind_label = |kind| match kind {
        MatcherColorKind::Any => crate::i18n::ts!("editor-color-any"),
        MatcherColorKind::Ansi => crate::i18n::ts!("editor-color-ansi"),
        MatcherColorKind::Xterm => crate::i18n::ts!("editor-color-xterm"),
        MatcherColorKind::Truecolor => crate::i18n::ts!("editor-color-truecolor"),
        MatcherColorKind::ColorRange => crate::i18n::ts!("editor-color-range"),
    };
    let kinds_content: Elem<'a> = MatcherColorKind::ALL
        .into_iter()
        .fold(iced::widget::Row::new().spacing(12.0), |row, kind| {
            row.push(selector_tab(
                kind_label(kind),
                selected_kind == kind,
                Message::SelectRowColorKind(index, kind),
            ))
        })
        .into();
    let selected_kind_index = MatcherColorKind::ALL
        .iter()
        .position(|kind| *kind == selected_kind)
        .unwrap_or(0);
    let kinds = color_keyboard_control(
        kinds_content,
        color_control_id(window_id, index, "kind"),
        focus_color,
        move |key, _repeat| {
            publish_selection(
                linear_selection(key, selected_kind_index, MatcherColorKind::ALL.len()),
                |selected| Message::SelectRowColorKind(index, MatcherColorKind::ALL[selected]),
            )
        },
    );

    let chooser: Elem<'a> = match selected_kind {
        MatcherColorKind::Any => text(crate::i18n::ts!("editor-color-any-note"))
            .size(12.0)
            .style(common::muted)
            .into(),
        MatcherColorKind::Ansi => ansi_color_grid(window_id, index, selected_color, focus_color),
        MatcherColorKind::Xterm => xterm_color_grid(window_id, index, selected_color, focus_color),
        MatcherColorKind::Truecolor => exact_truecolor_editor(index, trigger_row, selected_color),
        MatcherColorKind::ColorRange => {
            let point = MatcherHsv::from_rgb(255, 255, 255);
            let range = selected_color
                .and_then(matcher_truecolor_range)
                .unwrap_or_else(|| MatcherHsvRange::from_to(point, point));
            column![
                row![
                    color_range_endpoint_editor(
                        index,
                        ColorRangeEndpoint::First,
                        crate::i18n::ts!("editor-color-from"),
                        range.first,
                        &color_draft.color_range_hex[0],
                    ),
                    color_range_endpoint_editor(
                        index,
                        ColorRangeEndpoint::Second,
                        crate::i18n::ts!("editor-color-to"),
                        range.second,
                        &color_draft.color_range_hex[1],
                    ),
                ]
                .spacing(12.0),
                row![
                    range_color_swatches(range),
                    text(hsv_range_name(range)).size(11.0),
                ]
                .spacing(10.0)
                .align_y(Vertical::Center),
                text(crate::i18n::ts!("editor-color-range-note"))
                    .size(11.0)
                    .style(common::muted),
            ]
            .spacing(8.0)
            .into()
        }
    };

    let attributes = color_attribute_controls(window_id, index, filter, focus_color);
    let panel = container(
        column![
            channels,
            kinds,
            chooser,
            attributes,
            inline_error_slot(color_filter_constraint_error(trigger_row)),
        ]
        .spacing(8.0),
    )
    .padding(8.0)
    .style(|theme: &Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(
            theme.styles.general.container_background,
        )),
        border: iced::Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    });

    column![heading, color_filter_summary_chip(filter), panel]
        .spacing(6.0)
        .into()
}

/// Keeps live validation text at a stable widget-tree position.
fn inline_error_slot<'a>(message: Option<&'a str>) -> Elem<'a> {
    let content: Elem<'a> = match message {
        Some(message) => text(message).size(11.0).style(common::danger).into(),
        None => Space::new().height(0).into(),
    };
    container(content).into()
}

fn exact_truecolor_editor<'a>(
    index: usize,
    trigger_row: &'a TriggerRow,
    selected_color: Option<MatcherColor>,
) -> Elem<'a> {
    let (r, g, b) = match selected_color {
        Some(MatcherColor::Truecolor {
            r,
            g,
            b,
            range: None,
        }) => (r, g, b),
        _ => (255, 255, 255),
    };
    let draft = &trigger_row
        .color_draft(trigger_row.color_channel)
        .exact_truecolor;
    let hex_valid = parse_matcher_hex(&draft.hex).is_some();
    let rgb_valid = draft.rgb.iter().all(|value| value.parse::<u8>().is_ok());
    let label = |value: &'a str| {
        text(value).size(11.0).font(Font {
            weight: iced::font::Weight::Semibold,
            ..fonts::GEIST_VF
        })
    };
    column![
        row![
            column![
                label(crate::i18n::ts!("editor-color-preview")),
                mini_color_swatch(iced::Color::from_rgb8(r, g, b), 30.0),
            ]
            .spacing(5.0),
            column![
                label(crate::i18n::ts!("editor-color-hex")),
                text_input("#rrggbb", &draft.hex)
                    .on_input(move |value| Message::SetRowExactTruecolorHex(index, value))
                    .width(Length::Fixed(120.0))
                    .size(12.0),
            ]
            .spacing(5.0),
            column![
                label(crate::i18n::ts!("editor-color-red")),
                text_input("0", &draft.rgb[0])
                    .on_input(move |value| Message::SetRowExactTruecolorRgb(
                        index,
                        TruecolorComponent::Red,
                        value,
                    ))
                    .width(Length::Fixed(64.0))
                    .size(12.0),
            ]
            .spacing(5.0),
            column![
                label(crate::i18n::ts!("editor-color-green")),
                text_input("0", &draft.rgb[1])
                    .on_input(move |value| Message::SetRowExactTruecolorRgb(
                        index,
                        TruecolorComponent::Green,
                        value,
                    ))
                    .width(Length::Fixed(64.0))
                    .size(12.0),
            ]
            .spacing(5.0),
            column![
                label(crate::i18n::ts!("editor-color-blue")),
                text_input("0", &draft.rgb[2])
                    .on_input(move |value| Message::SetRowExactTruecolorRgb(
                        index,
                        TruecolorComponent::Blue,
                        value,
                    ))
                    .width(Length::Fixed(64.0))
                    .size(12.0),
            ]
            .spacing(5.0),
        ]
        .spacing(10.0)
        .align_y(Vertical::Bottom),
        inline_error_slot((!hex_valid).then_some(crate::i18n::ts!("editor-color-invalid-hex"))),
        inline_error_slot((!rgb_valid).then_some(crate::i18n::ts!("editor-color-invalid-rgb"))),
        text(crate::i18n::ts!("editor-color-truecolor-note"))
            .size(11.0)
            .style(common::muted),
    ]
    .spacing(5.0)
    .into()
}

fn color_range_endpoint_editor<'a>(
    index: usize,
    endpoint: ColorRangeEndpoint,
    label: &'a str,
    hsv: MatcherHsv,
    hex: &'a str,
) -> Elem<'a> {
    let picker = ColorPicker::view_for_hsv(matcher_hsv_to_picker(hsv))
        .map(move |message| Message::SetRowColorRange(index, endpoint, message));
    let (r, g, b) = hsv.to_rgb();
    container(
        column![
            row![
                text(label).size(11.0).font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..fonts::GEIST_VF
                }),
                mini_color_swatch(iced::Color::from_rgb8(r, g, b), 14.0),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
            picker,
            column![
                text(crate::i18n::ts!("editor-color-hex"))
                    .size(11.0)
                    .style(common::muted),
                text_input("#rrggbb", hex)
                    .on_input(move |value| Message::SetRowColorRangeHex(index, endpoint, value))
                    .width(Length::Fill)
                    .size(12.0),
            ]
            .spacing(4.0),
            inline_error_slot(
                parse_matcher_hex(hex)
                    .is_none()
                    .then_some(crate::i18n::ts!("editor-color-invalid-hex"))
            ),
        ]
        .spacing(6.0),
    )
    .width(Length::FillPortion(1))
    .padding(8.0)
    .style(|theme: &Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(
            theme.styles.text.normal.scale_alpha(0.035),
        )),
        border: iced::Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn selector_tab<'a>(label: &'a str, selected: bool, message: Message) -> Elem<'a> {
    let mut control = button(
        column![
            text(label).size(12.0).style(if selected {
                common::regular
            } else {
                common::muted
            }),
            container(Space::new())
                .height(Length::Fixed(2.0))
                .width(Length::Fill)
                .style(move |theme: &Theme| iced::widget::container::Style {
                    background: selected
                        .then_some(iced::Background::Color(theme.styles.general.accent)),
                    ..Default::default()
                }),
        ]
        .spacing(3.0),
    )
    .style(|_theme: &Theme, _status| iced::widget::button::Style::default())
    .padding(Padding {
        top: 2.0,
        bottom: 0.0,
        left: 2.0,
        right: 2.0,
    });
    if !selected {
        control = control.on_press(message);
    }
    control.into()
}

fn ansi_color_grid<'a>(
    window_id: iced::window::Id,
    index: usize,
    selected: Option<MatcherColor>,
    focus_color: iced::Color,
) -> Elem<'a> {
    let selected = match selected {
        Some(MatcherColor::Ansi { index }) => Some(index.min(15)),
        _ => None,
    };
    let mut grid = Column::new().spacing(4.0);
    for row_index in 0..2 {
        let mut swatches = iced::widget::Row::new().spacing(4.0);
        for column_index in 0..8 {
            let ansi_index = row_index * 8 + column_index;
            swatches = swatches.push(color_swatch(
                MatcherColor::Ansi {
                    index: ansi_index as u8,
                },
                selected == Some(ansi_index as u8),
                Message::SetRowAnsiColor(index, ansi_index as u8),
                palette_color_identifier(MatcherColor::Ansi {
                    index: ansi_index as u8,
                }),
            ));
        }
        grid = grid.push(swatches);
    }
    if let Some(selected) = selected {
        grid = grid.push(palette_selected_row(MatcherColor::Ansi { index: selected }));
    }
    let content: Elem<'a> = grid.into();
    let current = usize::from(selected.unwrap_or(7));
    color_keyboard_control(
        content,
        color_control_id(window_id, index, "ansi-grid"),
        focus_color,
        move |key, _repeat| {
            publish_selection(grid_selection(key, current, 8, 16), |selected| {
                Message::SetRowAnsiColor(index, selected as u8)
            })
        },
    )
}

fn xterm_color_grid<'a>(
    window_id: iced::window::Id,
    index: usize,
    selected: Option<MatcherColor>,
    focus_color: iced::Color,
) -> Elem<'a> {
    let selected = match selected {
        Some(MatcherColor::Xterm { index }) => Some(index),
        _ => None,
    };
    let mut grid = Column::new().spacing(2.0);
    for row_index in 0..16_u16 {
        let mut swatches = iced::widget::Row::new().spacing(2.0);
        for column_index in 0..16_u16 {
            let xterm_index = (row_index * 16 + column_index) as u8;
            swatches = swatches.push(color_swatch(
                MatcherColor::Xterm { index: xterm_index },
                selected == Some(xterm_index),
                Message::SetRowXtermColor(index, xterm_index),
                palette_color_identifier(MatcherColor::Xterm { index: xterm_index }),
            ));
        }
        grid = grid.push(swatches);
    }
    if let Some(selected) = selected {
        grid = grid.push(palette_selected_row(MatcherColor::Xterm {
            index: selected,
        }));
    }
    let content: Elem<'a> = grid.into();
    let current = usize::from(selected.unwrap_or(7));
    color_keyboard_control(
        content,
        color_control_id(window_id, index, "xterm-grid"),
        focus_color,
        move |key, _repeat| {
            publish_selection(grid_selection(key, current, 16, 256), |selected| {
                Message::SetRowXtermColor(index, selected as u8)
            })
        },
    )
}

fn palette_color_identifier(color: MatcherColor) -> String {
    match color {
        MatcherColor::Ansi { index } => format!("ANSI {index}"),
        MatcherColor::Xterm { index } => format!("xterm {index}"),
        MatcherColor::Truecolor { .. } => matcher_color_name(color),
    }
}

fn palette_selected_row<'a>(color: MatcherColor) -> Elem<'a> {
    row![
        mini_color_swatch(matcher_display_color(color), 14.0),
        text(crate::i18n::t!(
            "editor-color-selected",
            "color" => palette_color_identifier(color)
        ))
        .size(11.0),
    ]
    .spacing(6.0)
    .align_y(Vertical::Center)
    .into()
}

fn color_swatch<'a>(
    color: MatcherColor,
    selected: bool,
    message: Message,
    label: String,
) -> Elem<'a> {
    let display = matcher_display_color(color);
    let size = if selected { 28.0 } else { 24.0 };
    let control = button(Space::new())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .padding(0.0)
        .style(move |theme: &Theme, status| iced::widget::button::Style {
            background: Some(iced::Background::Color(display)),
            border: iced::Border {
                color: if selected {
                    theme.styles.general.accent
                } else if status == iced::widget::button::Status::Hovered {
                    theme.styles.text.normal
                } else {
                    theme.styles.general.border
                },
                width: if selected { 3.0 } else { 1.0 },
                radius: 4.0.into(),
            },
            shadow: if selected {
                iced::Shadow {
                    color: iced::Color::BLACK.scale_alpha(0.45),
                    offset: iced::Vector::new(0.0, 2.0),
                    blur_radius: 5.0,
                }
            } else {
                iced::Shadow::default()
            },
            ..Default::default()
        })
        .on_press(message);
    let layer = container(control)
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(Vertical::Center);
    let exploded: Elem<'a> = if selected {
        let base = container(Space::new())
            .width(Length::Fixed(24.0))
            .height(Length::Fixed(24.0))
            .style(move |theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(display)),
                border: iced::Border {
                    color: theme.styles.general.border,
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..Default::default()
            });
        iced::widget::stack![
            container(base)
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(Vertical::Center),
            layer,
        ]
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .into()
    } else {
        layer.into()
    };
    tip(exploded, label)
}

fn mini_color_swatch<'a>(color: iced::Color, size: f32) -> Elem<'a> {
    container(Space::new())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border {
                color: theme.styles.general.border,
                width: 1.0,
                radius: 2.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn range_color_swatches<'a>(range: MatcherHsvRange) -> Elem<'a> {
    let range = range.rgb_canonicalized();
    let (from, to) = range.directed_endpoints();
    let display = |hsv: MatcherHsv| {
        let (r, g, b) = hsv.to_rgb();
        iced::Color::from_rgb8(r, g, b)
    };
    row![
        mini_color_swatch(display(from), 11.0),
        mini_color_swatch(display(to), 11.0),
    ]
    .spacing(2.0)
    .into()
}

fn matcher_color_swatches<'a>(color: MatcherColor) -> Elem<'a> {
    match color {
        MatcherColor::Truecolor {
            range: Some(range), ..
        } => range_color_swatches(range),
        _ => mini_color_swatch(matcher_display_color(color), 11.0),
    }
}

fn channel_color_summary<'a>(label: &'a str, color: Option<MatcherColor>) -> Elem<'a> {
    let mut content = iced::widget::Row::new()
        .spacing(4.0)
        .align_y(Vertical::Center)
        .push(text(label).size(11.0));
    if let Some(color) = color {
        content = content
            .push(matcher_color_swatches(color))
            .push(text(matcher_color_name(color)).size(11.0));
    } else {
        content = content.push(text(crate::i18n::ts!("editor-color-any-short")).size(11.0));
    }
    content.into()
}

fn color_filter_summary_chip<'a>(filter: &MatcherColorMatch) -> Elem<'a> {
    let mut parts = vec![channel_color_summary(
        crate::i18n::ts!("editor-color-foreground"),
        filter.foreground,
    )];
    parts.push(
        row![
            text("·").size(11.0),
            channel_color_summary(
                crate::i18n::ts!("editor-color-background"),
                filter.background,
            ),
        ]
        .spacing(4.0)
        .align_y(Vertical::Center)
        .into(),
    );
    parts.extend(filter.attributes.iter().map(|attribute| {
        Elem::from(
            text(format!(
                ", {}",
                color_attribute_label(*attribute).to_lowercase()
            ))
            .size(11.0),
        )
    }));
    container(wrap_row(parts).spacing(4.0, 3.0))
        .padding(Padding {
            top: 2.0,
            bottom: 2.0,
            left: 6.0,
            right: 6.0,
        })
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(
                theme.styles.text.normal.scale_alpha(0.06),
            )),
            border: iced::Border {
                color: theme.styles.general.border,
                width: 1.0,
                radius: 9.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn color_attribute_label(attribute: MatcherTextAttribute) -> &'static str {
    crate::i18n::translate_static(match attribute {
        MatcherTextAttribute::Bold => "editor-color-bold",
        MatcherTextAttribute::Faint => "editor-color-faint",
        MatcherTextAttribute::Italic => "editor-color-italic",
        MatcherTextAttribute::Underline => "editor-color-underline",
        MatcherTextAttribute::DoubleUnderline => "editor-color-double-underline",
        MatcherTextAttribute::SlowBlink => "editor-color-slow-blink",
        MatcherTextAttribute::FastBlink => "editor-color-fast-blink",
        MatcherTextAttribute::CrossedOut => "editor-color-crossed-out",
        MatcherTextAttribute::Reverse => "editor-color-reverse",
    })
}

fn color_attribute_controls<'a>(
    window_id: iced::window::Id,
    index: usize,
    filter: &MatcherColorMatch,
    focus_color: iced::Color,
) -> Elem<'a> {
    let choices = [
        (MatcherTextAttribute::Bold, "editor-color-bold"),
        (MatcherTextAttribute::Faint, "editor-color-faint"),
        (MatcherTextAttribute::Italic, "editor-color-italic"),
        (MatcherTextAttribute::Underline, "editor-color-underline"),
        (
            MatcherTextAttribute::DoubleUnderline,
            "editor-color-double-underline",
        ),
        (MatcherTextAttribute::SlowBlink, "editor-color-slow-blink"),
        (MatcherTextAttribute::FastBlink, "editor-color-fast-blink"),
        (MatcherTextAttribute::CrossedOut, "editor-color-crossed-out"),
        (MatcherTextAttribute::Reverse, "editor-color-reverse"),
    ];
    let children = choices
        .into_iter()
        .map(|(attribute, key)| {
            let selected = filter.attributes.contains(&attribute);
            let content: Elem<'a> = checkbox(selected)
                .label(crate::i18n::translate_static(key))
                .on_toggle(move |value| Message::ToggleRowColorAttribute(index, attribute, value))
                .size(13.0)
                .text_size(11.0)
                .into();
            color_keyboard_control(
                content,
                color_attribute_control_id(window_id, index, attribute),
                focus_color,
                move |key, repeat| {
                    activation(
                        key,
                        repeat,
                        Message::ToggleRowColorAttribute(index, attribute, !selected),
                    )
                },
            )
        })
        .collect();
    column![
        text(crate::i18n::ts!("editor-color-attributes"))
            .size(11.0)
            .style(common::muted),
        wrap_row(children).spacing(12.0, 5.0),
    ]
    .spacing(5.0)
    .into()
}

fn matcher_color_name(color: MatcherColor) -> String {
    match color {
        MatcherColor::Ansi { index } => ansi_color_name(index),
        MatcherColor::Xterm { index } => format!("xterm {index}"),
        MatcherColor::Truecolor {
            range: Some(range), ..
        } => hsv_range_name(range),
        MatcherColor::Truecolor {
            r,
            g,
            b,
            range: None,
        } => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn ansi_color_name(index: u8) -> String {
    format!("ANSI {index}")
}

fn hsv_range_name(range: MatcherHsvRange) -> String {
    let range = range.rgb_canonicalized();
    let (from, to) = range.directed_endpoints();
    let (saturation_min, saturation_max) = range.saturation_bounds();
    let (value_min, value_max) = range.value_bounds();
    let percent = |component: u8| (u16::from(component) * 100 + 127) / 255;
    format!(
        "H {}→{}°  S {}–{}%  V {}–{}%",
        from.hue,
        to.hue,
        percent(saturation_min),
        percent(saturation_max),
        percent(value_min),
        percent(value_max),
    )
}

fn matcher_display_color(color: MatcherColor) -> iced::Color {
    let prefs = crate::prefs::current();
    prefs.resolve(matcher_vt_color(color))
}

fn matcher_vt_color(color: MatcherColor) -> smudgy_core::session::styled_line::Color {
    use smudgy_core::session::connection::vt_processor::AnsiColor;
    use smudgy_core::session::styled_line::Color;
    let ansi = |index: u8| match index % 8 {
        0 => AnsiColor::Black,
        1 => AnsiColor::Red,
        2 => AnsiColor::Green,
        3 => AnsiColor::Yellow,
        4 => AnsiColor::Blue,
        5 => AnsiColor::Magenta,
        6 => AnsiColor::Cyan,
        _ => AnsiColor::White,
    };
    match color {
        MatcherColor::Ansi { index } | MatcherColor::Xterm { index } if index < 16 => Color::Ansi {
            color: ansi(index),
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
        MatcherColor::Ansi { index } => Color::Ansi {
            color: ansi(index),
            bold: index >= 8,
        },
    }
}

fn preview_has_color_matched_start(
    regex: &regex::Regex,
    subject: &str,
    line: &smudgy_core::session::styled_line::StyledLine,
    filter: &MatcherColorMatch,
) -> bool {
    let bold_is_bright = crate::prefs::current().bold_mode.uses_bright_palette();
    if regex.as_str().is_empty() {
        // Consecutive SGR changes can create zero-width transition spans.
        // These spans contain no text. An empty pattern cannot start in them
        // on a nonempty line. On an empty line, the final span defines the
        // cursor style at the only match position.
        return if subject.is_empty() {
            line.spans
                .last()
                .is_some_and(|span| preview_style_matches(span.style, filter, bold_is_bright))
        } else {
            line.spans.iter().any(|span| {
                span.begin_pos < span.end_pos
                    && preview_style_matches(span.style, filter, bold_is_bright)
            })
        };
    }
    let mut span_index = 0;
    let mut cached_span_index = usize::MAX;
    let mut cached_style_matches = false;
    regex.find_iter(subject).any(|matched| {
        let start = matched.start();
        while line
            .spans
            .get(span_index)
            .is_some_and(|span| span.end_pos <= start)
        {
            span_index += 1;
        }
        let Some(span) = line.spans.get(span_index) else {
            return false;
        };
        if span.begin_pos > start || start >= span.end_pos {
            return false;
        }
        if cached_span_index != span_index {
            cached_span_index = span_index;
            cached_style_matches = preview_style_matches(span.style, filter, bold_is_bright);
        }
        cached_style_matches
    })
}

fn preview_style_matches(
    style: smudgy_core::session::styled_line::Style,
    filter: &MatcherColorMatch,
    bold_is_bright: bool,
) -> bool {
    use smudgy_core::session::styled_line::{Blink, Color, Underline};
    let foreground = if style.attributes.bold && bold_is_bright {
        match style.fg {
            Color::Ansi { color, bold: false } => Color::Ansi { color, bold: true },
            Color::DefaultForeground { bold: false } => Color::DefaultForeground { bold: true },
            other => other,
        }
    } else {
        style.fg
    };
    if filter
        .foreground
        .is_some_and(|color| !preview_color_matches(foreground, color))
        || filter
            .background
            .is_some_and(|color| !preview_color_matches(style.bg, color))
    {
        return false;
    }
    filter.attributes.iter().all(|attribute| match attribute {
        MatcherTextAttribute::Bold => style.attributes.bold,
        MatcherTextAttribute::Faint => style.attributes.faint,
        MatcherTextAttribute::Italic => style.attributes.italic,
        MatcherTextAttribute::Underline => style.attributes.underline == Underline::Single,
        MatcherTextAttribute::DoubleUnderline => style.attributes.underline == Underline::Double,
        MatcherTextAttribute::SlowBlink => style.attributes.blink == Blink::Slow,
        MatcherTextAttribute::FastBlink => style.attributes.blink == Blink::Fast,
        MatcherTextAttribute::CrossedOut => style.attributes.crossed_out,
        MatcherTextAttribute::Reverse => style.attributes.reverse,
    })
}

fn preview_color_matches(
    actual: smudgy_core::session::styled_line::Color,
    matcher: MatcherColor,
) -> bool {
    use smudgy_core::session::styled_line::Color;
    if let MatcherColor::Truecolor {
        range: Some(range), ..
    } = matcher
    {
        let Color::Rgb { r, g, b } = actual else {
            return false;
        };
        let range = range.rgb_canonicalized();
        let hsv = MatcherHsv::from_rgb(r, g, b);
        let (saturation_min, saturation_max) = range.saturation_bounds();
        let (value_min, value_max) = range.value_bounds();
        return (hsv.saturation == 0 || range.hue_matches(hsv.hue))
            && (saturation_min..=saturation_max).contains(&hsv.saturation)
            && (value_min..=value_max).contains(&hsv.value);
    }
    actual == matcher_vt_color(matcher)
}

/// The raw kind's teaching hint, shown only while its field is blank.
fn raw_hint<'a>() -> Elem<'a> {
    text(crate::i18n::ts!("editor-raw-hint"))
        .size(12.0)
        .style(common::muted)
        .into()
}

/// A status dot with its deck tooltip: matches / does not match / blocks.
fn dot_with_tooltip<'a>(status: NodeStatus, role: PatternKind) -> Elem<'a> {
    let dot = container(common::status_dot(status)).padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 4.0,
        right: 4.0,
    });
    let label = match status {
        NodeStatus::Ok => crate::i18n::t!("editor-dot-matches"),
        NodeStatus::Error if role == PatternKind::Anti => crate::i18n::t!("editor-dot-blocks"),
        NodeStatus::Disabled => crate::i18n::t!("editor-dot-no-match"),
        // A compile error already reads inline; the dot stays bare.
        NodeStatus::Neutral | NodeStatus::Error | NodeStatus::Warning => return dot.into(),
    };
    tip(dot.into(), label)
}

/// A small quiet icon button with a tooltip; renders disabled with no
/// `on_press` (the ends of a role group's reorder range).
fn icon_button<'a>(icon: &'a str, tooltip_label: String, on_press: Option<Message>) -> Elem<'a> {
    let mut control = button(text(icon).font(fonts::BOOTSTRAP_ICONS).size(13.0))
        .style(button_style::toolbar)
        .padding(6);
    if let Some(message) = on_press {
        control = control.on_press(message);
    }
    tip(control.into(), tooltip_label)
}

/// A labeled group header: the colored title plus its precedence note.
fn group_header<'a>(
    title: &'a str,
    note: &'a str,
    title_style: impl Fn(&Theme) -> iced::widget::text::Style + 'a,
) -> Elem<'a> {
    row![
        text(title)
            .size(12.0)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..fonts::GEIST_VF
            })
            .style(title_style),
        text(note).size(12.0).style(common::muted),
    ]
    .spacing(10.0)
    .align_y(Vertical::Center)
    .into()
}

// ---- kind cards -------------------------------------------------------------

/// One selectable kind card's content (visual-contract §1–2).
struct KindCard<'a> {
    title: &'a str,
    example: &'a str,
    badge: Option<&'a str>,
    hue: iced::Color,
    selected: bool,
    message: Message,
}

/// The kind dot on a card: a small filled circle in the kind hue.
fn kind_dot<'a>(hue: iced::Color) -> Elem<'a> {
    container(Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(hue)),
            border: iced::Border::default().rounded(4.0),
            ..Default::default()
        })
        .into()
}

/// A difficulty badge (`Advanced` / `Wizardry`): a small uppercase pill
/// outlined in the kind hue.
fn kind_badge<'a>(label: &str, hue: iced::Color) -> Elem<'a> {
    container(
        text(label.to_uppercase())
            .size(10.0)
            .style(move |_theme: &Theme| iced::widget::text::Style { color: Some(hue) }),
    )
    .padding(Padding {
        top: 1.0,
        bottom: 1.0,
        left: 6.0,
        right: 6.0,
    })
    .style(move |_theme: &Theme| iced::widget::container::Style {
        border: iced::Border {
            color: hue,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// One selectable kind card. Hue is identity, value is state: selected takes
/// the kind hue at full strength (border, dot, example, badge) over a 10%
/// tint; unselected keeps the hue at 50% alpha on a neutral body with a
/// hairline border — never grey. Hover is a faint wash, no hue change.
fn kind_card<'a>(card: KindCard<'a>) -> Elem<'a> {
    let strength = if card.selected {
        card.hue
    } else {
        card.hue.scale_alpha(0.5)
    };
    let mut title_row = row![
        kind_dot(strength),
        text(card.title).size(13.0).font(Font {
            weight: iced::font::Weight::Semibold,
            ..fonts::GEIST_VF
        }),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    if let Some(badge) = card.badge {
        title_row = title_row.push(iced::widget::space::horizontal());
        title_row = title_row.push(kind_badge(badge, strength));
    }
    let inner = column![
        title_row,
        text(card.example)
            .size(12.0)
            .font(fonts::GEIST_MONO_VF)
            .style(move |_theme: &Theme| iced::widget::text::Style {
                color: Some(strength),
            }),
    ]
    .spacing(6.0);
    let hue = card.hue;
    let selected = card.selected;
    button(inner)
        .style(move |theme: &Theme, status| {
            let background = if selected {
                hue.scale_alpha(0.10)
            } else if status == iced::widget::button::Status::Hovered {
                theme.styles.text.normal.scale_alpha(0.04)
            } else {
                iced::Color::TRANSPARENT
            };
            iced::widget::button::Style {
                background: Some(iced::Background::Color(background)),
                border: iced::Border {
                    color: if selected {
                        hue
                    } else {
                        theme.styles.general.border
                    },
                    width: 1.0,
                    radius: 6.0.into(),
                },
                text_color: theme.styles.text.normal,
                ..Default::default()
            }
        })
        .padding(12)
        .width(Length::FillPortion(1))
        .on_press(card.message)
        .into()
}

fn error_bar<'a>(message: &str) -> Elem<'a> {
    container(
        row![
            text(bootstrap_icons::EXCLAMATION_TRIANGLE)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(13.0)
                .style(common::danger),
            text(message.to_string()).size(13.0).style(common::danger),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 12.0,
        right: 12.0,
    })
    .style(|theme: &Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(
            theme.styles.text.error.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme.styles.text.error.scale_alpha(0.4),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A stable editor-tree position for an optional error banner.
///
/// The outer container is always present, even when its zero-height child is
/// empty. This matters for errors derived live from an input: conditionally
/// inserting a column child ahead of that input makes iced reconcile its
/// focus state against the wrong child on the next frame.
fn error_slot<'a>(message: Option<&str>) -> Elem<'a> {
    let content: Elem<'a> = match message {
        Some(message) => error_bar(message),
        None => Space::new().height(0).into(),
    };
    container(content).into()
}

fn verdict_style(status: NodeStatus) -> fn(&Theme) -> iced::widget::text::Style {
    match status {
        NodeStatus::Neutral => common::muted,
        NodeStatus::Ok => common::success,
        NodeStatus::Error => common::danger,
        NodeStatus::Warning => common::warning,
        NodeStatus::Disabled => common::muted,
    }
}

/// The Parsing picker's per-mode strings: `(label, example, what it gets)`.
fn parse_mode_strings(
    mode: smudgy_core::models::matchers::ParseMode,
) -> (&'static str, &'static str, &'static str) {
    use smudgy_core::models::matchers::ParseMode;
    match mode {
        ParseMode::Spaces => (
            crate::i18n::ts!("editor-parse-spaces"),
            crate::i18n::ts!("editor-parse-spaces-example"),
            crate::i18n::ts!("editor-parse-spaces-gets"),
        ),
        ParseMode::Quotes => (
            crate::i18n::ts!("editor-parse-quotes"),
            crate::i18n::ts!("editor-parse-quotes-example"),
            crate::i18n::ts!("editor-parse-quotes-gets"),
        ),
        ParseMode::Braces => (
            crate::i18n::ts!("editor-parse-braces"),
            crate::i18n::ts!("editor-parse-braces-example"),
            crate::i18n::ts!("editor-parse-braces-gets"),
        ),
        ParseMode::All => (
            crate::i18n::ts!("editor-parse-all"),
            crate::i18n::ts!("editor-parse-all-example"),
            crate::i18n::ts!("editor-parse-all-gets"),
        ),
        ParseMode::Raw => (
            crate::i18n::ts!("editor-parse-raw"),
            crate::i18n::ts!("editor-parse-raw-example"),
            crate::i18n::ts!("editor-parse-raw-gets"),
        ),
    }
}

/// A capture badge on the Matched-values rail (visual-contract §5): the
/// lavender-family fill and border that mean "a value the script receives",
/// lifted on hover.
fn capture_badge_style(
    theme: &Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let fill = iced::Color::from_rgb8(0x7C, 0x57, 0xFF);
    let hovered = matches!(
        status,
        iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed
    );
    iced::widget::button::Style {
        background: Some(iced::Background::Color(fill.scale_alpha(if hovered {
            0.3
        } else {
            0.14
        }))),
        border: iced::Border {
            color: iced::Color::from_rgb8(0x4E, 0x37, 0x83),
            width: 1.0,
            radius: 5.0.into(),
        },
        text_color: theme.styles.text.normal,
        ..Default::default()
    }
}

/// Which generated example a pane shows (`matching-logic.md` §8).
#[derive(Clone, Copy)]
enum ExampleKind {
    /// Alias, Command or Regex kind: the `say Hello` example.
    AliasSay,
    /// Alias, Simple-pattern kind: the `emote` example.
    AliasEmote,
    /// Trigger: the `say I heard about` example.
    Trigger,
}

/// One generated action body, in the deck's words: the example line with the
/// first capture reference interpolated (or the no-captures variant), wrapped
/// in ``send(`…`);`` for the script tab.
fn generated_body(kind: ExampleKind, reference: Option<&str>, script: bool) -> String {
    let hole = reference.map(|reference| {
        if script {
            format!("${{{reference}}}")
        } else {
            reference.to_string()
        }
    });
    let line = match (kind, hole) {
        (ExampleKind::AliasSay, Some(hole)) => {
            crate::i18n::t!("editor-gen-alias-hello", "hole" => hole)
        }
        (ExampleKind::AliasSay, None) => crate::i18n::t!("editor-gen-alias-hello-none"),
        (ExampleKind::AliasEmote, Some(hole)) => {
            crate::i18n::t!("editor-gen-alias-emote", "hole" => hole)
        }
        (ExampleKind::AliasEmote, None) => crate::i18n::t!("editor-gen-alias-emote-none"),
        (ExampleKind::Trigger, Some(hole)) => {
            crate::i18n::t!("editor-gen-trigger", "hole" => hole)
        }
        (ExampleKind::Trigger, None) => crate::i18n::t!("editor-gen-trigger-none"),
    };
    if script {
        format!("send(`{line}`);")
    } else {
        line
    }
}

/// Renders capture references in the action language's vocabulary: `$name` /
/// `$N` for a text body, `matches.name` / `matches[N]` for JavaScript.
fn render_references(captures: &[Option<String>], language: ScriptLang) -> Vec<String> {
    captures
        .iter()
        .enumerate()
        .map(|(i, name)| match (name, language) {
            (Some(name), ScriptLang::Plaintext) => format!("${name}"),
            (Some(name), _) => format!("matches.{name}"),
            (None, ScriptLang::Plaintext) => format!("${}", i + 1),
            (None, _) => format!("matches[{}]", i + 1),
        })
        .collect()
}

/// The Try-it verdict for a Command miss, in the deck's words.
fn command_miss_verdict(name: &str, miss: &matchers::CommandMiss) -> (String, NodeStatus) {
    use matchers::{CommandMiss, TokenizeError};
    match miss {
        CommandMiss::Empty => (
            crate::i18n::t!("editor-enter-command"),
            NodeStatus::Disabled,
        ),
        CommandMiss::WrongFirstWord => (
            crate::i18n::t!("editor-wrong-first-word", "name" => name),
            NodeStatus::Disabled,
        ),
        CommandMiss::MissingRequired { name } => (
            crate::i18n::t!("editor-missing-arg", "name" => name.clone()),
            NodeStatus::Error,
        ),
        CommandMiss::Unclaimed { text } => (
            crate::i18n::t!("editor-unclaimed", "text" => text.clone()),
            NodeStatus::Disabled,
        ),
        CommandMiss::Tokenize(TokenizeError::UnterminatedQuote) => (
            crate::i18n::t!("editor-unterminated-quote"),
            NodeStatus::Error,
        ),
        CommandMiss::Tokenize(TokenizeError::UnbalancedBraces) => (
            crate::i18n::t!("editor-unbalanced-braces"),
            NodeStatus::Error,
        ),
    }
}

/// The Try-it field's raw-line simulation: `\e` means the ESC byte, so escape
/// sequences can be typed into the tester (`matching-logic.md` §6).
fn raw_of(test: &str) -> String {
    test.replace("\\e", "\x1b")
}

fn alias_verdict(pattern: &str, sample: &str) -> (String, NodeStatus) {
    if pattern.is_empty() {
        return (
            crate::i18n::t!("editor-verdict-no-regex"),
            NodeStatus::Disabled,
        );
    }
    match regex::Regex::new(pattern) {
        Err(e) => (
            crate::i18n::t!("editor-verdict-invalid-regex", "error" => e.to_string()),
            NodeStatus::Error,
        ),
        Ok(re) => {
            if sample.is_empty() {
                (
                    crate::i18n::t!("editor-enter-command"),
                    NodeStatus::Disabled,
                )
            } else if re.is_match(sample) {
                (crate::i18n::t!("editor-would-fire"), NodeStatus::Ok)
            } else {
                (crate::i18n::t!("editor-no-match"), NodeStatus::Disabled)
            }
        }
    }
}

/// Wraps a pane body in the standard padded, width-capped column.
pub(super) fn pane_scroll<'a>(body: Column<'a, Message, Theme>) -> Elem<'a> {
    pane_scroll_element(body.width(Length::Fill).into())
}

/// The smallest height an alias, trigger, or hotkey action editor (tab strip included) takes
/// before the pane starts scrolling.
const ACTION_EDITOR_MIN_HEIGHT: f32 = 260.0;
/// The smallest height a module's source editor (section label included) takes before the pane
/// starts scrolling.
const MODULE_EDITOR_MIN_HEIGHT: f32 = 400.0;

/// Vertical room the pane wrapper's own padding takes from the scroll viewport.
const PANE_PADDING_HEIGHT: f32 = 26.0 + 32.0;

/// The pane wrapper for already-built content: the same padding as [`pane_scroll`], the full
/// width of the window.
pub(super) fn pane_scroll_element(body: Elem<'_>) -> Elem<'_> {
    container(body)
        .padding(Padding {
            top: 26.0,
            bottom: 32.0,
            left: 30.0,
            right: 30.0,
        })
        .width(Length::Fill)
        .into()
}

/// A pane whose `editor` takes every pixel of `viewport_height` the rest of the pane leaves,
/// down to `min_editor_height`; below that the pane scrolls. `top` is everything above the
/// editor, `bottom` everything below it (a save bar, usually).
pub(super) fn pane_scroll_growing<'a>(
    top: Column<'a, Message, Theme>,
    editor: Elem<'a>,
    bottom: Option<Elem<'a>>,
    min_editor_height: f32,
    viewport_height: f32,
) -> Elem<'a> {
    let mut children: Vec<Elem<'a>> = vec![top.width(Length::Fill).into(), editor];
    children.extend(bottom);
    pane_scroll_element(
        crate::widgets::grow_column::GrowColumn::new(
            children,
            1,
            min_editor_height,
            viewport_height - PANE_PADDING_HEIGHT,
        )
        .spacing(16.0)
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use iced::advanced::Widget;
    use iced::advanced::widget::tree::Tree;
    use iced::keyboard::key::Named;

    use super::*;

    #[test]
    fn folder_subtree_matching_preserves_exact_case_siblings() {
        assert_eq!(
            folder_relative_suffix("Combat/Healing/Fast", "Combat/Healing").as_deref(),
            Some("Fast")
        );
        assert_eq!(folder_relative_suffix("COMBAT", "combat"), None);
        assert_eq!(folder_relative_suffix("combatant", "combat"), None);
    }

    #[test]
    fn action_tabs_support_keyboard_navigation_in_both_directions() {
        let to_script = action_tab_key_action(
            &Key::Named(Named::ArrowRight),
            ScriptLang::Plaintext,
            ScriptLang::TS,
        );
        assert!(matches!(
            to_script,
            KeyAction::Publish(Message::SetBehavior(ScriptLang::TS))
        ));

        let to_text = action_tab_key_action(
            &Key::Named(Named::ArrowLeft),
            ScriptLang::JS,
            ScriptLang::JS,
        );
        assert!(matches!(
            to_text,
            KeyAction::Publish(Message::SetBehavior(ScriptLang::Plaintext))
        ));
    }

    #[test]
    fn activation_controls_publish_bulk_and_profile_actions_from_the_keyboard() {
        let bulk = activation_control_key_action(
            &Key::Named(Named::Space),
            false,
            Message::EnableEverywhere,
        );
        assert!(matches!(
            bulk,
            KeyAction::Publish(Message::EnableEverywhere)
        ));

        let profile = activation_control_key_action(
            &Key::Named(Named::Enter),
            false,
            Message::ToggleActivationProfile("Main".to_string()),
        );
        assert!(matches!(
            profile,
            KeyAction::Publish(Message::ToggleActivationProfile(name)) if name == "Main"
        ));
    }

    #[test]
    fn unsaved_package_drafts_block_only_activation_that_enables_execution() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "activation-draft-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.pane = Pane::OwnedPackage;
        window.dirty = true;
        window.profile_names = vec!["Main".to_string(), "Alt".to_string()];

        assert!(window.activation_enable_block_reason().is_some());
        assert!(window.activation_change_enables_more_profiles(
            &ProfileActivation::None,
            &ProfileActivation::All,
        ));
        assert!(!window.activation_change_enables_more_profiles(
            &ProfileActivation::All,
            &ProfileActivation::None,
        ));
        assert!(!window.activation_change_enables_more_profiles(
            &ProfileActivation::All,
            &ProfileActivation::Selected {
                profiles: ["Main".to_string()].into_iter().collect(),
            },
        ));
    }

    #[test]
    fn color_control_ids_and_palette_labels_are_stable_and_locale_neutral() {
        let first_window = iced::window::Id::unique();
        let second_window = iced::window::Id::unique();
        assert_eq!(
            color_control_id(first_window, 4, "channel"),
            color_control_id(first_window, 4, "channel")
        );
        assert_ne!(
            color_control_id(first_window, 4, "channel"),
            color_control_id(first_window, 5, "channel")
        );
        assert_ne!(
            color_control_id(first_window, 4, "channel"),
            color_control_id(second_window, 4, "channel")
        );
        assert_ne!(
            color_attribute_control_id(first_window, 4, MatcherTextAttribute::Bold),
            color_attribute_control_id(first_window, 4, MatcherTextAttribute::Faint)
        );
        assert_eq!(
            palette_color_identifier(MatcherColor::Ansi { index: 12 }),
            "ANSI 12"
        );
        assert_eq!(
            palette_color_identifier(MatcherColor::Xterm { index: 196 }),
            "xterm 196"
        );
    }

    #[test]
    fn color_only_row_reports_a_try_it_match() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "color-only-status-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.test_input = "\u{1b}[31mred".to_string();
        let row = TriggerRow {
            color: Some(MatcherColorMatch {
                foreground: Some(MatcherColor::Ansi { index: 1 }),
                ..Default::default()
            }),
            ..TriggerRow::new(PatternKind::Match)
        };

        assert_eq!(window.row_status(&row), NodeStatus::Ok);
        window.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: "color only".to_string(),
            node: EditNode::Trigger {
                enabled: true,
                language: ScriptLang::Plaintext,
                prompt: false,
                priority: 0,
                fallthrough: false,
                package: None,
                rows: vec![row],
            },
            error: None,
        });
        assert_eq!(window.trigger_verdict().1, NodeStatus::Ok);
    }

    #[test]
    fn all_any_color_only_row_is_invalid_but_an_attribute_constraint_matches() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "color-only-constraint-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.test_input = "\u{1b}[1mbold".to_string();
        let mut row = TriggerRow {
            color: Some(MatcherColorMatch::default()),
            ..TriggerRow::new(PatternKind::Match)
        };

        assert_eq!(
            color_filter_constraint_error(&row),
            Some(crate::i18n::ts!("editor-color-needs-constraint"))
        );
        assert_eq!(window.row_status(&row), NodeStatus::Error);
        window.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: "color only".to_string(),
            node: EditNode::Trigger {
                enabled: true,
                language: ScriptLang::Plaintext,
                prompt: false,
                priority: 0,
                fallthrough: false,
                package: None,
                rows: vec![row.clone()],
            },
            error: None,
        });
        assert_eq!(window.trigger_verdict().1, NodeStatus::Error);

        row.color
            .as_mut()
            .unwrap()
            .attributes
            .push(MatcherTextAttribute::Bold);
        assert_eq!(color_filter_constraint_error(&row), None);
        assert_eq!(window.row_status(&row), NodeStatus::Ok);
        let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &mut window.pane
        else {
            panic!("test window must contain a trigger editor");
        };
        rows[0] = row;
        assert_eq!(window.trigger_verdict().1, NodeStatus::Ok);
    }

    #[test]
    fn color_only_preview_ignores_zero_width_pre_sgr_style() {
        use smudgy_core::session::connection::vt_processor::AnsiColor;
        use smudgy_core::session::styled_line::{Color, Style, StyledLine, TextAttributes, VtSpan};

        let cyan = Color::Ansi {
            color: AnsiColor::Cyan,
            bold: false,
        };
        let line = StyledLine::new(
            "cyan",
            vec![
                // The cursor inherited dim cyan from the previous line. SGR
                // bold changed the style before this line emitted any text.
                VtSpan {
                    style: Style {
                        fg: cyan,
                        ..Style::DEFAULT
                    },
                    begin_pos: 0,
                    end_pos: 0,
                },
                VtSpan {
                    style: Style {
                        fg: cyan,
                        attributes: TextAttributes {
                            bold: true,
                            ..TextAttributes::DEFAULT
                        },
                        ..Style::DEFAULT
                    },
                    begin_pos: 0,
                    end_pos: 4,
                },
            ],
        );
        let empty = regex::Regex::new("").unwrap();
        let dim = MatcherColorMatch {
            foreground: Some(MatcherColor::Ansi { index: 6 }),
            ..Default::default()
        };
        let bright = MatcherColorMatch {
            foreground: Some(MatcherColor::Ansi { index: 14 }),
            ..Default::default()
        };

        assert!(!preview_has_color_matched_start(
            &empty, "cyan", &line, &dim
        ));
        assert!(preview_has_color_matched_start(
            &empty, "cyan", &line, &bright
        ));
    }

    #[test]
    fn live_error_slot_keeps_following_input_focus_state() {
        let value = String::new();
        let valid = iced::widget::column![
            error_slot(None),
            text_input("pattern", &value).on_input(Message::SetName)
        ];
        let mut tree = Tree::new(&valid as &dyn Widget<Message, Theme, iced::Renderer>);

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let input_state = tree.children[1]
            .state
            .downcast_mut::<iced::widget::text_input::State<Paragraph>>();
        input_state.focus();

        let invalid = iced::widget::column![
            error_slot(Some("invalid regular expression")),
            text_input("pattern", &value).on_input(Message::SetName)
        ];
        tree.diff(&invalid as &dyn Widget<Message, Theme, iced::Renderer>);

        let input_state = tree.children[1]
            .state
            .downcast_ref::<iced::widget::text_input::State<Paragraph>>();
        assert!(input_state.is_focused());
    }

    #[test]
    fn inline_color_error_slot_keeps_following_input_focus_state() {
        let value = String::new();
        let valid = iced::widget::column![
            inline_error_slot(None),
            text_input("#rrggbb", &value).on_input(Message::SetName)
        ];
        let mut tree = Tree::new(&valid as &dyn Widget<Message, Theme, iced::Renderer>);

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let input_state = tree.children[1]
            .state
            .downcast_mut::<iced::widget::text_input::State<Paragraph>>();
        input_state.focus();

        let invalid = iced::widget::column![
            inline_error_slot(Some("Enter six hexadecimal digits.")),
            text_input("#rrggbb", &value).on_input(Message::SetName)
        ];
        tree.diff(&invalid as &dyn Widget<Message, Theme, iced::Renderer>);

        let input_state = tree.children[1]
            .state
            .downcast_ref::<iced::widget::text_input::State<Paragraph>>();
        assert!(input_state.is_focused());
    }

    /// The fixtures §10 gutter-derivation table for regex sources.
    #[test]
    fn regex_gutters_derive_from_the_source_anchors() {
        assert_eq!(regex_loose_sides("^greet$"), (false, false));
        assert_eq!(regex_loose_sides("greet$"), (true, false));
        assert_eq!(regex_loose_sides("^greet"), (false, true));
        assert_eq!(regex_loose_sides("greet"), (true, true));
        // An escaped `$` is not an anchor.
        assert_eq!(regex_loose_sides(r"costs 5\$"), (true, true));
        assert_eq!(regex_loose_sides(""), (false, false));
        assert_eq!(regex_loose_sides("$"), (true, false));
    }

    /// The field composite's widget tree must be identical in every anchor
    /// state. iced diffs children positionally, so a gutter cell mounting or
    /// unmounting would shift the editor's tree position and reset its state
    /// — focus included — on the very keystroke that flips the derivation
    /// (the first unanchored character, adding or removing `^`/`$`), leaving
    /// the field unfocused and swallowing everything typed after it.
    #[test]
    fn matcher_field_tree_is_stable_across_anchor_states() {
        fn topology(tree: &Tree, depth: usize, out: &mut Vec<String>) {
            out.push(format!("{depth}:{:?}", tree.tag));
            for child in &tree.children {
                topology(child, depth + 1, out);
            }
        }

        let content = iced::widget::text_editor::Content::with_text("You are (hungry");
        let mut shapes: Vec<Vec<String>> = Vec::new();
        for loose in [(false, false), (true, false), (false, true), (true, true)] {
            let field = matcher_field(
                &content,
                "placeholder",
                highlight::FieldSyntax::Regex,
                loose,
                false,
                Message::AliasRegexAction,
            );
            // The composite must report a Shrink height in every state: the
            // gutters are Fill-height internally, and if that fluidity leaks
            // into the composite's own size hint (`Row::push` encloses child
            // hints; `Container::new` derives from its content's), the field
            // measures against nothing in rows with no fixed-size sibling and
            // the whole matcher section collapses.
            assert_eq!(
                field.as_widget().size(),
                iced::Size::new(Length::Fill, Length::Shrink),
                "the field composite must not inherit the gutters' Fill height"
            );
            let tree = Tree::new(field.as_widget());
            let mut tags = Vec::new();
            topology(&tree, 0, &mut tags);
            shapes.push(tags);
        }
        assert_eq!(
            shapes[0], shapes[1],
            "gutter states must not change the composite's tree shape"
        );
        assert_eq!(shapes[1], shapes[2]);
        assert_eq!(shapes[2], shapes[3]);
    }

    /// Enter never reaches a one-line buffer, and pasted newlines flatten.
    #[test]
    fn single_line_fields_stay_single_line() {
        use iced::widget::text_editor::{Action, Content, Edit};

        let mut content = Content::new();
        perform_single_line(&mut content, Action::Edit(Edit::Insert('a')));
        perform_single_line(&mut content, Action::Edit(Edit::Enter));
        perform_single_line(
            &mut content,
            Action::Edit(Edit::Paste(Arc::new("b\r\nc\nd".to_string()))),
        );
        assert_eq!(single_line_text(&content), "ab c d");
    }
}

/// The inline script an editor body persists as.
///
/// A body holding only line breaks is no script at all: persisting it would
/// replace a file-backed body (`script: None`) with an empty inline one that
/// shadows the file. Real bodies keep their trailing newline so the saved
/// text matches the editor buffer exactly.
fn persisted_script(body: String) -> Option<String> {
    (!body.trim_end_matches('\n').is_empty()).then_some(body)
}

#[cfg(test)]
mod persisted_script_tests {
    use super::persisted_script;

    #[test]
    fn line_break_only_bodies_persist_as_no_script() {
        assert_eq!(persisted_script(String::new()), None);
        assert_eq!(persisted_script("\n".to_owned()), None);
        assert_eq!(persisted_script("\n\n\n".to_owned()), None);
    }

    #[test]
    fn real_bodies_keep_their_exact_text() {
        assert_eq!(
            persisted_script("echo(1);\n".to_owned()).as_deref(),
            Some("echo(1);\n")
        );
        assert_eq!(
            persisted_script("\necho(1);".to_owned()).as_deref(),
            Some("\necho(1);")
        );
    }
}
