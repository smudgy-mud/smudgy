use serde::{Deserialize, Serialize};

pub mod aliases;
pub mod auth;
pub mod hotkeys;
pub mod local_packages;
pub mod map_scopes;
pub mod modules;
pub mod naming;
pub mod packages;
mod persistence;
pub mod profile;
pub mod script_typings;
pub mod server;
pub mod settings;
pub mod shared_packages;
pub mod triggers;

/// Represents the programming language of a script.
#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ScriptLang {
    #[default]
    Plaintext,
    JS,
    TS,
}
