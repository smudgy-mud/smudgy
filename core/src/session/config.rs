//! Bundled loading of session configuration from disk, shaped as
//! ready-to-send [`RuntimeAction`]s.
//!
//! These load fresh on every call by design: the UI invokes them again on
//! session reload and reconnect so that edits to the on-disk configuration
//! take effect without restarting the application.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::models::{
    aliases::load_aliases, hotkeys::load_hotkeys, profile::load_profile, server::load_server,
    triggers::load_triggers,
};

use super::runtime::{IsolateId, Origin, RuntimeAction};

/// Load every automation (hotkeys, triggers, aliases) defined for a server as
/// ready-to-send runtime actions, in registration order. A category that
/// fails to load is logged and skipped so one bad file doesn't take down the
/// rest.
#[must_use]
pub fn load_automation_actions(server_name: &str) -> Vec<RuntimeAction> {
    let mut actions = Vec::new();

    match load_hotkeys(server_name) {
        Ok(hotkeys) => actions.extend(hotkeys.into_iter().map(|(name, hotkey)| {
            RuntimeAction::AddHotkey {
                // Disk-defined hotkeys are the user's own, in the trusted main isolate.
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name),
                hotkey,
                // Disk hotkeys are script-string bodies, not registered function handles.
                function_id: None,
            }
        })),
        Err(e) => log::warn!("Failed to load hotkeys for server {server_name}: {e:?}"),
    }

    match load_triggers(server_name) {
        Ok(triggers) => actions.extend(triggers.into_iter().map(|(name, trigger)| {
            RuntimeAction::AddTrigger {
                // Disk-defined automations are the user's own, in the trusted main isolate.
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name),
                trigger,
                // Disk automations have no script-supplied self-limit.
                fire_limit: None,
                line_limit: None,
            }
        })),
        Err(e) => log::warn!("Failed to load triggers for server {server_name}: {e:?}"),
    }

    match load_aliases(server_name) {
        Ok(aliases) => actions.extend(aliases.into_iter().map(|(name, alias)| {
            RuntimeAction::AddAlias {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name),
                alias,
                fire_limit: None,
            }
        })),
        Err(e) => log::warn!("Failed to load aliases for server {server_name}: {e:?}"),
    }

    actions
}

/// Build the [`RuntimeAction::Connect`] for a server/profile from their saved
/// configurations.
///
/// # Errors
///
/// Returns an error if the profile or server configuration fails to load
/// (missing or malformed config file).
pub fn load_connect_action(server_name: &str, profile_name: &str) -> Result<RuntimeAction> {
    let profile =
        load_profile(server_name, profile_name).context("Failed to load profile config")?;
    let server = load_server(server_name).context("Failed to load server config")?;

    // Substitute the $PASSWORD token (if present) with the password stored in the
    // OS keyring for this profile, and collect the secret(s) to redact from the
    // client's view and the session log when the auto-login text is echoed. The
    // token — not the password — is what lives in profile.json.
    let (send_on_connect, send_on_connect_redactions) =
        if profile.config.send_on_connect.is_empty() {
            (None, Vec::new())
        } else {
            let (text, redactions) = crate::models::profile::substitute_password_with_redactions(
                server_name,
                profile_name,
                &profile.config.send_on_connect,
            );
            (Some(Arc::new(text)), redactions)
        };

    Ok(RuntimeAction::Connect {
        host: server.config.host.into(),
        port: server.config.port,
        send_on_connect,
        send_on_connect_redactions: Arc::new(send_on_connect_redactions),
    })
}
