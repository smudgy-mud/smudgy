//! Platform-neutral Smudgy session engine.
//!
//! Hosts deliver typed events and execute returned effects. This crate owns no
//! socket, async runtime, filesystem, browser API, native window, or script
//! engine.

mod automation;
mod telnet;
mod terminal;

pub use automation::{
    Alias, AutomationEffect, AutomationHost, InputOutcome, NoAutomation, PaneEffect,
    PlaintextAutomation, Trigger,
};
pub use terminal::{
    DEFAULT_SCROLLBACK_LINES, TerminalBuffer, TerminalDelta, TerminalDeltaCursor, TerminalDocument,
    TerminalLine,
};

use encoding_rs::{Encoding, UTF_8};
use smudgy_session_model::input_policy::{CommandSyntax, REDACTION_MASK, redact, split_commands};
use telnet::{DisplayEvent, TelnetBridge};

// Native's alias processor stops expansion after 100 nested matches. Keep
// scripted sends on that same bounded path instead of sending them verbatim.
const MAX_ALIAS_DEPTH: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

/// Facts that a platform shell can deliver to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    ConnectRequested {
        endpoint: String,
        send_on_connect: String,
        send_on_connect_redactions: Vec<String>,
    },
    DisconnectRequested,
    TransportOpened,
    TransportBytes(Vec<u8>),
    TransportClosed {
        code: u16,
        reason: String,
    },
    TransportFailed(String),
    UserInput {
        text: String,
        masked: bool,
    },
    Resize {
        columns: u16,
        rows: u16,
    },
}

/// Side effects requested by the portable engine and performed by a shell.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEffect {
    OpenTransport(String),
    CloseTransport,
    /// A Telnet protocol reply, not a user command.
    SendNetwork(Vec<u8>),
    /// Send the bytes, then echo the already-redacted `display` only if the
    /// transport accepted them.
    SendCommand {
        bytes: Vec<u8>,
        display: String,
    },
    Pane(PaneEffect),
}

pub struct ClientEngine<A> {
    automation: A,
    telnet: TelnetBridge,
    terminal: TerminalDocument,
    connection: ConnectionState,
    status: String,
    server_echo: bool,
    gmcp_messages: u64,
    pending_send_on_connect: Option<(String, Vec<String>)>,
    columns: u16,
    rows: u16,
    encoding: &'static Encoding,
    command_syntax: CommandSyntax,
}

impl<A: AutomationHost> ClientEngine<A> {
    /// Drain mutations emitted while the host constructed its script scope.
    pub fn start_automation(&mut self) -> Vec<ClientEffect> {
        let effects = self.automation.on_start();
        self.apply_automation(effects)
    }

    #[must_use]
    pub fn new(automation: A, columns: u16, rows: u16) -> Self {
        Self::with_encoding(automation, columns, rows, UTF_8)
    }

    #[must_use]
    pub fn with_encoding(
        automation: A,
        columns: u16,
        rows: u16,
        encoding: &'static Encoding,
    ) -> Self {
        Self::with_terminal_and_encoding(
            automation,
            columns,
            rows,
            TerminalBuffer::default(),
            encoding,
        )
    }

    /// Restores a session around an existing presentation buffer. Browser
    /// session workers use this seam to rebuild runtime state without making
    /// the UI discard a large transcript.
    #[must_use]
    pub fn with_terminal(automation: A, columns: u16, rows: u16, terminal: TerminalBuffer) -> Self {
        Self::with_terminal_and_encoding(automation, columns, rows, terminal, UTF_8)
    }

    #[must_use]
    pub fn with_terminal_and_encoding(
        automation: A,
        columns: u16,
        rows: u16,
        terminal: TerminalBuffer,
        encoding: &'static Encoding,
    ) -> Self {
        Self {
            automation,
            telnet: TelnetBridge::with_encoding(columns, rows, encoding),
            terminal,
            connection: ConnectionState::Disconnected,
            status: "Ready".into(),
            server_echo: false,
            gmcp_messages: 0,
            pending_send_on_connect: None,
            columns,
            rows,
            encoding,
            command_syntax: CommandSyntax::default(),
        }
    }

    pub fn handle(&mut self, event: ClientEvent) -> Vec<ClientEffect> {
        match event {
            ClientEvent::ConnectRequested {
                endpoint,
                send_on_connect,
                send_on_connect_redactions,
            } => {
                if self.connection != ConnectionState::Disconnected {
                    return Vec::new();
                }
                self.telnet = TelnetBridge::with_encoding(self.columns, self.rows, self.encoding);
                self.terminal.begin_new_connection();
                self.connection = ConnectionState::Connecting;
                self.server_echo = false;
                self.gmcp_messages = 0;
                self.pending_send_on_connect = (!send_on_connect.is_empty())
                    .then_some((send_on_connect, send_on_connect_redactions));
                self.status = format!("Connecting to {endpoint}…");
                vec![ClientEffect::OpenTransport(endpoint)]
            }
            ClientEvent::DisconnectRequested => {
                self.connection = ConnectionState::Disconnected;
                self.pending_send_on_connect = None;
                self.status = "Disconnected".into();
                vec![ClientEffect::CloseTransport]
            }
            ClientEvent::TransportOpened => {
                self.connection = ConnectionState::Connected;
                self.status = "Connected; negotiating Telnet…".into();
                Vec::new()
            }
            ClientEvent::TransportBytes(bytes) => self.on_network_bytes(&bytes),
            ClientEvent::TransportClosed { code, reason } => {
                self.connection = ConnectionState::Disconnected;
                self.pending_send_on_connect = None;
                self.status = if reason.is_empty() {
                    format!("WebSocket closed ({code})")
                } else {
                    format!("WebSocket closed ({code}): {reason}")
                };
                Vec::new()
            }
            ClientEvent::TransportFailed(error) => {
                self.connection = ConnectionState::Disconnected;
                self.pending_send_on_connect = None;
                self.status = error;
                vec![ClientEffect::CloseTransport]
            }
            ClientEvent::UserInput { text, masked } => self.on_submitted_input(&text, masked),
            ClientEvent::Resize { columns, rows } => {
                self.columns = columns;
                self.rows = rows;
                self.telnet
                    .resize(columns, rows)
                    .map(ClientEffect::SendNetwork)
                    .into_iter()
                    .collect()
            }
        }
    }

    fn on_network_bytes(&mut self, bytes: &[u8]) -> Vec<ClientEffect> {
        let output = match self.telnet.feed(bytes) {
            Ok(output) => output,
            Err(error) => return self.handle(ClientEvent::TransportFailed(error)),
        };
        if let Some(server_echo) = output.server_echo {
            self.server_echo = server_echo;
        }
        self.gmcp_messages += output.gmcp_messages;

        let mut effects: Vec<_> = output
            .replies
            .into_iter()
            .map(ClientEffect::SendNetwork)
            .collect();
        let mut has_displayable_text = false;
        for display in output.display {
            let lines = match display {
                DisplayEvent::Data(bytes) => {
                    let (lines, displayable) = self.terminal.feed_with_displayable_marker(&bytes);
                    has_displayable_text |= displayable;
                    lines
                }
                DisplayEvent::Prompt => self.terminal.commit_prompt(),
            };
            self.status = "Connected".into();
            for line in lines {
                let automation = self.automation.on_output_line(&line);
                effects.extend(self.apply_automation(automation));
            }
        }
        if self.connection == ConnectionState::Connected
            && has_displayable_text
            && let Some((commands, redactions)) = self.pending_send_on_connect.take()
        {
            if redactions.is_empty() {
                self.dispatch_outgoing_input(&commands, 0, &mut effects);
            } else {
                for command in commands.split('\n') {
                    // Native SendWithRedactions sends the expanded startup
                    // text verbatim: aliases and separators must not inspect
                    // a password before its visible copy is masked.
                    let display = redact(command, &redactions);
                    self.send_line(command, &display, &mut effects);
                }
            }
        }
        effects
    }

    fn on_submitted_input(&mut self, input: &str, masked: bool) -> Vec<ClientEffect> {
        if self.connection != ConnectionState::Connected {
            return Vec::new();
        }
        if masked {
            // Native SendWithRedactions skips sys:input, aliases, and command
            // separators. Do not pass passwords to automation callbacks.
            let mut effects = Vec::new();
            for line in input.split('\n') {
                self.send_line(line, REDACTION_MASK, &mut effects);
            }
            return effects;
        }
        let intercept = self.automation.on_submitted_input(input);
        let mut effects = self.apply_automation(intercept.effects);
        if !intercept.handled {
            effects.extend(self.on_outgoing_input(input));
        }
        effects
    }

    fn on_outgoing_input(&mut self, input: &str) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        self.dispatch_outgoing_input(input, 0, &mut effects);
        effects
    }

    fn dispatch_outgoing_input(
        &mut self,
        input: &str,
        depth: usize,
        effects: &mut Vec<ClientEffect>,
    ) {
        if self.connection != ConnectionState::Connected {
            return;
        }
        if !self.command_syntax.raw_prefix.is_empty()
            && let Some(rest) = input.strip_prefix(&self.command_syntax.raw_prefix)
        {
            for line in rest.split('\n') {
                self.send_line(line, line, effects);
            }
            return;
        }
        if let Some(rest) = input.strip_prefix('=') {
            self.route_alias_or_send(rest, depth, effects);
            return;
        }
        for line in split_commands(input, &self.command_syntax.separator) {
            self.route_alias_or_send(line, depth, effects);
        }
    }

    fn route_alias_or_send(&mut self, input: &str, depth: usize, effects: &mut Vec<ClientEffect>) {
        if depth > MAX_ALIAS_DEPTH {
            self.terminal.push_local_line(
                "Script processor bailing, depth limit reached. Do you have an alias that triggers itself?",
            );
            return;
        }
        let outcome = self.automation.on_user_input(input);
        let handled = outcome.handled;
        self.apply_automation_at_depth(outcome.effects, depth + 1, effects);
        if !handled {
            self.send_line(input, input, effects);
        }
    }

    fn apply_automation(&mut self, automation: Vec<AutomationEffect>) -> Vec<ClientEffect> {
        let mut effects = Vec::new();
        self.apply_automation_at_depth(automation, 0, &mut effects);
        effects
    }

    fn apply_automation_at_depth(
        &mut self,
        automation: Vec<AutomationEffect>,
        depth: usize,
        effects: &mut Vec<ClientEffect>,
    ) {
        for effect in automation {
            match effect {
                AutomationEffect::Send(command) => {
                    self.dispatch_outgoing_input(&command, depth, effects);
                }
                AutomationEffect::Display(line) => self.terminal.push_local_line(line),
                AutomationEffect::Pane(pane) => effects.push(ClientEffect::Pane(pane)),
            }
        }
    }

    fn send_line(&mut self, line: &str, display: &str, effects: &mut Vec<ClientEffect>) {
        match self.telnet.encode_line(line) {
            Ok(bytes) => effects.push(ClientEffect::SendCommand {
                bytes,
                display: display.to_owned(),
            }),
            Err(error) => self.terminal.push_local_line(error),
        }
    }

    #[must_use]
    pub const fn connection(&self) -> ConnectionState {
        self.connection
    }

    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[must_use]
    pub const fn server_echo(&self) -> bool {
        self.server_echo
    }

    #[must_use]
    pub const fn gmcp_messages(&self) -> u64 {
        self.gmcp_messages
    }

    #[must_use]
    pub const fn terminal(&self) -> &TerminalDocument {
        &self.terminal
    }

    /// Called by a host only after it successfully executes `SendCommand`.
    pub fn command_sent(&mut self, display: &str) {
        self.terminal.echo_sent_command(display);
    }

    /// Change the worker transcript cap. Hosts send a presentation update
    /// after this so the window's buffer adopts the same limit.
    pub fn set_scrollback_lines(&mut self, max_lines: usize) {
        self.terminal.set_max_lines(max_lines);
    }

    pub fn set_command_syntax(&mut self, syntax: CommandSyntax) {
        self.command_syntax = syntax;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_protocol::telnet::command::{GA, IAC, WILL};
    use smudgy_protocol::telnet::option;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn sent(line: &str) -> ClientEffect {
        ClientEffect::SendCommand {
            bytes: format!("{line}\r\n").into_bytes(),
            display: line.to_owned(),
        }
    }

    fn redacted_send(line: &str, display: &str) -> ClientEffect {
        ClientEffect::SendCommand {
            bytes: format!("{line}\r\n").into_bytes(),
            display: display.to_owned(),
        }
    }

    struct RecordingAutomation(Rc<RefCell<Vec<String>>>);

    impl AutomationHost for RecordingAutomation {
        fn on_submitted_input(&mut self, input: &str) -> InputOutcome {
            self.0.borrow_mut().push(format!("submit:{input}"));
            InputOutcome {
                handled: false,
                effects: vec![AutomationEffect::Send("intercept".into())],
            }
        }

        fn on_user_input(&mut self, input: &str) -> InputOutcome {
            self.0.borrow_mut().push(format!("alias:{input}"));
            if input == "go" {
                InputOutcome {
                    handled: true,
                    effects: vec![AutomationEffect::Send("aliased".into())],
                }
            } else {
                InputOutcome::default()
            }
        }

        fn on_output_line(&mut self, _line: &str) -> Vec<AutomationEffect> {
            Vec::new()
        }
    }

    #[test]
    fn only_unmasked_typed_input_enters_submission_handlers() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut engine = ClientEngine::new(RecordingAutomation(Rc::clone(&calls)), 80, 24);
        engine.handle(ClientEvent::TransportOpened);
        assert_eq!(
            engine.handle(ClientEvent::UserInput {
                text: "go".into(),
                masked: false,
            }),
            [sent("intercept"), sent("aliased")]
        );
        assert_eq!(
            calls.borrow().as_slice(),
            ["submit:go", "alias:intercept", "alias:go", "alias:aliased"]
        );

        assert_eq!(
            engine.handle(ClientEvent::UserInput {
                text: "secret;literal".into(),
                masked: true,
            }),
            [ClientEffect::SendCommand {
                bytes: b"secret;literal\r\n".to_vec(),
                display: "********".into(),
            }]
        );
        assert_eq!(
            calls.borrow().as_slice(),
            ["submit:go", "alias:intercept", "alias:go", "alias:aliased"]
        );
    }

    #[test]
    fn deferred_profile_send_enters_aliases_but_not_submission_handlers() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut engine = ClientEngine::new(RecordingAutomation(Rc::clone(&calls)), 80, 24);
        engine.handle(ClientEvent::ConnectRequested {
            endpoint: "wss://example.test/ws".into(),
            send_on_connect: "go".into(),
            send_on_connect_redactions: Vec::new(),
        });
        engine.handle(ClientEvent::TransportOpened);
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"Welcome\r\n".to_vec())),
            [sent("aliased")]
        );
        assert_eq!(calls.borrow().as_slice(), ["alias:go", "alias:aliased"]);
    }

    #[test]
    fn password_profile_send_is_verbatim_and_redacted_before_echo() {
        let mut automation = PlaintextAutomation::new();
        automation.add_alias(Alias::new(r"^login (.+)$", "leak $1").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::ConnectRequested {
            endpoint: "wss://example.test/ws".into(),
            send_on_connect: "login secret;literal\n".into(),
            send_on_connect_redactions: vec!["secret".into()],
        });
        engine.handle(ClientEvent::TransportOpened);

        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"Welcome\r\n".to_vec())),
            [
                redacted_send("login secret;literal", "login ********;literal"),
                redacted_send("", ""),
            ]
        );
    }

    #[test]
    fn outgoing_syntax_matches_native_separator_raw_prefix_and_equals_rules() {
        let mut engine = ClientEngine::new(NoAutomation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);
        let input = |text: &str| ClientEvent::UserInput {
            text: text.into(),
            masked: false,
        };
        assert_eq!(
            engine.handle(input("north;south\n")),
            [sent("north"), sent("south"), sent("")]
        );
        assert_eq!(
            engine.handle(input(r"\\north;south")),
            [sent("north;south")]
        );
        assert_eq!(engine.handle(input("=north;south")), [sent("north;south")]);
        engine.set_command_syntax(CommandSyntax {
            separator: ";;".into(),
            raw_prefix: "!".into(),
        });
        assert_eq!(
            engine.handle(input("north;south;;east")),
            [sent("north;south"), sent("east")]
        );
        assert_eq!(
            engine.handle(input("!north;;south")),
            [sent("north;;south")]
        );
    }
    #[test]
    fn alias_and_trigger_effects_cross_the_same_ordered_boundary() {
        let mut automation = PlaintextAutomation::new();
        automation.add_alias(Alias::new(r"^k (.+)$", "kill $1").unwrap());
        automation.add_trigger(Trigger::new(r"^HP: (\d+)$", "say hp=$1").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);

        assert_eq!(
            engine.handle(ClientEvent::UserInput {
                text: "k rat".into(),
                masked: false,
            }),
            [sent("kill rat")]
        );
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"HP: 7\n".to_vec())),
            [sent("say hp=7")]
        );
    }

    #[test]
    fn automation_sends_reenter_syntax_and_aliases_without_submission_handlers() {
        let mut automation = PlaintextAutomation::new();
        automation.add_alias(Alias::new(r"^first$", "second;third").unwrap());
        automation.add_alias(Alias::new(r"^second$", "done").unwrap());
        automation.add_trigger(Trigger::new(r"^READY$", "first").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);

        let expected = [sent("done"), sent("third")];
        assert_eq!(
            engine.handle(ClientEvent::UserInput {
                text: "first".into(),
                masked: false,
            }),
            expected
        );
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"READY\r\n".to_vec())),
            expected
        );
    }

    #[test]
    fn recursive_alias_stops_at_native_depth_limit() {
        let mut automation = PlaintextAutomation::new();
        automation.add_alias(Alias::new(r"^loop$", "loop").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);

        assert!(
            engine
                .handle(ClientEvent::UserInput {
                    text: "loop".into(),
                    masked: false,
                })
                .is_empty()
        );
        assert!(
            engine
                .terminal
                .lines()
                .any(|line| line.text.contains("depth limit"))
        );
    }

    #[test]
    fn connect_is_an_effect_not_platform_io() {
        let mut engine = ClientEngine::new(NoAutomation, 80, 24);
        assert_eq!(
            engine.handle(ClientEvent::ConnectRequested {
                endpoint: "wss://example.test/telnet".into(),
                send_on_connect: String::new(),
                send_on_connect_redactions: Vec::new(),
            }),
            [ClientEffect::OpenTransport(
                "wss://example.test/telnet".into()
            )]
        );
        assert_eq!(engine.connection(), ConnectionState::Connecting);
    }

    #[test]
    fn transport_failure_closes_the_physical_socket() {
        let mut engine = ClientEngine::new(NoAutomation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);
        assert_eq!(
            engine.handle(ClientEvent::TransportFailed("non-binary frame".into())),
            [ClientEffect::CloseTransport]
        );
        assert_eq!(engine.connection(), ConnectionState::Disconnected);
        assert_eq!(engine.status(), "non-binary frame");
    }

    #[test]
    fn profile_send_requires_fresh_displayable_network_text() {
        let mut automation = PlaintextAutomation::new();
        automation.add_trigger(Trigger::new(r"^Welcome", "look").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::ConnectRequested {
            endpoint: "wss://example.test/telnet".into(),
            send_on_connect: "login test\n\npassword secret".into(),
            send_on_connect_redactions: Vec::new(),
        });
        engine.handle(ClientEvent::TransportOpened);

        let negotiation = engine.handle(ClientEvent::TransportBytes(vec![IAC, WILL, option::ECHO]));
        assert!(!negotiation.iter().any(|effect| matches!(
            effect,
            ClientEffect::SendCommand { bytes, .. } if bytes.starts_with(b"login")
        )));
        assert!(
            engine
                .handle(ClientEvent::TransportBytes(b"\x1b[31m".to_vec()))
                .is_empty()
        );
        engine.terminal.push_local_line("local text");
        assert!(
            engine
                .handle(ClientEvent::TransportBytes(Vec::new()))
                .is_empty()
        );

        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"Welcome\r\n".to_vec())),
            [
                sent("look"),
                sent("login test"),
                sent(""),
                sent("password secret")
            ]
        );
        assert!(
            engine
                .handle(ClientEvent::TransportBytes(b"Another line\r\n".to_vec()))
                .is_empty()
        );
    }

    #[test]
    fn reconnect_does_not_release_profile_send_for_old_prompt() {
        let mut engine = ClientEngine::new(NoAutomation, 80, 24);
        let connect = || ClientEvent::ConnectRequested {
            endpoint: "wss://example.test/telnet".into(),
            send_on_connect: "login test".into(),
            send_on_connect_redactions: Vec::new(),
        };
        engine.handle(connect());
        engine.handle(ClientEvent::TransportOpened);
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"Old prompt: ".to_vec())),
            [sent("login test")]
        );
        engine.handle(ClientEvent::DisconnectRequested);
        engine.handle(connect());
        engine.handle(ClientEvent::TransportOpened);
        assert!(
            engine
                .handle(ClientEvent::TransportBytes(Vec::new()))
                .is_empty()
        );
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(b"New prompt: ".to_vec())),
            [sent("login test")]
        );
    }

    #[test]
    fn prompt_boundary_runs_plaintext_trigger_synchronously() {
        let mut automation = PlaintextAutomation::new();
        automation.add_trigger(Trigger::new(r"^HP: (\d+)$", "say hp=$1").unwrap());
        let mut engine = ClientEngine::new(automation, 80, 24);
        engine.handle(ClientEvent::TransportOpened);
        assert_eq!(
            engine.handle(ClientEvent::TransportBytes(
                [&b"HP: 7"[..], &[IAC, GA]].concat()
            )),
            [sent("say hp=7")]
        );
        assert_eq!(engine.terminal().committed_len(), 0);
        assert_eq!(engine.terminal().current_line().text, "HP: 7");
        engine.command_sent("say hp=7");
        assert_eq!(
            engine.terminal().lines().next().unwrap().text,
            "HP: 7say hp=7"
        );
        engine.handle(ClientEvent::TransportBytes(b"after\r\n".to_vec()));
        assert_eq!(
            engine
                .terminal()
                .lines()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["HP: 7say hp=7", "after"]
        );
    }
}
