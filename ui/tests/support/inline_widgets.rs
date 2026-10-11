//! Exercise real JSX, native widget construction, ordered echo and trigger edits together.
use iced::{Rectangle, advanced::widget::Operation};
pub struct Labels(pub std::collections::BTreeMap<String, Rectangle>);
impl Operation for Labels {
    fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
        f(self);
    }
    fn text(&mut self, _: Option<&iced::widget::Id>, bounds: Rectangle, text: &str) {
        self.0.insert(text.into(), bounds);
    }
}
use smudgy_core::session::{SessionId, SessionParams};
use std::sync::Arc;

pub static SESSION_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
pub async fn join_fixture_runtime() {
    let outcome = tokio::task::spawn_blocking(|| {
        smudgy_core::session::runtime::join_runtime_thread(SessionId::from(7915))
    })
    .await
    .unwrap();
    assert!(outcome.is_clean(), "fixture runtime shutdown: {outcome:?}");
}

pub fn session_fixture(
    script: &str,
    extra_modules: &[(&str, &str)],
) -> (
    &'static tempfile::TempDir,
    smudgy_widgets::WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>,
    Arc<SessionParams>,
) {
    // The application accepts its data-home override once per process. Keep the same
    // temporary home alive across these serialized scenarios rather than resetting it.
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        smudgy_core::set_smudgy_home(home.path());
        home
    });
    let server = "InlineWidgets";
    let base = smudgy_core::get_smudgy_home().unwrap().join(server);
    std::fs::create_dir_all(base.join("modules")).unwrap();
    std::fs::create_dir_all(base.join("logs")).unwrap();
    std::fs::write(base.join("modules/inline.tsx"), script).unwrap();
    for (name, source) in extra_modules {
        std::fs::write(base.join("modules").join(name), source).unwrap();
    }
    std::fs::write(base.join("modules/copy.wgsl"), "struct Parameters { strength: f32 } @group(1) @binding(0) var<uniform> params: Parameters; fn effect(p: vec2f) -> vec4f { return sampleText(p) * params.strength; }").unwrap();
    std::fs::write(
        base.join("modules/bad.wgsl"),
        "fn effect(p: vec2f) -> vec4f { return missing; }",
    )
    .unwrap();
    let root = smudgy_widgets::WidgetRoot::<'static, smudgy_theme::Theme, iced::Renderer>::new();
    let factory_root = root.clone();
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7915),
        server_name: Arc::new(server.into()),
        profile_name: Arc::new("Test".into()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(move || {
            vec![smudgy_widgets::ext::init(factory_root.clone(), None, None)]
        }),
        on_engine_rebuild: None,
    });
    (home, root, params)
}

pub fn native_renderer() -> iced::Renderer {
    iced::advanced::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(smudgy_ui_shared::assets::GEIST_MONO_BYTES.into());
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        smudgy_ui_shared::assets::GEIST_MONO,
        iced::Pixels(16.0),
    ))
}
