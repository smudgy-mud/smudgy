//! End-to-end regression for the bundled tintin-emulator SPEEDWALK alias.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackageProvider,
    ResolvedPackage,
};

const SPEC: &str = "smudgy://smudgy-mud/tintin-emulator";

fn module(subpath: &str, text: &str) -> PackageModuleSource {
    PackageModuleSource {
        subpath: subpath.to_string(),
        text: text.to_string(),
    }
}

fn tintin_package() -> ResolvedPackage {
    let manifest = PackageManifest::parse(include_str!(
        "../../packages/tintin-emulator/smudgy.package.json"
    ))
    .expect("valid tintin-emulator manifest");
    ResolvedPackage {
        key: PackageKey {
            owner: "smudgy-mud".to_string(),
            name: "tintin-emulator".to_string(),
        },
        resolved_version: manifest.version.clone(),
        manifest,
        integrity: "test-tintin-emulator".to_string(),
        modules: vec![
            module(
                "index.ts",
                include_str!("../../packages/tintin-emulator/index.ts"),
            ),
            module(
                "engine/eval.ts",
                include_str!("../../packages/tintin-emulator/engine/eval.ts"),
            ),
            module(
                "engine/format.ts",
                include_str!("../../packages/tintin-emulator/engine/format.ts"),
            ),
            module(
                "engine/keys.ts",
                include_str!("../../packages/tintin-emulator/engine/keys.ts"),
            ),
            module(
                "engine/parse.ts",
                include_str!("../../packages/tintin-emulator/engine/parse.ts"),
            ),
            module(
                "engine/path.ts",
                include_str!("../../packages/tintin-emulator/engine/path.ts"),
            ),
            module(
                "engine/pattern.ts",
                include_str!("../../packages/tintin-emulator/engine/pattern.ts"),
            ),
            module(
                "engine/text.ts",
                include_str!("../../packages/tintin-emulator/engine/text.ts"),
            ),
            module(
                "runtime/definitions.ts",
                include_str!("../../packages/tintin-emulator/runtime/definitions.ts"),
            ),
            module(
                "runtime/dispatcher.ts",
                include_str!("../../packages/tintin-emulator/runtime/dispatcher.ts"),
            ),
            module(
                "runtime/env.ts",
                include_str!("../../packages/tintin-emulator/runtime/env.ts"),
            ),
            module(
                "runtime/files.ts",
                include_str!("../../packages/tintin-emulator/runtime/files.ts"),
            ),
            module(
                "runtime/output.ts",
                include_str!("../../packages/tintin-emulator/runtime/output.ts"),
            ),
            module(
                "runtime/panes.ts",
                include_str!("../../packages/tintin-emulator/runtime/panes.ts"),
            ),
            module(
                "runtime/paths.ts",
                include_str!("../../packages/tintin-emulator/runtime/paths.ts"),
            ),
        ],
    }
}

fn factory_for(package: ResolvedPackage) -> PackageProviderFactory {
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(package.clone());
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

#[tokio::test]
async fn speedwalk_direct_send_burst_does_not_stall_the_runtime() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server = "TintinEmulatorSpeedwalk";
    std::fs::create_dir_all(home.join(server).join("modules")).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();

    let package = tintin_package();
    shared_packages::install_package(server, SPEC, UpdateMode::Auto, true).unwrap();
    shared_packages::record_consent(server, SPEC, &package.manifest.permissions).unwrap();
    shared_packages::save_param_value(server, SPEC, "commandChar", serde_json::json!("#")).unwrap();
    shared_packages::save_param_value(server, SPEC, "speedwalk", serde_json::json!(true)).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9911),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn_with_package_provider(params, factory_for(package)));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("runtime ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    for input in ["look", "nwe2n", "look"] {
        tx.send(RuntimeAction::SubmitInput(Arc::new(input.to_string())))
            .unwrap();
    }

    let mut output = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(2), events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    output.push(line.text.clone());
                }
            }
        }
        if output.len() >= 7 {
            break;
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    assert_eq!(
        output,
        ["look", "n", "w", "e", "n", "n", "look"],
        "the granted sendRaw burst must drain and leave the runtime responsive"
    );
}
