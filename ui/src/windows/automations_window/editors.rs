//! The script editors (alias / trigger / hotkey), the folder editor, and the
//! module pane — both the update-side logic and the views.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::alignment::Vertical;
use iced::widget::{
    Column, button, column, container, pick_list, radio, row, text, text_editor, text_input,
};
use iced::{Element, Font, Length, Padding};

use smudgy_core::models::server;
use smudgy_core::models::{ScriptLang, aliases, hotkeys, naming, packages, triggers};

use crate::assets::{bootstrap_icons, fonts};
use crate::keymap::{self as hotkey_helpers, MaybePhysicalKey};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::update::Update;
use crate::widgets::hotkey_input::HotkeyInput;

use super::common;
use super::model::{
    NodeStatus, PatternKind, Script, ScriptKey, rows_into_trigger, trigger_rows,
    upsert_script_folder,
};
use super::{
    AutomationsWindow, EditNode, EditorMode, EditorState, Elem, Event, FolderState, Message,
    ModuleMode, ModuleState, Pane, Selection,
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
            FolderChoice::TopLevel => f.write_str("(top level)"),
            FolderChoice::Folder(path) => f.write_str(path),
        }
    }
}

/// Logs `msg` and returns an empty update (used for non-fatal save failures).
fn warn_none(msg: String) -> Update<Message, Event> {
    log::warn!("{msg}");
    Update::none()
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

        let body = match &script {
            Script::Alias(a) => a.script.clone().unwrap_or_default(),
            Script::Hotkey(h) => h.script.clone().unwrap_or_default(),
            Script::Trigger(t) => t.script.clone().unwrap_or_default(),
            Script::Folder(_, _) => return Update::none(),
        };
        self.editor_content = text_editor::Content::with_text(&body);

        let node = match script {
            Script::Alias(a) => EditNode::Alias(a),
            Script::Hotkey(h) => {
                self.hotkey_state = hotkey_definition_to_keys(&h);
                EditNode::Hotkey(h)
            }
            Script::Trigger(t) => EditNode::Trigger {
                enabled: t.enabled,
                language: t.language,
                prompt: t.prompt,
                package: t.package.clone(),
                rows: trigger_rows(&t),
            },
            Script::Folder(_, _) => return Update::none(),
        };
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Edit,
            original_name: Some(key.script_name.clone()),
            name: key.script_name,
            node,
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_alias(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.editor_content = text_editor::Content::new();
        self.test_input.clear();
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: String::new(),
            node: EditNode::Alias(aliases::AliasDefinition {
                pattern: String::new(),
                script: None,
                package: self.current_folder(),
                enabled: true,
                language: ScriptLang::Plaintext,
            }),
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_trigger(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.editor_content = text_editor::Content::new();
        self.test_input.clear();
        self.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: String::new(),
            node: EditNode::Trigger {
                enabled: true,
                language: ScriptLang::Plaintext,
                prompt: false,
                package: self.current_folder(),
                rows: vec![(PatternKind::Match, String::new())],
            },
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_hotkey(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.editor_content = text_editor::Content::new();
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
            enabled: true,
            error: None,
        });
        Update::none()
    }

    pub(super) fn new_module(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.editor_content = text_editor::Content::with_text(
            "// A local module: shared helpers, private to this profile.\n",
        );
        self.pane = Pane::Module(ModuleState {
            mode: ModuleMode::Create,
            subpath: String::new(),
            path: None,
            name: String::new(),
            error: None,
        });
        Update::none()
    }

    pub(super) fn open_folder(&mut self, path: String) -> Update<Message, Event> {
        self.clear_selection();
        let enabled = packages::folder_enabled(&self.packages, &path);
        self.selection = Selection::Folder(path.clone());
        self.pane = Pane::Folder(FolderState {
            mode: EditorMode::Edit,
            original_path: Some(path.clone()),
            path,
            enabled,
            error: None,
        });
        Update::none()
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
                    self.editor_content = text_editor::Content::with_text(&content);
                    self.pane = Pane::Module(ModuleState {
                        mode: ModuleMode::View,
                        subpath,
                        path: Some(path),
                        name: String::new(),
                        error: None,
                    });
                }
                Err(e) => {
                    self.pane =
                        Pane::Error(Arc::new(vec![format!("Failed to read '{subpath}': {e}")]));
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
        match &mut self.pane {
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
                // Enable is a persisted property — save immediately so the change
                // is live, without requiring a separate Save.
                self.dirty = true;
                let _ = now;
                self.save_open()
            }
            Pane::Folder(_) => self.toggle_folder_enabled(),
            _ => Update::none(),
        }
    }

    /// Move the open script into `folder` (`None` = top level). In edit mode this
    /// re-homes and persists immediately — like the enable switch (`save_open`
    /// rewrites the `package` field via the same path a rename uses). In create
    /// mode it only records the choice; it's applied when the user clicks Create.
    /// The palette's "Move to…" group routes here too: the selected script is the
    /// one open in the editor, so this single handler drives both surfaces.
    pub(super) fn set_script_folder(&mut self, folder: Option<String>) -> Update<Message, Event> {
        // Normalize an empty path to top level so a stray "" never becomes a folder.
        let folder = folder.filter(|p| !p.is_empty());
        let is_edit = match &mut self.pane {
            Pane::Editor(state) => {
                match &mut state.node {
                    EditNode::Alias(a) => a.package = folder,
                    EditNode::Hotkey(h) => h.package = folder,
                    EditNode::Trigger { package, .. } => *package = folder,
                }
                state.mode == EditorMode::Edit
            }
            _ => return Update::none(),
        };
        if is_edit {
            self.dirty = true;
            self.save_open()
        } else {
            Update::none()
        }
    }

    fn toggle_folder_enabled(&mut self) -> Update<Message, Event> {
        let Pane::Folder(state) = &mut self.pane else {
            return Update::none();
        };
        let Some(path) = state.original_path.clone() else {
            return Update::none();
        };
        let next = !state.enabled;
        state.enabled = next;
        packages::set_folder_enabled(&mut self.packages, &path, next);
        if let Err(e) = packages::save_packages(&self.server_name, &self.packages) {
            return warn_none(format!("Failed to save folders: {e}"));
        }
        Update::with_event(Event::ScriptsChanged {
            server_name: self.server_name.clone(),
        })
    }

    pub(super) fn save_open(&mut self) -> Update<Message, Event> {
        let Pane::Editor(state) = &mut self.pane else {
            return Update::none();
        };
        state.error = None;
        let name = state.name.trim().to_string();
        if name.is_empty() {
            state.error = Some("Name cannot be empty".to_string());
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
                state.error = Some("Name already in use".to_string());
            }
            return Update::none();
        }

        let body = self.editor_content.text();
        let body = body.trim_end_matches('\n').to_string();
        let final_script = match &self.pane {
            Pane::Editor(EditorState { node, .. }) => match node {
                EditNode::Alias(a) => Script::Alias(aliases::AliasDefinition {
                    script: (!body.is_empty()).then_some(body),
                    ..a.clone()
                }),
                EditNode::Hotkey(h) => {
                    let mut h = h.clone();
                    if !self.hotkey_state.is_empty() {
                        hotkey_helpers::set_key_and_modifiers_from_maybe_physical(
                            &mut h,
                            self.hotkey_state.clone(),
                        );
                    }
                    Script::Hotkey(hotkeys::HotkeyDefinition {
                        script: (!body.is_empty()).then_some(body),
                        ..h
                    })
                }
                EditNode::Trigger {
                    enabled,
                    language,
                    prompt,
                    package,
                    rows,
                } => {
                    let mut t = triggers::TriggerDefinition {
                        patterns: None,
                        raw_patterns: None,
                        anti_patterns: None,
                        script: (!body.is_empty()).then_some(body),
                        package: package.clone(),
                        language: *language,
                        enabled: *enabled,
                        prompt: *prompt,
                    };
                    rows_into_trigger(rows, &mut t);
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
                if let Pane::Editor(state) = &mut self.pane {
                    state.error = Some(e);
                }
                return Update::none();
            }
        }
        if let Err(e) = self.serialize_scripts() {
            if let Pane::Editor(state) = &mut self.pane {
                state.error = Some(format!("Failed to save: {e}"));
            }
            return Update::none();
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
        self.pending_nav = None;
        let toast = self.show_toast(format!("Saved {name}."));
        Update::new(
            toast,
            Some(Event::ScriptsChanged {
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
        self.remove_script_by_name(&original);
        if let Err(e) = self.serialize_scripts() {
            self.pane = Pane::Error(Arc::new(vec![format!("Failed to save after delete: {e}")]));
            return Update::none();
        }
        self.dirty = false;
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let toast = self.show_toast(format!("Deleted {original}."));
        Update::new(
            toast,
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- folder save / delete ---------------------------------------------

    pub(super) fn save_folder(&mut self) -> Update<Message, Event> {
        let (mode, original_path, path, enabled) = match &self.pane {
            Pane::Folder(state) => (
                state.mode,
                state.original_path.clone(),
                state.path.trim_matches('/').to_string(),
                state.enabled,
            ),
            _ => return Update::none(),
        };
        if let Err(message) = naming::validate_folder_path(&path) {
            if let Pane::Folder(state) = &mut self.pane {
                state.error = Some(message);
            }
            return Update::none();
        }
        match mode {
            EditorMode::Create => {
                packages::insert_folder(&mut self.packages, &path);
                if let Err(e) = packages::save_packages(&self.server_name, &self.packages) {
                    if let Pane::Folder(state) = &mut self.pane {
                        state.error = Some(format!("Failed to save folders: {e}"));
                    }
                    return Update::none();
                }
                self.merge_folders();
                self.selection = Selection::Folder(path.clone());
                self.pane = Pane::Folder(FolderState {
                    mode: EditorMode::Edit,
                    original_path: Some(path.clone()),
                    path,
                    enabled,
                    error: None,
                });
                Update::with_task(self.show_toast("Folder created."))
            }
            EditorMode::Edit => {
                let Some(old_path) = original_path else {
                    return Update::none();
                };
                if old_path == path {
                    return Update::none();
                }
                packages::rename_folder(&mut self.packages, &old_path, &path);
                self.rename_script_packages(&old_path, &path);
                if let Err(e) = packages::save_packages(&self.server_name, &self.packages) {
                    return warn_none(format!("Failed to save folders: {e}"));
                }
                if let Err(e) = self.serialize_scripts() {
                    return warn_none(format!("Failed to save scripts: {e}"));
                }
                self.selection = Selection::Folder(path.clone());
                self.pane = Pane::Folder(FolderState {
                    mode: EditorMode::Edit,
                    original_path: Some(path.clone()),
                    path,
                    enabled,
                    error: None,
                });
                Update::new(
                    Task_batch_reload(self),
                    Some(Event::ScriptsChanged {
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
        packages::remove_folder(&mut self.packages, &path);
        if delete_scripts {
            for name in self.scripts_under(&path) {
                self.remove_script_by_name(&name);
            }
        } else {
            let parent = packages::parent_path(&path);
            self.reparent_scripts(&path, parent);
        }
        self.confirm_folder_delete = false;
        if let Err(e) = packages::save_packages(&self.server_name, &self.packages) {
            return warn_none(format!("Failed to save folders: {e}"));
        }
        if let Err(e) = self.serialize_scripts() {
            return warn_none(format!("Failed to save scripts: {e}"));
        }
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        Update::new(
            Task_batch_reload(self),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- module save / create ---------------------------------------------

    pub(super) fn save_module(&mut self) -> Update<Message, Event> {
        let path = match &self.pane {
            Pane::Module(ModuleState {
                path: Some(path), ..
            }) => path.clone(),
            _ => return Update::none(),
        };
        if let Err(e) = std::fs::write(&path, self.editor_content.text()) {
            return warn_none(format!("Failed to save module: {e}"));
        }
        self.dirty = false;
        self.pending_nav = None;
        let toast = self.show_toast("Saved module.");
        Update::new(
            toast,
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn create_module(&mut self) -> Update<Message, Event> {
        let name = match &self.pane {
            Pane::Module(state) => state.name.trim().to_string(),
            _ => return Update::none(),
        };
        if let Err(message) = naming::validate_module_subpath(&name) {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(message);
            }
            return Update::none();
        }
        let dir = match server::load_server(&self.server_name) {
            Ok(server) => server.path.join("modules"),
            Err(e) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.error = Some(format!("Failed to resolve modules dir: {e}"));
                }
                return Update::none();
            }
        };
        let target = dir.join(&name);
        if let Some(parent) = target.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(format!("Failed to create module: {e}"));
            }
            return Update::none();
        }
        if let Err(e) = std::fs::write(&target, self.editor_content.text()) {
            if let Pane::Module(state) = &mut self.pane {
                state.error = Some(format!("Failed to create module: {e}"));
            }
            return Update::none();
        }
        self.dirty = false;
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let toast = self.show_toast(format!("Created module {name}."));
        Update::new(
            Task_batch_module_reload(toast),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- tree mutation helpers (folder rename/delete) ---------------------

    fn scripts_under(&self, folder: &str) -> Vec<String> {
        let folder_slash = format!("{folder}/");
        let mut names = Vec::new();
        collect_scripts_under(&self.scripts, folder, &folder_slash, &mut names);
        names
    }

    fn rename_script_packages(&mut self, old: &str, new: &str) {
        let old_slash = format!("{old}/");
        for_each_script_mut(&mut self.scripts, &mut |script| {
            if let Some(pkg) = script_package_field(script) {
                let updated = pkg.as_deref().and_then(|p| {
                    if p == old {
                        Some(new.to_owned())
                    } else {
                        p.strip_prefix(&old_slash)
                            .map(|suffix| format!("{new}/{suffix}"))
                    }
                });
                if let Some(updated) = updated {
                    *pkg = Some(updated);
                }
            }
        });
    }

    fn reparent_scripts(&mut self, folder: &str, target: Option<String>) {
        let folder_slash = format!("{folder}/");
        for_each_script_mut(&mut self.scripts, &mut |script| {
            if let Some(pkg) = script_package_field(script) {
                let under = pkg
                    .as_deref()
                    .is_some_and(|p| p == folder || p.starts_with(&folder_slash));
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

fn collect_scripts_under(
    scripts: &BTreeMap<String, Script>,
    folder: &str,
    folder_slash: &str,
    out: &mut Vec<String>,
) {
    for (name, script) in scripts {
        if let Script::Folder(_, children) = script {
            collect_scripts_under(children, folder, folder_slash, out);
        } else {
            let pkg = script.folder_name();
            if pkg == Some(folder) || pkg.is_some_and(|p| p.starts_with(folder_slash)) {
                out.push(name.clone());
            }
        }
    }
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

    /// The sticky save bar shown for dirty editors / create panes.
    pub(super) fn save_bar<'a>(
        &self,
        create: bool,
        can_delete: bool,
        save_label: &str,
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
            bar = bar.push(
                button(text("Delete").size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Delete),
            );
        }
        if self.dirty {
            bar = bar.push(text("\u{25CF}").size(9.0).style(common::accent));
            bar = bar.push(text("Unsaved changes").size(13.0).style(common::muted));
            bar = bar.push(iced::widget::space::horizontal());
            bar = bar.push(
                button(text("Discard").size(13.0))
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

    fn behavior_radios<'a>(&self, current: ScriptLang) -> Elem<'a> {
        row![
            radio(
                "Send as text",
                ScriptLang::Plaintext,
                Some(current),
                Message::SetBehavior
            ),
            radio(
                "JavaScript",
                ScriptLang::JS,
                Some(current),
                Message::SetBehavior
            ),
        ]
        .spacing(24.0)
        .align_y(Vertical::Center)
        .into()
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

    /// The syntax-highlighted code body editor.
    fn code_editor<'a>(&'a self, language: ScriptLang) -> Elem<'a> {
        let token = match language {
            ScriptLang::JS => "js",
            ScriptLang::TS => "ts",
            ScriptLang::Plaintext => "txt",
        }
        .to_string();
        let editor = text_editor(&self.editor_content)
            .highlight_with::<iced::highlighter::Highlighter>(
                iced::highlighter::Settings {
                    theme: iced::highlighter::Theme::SolarizedDark,
                    token,
                },
                |h: &iced::highlighter::Highlight, _| h.to_format(),
            )
            .font(fonts::GEIST_MONO_VF)
            .on_action(Message::ScriptEditorAction)
            .height(Length::Fixed(220.0));
        column![
            common::section_label("Script"),
            container(editor).style(common::code_surface_style),
        ]
        .spacing(6.0)
        .into()
    }

    fn field_label<'a>(label: &str) -> Elem<'a> {
        container(text(label.to_string()).size(13.0).style(common::muted))
            .width(Length::Fixed(LABEL_WIDTH))
            .align_y(Vertical::Center)
            .height(Length::Fixed(34.0))
            .into()
    }

    pub(super) fn view_editor<'a>(&'a self, state: &'a EditorState) -> Elem<'a> {
        match &state.node {
            EditNode::Alias(alias) => self.view_alias_editor(state, alias),
            EditNode::Hotkey(hotkey) => self.view_hotkey_editor(state, hotkey),
            EditNode::Trigger {
                enabled,
                language,
                rows,
                ..
            } => self.view_trigger_editor(state, *enabled, *language, rows),
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
            text("Folder").size(13.0).style(common::muted),
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
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let badge_label = if alias.language == ScriptLang::JS {
            "JavaScript"
        } else {
            "Text"
        };
        let title = if create {
            "New alias"
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(create, "Alias", alias.package.as_deref());
        let status = Self::editor_status(create, alias.enabled, false);

        let mut body = column![self.scene_header_with_aside(
            Some(status),
            title,
            Some(subtitle),
            Some(self.header_actions(badge_label, alias.enabled)),
            self.folder_aside(alias.package.as_deref()),
        ),]
        .spacing(16.0);

        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }

        body = body.push(field_row(
            "Name",
            text_input("e.g. kill", &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));
        body = body.push(field_row(
            "Pattern",
            text_input("e.g. ^k (.+)$", &alias.pattern)
                .on_input(Message::SetAliasPattern)
                .size(14.0)
                .into(),
        ));
        body = body.push(self.tester_box("Test against a command", &alias.pattern, true));
        body = body.push(field_row("Behavior", self.behavior_radios(alias.language)));
        body = body.push(self.code_editor(alias.language));
        if let Some(bar) = self.save_bar(
            create,
            !create,
            if create { "Create alias" } else { "Save" },
        ) {
            body = body.push(bar);
        }
        pane_scroll(body)
    }

    fn view_hotkey_editor<'a>(
        &'a self,
        state: &'a EditorState,
        hotkey: &'a hotkeys::HotkeyDefinition,
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let badge_label = if hotkey.language == ScriptLang::JS {
            "JavaScript"
        } else {
            "Text"
        };
        let title = if create {
            "New hotkey"
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(create, "Hotkey", hotkey.package.as_deref());
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
            "Name",
            text_input("e.g. north", &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));
        body = body.push(field_row(
            "Shortcut",
            Element::new(
                HotkeyInput::new(&self.hotkey_state, true)
                    .height(Length::Fixed(34.0))
                    .on_action(Message::MarkHotkeyState),
            ),
        ));
        body = body.push(field_row("Behavior", self.behavior_radios(hotkey.language)));
        body = body.push(self.code_editor(hotkey.language));
        if let Some(bar) = self.save_bar(
            create,
            !create,
            if create { "Create hotkey" } else { "Save" },
        ) {
            body = body.push(bar);
        }
        pane_scroll(body)
    }

    fn view_trigger_editor<'a>(
        &'a self,
        state: &'a EditorState,
        enabled: bool,
        language: ScriptLang,
        rows: &'a [(PatternKind, String)],
    ) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let title = if create {
            "New trigger"
        } else {
            state.name.as_str()
        };
        let subtitle = subtitle_for(create, "Trigger", trigger_package(state));
        let any_invalid = rows
            .iter()
            .any(|(_, p)| !p.is_empty() && regex::Regex::new(p).is_err());
        let status = Self::editor_status(create, enabled, any_invalid);

        let mut body = column![self.scene_header_with_aside(
            Some(status),
            title,
            Some(subtitle),
            Some(self.header_actions("JavaScript", enabled)),
            self.folder_aside(trigger_package(state)),
        )]
        .spacing(16.0);

        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        } else if any_invalid {
            body = body.push(error_bar(
                "One or more patterns won't compile — check the highlighted rows.",
            ));
        }

        body = body.push(field_row(
            "Name",
            text_input("e.g. low-health-alert", &state.name)
                .on_input(Message::SetName)
                .size(14.0)
                .into(),
        ));

        // The unified pattern list.
        let mut patterns = Column::new().spacing(6.0);
        for (i, (kind, pattern)) in rows.iter().enumerate() {
            let valid = if pattern.is_empty() {
                NodeStatus::Disabled
            } else if regex::Regex::new(pattern).is_err() {
                NodeStatus::Error
            } else if !self.test_input.is_empty()
                && regex::Regex::new(pattern)
                    .map(|re| re.is_match(&self.test_input))
                    .unwrap_or(false)
            {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            };
            patterns = patterns.push(
                row![
                    pick_list(PatternKind::ALL.to_vec(), Some(*kind), move |k| {
                        Message::SetPatternKind(i, k)
                    }),
                    text_input("\\bpattern\\b", pattern)
                        .on_input(move |v| Message::SetPatternText(i, v))
                        .size(14.0)
                        .width(Length::Fill),
                    container(common::status_dot(valid)).padding(Padding {
                        top: 0.0,
                        bottom: 0.0,
                        left: 4.0,
                        right: 4.0,
                    }),
                    button(
                        text(bootstrap_icons::TRASH_3)
                            .font(fonts::BOOTSTRAP_ICONS)
                            .size(14.0)
                    )
                    .style(button_style::secondary)
                    .on_press(Message::RemovePattern(i))
                    .padding(8),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        }
        patterns = patterns.push(
            button(
                row![
                    text(bootstrap_icons::PLUS_LG)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(12.0),
                    text("Add pattern").size(13.0),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center),
            )
            .style(button_style::secondary)
            .on_press(Message::AddPattern),
        );
        body = body.push(field_row("Patterns", patterns.into()));

        body = body.push(self.tester_box("Test against a line", "", false));
        body = body.push(field_row("Behavior", self.behavior_radios(language)));
        body = body.push(self.code_editor(language));
        if let Some(bar) = self.save_bar(
            create,
            !create,
            if create { "Create trigger" } else { "Save" },
        ) {
            body = body.push(bar);
        }
        pane_scroll(body)
    }

    /// The live tester box. `alias` true → single-pattern verdict (matches what
    /// you type); false → trigger verdict over the open trigger's rows.
    fn tester_box<'a>(&self, label: &str, alias_pattern: &str, alias: bool) -> Elem<'a> {
        let (verdict, status): (String, NodeStatus) = if alias {
            alias_verdict(alias_pattern, &self.test_input)
        } else {
            self.trigger_verdict()
        };
        let placeholder = if alias {
            "k goblin"
        } else {
            "You are badly hurt and bleeding."
        };
        let inner = column![
            row![text(label.to_uppercase()).size(10.0).style(common::faint),],
            text_input(placeholder, &self.test_input)
                .on_input(Message::SetTestInput)
                .size(13.0),
            container(
                row![
                    common::status_dot(status),
                    text(verdict).size(12.0).style(verdict_style(status)),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center)
            ),
        ]
        .spacing(8.0);
        let body = container(inner)
            .padding(12.0)
            .width(Length::Fill)
            .style(common::banner_style);
        field_row("", body.into())
    }

    fn trigger_verdict(&self) -> (String, NodeStatus) {
        let rows = match &self.pane {
            Pane::Editor(EditorState {
                node: EditNode::Trigger { rows, .. },
                ..
            }) => rows,
            _ => return ("No match".to_string(), NodeStatus::Disabled),
        };
        let line = &self.test_input;
        let mut any = false;
        for (kind, pattern) in rows {
            if pattern.is_empty() {
                continue;
            }
            any = true;
            let re = match regex::Regex::new(pattern) {
                Ok(re) => re,
                Err(_) => return ("Invalid pattern".to_string(), NodeStatus::Error),
            };
            let matches = re.is_match(line);
            match kind {
                PatternKind::Match | PatternKind::Raw if !matches => {
                    return ("No match".to_string(), NodeStatus::Disabled);
                }
                PatternKind::Anti if matches => {
                    return ("No match".to_string(), NodeStatus::Disabled);
                }
                _ => {}
            }
        }
        if !any || line.is_empty() {
            return ("Enter a line".to_string(), NodeStatus::Disabled);
        }
        ("Would fire".to_string(), NodeStatus::Ok)
    }

    // ---- folder + module views --------------------------------------------

    pub(super) fn view_folder_editor<'a>(&'a self, state: &'a FolderState) -> Elem<'a> {
        let create = state.mode == EditorMode::Create;
        let count = if let Some(path) = &state.original_path {
            self.folder_child_rows(path).len()
        } else {
            0
        };
        let title = if create {
            "New folder".to_string()
        } else {
            state
                .original_path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("Folder")
                .to_string()
        };
        let subtitle = if create {
            "Folder".to_string()
        } else {
            format!("Folder · {count} items")
        };
        let actions: Option<Elem<'a>> = if create {
            None
        } else {
            Some(common::pill_switch(
                state.enabled,
                false,
                Some(Message::ToggleEnabled),
            ))
        };
        let status = if create || state.enabled {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };

        let mut body =
            column![self.scene_header(Some(status), &title, Some(subtitle), actions)].spacing(16.0);

        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }
        body = body.push(field_row(
            "Path",
            text_input("e.g. combat/healing", &state.path)
                .on_input(Message::SetFolderPath)
                .size(14.0)
                .into(),
        ));
        let hint = if !create && !state.enabled {
            "Disabled — scripts in this folder won't run. Use \"/\" to nest folders."
        } else {
            "Use \"/\" to nest folders. Move a script into a folder from its Folder field in the editor (or the command palette)."
        };
        body = body.push(text(hint).size(12.0).style(common::muted));

        // Contents.
        if let Some(path) = &state.original_path {
            let rows = self.folder_child_rows(path);
            if !rows.is_empty() {
                let mut contents = Column::new()
                    .spacing(4.0)
                    .push(common::section_label("Contents"));
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
                        text("Delete this folder?")
                            .size(13.0)
                            .align_y(Vertical::Center),
                        iced::widget::space::horizontal(),
                        button(text("Move scripts to parent").size(13.0))
                            .style(button_style::secondary)
                            .on_press(Message::ConfirmDeleteFolder(false)),
                        button(text("Delete scripts too").size(13.0))
                            .style(button_style::secondary)
                            .on_press(Message::ConfirmDeleteFolder(true)),
                        button(text("Cancel").size(13.0))
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
                    button(text("Delete").size(13.0))
                        .style(button_style::secondary)
                        .on_press(Message::RequestDeleteFolder),
                );
            }
            bar = bar.push(iced::widget::space::horizontal());
            bar = bar.push(
                button(text("Discard").size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
            );
            bar = bar.push(
                button(text(if create { "Create folder" } else { "Save" }).size(13.0))
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
            match current.get(segment) {
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
                        if packages::is_package_effectively_enabled(&path, &self.packages) {
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

    pub(super) fn view_module<'a>(&'a self, state: &'a ModuleState) -> Elem<'a> {
        let create = state.mode == ModuleMode::Create;
        let title = if create {
            "New module".to_string()
        } else {
            state.subpath.clone()
        };
        let subtitle =
            "Local modules are loaded similar to packages, but run with full privileges and no sandbox".to_string();
        let mut body =
            column![self.scene_header(Some(NodeStatus::Ok), &title, Some(subtitle), None)]
                .spacing(16.0);
        if let Some(error) = &state.error {
            body = body.push(error_bar(error));
        }
        if create {
            body = body.push(field_row(
                "Name",
                text_input("e.g. lib/util.ts", &state.name)
                    .on_input(Message::SetNewModuleName)
                    .size(14.0)
                    .into(),
            ));
        }

        let token = state
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                if create {
                    state.name.clone()
                } else {
                    state.subpath.clone()
                }
            });
        let token = std::path::Path::new(&token)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("ts")
            .to_string();
        let editor = text_editor(&self.editor_content)
            .highlight_with::<iced::highlighter::Highlighter>(
                iced::highlighter::Settings {
                    theme: iced::highlighter::Theme::SolarizedDark,
                    token,
                },
                |h: &iced::highlighter::Highlight, _| h.to_format(),
            )
            .font(fonts::GEIST_MONO_VF)
            .on_action(Message::ScriptEditorAction)
            .height(Length::Fixed(360.0));
        body = body.push(
            column![
                common::section_label("Source"),
                container(editor).style(common::code_surface_style),
            ]
            .spacing(6.0),
        );

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
        bar = bar.push(
            button(text("Discard").size(13.0))
                .style(button_style::secondary)
                .on_press(Message::Discard),
        );
        if create {
            bar = bar.push(
                button(text("Create module").size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::CreateModule),
            );
        } else {
            bar = bar.push(
                button(text("Save").size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::SaveModule),
            );
        }
        body = body.push(bar);
        pane_scroll(body)
    }
}

// ---- view helpers ----------------------------------------------------------

fn subtitle_for(create: bool, kind: &str, package: Option<&str>) -> String {
    if create {
        kind.to_string()
    } else if let Some(folder) = package {
        format!("{kind} · in {folder}")
    } else {
        format!("{kind} · top level")
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

fn verdict_style(status: NodeStatus) -> fn(&Theme) -> iced::widget::text::Style {
    match status {
        NodeStatus::Ok => common::success,
        NodeStatus::Error => common::danger,
        NodeStatus::Warning => common::warning,
        NodeStatus::Disabled => common::muted,
    }
}

fn alias_verdict(pattern: &str, sample: &str) -> (String, NodeStatus) {
    if pattern.is_empty() {
        return ("Enter a pattern".to_string(), NodeStatus::Disabled);
    }
    match regex::Regex::new(pattern) {
        Err(_) => ("Invalid pattern".to_string(), NodeStatus::Error),
        Ok(re) => {
            if sample.is_empty() {
                ("Enter a command".to_string(), NodeStatus::Disabled)
            } else if re.is_match(sample) {
                ("Matches".to_string(), NodeStatus::Ok)
            } else {
                ("No match".to_string(), NodeStatus::Disabled)
            }
        }
    }
}

/// Wraps a pane body in the standard padded, width-capped column.
pub(super) fn pane_scroll<'a>(body: Column<'a, Message, Theme>) -> Elem<'a> {
    container(body.max_width(860.0).width(Length::Fill))
        .padding(Padding {
            top: 26.0,
            bottom: 32.0,
            left: 30.0,
            right: 30.0,
        })
        .width(Length::Fill)
        .into()
}
