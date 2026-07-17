use super::*;
// use iced::Command; // Ensure this line is removed or commented if present from previous edits
use smudgy_core::models::profile::ProfileConfig;
use smudgy_core::models::server::{ServerConfig, ServerEncoding};

// Helper to create a default state
fn initial_state() -> State {
    State::default()
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
    assert_eq!(state.server_form_data.encoding, ServerEncoding::Utf8);
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
fn test_update_server_encoding_changes_active_form() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);

    let (_task, event) = update(
        &mut state,
        Message::UpdateServerEncoding(ServerEncoding::Big5),
    );

    assert!(event.is_none());
    assert_eq!(state.server_form_data.encoding, ServerEncoding::Big5);
}

#[test]
fn test_submit_server_form_create_valid() {
    let mut state = initial_state();
    state.server_action = Some(ServerCrudAction::Create);
    state.server_form_data = ServerConfigFormData {
        name: "MyMUD".to_string(),
        host: "mud.example.com".to_string(),
        port: "4000".to_string(),
        ..Default::default()
    };

    // The task is not asserted directly. Its effect is tested via Message::ServerCreated.
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
        ..Default::default()
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
        ..Default::default()
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
        vec![Profile {
            name: "TestProfile".to_string(),
            config: ProfileConfig {
                caption: "Caption".to_string(),
                send_on_connect: "".to_string(),
            },
            path: std::path::PathBuf::new(),
        }],
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
    let profile1 = Profile {
        name: "Char1".to_string(),
        config: ProfileConfig {
            caption: "".to_string(),
            send_on_connect: "".to_string(),
        },
        path: std::path::PathBuf::new(),
    };
    state.selected_server = Some(server_name.clone());
    state.is_loading_profiles = Some(server_name.clone());

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(server_name.clone(), Ok(vec![profile1.clone()])),
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
fn test_profiles_loaded_success_for_non_current_loading_server() {
    let mut state = initial_state();
    let server_name_loaded = "ServerLoaded".to_string();
    let server_name_currently_loading = "ServerCurrentlyLoading".to_string();
    let profile1 = Profile {
        name: "Char1".to_string(),
        config: ProfileConfig {
            caption: "".to_string(),
            send_on_connect: "".to_string(),
        },
        path: std::path::PathBuf::new(),
    };

    state.selected_server = Some(server_name_currently_loading.clone());
    state.is_loading_profiles = Some(server_name_currently_loading.clone());

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(server_name_loaded.clone(), Ok(vec![profile1.clone()])),
    );

    assert!(event.is_none());
    assert_eq!(
        state.is_loading_profiles,
        Some(server_name_currently_loading)
    );
    assert!(state.profiles.contains_key(&server_name_loaded));
    assert_eq!(state.profiles.get(&server_name_loaded).unwrap().len(), 1);
}

#[test]
fn test_profiles_loaded_error() {
    let mut state = initial_state();
    let server_name = "MyServer".to_string();
    state.selected_server = Some(server_name.clone());
    state.is_loading_profiles = Some(server_name.clone());
    let error_msg = "Failed to load profiles".to_string();

    let (_task, event) = update(
        &mut state,
        Message::ProfilesLoaded(server_name.clone(), Err(error_msg.clone())),
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
    state.profile_action = Some(ProfileCrudAction::Create);

    let (_task, event) = update(&mut state, Message::RequestCreateServer);

    assert!(event.is_none());
    assert_eq!(state.server_action, Some(ServerCrudAction::Create));
    assert!(state.profile_action.is_none());
}

#[test]
fn test_update_profile_form_description_field() {
    let mut state = initial_state();
    state.profile_action = Some(ProfileCrudAction::Create);

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

    let (_task, event) = update(&mut state, Message::ServerCreated(Ok(new_server.clone())));

    assert!(event.is_none());
    assert!(state.server_action.is_none());
    assert_eq!(state.selected_server, Some(new_server.name.clone()));
    assert_eq!(state.profile_action, Some(ProfileCrudAction::Create));
    assert_eq!(state.profile_form_data.name, "");
    assert!(state.servers.iter().any(|s| s.name == new_server.name));
}
