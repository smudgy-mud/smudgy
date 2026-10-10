//! Target-neutral iced presentation shared by Smudgy hosts.

#![allow(clippy::pedantic)]

pub mod assets;
pub mod bounds_probe;
pub mod connect_modal;
pub mod connect_model;
pub mod controls;
pub mod crt_cat;
pub mod drag_overlay;
pub mod hotkey_matching_input;
pub mod i18n;
pub mod inline_effects;
pub mod inline_fonts;
pub mod inline_object;
pub mod keymap;
pub mod layouts_modal;
pub mod layouts_view;
pub mod main_toolbar;
pub mod modal_layer;
pub mod palettes;
pub mod pane_drag;
pub mod pane_groups;
pub mod pane_workspace;
pub mod prefs;
pub mod session_input;
pub mod settings_appearance;
pub mod settings_input;
pub mod settings_theme;
pub(crate) mod span_cuts;
pub mod split_terminal_pane;
pub mod tab_host;
pub mod tab_press;
pub mod tab_strip;
pub mod terminal_buffer;
pub mod terminal_span;
pub mod update;
pub mod workspace_snapshot;
pub mod wrap_row;

pub mod text_effect;

#[cfg(any(test, feature = "profiling"))]
pub mod profiling;
