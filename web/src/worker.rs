//! One browser-worker-owned Smudgy session runtime.
//!
//! Each session worker instantiates the worker-only WASM binary and calls the
//! exported [`worker_message`] entry point from `session-worker.js`.

use std::cell::RefCell;

use encoding_rs::{Encoding, UTF_8};
use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use smudgy_engine::{
    ClientEffect, ClientEngine, ClientEvent, PaneEffect, TerminalBuffer, TerminalDeltaCursor,
};
use smudgy_session_model::automation::AutomationPlan;
use smudgy_session_model::input_policy::CommandSyntax;
use smudgy_session_model::{DEFAULT_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES, MIN_SCROLLBACK_LINES};
use smudgy_web_client::flow::PresentationFlow;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::{BinaryType, CloseEvent, DedicatedWorkerGlobalScope, Event, MessageEvent, WebSocket};

use crate::script_host::WorkerAutomation;

const DEFAULT_COLUMNS: u16 = 100;
const DEFAULT_ROWS: u16 = 36;
const PRESENTATION_DELAY_MS: i32 = 8;

thread_local! {
    static SESSION: RefCell<Option<SessionWorker>> = const { RefCell::new(None) };
    static FLUSH_TIMER: Closure<dyn FnMut()> = Closure::new(flush_global);
}

struct SessionWorker {
    engine: ClientEngine<WorkerAutomation>,
    socket: Option<SocketTransport>,
    socket_generation: u64,
    terminal_cursor: TerminalDeltaCursor,
    flow: PresentationFlow,
}

impl SessionWorker {
    fn new(
        columns: u16,
        rows: u16,
        scrollback_lines: usize,
        automation: &AutomationPlan,
        encoding: &'static Encoding,
        factory: Option<&Function>,
    ) -> Result<Self, String> {
        Ok(Self {
            engine: ClientEngine::with_terminal_and_encoding(
                WorkerAutomation::new(automation, factory)?,
                columns,
                rows,
                TerminalBuffer::with_max_lines(scrollback_lines),
                encoding,
            ),
            socket: None,
            socket_generation: 0,
            terminal_cursor: TerminalDeltaCursor::default(),
            flow: PresentationFlow::default(),
        })
    }

    fn dispatch(&mut self, event: ClientEvent) {
        let mut pending = vec![event];
        while let Some(event) = pending.pop() {
            let effects = self.engine.handle(event);
            for effect in effects {
                if let Err(error) = self.execute(effect) {
                    pending.push(ClientEvent::TransportFailed(error));
                    break;
                }
            }
        }
        if self.flow.changed() {
            Self::schedule_flush();
        }
    }

    fn set_scrollback_lines(&mut self, lines: usize) {
        if self.engine.terminal().max_lines() == lines {
            return;
        }
        self.engine.set_scrollback_lines(lines);
        if self.flow.changed() {
            Self::schedule_flush();
        }
    }

    fn execute(&mut self, effect: ClientEffect) -> Result<(), String> {
        match effect {
            ClientEffect::OpenTransport(url) => {
                self.socket = None;
                self.socket_generation = self.socket_generation.wrapping_add(1);
                self.socket = Some(SocketTransport::open(&url, self.socket_generation)?);
                Ok(())
            }
            ClientEffect::CloseTransport => {
                self.socket.take().map_or(Ok(()), |socket| socket.close())
            }
            ClientEffect::SendNetwork(bytes) => self
                .socket
                .as_ref()
                .ok_or_else(|| "connection worker has no WebSocket".to_owned())?
                .send(&bytes),
            ClientEffect::SendCommand { bytes, display } => {
                self.socket
                    .as_ref()
                    .ok_or_else(|| "connection worker has no WebSocket".to_owned())?
                    .send(&bytes)?;
                self.engine.command_sent(&display);
                Ok(())
            }
            ClientEffect::Pane(effect) => {
                publish_pane(effect);
                Ok(())
            }
        }
    }

    fn schedule_flush() {
        let global: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let scheduled = FLUSH_TIMER.with(|timer| {
            global.set_timeout_with_callback_and_timeout_and_arguments_0(
                timer.as_ref().unchecked_ref(),
                PRESENTATION_DELAY_MS,
            )
        });
        if scheduled.is_err() {
            publish_fatal("session worker could not schedule its terminal update");
        }
    }

    fn acknowledge(&mut self, sequence: u64) {
        if self.flow.acknowledge(sequence) {
            Self::schedule_flush();
        }
    }

    fn publish(&mut self) {
        let Some(sequence) = self.flow.timer_fired() else {
            return;
        };
        let terminal = self.engine.terminal();
        let mut candidate_cursor = self.terminal_cursor;
        let delta = candidate_cursor.take(terminal);
        let frame = smudgy_web_client::wire::SessionFrame {
            sequence,
            connection: self.engine.connection(),
            status: self.engine.status(),
            server_echo: self.engine.server_echo(),
            gmcp_messages: self.engine.gmcp_messages(),
            max_lines: terminal.max_lines(),
            delta,
        };
        let Ok(bytes) = smudgy_web_client::wire::encode(&frame) else {
            publish_fatal("session worker could not encode its terminal update");
            return;
        };
        // wasm-bindgen copies once into a standalone JS buffer. Transferring
        // that buffer then moves it to the window without structured-cloning
        // every line, span, string, color, and counter.
        let payload = Uint8Array::new_from_slice(&bytes);
        let buffer = payload.buffer();
        let transfer = Array::of1(&buffer);
        let global: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        if global
            .post_message_with_transfer(&buffer, &transfer)
            .is_err()
        {
            publish_fatal("session worker could not post its terminal update");
            return;
        }
        self.terminal_cursor = candidate_cursor;
        self.flow.sent(sequence);
    }
}

struct SocketTransport {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
}

impl SocketTransport {
    fn open(url: &str, generation: u64) -> Result<Self, String> {
        let socket =
            WebSocket::new(url).map_err(|error| format!("could not open WebSocket: {error:?}"))?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
            dispatch_global_for(generation, ClientEvent::TransportOpened);
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = event.data();
            if data.is_instance_of::<js_sys::ArrayBuffer>() {
                dispatch_global_for(
                    generation,
                    ClientEvent::TransportBytes(Uint8Array::new(&data).to_vec()),
                );
            } else {
                dispatch_global_for(
                    generation,
                    ClientEvent::TransportFailed(
                        "MUD sent a non-binary WebSocket message".to_owned(),
                    ),
                );
            }
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
            dispatch_global_for(
                generation,
                ClientEvent::TransportClosed {
                    code: event.code(),
                    reason: event.reason(),
                },
            );
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        let on_error = Closure::<dyn FnMut(Event)>::new(move |_| {
            dispatch_global_for(
                generation,
                ClientEvent::TransportFailed("WebSocket connection failed".to_owned()),
            );
        });
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        Ok(Self {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
            _on_error: on_error,
        })
    }

    fn send(&self, bytes: &[u8]) -> Result<(), String> {
        self.socket
            .send_with_u8_array(bytes)
            .map_err(|error| format!("could not send WebSocket data: {error:?}"))
    }

    fn close(&self) -> Result<(), String> {
        self.socket
            .close_with_code_and_reason(1000, "client disconnect")
            .map_err(|error| format!("could not close WebSocket: {error:?}"))
    }
}

impl Drop for SocketTransport {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
    }
}

fn dispatch_global(event: ClientEvent) {
    SESSION.with(|slot| {
        if let Some(session) = slot.borrow_mut().as_mut() {
            session.dispatch(event);
        }
    });
}

fn dispatch_global_for(generation: u64, event: ClientEvent) {
    SESSION.with(|slot| {
        if let Some(session) = slot.borrow_mut().as_mut()
            && session.socket_generation == generation
        {
            session.dispatch(event);
        }
    });
}

fn flush_global() {
    SESSION.with(|slot| {
        if let Some(session) = slot.borrow_mut().as_mut() {
            session.publish();
        }
    });
}

fn start_session(message: &JsValue) -> Result<(), String> {
    let columns = get_u16(message, "columns").unwrap_or(DEFAULT_COLUMNS);
    let rows = get_u16(message, "rows").unwrap_or(DEFAULT_ROWS);
    let scrollback_lines = get_scrollback_lines(message)?.unwrap_or(DEFAULT_SCROLLBACK_LINES);
    let command_syntax = get_command_syntax(message)?.unwrap_or_default();
    let endpoint = get_string(message, "endpoint").unwrap_or_default();
    let encoding = match get_string(message, "encoding") {
        None => UTF_8,
        Some(label) if label.is_empty() => UTF_8,
        Some(label) => Encoding::for_label_no_replacement(label.as_bytes()).ok_or_else(|| {
            "session worker received an unsupported character encoding".to_owned()
        })?,
    };
    let send_on_connect = get_string(message, "send_on_connect").unwrap_or_default();
    let send_on_connect_redactions = get_redactions(message)?;
    let automation: AutomationPlan = serde_json::from_str(
        &get_string(message, "automation").ok_or("worker start missing automation")?,
    )
    .map_err(|error| format!("invalid automation: {error}"))?;
    let factory = Reflect::get(message, &JsValue::from_str("script_factory"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok());
    SESSION.with(|slot| {
        let mut session = SessionWorker::new(
            columns,
            rows,
            scrollback_lines,
            &automation,
            encoding,
            factory.as_ref(),
        )?;
        session.engine.set_command_syntax(command_syntax);
        for effect in session.engine.start_automation() {
            session.execute(effect)?;
        }
        session.dispatch(ClientEvent::ConnectRequested {
            endpoint,
            send_on_connect,
            send_on_connect_redactions,
        });
        *slot.borrow_mut() = Some(session);
        Ok(())
    })
}

/// Receive a page command inside a dedicated session worker.
#[wasm_bindgen]
#[allow(clippy::needless_pass_by_value)]
pub fn worker_message(message: JsValue) {
    let kind = get_string(&message, "kind").unwrap_or_default();
    match kind.as_str() {
        "start" => {
            if let Err(error) = start_session(&message) {
                publish_fatal(&error);
            }
        }
        "input" => {
            if let Some(text) = get_string(&message, "text") {
                let masked = Reflect::get(&message, &JsValue::from_str("masked"))
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                dispatch_global(ClientEvent::UserInput { text, masked });
            }
        }
        "set-scrollback" => match get_scrollback_lines(&message) {
            Ok(Some(lines)) => SESSION.with(|slot| {
                if let Some(session) = slot.borrow_mut().as_mut() {
                    session.set_scrollback_lines(lines);
                }
            }),
            Ok(None) => publish_fatal("scrollback update is missing its limit"),
            Err(error) => publish_fatal(&error),
        },
        "set-command-syntax" => match get_command_syntax(&message) {
            Ok(Some(syntax)) => SESSION.with(|slot| {
                if let Some(session) = slot.borrow_mut().as_mut() {
                    session.engine.set_command_syntax(syntax);
                }
            }),
            Ok(None) => publish_fatal("command syntax update is missing its value"),
            Err(error) => publish_fatal(&error),
        },
        "connect" => {
            if let Some(endpoint) = get_string(&message, "endpoint") {
                let send_on_connect = get_string(&message, "send_on_connect").unwrap_or_default();
                match get_redactions(&message) {
                    Ok(send_on_connect_redactions) => {
                        dispatch_global(ClientEvent::ConnectRequested {
                            endpoint,
                            send_on_connect,
                            send_on_connect_redactions,
                        });
                    }
                    Err(error) => publish_fatal(&error),
                }
            }
        }
        "disconnect" => dispatch_global(ClientEvent::DisconnectRequested),
        "resize" => {
            let columns = get_u16(&message, "columns").unwrap_or(DEFAULT_COLUMNS);
            let rows = get_u16(&message, "rows").unwrap_or(DEFAULT_ROWS);
            dispatch_global(ClientEvent::Resize { columns, rows });
        }
        "ack" => {
            if let Some(sequence) =
                get_string(&message, "sequence").and_then(|value| value.parse::<u64>().ok())
            {
                SESSION.with(|slot| {
                    if let Some(session) = slot.borrow_mut().as_mut() {
                        session.acknowledge(sequence);
                    }
                });
            }
        }
        _ => publish_fatal("session worker received an unknown command"),
    }
}

fn publish_fatal(message: &str) {
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str("fatal"));
    set(&value, "message", &JsValue::from_str(message));
    let global: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let _ = global.post_message(&value);
}

fn publish_pane(effect: PaneEffect) {
    use smudgy_session_model::pane::{PanePlacement, SplitDirection};

    let value = Object::new();
    set(&value, "kind", &JsValue::from_str("pane"));
    match effect {
        PaneEffect::Opened { def, placement } => {
            set(&value, "action", &JsValue::from_str("opened"));
            set(
                &value,
                "key",
                &JsValue::from_f64(f64::from(def.key.as_u32())),
            );
            set(&value, "name", &JsValue::from_str(&def.name));
            set(&value, "hidden", &JsValue::from_bool(def.hidden));
            match placement {
                PanePlacement::Split {
                    reference,
                    direction,
                    size_px,
                } => {
                    set(&value, "placement", &JsValue::from_str("split"));
                    set(
                        &value,
                        "reference",
                        &JsValue::from_f64(f64::from(reference.as_u32())),
                    );
                    set(
                        &value,
                        "direction",
                        &JsValue::from_str(match direction {
                            SplitDirection::Left => "left",
                            SplitDirection::Right => "right",
                            SplitDirection::Top => "top",
                            SplitDirection::Bottom => "bottom",
                        }),
                    );
                    if let Some(size) = size_px {
                        set(&value, "size", &JsValue::from_f64(f64::from(size)));
                    }
                }
                PanePlacement::Tab {
                    reference,
                    selected,
                    ..
                } => {
                    set(&value, "placement", &JsValue::from_str("tab"));
                    set(
                        &value,
                        "reference",
                        &JsValue::from_f64(f64::from(reference.as_u32())),
                    );
                    set(&value, "selected", &JsValue::from_bool(selected));
                }
            }
        }
        PaneEffect::Updated(def) => {
            set(&value, "action", &JsValue::from_str("updated"));
            set(
                &value,
                "key",
                &JsValue::from_f64(f64::from(def.key.as_u32())),
            );
            set(&value, "hidden", &JsValue::from_bool(def.hidden));
        }
        PaneEffect::Echo { key, text } => {
            set(&value, "action", &JsValue::from_str("echo"));
            set(&value, "key", &JsValue::from_f64(f64::from(key.as_u32())));
            set(&value, "text", &JsValue::from_str(&text));
        }
        PaneEffect::Clear(key) => {
            set(&value, "action", &JsValue::from_str("clear"));
            set(&value, "key", &JsValue::from_f64(f64::from(key.as_u32())));
        }
        PaneEffect::Closed(key) => {
            set(&value, "action", &JsValue::from_str("closed"));
            set(&value, "key", &JsValue::from_f64(f64::from(key.as_u32())));
        }
    }
    let global: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let _ = global.post_message(&value);
}

fn set(object: &Object, name: &str, value: &JsValue) {
    let _ = Reflect::set(object, &JsValue::from_str(name), value);
}

fn get_string(value: &JsValue, name: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_string())
}

fn get_redactions(message: &JsValue) -> Result<Vec<String>, String> {
    get_string(message, "send_on_connect_redactions").map_or_else(
        || Ok(Vec::new()),
        |json| {
            serde_json::from_str(&json)
                .map_err(|error| format!("invalid startup redactions: {error}"))
        },
    )
}

fn get_command_syntax(message: &JsValue) -> Result<Option<CommandSyntax>, String> {
    get_string(message, "command_syntax")
        .map(|json| {
            serde_json::from_str::<CommandSyntax>(&json)
                .map_err(|error| format!("invalid command syntax: {error}"))?
                .validate()
                .map_err(str::to_owned)
        })
        .transpose()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn get_u16(value: &JsValue, name: &str) -> Option<u16> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && *value >= 1.0 && *value <= f64::from(u16::MAX))
        .and_then(|value| u16::try_from(value as u64).ok())
}

// The bounds are at most ten million, so every accepted value is exactly
// representable in f64 and usize on both wasm32 and native test hosts.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn get_scrollback_lines(message: &JsValue) -> Result<Option<usize>, String> {
    let value = Reflect::get(message, &JsValue::from_str("max_lines"))
        .map_err(|_| "scrollback limit could not be read".to_owned())?;
    if value.is_undefined() {
        return Ok(None);
    }
    let number = value
        .as_f64()
        .filter(|number| {
            number.is_finite()
                && number.fract() == 0.0
                && *number >= MIN_SCROLLBACK_LINES as f64
                && *number <= MAX_SCROLLBACK_LINES as f64
        })
        .ok_or_else(|| "scrollback limit is outside the supported range".to_owned())?;
    Ok(Some(number as usize))
}
