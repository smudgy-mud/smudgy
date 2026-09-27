use smudgy_core::models::{
    profile::{
        DEFAULT_PROFILE_NAME, ProfileConfig, create_profile, ensure_default_profile, load_profile,
        update_profile,
    },
    server::{ServerConfig, create_server},
};

#[test]
fn quick_connect_uses_one_durable_default_without_overwriting_user_data() {
    let home = tempfile::tempdir().expect("temporary home");
    smudgy_core::set_smudgy_home(home.path());
    create_server("Mud", ServerConfig::new("localhost".into(), 4000)).expect("create server");

    let default = ensure_default_profile("Mud").expect("create Default");
    assert_eq!(default.name, DEFAULT_PROFILE_NAME);
    assert_eq!(default.config.send_on_connect, "");
    assert_eq!(
        default.path,
        home.path().join("Mud").join("profiles").join("Default")
    );

    update_profile(
        "Mud",
        DEFAULT_PROFILE_NAME,
        ProfileConfig {
            caption: "Shared setup".into(),
            send_on_connect: "look".into(),
        },
    )
    .expect("edit Default");
    assert_eq!(
        ensure_default_profile("Mud")
            .expect("reuse Default")
            .config
            .send_on_connect,
        "look"
    );

    create_profile(
        "Mud",
        "Named",
        ProfileConfig {
            caption: String::new(),
            send_on_connect: "say hi".into(),
        },
    )
    .expect("create a named profile");
    assert_eq!(
        load_profile("Mud", "Named")
            .expect("load named profile")
            .config
            .send_on_connect,
        "say hi"
    );
    assert_eq!(
        load_profile("Mud", DEFAULT_PROFILE_NAME)
            .expect("load Default")
            .config
            .send_on_connect,
        "look"
    );
}
