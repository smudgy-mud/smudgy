//! Host shader permissions exercised through a minimal installed package.
use super::*;
#[tokio::test]
async fn sandboxed_widget_creation_does_not_authorize_shader_imports_or_raw_compilation() {
    use smudgy_core::models::shared_packages::{self, UpdateMode};
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (home, _root, params) = session_fixture();
    let modules = home.path().join("InlineWidgets/modules");
    std::fs::write(modules.join("inline.tsx"), "").unwrap();
    let specifier = "smudgy://fixture/shaders";
    shared_packages::install_package("InlineWidgets", specifier, UpdateMode::Auto, true).unwrap();
    let mut permissions = smudgy_script::PackagePermissions::default();
    permissions.smudgy.widgets = true;
    permissions.smudgy.echo = true;
    shared_packages::record_consent("InlineWidgets", specifier, &permissions).unwrap();
    let entry = r#"
import {echo} from "smudgy:core";
import {Button} from "smudgy:widgets";
for (const attempt of [
    () => import("./copy.wgsl"),
    () => import("./component.ts"),
    () => globalThis.__smudgy_compile_text_shader("denied.wgsl", "invalid WGSL"),
]) {
    try { await attempt(); }
    catch (error) {
        if (String(error).includes("widgets:shaders")) { echo("SHADER_PERMISSION_DENIED"); continue; }
        throw error;
    }
    throw Error("unconsented shaders must reject");
}
echo(<Button>Ordinary widgets still work</Button>);
echo("SHADER_GATE_READY");
"#;
    let mut events = Box::pin(spawn_with_package_provider(params, sandbox_provider(entry)));
    let mut sender = None;
    let mut lines = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    'collect: loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap_or_else(|_| panic!("shader permission transcript: {lines:?}"))
            .expect("session closed");
        match event.event {
            SessionEvent::RuntimeReady(tx) => sender = Some(tx),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                        if line.text == "SHADER_GATE_READY" {
                            break 'collect;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        lines
            .iter()
            .filter(|text| *text == "SHADER_PERMISSION_DENIED")
            .count(),
        3
    );
    assert!(
        lines
            .iter()
            .any(|text| text == "Ordinary widgets still work")
    );
    sender.unwrap().send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    join_fixture_runtime().await;
    shared_packages::uninstall_package("InlineWidgets", specifier).unwrap();
}

fn sandbox_provider(entry: &str) -> smudgy_core::session::PackageProviderFactory {
    use smudgy_script::{
        InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, ResolvedPackage,
    };
    let package = ResolvedPackage {
        key: PackageKey {owner:"fixture".into(),name:"shaders".into()},
        resolved_version:"0.1.0".into(), integrity:"test-fixture".into(),
        manifest:PackageManifest::parse(r#"{"version":"0.1.0","entry":"index.tsx","importable":true,"permissions":{"smudgy":{"widgets":["create"],"session":["echo"]}}}"#).unwrap(),
        modules:vec![
            PackageModuleSource {subpath:"index.tsx".into(),text:entry.into()},
            PackageModuleSource {subpath:"copy.wgsl".into(),text:"fn effect(p:vec2f)->vec4f{return sampleText(p);}".into()},
            PackageModuleSource {subpath:"component.ts".into(),text:"import shader from './copy.wgsl'; export default shader;".into()},
        ],
    };
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(package.clone());
        std::rc::Rc::new(provider)
    })
}
