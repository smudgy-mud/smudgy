//! Bundled loading of session configuration from disk, shaped as
//! ready-to-send [`RuntimeAction`]s.
//!
//! These load fresh on every call by design: the UI invokes them again on
//! session reload and reconnect so that edits to the on-disk configuration
//! take effect without restarting the application.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::models::{
    aliases::{AliasDefinition, load_aliases},
    automation_transaction,
    hotkeys::{HotkeyDefinition, load_hotkeys},
    packages::{PackageTree, load_packages},
    profile::{Profile, load_profile},
    server::{Server, load_server},
    triggers::{TriggerDefinition, load_triggers},
};

use super::runtime::{IsolateId, Origin, RuntimeAction};

/// The persisted user-authored automations and legacy folder enablement for one server.
///
/// Each runtime retains its previous snapshot and reconciles only definitions whose effective
/// runtime form changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct UserAutomations {
    aliases: HashMap<String, AliasDefinition>,
    hotkeys: HashMap<String, HotkeyDefinition>,
    triggers: HashMap<String, TriggerDefinition>,
    packages: PackageTree,
    profile_name: String,
    /// `packages.json` could not be read, so folder effective enablement is unknown for this
    /// snapshot. Every folder-placed definition is then treated as disabled (fail closed for
    /// folder scoping only); definitions without a folder resolve from their own flag.
    folders_unavailable: bool,
}

/// Load one persisted user-automation snapshot for `profile_name`.
///
/// All four compatibility files are read under the automation transaction lock so a reader
/// never combines categories from two different commits. Failure is isolated per category: a
/// category whose file cannot be read or parsed loads as empty while the other categories load
/// normally, and each failure is returned as one user-facing line naming the category and file.
///
/// A failed `packages.json` is the one cross-category case: folder enablement cannot be computed,
/// so the snapshot marks folders unavailable and every folder-placed alias, trigger, and hotkey is
/// treated as disabled until the file loads again. Definitions outside any folder are unaffected.
/// This fails closed for folder scoping only, never for the whole session.
#[must_use]
pub(crate) fn load_user_automations(
    server_name: &str,
    profile_name: &str,
) -> (UserAutomations, Vec<String>) {
    let _guard = automation_transaction::guard(server_name);
    let mut failures = Vec::new();
    let mut report = |category: &str, file: &str, error: &anyhow::Error| {
        let message = format!(
            "[automations] {category} for server {server_name} were not loaded because {file} could not be read: {error:#}"
        );
        log::warn!("{message}");
        failures.push(message);
    };
    let aliases = load_aliases(server_name).unwrap_or_else(|error| {
        report("Aliases", "aliases/aliases.json", &error);
        HashMap::new()
    });
    let hotkeys = load_hotkeys(server_name).unwrap_or_else(|error| {
        report("Hotkeys", "hotkeys/hotkeys.json", &error);
        HashMap::new()
    });
    let triggers = load_triggers(server_name).unwrap_or_else(|error| {
        report("Triggers", "triggers/triggers.json", &error);
        HashMap::new()
    });
    let (packages, folders_unavailable) = match load_packages(server_name) {
        Ok(packages) => (packages, false),
        Err(error) => {
            let message = format!(
                "[automations] Folder enablement for server {server_name} is unknown because packages.json could not be read: {error:#}. Every alias, trigger, and hotkey inside a folder stays off until the file loads."
            );
            log::warn!("{message}");
            failures.push(message);
            (PackageTree::new(), true)
        }
    };
    (
        UserAutomations {
            aliases,
            hotkeys,
            triggers,
            packages,
            profile_name: profile_name.to_string(),
            folders_unavailable,
        },
        failures,
    )
}

fn enabled_for_profile(enabled: bool, package: Option<&str>, snapshot: &UserAutomations) -> bool {
    enabled
        && package.is_none_or(|path| {
            !snapshot.folders_unavailable
                && crate::models::packages::is_package_effectively_enabled_for(
                    path,
                    &snapshot.packages,
                    &snapshot.profile_name,
                )
        })
}

fn runtime_alias(definition: &AliasDefinition, snapshot: &UserAutomations) -> AliasDefinition {
    let mut runtime = definition.clone();
    runtime.enabled =
        enabled_for_profile(definition.enabled, definition.package.as_deref(), snapshot);
    // Folder placement has no runtime meaning once effective enablement has been resolved.
    runtime.package = None;
    runtime
}

fn runtime_hotkey(definition: &HotkeyDefinition, snapshot: &UserAutomations) -> HotkeyDefinition {
    let mut runtime = definition.clone();
    runtime.enabled =
        enabled_for_profile(definition.enabled, definition.package.as_deref(), snapshot);
    runtime.package = None;
    runtime
}

fn runtime_trigger(
    definition: &TriggerDefinition,
    snapshot: &UserAutomations,
) -> TriggerDefinition {
    let mut runtime = definition.clone();
    runtime.enabled =
        enabled_for_profile(definition.enabled, definition.package.as_deref(), snapshot);
    runtime.package = None;
    runtime
}

fn reconcile_alias_actions(
    previous: &UserAutomations,
    desired: &UserAutomations,
) -> Vec<RuntimeAction> {
    let mut actions = Vec::new();

    for (name, definition) in &previous.aliases {
        let old = runtime_alias(definition, previous);
        let new = desired
            .aliases
            .get(name)
            .map(|definition| runtime_alias(definition, desired));
        if old.enabled
            && new
                .as_ref()
                .is_none_or(|definition| !definition.enabled || definition != &old)
        {
            actions.push(RuntimeAction::RemoveAlias(
                IsolateId::Main,
                Origin::User,
                Arc::new(name.clone()),
            ));
        }
    }
    for (name, definition) in &desired.aliases {
        let new = runtime_alias(definition, desired);
        if !new.enabled {
            continue;
        }
        let unchanged = previous
            .aliases
            .get(name)
            .map(|definition| runtime_alias(definition, previous))
            .is_some_and(|old| old == new);
        if !unchanged {
            actions.push(RuntimeAction::AddAlias {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name.clone()),
                alias: Box::new(new),
                fire_limit: None,
            });
        }
    }

    actions
}

fn reconcile_trigger_actions(
    previous: &UserAutomations,
    desired: &UserAutomations,
) -> Vec<RuntimeAction> {
    let mut actions = Vec::new();

    for (name, definition) in &previous.triggers {
        let old = runtime_trigger(definition, previous);
        let new = desired
            .triggers
            .get(name)
            .map(|definition| runtime_trigger(definition, desired));
        if old.enabled
            && new
                .as_ref()
                .is_none_or(|definition| !definition.enabled || definition != &old)
        {
            actions.push(RuntimeAction::RemoveTrigger(
                IsolateId::Main,
                Origin::User,
                Arc::new(name.clone()),
            ));
        }
    }
    for (name, definition) in &desired.triggers {
        let new = runtime_trigger(definition, desired);
        if !new.enabled {
            continue;
        }
        let unchanged = previous
            .triggers
            .get(name)
            .map(|definition| runtime_trigger(definition, previous))
            .is_some_and(|old| old == new);
        if !unchanged {
            actions.push(RuntimeAction::AddTrigger {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name.clone()),
                trigger: Box::new(new),
                fire_limit: None,
                line_limit: None,
            });
        }
    }

    actions
}

fn reconcile_hotkey_actions(
    previous: &UserAutomations,
    desired: &UserAutomations,
) -> Vec<RuntimeAction> {
    let mut actions = Vec::new();

    for (name, definition) in &previous.hotkeys {
        let old = runtime_hotkey(definition, previous);
        let new = desired
            .hotkeys
            .get(name)
            .map(|definition| runtime_hotkey(definition, desired));
        if old.enabled
            && new
                .as_ref()
                .is_none_or(|definition| !definition.enabled || definition != &old)
        {
            actions.push(RuntimeAction::RemoveHotkey(
                IsolateId::Main,
                Origin::User,
                Arc::new(name.clone()),
            ));
        }
    }
    for (name, definition) in &desired.hotkeys {
        let new = runtime_hotkey(definition, desired);
        if !new.enabled {
            continue;
        }
        let unchanged = previous
            .hotkeys
            .get(name)
            .map(|definition| runtime_hotkey(definition, previous))
            .is_some_and(|old| old == new);
        if !unchanged {
            actions.push(RuntimeAction::AddHotkey {
                isolate: IsolateId::Main,
                origin: Origin::User,
                name: Arc::new(name.clone()),
                hotkey: Box::new(new),
                function_id: None,
            });
        }
    }

    actions
}

/// Diff two persisted snapshots into the existing fine-grained runtime action vocabulary.
///
/// Only effectively enabled definitions are installed. Disabling therefore removes matcher and
/// hotkey registrations completely; enabling adds them back without replacing the script engine.
/// A changed active alias/trigger is removed before replacement so a failed regex or script
/// compile cannot leave the obsolete definition active.
pub(crate) fn reconcile_automation_actions(
    previous: &UserAutomations,
    desired: &UserAutomations,
) -> Vec<RuntimeAction> {
    let mut actions = reconcile_alias_actions(previous, desired);
    actions.extend(reconcile_trigger_actions(previous, desired));
    actions.extend(reconcile_hotkey_actions(previous, desired));
    actions
}

/// Build the action that asks a session to load and reconcile the server's current persisted
/// aliases, triggers, hotkeys, and folder enablement.
#[must_use]
pub fn load_automation_actions(_server_name: &str) -> Vec<RuntimeAction> {
    vec![RuntimeAction::SyncUserAutomations]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScriptLang;
    use crate::models::packages::PackageNode;

    fn alias(package: Option<&str>, enabled: bool) -> AliasDefinition {
        AliasDefinition {
            pattern: "^go$".to_string(),
            script: Some("north".to_string()),
            package: package.map(str::to_string),
            enabled,
            priority: 0,
            fallthrough: true,
            allow_self_match: false,
            language: ScriptLang::Plaintext,
            matcher: None,
            state: Vec::new(),
        }
    }

    fn trigger(package: Option<&str>, enabled: bool) -> TriggerDefinition {
        TriggerDefinition {
            patterns: Some(vec!["^ready$".to_string()]),
            script: Some("look".to_string()),
            package: package.map(str::to_string),
            enabled,
            ..TriggerDefinition::default()
        }
    }

    fn hotkey(package: Option<&str>, enabled: bool) -> HotkeyDefinition {
        HotkeyDefinition {
            key: "F1".to_string(),
            modifiers: Vec::new(),
            script: Some("score".to_string()),
            package: package.map(str::to_string),
            language: ScriptLang::Plaintext,
            enabled,
            state: Vec::new(),
        }
    }

    fn folder(enabled: bool) -> PackageTree {
        HashMap::from([(
            "combat".to_string(),
            PackageNode {
                enabled,
                activation: None,
                children: HashMap::new(),
            },
        )])
    }

    #[test]
    fn unchanged_snapshot_emits_no_actions() {
        let snapshot = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(None, true))]),
            hotkeys: HashMap::from([("score".to_string(), hotkey(None, true))]),
            triggers: HashMap::from([("ready".to_string(), trigger(None, true))]),
            packages: PackageTree::new(),
            profile_name: "main".to_string(),
            folders_unavailable: false,
        };

        assert!(reconcile_automation_actions(&snapshot, &snapshot).is_empty());
    }

    #[test]
    fn changed_active_alias_is_removed_before_replacement() {
        let previous = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(None, true))]),
            ..UserAutomations::default()
        };
        let mut replacement = alias(None, true);
        replacement.pattern = "^run$".to_string();
        let desired = UserAutomations {
            aliases: HashMap::from([("go".to_string(), replacement)]),
            ..UserAutomations::default()
        };

        let actions = reconcile_automation_actions(&previous, &desired);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], RuntimeAction::RemoveAlias(..)));
        assert!(matches!(actions[1], RuntimeAction::AddAlias { .. }));
    }

    #[test]
    fn disabling_folder_removes_each_effectively_disabled_kind() {
        let previous = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(Some("combat"), true))]),
            hotkeys: HashMap::from([("score".to_string(), hotkey(Some("combat"), true))]),
            triggers: HashMap::from([("ready".to_string(), trigger(Some("combat"), true))]),
            packages: folder(true),
            profile_name: "main".to_string(),
            folders_unavailable: false,
        };
        let desired = UserAutomations {
            packages: folder(false),
            ..previous.clone()
        };

        let actions = reconcile_automation_actions(&previous, &desired);
        assert_eq!(actions.len(), 3);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::RemoveAlias(..)))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::RemoveTrigger(..)))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::RemoveHotkey(..)))
        );
    }

    #[test]
    fn the_same_folder_scope_produces_different_profile_actions() {
        let packages = HashMap::from([(
            "combat".to_string(),
            PackageNode {
                enabled: false,
                activation: Some(
                    crate::models::profile_activation::ProfileActivation::Selected {
                        profiles: ["main".to_string()].into_iter().collect(),
                    },
                ),
                children: HashMap::new(),
            },
        )]);
        let inactive = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(Some("combat"), true))]),
            packages: packages.clone(),
            profile_name: "alt".to_string(),
            ..UserAutomations::default()
        };
        let active = UserAutomations {
            profile_name: "main".to_string(),
            ..inactive.clone()
        };

        let actions = reconcile_automation_actions(&inactive, &active);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], RuntimeAction::AddAlias { .. }));
    }

    #[test]
    fn enabling_leaf_adds_each_kind() {
        let previous = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(None, false))]),
            hotkeys: HashMap::from([("score".to_string(), hotkey(None, false))]),
            triggers: HashMap::from([("ready".to_string(), trigger(None, false))]),
            packages: PackageTree::new(),
            profile_name: "main".to_string(),
            folders_unavailable: false,
        };
        let desired = UserAutomations {
            aliases: HashMap::from([("go".to_string(), alias(None, true))]),
            hotkeys: HashMap::from([("score".to_string(), hotkey(None, true))]),
            triggers: HashMap::from([("ready".to_string(), trigger(None, true))]),
            packages: PackageTree::new(),
            profile_name: "main".to_string(),
            folders_unavailable: false,
        };

        let actions = reconcile_automation_actions(&previous, &desired);
        assert_eq!(actions.len(), 3);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::AddAlias { .. }))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::AddTrigger { .. }))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::AddHotkey { .. }))
        );
    }

    #[test]
    fn folder_placed_definitions_fail_closed_when_folders_are_unavailable() {
        let snapshot = UserAutomations {
            aliases: HashMap::from([
                ("go".to_string(), alias(Some("combat"), true)),
                ("free".to_string(), alias(None, true)),
            ]),
            packages: folder(true),
            profile_name: "main".to_string(),
            folders_unavailable: true,
            ..UserAutomations::default()
        };

        let actions = reconcile_automation_actions(&UserAutomations::default(), &snapshot);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            RuntimeAction::AddAlias { name, .. } if name.as_str() == "free"
        ));
    }

    /// A process-wide temporary Smudgy home shared by every disk-backed test in this crate
    /// (the override is set once per process); each test uses its own server directory.
    fn test_server(label: &str) -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "smudgy-session-config-test-home-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).unwrap();
        crate::set_smudgy_home(home);
        let name = format!(
            "cfg-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let server_dir = crate::get_smudgy_home().unwrap().join(&name);
        for kind in ["aliases", "hotkeys", "triggers"] {
            std::fs::create_dir_all(server_dir.join(kind)).unwrap();
        }
        name
    }

    #[test]
    fn malformed_hotkeys_file_leaves_aliases_and_triggers_loaded() {
        let server = test_server("hotkeys");
        crate::models::aliases::save_aliases(
            &server,
            &HashMap::from([("go".to_string(), alias(None, true))]),
        )
        .unwrap();
        crate::models::triggers::save_triggers(
            &server,
            &HashMap::from([("ready".to_string(), trigger(None, true))]),
        )
        .unwrap();
        let server_dir = crate::get_smudgy_home().unwrap().join(&server);
        std::fs::write(
            server_dir.join("hotkeys").join("hotkeys.json"),
            "{ not json",
        )
        .unwrap();

        let (snapshot, failures) = load_user_automations(&server, "main");
        assert_eq!(snapshot.aliases.len(), 1);
        assert_eq!(snapshot.triggers.len(), 1);
        assert!(snapshot.hotkeys.is_empty());
        assert!(!snapshot.folders_unavailable);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("Hotkeys"));
        assert!(failures[0].contains("hotkeys/hotkeys.json"));

        let actions = reconcile_automation_actions(&UserAutomations::default(), &snapshot);
        assert_eq!(actions.len(), 2);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::AddAlias { .. }))
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, RuntimeAction::AddTrigger { .. }))
        );
    }

    #[test]
    fn malformed_packages_file_is_reported_and_disables_only_folder_scoping() {
        let server = test_server("packages");
        crate::models::aliases::save_aliases(
            &server,
            &HashMap::from([
                ("go".to_string(), alias(Some("combat"), true)),
                ("free".to_string(), alias(None, true)),
            ]),
        )
        .unwrap();
        let server_dir = crate::get_smudgy_home().unwrap().join(&server);
        std::fs::write(server_dir.join("packages.json"), "[1, 2").unwrap();

        let (snapshot, failures) = load_user_automations(&server, "main");
        assert_eq!(snapshot.aliases.len(), 2);
        assert!(snapshot.folders_unavailable);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("packages.json"));

        let actions = reconcile_automation_actions(&UserAutomations::default(), &snapshot);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            RuntimeAction::AddAlias { name, .. } if name.as_str() == "free"
        ));
    }
}

/// Build the [`RuntimeAction::Connect`] for a server/profile from their saved
/// configurations.
///
/// # Errors
///
/// Returns an error if the profile or server configuration fails to load
/// (missing or malformed config file).
fn connect_action_from_snapshot(
    server_name: &str,
    profile_name: &str,
    profile: Profile,
    server: Server,
) -> RuntimeAction {
    // Substitute the $PASSWORD token (if present) with the password stored in the
    // OS keyring for this profile, and collect the secret(s) to redact from the
    // client's view and the session log when the auto-login text is echoed. The
    // token — not the password — is what lives in profile.json.
    let (send_on_connect, send_on_connect_redactions) = if profile.config.send_on_connect.is_empty()
    {
        (None, Vec::new())
    } else {
        let (text, redactions) = crate::models::profile::substitute_password_with_redactions(
            server_name,
            profile_name,
            &profile.config.send_on_connect,
        );
        (Some(Arc::new(text)), redactions)
    };

    let mccp4_compression = server.config.accepts_mccp4_compression();
    RuntimeAction::Connect {
        host: server.config.host.into(),
        port: server.config.port,
        send_on_connect,
        send_on_connect_redactions: Arc::new(send_on_connect_redactions),
        encoding: server.config.encoding.map(Arc::new),
        compression: crate::session::connection::InboundCompression::new(
            server.config.compression,
            mccp4_compression,
        ),
        tls: crate::session::connection::TlsMode::from_settings(
            server.config.tls,
            server.config.tls_verify,
        ),
    }
}

/// Loads the profile, server, and password snapshot for a connection.
///
/// # Errors
///
/// Returns an error if the profile or server configuration fails to load
/// (missing or malformed config file).
pub fn load_connect_action(server_name: &str, profile_name: &str) -> Result<RuntimeAction> {
    let profile =
        load_profile(server_name, profile_name).context("Failed to load profile config")?;
    let server = load_server(server_name).context("Failed to load server config")?;
    Ok(connect_action_from_snapshot(
        server_name,
        profile_name,
        profile,
        server,
    ))
}
