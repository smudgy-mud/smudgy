//! Exercise real JSX, native widget construction, ordered echo and trigger edits together.
use futures::StreamExt;
use iced::{
    Event, Rectangle, Size,
    advanced::{Layout, Shell, clipboard, layout, mouse, widget::Tree},
};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionParams, spawn_with_package_provider,
};
use smudgy_session_model::StyledLine;
use std::{sync::Arc, time::Duration};

#[path = "../../ui/tests/support/inline_widgets.rs"]
mod support;
use support::*;
const SCRIPT: &str = include_str!("native/content.tsx");
#[path = "native/content.rs"]
mod content;
#[path = "native/imports.rs"]
mod imports;
#[path = "native/reload.rs"]
mod reload;
fn effects_package_provider() -> smudgy_core::session::PackageProviderFactory {
    effects_package_provider_with_entry(None, true)
}

fn effects_package_provider_with_entry(
    entry: Option<&str>,
    shader_permission: bool,
) -> smudgy_core::session::PackageProviderFactory {
    use smudgy_script::{
        InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, ResolvedPackage,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects");
    let mut package = ResolvedPackage {
        key: PackageKey {
            owner: "official".into(),
            name: "text-effects".into(),
        },
        resolved_version: "0.1.0".into(),
        manifest: PackageManifest::parse(
            &std::fs::read_to_string(root.join("smudgy.package.json")).unwrap(),
        )
        .unwrap(),
        integrity: "test-text-effects".into(),
        modules: {
            fn collect(
                root: &std::path::Path,
                directory: &std::path::Path,
                modules: &mut Vec<PackageModuleSource>,
            ) {
                for entry in std::fs::read_dir(directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        collect(root, &path, modules);
                    } else if path
                        .extension()
                        .is_some_and(|ext| matches!(ext.to_str(), Some("ts" | "tsx" | "wgsl")))
                    {
                        modules.push(PackageModuleSource {
                            subpath: path
                                .strip_prefix(root)
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .replace('\\', "/"),
                            text: std::fs::read_to_string(path).unwrap(),
                        });
                    }
                }
            }
            let mut modules = Vec::new();
            collect(&root, &root, &mut modules);
            modules.sort_by(|a, b| a.subpath.cmp(&b.subpath));
            modules
        },
    };
    if let Some(source) = entry {
        let original = package
            .modules
            .iter()
            .find(|module| module.subpath == "index.tsx")
            .unwrap()
            .text
            .clone();
        package.modules.push(PackageModuleSource {
            subpath: "original-index.tsx".into(),
            text: original,
        });
        package
            .modules
            .iter_mut()
            .find(|module| module.subpath == "index.tsx")
            .unwrap()
            .text = source.into();
    }
    package.manifest.permissions.smudgy.widget_shaders = shader_permission;
    if !shader_permission {
        package.manifest.permissions.smudgy = smudgy_script::SmudgyCapabilities {
            widgets: true,
            echo: true,
            ..Default::default()
        };
    }
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(package.clone());
        std::rc::Rc::new(provider)
    })
}

fn session_fixture() -> (
    &'static tempfile::TempDir,
    smudgy_widgets::WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>,
    Arc<SessionParams>,
) {
    support::session_fixture(
        SCRIPT,
        &[
            (
                "effect-burst.ts",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../examples/text-effects/effect-burst.ts"
                )),
            ),
            (
                "mud-effects.tsx",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../examples/text-effects/mud-effects.tsx"
                )),
            ),
            ("showcase.tsx", "import '@text-effects';"),
        ],
    )
}
/// Native shader IDs reveal compilation across real package imports, without a mock loader.
fn click_playground_font(
    root: &smudgy_widgets::WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>,
) -> Vec<smudgy_widgets::WidgetMessage> {
    let renderer = native_renderer();
    let mut view = root.view(
        |_| true,
        || Box::new(|_| iced::widget::container::Style::default()),
    );
    let mut tree = Tree::new(&view);
    let size = Size::new(520.0, 1000.0);
    let node =
        view.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let mut labels = Labels(std::collections::BTreeMap::default());
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    assert!(labels.0.contains_key("Text effects playground"));
    assert!(
        labels
            .0
            .keys()
            .any(|text| text.contains("@text-effects/fire"))
    );
    assert!(labels.0.keys().any(|text| text.contains("Use /load")));
    assert!(
        labels
            .0
            .keys()
            .any(|text| text.contains("smudgy:@text-effects"))
    );
    for name in effect_component_names() {
        assert!(
            labels.0.contains_key(&name),
            "missing playground button: {name}"
        );
    }
    let point = labels
        .0
        .get("Font: 24")
        .expect("bound font control")
        .center();
    let viewport = Rectangle::with_size(size);
    assert!(viewport.contains(point));
    let mut messages = Vec::new();
    for event in [
        Event::Mouse(mouse::Event::CursorMoved { position: point }),
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        view.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
    }
    assert!(messages.iter().any(|message| matches!(
        message,
        smudgy_widgets::WidgetMessage::InvokeCallback { .. }
    )));
    messages
}

fn effect_component_names() -> std::collections::BTreeSet<String> {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects");
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .filter(|name| !name.starts_with('_') && !["all", "load", "types"].contains(&name.as_str()))
        .collect()
}
