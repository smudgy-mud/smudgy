//! Action dispatch: how each [`RuntimeAction`] is actually handled.

use std::ops::Add;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::bail;
use futures::SinkExt;

use crate::models::ScriptLang;
use crate::session::connection::Connection;
use crate::session::ui_command::{PaneCommand, UiCommand};
use crate::session::{BufferUpdate, SessionEvent, TaggedSessionEvent};

use super::pane::{MAIN_PANE_KEY, PaneError, PaneKey, PaneKind, PaneNamespace};
use super::trigger::{self, PushTriggerParams};
use super::{ActionResult, Inner, IsolateId, RuntimeAction, ScriptAction};
use crate::session::styled_line::StyledLine;

/// Forward a lazy tooltip request while preserving a terminal failure path.
/// A runtime's receiver is dropped just before its registry entry is removed,
/// so a successful lookup does not guarantee that the send will succeed.
fn send_link_tooltip(
    tx: &tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
    action: RuntimeAction,
) -> bool {
    match tx.send(action) {
        Ok(()) => true,
        Err(error) => {
            let RuntimeAction::ResolveLinkTooltip { state, .. } = error.0 else {
                unreachable!("send_link_tooltip accepts only tooltip actions");
            };
            state.resolve(None);
            false
        }
    }
}

fn prepare_pane_open(
    registry: &super::SharedPaneRegistry,
    def: super::pane::PaneDef,
    placement: super::pane::PanePlacement,
    reconcile_registry: bool,
) -> Option<(super::pane::PaneDef, super::pane::PanePlacement)> {
    if !reconcile_registry {
        return Some((def, placement));
    }

    let registry = registry.lock().unwrap();
    let def = registry.get(def.key)?.clone();
    let reference = placement.reference();
    let placement = placement.with_reference(if registry.is_live(reference) {
        reference
    } else {
        super::pane::MAIN_PANE_KEY
    });
    Some((def, placement))
}

fn pane_closed_event(key: PaneKey, ui_command_published: bool) -> SessionEvent {
    if ui_command_published {
        SessionEvent::PaneClosedOrdered(key)
    } else {
        SessionEvent::PaneClosed(key)
    }
}

impl Inner<'_> {
    /// Deliver a host-native (`sys:`/`map:`) event, returning an `ActionResult::Run` that splices the
    /// subscriber calls depth-first (or `None` when nobody is listening). (See `PACKAGE-EVENTS.md`.)
    fn run_host_event(&self, event: &str, payload_json: &str) -> ActionResult {
        let actions = self.script_engine.host_emit(event, payload_json);
        if actions.is_empty() {
            ActionResult::None
        } else {
            ActionResult::Run(actions)
        }
    }

    /// Write one pre-framed GMCP subnegotiation on the live connection (the binary write
    /// path, ordered with normal sends by the shared socket channel). An empty frame is
    /// the registry's "nothing to send"; a missing connection can only be a race with a
    /// drop — logged, never fatal.
    async fn write_gmcp_frame(&self, frame: Vec<u8>) {
        if frame.is_empty() {
            return;
        }
        match self.connection.as_ref() {
            Some(connection) => {
                if let Err(err) = connection.write_raw(Arc::from(frame)).await {
                    warn!("GMCP frame dropped: {err:?}");
                }
            }
            None => warn!("GMCP frame dropped: no live connection"),
        }
    }

    /// Queue GMCP session notices as echo actions (depth-first, after the current action).
    fn queue_gmcp_echoes(&self, echoes: Vec<String>) {
        let mut spawned = self.spawned_actions.borrow_mut();
        for line in echoes {
            spawned.push_back(RuntimeAction::Echo(Arc::new(line)));
        }
    }

    /// Emit a host event only when someone is subscribed, building the payload only
    /// then. The subscriber gate MUST run before payload construction — `sys:receive`
    /// rides the per-line hot path and `sys:input` every typed submission, so the
    /// common no-listener case pays neither the payload build nor a catalogue sample.
    /// That gate-then-build invariant lives here, once.
    fn gated_host_emit(&self, event: &str, payload: impl FnOnce() -> String) -> Vec<RuntimeAction> {
        if self.script_engine.has_event_subscribers(event) {
            self.script_engine.host_emit(event, &payload())
        } else {
            Vec::new()
        }
    }

    /// The `pane:visibility` host event for one actual eyeball-state change —
    /// fired on every toggle whatever the spelling (spec restatement,
    /// `hide()`/`show()`, cross-session, the user's eyeball click). Like
    /// `input:change`'s `pane` field, the payload carries the display-cased
    /// name; subscribers resolve it in their own namespace.
    fn pane_visibility_emit(&self, def: &super::pane::PaneDef) -> Vec<RuntimeAction> {
        self.gated_host_emit("pane:visibility", || {
            serde_json::json!({ "pane": def.name.as_ref(), "hidden": def.hidden }).to_string()
        })
    }

    /// Send `text` to the wire verbatim — '\n' splits, nothing else does, and no
    /// alias matching — then flush the buffered display updates (the echoed copy)
    /// to the UI. The shared tail of every raw-send arm: the raw-prefix branch of
    /// [`Self::dispatch_send`], `SendRaw`, and `SendRawUnless`.
    async fn send_verbatim_lines(&mut self, text: &str) -> Result<(), anyhow::Error> {
        for line in text.split('\n') {
            self.send(line).await?;
        }
        if let Some(fut) = self.flush_buffer_updates()? {
            fut.await?;
        }
        Ok(())
    }

    /// Resolve a pane-delivery target to `(key, kind, is_main)`. Own-session ops carry
    /// the key they resolved synchronously at call time, so a delivery issued before a
    /// `close()` in the same script body still lands on that incarnation (the UI still
    /// holds the pane — `PaneClosed` trails the delivery on the ordered channel).
    /// Cross-session actions carry no key and resolve by name on this owning runtime,
    /// which also reattaches to a recreated same-name pane. `None` = unknown pane.
    fn resolve_pane_target(
        &self,
        key: Option<PaneKey>,
        namespace: &PaneNamespace,
        name: &str,
    ) -> Option<(PaneKey, PaneKind, bool)> {
        match key {
            Some(key) => Some((key, PaneKind::Terminal, key == MAIN_PANE_KEY)),
            None => self
                .pane_registry
                .lock()
                .unwrap()
                .resolve(namespace, name)
                .map(|def| (def.key, def.kind, def.is_main)),
        }
    }

    /// The outgoing-line pipeline entry shared by `Send` and the typed-submission
    /// actions: the raw-line prefix sends the remainder verbatim — no separator
    /// splitting AND no alias matching — exactly like `RuntimeAction::SendRaw`
    /// ('\n' still splits). It is checked before the legacy `=` prefix, which skips
    /// splitting but still alias-matches. Because the check lives here, script
    /// `send("\\...")` inherits raw behavior by design — and a `sys:input`
    /// replacement does too, since a submission completes into this same entry.
    async fn dispatch_send(&mut self, line: Arc<String>) -> Result<ActionResult, anyhow::Error> {
        if !self.raw_line_prefix.is_empty()
            && let Some(rest) = line.strip_prefix(self.raw_line_prefix.as_str())
        {
            self.send_verbatim_lines(rest).await?;
            Ok(ActionResult::None)
        } else if let Some(rest) = line.strip_prefix('=') {
            match self.trigger_manager.process_outgoing_line(rest) {
                Ok(()) => Ok(ActionResult::None),
                Err(err) => Ok(ActionResult::Echo(format!(
                    "Error processing command {err:?}"
                ))),
            }
        } else {
            Ok(ActionResult::Run(
                trigger::split_commands(&line, &self.command_separator)
                    .into_iter()
                    .map(|line| RuntimeAction::ProcessOutgoingLine(Arc::new(line.to_string())))
                    .collect(),
            ))
        }
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn handle_action(
        &mut self,
        action: RuntimeAction,
    ) -> Result<ActionResult, anyhow::Error> {
        match action {
            RuntimeAction::RemoteStoreFlushed {
                source,
                published,
                writes,
            } => {
                let (actions, bindings_changed) = self
                    .script_engine
                    .remote_store_flushed(source, &published, &writes);
                if bindings_changed
                    && let Err(error) = self.ui_tx.try_send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::StoreBindingsChanged,
                    })
                    && !error.is_full()
                {
                    warn!("Failed to send directed store-bindings wake: {error:?}");
                }
                Ok(ActionResult::Run(actions))
            }
            RuntimeAction::FanOutInteropEvent {
                canonical,
                stamped,
                payload,
                source,
                depth,
            } => {
                let mut local = Vec::new();
                for runtime in
                    crate::session::registry::get_runtimes_for_server(self.server_name.as_str())
                {
                    let action = RuntimeAction::InteropEvent {
                        canonical: Arc::clone(&canonical),
                        stamped: Arc::clone(&stamped),
                        payload: Arc::clone(&payload),
                        source: source.clone(),
                        depth,
                    };
                    if runtime.session_id == self.session_id {
                        local.push(action);
                    } else if runtime.tx.send(action).is_err() {
                        warn!("Dropping interop event for session {}", runtime.session_id);
                    }
                }
                Ok(ActionResult::Run(local))
            }
            RuntimeAction::InteropEvent {
                canonical,
                stamped,
                payload,
                source,
                depth,
            } => {
                let mut actions = Vec::new();
                if canonical.as_ref() == "sessions:destroyed" {
                    let (invalidation, bindings_changed) =
                        self.script_engine.remote_session_destroyed(source.id);
                    actions.extend(invalidation);
                    if bindings_changed
                        && let Err(error) = self.ui_tx.try_send(TaggedSessionEvent {
                            session_id: self.session_id,
                            event: SessionEvent::StoreBindingsChanged,
                        })
                        && !error.is_full()
                    {
                        warn!("Failed to send destroyed-session binding wake: {error:?}");
                    }
                }
                actions.extend(
                    self.script_engine
                        .deliver_interop_event(&canonical, &stamped, &payload, &source, depth),
                );
                Ok(ActionResult::Run(actions))
            }
            RuntimeAction::ProcedurePost {
                canonical,
                producer,
                name,
                payload,
                caller_origin,
                caller_session,
                depth,
            } => Ok(ActionResult::Run(
                self.script_engine.deliver_procedure_post(
                    canonical,
                    producer,
                    name,
                    payload,
                    caller_origin,
                    &caller_session,
                    depth,
                ),
            )),
            RuntimeAction::ForwardProcedurePost {
                target,
                canonical,
                producer,
                name,
                payload,
                caller_origin,
                caller_session,
                depth,
            } => {
                let Some(runtime) = crate::session::registry::get_runtime(target) else {
                    return Ok(ActionResult::None);
                };
                if runtime.server_name.as_str() != self.server_name.as_str() {
                    warn!("Dropping cross-server procedure post for session {target}");
                    return Ok(ActionResult::None);
                }
                if runtime
                    .tx
                    .send(RuntimeAction::ProcedurePost {
                        canonical,
                        producer,
                        name,
                        payload,
                        caller_origin,
                        caller_session,
                        depth,
                    })
                    .is_err()
                {
                    warn!("Dropping procedure post for session {target}");
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::Connect {
                host,
                port,
                send_on_connect,
                send_on_connect_redactions,
                encoding,
                compression,
                tls,
            } => {
                self.connection_generation = self.connection_generation.wrapping_add(1);
                let connection_generation = self.connection_generation;
                self.pending_send_on_connect = send_on_connect.map(|text| {
                    // When the auto-login text carries secrets (a substituted
                    // $PASSWORD), send it with redactions so it reaches the wire
                    // but is masked in the client view + log; otherwise keep the
                    // ordinary Send path (alias matching / separator splitting).
                    if send_on_connect_redactions.is_empty() {
                        RuntimeAction::Send(text)
                    } else {
                        RuntimeAction::SendWithRedactions {
                            text,
                            redactions: send_on_connect_redactions,
                        }
                    }
                });
                // Arm the packet-marker gate before the socket task spawns:
                // the new connection's VtProcessor reads this cell, and the
                // marker's whole purpose is releasing the pending send.
                self.send_on_connect_armed.store(
                    self.pending_send_on_connect.is_some(),
                    std::sync::atomic::Ordering::Relaxed,
                );

                let mut connection = Connection::with_generation(
                    self.session_runtime_tx.clone(),
                    self.ui_tx.clone(),
                    self.trigger_manager.raw_wanted_flag(),
                    self.window_size.clone(),
                    connection_generation,
                );
                connection.set_marker_armed_flag(self.send_on_connect_armed.clone());

                // Resolve the configured encoding label; an unresolvable one falls back
                // to UTF-8 loudly — in the session view, not just the log, since the
                // symptom (mojibake) gives no hint of the cause. `no_replacement`: the
                // WHATWG mapping sends ISO-2022/HZ labels to the replacement encoding,
                // which would collapse the whole feed to U+FFFD; treat those as unknown.
                let encoding = encoding.as_ref().and_then(|label| {
                    let resolved =
                        encoding_rs::Encoding::for_label_no_replacement(label.as_bytes());
                    if resolved.is_none() {
                        warn!("Unknown encoding label {label:?} for this server; using UTF-8");
                        self.session_runtime_tx
                            .send(RuntimeAction::Echo(Arc::new(format!(
                                "Unknown encoding \"{label}\" configured for this server; using UTF-8."
                            ))))
                            .ok();
                    }
                    resolved
                });

                // Raw logging is decided per connection: load settings fresh
                // so toggling `log_raw` applies to the next connect.
                let raw_log_path = if crate::models::settings::load_settings().logging.log_raw {
                    match crate::get_smudgy_home() {
                        Ok(home) => Some(home.join(self.server_name.as_str()).join("logs").join(
                            format!(
                                "{}-{}.raw.log",
                                self.profile_name,
                                chrono::Local::now().format("%Y-%m-%d_%H-%M-%S%.3f")
                            ),
                        )),
                        Err(err) => {
                            warn!("Failed to resolve smudgy home for the raw log: {err:?}");
                            None
                        }
                    }
                } else {
                    None
                };

                connection.connect(
                    host.as_str(),
                    port,
                    raw_log_path,
                    encoding,
                    compression,
                    tls,
                );

                self.connection = Some(connection);
                Ok(ActionResult::None)
            }
            RuntimeAction::Disconnect => {
                // Signal the socket task to stop; it emits `Disconnected` on its
                // way out (the same path an unexpected drop takes). Keeping the
                // `Connection` around is harmless — a later `Connect` replaces it.
                if let Some(connection) = self.connection.as_mut() {
                    connection.disconnect();
                }
                self.pending_send_on_connect = None;
                self.send_on_connect_armed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Ok(ActionResult::None)
            }
            RuntimeAction::HandleIncomingLine(line) => {
                self.script_engine
                    .set_current_line(Some(Arc::downgrade(&line)));
                if let Err(err) = self.trigger_manager.process_incoming_line(&line) {
                    self.abort_incoming_line_sync();
                    return Ok(ActionResult::Echo(format!("Error processing line {err:?}")));
                }

                // `sys:receive` fires post-trigger but before `CompleteLineTriggersProcessed`
                // applies transforms and routes the line: depth-first drain runs the whole
                // trigger cascade, then these handlers, then `Complete`. So a subscriber sees
                // the original text (edits are deferred to `Complete`) and can `gag()`/
                // `redirect()`/`replace()` the ambient `line` before it appears, exactly like a
                // trigger. Gated (subscriber check before payload build) because this is the
                // hot per-line path.
                let sys_receive = self.gated_host_emit("sys:receive", || {
                    serde_json::json!({ "text": &**line }).to_string()
                });
                {
                    let mut spawned = self.spawned_actions.borrow_mut();
                    spawned.extend(sys_receive);
                    spawned.push_back(RuntimeAction::CompleteLineTriggersProcessed(line));
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::HandleIncomingPartialLine(line) => {
                self.script_engine
                    .set_current_line(Some(Arc::downgrade(&line)));
                match self.trigger_manager.process_partial_line(line) {
                    Ok(()) => Ok(ActionResult::None),
                    Err(err) => {
                        self.abort_incoming_line_sync();
                        Ok(ActionResult::Echo(format!(
                            "Error processing partial line {err:?}"
                        )))
                    }
                }
            }
            RuntimeAction::PromptBoundary => {
                self.pending_buffer_updates
                    .push(BufferUpdate::PromptBoundary);
                Ok(ActionResult::None)
            }
            RuntimeAction::RetractIncomingPartialLine => {
                self.retract_incoming_open_line_sync();
                Ok(ActionResult::None)
            }
            RuntimeAction::RequestRepaint => {
                if let Some(fut) = self.flush_buffer_updates()? {
                    fut.await?;
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::IncomingPacketProcessed {
                connection_generation,
                has_displayable_text,
            } => {
                if connection_generation != self.connection_generation || !has_displayable_text {
                    return Ok(ActionResult::None);
                }
                // Released (or nothing was pending): disarm so the socket
                // task stops emitting markers for the rest of the connection.
                self.send_on_connect_armed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                match self.pending_send_on_connect.take() {
                    Some(action) => Ok(ActionResult::Run(vec![action])),
                    None => Ok(ActionResult::None),
                }
            }
            RuntimeAction::LinkTooltipChanged => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::LinkTooltipChanged,
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            // Echo arms append WITHOUT flushing: delivery rides the run loop's
            // coalescing points (the storm threshold and the before-park flush),
            // so an echo storm reaches the UI as a few batched events instead of
            // one event per call. The ingest path already works this way.
            RuntimeAction::Echo(line) => {
                self.echo_str_sync(line.as_str());
                Ok(ActionResult::None)
            }
            RuntimeAction::EchoStyled(lines) => {
                self.echo_styled_lines_sync(&lines);
                Ok(ActionResult::None)
            }
            RuntimeAction::CompleteLineTriggersProcessed(line) => {
                // Transforms first (always applied, even to gagged/redirected lines),
                // then the per-line routing state decides the sink set.
                self.script_engine.set_current_line(None);
                let processed_line = self.apply_pending_line_operations(line);
                let routing = self.line_routing.borrow_mut().take();
                self.route_complete_line(processed_line, &routing);
                Ok(ActionResult::None)
            }
            RuntimeAction::PartialLineTriggersProcessed(line) => {
                self.script_engine.set_current_line(None);
                let processed_line = self.apply_pending_line_operations(line);
                let routing = self.line_routing.borrow_mut().take();
                self.route_partial_line(processed_line, &routing);
                Ok(ActionResult::None)
            }
            RuntimeAction::Send(line) => self.dispatch_send(line).await,
            RuntimeAction::SubmitInput(line) => {
                // The typed-submission pipeline entry: `sys:input` fires here, before
                // raw-prefix/`=` handling and separator splitting, so handlers see the
                // line exactly as submitted and can rewrite or cancel it. Only the UI's
                // input submit routing constructs this action — `session.send()` and
                // every other script/link route arrive as `Send`, and a masked
                // submission rides `SendWithRedactions` (the redaction path), so
                // neither ever reaches these handlers. Gated (subscriber check before
                // payload build) so the common no-listener submission is exactly a `Send`.
                let handlers = self.gated_host_emit("sys:input", || {
                    serde_json::json!({ "text": line.as_str() }).to_string()
                });
                if handlers.is_empty() {
                    self.dispatch_send(line).await
                } else {
                    // Install the generation-stamped submission the handlers act on.
                    self.input_submission.borrow_mut().install(line);
                    // Depth-first: the handler splice runs, then the completion reads
                    // what it left (the `HandleIncomingLine` → `Complete…` shape).
                    let mut spawned = self.spawned_actions.borrow_mut();
                    spawned.extend(handlers);
                    spawned.push_back(RuntimeAction::CompleteInputSubmission);
                    Ok(ActionResult::None)
                }
            }
            RuntimeAction::CompleteInputSubmission => {
                // The back half of `SubmitInput`: consume what the handlers left.
                // Cancel wins over replace regardless of handler order; an absent
                // submission (a reload tore the splice down) has nothing to send.
                let submission = self.input_submission.borrow_mut().take();
                match submission {
                    Some(submission) if !submission.is_cancelled() => {
                        self.dispatch_send(submission.into_text()).await
                    }
                    _ => Ok(ActionResult::None),
                }
            }
            RuntimeAction::ProcessOutgoingLine(line) => {
                // Pre-match reset of the capture flag; the per-eval set/get bracket below
                // re-primes it on the actual target isolate, so resetting Main here is
                // harmless regardless (each eval overrides it).
                self.script_engine.set_is_captured(&IsolateId::Main, false);

                match self.trigger_manager.process_outgoing_line(line.as_str()) {
                    Ok(()) => {
                        // sys:send — the command (post-alias) about to reach the game.
                        let payload = serde_json::json!({ "command": line.as_str() }).to_string();
                        Ok(self.run_host_event("sys:send", &payload))
                    }
                    Err(err) => Ok(ActionResult::Echo(format!(
                        "Error processing command {err:?}"
                    ))),
                }
            }
            RuntimeAction::SendRaw(str) => {
                self.send_verbatim_lines(&str).await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::SendWithRedactions { text, redactions } => {
                // Verbatim to the wire (like SendRaw), but the echoed/logged copy
                // has each secret substring masked. Masked input submissions ride
                // this arm too, which is what keeps them away from `sys:input`
                // handlers (the `SubmitInput` arm) and the alias/split pipeline
                // alike — a secret must never feed either.
                for line in text.split('\n') {
                    self.send_with_redactions(line, &redactions).await?;
                }
                if let Some(fut) = self.flush_buffer_updates()? {
                    fut.await?;
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::SendRawUnless(is_captured, str) => {
                if is_captured.load(Ordering::Relaxed) {
                    return Ok(ActionResult::None);
                }

                self.send_verbatim_lines(&str).await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::RunAutomation {
                isolate,
                origin,
                name,
                script,
                matches,
                depth,
                is_captured,
                stopped,
                fallthrough,
                is_alias,
            } => {
                if stopped.load(Ordering::Relaxed) {
                    return Ok(ActionResult::None);
                }

                // Count the invocation before entering user code. Besides matching the old
                // fire-limit timing, this ensures a handler that replaces itself cannot charge
                // the new definition for the old definition's fire.
                self.trigger_manager
                    .record_fire(&isolate, &origin, &name, is_alias);

                let mut continue_matching = fallthrough;
                let result = match script {
                    ScriptAction::EvalJavascript(id) => {
                        self.script_engine.begin_fallthrough(&isolate, fallthrough);
                        self.script_engine.set_is_captured(&isolate, true);
                        let result = self
                            .script_engine
                            .run_script(&self.trigger_manager, &isolate, id, &matches, depth)
                            .unwrap_or_else(|err| {
                                ActionResult::Echo(format!("JavaScript Error: {err:?}"))
                            });
                        if self.script_engine.get_is_captured(&isolate)
                            && let Some(is_captured) = &is_captured
                        {
                            is_captured.store(true, Ordering::Relaxed);
                        }
                        continue_matching = self.script_engine.end_fallthrough(&isolate);
                        result
                    }
                    ScriptAction::CallJavascriptFunction(id) => {
                        self.script_engine.begin_fallthrough(&isolate, fallthrough);
                        self.script_engine.set_is_captured(&isolate, true);
                        let result = self
                            .script_engine
                            .call_javascript_function(
                                &self.trigger_manager,
                                &isolate,
                                id,
                                &matches,
                                depth,
                            )
                            .unwrap_or_else(|err| {
                                ActionResult::Echo(format!("Error in Javascript Function: {err:?}"))
                            });
                        if self.script_engine.get_is_captured(&isolate)
                            && let Some(is_captured) = &is_captured
                        {
                            is_captured.store(true, Ordering::Relaxed);
                        }
                        continue_matching = self.script_engine.end_fallthrough(&isolate);
                        result
                    }
                    ScriptAction::SendSimple(script) => {
                        if let Some(is_captured) = &is_captured {
                            is_captured.store(true, Ordering::Relaxed);
                        }
                        self.trigger_manager
                            .run_simple_automation(&script, &matches, depth)?;
                        ActionResult::None
                    }
                    ScriptAction::SendRaw(script) => {
                        if let Some(is_captured) = &is_captured {
                            is_captured.store(true, Ordering::Relaxed);
                        }
                        ActionResult::Run(vec![RuntimeAction::SendRaw(script)])
                    }
                    ScriptAction::Noop => ActionResult::None,
                };

                if !continue_matching {
                    stopped.store(true, Ordering::Relaxed);
                }
                Ok(result)
            }
            RuntimeAction::EvalJavascript {
                isolate,
                id,
                matches,
                depth,
                is_captured,
            } => {
                self.script_engine.set_is_captured(&isolate, true);

                let result = self
                    .script_engine
                    .run_script(&self.trigger_manager, &isolate, id, &matches, depth)
                    .unwrap_or_else(|err| ActionResult::Echo(format!("JavaScript Error: {err:?}")));

                if self.script_engine.get_is_captured(&isolate)
                    && let Some(is_captured) = is_captured
                {
                    is_captured.store(true, Ordering::Relaxed);
                }

                Ok(result)
            }
            RuntimeAction::CallJavascriptFunction {
                isolate,
                id,
                matches,
                depth,
                is_captured,
            } => {
                self.script_engine.set_is_captured(&isolate, true);

                let result = self
                    .script_engine
                    .call_javascript_function(&self.trigger_manager, &isolate, id, &matches, depth)
                    .unwrap_or_else(|err| {
                        ActionResult::Echo(format!("Error in Javascript Function: {err:?}"))
                    });

                if self.script_engine.get_is_captured(&isolate)
                    && let Some(is_captured) = is_captured
                {
                    is_captured.store(true, Ordering::Relaxed);
                }

                Ok(result)
            }
            RuntimeAction::ExecuteJavascriptFunction {
                isolate,
                instance,
                function,
                args,
            } => self.script_engine.execute_javascript_function(
                &isolate,
                instance,
                function.as_ref(),
                &args,
            ),
            RuntimeAction::InvokeLinkCallback {
                session,
                isolate,
                instance,
                id,
                shift,
                ctrl,
                alt,
            } => {
                // The UI addressed the session owning the clicked pane; a fragment
                // echoed cross-session names its creating session here, so forward
                // the click home — the callback lives in that engine.
                if session == self.session_id {
                    self.script_engine
                        .invoke_link_callback(&isolate, instance, id, shift, ctrl, alt)
                } else {
                    if let Some(runtime) = crate::session::registry::get_runtime(session) {
                        runtime
                            .tx
                            .send(RuntimeAction::InvokeLinkCallback {
                                session,
                                isolate,
                                instance,
                                id,
                                shift,
                                ctrl,
                                alt,
                            })
                            .ok();
                    } else {
                        warn!("Dropping link click for session {session}: no live runtime");
                    }
                    Ok(ActionResult::None)
                }
            }
            RuntimeAction::ResolveLinkTooltip {
                session,
                isolate,
                instance,
                id,
                state,
            } => {
                // Like click callbacks, tooltip callbacks execute in the isolate
                // that created the fragment, even when another session displays it.
                if session == self.session_id {
                    self.script_engine
                        .resolve_link_tooltip(&isolate, instance, id, state)
                } else {
                    if let Some(runtime) = crate::session::registry::get_runtime(session) {
                        if !send_link_tooltip(
                            &runtime.tx,
                            RuntimeAction::ResolveLinkTooltip {
                                session,
                                isolate,
                                instance,
                                id,
                                state,
                            },
                        ) {
                            warn!(
                                "Dropping link tooltip for session {session}: runtime channel closed"
                            );
                        }
                    } else {
                        state.resolve(None);
                        warn!("Dropping link tooltip for session {session}: no live runtime");
                    }
                    Ok(ActionResult::None)
                }
            }
            RuntimeAction::AddHotkey {
                isolate,
                origin,
                name,
                hotkey,
                function_id,
            } => {
                // Upsert by `(isolate, origin, name)`: if this key already has a binding, drop
                // and unregister the old one first so a redefine replaces it.
                let key = (isolate.clone(), origin, name);
                if let Some(old_id) = self.hotkey_ids.remove(&key) {
                    self.hotkeys.remove(&old_id);
                    self.ui_tx
                        .send(TaggedSessionEvent {
                            session_id: self.session_id,
                            event: SessionEvent::UnregisterHotkey(old_id),
                        })
                        .await?;
                }

                let hotkey_id = self.next_hotkey_id;
                self.next_hotkey_id.0 = self.next_hotkey_id.0.add(1);
                let action = if let Some(function_id) = function_id {
                    // `createHotkey(.., handler)`: the handler is a function already registered
                    // in the creating isolate's `script_functions`; fire it there.
                    ScriptAction::CallJavascriptFunction(function_id)
                } else {
                    match hotkey.language {
                        ScriptLang::Plaintext => ScriptAction::SendSimple(
                            hotkey.script.clone().unwrap_or_default().into(),
                        ),
                        ScriptLang::JS | ScriptLang::TS => {
                            // Disk/inline-string hotkeys are user automations: the main isolate
                            // (the script-string path has no package provenance to honor).
                            match self.script_engine.add_script(
                                &IsolateId::Main,
                                hotkey.script.as_ref().map_or("", |s| s.as_str()),
                            ) {
                                Ok(script_id) => ScriptAction::EvalJavascript(script_id),
                                Err(err) => {
                                    self.echo_warn_str(
                                        format!("Error adding script: {err:?}").as_str(),
                                    )?;
                                    ScriptAction::Noop
                                }
                            }
                        }
                    }
                };
                self.hotkeys.insert(hotkey_id, (isolate, action));
                self.hotkey_ids.insert(key, hotkey_id);
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::RegisterHotkey(hotkey_id, hotkey),
                    })
                    .await?;

                Ok(ActionResult::None)
            }
            RuntimeAction::RemoveHotkey(isolate, origin, name) => {
                // `delete()`: drop the binding under its `(isolate, origin, name)` key and
                // unregister it from the UI. Unknown key ⇒ no-op.
                if let Some(id) = self.hotkey_ids.remove(&(isolate, origin, name)) {
                    self.hotkeys.remove(&id);
                    self.ui_tx
                        .send(TaggedSessionEvent {
                            session_id: self.session_id,
                            event: SessionEvent::UnregisterHotkey(id),
                        })
                        .await?;
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::ExecHotkey { id } => {
                if let Some((isolate, action)) = self.hotkeys.get(&id) {
                    match action {
                        ScriptAction::SendRaw(script) => {
                            self.send(script.clone().as_str()).await?;
                            Ok(ActionResult::None)
                        }
                        ScriptAction::SendSimple(script) => Ok(ActionResult::Run(
                            trigger::split_commands(script, &self.command_separator)
                                .into_iter()
                                .map(|line| {
                                    RuntimeAction::ProcessOutgoingLine(Arc::new(line.to_string()))
                                })
                                .collect(),
                        )),
                        ScriptAction::EvalJavascript(script_id) => {
                            // Disk/inline-string hotkeys compile into the main isolate; a
                            // script-created function hotkey runs in its creating isolate.
                            let isolate = isolate.clone();
                            self.script_engine
                                .run_script(
                                    &self.trigger_manager,
                                    &isolate,
                                    *script_id,
                                    &Arc::new(vec![]),
                                    0,
                                )
                                .unwrap_or_else(|err| {
                                    ActionResult::Echo(format!(
                                        "Error in Javascript Function: {err:?}"
                                    ))
                                });

                            Ok(ActionResult::None)
                        }
                        ScriptAction::CallJavascriptFunction(function_id) => {
                            let isolate = isolate.clone();
                            self.script_engine
                                .call_javascript_function(
                                    &self.trigger_manager,
                                    &isolate,
                                    *function_id,
                                    &Arc::new(vec![]),
                                    0,
                                )
                                .unwrap_or_else(|err| {
                                    ActionResult::Echo(format!(
                                        "Error calling Javascript Function: {err:?}"
                                    ))
                                });

                            Ok(ActionResult::None)
                        }
                        ScriptAction::Noop => Ok(ActionResult::None),
                    }
                } else {
                    bail!("Hotkey {id} not found")
                }
            }
            RuntimeAction::AddAlias {
                isolate,
                origin,
                name,
                alias,
                fire_limit,
            } => {
                match alias.language {
                    ScriptLang::Plaintext => {
                        self.trigger_manager.push_simple_alias(
                            isolate,
                            origin,
                            name,
                            Arc::new(vec![alias.pattern]),
                            alias.script.unwrap_or_default().into(),
                            alias.priority,
                            alias.fallthrough,
                            fire_limit,
                        )?;
                    }
                    ScriptLang::JS | ScriptLang::TS => {
                        let src = alias.script.unwrap_or_default();
                        let script_id = self.script_engine.add_script(&isolate, src.as_str())?;
                        self.trigger_manager.push_javascript_alias(
                            isolate,
                            origin,
                            &name,
                            &Arc::new(vec![alias.pattern]),
                            script_id,
                            alias.priority,
                            alias.fallthrough,
                            fire_limit,
                            Some(Arc::from(src)),
                        )?;
                    }
                }

                Ok(ActionResult::None)
            }
            RuntimeAction::AddJavascriptFunctionAlias {
                isolate,
                origin,
                name,
                patterns,
                function_id,
                priority,
                fallthrough,
                fire_limit,
                script_source,
            } => {
                self.trigger_manager.push_javascript_function_alias(
                    isolate,
                    origin,
                    name,
                    patterns,
                    function_id,
                    priority,
                    fallthrough,
                    fire_limit,
                    script_source,
                )?;
                Ok(ActionResult::None)
            }
            RuntimeAction::AddTrigger {
                isolate,
                origin,
                name,
                trigger,
                fire_limit,
                line_limit,
            } => {
                // Capture the JS/TS eval source for the read-only detail pane; plaintext
                // bodies are recovered from the `ScriptAction` itself, so they carry no source.
                let mut source: Option<Arc<str>> = None;
                let action = match trigger.language {
                    ScriptLang::Plaintext => {
                        ScriptAction::SendSimple(trigger.script.unwrap_or_default().into())
                    }
                    ScriptLang::JS | ScriptLang::TS => {
                        let src = trigger.script.unwrap_or_default();
                        let script_id = self.script_engine.add_script(&isolate, src.as_str())?;
                        source = Some(Arc::from(src));
                        ScriptAction::EvalJavascript(script_id)
                    }
                };

                self.trigger_manager.push_trigger(PushTriggerParams {
                    isolate,
                    origin,
                    name: &name,
                    patterns: &Arc::new(trigger.patterns.unwrap_or_default()),
                    raw_patterns: &Arc::new(trigger.raw_patterns.unwrap_or_default()),
                    anti_patterns: &Arc::new(trigger.anti_patterns.unwrap_or_default()),
                    action,
                    enabled: trigger.enabled,
                    priority: trigger.priority,
                    fallthrough: trigger.fallthrough,
                    prompt: trigger.prompt,
                    fire_limit,
                    line_limit,
                    source,
                })?;
                Ok(ActionResult::None)
            }
            RuntimeAction::AddJavascriptFunctionTrigger {
                isolate,
                origin,
                name,
                patterns,
                raw_patterns,
                anti_patterns,
                function_id,
                prompt,
                enabled,
                priority,
                fallthrough,
                fire_limit,
                line_limit,
                script_source,
            } => {
                self.trigger_manager.push_trigger(PushTriggerParams {
                    isolate,
                    origin,
                    name: &name,
                    patterns: &patterns,
                    raw_patterns: &raw_patterns,
                    anti_patterns: &anti_patterns,
                    action: ScriptAction::CallJavascriptFunction(function_id),
                    enabled,
                    priority,
                    fallthrough,
                    prompt,
                    fire_limit,
                    line_limit,
                    source: script_source,
                })?;
                Ok(ActionResult::None)
            }
            RuntimeAction::EnableAlias(isolate, origin, name, enabled) => {
                self.trigger_manager
                    .enable_alias(&isolate, &origin, &name, enabled);
                Ok(ActionResult::None)
            }
            RuntimeAction::EnableTrigger(isolate, origin, name, enabled) => {
                self.trigger_manager
                    .enable_trigger(&isolate, &origin, &name, enabled);
                Ok(ActionResult::None)
            }
            RuntimeAction::RemoveAlias(isolate, origin, name) => {
                self.trigger_manager.remove_alias(&isolate, &origin, &name);
                Ok(ActionResult::None)
            }
            RuntimeAction::RemoveTrigger(isolate, origin, name) => {
                self.trigger_manager
                    .remove_trigger(&isolate, &origin, &name);
                Ok(ActionResult::None)
            }
            RuntimeAction::Connected => {
                if !self
                    .connected
                    .swap(true, std::sync::atomic::Ordering::AcqRel)
                    && let Some(snapshot) = crate::session::registry::snapshot(self.session_id)
                {
                    crate::session::registry::broadcast_lifecycle(
                        self.server_name.as_str(),
                        "connected",
                        &snapshot,
                        false,
                    );
                }
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::Connected,
                    })
                    .await?;
                Ok(self.run_host_event("sys:connect", "{}"))
            }
            RuntimeAction::Disconnected {
                connection_generation,
            } => {
                // Only the live socket's own teardown may clear the deferred
                // profile send: a replaced socket's late `Disconnected` would
                // otherwise erase the NEW connection's pending send before its
                // first displayable packet releases it.
                if connection_generation == self.connection_generation {
                    self.pending_send_on_connect = None;
                    self.send_on_connect_armed
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
                if self
                    .connected
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                    && let Some(mut snapshot) = crate::session::registry::snapshot(self.session_id)
                {
                    snapshot.connected = false;
                    crate::session::registry::broadcast_lifecycle(
                        self.server_name.as_str(),
                        "disconnected",
                        &snapshot,
                        false,
                    );
                }
                // The tail of the session log is what users read after a
                // drop; don't leave it sitting in the BufWriter.
                self.flush_log();
                // Drop any unterminated line pipeline: the next connection starts
                // a fresh logical line, so neither pane-routing state nor a
                // carriage-return replacement transaction may cross the boundary.
                // The main open line is committed by the disconnect notice echo.
                self.abort_incoming_line_sync();
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::Disconnected,
                    })
                    .await?;
                // The telnet ECHO mask dies with the connection: a server that
                // dropped while it held ECHO can never send the WONT, so the
                // release rides the disconnect (a no-op when it was not held).
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::ServerEcho { enabled: false },
                    })
                    .await?;
                // A drop while GMCP was negotiated closes the protocol too; the subtree is
                // retained for post-mortem reads (`docs/gmcp.md` §4.6).
                let mut actions = self.script_engine.host_emit("sys:disconnect", "{}");
                if self.gmcp.on_disabled() {
                    actions.extend(self.script_engine.host_emit("gmcp:closed", "{}"));
                }
                if self.msdp.on_disabled() {
                    actions.extend(self.script_engine.host_emit("msdp:closed", "{}"));
                }
                if actions.is_empty() {
                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::Run(actions))
                }
            }
            RuntimeAction::GmcpMessage { name, data } => {
                let effects = self.gmcp.ingest(
                    &mut self.session_store.borrow_mut(),
                    &self.catalogue,
                    &name,
                    data.as_deref(),
                );
                // The write flushes at the run loop's normal per-turn flush point, which
                // precedes the next dispatched action — so a trigger on the line that
                // followed this message on the wire reads the new value
                // (`docs/gmcp.md` §3.3).
                self.queue_gmcp_echoes(effects.echoes);
                Ok(ActionResult::None)
            }
            RuntimeAction::GmcpEnabled => {
                // The connection task already framed Core.Hello + the baseline
                // Core.Supports.Set onto the reply buffer; here the session side clears
                // the subtree (fresh server, fresh truth), follows with the module
                // registry's Supports.Add (pre-ready registrations and renegotiation
                // re-send alike, `docs/gmcp.md` §6.2), and announces readiness.
                self.gmcp.on_enabled(&mut self.session_store.borrow_mut());
                self.write_gmcp_frame(self.gmcp.supports_add_frame()).await;
                Ok(self.run_host_event("gmcp:ready", "{}"))
            }
            RuntimeAction::GmcpDisabled => {
                if self.gmcp.on_disabled() {
                    Ok(self.run_host_event("gmcp:closed", "{}"))
                } else {
                    Ok(ActionResult::None)
                }
            }
            RuntimeAction::WindowSizeChanged { cols, rows } => {
                // Store first, then wake: the socket task re-reads the cell on the
                // wakeup, so it can never observe the wakeup without the new value,
                // and it decides in write order whether NAWS is negotiated and a
                // report is due.
                self.window_size.store(
                    crate::session::connection::responders::pack_dims(cols, rows),
                    std::sync::atomic::Ordering::Relaxed,
                );
                if let Some(connection) = self.connection.as_ref() {
                    connection.notify_window_size();
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::ServerEchoChanged { enabled } => {
                // Forward the negotiation fact to the UI, which owns the mask
                // (pref check, compose with a script-set mask, stash/restore).
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::ServerEcho { enabled },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::MsdpMessage { payload } => {
                let effects = self.msdp.ingest(
                    &mut self.session_store.borrow_mut(),
                    &self.catalogue,
                    &payload,
                );
                // Same flush point as GmcpMessage: the write is readable by every
                // consumer of any line that followed it on the wire.
                self.queue_gmcp_echoes(effects.echoes);
                Ok(ActionResult::None)
            }
            RuntimeAction::MsdpEnabled => {
                // The connection task already framed LIST + the baseline REPORT onto the
                // reply buffer; here the session side clears the subtree (fresh server,
                // fresh truth) and announces readiness.
                self.msdp.on_enabled(&mut self.session_store.borrow_mut());
                Ok(self.run_host_event("msdp:ready", "{}"))
            }
            RuntimeAction::MsdpDisabled => {
                if self.msdp.on_disabled() {
                    Ok(self.run_host_event("msdp:closed", "{}"))
                } else {
                    Ok(ActionResult::None)
                }
            }
            RuntimeAction::GmcpSend { name, data } => {
                let (allowed, notice) = self.gmcp.send_gate();
                if let Some(notice) = notice {
                    self.queue_gmcp_echoes(vec![notice]);
                }
                if allowed {
                    let mut frame = Vec::new();
                    crate::session::connection::gmcp::frame_message(
                        &name,
                        data.as_deref(),
                        &mut frame,
                    );
                    self.write_gmcp_frame(frame).await;
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::GmcpEnableModule {
                isolate,
                module,
                version,
            } => {
                let frame = self.gmcp.enable_module(isolate, &module, version);
                self.write_gmcp_frame(frame).await;
                Ok(ActionResult::None)
            }
            RuntimeAction::GmcpDisableModule { isolate, module } => {
                let frame = self.gmcp.disable_module(&isolate, &module);
                self.write_gmcp_frame(frame).await;
                Ok(ActionResult::None)
            }
            RuntimeAction::GmcpAddMergeKeys(names) => {
                self.gmcp.add_merge_keys(&names);
                Ok(ActionResult::None)
            }
            RuntimeAction::PerformLineOperation {
                line_number,
                operation,
            } => {
                // Write consistency: apply the SAME deterministic op to the ring entry (if
                // the target line is still within the window) so a later `buffer.line(n).text`
                // reflects the edit, then forward `PerformLineOperation` to the UI.
                // Both sides apply `LineOperation::apply`, so the ring and the on-screen buffer
                // stay identical. A line number outside the window is a no-op on the ring
                // (still forwarded to the UI, which holds the larger 10k scrollback).
                {
                    let mut ring = self.recent_lines.borrow_mut();
                    if let Some(entry) = ring.iter_mut().find(|(n, _)| *n == line_number) {
                        entry.1 = operation.apply(&entry.1);
                    }
                }
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PerformLineOperation {
                            line_number,
                            operation,
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::SetCurrentLocation(id, room_number) => {
                // Mirror into the shared cell so `getCurrentLocation` reads the latest value
                // even when the action arrives by a path other than the op (the op also writes it,
                // but this keeps the runtime the single source of truth).
                *self.current_location.borrow_mut() = Some((id, room_number));
                // map:room — the host emits it at the location-change site so
                // any package gets room events even without the mapper package installed.
                let payload = serde_json::json!({
                    "areaId": id.to_string(),
                    "roomNumber": room_number,
                })
                .to_string();
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::SetCurrentLocation(id, room_number),
                    })
                    .await?;
                Ok(self.run_host_event("map:room", &payload))
            }
            RuntimeAction::NoteMapperNavigation(area_id) => {
                // Advisory scope hint: forward to the UI daemon, which owns the
                // per-server association store and decides whether to bind.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::MapperNavigated(area_id),
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::OfferMapRescue {
                area_id,
                atlas_id,
                atlas_name,
            } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::OfferMapRescue {
                            area_id,
                            atlas_id,
                            atlas_name,
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::AssociateCreatedArea(area_id) => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::MapAreaCreated(area_id),
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::AssociateCreatedAtlas(atlas_id) => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::MapAtlasCreated(atlas_id),
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneOpened {
                def,
                placement,
                reconcile_registry,
            } => {
                let Some((def, placement)) =
                    prepare_pane_open(&self.pane_registry, def, placement, reconcile_registry)
                else {
                    // A foreign split mutates this data-only registry on its
                    // caller thread before queueing the open. Reconcile at the
                    // owner queue: an intervening close retires the key (drop
                    // the stale open), while an intervening update is reflected
                    // in the def we publish.
                    return Ok(ActionResult::None);
                };
                // Own-runtime split/close sequences are already ordered on
                // this queue. `prepare_pane_open` preserves their historical
                // UI transition even when close or failed-load cleanup has
                // retired the key by the time the queued open is dispatched.
                // Input callbacks are seated by the same split op before this
                // action is sent, so the UI cannot expose a submit path whose
                // first input races its handler registration.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneOpened { def, placement },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneClosed {
                key,
                ui_command_published,
            } => {
                // Flush first: buffered updates may hold `AppendTo`s for this key, and the
                // dangling-sink rule promises the UI that `PaneClosed` arrives behind them.
                // The closed pane's mirrored size dies with it (keys are never reused).
                self.pane_size_mirror.lock().unwrap().remove(key);
                if let Some(fut) = self.flush_buffer_updates()? {
                    fut.await?;
                }
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: pane_closed_event(key, ui_command_published),
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneUpdated {
                def,
                announce_visibility,
            } => {
                // The registry mutation already happened synchronously in the op; this is a
                // pure display-state refresh (title bar, hidden, font size), so no flush is
                // needed. `announce_visibility` marks that the change included the hidden
                // toggle — that is the queuing op's edge detection, carried here because
                // the def alone can't say what changed.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneUpdated(def.clone()),
                    })
                    .await?;
                let actions = if announce_visibility {
                    self.pane_visibility_emit(&def)
                } else {
                    Vec::new()
                };
                if actions.is_empty() {
                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::Run(actions))
                }
            }
            RuntimeAction::PaneReloadSweep => {
                // Reload garbage collection: close every pane no script re-claimed
                // (split under the current epoch) while the engine rebuilt. Queued
                // behind the load's own actions, so a pane the reloading scripts
                // echoed into before abandoning still shows those lines before it
                // closes; the flush upholds the AppendTo-before-PaneClosed promise.
                let ui_command_producer = self.ui_command_producer.clone();
                let swept = {
                    let mut registry = self.pane_registry.lock().unwrap();
                    registry
                        .sweep_unclaimed()
                        .into_iter()
                        .map(|key| {
                            let published = ui_command_producer.as_ref().is_some_and(|producer| {
                                producer.send(UiCommand::Pane(PaneCommand::Close {
                                    session_id: self.session_id,
                                    key,
                                }))
                            });
                            (key, published)
                        })
                        .collect::<Vec<_>>()
                };
                if !swept.is_empty() {
                    if let Some(fut) = self.flush_buffer_updates()? {
                        fut.await?;
                    }
                    for (key, ui_command_published) in swept {
                        // The swept pane's input state dies with it, exactly
                        // as on an explicit close.
                        super::input::purge_pane_input_state(
                            &self.input_mirror,
                            &self.input_word_sets,
                            &self.pane_input_callbacks,
                            key,
                        );
                        self.pane_size_mirror.lock().unwrap().remove(key);
                        self.ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: pane_closed_event(key, ui_command_published),
                            })
                            .await?;
                    }
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneCloseRemote {
                namespace,
                name,
                ui_command_published,
            } => {
                let (closed, ui_command_published) = {
                    let mut registry = self.pane_registry.lock().unwrap();
                    let closed = registry.close(&namespace, &name);
                    let published = match &closed {
                        Ok(key) if !ui_command_published => {
                            self.ui_command_producer.as_ref().is_some_and(|producer| {
                                producer.send(UiCommand::Pane(PaneCommand::Close {
                                    session_id: self.session_id,
                                    key: *key,
                                }))
                            })
                        }
                        Ok(_) => ui_command_published,
                        Err(_) => false,
                    };
                    (closed, published)
                };
                match closed {
                    Ok(key) => {
                        // The closed pane's input state dies with it, like the
                        // own-session close op's purge.
                        super::input::purge_pane_input_state(
                            &self.input_mirror,
                            &self.input_word_sets,
                            &self.pane_input_callbacks,
                            key,
                        );
                        self.pane_size_mirror.lock().unwrap().remove(key);
                        if let Some(fut) = self.flush_buffer_updates()? {
                            fut.await?;
                        }
                        self.ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: pane_closed_event(key, ui_command_published),
                            })
                            .await?;
                    }
                    // Idempotent best-effort: an unknown/already-closed name is a no-op.
                    Err(PaneError::NoSuchPane(_)) => {}
                    Err(err) => warn!("Cross-session pane close '{name}' refused: {err}"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneSetHiddenRemote {
                namespace,
                name,
                hidden,
            } => {
                // Cross-session hide/show, resolved on this (owning) runtime;
                // last-writer-wins in queue order, silent no-op on an unknown
                // name (the `PaneCloseRemote` rule).
                let updated = self
                    .pane_registry
                    .lock()
                    .unwrap()
                    .set_hidden(&namespace, &name, hidden);
                match updated {
                    Ok(Some(def)) => {
                        self.ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: SessionEvent::PaneUpdated(def.clone()),
                            })
                            .await?;
                        let actions = self.pane_visibility_emit(&def);
                        if !actions.is_empty() {
                            return Ok(ActionResult::Run(actions));
                        }
                    }
                    Ok(None) | Err(PaneError::NoSuchPane(_)) => {}
                    Err(err) => warn!("Cross-session pane hide/show '{name}' refused: {err}"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneSetFontSizeRemote {
                namespace,
                name,
                font_size,
            } => {
                let updated = self
                    .pane_registry
                    .lock()
                    .unwrap()
                    .set_font_size(&namespace, &name, font_size);
                match updated {
                    Ok(Some(def)) => {
                        self.ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: SessionEvent::PaneUpdated(def),
                            })
                            .await?;
                    }
                    Ok(None) | Err(PaneError::NoSuchPane(_)) => {}
                    Err(err) => warn!("Cross-session pane font size '{name}' refused: {err}"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneUserHidden { key, hidden } => {
                // The UI's report of a user eyeball click. The toggling window
                // already flipped optimistically; writing the def and echoing
                // `PaneUpdated` makes every consumer converge on it (idempotent
                // for the reporter). No main guard — the user may hide any
                // pane, including main (the all-hidden fallback keeps a window
                // usable). A retired key or an already-matching state drops
                // the report whole, like a stale `InputStateChanged`.
                let updated = self
                    .pane_registry
                    .lock()
                    .unwrap()
                    .set_hidden_by_key(key, hidden);
                match updated {
                    Some(def) => {
                        self.ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: SessionEvent::PaneUpdated(def.clone()),
                            })
                            .await?;
                        let actions = self.pane_visibility_emit(&def);
                        if actions.is_empty() {
                            Ok(ActionResult::None)
                        } else {
                            Ok(ActionResult::Run(actions))
                        }
                    }
                    None => Ok(ActionResult::None),
                }
            }
            RuntimeAction::PaneResize { key, width, height } => {
                // Placement command: no core state, no ordering constraints —
                // the daemon applies it to the hosting window's cluster model.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneResize { key, width, height },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneRelocate {
                key,
                reference,
                direction,
                size_px,
            } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneRelocate {
                            key,
                            reference,
                            direction,
                            size_px,
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneGroupWith {
                key,
                reference_session,
                reference,
                position,
                selected,
            } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneGroupWith {
                            key,
                            reference_session,
                            reference,
                            position,
                            selected,
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneSelect { key } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneSelect { key },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneTearOut { key, width, height } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneTearOut { key, width, height },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneSwap {
                key,
                other_session,
                other_key,
            } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneSwap {
                            key,
                            other_session,
                            other_key,
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneEcho {
                key,
                namespace,
                name,
                text,
            } => {
                // Pane echoes are whole lines by construction and skip
                // emitted_line_count / record_emitted_line and the main
                // open-line heuristic entirely.
                match self.resolve_pane_target(key, &namespace, &name) {
                    // `pane.echo` on the main pane IS a normal echo: it takes
                    // the counted Append path (numbering parity), never an
                    // `AppendTo(MAIN)`. Appends only — delivery rides the run
                    // loop's coalescing points, like every echo arm.
                    Some((_, _, true)) => {
                        self.echo_str_sync(text.as_str());
                    }
                    Some((key, PaneKind::Terminal, _)) => {
                        for line in text.split('\n') {
                            self.pending_buffer_updates.push(BufferUpdate::AppendTo(
                                key,
                                Arc::new(StyledLine::from_echo_str(line)),
                            ));
                        }
                    }
                    Some((_, PaneKind::Widgets, _)) => {
                        warn!("Dropping echo to widgets pane '{name}'");
                    }
                    None => warn!("Dropping echo to unknown pane '{name}'"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneEchoStyled {
                key,
                namespace,
                name,
                lines,
            } => {
                // The lines arrive pre-split and pre-styled from the op boundary.
                match self.resolve_pane_target(key, &namespace, &name) {
                    // Main-pane delivery IS a normal styled echo: counted Append path.
                    // Appends only — delivery rides the run loop's coalescing points.
                    Some((_, _, true)) => {
                        self.echo_styled_lines_sync(&lines);
                    }
                    Some((key, PaneKind::Terminal, _)) => {
                        for line in &lines {
                            self.pending_buffer_updates
                                .push(BufferUpdate::AppendTo(key, line.clone()));
                        }
                    }
                    Some((_, PaneKind::Widgets, _)) => {
                        warn!("Dropping styled echo to widgets pane '{name}'");
                    }
                    None => warn!("Dropping styled echo to unknown pane '{name}'"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneClear {
                key,
                namespace,
                name,
            } => {
                match self.resolve_pane_target(key, &namespace, &name) {
                    Some((key, PaneKind::Terminal, is_main)) => {
                        if is_main && self.main_open_line {
                            // The open partial vanishes with the clear; account for it as
                            // committed-then-cleared so core's count stays in step with the
                            // UI's (which consumed a number when the partial started).
                            self.emitted_line_count
                                .set(self.emitted_line_count.get() + 1);
                            self.main_open_line = false;
                        }
                        self.pending_buffer_updates.push(BufferUpdate::Clear(key));
                        if let Some(fut) = self.flush_buffer_updates()? {
                            fut.await?;
                        }
                    }
                    Some((_, PaneKind::Widgets, _)) => {
                        warn!("Dropping clear of widgets pane '{name}'");
                    }
                    None => warn!("Dropping clear of unknown pane '{name}'"),
                }
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneInputSubmit { key, text, retry } => {
                // Deliver a pane input's submission to its registered onSubmit
                // handler — and to nothing else: no pipeline entry, no
                // `sys:input`, no main history. The handler runs in the
                // creating isolate under its instantiation nonce; every stale
                // form of the address (reload, uninstall) is a warn-and-drop
                // inside the engine call, like widget callbacks.
                let callback = self.pane_input_callbacks.lock().unwrap().get(key);
                let Some(cb) = callback else {
                    if !retry {
                        let _ = self
                            .session_runtime_tx
                            .send(RuntimeAction::PaneInputSubmit {
                                key,
                                text,
                                retry: true,
                            });
                        return Ok(ActionResult::None);
                    }
                    warn!(
                        "Dropping pane-input submission for {key}: no registered onSubmit \
                         handler (a reloaded script re-registers by re-splitting its pane)"
                    );
                    return Ok(ActionResult::None);
                };
                if cb.home_session != self.session_id {
                    if let Some(runtime) = crate::session::registry::get_runtime(cb.home_session)
                        && runtime.server_name.as_str() == self.server_name.as_str()
                    {
                        let _ = runtime
                            .tx
                            .send(RuntimeAction::InvokePaneInputSubmit { callback: cb, text });
                    } else {
                        warn!("Dropping pane-input submission: callback home session is gone");
                    }
                    return Ok(ActionResult::None);
                }
                self.script_engine.invoke_pane_input_submit(
                    &cb.isolate,
                    cb.instance,
                    cb.function_id,
                    text.as_str(),
                )
            }
            RuntimeAction::InvokePaneInputSubmit { callback, text } => {
                if callback.home_session != self.session_id {
                    warn!("Dropping misrouted pane-input callback");
                    return Ok(ActionResult::None);
                }
                self.script_engine.invoke_pane_input_submit(
                    &callback.isolate,
                    callback.instance,
                    callback.function_id,
                    text.as_str(),
                )
            }
            RuntimeAction::InputApply { key, op } => {
                // The op already resolved (and kind-checked) the key synchronously; this
                // just publishes the mutation on the ordered UI channel.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::InputOp { key, op },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::InputMirrorInterest => {
                // Queued once, on the session's first input-mirror read; the UI
                // starts feeding the mirror and pushes the current state immediately.
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::InputMirrorInterest,
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneMirrorInterest => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::PaneMirrorInterest,
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::LayoutSave { name } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::LayoutSave { name },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::LayoutApply { name } => {
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::LayoutApply { name },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::PaneDisplayChanged { key, width, height } => {
                // The UI's coalesced pane-size update: write the mirror the
                // `pane.size` read op consults; the `pane:resize` host event
                // derives from the same feed's edges. A non-main key with no
                // live registry entry drops whole (the update was in flight
                // when the close purge ran), like a stale `InputStateChanged`.
                let pane_name = {
                    let registry = self.pane_registry.lock().unwrap();
                    match registry.get(key) {
                        Some(def) => def.name.to_string(),
                        None => return Ok(ActionResult::None),
                    }
                };
                let size = super::pane::PaneSize { width, height };
                let prior = self.pane_size_mirror.lock().unwrap().apply(key, size);
                // A pane's first-ever report is a BASELINE, not an edge: the
                // UI pushes current sizes unconditionally when interest is
                // flagged (and when a pane opens under standing interest), so
                // state that merely predates the subscription seeds the
                // mirror without replaying as a resize event.
                let Some(prior) = prior else {
                    return Ok(ActionResult::None);
                };
                if prior == size {
                    return Ok(ActionResult::None);
                }
                let actions = self.gated_host_emit("pane:resize", || {
                    serde_json::json!({
                        "pane": pane_name,
                        "width": width,
                        "height": height,
                    })
                    .to_string()
                });
                if actions.is_empty() {
                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::Run(actions))
                }
            }
            RuntimeAction::InputStateChanged {
                key,
                snapshot,
                source,
            } => {
                // The UI's coalesced state update: write the mirror the read ops
                // consult. `source` attributes the change (typing vs script
                // stuffing); the mirror itself stores only the snapshot.
                //
                // The observe-only `input:change`/`input:focus` host events
                // (`docs/input.md` §3.5) derive from this same feed:
                // edges are detected against the mirror's prior state before it
                // is overwritten, and the effective (content-suppressed while
                // masked) snapshot is what the payload reads — a masked update
                // can never leak content through the event either. Both emits
                // are subscriber-gated before any payload builds; updates ride
                // the UI's per-input coalescing (identical successive states
                // collapse), and the delivered source is the last mutation's.
                //
                // `pane` names the pane hosting the input; omitted for main.
                // Resolved before the mirror write because a pane update can
                // arrive behind its pane's close (the UI had it in flight when
                // the close purge ran): with the registry entry gone, applying
                // it would resurrect mirror state for the dead key and the
                // pane-less payload would read as the MAIN input's — so a
                // non-main key with no live registry entry drops here whole.
                let pane_name = if key == MAIN_PANE_KEY {
                    None
                } else {
                    let name = self
                        .pane_registry
                        .lock()
                        .unwrap()
                        .get(key)
                        .map(|def| def.name.to_string());
                    if name.is_none() {
                        return Ok(ActionResult::None);
                    }
                    name
                };
                let (prior, effective) = {
                    let mut mirror = self.input_mirror.lock().unwrap();
                    let prior = mirror.apply(key, snapshot);
                    (prior, mirror.snapshot(key))
                };
                // An input's first-ever report is a BASELINE, not an edge: the
                // UI pushes current state unconditionally when interest is
                // flagged (and when a pane input is created under standing
                // interest), so state that merely predates the subscription
                // must seed the mirror without replaying as change/focus
                // events. Edges exist only against a recorded prior.
                let Some(prior) = prior else {
                    return Ok(ActionResult::None);
                };
                let mut actions = Vec::new();
                // A change is content news: the value moved, or masking flipped
                // (while masked, per-keystroke updates never cross the channel
                // at all, so masked typing is invisible here by construction).
                if prior.value != effective.value || prior.masked != effective.masked {
                    actions.extend(self.gated_host_emit("input:change", || {
                        let mut payload = serde_json::Map::new();
                        if effective.masked {
                            payload.insert("masked".into(), serde_json::Value::Bool(true));
                        } else {
                            payload.insert(
                                "value".into(),
                                serde_json::Value::String(effective.value.as_str().to_string()),
                            );
                        }
                        if let Some(pane) = &pane_name {
                            payload.insert("pane".into(), serde_json::Value::String(pane.clone()));
                        }
                        payload.insert(
                            "source".into(),
                            serde_json::Value::String(source.as_str().to_string()),
                        );
                        serde_json::Value::Object(payload).to_string()
                    }));
                }
                if prior.focused != effective.focused {
                    actions.extend(self.gated_host_emit("input:focus", || {
                        let mut payload = serde_json::Map::new();
                        payload
                            .insert("focused".into(), serde_json::Value::Bool(effective.focused));
                        if effective.masked {
                            payload.insert("masked".into(), serde_json::Value::Bool(true));
                        }
                        if let Some(pane) = &pane_name {
                            payload.insert("pane".into(), serde_json::Value::String(pane.clone()));
                        }
                        serde_json::Value::Object(payload).to_string()
                    }));
                }
                if actions.is_empty() {
                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::Run(actions))
                }
            }
            RuntimeAction::InputHistoryChanged { key, entries } => {
                // The UI's history update: write the mirror the history read op
                // consults. Unconditional — history changes per submission, not
                // per keystroke, so there is no interest gate to check.
                self.input_mirror
                    .lock()
                    .unwrap()
                    .apply_history(key, entries);
                Ok(ActionResult::None)
            }
            RuntimeAction::InputWordSetsChanged { key } => {
                // Push one input's merged word sets to the UI. The merge reads the
                // live sets at dispatch — a burst of registry calls coalesced onto
                // this one action all ride the same (final) view — and clearing the
                // pending flag re-arms the ops' queue-on-flip.
                let merged = {
                    let mut sets = self.input_word_sets.lock().unwrap();
                    sets.take_push(key);
                    sets.merged(key)
                };
                self.ui_tx
                    .send(TaggedSessionEvent {
                        session_id: self.session_id,
                        event: SessionEvent::InputWordSets {
                            key,
                            suggestions: Arc::new(merged.suggestions),
                            blacklist: Arc::new(merged.blacklist),
                        },
                    })
                    .await?;
                Ok(ActionResult::None)
            }
            RuntimeAction::ApplySettings {
                command_separator,
                raw_line_prefix,
                log_enabled,
                script_settings,
            } => {
                self.trigger_manager
                    .set_command_separator(command_separator.clone());
                self.command_separator = command_separator;
                self.raw_line_prefix = raw_line_prefix;
                self.set_log_enabled(log_enabled);
                // Refresh the script-visible snapshot (`getSettings()`) including the
                // UI-resolved palette.
                *self.settings_snapshot.borrow_mut() = *script_settings;
                Ok(ActionResult::None)
            }
            RuntimeAction::Reload => Ok(ActionResult::Reload),
            RuntimeAction::Shutdown => Ok(ActionResult::CloseSession),
            RuntimeAction::Noop => Ok(ActionResult::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{prepare_pane_open, send_link_tooltip};
    use crate::session::SessionId;
    use crate::session::runtime::pane::{
        DefStateSpec, MAIN_PANE_KEY, PaneKind, PaneNamespace, PanePlacement, PaneRegistry,
        SplitDirection,
    };
    use crate::session::runtime::{IsolateId, RuntimeAction};
    use crate::session::styled_line::LinkTooltipState;

    #[test]
    fn failed_tooltip_forward_resolves_the_loading_state() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let state = Arc::new(LinkTooltipState::default());
        assert!(state.begin_request());

        assert!(!send_link_tooltip(
            &tx,
            RuntimeAction::ResolveLinkTooltip {
                session: SessionId::from(7),
                isolate: IsolateId::Main,
                instance: 1,
                id: 2,
                state: Arc::clone(&state),
            },
        ));
        assert!(!state.is_loading());
        assert!(state.text().is_none());
    }

    #[test]
    fn retired_own_open_is_preserved_but_retired_foreign_open_is_dropped() {
        let registry = Arc::new(Mutex::new(PaneRegistry::new()));
        let namespace = PaneNamespace::User;
        let def = registry
            .lock()
            .unwrap()
            .split(
                &namespace,
                "chat",
                PaneKind::Terminal,
                DefStateSpec::default(),
                None,
            )
            .unwrap()
            .def;
        let placement = PanePlacement::Split {
            reference: MAIN_PANE_KEY,
            direction: SplitDirection::Right,
            size_px: None,
        };
        registry.lock().unwrap().close(&namespace, "chat").unwrap();

        let own = prepare_pane_open(&registry, def.clone(), placement, false)
            .expect("an own-runtime open keeps its ordered UI history");
        assert_eq!(own.0, def);
        assert!(prepare_pane_open(&registry, def, placement, true).is_none());
    }

    #[test]
    fn foreign_open_uses_current_def_and_falls_back_from_a_retired_reference() {
        let registry = Arc::new(Mutex::new(PaneRegistry::new()));
        let namespace = PaneNamespace::User;
        let (original, reference) = {
            let mut registry = registry.lock().unwrap();
            let original = registry
                .split(
                    &namespace,
                    "chat",
                    PaneKind::Terminal,
                    DefStateSpec::default(),
                    None,
                )
                .unwrap()
                .def;
            let reference = registry
                .split(
                    &namespace,
                    "reference",
                    PaneKind::Terminal,
                    DefStateSpec::default(),
                    None,
                )
                .unwrap()
                .def;
            registry
                .split(
                    &namespace,
                    "chat",
                    PaneKind::Terminal,
                    DefStateSpec {
                        hidden: Some(true),
                        ..DefStateSpec::default()
                    },
                    None,
                )
                .unwrap();
            registry.close(&namespace, "reference").unwrap();
            (original, reference)
        };

        let prepared = prepare_pane_open(
            &registry,
            original,
            PanePlacement::Split {
                reference: reference.key,
                direction: SplitDirection::Left,
                size_px: Some(240.0),
            },
            true,
        )
        .expect("the foreign target remains live");
        assert!(prepared.0.hidden);
        assert_eq!(
            prepared.1,
            PanePlacement::Split {
                reference: MAIN_PANE_KEY,
                direction: SplitDirection::Left,
                size_px: Some(240.0),
            }
        );
    }
}
