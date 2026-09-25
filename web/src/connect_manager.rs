//! Browser connection catalog and server/profile manager state.
//!
//! The UI emits immutable catalog mutations. A host commits them with its own
//! storage transaction, then reports the result. No storage backend or virtual
//! dispatch is involved in the session hot path.

use std::collections::BTreeSet;

use iced::widget::{button, column, container, row, text, text_editor, text_input};
use iced::{Element, Fill, Length, Pixels};
use serde::{Deserialize, Serialize};
use smudgy_session_model::automation::{AutomationDefinition, AutomationPlan, PlaintextRule};
use smudgy_session_model::connect::DEFAULT_PROFILE_NAME;
use smudgy_session_model::profile_activation::ProfileActivation;
use smudgy_theme::{Theme, builtins};

use smudgy_ui_shared::connect_modal::{self, RailItem, ServerDetails};
use smudgy_ui_shared::connect_model::{
    ConnectViewModel, Form as ConnectForm, Intent as ConnectIntent, Panel as ConnectPanel,
    ProfileInventory, ProfileSummary, Profiles,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub generation: u64,
    pub servers: Vec<SavedServer>,
}

/// A resolved connection uses the current catalog, never a stale restore
/// snapshot. The same resolution serves manual Connect and window restore.
#[derive(Debug, Clone)]
pub struct ConnectionRequest {
    pub server: String,
    pub profile: String,
    pub endpoint: String,
    pub encoding: String,
    pub send_on_connect: String,
    pub automation: AutomationPlan,
    pub packages: Vec<String>,
}

impl Catalog {
    /// Convert v2 profile-local package lists into one explicit activation per
    /// server package. `Selected { Default }` stays selected even when Default
    /// is currently the only profile; it must not imply future profiles.
    pub fn normalize_packages(&mut self) {
        for server in &mut self.servers {
            for profile in &mut server.profiles {
                for name in profile.packages.drain(..) {
                    let scope = server
                        .package_activations
                        .iter_mut()
                        .find(|scope| scope.name == name);
                    if let Some(scope) = scope {
                        if let ProfileActivation::Selected { profiles } = &mut scope.activation {
                            profiles.insert(profile.name.clone());
                        } else if scope.activation == ProfileActivation::None {
                            scope.activation = ProfileActivation::Selected {
                                profiles: BTreeSet::from([profile.name.clone()]),
                            };
                        }
                    } else {
                        server.package_activations.push(PackageActivation {
                            name,
                            activation: ProfileActivation::Selected {
                                profiles: BTreeSet::from([profile.name.clone()]),
                            },
                        });
                    }
                }
            }
            server
                .package_activations
                .sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    #[must_use]
    pub fn resolve_connection(
        &self,
        server_name: &str,
        profile_name: &str,
    ) -> Option<ConnectionRequest> {
        let server = self
            .servers
            .iter()
            .find(|server| server.name == server_name)?;
        let selected_profile = server
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)?;
        let packages = server
            .package_activations
            .iter()
            .filter(|scope| scope.activation.is_enabled_for(profile_name))
            .map(|scope| scope.name.clone())
            .collect();
        Some(ConnectionRequest {
            server: server.name.clone(),
            profile: profile_name.to_owned(),
            endpoint: server.endpoint.clone(),
            encoding: server.encoding.clone(),
            send_on_connect: selected_profile.send_on_connect.clone(),
            automation: AutomationPlan {
                shared: server.shared_automation.clone(),
                profile: Some(selected_profile.automation.clone()),
            },
            packages,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    pub name: String,
    /// Full WSS URL, including path and query. Do not reduce this to host/port.
    pub endpoint: String,
    /// Empty means UTF-8, preserving existing `IndexedDB` catalogs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub encoding: String,
    /// Rules active for every current and future profile.
    #[serde(default)]
    pub shared_automation: AutomationDefinition,
    /// Activation of installed local packages, independent of profile records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_activations: Vec<PackageActivation>,
    pub profiles: Vec<SavedProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageActivation {
    pub name: String,
    pub activation: ProfileActivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedProfile {
    pub name: String,
    pub caption: String,
    pub send_on_connect: String,
    #[serde(default)]
    pub automation: AutomationDefinition,
    /// v2 compatibility only. Loaded catalogs migrate these names into
    /// `SavedServer::package_activations`; new saves leave the list empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

/// Password text crossing the window's storage adapter. Debug output is
/// deliberately redacted because iced messages and effects can be traced.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectServer(String),
    EditServer(Option<String>),
    ServerName(String),
    ServerEndpoint(String),
    ServerEncoding(String),
    SaveServer,
    DeleteServer(String),
    ConfirmDeleteServer(String),
    EditProfile(Option<String>),
    ProfileName(String),
    ProfileCaption(String),
    ProfilePassword(SecretString),
    ClearProfilePassword,
    ProfileSendOnConnect(text_editor::Action),
    AddAlias,
    AddTrigger,
    RulePattern {
        trigger: bool,
        index: usize,
        value: String,
    },
    RuleCommand {
        trigger: bool,
        index: usize,
        value: String,
    },
    RemoveRule {
        trigger: bool,
        index: usize,
    },
    ProfileScript(text_editor::Action),
    ImportPackage,
    PackageImported {
        nonce: u64,
        name: String,
    },
    RemovePackage(String),
    TogglePackageAll(String),
    SaveProfile,
    DeleteProfile(String),
    ConfirmDeleteProfile(String),
    Connect(String, String),
    RestoreWindow,
    Cancel,
}

impl From<ConnectIntent> for Message {
    fn from(intent: ConnectIntent) -> Self {
        match intent {
            ConnectIntent::SelectServer(name) => Self::SelectServer(name),
            ConnectIntent::NewServer => Self::EditServer(None),
            ConnectIntent::EditServer(name) => Self::EditServer(Some(name)),
            ConnectIntent::NewProfile => Self::EditProfile(None),
            ConnectIntent::EditProfile(name) => Self::EditProfile(Some(name)),
            ConnectIntent::Connect { server, profile } => Self::Connect(server, profile),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Effect {
    Persist {
        expected: u64,
        catalog: Catalog,
        secret: Option<SecretChange>,
    },
    Connect(ConnectionRequest),
    RestoreWindow,
    ImportPackage {
        nonce: u64,
    },
    None,
}

/// An explicit credential edit accompanying one generation-checked catalog
/// mutation. Password text never becomes part of the serializable catalog.
#[derive(Debug, Clone, Serialize)]
pub struct SecretChange {
    pub server: String,
    pub profile: String,
    pub value: Option<SecretString>,
}

enum Form {
    None,
    ConfirmDeleteServer {
        name: String,
        generation: u64,
    },
    ConfirmDeleteProfile {
        server: String,
        name: String,
        generation: u64,
    },
    Server {
        original: Option<String>,
        name: String,
        endpoint: String,
        encoding: String,
    },
    Profile {
        original: Option<String>,
        name: String,
        caption: String,
        password: String,
        clear_password: bool,
        send_on_connect: text_editor::Content,
        aliases: Vec<PlaintextRule>,
        triggers: Vec<PlaintextRule>,
        script: text_editor::Content,
        packages: Vec<String>,
        all_packages: BTreeSet<String>,
    },
}

pub struct State {
    pub catalog: Catalog,
    pub selected_server: Option<String>,
    pub error: Option<String>,
    pub busy: bool,
    restore_count: usize,
    pending_selection: Option<String>,
    pending_connect: Option<(String, String)>,
    profile_form_nonce: u64,
    form: Form,
}

impl Default for State {
    fn default() -> Self {
        Self {
            catalog: Catalog::default(),
            selected_server: None,
            error: None,
            busy: false,
            restore_count: 0,
            pending_selection: None,
            pending_connect: None,
            profile_form_nonce: 0,
            form: Form::None,
        }
    }
}

impl State {
    pub fn set_restore_count(&mut self, count: usize) {
        self.restore_count = count;
    }

    pub fn loaded(&mut self, mut catalog: Catalog) {
        catalog.normalize_packages();
        self.selected_server = smudgy_session_model::connect::preferred_index(
            catalog.servers.iter().map(|server| server.name.as_str()),
            self.selected_server.as_deref(),
        )
        .map(|index| catalog.servers[index].name.clone());
        self.catalog = catalog;
        self.busy = false;
    }

    pub fn saved(&mut self, catalog: Catalog) -> Option<ConnectionRequest> {
        if let Some(selection) = self.pending_selection.take() {
            self.selected_server = Some(selection);
        }
        self.loaded(catalog);
        self.form = Form::None;
        self.error = None;
        let (server, profile) = self.pending_connect.take()?;
        self.catalog.resolve_connection(&server, &profile)
    }

    pub fn failed(&mut self, error: impl Into<String>) {
        self.busy = false;
        self.pending_selection = None;
        self.pending_connect = None;
        self.error = Some(error.into());
    }

    /// A cancelled or replaced profile draft must not receive a late import error.
    pub fn import_failed(&mut self, nonce: u64, error: impl Into<String>) {
        if !self.busy
            && nonce == self.profile_form_nonce
            && matches!(self.form, Form::Profile { .. })
        {
            self.failed(error);
        }
    }

    /// # Panics
    ///
    /// Panics only if the selected server disappears from the catalog during
    /// a synchronous update.
    #[allow(clippy::too_many_lines)]
    pub fn update(&mut self, message: Message) -> Effect {
        if self.busy {
            return Effect::None;
        }
        self.error = None;
        match message {
            Message::SelectServer(name) => {
                if self.catalog.servers.iter().any(|s| s.name == name) {
                    self.profile_form_nonce = self.profile_form_nonce.wrapping_add(1);
                    self.selected_server = Some(name);
                    self.form = Form::None;
                }
            }
            Message::EditServer(original) => {
                let server = original
                    .as_ref()
                    .and_then(|name| self.catalog.servers.iter().find(|s| &s.name == name));
                self.form = Form::Server {
                    original,
                    name: server.map_or(String::new(), |s| s.name.clone()),
                    endpoint: server.map_or(String::new(), |s| s.endpoint.clone()),
                    encoding: server.map_or(String::new(), |s| s.encoding.clone()),
                };
            }
            Message::ServerName(value) => {
                if let Form::Server {
                    original: None,
                    name,
                    ..
                } = &mut self.form
                {
                    *name = value;
                }
            }
            Message::ServerEndpoint(value) => {
                if let Form::Server { endpoint, .. } = &mut self.form {
                    *endpoint = value;
                }
            }
            Message::ServerEncoding(value) => {
                if let Form::Server { encoding, .. } = &mut self.form {
                    *encoding = value;
                }
            }
            Message::SaveServer => return self.save_server(),
            Message::DeleteServer(name) => {
                if self
                    .catalog
                    .servers
                    .iter()
                    .any(|server| server.name == name)
                {
                    self.form = Form::ConfirmDeleteServer {
                        name,
                        generation: self.catalog.generation,
                    };
                }
            }
            Message::ConfirmDeleteServer(name) => {
                if !matches!(
                    &self.form,
                    Form::ConfirmDeleteServer { name: pending, generation }
                        if pending == &name && *generation == self.catalog.generation
                ) {
                    return Effect::None;
                }
                let mut catalog = self.catalog.clone();
                catalog.servers.retain(|s| s.name != name);
                if catalog.servers.len() == self.catalog.servers.len() {
                    return Effect::None;
                }
                return self.persist(catalog);
            }
            Message::EditProfile(original) => {
                self.profile_form_nonce = self.profile_form_nonce.wrapping_add(1);
                let profile = self.selected().and_then(|s| {
                    original
                        .as_ref()
                        .and_then(|name| s.profiles.iter().find(|p| &p.name == name))
                });
                let packages: Vec<String> = self.selected().map_or_else(Vec::new, |server| {
                    server
                        .package_activations
                        .iter()
                        .filter(|scope| {
                            matches!(scope.activation, ProfileActivation::All)
                                || profile.is_some_and(|profile| {
                                    scope.activation.is_enabled_for(&profile.name)
                                })
                        })
                        .map(|scope| scope.name.clone())
                        .collect()
                });
                let all_packages: BTreeSet<String> =
                    self.selected().map_or_else(BTreeSet::new, |server| {
                        server
                            .package_activations
                            .iter()
                            .filter(|scope| matches!(scope.activation, ProfileActivation::All))
                            .map(|scope| scope.name.clone())
                            .collect()
                    });
                self.form = Form::Profile {
                    original,
                    name: profile.map_or(String::new(), |p| p.name.clone()),
                    caption: profile.map_or(String::new(), |p| p.caption.clone()),
                    password: String::new(),
                    clear_password: false,
                    send_on_connect: text_editor::Content::with_text(
                        profile.map_or("", |p| &p.send_on_connect),
                    ),
                    aliases: profile.map_or_else(Vec::new, |p| p.automation.aliases.clone()),
                    triggers: profile.map_or_else(Vec::new, |p| p.automation.triggers.clone()),
                    script: text_editor::Content::with_text(
                        profile.map_or("", |p| &p.automation.script),
                    ),
                    packages,
                    all_packages,
                };
            }
            Message::ProfileName(value) => {
                if let Form::Profile {
                    original: None,
                    name,
                    ..
                } = &mut self.form
                {
                    *name = value;
                }
            }
            Message::ProfileCaption(value) => {
                if let Form::Profile { caption, .. } = &mut self.form {
                    *caption = value;
                }
            }
            Message::ProfilePassword(value) => {
                if let Form::Profile {
                    password,
                    clear_password,
                    ..
                } = &mut self.form
                {
                    *password = value.0;
                    *clear_password = false;
                }
            }
            Message::ClearProfilePassword => {
                if let Form::Profile {
                    original: Some(_),
                    password,
                    clear_password,
                    ..
                } = &mut self.form
                {
                    password.clear();
                    *clear_password = true;
                }
            }
            Message::ProfileSendOnConnect(action) => {
                if let Form::Profile {
                    send_on_connect, ..
                } = &mut self.form
                {
                    send_on_connect.perform(action);
                }
            }
            Message::AddAlias => {
                if let Form::Profile { aliases, .. } = &mut self.form {
                    aliases.push(PlaintextRule::default());
                }
            }
            Message::AddTrigger => {
                if let Form::Profile { triggers, .. } = &mut self.form {
                    triggers.push(PlaintextRule::default());
                }
            }
            Message::RulePattern {
                trigger,
                index,
                value,
            } => {
                if let Some(rule) = self.rule_mut(trigger, index) {
                    rule.pattern = value;
                }
            }
            Message::RuleCommand {
                trigger,
                index,
                value,
            } => {
                if let Some(rule) = self.rule_mut(trigger, index) {
                    rule.command = value;
                }
            }
            Message::RemoveRule { trigger, index } => {
                if let Form::Profile {
                    aliases, triggers, ..
                } = &mut self.form
                {
                    let rules = if trigger { triggers } else { aliases };
                    if index < rules.len() {
                        rules.remove(index);
                    }
                }
            }
            Message::ProfileScript(action) => {
                if let Form::Profile { script, .. } = &mut self.form {
                    script.perform(action);
                }
            }
            Message::ImportPackage => {
                if matches!(self.form, Form::Profile { .. }) {
                    return Effect::ImportPackage {
                        nonce: self.profile_form_nonce,
                    };
                }
            }
            Message::PackageImported { nonce, name } => {
                if nonce == self.profile_form_nonce
                    && let Form::Profile { packages, .. } = &mut self.form
                    && !packages.contains(&name)
                {
                    packages.push(name);
                    packages.sort();
                }
            }
            Message::RemovePackage(name) => {
                if let Form::Profile {
                    packages,
                    all_packages,
                    ..
                } = &mut self.form
                {
                    packages.retain(|package| package != &name);
                    all_packages.remove(&name);
                }
            }
            Message::TogglePackageAll(name) => {
                if let Form::Profile {
                    packages,
                    all_packages,
                    ..
                } = &mut self.form
                    && packages.contains(&name)
                    && !all_packages.remove(&name)
                {
                    all_packages.insert(name);
                }
            }
            Message::SaveProfile => return self.save_profile(),
            Message::DeleteProfile(name) => {
                if let Some(server) = self
                    .selected()
                    .filter(|server| server.profiles.iter().any(|profile| profile.name == name))
                {
                    self.form = Form::ConfirmDeleteProfile {
                        server: server.name.clone(),
                        name,
                        generation: self.catalog.generation,
                    };
                }
            }
            Message::ConfirmDeleteProfile(name) => {
                let Some(selected) = self.selected_server.clone() else {
                    return Effect::None;
                };
                if !matches!(
                    &self.form,
                    Form::ConfirmDeleteProfile { server, name: pending, generation }
                        if server == &selected && pending == &name && *generation == self.catalog.generation
                ) {
                    return Effect::None;
                }
                let mut catalog = self.catalog.clone();
                let Some(server) = catalog.servers.iter_mut().find(|s| s.name == selected) else {
                    return Effect::None;
                };
                let before = server.profiles.len();
                server.profiles.retain(|p| p.name != name);
                if server.profiles.len() == before {
                    return Effect::None;
                }
                for scope in &mut server.package_activations {
                    scope.activation = scope.activation.clone().without_profile(&name);
                }
                return self.persist(catalog);
            }
            Message::Connect(server_name, profile) => {
                if let Some(server) = self.selected().filter(|server| server.name == server_name) {
                    let profile = profile.as_str();
                    if profile == DEFAULT_PROFILE_NAME
                        && !server
                            .profiles
                            .iter()
                            .any(|saved| saved.name == DEFAULT_PROFILE_NAME)
                    {
                        let server_name = server.name.clone();
                        let mut catalog = self.catalog.clone();
                        let target = catalog
                            .servers
                            .iter_mut()
                            .find(|saved| saved.name == server_name)
                            .expect("selected server exists in the catalog");
                        target.profiles.push(SavedProfile {
                            name: DEFAULT_PROFILE_NAME.to_owned(),
                            caption: String::new(),
                            send_on_connect: String::new(),
                            automation: AutomationDefinition::default(),
                            packages: Vec::new(),
                        });
                        target.profiles.sort_by(|a, b| a.name.cmp(&b.name));
                        let effect = self.persist(catalog);
                        if matches!(effect, Effect::Persist { .. }) {
                            self.pending_connect =
                                Some((server_name, DEFAULT_PROFILE_NAME.to_owned()));
                        }
                        return effect;
                    }
                    if let Some(request) = self.catalog.resolve_connection(&server.name, profile) {
                        return Effect::Connect(request);
                    }
                }
            }
            Message::RestoreWindow => {
                if self.restore_count > 0 {
                    self.restore_count = 0;
                    return Effect::RestoreWindow;
                }
            }
            Message::Cancel => {
                self.profile_form_nonce = self.profile_form_nonce.wrapping_add(1);
                self.form = Form::None;
            }
        }
        Effect::None
    }

    fn selected(&self) -> Option<&SavedServer> {
        self.selected_server
            .as_ref()
            .and_then(|name| self.catalog.servers.iter().find(|s| &s.name == name))
    }

    fn view_model(&self) -> ConnectViewModel<'_> {
        let inventory = self
            .selected()
            .map_or(ProfileInventory::Unavailable, |server| {
                ProfileInventory::Ready(server.profiles.iter().map(|profile| ProfileSummary {
                    name: &profile.name,
                    caption: &profile.caption,
                }))
            });
        let form = match &self.form {
            Form::None => ConnectForm::None,
            Form::Server { .. } | Form::ConfirmDeleteServer { .. } => ConnectForm::Server,
            Form::Profile { .. } | Form::ConfirmDeleteProfile { .. } => ConnectForm::Profile,
        };
        ConnectViewModel::project(
            self.catalog
                .servers
                .iter()
                .map(|server| server.name.as_str()),
            self.selected_server.as_deref(),
            inventory,
            form,
        )
    }

    fn rule_mut(&mut self, trigger: bool, index: usize) -> Option<&mut PlaintextRule> {
        let Form::Profile {
            aliases, triggers, ..
        } = &mut self.form
        else {
            return None;
        };
        (if trigger { triggers } else { aliases }).get_mut(index)
    }

    fn save_server(&mut self) -> Effect {
        let Form::Server {
            original,
            name,
            endpoint,
            encoding,
        } = &self.form
        else {
            return Effect::None;
        };
        let name = name.trim().to_owned();
        let endpoint = endpoint.trim().to_owned();
        let encoding = encoding.trim();
        if let Err(error) = validate_name(&name).and_then(|()| validate_endpoint(&endpoint)) {
            self.error = Some(error);
            return Effect::None;
        }
        let encoding = if encoding.is_empty() {
            String::new()
        } else if let Some(resolved) =
            encoding_rs::Encoding::for_label_no_replacement(encoding.as_bytes())
        {
            resolved.name().to_owned()
        } else {
            self.error = Some("Enter a supported character encoding".to_owned());
            return Effect::None;
        };
        let mut catalog = self.catalog.clone();
        if catalog
            .servers
            .iter()
            .any(|s| s.name == name && original.as_ref() != Some(&name))
        {
            self.error = Some("A server with that name already exists".to_owned());
            return Effect::None;
        }
        if let Some(original) = original {
            let Some(server) = catalog.servers.iter_mut().find(|s| &s.name == original) else {
                return Effect::None;
            };
            server.name.clone_from(&name);
            server.endpoint = endpoint;
            server.encoding = encoding;
        } else {
            catalog.servers.push(SavedServer {
                name: name.clone(),
                endpoint,
                encoding,
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: Vec::new(),
            });
        }
        catalog.servers.sort_by(|a, b| a.name.cmp(&b.name));
        self.pending_selection = Some(name);
        self.persist(catalog)
    }

    #[allow(clippy::too_many_lines)]
    fn save_profile(&mut self) -> Effect {
        let Form::Profile {
            original,
            name,
            caption,
            password,
            clear_password,
            send_on_connect,
            aliases,
            triggers,
            script,
            packages,
            all_packages,
        } = &self.form
        else {
            return Effect::None;
        };
        let name = name.trim().to_owned();
        if let Err(error) = validate_name(&name) {
            self.error = Some(error);
            return Effect::None;
        }
        for (kind, rules) in [("Alias", aliases), ("Trigger", triggers)] {
            for (index, rule) in rules.iter().enumerate() {
                if rule.pattern.is_empty() || rule.command.is_empty() {
                    self.error = Some(format!("{kind} {} needs a pattern and command", index + 1));
                    return Effect::None;
                }
                if let Err(error) = regex::Regex::new(&rule.pattern) {
                    self.error = Some(format!("{kind} {}: {error}", index + 1));
                    return Effect::None;
                }
            }
        }
        let Some(selected) = self.selected_server.clone() else {
            return Effect::None;
        };
        let mut catalog = self.catalog.clone();
        let Some(server) = catalog.servers.iter_mut().find(|s| s.name == selected) else {
            return Effect::None;
        };
        if server
            .profiles
            .iter()
            .any(|p| p.name == name && original.as_ref() != Some(&name))
        {
            self.error = Some("A profile with that name already exists".to_owned());
            return Effect::None;
        }
        let profile_name = name.clone();
        let profile = SavedProfile {
            name,
            caption: caption.trim().to_owned(),
            send_on_connect: send_on_connect.text(),
            automation: AutomationDefinition {
                aliases: aliases.clone(),
                triggers: triggers.clone(),
                script: script.text(),
            },
            packages: Vec::new(),
        };
        if let Some(original) = original {
            let Some(existing) = server.profiles.iter_mut().find(|p| &p.name == original) else {
                return Effect::None;
            };
            *existing = profile;
        } else {
            server.profiles.push(profile);
        }
        server.profiles.sort_by(|a, b| a.name.cmp(&b.name));
        let known: BTreeSet<String> = server
            .profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect();
        for scope in &mut server.package_activations {
            let enabled = packages.contains(&scope.name);
            scope.activation = if all_packages.contains(&scope.name) {
                ProfileActivation::All
            } else if enabled && scope.activation == ProfileActivation::All {
                ProfileActivation::Selected {
                    profiles: BTreeSet::from([profile_name.clone()]),
                }
            } else {
                scope
                    .activation
                    .with_profile(&profile_name, enabled, &known)
            };
        }
        for package in packages {
            if !server
                .package_activations
                .iter()
                .any(|scope| scope.name == *package)
            {
                server.package_activations.push(PackageActivation {
                    name: package.clone(),
                    activation: if all_packages.contains(package) {
                        ProfileActivation::All
                    } else {
                        ProfileActivation::Selected {
                            profiles: BTreeSet::from([profile_name.clone()]),
                        }
                    },
                });
            }
        }
        server
            .package_activations
            .sort_by(|a, b| a.name.cmp(&b.name));
        let secret = if !password.is_empty() || *clear_password {
            Some(SecretChange {
                server: selected,
                profile: profile_name,
                value: (!*clear_password).then(|| SecretString(password.clone())),
            })
        } else {
            None
        };
        self.persist_with_secret(catalog, secret)
    }

    fn persist(&mut self, catalog: Catalog) -> Effect {
        self.persist_with_secret(catalog, None)
    }

    fn persist_with_secret(
        &mut self,
        mut catalog: Catalog,
        secret: Option<SecretChange>,
    ) -> Effect {
        let expected = self.catalog.generation;
        // IndexedDB's JavaScript adapter compares numeric JSON generations.
        // Stop before IEEE-754 integers lose exactness instead of weakening CAS.
        if expected >= 9_007_199_254_740_991 {
            self.error = Some("Saved catalog generation exceeded its safe limit".to_owned());
            return Effect::None;
        }
        catalog.generation = expected + 1;
        self.busy = true;
        Effect::Persist {
            expected,
            catalog,
            secret,
        }
    }
}

/// Validate a catalog name.
///
/// # Errors
///
/// Returns a description when the name is empty or contains unsupported characters.
pub fn validate_name(name: &str) -> Result<(), String> {
    if smudgy_session_model::connect::valid_name(name) {
        Ok(())
    } else {
        Err("Use a nonempty name with only letters, numbers, _ or -".to_owned())
    }
}

/// Validate a browser WSS endpoint.
///
/// # Errors
///
/// Returns a description when the endpoint is not a supported secure URL.
pub fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    smudgy_protocol::wss::parse_address(endpoint)
        .map(|_| ())
        .map_err(str::to_owned)
}

#[allow(clippy::too_many_lines)]
pub fn view(state: &State) -> Element<'_, Message, Theme> {
    let ConnectViewModel {
        panel,
        servers,
        selected,
        new_server,
    } = state.view_model();
    let rail = connect_modal::server_rail(
        "Servers",
        "No saved servers",
        "+ New Server",
        servers
            .into_iter()
            .map(|server| RailItem {
                label: text(server.name).into(),
                selected: server.selected,
                select: server.select.map(Message::from),
            })
            .collect(),
        Message::from(new_server.clone()),
    );
    let details: Element<'_, Message, Theme> = match &state.form {
        Form::ConfirmDeleteServer { name, .. } => connect_modal::form(
            text("Delete server").size(Pixels(22.0)).into(),
            vec![text(format!("Delete server {name} and its profiles?")).into()],
            row![
                button("Delete")
                    .style(builtins::button::secondary)
                    .padding([8, 18])
                    .on_press(Message::ConfirmDeleteServer(name.clone())),
                button("Cancel")
                    .style(builtins::button::secondary)
                    .padding([8, 18])
                    .on_press(Message::Cancel),
            ]
            .spacing(10)
            .into(),
            None,
        ),
        Form::ConfirmDeleteProfile { name, .. } => connect_modal::form(
            text("Delete profile").size(Pixels(22.0)).into(),
            vec![text(format!("Delete profile {name}?")).into()],
            row![
                button("Delete")
                    .style(builtins::button::secondary)
                    .padding([8, 18])
                    .on_press(Message::ConfirmDeleteProfile(name.clone())),
                button("Cancel")
                    .style(builtins::button::secondary)
                    .padding([8, 18])
                    .on_press(Message::Cancel),
            ]
            .spacing(10)
            .into(),
            None,
        ),
        Form::Server {
            original,
            name,
            endpoint,
            encoding,
        } => {
            let name_field = if original.is_some() {
                connect_modal::field("Server name", text(name).size(Pixels(16.0)).into())
            } else {
                connect_modal::field(
                    "Server name",
                    text_input("Server name", name)
                        .on_input(Message::ServerName)
                        .on_submit(Message::SaveServer)
                        .into(),
                )
            };
            connect_modal::form(
                text(if original.is_some() {
                    "Edit server"
                } else {
                    "Add server"
                })
                .size(Pixels(22.0))
                .into(),
                vec![
                    name_field,
                    connect_modal::field(
                        "WSS URL",
                        text_input("wss://example.org/path", endpoint)
                            .on_input(Message::ServerEndpoint)
                            .on_submit(Message::SaveServer)
                            .into(),
                    ),
                    connect_modal::field(
                        "Character encoding (blank = UTF-8)",
                        text_input("UTF-8 or windows-1252", encoding)
                            .on_input(Message::ServerEncoding)
                            .on_submit(Message::SaveServer)
                            .into(),
                    ),
                ],
                row![
                    button("Save")
                        .style(builtins::button::primary)
                        .padding([8, 18])
                        .on_press(Message::SaveServer),
                    button("Cancel")
                        .style(builtins::button::secondary)
                        .padding([8, 18])
                        .on_press(Message::Cancel),
                ]
                .spacing(10)
                .into(),
                original.as_ref().map(|name| {
                    button("Delete server")
                        .style(builtins::button::link)
                        .on_press(Message::DeleteServer(name.clone()))
                        .into()
                }),
            )
        }
        Form::Profile {
            original,
            name,
            caption,
            password,
            clear_password,
            send_on_connect,
            aliases,
            triggers,
            script,
            packages,
            all_packages,
        } => {
            let name_field = if original.is_some() {
                connect_modal::field("Profile name", text(name).size(Pixels(16.0)).into())
            } else {
                connect_modal::field(
                    "Profile name",
                    text_input("Profile name", name)
                        .on_input(Message::ProfileName)
                        .on_submit(Message::SaveProfile)
                        .into(),
                )
            };
            let send_on_connect = column![
                connect_modal::field(
                    "Send on connect",
                    text_editor(send_on_connect)
                        .placeholder("One command per line")
                        .height(Length::Fixed(140.0))
                        .font(smudgy_ui_shared::assets::GEIST_MONO)
                        .on_action(Message::ProfileSendOnConnect)
                        .into(),
                ),
                container(
                    text("Use $PASSWORD for the saved password. Command text is not secret.")
                        .size(12)
                )
                .width(Fill)
                .padding([6, 10])
                .style(builtins::container::notice),
            ]
            .spacing(8)
            .into();
            let mut password_field = column![connect_modal::field(
                "Saved password",
                text_input("Enter a new password, or leave unchanged", password)
                    .secure(true)
                    .on_input(|value| Message::ProfilePassword(value.into()))
                    .into(),
            )]
            .spacing(6);
            if original.is_some() {
                password_field = password_field.push(
                    button("Clear saved password")
                        .style(builtins::button::link)
                        .on_press(Message::ClearProfilePassword),
                );
            }
            password_field = password_field.push(
                text(if *clear_password {
                    "The saved password will be removed when you save."
                } else {
                    "Stored separately in this browser. Existing passwords are not displayed."
                })
                .size(Pixels(12.0)),
            );
            connect_modal::form(
                text(format!(
                    "{} · {}",
                    if original.is_some() {
                        "Edit profile"
                    } else {
                        "Add profile"
                    },
                    state.selected_server.as_deref().unwrap_or_default()
                ))
                .size(Pixels(22.0))
                .into(),
                vec![
                    name_field,
                    connect_modal::field(
                        "Description",
                        text_input("Description", caption)
                            .on_input(Message::ProfileCaption)
                            .on_submit(Message::SaveProfile)
                            .into(),
                    ),
                    send_on_connect,
                    password_field.into(),
                    rules_view("Aliases", aliases, false),
                    rules_view("Triggers", triggers, true),
                    column![
                        connect_modal::field(
                            "Session JavaScript (synchronous worker handlers)",
                            text_editor(script)
                                .placeholder(
                                    "function onLine(line, api) { }\nfunction onInput(text, api) { }",
                                )
                                .height(Length::Fixed(180.0))
                                .font(smudgy_ui_shared::assets::GEIST_MONO)
                                .on_action(Message::ProfileScript)
                                .into(),
                        ),
                        container(text("Use only scripts you trust. Source is stored in this browser and has worker browser access.").size(12))
                            .width(Fill)
                            .padding([6, 10])
                            .style(builtins::container::notice),
                    ].spacing(8).into(),
                    column![
                        text("Local packages").size(Pixels(14.0)),
                        packages.iter().fold(column![].spacing(4), |items, package| {
                            items.push(row![
                                text(package),
                                button(if all_packages.contains(package) { "All profiles" } else { "Only this profile" })
                                    .style(builtins::button::secondary)
                                    .on_press(Message::TogglePackageAll(package.clone())),
                                button("Remove")
                                    .style(builtins::button::link)
                                    .on_press(Message::RemovePackage(package.clone())),
                            ].spacing(12))
                        }),
                        button("+ Import local package")
                            .style(builtins::button::secondary)
                            .on_press(Message::ImportPackage),
                        text("All profiles includes future profiles. Default-only does not. Import only source you trust.")
                            .size(Pixels(12.0)),
                    ].spacing(8).into(),
                ],
                row![
                    button("Save")
                        .style(builtins::button::primary)
                        .padding([8, 18])
                        .on_press(Message::SaveProfile),
                    button("Cancel")
                        .style(builtins::button::secondary)
                        .padding([8, 18])
                        .on_press(Message::Cancel),
                ]
                .spacing(10)
                .into(),
                original.as_ref().map(|name| {
                    button("Delete profile")
                        .style(builtins::button::link)
                        .on_press(Message::DeleteProfile(name.clone()))
                        .into()
                }),
            )
        }
        Form::None => match (state.selected(), selected) {
            (Some(server), Some(details)) => {
                let profiles: Element<'_, Message, Theme> = match details.profiles {
                    Profiles::Loading => column![].into(),
                    Profiles::Unavailable => text("Could not load profiles")
                        .style(builtins::text::danger)
                        .into(),
                    Profiles::Ready(rows) if rows.is_empty() => connect_modal::empty_profiles(
                        "No profiles yet",
                        "+ New Profile",
                        Message::from(ConnectIntent::NewProfile),
                    ),
                    Profiles::Ready(rows) => rows
                        .into_iter()
                        .fold(column![].spacing(10), |column, profile| {
                            column.push(connect_modal::profile_row(
                                profile.name,
                                profile.caption,
                                vec![
                                    button("Edit")
                                        .style(builtins::button::link)
                                        .on_press(Message::from(profile.edit))
                                        .into(),
                                    button("Connect")
                                        .style(builtins::button::primary)
                                        .padding([6, 16])
                                        .on_press(Message::from(profile.connect))
                                        .into(),
                                ],
                            ))
                        })
                        .into(),
                };
                let mut summary = vec![
                    text(&server.endpoint)
                        .font(smudgy_ui_shared::assets::GEIST_MONO)
                        .size(13)
                        .style(builtins::text::muted)
                        .into(),
                ];
                if let Some(intent) = details.quick_connect {
                    summary.push(
                        button("Connect with Default")
                            .width(Fill)
                            .padding([6, 10])
                            .style(builtins::button::secondary)
                            .on_press(Message::from(intent))
                            .into(),
                    );
                }
                connect_modal::server_details(ServerDetails {
                    name: details.name,
                    icon: None,
                    edit: button("Edit")
                        .style(builtins::button::link)
                        .padding([2, 8])
                        .on_press(Message::from(details.edit))
                        .into(),
                    summary,
                    profiles_title: "Profiles".to_owned(),
                    profiles_help: "Saved logins for this server.".to_owned(),
                    profiles,
                    new_profile: details.new_profile.map(|intent| {
                        connect_modal::new_profile("+ New Profile", Message::from(intent))
                    }),
                })
            }
            _ => connect_modal::empty_servers(
                "Get started",
                "Add a WSS server to connect with a saved profile.",
                "+ New Server",
                Message::from(new_server),
            ),
        },
    };
    let own_scroll = panel == ConnectPanel::ServerDetails;
    let mut banners = column![].spacing(4);
    if state.restore_count > 0 {
        let count = state.restore_count;
        banners = banners.push(
            container(
                row![
                    text(format!(
                        "Previous window: {count} session{}",
                        if count == 1 { "" } else { "s" }
                    ))
                    .width(Fill),
                    button("Restore this window")
                        .style(builtins::button::secondary)
                        .on_press(Message::RestoreWindow),
                ]
                .spacing(10),
            )
            .padding([8, 15]),
        );
    }
    if let Some(error) = &state.error {
        banners =
            banners.push(container(text(error).style(builtins::text::danger)).padding([8, 15]));
    }
    let banner = (state.restore_count > 0 || state.error.is_some()).then(|| banners.into());
    let details = if state.busy {
        column![details, text("Saving…")].spacing(8).into()
    } else {
        details
    };
    connect_modal::body(rail, details, own_scroll, banner)
}

fn rules_view<'a>(
    title: &'static str,
    rules: &'a [PlaintextRule],
    trigger: bool,
) -> Element<'a, Message, Theme> {
    let mut fields = column![text(title).size(16)].spacing(7);
    for (index, rule) in rules.iter().enumerate() {
        fields = fields.push(
            row![
                text_input("Regex pattern", &rule.pattern).on_input(move |value| {
                    Message::RulePattern {
                        trigger,
                        index,
                        value,
                    }
                }),
                text_input("Command ($1 captures)", &rule.command).on_input(move |value| {
                    Message::RuleCommand {
                        trigger,
                        index,
                        value,
                    }
                }),
                button("Remove")
                    .style(builtins::button::secondary)
                    .on_press(Message::RemoveRule { trigger, index }),
            ]
            .spacing(8),
        );
    }
    fields = fields.push(
        button(if trigger {
            "+ Add trigger"
        } else {
            "+ Add alias"
        })
        .style(builtins::button::secondary)
        .on_press(if trigger {
            Message::AddTrigger
        } else {
            Message::AddAlias
        }),
    );
    fields.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_full_wss_path_and_rejects_credentials() {
        assert!(validate_endpoint("wss://last-outpost.com/ws/telnet/?session=1").is_ok());
        assert!(validate_endpoint("wss://user:secret@example.com/ws").is_err());
        assert!(validate_endpoint("ws://example.com/ws").is_err());
        assert!(validate_endpoint("wss://example.com/ws#fragment").is_err());
        assert!(validate_endpoint("wss://example.com:0/ws").is_err());
    }

    #[test]
    fn saves_profiles_without_erasing_server_address() {
        let mut state = State::default();
        state.update(Message::EditServer(None));
        state.update(Message::ServerName("Outpost".to_owned()));
        state.update(Message::ServerEndpoint(
            "wss://last-outpost.com/ws/telnet/".to_owned(),
        ));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveServer) else {
            panic!("server save");
        };
        state.saved(catalog);
        state.update(Message::EditProfile(None));
        state.update(Message::ProfileName("Main".to_owned()));
        state.update(Message::ProfileSendOnConnect(text_editor::Action::Edit(
            text_editor::Edit::Paste(std::sync::Arc::new("look".to_owned())),
        )));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("profile save");
        };
        assert_eq!(
            catalog.servers[0].endpoint,
            "wss://last-outpost.com/ws/telnet/"
        );
        assert_eq!(catalog.servers[0].profiles[0].send_on_connect, "look");
    }

    #[test]
    fn imported_package_applies_only_to_the_draft_that_requested_it() {
        let mut state = State::default();
        state.catalog.servers.push(SavedServer {
            name: "mud".into(),
            endpoint: "wss://example.org/ws".into(),
            encoding: String::new(),
            shared_automation: AutomationDefinition::default(),
            package_activations: Vec::new(),
            profiles: Vec::new(),
        });
        state.selected_server = Some("mud".into());
        state.update(Message::EditProfile(None));
        let Effect::ImportPackage { nonce } = state.update(Message::ImportPackage) else {
            panic!("package import");
        };
        state.update(Message::Cancel);
        state.update(Message::EditProfile(None));
        state.import_failed(nonce, "stale error");
        assert!(state.error.is_none());
        state.update(Message::PackageImported {
            nonce,
            name: "stale".into(),
        });
        let Effect::ImportPackage { nonce } = state.update(Message::ImportPackage) else {
            panic!("new package import");
        };
        state.update(Message::PackageImported {
            nonce,
            name: "current".into(),
        });
        state.update(Message::ProfileName("main".into()));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("profile save");
        };
        assert!(catalog.servers[0].profiles[0].packages.is_empty());
        assert_eq!(
            catalog.servers[0].package_activations,
            [PackageActivation {
                name: "current".into(),
                activation: ProfileActivation::Selected {
                    profiles: BTreeSet::from(["main".into()])
                },
            }]
        );
    }

    #[test]
    fn deletion_requires_a_second_explicit_action() {
        let mut state = State::default();
        state.catalog.servers.push(SavedServer {
            name: "outpost".to_owned(),
            endpoint: "wss://last-outpost.com/ws/telnet/".to_owned(),
            encoding: String::new(),
            shared_automation: AutomationDefinition::default(),
            package_activations: Vec::new(),
            profiles: Vec::new(),
        });
        assert!(matches!(
            state.update(Message::DeleteServer("outpost".to_owned())),
            Effect::None
        ));
        assert_eq!(state.catalog.servers.len(), 1);
        assert!(matches!(
            state.update(Message::ConfirmDeleteServer("other".to_owned())),
            Effect::None
        ));
        assert!(matches!(
            state.update(Message::ConfirmDeleteServer("outpost".to_owned())),
            Effect::Persist { .. }
        ));
    }

    #[test]
    fn stale_or_unrequested_deletes_do_not_mutate_the_catalog() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 4,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: vec![SavedProfile {
                    name: "main".into(),
                    caption: String::new(),
                    send_on_connect: String::new(),
                    automation: AutomationDefinition::default(),
                    packages: Vec::new(),
                }],
            }],
        });
        assert!(matches!(
            state.update(Message::ConfirmDeleteServer("mud".into())),
            Effect::None
        ));
        assert!(matches!(
            state.update(Message::ConfirmDeleteProfile("main".into())),
            Effect::None
        ));

        state.update(Message::DeleteServer("mud".into()));
        state.loaded(Catalog {
            generation: 5,
            ..state.catalog.clone()
        });
        assert!(matches!(
            state.update(Message::ConfirmDeleteServer("mud".into())),
            Effect::None
        ));

        state.update(Message::DeleteProfile("main".into()));
        state.update(Message::Cancel);
        assert!(matches!(
            state.update(Message::ConfirmDeleteProfile("main".into())),
            Effect::None
        ));

        state.update(Message::DeleteProfile("main".into()));
        state.loaded(Catalog {
            generation: 6,
            ..state.catalog.clone()
        });
        assert!(matches!(
            state.update(Message::ConfirmDeleteProfile("main".into())),
            Effect::None
        ));
        assert_eq!(state.catalog.servers[0].profiles.len(), 1);
    }

    #[test]
    fn edit_keeps_server_and_profile_identity_stable() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "outpost".to_owned(),
                endpoint: "wss://example.org/play".to_owned(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: vec![SavedProfile {
                    name: "main".to_owned(),
                    caption: String::new(),
                    send_on_connect: String::new(),
                    automation: AutomationDefinition::default(),
                    packages: Vec::new(),
                }],
            }],
        });

        state.update(Message::EditServer(Some("outpost".to_owned())));
        state.update(Message::ServerName("renamed".to_owned()));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveServer) else {
            panic!("server edit must persist");
        };
        assert_eq!(catalog.servers[0].name, "outpost");
        state.saved(catalog);

        state.update(Message::EditProfile(Some("main".to_owned())));
        state.update(Message::ProfileName("renamed".to_owned()));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("profile edit must persist");
        };
        assert_eq!(catalog.servers[0].profiles[0].name, "main");
    }

    #[test]
    fn existing_catalog_profiles_load_without_automation_field() {
        let catalog: Catalog = serde_json::from_str(r#"{"generation":1,"servers":[{"name":"mud","endpoint":"wss://example.org/ws","profiles":[{"name":"main","caption":"","send_on_connect":"look"}]}]}"#).unwrap();
        assert_eq!(
            catalog.servers[0].profiles[0].automation,
            AutomationDefinition::default()
        );
    }

    #[test]
    fn profile_automation_round_trips_and_rejects_invalid_regex() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: Vec::new(),
            }],
        });
        state.update(Message::EditProfile(None));
        state.update(Message::ProfileName("main".into()));
        state.update(Message::AddAlias);
        state.update(Message::RulePattern {
            trigger: false,
            index: 0,
            value: "[".into(),
        });
        state.update(Message::RuleCommand {
            trigger: false,
            index: 0,
            value: "kill $1".into(),
        });
        assert!(matches!(state.update(Message::SaveProfile), Effect::None));
        assert!(state.error.as_deref().unwrap().starts_with("Alias 1:"));
        state.update(Message::RulePattern {
            trigger: false,
            index: 0,
            value: "^k (.+)$".into(),
        });
        state.update(Message::ProfileScript(text_editor::Action::Edit(
            text_editor::Edit::Paste(std::sync::Arc::new(
                "function onLine(line, api) { api.print(line); }".into(),
            )),
        )));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("profile save")
        };
        state.saved(catalog);
        let Effect::Connect(ConnectionRequest { automation, .. }) =
            state.update(Message::Connect("mud".into(), "main".into()))
        else {
            panic!("connect")
        };
        let named = automation.profile.expect("named scope");
        assert_eq!(named.aliases[0].pattern, "^k (.+)$");
        assert!(named.script.contains("api.print"));
    }

    #[test]
    fn default_profile_is_isolated_while_all_profile_scopes_include_future_profiles() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition {
                    aliases: vec![PlaintextRule {
                        pattern: "^all$".into(),
                        command: "shared".into(),
                    }],
                    ..AutomationDefinition::default()
                },
                package_activations: vec![
                    PackageActivation {
                        name: "all".into(),
                        activation: ProfileActivation::All,
                    },
                    PackageActivation {
                        name: "default-only".into(),
                        activation: ProfileActivation::Selected {
                            profiles: BTreeSet::from([DEFAULT_PROFILE_NAME.into()]),
                        },
                    },
                    PackageActivation {
                        name: "named-only".into(),
                        activation: ProfileActivation::Selected {
                            profiles: BTreeSet::from(["Named".into()]),
                        },
                    },
                ],
                profiles: vec![
                    SavedProfile {
                        name: DEFAULT_PROFILE_NAME.into(),
                        caption: String::new(),
                        send_on_connect: "look".into(),
                        automation: AutomationDefinition {
                            aliases: vec![PlaintextRule {
                                pattern: "^go$".into(),
                                command: "north".into(),
                            }],
                            script: "function onLine() {}".into(),
                            ..AutomationDefinition::default()
                        },
                        packages: Vec::new(),
                    },
                    SavedProfile {
                        name: "Named".into(),
                        caption: String::new(),
                        send_on_connect: "login".into(),
                        automation: AutomationDefinition {
                            aliases: vec![PlaintextRule {
                                pattern: "^go$".into(),
                                command: "south".into(),
                            }],
                            script: "function onInput() {}".into(),
                            ..AutomationDefinition::default()
                        },
                        packages: Vec::new(),
                    },
                ],
            }],
        });
        let Effect::Connect(ConnectionRequest {
            send_on_connect,
            automation,
            packages,
            ..
        }) = state.update(Message::Connect("mud".into(), "Named".into()))
        else {
            panic!("connect named");
        };
        assert_eq!(send_on_connect, "login");
        assert_eq!(automation.shared.aliases[0].command, "shared");
        assert_eq!(automation.profile.unwrap().aliases[0].command, "south");
        assert_eq!(packages, ["all", "named-only"]);

        let Effect::Connect(ConnectionRequest {
            send_on_connect,
            automation,
            packages,
            ..
        }) = state.update(Message::Connect("mud".into(), DEFAULT_PROFILE_NAME.into()))
        else {
            panic!("connect Default");
        };
        assert_eq!(send_on_connect, "look");
        assert_eq!(automation.profile.unwrap().aliases[0].command, "north");
        assert_eq!(packages, ["all", "default-only"]);
    }

    #[test]
    fn quick_connect_saves_default_before_opening_it() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: Vec::new(),
            }],
        });
        let Effect::Persist { catalog, .. } =
            state.update(Message::Connect("mud".into(), DEFAULT_PROFILE_NAME.into()))
        else {
            panic!("create Default");
        };
        assert_eq!(catalog.servers[0].profiles[0].name, DEFAULT_PROFILE_NAME);
        assert_eq!(state.saved(catalog).unwrap().profile, DEFAULT_PROFILE_NAME);
        assert!(matches!(
            state.update(Message::Connect("mud".into(), DEFAULT_PROFILE_NAME.into())),
            Effect::Connect(_)
        ));
    }

    #[test]
    fn stale_connect_intent_cannot_open_a_different_selected_server() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: ["a", "b"]
                .into_iter()
                .map(|name| SavedServer {
                    name: name.into(),
                    endpoint: format!("wss://{name}.example/ws"),
                    encoding: String::new(),
                    shared_automation: AutomationDefinition::default(),
                    package_activations: Vec::new(),
                    profiles: vec![SavedProfile {
                        name: DEFAULT_PROFILE_NAME.into(),
                        caption: String::new(),
                        send_on_connect: String::new(),
                        automation: AutomationDefinition::default(),
                        packages: Vec::new(),
                    }],
                })
                .collect(),
        });
        assert_eq!(state.selected_server.as_deref(), Some("a"));
        assert!(matches!(
            state.update(Message::Connect("b".into(), DEFAULT_PROFILE_NAME.into())),
            Effect::None
        ));
    }

    #[test]
    fn legacy_packages_migrate_to_explicit_selected_scopes_not_all() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: vec![SavedProfile {
                    name: DEFAULT_PROFILE_NAME.into(),
                    caption: String::new(),
                    send_on_connect: String::new(),
                    automation: AutomationDefinition::default(),
                    packages: vec!["solo".into()],
                }],
            }],
        });
        assert!(state.catalog.servers[0].profiles[0].packages.is_empty());
        let activation = &state.catalog.servers[0].package_activations[0].activation;
        assert!(activation.is_enabled_for(DEFAULT_PROFILE_NAME));
        assert!(!activation.is_enabled_for("NewProfile"));
        assert!(!activation.legacy_enabled());
    }

    #[test]
    fn package_scope_toggle_is_explicit_about_future_profiles() {
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: vec![SavedProfile {
                    name: DEFAULT_PROFILE_NAME.into(),
                    caption: String::new(),
                    send_on_connect: String::new(),
                    automation: AutomationDefinition::default(),
                    packages: Vec::new(),
                }],
            }],
        });
        state.update(Message::EditProfile(Some(DEFAULT_PROFILE_NAME.into())));
        let Effect::ImportPackage { nonce } = state.update(Message::ImportPackage) else {
            panic!("import request");
        };
        state.update(Message::PackageImported {
            nonce,
            name: "solo".into(),
        });
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("profile save");
        };
        let selected = &catalog.servers[0].package_activations[0].activation;
        assert!(selected.is_enabled_for(DEFAULT_PROFILE_NAME));
        assert!(!selected.is_enabled_for("Future"));

        state.saved(catalog);
        state.update(Message::EditProfile(Some(DEFAULT_PROFILE_NAME.into())));
        state.update(Message::TogglePackageAll("solo".into()));
        let Effect::Persist { catalog, .. } = state.update(Message::SaveProfile) else {
            panic!("all-profile save");
        };
        assert_eq!(
            catalog.servers[0].package_activations[0].activation,
            ProfileActivation::All
        );
        assert!(
            catalog.servers[0].package_activations[0]
                .activation
                .is_enabled_for("Future")
        );
    }

    #[test]
    fn password_edits_never_enter_the_catalog() {
        assert_eq!(
            format!(
                "{:?}",
                Message::ProfilePassword("log-secret".to_owned().into())
            ),
            "ProfilePassword(<redacted>)"
        );
        let mut state = State::default();
        state.loaded(Catalog {
            generation: 1,
            servers: vec![SavedServer {
                name: "mud".into(),
                endpoint: "wss://example.org/ws".into(),
                encoding: String::new(),
                shared_automation: AutomationDefinition::default(),
                package_activations: Vec::new(),
                profiles: Vec::new(),
            }],
        });
        state.update(Message::EditProfile(None));
        state.update(Message::ProfileName("Named".into()));
        state.update(Message::ProfilePassword(
            "do-not-persist-in-catalog".to_owned().into(),
        ));
        let Effect::Persist {
            catalog,
            secret: Some(secret),
            ..
        } = state.update(Message::SaveProfile)
        else {
            panic!("profile save with credential mutation");
        };
        assert_eq!(secret.server, "mud");
        assert_eq!(secret.profile, "Named");
        assert_eq!(
            secret.value.as_ref().map(SecretString::as_str),
            Some("do-not-persist-in-catalog")
        );
        assert!(
            !serde_json::to_string(&catalog)
                .unwrap()
                .contains("do-not-persist-in-catalog")
        );

        state.saved(catalog);
        state.update(Message::EditProfile(Some("Named".into())));
        state.update(Message::ClearProfilePassword);
        let Effect::Persist {
            secret: Some(secret),
            ..
        } = state.update(Message::SaveProfile)
        else {
            panic!("profile clear credential mutation");
        };
        assert!(secret.value.is_none());
    }
}
