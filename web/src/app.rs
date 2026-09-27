use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use futures::{StreamExt, channel::mpsc};
use iced::{Element, Subscription, Task};
use js_sys::{Object, Reflect, Uint8Array};
use smudgy_engine::ConnectionState;
use smudgy_session_model::automation::AutomationPlan;
use smudgy_session_model::input_policy::{CommandSyntax, REDACTION_MASK};
use smudgy_session_model::layout_template::TemplateSource;
use smudgy_session_model::pane::{PaneKey, PanePlacement, SplitDirection, TabPosition};
use smudgy_session_model::send_failure::{
    ConnectionIntent, ReconnectState, SendFailureAction, decide as decide_send_failure,
};
use smudgy_session_model::workspace as dto;
use smudgy_session_model::{
    Blink, Color, SessionId, Style, StyledLine, TextAttributes, Underline, VtSpan,
};
use smudgy_theme::Theme;
use smudgy_ui_shared::layouts_modal::{
    self, Completion, Effect, Event as LayoutEvent, SaveOutcome,
};
use smudgy_ui_shared::layouts_view::Labels;
use smudgy_ui_shared::settings_appearance::Change as AppearanceChange;
use smudgy_web_client::connect_manager::{self, Catalog, ConnectionRequest, SecretString};
use smudgy_web_client::credential_gate::CredentialGate;
use smudgy_web_client::ui::{self, ClientUiState, Message as UiMessage};
use smudgy_web_client::wire::SessionUpdate;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::MessageEvent;

mod layouts;
use crate::storage::{BrowserSettings, LoadedWindow};
use layouts::WebLabels;

#[wasm_bindgen(module = "/runtime-client.js")]
extern "C" {
    type SessionProxy;

    #[wasm_bindgen(catch, js_name = createSessionWorker)]
    fn create_session_worker(on_message: &js_sys::Function) -> Result<SessionProxy, JsValue>;

    #[wasm_bindgen(method, catch, js_name = postMessage)]
    fn post_message(this: &SessionProxy, message: &Object) -> Result<(), JsValue>;

    #[wasm_bindgen(method)]
    fn terminate(this: &SessionProxy);
}

const TERMINAL_COLUMNS: u16 = 100;
const TERMINAL_ROWS: u16 = 36;

fn scrollback_js_value(lines: usize) -> Result<JsValue, String> {
    let lines = u32::try_from(lines).map_err(|_| "scrollback limit exceeds u32".to_owned())?;
    Ok(JsValue::from_f64(f64::from(lines)))
}

fn apply_settings(settings: &BrowserSettings) {
    let mut prefs = (*smudgy_ui_shared::prefs::current()).clone();
    let appearance = settings.appearance;
    let palette = smudgy_ui_shared::prefs::palette_by_name(&settings.theme);
    prefs.font_size = appearance.font_size;
    prefs.line_height = (appearance.font_size * 1.25).round();
    prefs.bold_mode = appearance.bold_mode;
    prefs.disable_blink = appearance.disable_blink;
    prefs.line_length = appearance.line_length;
    prefs.link_tooltip_delay_ms = appearance.link_tooltip_delay_ms;
    prefs.theme_extended_colors = appearance.theme_extended_colors;
    prefs.palette = Arc::new(palette.render.clone());
    prefs.hide_pane_headers = appearance.hide_pane_headers;
    prefs.command_input_behavior = settings.input.command_input_behavior;
    prefs.mask_input_on_server_echo = settings.input.mask_input_on_server_echo;
    prefs.history_case_sensitive_match = settings.input.history_case_sensitive_match;
    prefs.max_history = settings.input.max_history;
    prefs.generation = prefs.generation.wrapping_add(1);
    smudgy_ui_shared::prefs::set_current(prefs);
}

struct StartupSend {
    text: String,
    redactions: Vec<String>,
}

impl StartupSend {
    fn plain(text: String) -> Self {
        Self {
            text,
            redactions: Vec::new(),
        }
    }

    fn write_to_worker(&self, command: &Object) -> Result<(), String> {
        set(command, "send_on_connect", &JsValue::from_str(&self.text))?;
        set(
            command,
            "send_on_connect_redactions",
            &JsValue::from_str(
                &serde_json::to_string(&self.redactions)
                    .map_err(|error| format!("could not serialize startup redactions: {error}"))?,
            ),
        )
    }
}

fn expand_password(template: &str, password: Option<SecretString>) -> Result<StartupSend, String> {
    if !template.contains("$PASSWORD") {
        return Ok(StartupSend::plain(template.to_owned()));
    }
    let password = password.ok_or_else(|| {
        "This profile uses $PASSWORD but has no saved password in this browser".to_owned()
    })?;
    Ok(StartupSend {
        text: template.replace("$PASSWORD", password.as_str()),
        redactions: vec![password.as_str().to_owned()],
    })
}

thread_local! {
    static EVENT_SENDER: RefCell<Option<mpsc::UnboundedSender<WorkerEvent>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone)]
enum WorkerEvent {
    State(SessionId, SessionUpdate),
    Pane(SessionId, ui::BrowserPaneEvent),
    Failed(SessionId, String),
    RuntimeFailed(SessionId, String),
    RuntimeUnavailable(SessionId),
    GridChanged(SessionId, u16, u16),
}

struct SessionWorker {
    worker: SessionProxy,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

#[derive(Clone, Copy)]
struct SessionStart<'a> {
    id: SessionId,
    endpoint: &'a str,
    encoding: &'a str,
    startup: &'a StartupSend,
    automation: &'a AutomationPlan,
    packages: &'a [String],
    scrollback_lines: usize,
    command_syntax: &'a CommandSyntax,
}

impl SessionWorker {
    fn start(options: SessionStart<'_>) -> Result<Self, String> {
        let SessionStart {
            id,
            endpoint,
            encoding,
            startup,
            automation,
            packages,
            scrollback_lines,
            command_syntax,
        } = options;
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = event.data();
            if data.is_instance_of::<js_sys::ArrayBuffer>() {
                match smudgy_web_client::wire::decode(&Uint8Array::new(&data).to_vec()) {
                    Ok(update) => publish(WorkerEvent::State(id, update)),
                    Err(error) => publish(WorkerEvent::Failed(id, error)),
                }
                return;
            }
            match get_string(&data, "kind").as_deref() {
                Some("pane") => match browser_pane_event(&data) {
                    Ok(event) => publish(WorkerEvent::Pane(id, event)),
                    Err(error) => publish(WorkerEvent::Failed(id, error)),
                },
                Some("fatal") => publish(WorkerEvent::Failed(
                    id,
                    get_string(&data, "message")
                        .unwrap_or_else(|| "session worker failed".to_owned()),
                )),
                Some("runtime-fatal") => publish(WorkerEvent::RuntimeFailed(
                    id,
                    get_string(&data, "message")
                        .unwrap_or_else(|| "script runtime is unavailable".to_owned()),
                )),
                Some("runtime-unavailable") => publish(WorkerEvent::RuntimeUnavailable(id)),
                _ => publish(WorkerEvent::Failed(
                    id,
                    "session worker sent an unknown message".to_owned(),
                )),
            }
        });
        let worker = create_session_worker(on_message.as_ref().unchecked_ref())
            .map_err(|error| format!("could not start script runtime: {error:?}"))?;

        let session = Self {
            worker,
            _on_message: on_message,
        };
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("start"))?;
        set(&command, "endpoint", &JsValue::from_str(endpoint))?;
        set(&command, "encoding", &JsValue::from_str(encoding))?;
        startup.write_to_worker(&command)?;
        set(
            &command,
            "automation",
            &JsValue::from_str(
                &serde_json::to_string(automation)
                    .map_err(|error| format!("could not serialize automation: {error}"))?,
            ),
        )?;
        set(
            &command,
            "packages",
            &JsValue::from_str(
                &serde_json::to_string(packages)
                    .map_err(|error| format!("could not serialize package list: {error}"))?,
            ),
        )?;
        set(
            &command,
            "columns",
            &JsValue::from_f64(f64::from(TERMINAL_COLUMNS)),
        )?;
        set(
            &command,
            "rows",
            &JsValue::from_f64(f64::from(TERMINAL_ROWS)),
        )?;
        set(
            &command,
            "max_lines",
            &scrollback_js_value(scrollback_lines)?,
        )?;
        set(
            &command,
            "command_syntax",
            &JsValue::from_str(
                &serde_json::to_string(command_syntax)
                    .map_err(|error| format!("could not serialize command syntax: {error}"))?,
            ),
        )?;
        session.post(&command)?;
        Ok(session)
    }

    fn reconnect(&mut self, endpoint: &str, startup: &StartupSend) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("connect"))?;
        set(&command, "endpoint", &JsValue::from_str(endpoint))?;
        startup.write_to_worker(&command)?;
        self.post(&command)?;
        Ok(())
    }

    fn input(&self, text: &str, masked: bool) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("input"))?;
        set(&command, "text", &JsValue::from_str(text))?;
        set(&command, "masked", &JsValue::from_bool(masked))?;
        self.post(&command)
    }

    fn set_scrollback_lines(&self, lines: usize) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("set-scrollback"))?;
        set(&command, "max_lines", &scrollback_js_value(lines)?)?;
        self.post(&command)
    }

    fn set_command_syntax(&self, syntax: &CommandSyntax) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("set-command-syntax"))?;
        set(
            &command,
            "command_syntax",
            &JsValue::from_str(
                &serde_json::to_string(syntax)
                    .map_err(|error| format!("could not serialize command syntax: {error}"))?,
            ),
        )?;
        self.post(&command)
    }

    fn disconnect(&self) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("disconnect"))?;
        self.post(&command)
    }

    fn acknowledge(&self, sequence: u64) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("ack"))?;
        set(
            &command,
            "sequence",
            &JsValue::from_str(&sequence.to_string()),
        )?;
        self.post(&command)
    }

    fn resize(&self, columns: u16, rows: u16) -> Result<(), String> {
        let command = Object::new();
        set(&command, "kind", &JsValue::from_str("resize"))?;
        set(&command, "columns", &JsValue::from_f64(f64::from(columns)))?;
        set(&command, "rows", &JsValue::from_f64(f64::from(rows)))?;
        self.post(&command)
    }

    fn post(&self, command: &Object) -> Result<(), String> {
        self.worker
            .post_message(command)
            .map_err(|error| format!("could not send to session worker: {error:?}"))
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        self.worker.terminate();
    }
}

fn publish(event: WorkerEvent) {
    EVENT_SENDER.with(|sender| {
        if let Some(sender) = sender.borrow_mut().as_mut() {
            let _ = sender.unbounded_send(event);
        }
    });
}

fn worker_events() -> impl futures::Stream<Item = Message> {
    let (sender, receiver) = mpsc::unbounded();
    EVENT_SENDER.with(|slot| *slot.borrow_mut() = Some(sender));
    receiver.map(Message::Worker)
}

#[derive(Debug, Clone)]
enum Message {
    Ui(UiMessage),
    LayoutsLoaded(u64, String, Result<Vec<String>, String>),
    Layouts(layouts_modal::Message),
    LayoutEffectCompleted(u64, Completion),
    LayoutSaved(u64, String, Result<(), String>),
    LayoutToApply(u64, String, Result<Option<dto::Workspace>, String>),
    CloseLayouts,
    Worker(WorkerEvent),
    CatalogLoaded(Result<Catalog, String>),
    SettingsLoaded(u64, Result<BrowserSettings, String>),
    SettingsSaved(Result<(), String>),
    WindowLoaded(u64, u64, Result<LoadedWindow, String>),
    WindowToRestore(u64, u64, Result<LoadedWindow, String>),
    CatalogSaved(Result<Option<Catalog>, String>),
    PackageImported(u64, Result<Option<String>, String>),
    CredentialLoaded(CredentialRequest, Result<Option<SecretString>, String>),
}

#[derive(Debug, Clone)]
enum CredentialRequest {
    Start(SessionDefinition),
    Reconnect(SessionId, u64),
}

struct SmudgyWeb {
    ui: ClientUiState,
    sessions: HashMap<SessionId, SessionWorker>,
    session_definitions: HashMap<SessionId, SessionDefinition>,
    last_window: Option<dto::Workspace>,
    window_revision: u64,
    window_read_nonce: u64,
    restore_nonce: u64,
    layouts: Option<layouts_modal::State>,
    layout_nonce: u64,
    pending_layout: Option<(String, dto::Workspace)>,
    settings_revision: u64,
}

#[derive(Debug, Clone)]
struct SessionDefinition {
    server: String,
    profile: String,
    endpoint: String,
    encoding: String,
    send_on_connect: String,
    automation: AutomationPlan,
    packages: Vec<String>,
    restore_connect: bool,
    credential_gate: CredentialGate,
}

impl From<ConnectionRequest> for SessionDefinition {
    fn from(request: ConnectionRequest) -> Self {
        Self {
            server: request.server,
            profile: request.profile,
            endpoint: request.endpoint,
            encoding: request.encoding,
            send_on_connect: request.send_on_connect,
            automation: request.automation,
            packages: request.packages,
            restore_connect: true,
            credential_gate: CredentialGate::default(),
        }
    }
}

impl Default for SmudgyWeb {
    fn default() -> Self {
        let mut ui = ClientUiState::new();
        apply_settings(&BrowserSettings {
            appearance: ui.appearance(),
            input: ui.input_preferences(),
            command_syntax: ui.command_syntax().clone(),
            theme: ui.theme_name().to_owned(),
        });
        ui.set_grid_change_handler(Rc::new(|id, columns, rows| {
            publish(WorkerEvent::GridChanged(id, columns, rows));
        }));
        if diagnostic_requested() {
            let id = ui.open_session("diagnostic://scrollback-test");
            drop(ui.apply_session_update(id, diagnostic_update()));
        }
        Self {
            ui,
            sessions: HashMap::new(),
            session_definitions: HashMap::new(),
            last_window: None,
            window_revision: 0,
            window_read_nonce: 1,
            restore_nonce: 0,
            layouts: None,
            layout_nonce: 0,
            pending_layout: None,
            settings_revision: 0,
        }
    }
}

impl SmudgyWeb {
    fn update_worker_scrollback(&mut self, lines: usize) {
        let failures: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(&id, worker)| {
                worker
                    .set_scrollback_lines(lines)
                    .err()
                    .map(|error| (id, error))
            })
            .collect();
        for (id, error) in failures {
            self.sessions.remove(&id);
            self.ui.set_session_error(id, error);
        }
    }

    fn update_worker_command_syntax(&mut self, syntax: &CommandSyntax) {
        let failures: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(&id, worker)| {
                worker
                    .set_command_syntax(syntax)
                    .err()
                    .map(|error| (id, error))
            })
            .collect();
        for (id, error) in failures {
            self.sessions.remove(&id);
            self.ui.set_session_error(id, error);
        }
    }

    fn save_settings(&mut self) -> Task<Message> {
        self.settings_revision = self.settings_revision.wrapping_add(1);
        let settings = BrowserSettings {
            appearance: self.ui.appearance(),
            input: self.ui.input_preferences(),
            command_syntax: self.ui.command_syntax().clone(),
            theme: self.ui.theme_name().to_owned(),
        };
        apply_settings(&settings);
        Task::perform(
            crate::storage::save_settings(settings),
            Message::SettingsSaved,
        )
    }

    fn handle_settings_loaded(
        &mut self,
        revision: u64,
        result: Result<BrowserSettings, String>,
    ) -> Task<Message> {
        if revision != self.settings_revision {
            return Task::none();
        }
        match result {
            Ok(settings) => {
                apply_settings(&settings);
                self.ui.set_appearance(settings.appearance);
                self.update_worker_scrollback(settings.appearance.scrollback_lines);
                let task = self.ui.set_input_preferences(settings.input);
                self.update_worker_command_syntax(&settings.command_syntax);
                self.ui.set_command_syntax(settings.command_syntax);
                self.ui.set_theme(settings.theme);
                task.map(Message::Ui)
            }
            Err(error) => {
                log::warn!("browser settings were not loaded: {error}");
                Task::none()
            }
        }
    }

    fn handle_appearance(
        &mut self,
        message: smudgy_ui_shared::settings_appearance::Message,
    ) -> Task<Message> {
        match self.ui.update_appearance(message) {
            Some(AppearanceChange::ScrollbackLines(lines)) => {
                self.update_worker_scrollback(lines);
                self.save_settings()
            }
            Some(_) => self.save_settings(),
            None => Task::none(),
        }
    }

    fn handle_input_preferences(
        &mut self,
        message: smudgy_ui_shared::settings_input::Message,
    ) -> Task<Message> {
        let (change, task) = self.ui.update_input_preferences(message);
        if change.is_some() {
            Task::batch([task.map(Message::Ui), self.save_settings()])
        } else {
            task.map(Message::Ui)
        }
    }

    fn handle_command_syntax(
        &mut self,
        message: smudgy_ui_shared::settings_input::SyntaxMessage,
    ) -> Task<Message> {
        if !self.ui.update_command_syntax(message) {
            return Task::none();
        }
        let syntax = self.ui.command_syntax().clone();
        self.update_worker_command_syntax(&syntax);
        self.save_settings()
    }

    fn handle_theme(
        &mut self,
        message: smudgy_ui_shared::settings_theme::Message,
    ) -> Task<Message> {
        if self.ui.update_theme(message) {
            self.save_settings()
        } else {
            Task::none()
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Ui(message) => self.handle_ui(message),
            message @ (Message::LayoutsLoaded(..)
            | Message::Layouts(..)
            | Message::LayoutEffectCompleted(..)
            | Message::LayoutSaved(..)
            | Message::LayoutToApply(..)
            | Message::CloseLayouts) => self.handle_layout_message(message),
            Message::Worker(event) => self.handle_worker(event),
            Message::CatalogLoaded(result) => {
                match result {
                    Ok(catalog) => {
                        self.ui.manager.loaded(catalog);
                        self.update_restore_count();
                    }
                    Err(error) => self.ui.manager.failed(error),
                }
                Task::none()
            }
            Message::SettingsLoaded(revision, result) => {
                self.handle_settings_loaded(revision, result)
            }
            Message::SettingsSaved(result) => {
                if let Err(error) = result {
                    log::warn!("browser settings were not saved: {error}");
                }
                Task::none()
            }
            Message::WindowLoaded(nonce, revision, result) => {
                self.handle_window_loaded(nonce, revision, result);
                Task::none()
            }
            Message::WindowToRestore(nonce, revision, result) => {
                self.handle_window_to_restore(nonce, revision, result)
            }
            Message::CatalogSaved(result) => {
                match result {
                    Ok(Some(catalog)) => {
                        if let Some(request) = self.ui.manager.saved(catalog) {
                            return self.connect_request(request);
                        }
                    }
                    Ok(None) => {
                        self.ui
                            .manager
                            .failed("Saved connections changed in another tab; reloading");
                        return Task::perform(crate::storage::load(), Message::CatalogLoaded);
                    }
                    Err(error) => self.ui.manager.failed(error),
                }
                Task::none()
            }
            Message::PackageImported(nonce, result) => {
                match result {
                    Ok(Some(name)) => {
                        self.ui
                            .manager
                            .update(connect_manager::Message::PackageImported { nonce, name });
                    }
                    Ok(None) => {}
                    Err(error) => self.ui.manager.import_failed(nonce, error),
                }
                Task::none()
            }
            Message::CredentialLoaded(request, password) => {
                match request {
                    CredentialRequest::Start(definition) => {
                        match password
                            .and_then(|value| expand_password(&definition.send_on_connect, value))
                        {
                            Ok(expanded) => self.start_session(definition, &expanded),
                            Err(error) => self.ui.manager.failed(error),
                        }
                    }
                    CredentialRequest::Reconnect(id, revision) => {
                        if let Some(definition) = self.session_definitions.get(&id) {
                            if !definition.credential_gate.accepts(revision) {
                                return Task::none();
                            }
                            match password.and_then(|value| {
                                expand_password(&definition.send_on_connect, value)
                            }) {
                                Ok(expanded) => self.reconnect_session(id, &expanded),
                                Err(error) => self.ui.set_session_error(id, error),
                            }
                        }
                    }
                }
                Task::none()
            }
        }
    }

    // This is the top-level exhaustive router for the shared UI protocol. Its
    // branches stay together so browser-only effects remain visible in one
    // place beside the shared reducer calls.
    #[allow(clippy::too_many_lines)]
    fn handle_ui(&mut self, message: UiMessage) -> Task<Message> {
        if let UiMessage::Toolbar(smudgy_ui_shared::main_toolbar::Message::Layouts) = message {
            return self.open_layouts();
        }
        if let UiMessage::PaneClicked(pane) = message {
            self.ui.activate_pane(pane);
            return Task::none();
        }
        if let UiMessage::TabSelected(tab) = message {
            if self.ui.select_tab(tab) {
                self.save_window();
            }
            return Task::none();
        }
        if let UiMessage::TabStrip(group, event) = message {
            match event {
                smudgy_ui_shared::tab_strip::Event::Connect(tab) => {
                    return self
                        .ui
                        .tab_session(tab)
                        .map_or_else(Task::none, |id| self.handle_ui(UiMessage::Reconnect(id)));
                }
                smudgy_ui_shared::tab_strip::Event::Disconnect(tab) => {
                    return self
                        .ui
                        .tab_session(tab)
                        .map_or_else(Task::none, |id| self.handle_ui(UiMessage::Disconnect(id)));
                }
                smudgy_ui_shared::tab_strip::Event::CloseSession(tab) => {
                    return self
                        .ui
                        .tab_session(tab)
                        .map_or_else(Task::none, |id| self.handle_ui(UiMessage::CloseSession(id)));
                }
                smudgy_ui_shared::tab_strip::Event::ToggleVisibility(_) => {
                    return Task::none();
                }
                event => {
                    if self.ui.handle_tab_strip(group, event) {
                        self.save_window();
                    }
                    return Task::none();
                }
            }
        }
        if let UiMessage::DragMoved(point) = message {
            self.ui.drag_moved(point);
            return Task::none();
        }
        if matches!(message, UiMessage::DragReleased | UiMessage::DragCanceled) {
            let changed = if matches!(message, UiMessage::DragReleased) {
                self.ui.release_drag()
            } else {
                let _ = self.ui.finish_drag(None);
                false
            };
            if changed {
                self.save_window();
            }
            return Task::none();
        }
        self.ui.handle_chrome(&message);
        match message {
            UiMessage::Toolbar(smudgy_ui_shared::main_toolbar::Message::Connect)
            | UiMessage::OpenConnect => self.refresh_last_window(),
            UiMessage::Toolbar(_)
            | UiMessage::ClosePanel
            | UiMessage::AnimationFrame(_)
            | UiMessage::PaneClicked(_)
            | UiMessage::TabSelected(_)
            | UiMessage::TabStrip(..)
            | UiMessage::DragMoved(_)
            | UiMessage::DragReleased
            | UiMessage::DragCanceled => Task::none(),
            UiMessage::Appearance(message) => self.handle_appearance(message),
            UiMessage::Input(message) => self.handle_input_preferences(message),
            UiMessage::Syntax(message) => self.handle_command_syntax(message),
            UiMessage::Theme(message) => self.handle_theme(message),
            UiMessage::Manager(message) => match self.ui.manager.update(message) {
                connect_manager::Effect::None => Task::none(),
                connect_manager::Effect::Persist {
                    expected,
                    catalog,
                    secret,
                } => Task::perform(
                    crate::storage::save(expected, catalog, secret),
                    Message::CatalogSaved,
                ),
                connect_manager::Effect::Connect(request) => self.connect_request(request),
                connect_manager::Effect::RestoreWindow => self.restore_window(),
                connect_manager::Effect::ImportPackage { nonce } => {
                    Task::perform(crate::packages::select_and_install(), move |result| {
                        Message::PackageImported(nonce, result)
                    })
                }
            },
            UiMessage::Disconnect(id) => {
                if let Some(definition) = self.session_definitions.get_mut(&id) {
                    definition.credential_gate.advance();
                    definition.restore_connect = false;
                    self.save_window();
                }
                if let Some(Err(error)) = self.sessions.get(&id).map(SessionWorker::disconnect) {
                    self.sessions.remove(&id);
                    self.ui.set_session_error(id, error);
                }
                Task::none()
            }
            UiMessage::Reconnect(id) => {
                let Some(definition) = self.session_definitions.get_mut(&id) else {
                    return Task::none();
                };
                let revision = definition.credential_gate.advance();
                if definition.send_on_connect.contains("$PASSWORD") {
                    self.request_password(CredentialRequest::Reconnect(id, revision))
                } else {
                    let send = definition.send_on_connect.clone();
                    self.reconnect_session(id, &StartupSend::plain(send));
                    Task::none()
                }
            }
            UiMessage::CloseSession(id) => {
                if let Some(worker) = self.sessions.remove(&id) {
                    let _ = worker.disconnect();
                }
                let _ = self.ui.close_session(id);
                self.session_definitions.remove(&id);
                self.save_window();
                self.update_restore_count();
                Task::none()
            }
            UiMessage::SessionInput(id, message) => {
                let update = self.ui.update_session_input(id, message);
                if let Some(smudgy_ui_shared::session_input::Event::Submit { text, masked }) =
                    update.event
                {
                    if self.ui.session_connection(id) != Some(ConnectionState::Connected) {
                        return Task::batch([
                            update.task.map(Message::Ui),
                            self.recover_failed_submission(id, &text, masked),
                        ]);
                    }
                    if let Some(Err(error)) = self
                        .sessions
                        .get(&id)
                        .map(|worker| worker.input(&text, masked))
                    {
                        self.sessions.remove(&id);
                        self.ui.set_session_error(id, error);
                    }
                }
                update.task.map(Message::Ui)
            }
            UiMessage::PaneResized(event) => {
                self.ui.resize_pane(event);
                self.save_window();
                Task::none()
            }
        }
    }

    fn start_session(&mut self, definition: SessionDefinition, startup: &StartupSend) {
        let id = self.ui.open_session(definition.endpoint.clone());
        match SessionWorker::start(SessionStart {
            id,
            endpoint: &definition.endpoint,
            encoding: &definition.encoding,
            startup,
            automation: &definition.automation,
            packages: &definition.packages,
            scrollback_lines: self.ui.appearance().scrollback_lines,
            command_syntax: self.ui.command_syntax(),
        }) {
            Ok(worker) => {
                self.sessions.insert(id, worker);
            }
            Err(error) => self.ui.set_session_error(id, error),
        }
        self.session_definitions.insert(id, definition);
        self.ui.manager.set_restore_count(0);
        self.save_window();
    }

    fn connect_request(&mut self, request: ConnectionRequest) -> Task<Message> {
        let definition = SessionDefinition::from(request);
        if definition.send_on_connect.contains("$PASSWORD") {
            self.request_password(CredentialRequest::Start(definition))
        } else {
            let send = definition.send_on_connect.clone();
            self.start_session(definition, &StartupSend::plain(send));
            Task::none()
        }
    }

    fn restore_window(&mut self) -> Task<Message> {
        if self.ui.session_count() != 0 {
            self.ui
                .manager
                .failed("Restore is available only in an empty window");
            return Task::none();
        }
        self.restore_nonce = self.restore_nonce.wrapping_add(1);
        let nonce = self.restore_nonce;
        let revision = self.window_revision;
        Task::perform(crate::storage::load_last_window(), move |result| {
            Message::WindowToRestore(nonce, revision, result)
        })
    }

    fn handle_window_loaded(
        &mut self,
        nonce: u64,
        revision: u64,
        result: Result<LoadedWindow, String>,
    ) {
        if nonce != self.window_read_nonce
            || revision != self.window_revision
            || self.ui.session_count() != 0
        {
            return;
        }
        match result {
            Ok(LoadedWindow {
                workspace,
                migrated,
            }) => {
                self.last_window = workspace;
                self.update_restore_count();
                if migrated && let Some(workspace) = self.last_window.clone() {
                    persist_window(workspace);
                }
            }
            Err(error) => {
                log::warn!("window restore snapshot was not loaded: {error}");
                self.ui.manager.failed(error);
            }
        }
    }

    fn handle_window_to_restore(
        &mut self,
        nonce: u64,
        revision: u64,
        result: Result<LoadedWindow, String>,
    ) -> Task<Message> {
        if nonce != self.restore_nonce
            || revision != self.window_revision
            || self.ui.session_count() != 0
        {
            return Task::none();
        }
        match result {
            Ok(LoadedWindow {
                workspace: Some(workspace),
                ..
            }) => self.apply_restored_window(workspace),
            Ok(LoadedWindow {
                workspace: None, ..
            }) => {
                self.ui
                    .manager
                    .failed("No saved window is available to restore");
                Task::none()
            }
            Err(error) => {
                self.ui.manager.failed(error);
                Task::none()
            }
        }
    }

    fn apply_restored_window(&mut self, mut workspace: dto::Workspace) -> Task<Message> {
        // The catalog is authoritative: remove deleted identities, then let
        // the shared sanitizer collapse only their pane branches.
        workspace.sessions.retain(|slot| {
            self.ui
                .manager
                .catalog
                .resolve_connection(&slot.server, &slot.profile)
                .is_some()
        });
        let Ok(workspace) = workspace.sanitized_single_window() else {
            self.ui
                .manager
                .failed("The previous window's servers or profiles no longer exist");
            return Task::none();
        };
        let window = &workspace.windows[0];
        if let Err(error) = ui::validate_browser_clusters(&window.clusters) {
            self.ui.manager.failed(error);
            return Task::none();
        }
        let mut slot_to_session = HashMap::new();
        for slot in &workspace.sessions {
            let request = self
                .ui
                .manager
                .catalog
                .resolve_connection(&slot.server, &slot.profile)
                .expect("restorable identities were checked above");
            let mut definition = SessionDefinition::from(request);
            definition.restore_connect = slot.connect;
            let id = self.ui.open_session(definition.endpoint.clone());
            self.ui.set_session_disconnected(id);
            self.session_definitions.insert(id, definition);
            slot_to_session.insert(slot.id, id);
        }
        let clusters = window
            .clusters
            .iter()
            .map(|cluster| {
                (
                    cluster.weight,
                    ui::blueprint_from_dto(&cluster.root, &|slot| {
                        slot_to_session.get(&slot).copied()
                    })
                    .expect("validated snapshot slots were resolved"),
                )
            })
            .collect();
        let active = window
            .active_slot
            .and_then(|slot| slot_to_session.get(&slot).copied());
        self.ui.install_blueprint(clusters, active);
        self.save_window();
        let mut tasks = Vec::new();
        for slot in &workspace.sessions {
            if !slot.connect {
                continue;
            }
            let id = slot_to_session[&slot.id];
            let definition = &self.session_definitions[&id];
            if definition.send_on_connect.contains("$PASSWORD") {
                tasks.push(self.request_password(CredentialRequest::Reconnect(id, 0)));
            } else {
                let send = definition.send_on_connect.clone();
                self.reconnect_session(id, &StartupSend::plain(send));
            }
        }
        Task::batch(tasks)
    }

    fn save_window(&mut self) {
        if self.ui.session_count() == 0 {
            return;
        }
        let workspace = match self.capture_workspace() {
            Ok(workspace) => workspace,
            Err(error) => {
                log::warn!("window restore snapshot was not captured: {error}");
                return;
            }
        };
        self.window_revision = self.window_revision.wrapping_add(1);
        persist_window(workspace.clone());
        self.last_window = Some(workspace);
    }

    fn refresh_last_window(&mut self) -> Task<Message> {
        if self.ui.session_count() != 0 {
            return Task::none();
        }
        self.window_read_nonce = self.window_read_nonce.wrapping_add(1);
        let nonce = self.window_read_nonce;
        let revision = self.window_revision;
        Task::perform(crate::storage::load_last_window(), move |result| {
            Message::WindowLoaded(nonce, revision, result)
        })
    }

    fn update_restore_count(&mut self) {
        self.ui
            .manager
            .set_restore_count(if self.ui.session_count() == 0 {
                self.last_window
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.sessions.len())
            } else {
                0
            });
    }

    fn request_password(&self, request: CredentialRequest) -> Task<Message> {
        let definition = match &request {
            CredentialRequest::Start(definition) => definition,
            CredentialRequest::Reconnect(id, _) => self
                .session_definitions
                .get(id)
                .expect("reconnect request has a session definition"),
        };
        let server = definition.server.clone();
        let profile = definition.profile.clone();
        Task::perform(
            async move { crate::storage::load_password(&server, &profile).await },
            move |result| Message::CredentialLoaded(request, result),
        )
    }

    fn reconnect_session(&mut self, id: SessionId, startup: &StartupSend) {
        let Some(definition) = self.session_definitions.get(&id) else {
            return;
        };
        let result = if let Some(worker) = self.sessions.get_mut(&id) {
            worker.reconnect(&definition.endpoint, startup)
        } else {
            SessionWorker::start(SessionStart {
                id,
                endpoint: &definition.endpoint,
                encoding: &definition.encoding,
                startup,
                automation: &definition.automation,
                packages: &definition.packages,
                scrollback_lines: self.ui.appearance().scrollback_lines,
                command_syntax: self.ui.command_syntax(),
            })
            .map(|worker| {
                self.sessions.insert(id, worker);
            })
        };
        match result {
            Ok(()) => {
                self.ui.set_session_connecting(id);
                if let Some(definition) = self.session_definitions.get_mut(&id) {
                    definition.restore_connect = true;
                }
                self.save_window();
            }
            Err(error) => {
                self.sessions.remove(&id);
                self.ui.set_session_error(id, error);
            }
        }
    }

    fn recover_failed_submission(
        &mut self,
        id: SessionId,
        text: &Arc<String>,
        masked: bool,
    ) -> Task<Message> {
        let Some(definition) = self.session_definitions.get(&id) else {
            return Task::none();
        };
        let action = decide_send_failure(
            ConnectionIntent::from_bool(definition.restore_connect),
            self.ui.input_preferences().reconnect_on_send_error,
            if self.ui.session_connection(id) == Some(ConnectionState::Connecting) {
                ReconnectState::Connecting
            } else {
                ReconnectState::Idle
            },
        );
        let outcome = match action {
            SendFailureAction::Reconnect | SendFailureAction::JoinReconnect => " Reconnecting…",
            SendFailureAction::ReportConnecting => " Still connecting…",
            SendFailureAction::ReportNotConnected => " Not connected.",
        };
        let display = if masked {
            REDACTION_MASK
        } else {
            text.as_str()
        };
        let notice = format!("“{display}” was not sent.{outcome}");
        let restore = self
            .ui
            .recover_failed_submission(id, Arc::clone(text), &notice)
            .map(Message::Ui);

        if action != SendFailureAction::Reconnect {
            return restore;
        }

        let (requires_password, startup, revision) = {
            let definition = self
                .session_definitions
                .get_mut(&id)
                .expect("the failed submission had a session definition");
            (
                definition.send_on_connect.contains("$PASSWORD"),
                definition.send_on_connect.clone(),
                definition.credential_gate.advance(),
            )
        };
        let reconnect = if requires_password {
            self.request_password(CredentialRequest::Reconnect(id, revision))
        } else {
            self.reconnect_session(id, &StartupSend::plain(startup));
            Task::none()
        };
        Task::batch([restore, reconnect])
    }

    fn handle_worker(&mut self, event: WorkerEvent) -> Task<Message> {
        match event {
            WorkerEvent::State(id, update) => {
                if !self.sessions.contains_key(&id) {
                    return Task::none();
                }
                let sequence = update.sequence;
                let task = self.ui.apply_session_update(id, update).map(Message::Ui);
                let mut worker_failed = false;
                if let Some(worker) = self.sessions.get_mut(&id)
                    && let Err(error) = worker.acknowledge(sequence)
                {
                    self.ui.set_session_error(id, error);
                    worker_failed = true;
                }
                if worker_failed {
                    self.sessions.remove(&id);
                }
                task
            }
            WorkerEvent::Pane(id, event) => {
                if self.sessions.contains_key(&id) {
                    let layout_changed = matches!(
                        &event,
                        ui::BrowserPaneEvent::Opened { .. }
                            | ui::BrowserPaneEvent::Updated { .. }
                            | ui::BrowserPaneEvent::Closed(_)
                    );
                    self.ui.apply_browser_pane_event(id, event);
                    if layout_changed {
                        self.save_window();
                    }
                }
                Task::none()
            }
            WorkerEvent::Failed(id, error) => {
                self.sessions.remove(&id);
                self.ui.set_session_error(id, error);
                Task::none()
            }
            WorkerEvent::RuntimeFailed(id, error) => {
                self.sessions.remove(&id);
                let display = self.ui.active_session().unwrap_or(id);
                self.ui.set_session_error(display, error);
                Task::none()
            }
            WorkerEvent::RuntimeUnavailable(id) => {
                self.sessions.remove(&id);
                self.ui.set_session_disconnected(id);
                Task::none()
            }
            WorkerEvent::GridChanged(id, columns, rows) => {
                if let Some(Err(error)) = self
                    .sessions
                    .get(&id)
                    .map(|worker| worker.resize(columns, rows))
                {
                    self.sessions.remove(&id);
                    self.ui.set_session_error(id, error);
                }
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message, Theme> {
        let translator = smudgy_i18n::Translator::default();
        let main = ui::view(&self.ui, &|id| translator.translate(id)).map(Message::Ui);
        let Some(state) = &self.layouts else {
            return main;
        };
        let labels = WebLabels(smudgy_i18n::Translator::default());
        let modal = smudgy_ui_shared::connect_modal::frame_with_size(
            labels.text("toolbar-layouts"),
            smudgy_ui_shared::layouts_view::view(state, &labels).map(Message::Layouts),
            640.0,
            500.0,
        );
        iced::widget::stack![
            main,
            iced::widget::opaque(
                iced::widget::mouse_area(
                    iced::widget::center(iced::widget::opaque(modal))
                        .style(smudgy_theme::builtins::container::overlay)
                )
                .on_press(Message::CloseLayouts)
            )
        ]
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![Subscription::run(worker_events)];
        if self.ui.drag_active() {
            subscriptions.push(iced::event::listen_with(ui::pane_drag_event).map(Message::Ui));
        }
        if self.ui.needs_animation_frames() {
            subscriptions.push(
                iced::window::frames().map(|now| Message::Ui(UiMessage::AnimationFrame(now))),
            );
        }
        Subscription::batch(subscriptions)
    }

    fn theme(app: &Self) -> Theme {
        app.ui.theme()
    }
}

fn persist_window(workspace: dto::Workspace) {
    wasm_bindgen_futures::spawn_local(async move {
        match crate::storage::save_last_window(workspace).await {
            Ok(()) => crate::window_restore::clear_legacy(),
            Err(error) => log::warn!("window restore snapshot was not saved: {error}"),
        }
    });
}

pub fn run() -> iced::Result {
    iced::application(
        || {
            let app = SmudgyWeb::default();
            let nonce = app.window_read_nonce;
            let revision = app.window_revision;
            (
                app,
                Task::batch([
                    Task::perform(crate::storage::load(), Message::CatalogLoaded),
                    Task::perform(crate::storage::load_settings(), |result| {
                        Message::SettingsLoaded(0, result)
                    }),
                    Task::perform(crate::storage::load_last_window(), move |result| {
                        Message::WindowLoaded(nonce, revision, result)
                    }),
                ]),
            )
        },
        SmudgyWeb::update,
        SmudgyWeb::view,
    )
    .title("Smudgy Web")
    .theme(SmudgyWeb::theme)
    .subscription(SmudgyWeb::subscription)
    .font(smudgy_ui_shared::assets::GEIST_BYTES)
    .font(smudgy_ui_shared::assets::GEIST_ITALIC_BYTES)
    .font(smudgy_ui_shared::assets::GEIST_MONO_BYTES)
    .font(smudgy_ui_shared::assets::GEIST_MONO_ITALIC_BYTES)
    .run()
}

fn diagnostic_requested() -> bool {
    web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .is_some_and(|hash| hash == "#scrollback-test")
}

fn diagnostic_update() -> SessionUpdate {
    let mut rows = Vec::with_capacity(100_000);
    for row in 0..100_000 {
        let mut line = String::new();
        write!(
            line,
            "{row:06}  Smudgy virtual scrollback · Geist Mono · row {:06}",
            row + 1
        )
        .expect("writing into a String cannot fail");
        let end_pos = line.len();
        rows.push(StyledLine::from_owned(
            line,
            vec![VtSpan {
                begin_pos: 0,
                end_pos,
                style: Style {
                    fg: Color::Rgb {
                        r: 70,
                        g: 150,
                        b: 220,
                    },
                    bg: Color::DefaultBackground,
                    attributes: TextAttributes {
                        blink: Blink::None,
                        underline: Underline::None,
                        ..TextAttributes::DEFAULT
                    },
                },
            }],
        ));
    }
    SessionUpdate {
        sequence: 0,
        connection: ConnectionState::Disconnected,
        status: "100,000-line worker presentation diagnostic".to_owned(),
        server_echo: false,
        gmcp_messages: 0,
        max_lines: 100_000,
        revision: 100_000,
        committed: 100_000,
        reset: true,
        rows,
        live: None,
    }
}

fn set(object: &Object, name: &str, value: &JsValue) -> Result<(), String> {
    Reflect::set(object, &JsValue::from_str(name), value)
        .map(|_| ())
        .map_err(|error| format!("could not construct worker message: {error:?}"))
}

fn get_string(value: &JsValue, name: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_string())
}

fn get_u32(value: &JsValue, name: &str) -> Option<u32> {
    let number = Reflect::get(value, &JsValue::from_str(name))
        .ok()?
        .as_f64()?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(0.0..=f64::from(u32::MAX)).contains(&number)
    {
        return None;
    }
    // JavaScript numbers are f64; the checks above prove this is an exact u32.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(number as u32)
}

fn get_f32(value: &JsValue, name: &str) -> Result<Option<f32>, String> {
    let Some(number) = Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_f64())
    else {
        return Ok(None);
    };
    if !number.is_finite() || !(f64::from(f32::MIN)..=f64::from(f32::MAX)).contains(&number) {
        return Err(format!("pane event has an invalid {name}"));
    }
    // The range check above makes this the intended narrowing conversion at
    // the JavaScript/WASM boundary.
    #[allow(clippy::cast_possible_truncation)]
    Ok(Some(number as f32))
}

fn get_bool(value: &JsValue, name: &str) -> Option<bool> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_bool())
}

fn browser_pane_event(value: &JsValue) -> Result<ui::BrowserPaneEvent, String> {
    let action = get_string(value, "action").ok_or("pane event is missing its action")?;
    let key = PaneKey::from_u32(get_u32(value, "key").ok_or("pane event has no valid key")?);
    match action.as_str() {
        "opened" => {
            let name = get_string(value, "name").ok_or("opened pane has no name")?;
            let hidden = get_bool(value, "hidden").unwrap_or(false);
            let reference = PaneKey::from_u32(
                get_u32(value, "reference").ok_or("opened pane has no reference")?,
            );
            let placement = match get_string(value, "placement").as_deref() {
                Some("split") => {
                    let direction = match get_string(value, "direction").as_deref() {
                        Some("left") => SplitDirection::Left,
                        Some("right") => SplitDirection::Right,
                        Some("top") => SplitDirection::Top,
                        Some("bottom") => SplitDirection::Bottom,
                        _ => return Err("opened pane has an invalid split direction".to_owned()),
                    };
                    let size_px = get_f32(value, "size")?;
                    PanePlacement::Split {
                        reference,
                        direction,
                        size_px,
                    }
                }
                Some("tab") => PanePlacement::Tab {
                    reference,
                    position: TabPosition::After,
                    selected: get_bool(value, "selected").unwrap_or(false),
                },
                _ => return Err("opened pane has an invalid placement".to_owned()),
            };
            Ok(ui::BrowserPaneEvent::Opened {
                key,
                name,
                hidden,
                placement,
            })
        }
        "updated" => Ok(ui::BrowserPaneEvent::Updated {
            key,
            hidden: get_bool(value, "hidden").ok_or("updated pane has no visibility")?,
        }),
        "echo" => Ok(ui::BrowserPaneEvent::Echo {
            key,
            text: get_string(value, "text").ok_or("pane echo has no text")?,
        }),
        "clear" => Ok(ui::BrowserPaneEvent::Clear(key)),
        "closed" => Ok(ui::BrowserPaneEvent::Closed(key)),
        _ => Err("session worker sent an unknown pane action".to_owned()),
    }
}
