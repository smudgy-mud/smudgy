//! Storage-neutral state machine for the named-layout dialog.
//!
//! Updates emit typed effects. A host may complete them in the same event
//! turn (native filesystem) or deliver a completion later (browser storage).
//! Request IDs prevent a late completion from acting on a newer form.

use smudgy_session_model::{SessionId, layout_template::TemplateSource, naming};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmittedRow {
    pub session: SessionId,
    pub label: String,
    pub close: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SaveOutcome {
    Saved { name: String, omitted: usize },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverwriteAction {
    Save,
    Rename { from: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Browse,
    SaveAs {
        name: String,
        error: Option<String>,
    },
    ConfirmOverwrite {
        name: String,
        action: OverwriteAction,
    },
    Rename {
        from: String,
        to: String,
        error: Option<String>,
    },
    ConfirmDelete {
        name: String,
    },
    ConfirmReset,
    KeepOrClose {
        source: TemplateSource,
        rows: Vec<OmittedRow>,
    },
}

#[derive(Debug)]
pub struct State {
    pub server: String,
    pub layouts: Vec<String>,
    pub stage: Stage,
    pub outcome: Option<SaveOutcome>,
    pub storage_error: Option<String>,
    next_request: u64,
    pending: Option<(u64, bool)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    ApplyPressed(String),
    SaveAsPressed,
    NameChanged(String),
    SaveAsConfirmed,
    OverwritePressed(String),
    OverwriteConfirmed,
    RenamePressed(String),
    RenameConfirmed,
    DeletePressed(String),
    DeleteConfirmed,
    ResetPressed,
    ResetConfirmed,
    Back,
    ToggleClose(SessionId, bool),
    KeepOrCloseConfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Apply {
        name: String,
    },
    ApplyWithAnswers {
        source: TemplateSource,
        close: Vec<SessionId>,
        keep: Vec<SessionId>,
    },
    Save {
        name: String,
        replace: bool,
    },
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    CheckName {
        id: u64,
        name: String,
    },
    Rename {
        id: u64,
        from: String,
        to: String,
        replace: bool,
    },
    Delete {
        id: u64,
        name: String,
    },
    Refresh {
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    NameChecked {
        id: u64,
        result: Result<bool, String>,
    },
    Renamed {
        id: u64,
        result: Result<Vec<String>, String>,
    },
    Deleted {
        id: u64,
        result: Result<Vec<String>, String>,
    },
    Refreshed {
        id: u64,
        result: Result<Vec<String>, String>,
    },
}

#[derive(Debug, Default)]
pub struct Transition {
    pub event: Option<Event>,
    pub effect: Option<Effect>,
    pub focus_name: bool,
}

impl Transition {
    fn event(event: Event) -> Self {
        Self {
            event: Some(event),
            ..Self::default()
        }
    }

    fn effect(effect: Effect) -> Self {
        Self {
            effect: Some(effect),
            ..Self::default()
        }
    }
}

impl State {
    #[must_use]
    pub fn new(server: String, layouts: Vec<String>) -> Self {
        Self {
            server,
            layouts,
            stage: Stage::Browse,
            outcome: None,
            storage_error: None,
            next_request: 0,
            pending: None,
        }
    }

    #[must_use]
    pub fn is_browsing(&self) -> bool {
        matches!(self.stage, Stage::Browse)
    }

    pub fn prompt_keep_or_close(&mut self, source: TemplateSource, rows: Vec<OmittedRow>) {
        self.pending = None;
        self.stage = Stage::KeepOrClose { source, rows };
    }

    pub fn record_save_outcome(&mut self, outcome: SaveOutcome) -> Transition {
        self.outcome = Some(outcome);
        Transition::effect(self.request(|id| Effect::Refresh { id }))
    }

    fn request(&mut self, make: impl FnOnce(u64) -> Effect) -> Effect {
        self.next_request = self.next_request.wrapping_add(1);
        let id = self.next_request;
        let effect = make(id);
        self.pending = Some((
            id,
            matches!(
                effect,
                Effect::Rename { .. } | Effect::Delete { .. } | Effect::Refresh { .. }
            ),
        ));
        effect
    }

    fn clear_pending(&mut self) {
        self.pending = None;
    }

    pub fn update(&mut self, message: Message) -> Transition {
        // A submitted mutation or its subsequent listing refresh cannot be
        // cancelled. Keep its completion authoritative until it settles.
        if self.pending.is_some_and(|(_, mutating)| mutating) {
            return Transition::default();
        }
        use Message as M;
        match message {
            M::ApplyPressed(name) => Transition::event(Event::Apply { name }),
            M::SaveAsPressed => {
                self.clear_pending();
                self.storage_error = None;
                self.stage = Stage::SaveAs {
                    name: String::new(),
                    error: None,
                };
                self.outcome = None;
                Transition {
                    focus_name: true,
                    ..Transition::default()
                }
            }
            M::NameChanged(value) => {
                self.clear_pending();
                match &mut self.stage {
                    Stage::SaveAs { name, error } => {
                        *name = value;
                        *error = None;
                    }
                    Stage::Rename { to, error, .. } => {
                        *to = value;
                        *error = None;
                    }
                    _ => {}
                }
                Transition::default()
            }
            M::SaveAsConfirmed => {
                let Stage::SaveAs { name, error } = &mut self.stage else {
                    return Transition::default();
                };
                let candidate = name.trim().to_string();
                if let Err(reason) = naming::validate_name(&candidate) {
                    *error = Some(reason);
                    return Transition::default();
                }
                Transition::effect(self.request(|id| Effect::CheckName {
                    id,
                    name: candidate,
                }))
            }
            M::OverwritePressed(name) => {
                self.clear_pending();
                self.storage_error = None;
                self.stage = Stage::ConfirmOverwrite {
                    name,
                    action: OverwriteAction::Save,
                };
                self.outcome = None;
                Transition::default()
            }
            M::OverwriteConfirmed => {
                let Stage::ConfirmOverwrite { name, action } = &self.stage else {
                    return Transition::default();
                };
                let (name, action) = (name.clone(), action.clone());
                match action {
                    OverwriteAction::Save => {
                        self.stage = Stage::Browse;
                        Transition::event(Event::Save {
                            name,
                            replace: true,
                        })
                    }
                    OverwriteAction::Rename { from } => self.rename(from, name, true),
                }
            }
            M::RenamePressed(name) => {
                self.clear_pending();
                self.storage_error = None;
                self.stage = Stage::Rename {
                    from: name.clone(),
                    to: name,
                    error: None,
                };
                self.outcome = None;
                Transition {
                    focus_name: true,
                    ..Transition::default()
                }
            }
            M::RenameConfirmed => {
                let Stage::Rename { from, to, error } = &mut self.stage else {
                    return Transition::default();
                };
                let target = to.trim().to_string();
                if let Err(reason) = naming::validate_name(&target) {
                    *error = Some(reason);
                    return Transition::default();
                }
                let from = from.clone();
                if naming::names_conflict(&from, &target) {
                    self.rename(from, target, false)
                } else {
                    Transition::effect(self.request(|id| Effect::CheckName { id, name: target }))
                }
            }
            M::DeletePressed(name) => {
                self.clear_pending();
                self.storage_error = None;
                self.stage = Stage::ConfirmDelete { name };
                self.outcome = None;
                Transition::default()
            }
            M::DeleteConfirmed => {
                let Stage::ConfirmDelete { name } = &self.stage else {
                    return Transition::default();
                };
                let name = name.clone();
                Transition::effect(self.request(|id| Effect::Delete { id, name }))
            }
            M::ResetPressed => {
                self.clear_pending();
                self.storage_error = None;
                self.stage = Stage::ConfirmReset;
                self.outcome = None;
                Transition::default()
            }
            M::ResetConfirmed => {
                self.stage = Stage::Browse;
                Transition::event(Event::Reset)
            }
            M::Back => {
                self.clear_pending();
                self.stage = Stage::Browse;
                Transition::default()
            }
            M::ToggleClose(session, close) => {
                if let Stage::KeepOrClose { rows, .. } = &mut self.stage
                    && let Some(row) = rows.iter_mut().find(|row| row.session == session)
                {
                    row.close = close;
                }
                Transition::default()
            }
            M::KeepOrCloseConfirmed => {
                let Stage::KeepOrClose { source, rows } = &self.stage else {
                    return Transition::default();
                };
                let event = Event::ApplyWithAnswers {
                    source: source.clone(),
                    close: rows
                        .iter()
                        .filter(|row| row.close)
                        .map(|row| row.session)
                        .collect(),
                    keep: rows
                        .iter()
                        .filter(|row| !row.close)
                        .map(|row| row.session)
                        .collect(),
                };
                self.stage = Stage::Browse;
                Transition::event(event)
            }
        }
    }

    fn rename(&mut self, from: String, to: String, replace: bool) -> Transition {
        Transition::effect(self.request(|id| Effect::Rename {
            id,
            from,
            to,
            replace,
        }))
    }

    pub fn complete(&mut self, completion: Completion) -> Transition {
        let id = match &completion {
            Completion::NameChecked { id, .. }
            | Completion::Renamed { id, .. }
            | Completion::Deleted { id, .. }
            | Completion::Refreshed { id, .. } => *id,
        };
        if self.pending.map(|(pending, _)| pending) != Some(id) {
            return Transition::default();
        }
        self.clear_pending();
        match completion {
            Completion::NameChecked { result, .. } => {
                let exists = match result {
                    Ok(exists) => exists,
                    Err(reason) => {
                        match &mut self.stage {
                            Stage::SaveAs { error, .. } | Stage::Rename { error, .. } => {
                                *error = Some(reason);
                            }
                            _ => {}
                        }
                        return Transition::default();
                    }
                };
                let (name, action) = match &self.stage {
                    Stage::SaveAs { name, .. } => (name.trim().to_string(), OverwriteAction::Save),
                    Stage::Rename { from, to, .. } => (
                        to.trim().to_string(),
                        OverwriteAction::Rename { from: from.clone() },
                    ),
                    _ => return Transition::default(),
                };
                if exists {
                    self.stage = Stage::ConfirmOverwrite { name, action };
                    Transition::default()
                } else {
                    match action {
                        OverwriteAction::Save => {
                            self.stage = Stage::Browse;
                            Transition::event(Event::Save {
                                name,
                                replace: false,
                            })
                        }
                        OverwriteAction::Rename { from } => self.rename(from, name, false),
                    }
                }
            }
            Completion::Renamed { result, .. } => {
                match result {
                    Ok(layouts) => {
                        self.layouts = layouts;
                        self.stage = Stage::Browse;
                    }
                    Err(error) => {
                        let (from, to) = match &self.stage {
                            Stage::Rename { from, to, .. } => (from.clone(), to.clone()),
                            Stage::ConfirmOverwrite {
                                name,
                                action: OverwriteAction::Rename { from },
                            } => (from.clone(), name.clone()),
                            _ => return Transition::default(),
                        };
                        self.stage = Stage::Rename {
                            from,
                            to,
                            error: Some(error),
                        };
                    }
                }
                Transition::default()
            }
            Completion::Deleted { result, .. } | Completion::Refreshed { result, .. } => {
                match result {
                    Ok(layouts) => {
                        self.layouts = layouts;
                        self.storage_error = None;
                    }
                    Err(error) => self.storage_error = Some(error),
                }
                self.stage = Stage::Browse;
                Transition::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_id(transition: Transition) -> u64 {
        let Some(Effect::CheckName { id, .. }) = transition.effect else {
            panic!("name check requested");
        };
        id
    }

    #[test]
    fn shared_save_flow_waits_for_host_answer() {
        let mut state = State::new("Arctic".into(), vec!["combat".into()]);
        state.update(Message::SaveAsPressed);
        state.update(Message::NameChanged("Combat".into()));
        let id = checked_id(state.update(Message::SaveAsConfirmed));
        assert!(matches!(state.stage, Stage::SaveAs { .. }));
        let answer = state.complete(Completion::NameChecked {
            id,
            result: Ok(true),
        });
        assert!(answer.event.is_none());
        assert!(matches!(state.stage, Stage::ConfirmOverwrite { .. }));
        assert_eq!(
            state.update(Message::OverwriteConfirmed).event,
            Some(Event::Save {
                name: "Combat".into(),
                replace: true,
            })
        );
    }

    #[test]
    fn stale_name_check_cannot_submit_a_changed_form() {
        let mut state = State::new("Arctic".into(), Vec::new());
        state.update(Message::SaveAsPressed);
        state.update(Message::NameChanged("old".into()));
        let old = checked_id(state.update(Message::SaveAsConfirmed));
        state.update(Message::NameChanged("new".into()));
        let new = checked_id(state.update(Message::SaveAsConfirmed));
        assert_ne!(old, new);
        assert!(
            state
                .complete(Completion::NameChecked {
                    id: old,
                    result: Ok(false)
                })
                .event
                .is_none()
        );
        assert!(matches!(&state.stage, Stage::SaveAs { name, .. } if name == "new"));
        assert_eq!(
            state
                .complete(Completion::NameChecked {
                    id: new,
                    result: Ok(false)
                })
                .event,
            Some(Event::Save {
                name: "new".into(),
                replace: false,
            })
        );
    }

    #[test]
    fn back_invalidates_a_delayed_check() {
        let mut state = State::new("Arctic".into(), Vec::new());
        state.update(Message::SaveAsPressed);
        state.update(Message::NameChanged("combat".into()));
        let id = checked_id(state.update(Message::SaveAsConfirmed));
        state.update(Message::Back);
        assert!(
            state
                .complete(Completion::NameChecked {
                    id,
                    result: Ok(false)
                })
                .event
                .is_none()
        );
        assert_eq!(state.stage, Stage::Browse);
    }

    #[test]
    fn failed_storage_check_cannot_turn_into_an_unconfirmed_save() {
        let mut state = State::new("Arctic".into(), Vec::new());
        state.update(Message::SaveAsPressed);
        state.update(Message::NameChanged("combat".into()));
        let id = checked_id(state.update(Message::SaveAsConfirmed));
        let transition = state.complete(Completion::NameChecked {
            id,
            result: Err("IndexedDB is unavailable".into()),
        });
        assert!(transition.event.is_none());
        assert!(matches!(
            &state.stage,
            Stage::SaveAs { error: Some(error), .. } if error == "IndexedDB is unavailable"
        ));
    }

    #[test]
    fn submitted_delete_cannot_be_cancelled_or_superseded() {
        let mut state = State::new("Arctic".into(), vec!["combat".into()]);
        state.update(Message::DeletePressed("combat".into()));
        let transition = state.update(Message::DeleteConfirmed);
        let Some(Effect::Delete { id, .. }) = transition.effect else {
            panic!("delete requested");
        };
        state.update(Message::Back);
        state.update(Message::SaveAsPressed);
        assert_eq!(
            state.stage,
            Stage::ConfirmDelete {
                name: "combat".into()
            }
        );
        state.complete(Completion::Deleted {
            id,
            result: Ok(Vec::new()),
        });
        assert_eq!(state.stage, Stage::Browse);
        assert!(state.layouts.is_empty());
    }

    #[test]
    fn failed_delete_keeps_the_listing_and_reports_the_error() {
        let mut state = State::new("Arctic".into(), vec!["combat".into()]);
        state.update(Message::DeletePressed("combat".into()));
        let Some(Effect::Delete { id, .. }) = state.update(Message::DeleteConfirmed).effect else {
            panic!("delete requested");
        };
        state.complete(Completion::Deleted {
            id,
            result: Err("IndexedDB transaction failed".into()),
        });
        assert_eq!(state.stage, Stage::Browse);
        assert_eq!(state.layouts, vec!["combat"]);
        assert_eq!(
            state.storage_error.as_deref(),
            Some("IndexedDB transaction failed")
        );
    }
}
