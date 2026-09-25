//! Native Layouts adapter for the shared dialog.
//!
//! The state machine and its typed storage requests live in ui_shared. This
//! adapter resolves each request against the native layout store in the same
//! event turn, preserving synchronous desktop behavior.

use std::path::{Path, PathBuf};

use iced::Task;
use iced::widget::operation;

use crate::theme::Element;
use crate::workspace::layouts;

#[cfg(test)]
use smudgy_session_model::SessionId;
use smudgy_session_model::layout_template::TemplateSource;
use smudgy_ui_shared::layouts_modal::{Completion, Effect, State as SharedState, Transition};
pub use smudgy_ui_shared::layouts_modal::{Event, Message, OmittedRow, SaveOutcome};
#[cfg(test)]
use smudgy_ui_shared::layouts_modal::{OverwriteAction, Stage};
use smudgy_ui_shared::layouts_view::Labels;

struct NativeLabels;

impl Labels for NativeLabels {
    fn text(&self, id: &'static str) -> String {
        crate::i18n::translate(id)
    }

    fn format(&self, id: &'static str, args: &[(&'static str, &str)]) -> String {
        let mut fluent = smudgy_i18n::FluentArgs::new();
        for &(key, value) in args {
            fluent.set(key, value);
        }
        crate::i18n::translate_with(id, &fluent)
    }
}

#[derive(Debug)]
pub struct State {
    model: SharedState,
    dir: Option<PathBuf>,
}

impl State {
    #[must_use]
    pub fn opening(server: String) -> Self {
        let dir = layouts::layouts_dir(&server);
        Self::with_dir(server, dir)
    }

    #[must_use]
    pub fn with_dir(server: String, dir: Option<PathBuf>) -> Self {
        let names = dir.as_deref().map(layouts::list_in).unwrap_or_default();
        Self {
            model: SharedState::new(server, names),
            dir,
        }
    }

    #[must_use]
    pub fn server(&self) -> &str {
        &self.model.server
    }

    #[must_use]
    pub fn is_browsing(&self) -> bool {
        self.model.is_browsing()
    }

    pub fn prompt_keep_or_close(&mut self, source: TemplateSource, rows: Vec<OmittedRow>) {
        self.model.prompt_keep_or_close(source, rows);
    }

    pub fn record_save_outcome(&mut self, outcome: SaveOutcome) {
        let transition = self.model.record_save_outcome(outcome);
        let _ = drive(
            &mut self.model,
            NativeStore {
                dir: self.dir.as_deref(),
            },
            transition,
        );
    }
}

/// The native implementation of the shared layout effects. It is a concrete
/// value passed to a generic interpreter: no trait object or virtual call is
/// introduced into the desktop update path.
trait LayoutHost {
    fn execute(&self, effect: Effect) -> Completion;
}

struct NativeStore<'a> {
    dir: Option<&'a Path>,
}

impl LayoutHost for NativeStore<'_> {
    fn execute(&self, effect: Effect) -> Completion {
        match effect {
            Effect::CheckName { id, name } => Completion::NameChecked {
                id,
                result: Ok(self.dir.is_some_and(|dir| layouts::exists_in(dir, &name))),
            },
            Effect::Rename { id, from, to, .. } => {
                let result = match self.dir {
                    Some(dir) => {
                        layouts::rename_in(dir, &from, &to).map(|()| layouts::list_in(dir))
                    }
                    None => Err(layouts::LayoutStoreError::OutsideStore),
                };
                if let Err(error) = &result {
                    log::warn!("[layouts] rename of '{from}' to '{to}' failed: {error}");
                }
                Completion::Renamed {
                    id,
                    result: result.map_err(|error| error.to_string()),
                }
            }
            Effect::Delete { id, name } => {
                let result = match self.dir {
                    Some(dir) => layouts::delete_in(dir, &name).map(|()| layouts::list_in(dir)),
                    None => Err(layouts::LayoutStoreError::OutsideStore),
                };
                if let Err(error) = &result {
                    log::warn!("[layouts] delete of '{name}' failed: {error}");
                }
                Completion::Deleted {
                    id,
                    result: result.map_err(|error| error.to_string()),
                }
            }
            Effect::Refresh { id } => Completion::Refreshed {
                id,
                result: Ok(self.dir.map(layouts::list_in).unwrap_or_default()),
            },
        }
    }
}

fn drive<H: LayoutHost>(
    model: &mut SharedState,
    host: H,
    mut transition: Transition,
) -> (Task<Message>, Option<Event>) {
    let mut focus_name = false;
    loop {
        focus_name |= transition.focus_name;
        if let Some(effect) = transition.effect {
            transition = model.complete(host.execute(effect));
            continue;
        }
        let task = if focus_name {
            operation::focus(smudgy_ui_shared::layouts_view::name_input_id())
        } else {
            Task::none()
        };
        return (task, transition.event);
    }
}

pub fn update(state: &mut State, message: Message) -> (Task<Message>, Option<Event>) {
    let transition = state.model.update(message);
    drive(
        &mut state.model,
        NativeStore {
            dir: state.dir.as_deref(),
        },
        transition,
    )
}
pub fn view(state: &State) -> Element<'_, Message> {
    smudgy_ui_shared::layouts_view::view(&state.model, &NativeLabels)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::dto;

    fn minimal_workspace() -> dto::Workspace {
        serde_json::from_value(serde_json::json!({
            "version": dto::SCHEMA_VERSION,
            "sessions": [{"id": 1, "server": "Arctic", "profile": "imm", "connect": true}],
            "windows": [{
                "id": 1,
                "geometry": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0, "scale": 1.0},
                "clusters": [{"weight": 1.0, "root": {"group": {
                    "tabs": [{"slot": 1, "id": "main"}], "selected": 0
                }}}]
            }]
        }))
        .expect("minimal workspace parses")
    }

    fn state_with_layouts(names: &[&str]) -> (tempfile::TempDir, State) {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = root.path().join("layouts");
        let workspace = minimal_workspace();
        for name in names {
            layouts::save_in(&dir, name, &workspace).expect("seed layout");
        }
        let state = State::with_dir("Arctic".to_string(), Some(dir));
        (root, state)
    }

    fn event(state: &mut State, message: Message) -> Option<Event> {
        update(state, message).1
    }

    #[test]
    fn clicking_a_layout_applies_it() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        assert_eq!(
            event(&mut state, Message::ApplyPressed("combat".to_string())),
            Some(Event::Apply {
                name: "combat".to_string()
            })
        );
    }

    #[test]
    fn save_as_validates_the_name_before_anything_happens() {
        let (_root, mut state) = state_with_layouts(&[]);
        assert!(event(&mut state, Message::SaveAsPressed).is_none());
        assert!(event(&mut state, Message::NameChanged("a/b".to_string())).is_none());
        assert!(event(&mut state, Message::SaveAsConfirmed).is_none());
        let Stage::SaveAs { error, .. } = &state.model.stage else {
            panic!("still in the save-as stage");
        };
        assert!(error.is_some(), "the invalid name is called out");
    }

    #[test]
    fn save_as_over_a_case_variant_requires_the_overwrite_confirmation() {
        let (_root, mut state) = state_with_layouts(&["Combat"]);
        event(&mut state, Message::SaveAsPressed);
        event(&mut state, Message::NameChanged("COMBAT".to_string()));
        assert!(
            event(&mut state, Message::SaveAsConfirmed).is_none(),
            "an existing (case-folded) name is not saved outright"
        );
        assert!(matches!(
            &state.model.stage,
            Stage::ConfirmOverwrite {
                name,
                action: OverwriteAction::Save
            } if name == "COMBAT"
        ));
        assert_eq!(
            event(&mut state, Message::OverwriteConfirmed),
            Some(Event::Save {
                name: "COMBAT".to_string(),
                replace: true,
            })
        );
        assert_eq!(state.model.stage, Stage::Browse);
    }

    #[test]
    fn fresh_names_save_without_a_confirmation() {
        let (_root, mut state) = state_with_layouts(&[]);
        event(&mut state, Message::SaveAsPressed);
        event(&mut state, Message::NameChanged("peace".to_string()));
        assert_eq!(
            event(&mut state, Message::SaveAsConfirmed),
            Some(Event::Save {
                name: "peace".to_string(),
                replace: false,
            })
        );
    }

    #[test]
    fn delete_requires_its_confirmation_and_refreshes_the_list() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        assert!(event(&mut state, Message::DeletePressed("combat".to_string())).is_none());
        assert_eq!(state.model.layouts, vec!["combat".to_string()]);
        assert!(event(&mut state, Message::DeleteConfirmed).is_none());
        assert!(state.model.layouts.is_empty(), "the listing refreshed");
    }

    #[test]
    fn cancelling_a_confirmation_changes_nothing() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        event(&mut state, Message::DeletePressed("combat".to_string()));
        assert!(event(&mut state, Message::Back).is_none());
        assert_eq!(state.model.stage, Stage::Browse);
        assert_eq!(state.model.layouts, vec!["combat".to_string()]);
    }

    #[test]
    fn rename_validates_and_renames_in_the_store() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        event(&mut state, Message::RenamePressed("combat".to_string()));
        event(&mut state, Message::NameChanged("nul".to_string()));
        assert!(event(&mut state, Message::RenameConfirmed).is_none());
        assert!(
            matches!(&state.model.stage, Stage::Rename { error: Some(_), .. }),
            "a reserved device name is rejected in place"
        );
        event(&mut state, Message::NameChanged("peace".to_string()));
        assert!(event(&mut state, Message::RenameConfirmed).is_none());
        assert_eq!(state.model.stage, Stage::Browse);
        assert_eq!(state.model.layouts, vec!["peace".to_string()]);
    }

    #[test]
    fn rename_onto_a_different_layout_requires_the_overwrite_confirmation() {
        let (_root, mut state) = state_with_layouts(&["combat", "peace"]);
        event(&mut state, Message::RenamePressed("combat".to_string()));
        event(&mut state, Message::NameChanged("PEACE".to_string()));
        assert!(event(&mut state, Message::RenameConfirmed).is_none());
        assert!(matches!(
            &state.model.stage,
            Stage::ConfirmOverwrite {
                name,
                action: OverwriteAction::Rename { from }
            } if name == "PEACE" && from == "combat"
        ));
        // Nothing moved while the confirmation is open.
        assert_eq!(
            state.model.layouts,
            vec!["combat".to_string(), "peace".to_string()]
        );
        assert!(event(&mut state, Message::OverwriteConfirmed).is_none());
        assert_eq!(state.model.stage, Stage::Browse);
        // The rename replaced peace by renaming over its file: the
        // displaced stem stands and combat is gone.
        assert_eq!(state.model.layouts, vec!["peace".to_string()]);
    }

    #[test]
    fn rename_replace_cancels_back_to_browsing_untouched() {
        let (_root, mut state) = state_with_layouts(&["combat", "peace"]);
        event(&mut state, Message::RenamePressed("combat".to_string()));
        event(&mut state, Message::NameChanged("peace".to_string()));
        event(&mut state, Message::RenameConfirmed);
        assert!(event(&mut state, Message::Back).is_none());
        assert_eq!(state.model.stage, Stage::Browse);
        assert_eq!(
            state.model.layouts,
            vec!["combat".to_string(), "peace".to_string()]
        );
    }

    #[test]
    fn case_only_renames_stay_free_of_confirmation() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        event(&mut state, Message::RenamePressed("combat".to_string()));
        event(&mut state, Message::NameChanged("COMBAT".to_string()));
        assert!(event(&mut state, Message::RenameConfirmed).is_none());
        assert_eq!(state.model.stage, Stage::Browse);
        assert_eq!(state.model.layouts, vec!["COMBAT".to_string()]);
    }

    #[test]
    fn reset_requires_its_confirmation() {
        let (_root, mut state) = state_with_layouts(&[]);
        assert!(event(&mut state, Message::ResetPressed).is_none());
        assert_eq!(
            event(&mut state, Message::ResetConfirmed),
            Some(Event::Reset)
        );
    }

    #[test]
    fn keep_or_close_defaults_to_keep_and_partitions_by_explicit_toggle() {
        let (_root, mut state) = state_with_layouts(&["combat"]);
        state.prompt_keep_or_close(
            TemplateSource::Named("combat".to_string()),
            vec![
                OmittedRow {
                    session: SessionId::from(1),
                    label: "imm on Arctic".to_string(),
                    close: false,
                },
                OmittedRow {
                    session: SessionId::from(2),
                    label: "alt on Arctic".to_string(),
                    close: false,
                },
            ],
        );
        // Confirming untouched answers keeps everything: closing is never
        // the silent default. A last-session restore's questions ride the
        // same stage, its source echoed back for the re-entry apply.
        let mut untouched = State::with_dir("Arctic".to_string(), None);
        untouched.prompt_keep_or_close(
            TemplateSource::LastSession,
            vec![OmittedRow {
                session: SessionId::from(7),
                label: "imm on Arctic".to_string(),
                close: false,
            }],
        );
        assert_eq!(
            event(&mut untouched, Message::KeepOrCloseConfirmed),
            Some(Event::ApplyWithAnswers {
                source: TemplateSource::LastSession,
                close: vec![],
                keep: vec![SessionId::from(7)],
            })
        );

        // An explicit toggle closes exactly that session.
        event(&mut state, Message::ToggleClose(SessionId::from(2), true));
        assert_eq!(
            event(&mut state, Message::KeepOrCloseConfirmed),
            Some(Event::ApplyWithAnswers {
                source: TemplateSource::Named("combat".to_string()),
                close: vec![SessionId::from(2)],
                keep: vec![SessionId::from(1)],
            })
        );
    }

    #[test]
    fn save_outcome_annotates_partial_captures() {
        let (_root, mut state) = state_with_layouts(&[]);
        state.record_save_outcome(SaveOutcome::Saved {
            name: "combat".to_string(),
            omitted: 2,
        });
        assert_eq!(
            state.model.outcome,
            Some(SaveOutcome::Saved {
                name: "combat".to_string(),
                omitted: 2
            })
        );
        // The daemon saved after this modal listed: the listing refreshed.
        assert!(state.model.layouts.is_empty(), "seeded empty; still empty");
    }
}
