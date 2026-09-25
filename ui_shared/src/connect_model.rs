//! Storage-neutral Connect navigation and presentation policy.
//!
//! A host projects its own server/profile records into borrowed summaries and
//! maps the resulting intents back to its own messages. No persistence API,
//! credential type, network client, or runtime dependency crosses this layer.

use smudgy_session_model::connect::DEFAULT_PROFILE_NAME;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    None,
    Server,
    Profile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Placeholder,
    ServerDetails,
    ServerForm,
    ProfileForm,
}

/// Navigation and admission requests common to both hosts. A host remains
/// responsible for validating the current record and carrying out the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SelectServer(String),
    NewServer,
    EditServer(String),
    NewProfile,
    EditProfile(String),
    Connect { server: String, profile: String },
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileSummary<'a> {
    pub name: &'a str,
    pub caption: &'a str,
}

/// `Unavailable` differs from an empty list: a failed or unfinished read must
/// not offer a quick connect that could overwrite an existing Default profile.
#[derive(Debug)]
pub enum ProfileInventory<I> {
    Loading,
    Unavailable,
    Ready(I),
}

#[derive(Debug, Clone)]
pub struct ServerRow<'a> {
    pub name: &'a str,
    pub selected: bool,
    pub select: Option<Intent>,
}

#[derive(Debug, Clone)]
pub struct ProfileRow<'a> {
    pub name: &'a str,
    pub caption: &'a str,
    pub edit: Intent,
    pub connect: Intent,
}

#[derive(Debug, Clone)]
pub enum Profiles<'a> {
    Loading,
    Unavailable,
    Ready(Vec<ProfileRow<'a>>),
}

#[derive(Debug, Clone)]
pub struct SelectedServer<'a> {
    pub name: &'a str,
    pub edit: Intent,
    pub profiles: Profiles<'a>,
    pub quick_connect: Option<Intent>,
    pub new_profile: Option<Intent>,
}

#[derive(Debug, Clone)]
pub struct ConnectViewModel<'a> {
    pub panel: Panel,
    pub servers: Vec<ServerRow<'a>>,
    pub selected: Option<SelectedServer<'a>>,
    pub new_server: Intent,
}

impl<'a> ConnectViewModel<'a> {
    #[must_use]
    pub fn project<I>(
        server_names: impl IntoIterator<Item = &'a str>,
        selected_name: Option<&str>,
        inventory: ProfileInventory<I>,
        form: Form,
    ) -> Self
    where
        I: IntoIterator<Item = ProfileSummary<'a>>,
    {
        let servers: Vec<_> = server_names
            .into_iter()
            .map(|name| ServerRow {
                name,
                selected: Some(name) == selected_name,
                select: (form != Form::Profile).then(|| Intent::SelectServer(name.to_owned())),
            })
            .collect();
        let selected_name = servers.iter().find(|row| row.selected).map(|row| row.name);
        let panel = match form {
            Form::Server => Panel::ServerForm,
            Form::Profile => Panel::ProfileForm,
            Form::None if selected_name.is_some() => Panel::ServerDetails,
            Form::None => Panel::Placeholder,
        };
        let selected = selected_name
            .filter(|_| panel == Panel::ServerDetails)
            .map(|name| {
                let (profiles, quick_connect, new_profile) = match inventory {
                    ProfileInventory::Loading => (Profiles::Loading, None, None),
                    ProfileInventory::Unavailable => (Profiles::Unavailable, None, None),
                    ProfileInventory::Ready(summaries) => {
                        let mut has_default = false;
                        let mut rows = Vec::new();
                        for profile in summaries {
                            has_default |= profile.name == DEFAULT_PROFILE_NAME;
                            rows.push(ProfileRow {
                                name: profile.name,
                                caption: profile.caption,
                                edit: Intent::EditProfile(profile.name.to_owned()),
                                connect: Intent::Connect {
                                    server: name.to_owned(),
                                    profile: profile.name.to_owned(),
                                },
                            });
                        }
                        let has_profiles = !rows.is_empty();
                        (
                            Profiles::Ready(rows),
                            (!has_default).then(|| Intent::Connect {
                                server: name.to_owned(),
                                profile: DEFAULT_PROFILE_NAME.to_owned(),
                            }),
                            has_profiles.then_some(Intent::NewProfile),
                        )
                    }
                };
                SelectedServer {
                    name,
                    edit: Intent::EditServer(name.to_owned()),
                    profiles,
                    quick_connect,
                    new_profile,
                }
            });
        Self {
            panel,
            servers,
            selected,
            new_server: Intent::NewServer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &'static str) -> ProfileSummary<'static> {
        ProfileSummary { name, caption: "" }
    }

    #[test]
    fn default_is_a_normal_row_and_quick_connect_only_fills_its_absence() {
        let model = ConnectViewModel::project(
            ["mud"],
            Some("mud"),
            ProfileInventory::Ready(vec![profile("Default"), profile("Alt")]),
            Form::None,
        );
        let selected = model.selected.unwrap();
        assert!(selected.quick_connect.is_none());
        assert!(selected.new_profile.is_some());
        let Profiles::Ready(rows) = selected.profiles else {
            panic!("loaded profiles");
        };
        assert_eq!(
            rows[0].connect,
            Intent::Connect {
                server: "mud".into(),
                profile: "Default".into()
            }
        );

        let empty = ConnectViewModel::project(
            ["mud"],
            Some("mud"),
            ProfileInventory::Ready(Vec::<ProfileSummary<'_>>::new()),
            Form::None,
        );
        let selected = empty.selected.unwrap();
        assert_eq!(
            selected.quick_connect,
            Some(Intent::Connect {
                server: "mud".into(),
                profile: "Default".into()
            })
        );
        assert!(selected.new_profile.is_none());
    }

    #[test]
    fn unknown_profile_inventory_never_offers_quick_connect() {
        for inventory in [
            ProfileInventory::<Vec<ProfileSummary<'_>>>::Loading,
            ProfileInventory::Unavailable,
        ] {
            let model = ConnectViewModel::project(["mud"], Some("mud"), inventory, Form::None);
            let selected = model.selected.unwrap();
            assert!(selected.quick_connect.is_none());
            assert!(selected.new_profile.is_none());
        }
    }

    #[test]
    fn stale_selection_is_not_rendered_and_profile_form_locks_rail() {
        let model = ConnectViewModel::project(
            ["a", "b"],
            Some("removed"),
            ProfileInventory::<Vec<ProfileSummary<'_>>>::Unavailable,
            Form::None,
        );
        assert_eq!(model.panel, Panel::Placeholder);
        assert!(model.selected.is_none());
        let editing = ConnectViewModel::project(
            ["a", "b"],
            Some("b"),
            ProfileInventory::<Vec<ProfileSummary<'_>>>::Unavailable,
            Form::Profile,
        );
        assert_eq!(editing.panel, Panel::ProfileForm);
        assert!(editing.servers.iter().all(|row| row.select.is_none()));
    }

    #[test]
    fn editing_a_form_does_not_project_hidden_profile_rows() {
        let profiles = std::iter::from_fn(|| -> Option<ProfileSummary<'static>> {
            panic!("hidden profile inventory must not be traversed")
        });
        let model = ConnectViewModel::project(
            ["mud"],
            Some("mud"),
            ProfileInventory::Ready(profiles),
            Form::Profile,
        );
        assert_eq!(model.panel, Panel::ProfileForm);
        assert!(model.selected.is_none());
    }
}
