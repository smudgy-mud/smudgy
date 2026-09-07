use super::*;
// use iced::Command; // Ensure this line is removed or commented if present from previous edits
use smudgy_core::models::profile::ProfileConfig;
use smudgy_core::models::server::{ServerCas, ServerConfig};

// Helper to create a default state
fn initial_state() -> State {
    State::default()
}

fn profile(name: &str, caption: &str, send_on_connect: &str) -> Profile {
    Profile {
        name: name.to_string(),
        path: std::path::PathBuf::from(name),
        config: ProfileConfig {
            caption: caption.to_string(),
            send_on_connect: send_on_connect.to_string(),
        },
    }
}

fn server(name: &str) -> Server {
    Server {
        name: name.to_string(),
        path: std::path::PathBuf::from(name),
        config: ServerConfig::new("mud.example.com".to_string(), 4000),
    }
}

#[test]
fn test_initial_state_is_correct() {
    let state = initial_state();
    assert!(state.servers.is_empty());
    assert!(state.profiles.is_empty());
    assert!(state.selected_server.is_none());
    assert!(!state.is_loading_servers); // Should be false until a load is triggered
    assert!(state.is_loading_profiles.is_none());
    assert!(state.server_action.is_none());
    assert_eq!(state.server_form_data.name, "");
    assert_eq!(state.server_form_data.host, "");
    assert_eq!(state.server_form_data.port, "");
    assert!(state.server_crud_error.is_none());
    assert!(state.profile_action.is_none());
    assert_eq!(state.profile_form_data.name, "");
    assert_eq!(state.profile_form_data.description, "");
    assert_eq!(state.profile_form_send_on_connect_content.text(), "");
    assert!(state.profile_crud_error.is_none());
}

#[test]
fn test_request_create_server_updates_state() {
    let mut state = initial_state();
    let (_task, event) = update(&mut state, Message::RequestCreateServer);

    assert!(event.is_none());
    assert_eq!(state.server_action, Some(ServerCrudAction::Create));
    assert_eq!(state.server_form_data.name, "");
    assert!(state.server_crud_error.is_none());
    assert!(state.selected_server.is_none());
    assert!(state.is_loading_profiles.is_none());
}

#[test]
fn test_cancel_server_form_resets_state() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data.name = "Test".to_string();
    state.server_crud_error = Some("Error".to_string());

    let (_task, event) = update(&mut state, Message::CancelServerForm);

    assert!(event.is_none());
    assert!(state.server_action.is_none());
    assert_eq!(state.server_form_data.name, "");
    assert!(state.server_crud_error.is_none());
}

#[test]
fn server_form_toggles_mccp2_and_mccp4_independently() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    assert!(state.server_form_data.compression);
    assert!(state.server_form_data.mccp4_compression);

    let _ = update(&mut state, Message::ToggleServerMccp4Compression(false));
    assert!(state.server_form_data.compression);
    assert!(!state.server_form_data.mccp4_compression);

    let _ = update(&mut state, Message::ToggleServerCompression(false));
    assert!(!state.server_form_data.compression);
    assert!(!state.server_form_data.mccp4_compression);
}

#[test]
fn test_submit_server_form_create_valid() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "MyMUD".to_string(),
        host: "mud.example.com".to_string(),
        port: "4000".to_string(),
        ..ServerConfigFormData::default()
    };

    // The task is not asserted directly. Its effect is tested via its completion message.
    let (_task, event) = update(&mut state, Message::SubmitServerForm);

    assert!(event.is_none());
    assert!(state.server_crud_error.is_none());
}

#[test]
fn test_submit_server_form_create_invalid_port() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "MyMUD".to_string(),
        host: "mud.example.com".to_string(),
        port: "invalid_port".to_string(),
        ..ServerConfigFormData::default()
    };

    // The task is not asserted directly. No task should be spawned.
    let (_task, event) = update(&mut state, Message::SubmitServerForm);
    // Ensure user's assert!(task) is removed if it was here.
    assert!(event.is_none());
    assert!(state.server_crud_error.is_some());
    assert_eq!(
        state.server_crud_error.as_ref().unwrap(),
        "Invalid port number. Must be between 1 and 65535."
    );
}

#[test]
fn test_submit_server_form_create_empty_name() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "".to_string(),
        host: "mud.example.com".to_string(),
        port: "4000".to_string(),
        ..ServerConfigFormData::default()
    };

    let (_task, event) = update(&mut state, Message::SubmitServerForm);

    assert!(event.is_none());
    assert!(state.server_crud_error.is_some());
    assert_eq!(
        state.server_crud_error.as_ref().unwrap(),
        "Server name cannot be empty."
    );
}

#[test]
fn test_submit_server_form_rejects_untranslated_core_name_error_early() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "My MUD".to_string(),
        host: "mud.example.com".to_string(),
        port: "4000".to_string(),
        ..Default::default()
    };

    let (_task, event) = update(&mut state, Message::SubmitServerForm);

    assert!(event.is_none());
    assert_eq!(
        state.server_crud_error.as_deref(),
        Some("Server name may contain only letters, numbers, underscores, and hyphens.")
    );
}

#[test]
fn test_submit_profile_form_rejects_untranslated_core_name_error_early() {
    let mut state = initial_state();
    state.selected_server = Some("MyMUD".to_string());
    state.profile_action = Some(ProfileCrudAction::Create {
        server: server("MyMUD"),
    });
    state.profile_form_data.name = "My Character".to_string();

    let (_task, event) = update(&mut state, Message::SubmitProfileForm);

    assert!(event.is_none());
    assert_eq!(
        state.profile_crud_error.as_deref(),
        Some("Profile name may contain only letters, numbers, underscores, and hyphens.")
    );
}

#[test]
fn test_select_server_loads_profiles_if_not_present() {
    let mut state = initial_state();
    let server_name = "TestServer".to_string();

    state.servers.push(Server {
        name: server_name.clone(),
        config: ServerConfig::new("test.com".to_string(), 1234),
        path: std::path::PathBuf::new(),
    });

    let (_task, event) = update(&mut state, Message::SelectServer(server_name.clone()));

    assert!(event.is_none());
    assert_eq!(state.selected_server, Some(server_name.clone()));
    assert_eq!(state.is_loading_profiles, Some(server_name.clone()));
}

#[test]
fn test_select_server_does_not_load_profiles_if_present() {
    let mut state = initial_state();
    let server_name = "TestServer".to_string();

    state.servers.push(Server {
        name: server_name.clone(),
        config: ServerConfig::new("test.com".to_string(), 1234),
        path: std::path::PathBuf::new(),
    });
    state.profiles.insert(
        server_name.clone(),
        vec![profile("TestProfile", "Caption", "")],
    );

    let (_task, event) = update(&mut state, Message::SelectServer(server_name.clone()));

    assert!(event.is_none());
    assert_eq!(state.selected_server, Some(server_name.clone()));
    assert!(state.is_loading_profiles.is_none());
}

#[test]
fn test_profiles_loaded_success() {
    let mut state = initial_state();
    let server_name = "MyServer".to_string();
    let expected_server = server(&server_name);
    let profile1 = profile("Char1", "", "");
    state.servers.push(expected_server.clone());
    state.selected_server = Some(server_name.clone());
    state.is_loading_profiles = Some(server_name.clone());
    state.pending_profile_load = Some((1, expected_server.clone()));

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(
            1,
            expected_server,
            Ok(ServerCas::Applied(vec![profile1.clone()])),
        ),
    );

    assert!(event.is_none());
    assert!(state.is_loading_profiles.is_none());
    assert!(state.profiles.contains_key(&server_name));
    assert_eq!(state.profiles.get(&server_name).unwrap().len(), 1);
    assert_eq!(
        state.profiles.get(&server_name).unwrap()[0].name,
        profile1.name
    );
}

#[test]
fn stale_profile_load_for_another_server_is_ignored() {
    let mut state = initial_state();
    let server_name_loaded = "ServerLoaded".to_string();
    let server_name_currently_loading = "ServerCurrentlyLoading".to_string();
    let loaded_server = server(&server_name_loaded);
    let current_server = server(&server_name_currently_loading);
    let profile1 = profile("Char1", "", "");

    state.servers = vec![loaded_server.clone(), current_server.clone()];
    state.selected_server = Some(server_name_currently_loading.clone());
    state.is_loading_profiles = Some(server_name_currently_loading.clone());
    state.pending_profile_load = Some((2, current_server));

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(1, loaded_server, Ok(ServerCas::Applied(vec![profile1]))),
    );

    assert!(event.is_none());
    assert_eq!(
        state.is_loading_profiles,
        Some(server_name_currently_loading)
    );
    assert!(!state.profiles.contains_key(&server_name_loaded));
}

#[test]
fn test_profiles_loaded_error() {
    let mut state = initial_state();
    let server_name = "MyServer".to_string();
    let expected_server = server(&server_name);
    state.servers.push(expected_server.clone());
    state.selected_server = Some(server_name.clone());
    state.is_loading_profiles = Some(server_name.clone());
    state.pending_profile_load = Some((1, expected_server.clone()));
    let error_msg = "Failed to load profiles".to_string();

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(1, expected_server, Err(error_msg.clone())),
    );

    assert!(event.is_none());
    assert!(state.is_loading_profiles.is_none());
    assert!(!state.profiles.contains_key(&server_name));
}

#[test]
fn test_request_create_server_clears_open_profile_form() {
    // `+ New Server` is persistent, so it can be pressed while a profile form is
    // open; opening the server form must drop the profile form so it doesn't
    // resurface on cancel.
    let mut state = initial_state();
    state.selected_server = Some("S".to_string());
    state.profile_action = Some(ProfileCrudAction::Create {
        server: server("S"),
    });

    let (_task, event) = update(&mut state, Message::RequestCreateServer);

    assert!(event.is_none());
    assert_eq!(state.server_action, Some(ServerCrudAction::Create));
    assert!(state.profile_action.is_none());
}

#[test]
fn test_update_profile_form_description_field() {
    let mut state = initial_state();
    state.profile_action = Some(ProfileCrudAction::Create {
        server: server("S"),
    });

    let (_task, event) = update(
        &mut state,
        Message::UpdateProfileFormField(ProfileFormField::Description, "White Robe".to_string()),
    );

    assert!(event.is_none());
    assert_eq!(state.profile_form_data.description, "White Robe");
}

#[test]
fn test_server_created_chains_into_add_profile() {
    // "Save & add profile" (5.3): creating a server selects it and flows directly
    // into the Add-profile form for that server.
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    let new_server = Server {
        name: "ArcticMud".to_string(),
        config: ServerConfig::new("mud.arctic.org".to_string(), 2700),
        path: std::path::PathBuf::new(),
    };
    let pending = ServerOperationEnvelope {
        id: 1,
        action: ServerOperationAction::Create {
            target_name: new_server.name.clone(),
            config: new_server.config.clone(),
        },
        form_revision: state.server_form_revision,
    };
    state.pending_server_operation = Some(pending.clone());

    let (_task, event) = update(
        &mut state,
        Message::ServerOperationFinished(ServerOperationCompletion {
            key: pending.key(),
            result: Ok(AppliedServerOperation::Created(new_server.clone())),
        }),
    );

    assert!(event.is_none());
    assert!(state.server_action.is_none());
    assert_eq!(state.selected_server, Some(new_server.name.clone()));
    assert_eq!(
        state.profile_action,
        Some(ProfileCrudAction::Create {
            server: new_server.clone()
        })
    );
    assert_eq!(state.profile_form_data.name, "");
    assert!(state.servers.iter().any(|s| s.name == new_server.name));
}

#[test]
fn server_submit_captures_the_exact_snapshot_and_hidden_config() {
    let mut state = initial_state();
    let mut expected = server("Arctic");
    expected.config.trusted_link_hosts = vec!["forum.example".to_string()];
    expected.config.trust_all_links = true;
    state.servers.push(expected.clone());
    state.selected_server = Some(expected.name.clone());

    let _ = update(
        &mut state,
        Message::RequestEditServer(expected.name.clone()),
    );
    let _ = update(
        &mut state,
        Message::UpdateServerFormField(ServerFormField::Host, "new.example".to_string()),
    );
    let _ = update(&mut state, Message::SubmitServerForm);

    let pending = state
        .pending_server_operation
        .as_ref()
        .expect("one server save is pending");
    assert!(matches!(
        &pending.action,
        ServerOperationAction::Update {
            expected: captured,
            config,
        } if captured == &expected
            && config.host == "new.example"
            && config.trusted_link_hosts == vec!["forum.example"]
            && config.trust_all_links
    ));

    let immutable = pending.clone();
    let _ = update(
        &mut state,
        Message::UpdateServerFormField(ServerFormField::Host, "later.example".to_string()),
    );
    assert_eq!(state.pending_server_operation.as_ref(), Some(&immutable));
}

#[test]
fn server_completion_updates_only_its_original_snapshot_and_not_a_new_form() {
    let mut state = initial_state();
    let expected = server("Arctic");
    let other = server("Discworld");
    state.servers = vec![expected.clone(), other.clone()];
    state.selected_server = Some(expected.name.clone());

    let _ = update(
        &mut state,
        Message::RequestEditServer(expected.name.clone()),
    );
    let _ = update(
        &mut state,
        Message::UpdateServerFormField(ServerFormField::Host, "saved.example".to_string()),
    );
    let _ = update(&mut state, Message::SubmitServerForm);
    let pending = state.pending_server_operation.clone().unwrap();
    let saved_config = match &pending.action {
        ServerOperationAction::Update { config, .. } => config.clone(),
        _ => panic!("expected an update"),
    };
    let updated = Server {
        config: saved_config,
        ..expected.clone()
    };

    let _ = update(&mut state, Message::SelectServer(other.name.clone()));
    let _ = update(&mut state, Message::RequestEditServer(other.name.clone()));
    let _ = update(
        &mut state,
        Message::UpdateServerFormField(ServerFormField::Host, "draft.example".to_string()),
    );
    let newer_revision = state.server_form_revision;

    let _ = update(
        &mut state,
        Message::ServerOperationFinished(ServerOperationCompletion {
            key: pending.key(),
            result: Ok(AppliedServerOperation::Updated(updated.clone())),
        }),
    );

    assert_eq!(state.selected_server.as_deref(), Some("Discworld"));
    assert_eq!(state.server_form_revision, newer_revision);
    assert_eq!(state.server_form_data.host, "draft.example");
    assert!(matches!(
        &state.server_action,
        Some(ServerCrudAction::Edit(server)) if server == &other
    ));
    assert!(state.servers.contains(&updated));
    assert!(state.servers.contains(&other));
}

#[test]
fn server_double_submit_and_out_of_order_completion_are_ignored() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "Arctic".to_string(),
        host: "mud.example".to_string(),
        port: "4000".to_string(),
        ..ServerConfigFormData::default()
    };
    let _ = update(&mut state, Message::SubmitServerForm);
    let pending = state.pending_server_operation.clone().unwrap();
    let operation_sequence = state.server_operation_sequence;

    let _ = update(&mut state, Message::SubmitServerForm);
    assert_eq!(state.server_operation_sequence, operation_sequence);
    assert_eq!(state.pending_server_operation.as_ref(), Some(&pending));

    let created = server("Arctic");
    let mut stale_key = pending.key();
    stale_key.id = stale_key.id.wrapping_sub(1);
    let _ = update(
        &mut state,
        Message::ServerOperationFinished(ServerOperationCompletion {
            key: stale_key,
            result: Ok(AppliedServerOperation::Created(created.clone())),
        }),
    );
    assert_eq!(state.pending_server_operation.as_ref(), Some(&pending));
    assert!(state.servers.is_empty());

    let completion = ServerOperationCompletion {
        key: pending.key(),
        result: Ok(AppliedServerOperation::Created(created.clone())),
    };
    let _ = update(
        &mut state,
        Message::ServerOperationFinished(completion.clone()),
    );
    let _ = update(&mut state, Message::ServerOperationFinished(completion));
    assert!(state.pending_server_operation.is_none());
    assert_eq!(state.servers, vec![created]);
}

#[test]
fn restore_last_session_emits_its_event_with_the_connect_housekeeping() {
    // The affordance's click is a plain message -> event hand-off; the
    // daemon owns the actual restore. Any half-open server form drops,
    // exactly as Connect/Offline do.
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data.name = "half-typed".to_string();

    let (_task, event) = update(
        &mut state,
        Message::RestoreLastSession("Arctic".to_string()),
    );

    assert_eq!(event, Some(Event::RestoreLastSession("Arctic".to_string())));
    assert!(state.server_action.is_none());
    assert_eq!(state.server_form_data.name, "");
}

#[test]
fn the_restore_affordance_derives_from_the_probed_snapshot() {
    // Headless view-data check: the detail pane offers a restore exactly
    // when the probe stored profile names, composing the label from them
    // in slot order.
    let mut state = initial_state();
    state.last_sessions.insert("Arctic".to_string(), None);
    assert!(
        state
            .last_sessions
            .get("Arctic")
            .and_then(|probed| probed.as_deref())
            .and_then(crate::workspace::last_session::summary)
            .is_none(),
        "a probed server without a snapshot offers nothing"
    );

    state.last_sessions.insert(
        "Arctic".to_string(),
        Some(vec!["Kapusnik".to_string(), "Kapusta".to_string()]),
    );
    assert_eq!(
        state
            .last_sessions
            .get("Arctic")
            .and_then(|probed| probed.as_deref())
            .and_then(crate::workspace::last_session::summary),
        Some("Kapusnik, Kapusta".to_string())
    );
}

#[test]
fn profile_submit_captures_an_immutable_redacted_operation_envelope() {
    let mut state = initial_state();
    let server = server("Arctic");
    let expected = profile("Gandalf", "old", "connect old");
    state.selected_server = Some(server.name.clone());
    state.servers.push(server.clone());
    state
        .profiles
        .insert(server.name.clone(), vec![expected.clone()]);
    state.profile_action = Some(ProfileCrudAction::Edit {
        server: server.clone(),
        expected: expected.clone(),
    });
    state.profile_form_data.description = "captured".to_string();
    state.profile_form_send_on_connect_content =
        text_editor::Content::with_text("connect Gandalf $PASSWORD");
    state.profile_form_password = "do-not-log-me".to_string();

    let _ = update(&mut state, Message::SubmitProfileForm);

    let pending = state
        .pending_profile_operation
        .as_ref()
        .expect("one operation is pending");
    assert_eq!(pending.server, server);
    assert!(matches!(
        &pending.action,
        ProfileOperationAction::Update {
            expected: captured,
            target_name,
            config,
        } if captured == &expected
            && target_name == "Gandalf"
            && config.caption == "captured"
            && config.send_on_connect == "connect Gandalf $PASSWORD"
    ));
    assert!(matches!(
        &pending.password,
        ProfilePasswordAction::Set(value) if value == "do-not-log-me"
    ));
    assert!(!format!("{pending:?}").contains("do-not-log-me"));

    let original = pending.clone();
    let _ = update(
        &mut state,
        Message::UpdateProfileFormField(ProfileFormField::Description, "later edit".to_string()),
    );
    let password_edit = Message::UpdateProfileFormPassword("later secret".to_string().into());
    assert!(!format!("{password_edit:?}").contains("later secret"));
    let _ = update(&mut state, password_edit);
    assert_eq!(state.pending_profile_operation.as_ref(), Some(&original));
}

#[test]
fn profile_completion_uses_its_original_server_without_clobbering_a_new_form() {
    let mut state = initial_state();
    let server_a = server("Arctic");
    let server_b = server("Discworld");
    let expected = profile("Gandalf", "old", "connect old");
    state.servers = vec![server_a.clone(), server_b.clone()];
    state.selected_server = Some(server_a.name.clone());
    state
        .profiles
        .insert(server_a.name.clone(), vec![expected.clone()]);
    state.profiles.insert(server_b.name.clone(), Vec::new());

    let _ = update(
        &mut state,
        Message::RequestEditProfile(expected.name.clone()),
    );
    let _ = update(
        &mut state,
        Message::UpdateProfileFormField(
            ProfileFormField::Description,
            "saved on Arctic".to_string(),
        ),
    );
    let _ = update(&mut state, Message::SubmitProfileForm);
    let pending = state.pending_profile_operation.clone().unwrap();
    let updated = Profile {
        config: ProfileConfig {
            caption: "saved on Arctic".to_string(),
            send_on_connect: expected.config.send_on_connect.clone(),
        },
        ..expected.clone()
    };

    let _ = update(&mut state, Message::SelectServer(server_b.name.clone()));
    let _ = update(&mut state, Message::RequestCreateProfile);
    let _ = update(
        &mut state,
        Message::UpdateProfileFormField(ProfileFormField::Name, "Rincewind".to_string()),
    );
    let _ = update(
        &mut state,
        Message::UpdateProfileFormPassword("new-form-secret".to_string().into()),
    );
    let newer_revision = state.profile_form_revision;

    let _ = update(
        &mut state,
        Message::ProfileOperationFinished(ProfileOperationCompletion {
            key: pending.key(),
            result: Ok(AppliedProfileOperation::Updated(updated.clone(), None)),
        }),
    );

    assert_eq!(state.selected_server.as_deref(), Some("Discworld"));
    assert_eq!(state.profile_form_revision, newer_revision);
    assert_eq!(state.profile_form_data.name, "Rincewind");
    assert_eq!(state.profile_form_password, "new-form-secret");
    assert!(matches!(
        &state.profile_action,
        Some(ProfileCrudAction::Create { server }) if server == &server_b
    ));
    assert_eq!(state.profiles["Arctic"], vec![updated]);
    assert!(state.profiles["Discworld"].is_empty());
}

#[test]
fn committed_profile_stays_editable_and_reports_a_password_failure() {
    let mut state = initial_state();
    let server = server("Arctic");
    let expected = profile("Gandalf", "old", "connect old");
    state.servers.push(server.clone());
    state.selected_server = Some(server.name.clone());
    state
        .profiles
        .insert(server.name.clone(), vec![expected.clone()]);
    state.profile_action = Some(ProfileCrudAction::Edit {
        server: server.clone(),
        expected: expected.clone(),
    });
    state.profile_form_data.description = "saved".to_string();
    state.profile_form_send_on_connect_content =
        text_editor::Content::with_text("connect Gandalf $PASSWORD");
    state.profile_form_password = "retry-me".to_string();
    let _ = update(&mut state, Message::SubmitProfileForm);
    let pending = state.pending_profile_operation.clone().unwrap();
    let updated = Profile {
        config: ProfileConfig {
            caption: "saved".to_string(),
            send_on_connect: "connect Gandalf $PASSWORD".to_string(),
        },
        ..expected
    };

    let _ = update(
        &mut state,
        Message::ProfileOperationFinished(ProfileOperationCompletion {
            key: pending.key(),
            result: Ok(AppliedProfileOperation::Updated(
                updated.clone(),
                Some(ProfilePasswordWarning::Failed(
                    "credential store unavailable".to_string(),
                )),
            )),
        }),
    );

    assert_eq!(state.profiles["Arctic"], vec![updated.clone()]);
    assert!(matches!(
        &state.profile_action,
        Some(ProfileCrudAction::Edit { server: owner, expected })
            if owner == &server && expected == &updated
    ));
    assert_eq!(state.profile_form_password, "retry-me");
    let warning = state.profile_crud_error.as_deref().unwrap_or_default();
    assert!(warning.contains("Arctic / Gandalf"));
    assert!(warning.contains("credential store unavailable"));
}

#[test]
fn profile_double_submit_and_out_of_order_completion_are_ignored() {
    let mut state = initial_state();
    let server = server("Arctic");
    state.servers.push(server.clone());
    state.selected_server = Some(server.name.clone());
    state.profiles.insert(server.name.clone(), Vec::new());
    let _ = update(&mut state, Message::RequestCreateProfile);
    let _ = update(
        &mut state,
        Message::UpdateProfileFormField(ProfileFormField::Name, "Gandalf".to_string()),
    );
    let _ = update(&mut state, Message::SubmitProfileForm);
    let pending = state.pending_profile_operation.clone().unwrap();
    let operation_sequence = state.profile_operation_sequence;

    let _ = update(&mut state, Message::SubmitProfileForm);
    assert_eq!(state.profile_operation_sequence, operation_sequence);
    assert_eq!(state.pending_profile_operation.as_ref(), Some(&pending));

    let created = profile("Gandalf", "", "");
    let mut stale_key = pending.key();
    stale_key.id = stale_key.id.wrapping_sub(1);
    let _ = update(
        &mut state,
        Message::ProfileOperationFinished(ProfileOperationCompletion {
            key: stale_key,
            result: Ok(AppliedProfileOperation::Created(created.clone(), None)),
        }),
    );
    assert_eq!(state.pending_profile_operation.as_ref(), Some(&pending));
    assert!(state.profiles["Arctic"].is_empty());

    let completion = ProfileOperationCompletion {
        key: pending.key(),
        result: Ok(AppliedProfileOperation::Created(created.clone(), None)),
    };
    let _ = update(
        &mut state,
        Message::ProfileOperationFinished(completion.clone()),
    );
    let _ = update(&mut state, Message::ProfileOperationFinished(completion));
    assert!(state.pending_profile_operation.is_none());
    assert_eq!(state.profiles["Arctic"], vec![created]);
}

#[test]
fn compare_and_swap_conflicts_are_reported_in_translated_text() {
    let mut state = initial_state();
    let server = server("Arctic");
    let expected = profile("Gandalf", "old", "connect old");
    state.servers.push(server.clone());
    state.selected_server = Some(server.name.clone());
    state
        .profiles
        .insert(server.name.clone(), vec![expected.clone()]);
    let _ = update(
        &mut state,
        Message::RequestEditProfile(expected.name.clone()),
    );
    let _ = update(
        &mut state,
        Message::UpdateProfileFormField(ProfileFormField::Description, "renamed".to_string()),
    );
    let _ = update(&mut state, Message::SubmitProfileForm);
    let pending = state.pending_profile_operation.clone().unwrap();

    let _ = update(
        &mut state,
        Message::ProfileOperationFinished(ProfileOperationCompletion {
            key: pending.key(),
            result: Err(ProfileOperationError::StateChanged),
        }),
    );

    // The conflict text is a catalog entry wrapped by the per-action catalog entry.
    let inner = t!("profile-error-state-changed");
    assert_eq!(
        state.profile_crud_error.as_deref(),
        Some(t!("profile-error-update", "error" => &inner).as_str())
    );
    assert_eq!(
        state.profile_crud_error.as_deref(),
        Some(
            "Failed to update profile: The profile changed before the save could finish. Review it and try again."
        )
    );
    assert!(state.pending_profile_operation.is_none());

    // The server form wraps its own conflict the same way.
    state.server_action = Some(ServerCrudAction::Edit(server.clone()));
    state.server_form_data = ServerConfigFormData {
        name: server.name.clone(),
        host: "mud.example.com".to_string(),
        port: "4000".to_string(),
        ..Default::default()
    };
    let _ = update(&mut state, Message::SubmitServerForm);
    let pending = state.pending_server_operation.clone().unwrap();
    let _ = update(
        &mut state,
        Message::ServerOperationFinished(ServerOperationCompletion {
            key: pending.key(),
            result: Err(ServerOperationError::StateChanged),
        }),
    );
    assert_eq!(
        state.server_crud_error.as_deref(),
        Some(
            "Failed to update server: The server changed before the save finished. Reload it and try again."
        )
    );
}

#[test]
fn opening_prefers_recent_server_and_falls_back_if_it_was_removed() {
    let servers = [server("Alpha"), server("Zulu")];
    assert_eq!(initial_server(&servers, Some("Zulu")).unwrap().name, "Zulu");
    assert_eq!(
        initial_server(&servers, Some("Removed")).unwrap().name,
        "Alpha"
    );
    assert_eq!(initial_server(&servers, None).unwrap().name, "Alpha");
    assert!(initial_server(&[], Some("Zulu")).is_none());
}
