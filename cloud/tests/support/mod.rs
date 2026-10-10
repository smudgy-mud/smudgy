//! Contract-shaped in-memory mock of the smudgy cloud map API, for integration
//! tests. Fidelity reference: `smudgy-cloudflare/src` and its format-3
//! contract. Authorization and mutation regressions also run on that service.
//!
//! Spawn with [`MockServer::spawn`]; poke state directly through
//! [`MockHandle::state`] or the ergonomic helpers on [`MockHandle`].
#![allow(
    dead_code,
    clippy::too_many_lines,
    clippy::unused_async,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::struct_field_names,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::similar_names,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::doc_markdown,
    clippy::single_match_else,
    clippy::option_option,
    clippy::trivially_copy_pass_by_ref,
    clippy::result_large_err
)]

pub mod areas;
pub mod clan_maps;
pub mod clan_resources;
pub mod clan_secret_grants;
pub mod clan_secrets;
pub mod clans;
pub mod clone;
pub mod connections;
pub mod filing;
pub mod http;
pub mod identity;
pub mod mock_server;
pub mod mutations;
pub mod projection;
pub mod reviewed_moves;
pub mod secret_grants;
pub mod secrets;
pub mod shares;
pub mod social;
pub mod source_refs;
pub mod state;
pub mod transfer_policy;
pub mod transfers;

#[allow(unused_imports)]
pub use mock_server::{GrantFlags, GrantScope, MockHandle, MockServer, SecretPlace, TestUser};
#[allow(unused_imports)]
pub use state::MockState;
