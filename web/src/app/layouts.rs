//! Browser host for the shared named-layout dialog and one-window workspace.

use std::collections::HashMap;

use super::{
    Completion, CredentialRequest, Effect, Labels, LayoutEvent, Message, SaveOutcome,
    SessionDefinition, SessionId, SmudgyWeb, StartupSend, Task, TemplateSource, UiMessage, dto,
    layouts_modal,
};

struct LayoutExecution {
    workspace: dto::Workspace,
    matched: Vec<SessionId>,
    slot_to_session: HashMap<u64, SessionId>,
    to_spawn: Vec<(dto::SessionSlot, SessionDefinition)>,
    close: Vec<SessionId>,
    retained: Vec<dto::Cluster>,
}

/// Reconcile prompt answers with the *current* omitted set. A session may
/// become matched while the prompt is open; stale answers must never close it.
fn chosen_omitted(
    omitted: &[SessionId],
    answers: Option<(&[SessionId], &[SessionId])>,
) -> Option<(Vec<SessionId>, Vec<SessionId>)> {
    if omitted.is_empty() {
        return Some((Vec::new(), Vec::new()));
    }
    let (close, keep) = answers?;
    if omitted
        .iter()
        .any(|id| !close.contains(id) && !keep.contains(id))
    {
        return None;
    }
    Some((
        omitted
            .iter()
            .copied()
            .filter(|id| close.contains(id))
            .collect(),
        omitted
            .iter()
            .copied()
            .filter(|id| keep.contains(id) && !close.contains(id))
            .collect(),
    ))
}

pub(super) struct WebLabels(pub(super) smudgy_i18n::Translator);

impl Labels for WebLabels {
    fn text(&self, id: &'static str) -> String {
        self.0.translate(id)
    }

    fn format(&self, id: &'static str, args: &[(&'static str, &str)]) -> String {
        let mut fluent = smudgy_i18n::FluentArgs::new();
        for &(key, value) in args {
            fluent.set(key, value);
        }
        self.0.translate_with(id, &fluent)
    }
}

impl SmudgyWeb {
    pub(super) fn handle_layout_message(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::LayoutsLoaded(nonce, server, result) => {
                if nonce == self.layout_nonce {
                    let mut state = layouts_modal::State::new(server, Vec::new());
                    match result {
                        Ok(names) => state.layouts = names,
                        Err(error) => state.storage_error = Some(error),
                    }
                    self.layouts = Some(state);
                }
                Task::none()
            }
            Message::Layouts(message) => {
                let Some(state) = self.layouts.as_mut() else {
                    return Task::none();
                };
                let transition = state.update(message);
                self.drive_layouts(transition)
            }
            Message::LayoutEffectCompleted(nonce, completion) => {
                if nonce != self.layout_nonce {
                    return Task::none();
                }
                let Some(state) = self.layouts.as_mut() else {
                    return Task::none();
                };
                let transition = state.complete(completion);
                self.drive_layouts(transition)
            }
            Message::LayoutSaved(nonce, name, result) => {
                if nonce != self.layout_nonce {
                    return Task::none();
                }
                let Some(state) = self.layouts.as_mut() else {
                    return Task::none();
                };
                let outcome = match result {
                    Ok(()) => SaveOutcome::Saved { name, omitted: 0 },
                    Err(error) => SaveOutcome::Failed { error },
                };
                let transition = state.record_save_outcome(outcome);
                self.drive_layouts(transition)
            }
            Message::LayoutToApply(nonce, name, result) => {
                if nonce != self.layout_nonce {
                    return Task::none();
                }
                self.apply_loaded_layout(name, result)
            }
            Message::CloseLayouts => {
                self.layout_nonce = self.layout_nonce.wrapping_add(1);
                self.layouts = None;
                self.pending_layout = None;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub(super) fn open_layouts(&mut self) -> Task<Message> {
        let Some(server) = self
            .ui
            .active_session()
            .and_then(|id| self.session_definitions.get(&id))
            .map(|definition| definition.server.clone())
        else {
            return Task::none();
        };
        self.ui.handle_chrome(&UiMessage::ClosePanel);
        self.layout_nonce = self.layout_nonce.wrapping_add(1);
        self.layouts = None;
        self.pending_layout = None;
        let nonce = self.layout_nonce;
        Task::perform(
            async move {
                let result = crate::storage::list_layouts(&server).await;
                (server, result)
            },
            move |(server, result)| Message::LayoutsLoaded(nonce, server, result),
        )
    }

    pub(super) fn drive_layouts(&mut self, transition: layouts_modal::Transition) -> Task<Message> {
        let mut tasks = Vec::new();
        if transition.focus_name {
            tasks.push(
                iced::widget::operation::focus(smudgy_ui_shared::layouts_view::name_input_id())
                    .map(Message::Layouts),
            );
        }
        if let Some(effect) = transition.effect {
            tasks.push(self.run_layout_effect(effect));
        }
        if let Some(event) = transition.event {
            tasks.push(self.handle_layout_event(event));
        }
        Task::batch(tasks)
    }

    fn run_layout_effect(&self, effect: Effect) -> Task<Message> {
        let Some(state) = self.layouts.as_ref() else {
            return Task::none();
        };
        let server = state.server.clone();
        let nonce = self.layout_nonce;
        Task::perform(
            async move {
                match effect {
                    Effect::CheckName { id, name } => Completion::NameChecked {
                        id,
                        result: crate::storage::layout_exists(&server, &name).await,
                    },
                    Effect::Rename {
                        id,
                        from,
                        to,
                        replace,
                    } => Completion::Renamed {
                        id,
                        result: crate::storage::rename_layout(&server, &from, &to, replace).await,
                    },
                    Effect::Delete { id, name } => Completion::Deleted {
                        id,
                        result: crate::storage::delete_layout(&server, &name).await,
                    },
                    Effect::Refresh { id } => Completion::Refreshed {
                        id,
                        result: crate::storage::list_layouts(&server).await,
                    },
                }
            },
            move |completion| Message::LayoutEffectCompleted(nonce, completion),
        )
    }

    fn handle_layout_event(&mut self, event: LayoutEvent) -> Task<Message> {
        match event {
            LayoutEvent::Save { name, replace } => {
                let Some(state) = self.layouts.as_ref() else {
                    return Task::none();
                };
                let server = state.server.clone();
                let workspace = match self.capture_workspace() {
                    Ok(workspace) => workspace,
                    Err(error) => {
                        let transition = self
                            .layouts
                            .as_mut()
                            .expect("the layouts dialog is open")
                            .record_save_outcome(SaveOutcome::Failed { error });
                        return self.drive_layouts(transition);
                    }
                };
                let nonce = self.layout_nonce;
                Task::perform(
                    async move {
                        let result =
                            crate::storage::save_layout(&server, &name, workspace, replace).await;
                        (name, result)
                    },
                    move |(name, result)| Message::LayoutSaved(nonce, name, result),
                )
            }
            LayoutEvent::Apply { name } => {
                let Some(state) = self.layouts.as_ref() else {
                    return Task::none();
                };
                let server = state.server.clone();
                let nonce = self.layout_nonce;
                Task::perform(
                    async move {
                        let result = crate::storage::load_layout(&server, &name).await;
                        (name, result)
                    },
                    move |(name, result)| Message::LayoutToApply(nonce, name, result),
                )
            }
            LayoutEvent::ApplyWithAnswers {
                source,
                close,
                keep,
            } => {
                let Some((name, _)) = self.pending_layout.as_ref() else {
                    return Task::none();
                };
                if source != TemplateSource::Named(name.clone()) {
                    return Task::none();
                }
                self.apply_pending_layout(Some((&close, &keep)))
            }
            LayoutEvent::Reset => {
                self.ui.reset_layout();
                self.save_window();
                self.layouts = None;
                Task::none()
            }
        }
    }

    pub(super) fn capture_workspace(&self) -> Result<dto::Workspace, String> {
        let mut sessions = Vec::new();
        for id in self.ui.session_ids() {
            let definition = self
                .session_definitions
                .get(&id)
                .ok_or_else(|| format!("session {id} has no saved connection identity"))?;
            sessions.push(dto::SessionSlot {
                id: u64::from(u32::from(id)),
                server: definition.server.clone(),
                profile: definition.profile.clone(),
                connect: definition.restore_connect,
            });
        }
        if sessions.is_empty() {
            return Err("Open a session before saving a layout".into());
        }
        let clusters = self.ui.snapshot_clusters(|id| {
            self.session_definitions
                .contains_key(&id)
                .then(|| u64::from(u32::from(id)))
        });
        dto::Workspace {
            version: dto::SCHEMA_VERSION,
            sessions,
            windows: vec![dto::Window {
                id: 1,
                geometry: dto::Geometry::default(),
                maximized: false,
                active_slot: self.ui.active_session().map(|id| u64::from(u32::from(id))),
                clusters,
            }],
        }
        .sanitized_single_window()
        .map_err(str::to_owned)
    }

    fn layout_error(&mut self, error: impl Into<String>) {
        if let Some(state) = self.layouts.as_mut() {
            state.storage_error = Some(error.into());
        }
    }

    pub(super) fn apply_loaded_layout(
        &mut self,
        name: String,
        result: Result<Option<dto::Workspace>, String>,
    ) -> Task<Message> {
        let workspace = match result {
            Ok(Some(workspace)) => workspace,
            Ok(None) => {
                self.layout_error(format!("Layout '{name}' no longer exists"));
                return Task::none();
            }
            Err(error) => {
                self.layout_error(error);
                return Task::none();
            }
        };
        self.pending_layout = Some((name, workspace));
        self.apply_pending_layout(None)
    }

    fn apply_pending_layout(
        &mut self,
        answers: Option<(&[SessionId], &[SessionId])>,
    ) -> Task<Message> {
        let Some((name, workspace)) = self.pending_layout.clone() else {
            return Task::none();
        };
        let Some(window) = workspace.windows.first() else {
            self.layout_error("Layout has no window");
            return Task::none();
        };
        // Validate renderability before changing a live session. The common
        // format may contain native script panes which this browser surface
        // cannot yet host; never discard them silently.
        if let Err(error) = super::ui::validate_browser_clusters(&window.clusters) {
            self.layout_error(error);
            return Task::none();
        }

        let live: Vec<SessionId> = self.ui.session_ids().collect();
        let mut matched = Vec::new();
        let mut slot_to_session = HashMap::new();
        let mut to_spawn = Vec::new();
        for slot in &workspace.sessions {
            let existing = live.iter().copied().find(|id| {
                !matched.contains(id)
                    && self.session_definitions.get(id).is_some_and(|definition| {
                        definition.server == slot.server && definition.profile == slot.profile
                    })
            });
            if let Some(id) = existing {
                matched.push(id);
                slot_to_session.insert(slot.id, id);
            } else if let Some(request) = self
                .ui
                .manager
                .catalog
                .resolve_connection(&slot.server, &slot.profile)
            {
                to_spawn.push((slot.clone(), SessionDefinition::from(request)));
            } else {
                self.layout_error(format!(
                    "Saved server/profile {}/{} is unavailable",
                    slot.server, slot.profile
                ));
                return Task::none();
            }
        }
        let omitted: Vec<_> = live
            .into_iter()
            .filter(|id| !matched.contains(id))
            .collect();
        let Some((close, keep)) = chosen_omitted(&omitted, answers) else {
            self.prompt_omitted(name, &omitted);
            return Task::none();
        };

        let retained = if keep.is_empty() {
            Vec::new()
        } else {
            let mut current = match self.capture_workspace() {
                Ok(current) => current,
                Err(error) => {
                    self.layout_error(error);
                    return Task::none();
                }
            };
            current.sessions.retain(|slot| {
                u32::try_from(slot.id)
                    .ok()
                    .map(SessionId::from)
                    .is_some_and(|id| keep.contains(&id))
            });
            current
                .sanitized()
                .windows
                .into_iter()
                .flat_map(|window| window.clusters)
                .collect()
        };

        self.execute_layout(LayoutExecution {
            workspace,
            matched,
            slot_to_session,
            to_spawn,
            close,
            retained,
        })
    }

    fn execute_layout(&mut self, plan: LayoutExecution) -> Task<Message> {
        let LayoutExecution {
            workspace,
            matched,
            mut slot_to_session,
            to_spawn,
            close,
            retained,
        } = plan;
        let window = &workspace.windows[0];
        for (slot, mut definition) in to_spawn {
            definition.restore_connect = slot.connect;
            let id = self.ui.open_session(definition.endpoint.clone());
            self.ui.set_session_disconnected(id);
            self.session_definitions.insert(id, definition);
            slot_to_session.insert(slot.id, id);
        }

        let mut blueprints = Vec::new();
        for cluster in &window.clusters {
            let blueprint = super::ui::blueprint_from_dto(&cluster.root, &|slot| {
                slot_to_session.get(&slot).copied()
            })
            .expect("validated layout slots were resolved before mutation");
            blueprints.push((cluster.weight, blueprint));
        }
        for cluster in &retained {
            let blueprint = super::ui::blueprint_from_dto(&cluster.root, &|slot| {
                u32::try_from(slot).ok().map(SessionId::from)
            })
            .expect("retained browser panes are renderable");
            blueprints.push((cluster.weight, blueprint));
        }
        for id in close {
            if let Some(worker) = self.sessions.remove(&id) {
                let _ = worker.disconnect();
            }
            let _ = self.ui.close_session(id);
            self.session_definitions.remove(&id);
        }
        let active = window
            .active_slot
            .and_then(|slot| slot_to_session.get(&slot).copied())
            .or_else(|| self.ui.active_session());
        self.ui.install_blueprint(blueprints, active);
        self.save_window();

        let mut tasks = Vec::new();
        for slot in &workspace.sessions {
            let id = slot_to_session[&slot.id];
            if matched.contains(&id) || !slot.connect {
                continue;
            }
            let definition = &self.session_definitions[&id];
            if definition.send_on_connect.contains("$PASSWORD") {
                tasks.push(self.request_password(CredentialRequest::Reconnect(id, 0)));
            } else {
                let send = definition.send_on_connect.clone();
                self.reconnect_session(id, &StartupSend::plain(send));
            }
        }
        self.pending_layout = None;
        self.layouts = None;
        self.layout_nonce = self.layout_nonce.wrapping_add(1);
        Task::batch(tasks)
    }

    fn prompt_omitted(&mut self, name: String, omitted: &[SessionId]) {
        let rows = omitted
            .iter()
            .filter_map(|id| {
                let definition = self.session_definitions.get(id)?;
                Some(layouts_modal::OmittedRow {
                    session: *id,
                    label: format!("{} @ {}", definition.profile, definition.server),
                    close: false,
                })
            })
            .collect();
        if let Some(state) = self.layouts.as_mut() {
            state.prompt_keep_or_close(TemplateSource::Named(name), rows);
        }
    }
}
