use serde::{Deserialize, Serialize};

pub mod aliases;
pub mod auth;
pub mod automation_transaction;
pub mod hotkeys;
pub mod input_history;
pub mod local_packages;
pub mod map_scopes;
pub mod matchers;
pub mod modules;
pub mod naming;
pub mod observed;
pub mod package_updates;
pub mod packages;
pub mod persistence;
pub mod profile;
pub mod profile_activation;
pub mod script_typings;
pub mod server;
pub mod settings;
pub mod shared_packages;
pub mod state_exposure;
pub(crate) mod state_lock;
pub mod triggers;

/// Represents the programming language of a script.
#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ScriptLang {
    #[default]
    Plaintext,
    JS,
    TS,
}
