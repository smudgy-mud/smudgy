//! Selective loading, installed playground and sandbox permission scenarios.
use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn selective_effect_imports_compile_only_requested_shader_families() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (home, _root, params) = session_fixture();
    let modules = home.path().join("InlineWidgets/modules");
    for file in ["showcase.tsx", "mud-effects.tsx", "effect-burst.ts"] {
        std::fs::remove_file(modules.join(file)).unwrap();
    }
    for stage in ["before", "loader"] {
        std::fs::write(
            modules.join(format!("probe-{stage}.wgsl")),
            "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
        )
        .unwrap();
    }
    std::fs::write(
        modules.join("inline.tsx"),
        r##"
import {echo} from "smudgy:core";
import {TextEffect} from "smudgy:widgets";
async function stamp(stage: string) {
    const {default: shader} = await import(`./probe-${stage}.wgsl`);
    echo(<TextEffect shader={shader} duration={1} animated={false}>{`PROBE:${stage}`}</TextEffect>);
}
await stamp("before");
const catalogue = await import("@text-effects/load");
await import("@text-effects/types");
if ("Effects" in catalogue || catalogue.effectNames.length === 0 || !Object.isFrozen(catalogue.effectNames))
    throw Error("/load must expose only the lightweight catalogue interface");
await stamp("loader");
const fireModule = await import("@text-effects/fire");
const Fire = fireModule.default;
const explicitFire = await import("smudgy:@text-effects/fire");
const legacyFire = await import("smudgy://official/text-effects/fire");
if (explicitFire.default !== Fire || legacyFire.default !== Fire)
    throw Error("Import shorthand and explicit addresses must share the same component");
if (fireModule.fire !== Fire || await catalogue.loadEffect("fire") !== Fire ||
    (await catalogue.loadEffects(["fire", "fire"])).fire !== Fire)
    throw Error("Direct and selective imports must reuse the same component");
echo(<Fire>SELECTIVE_FIRE</Fire>);
const cloud = await catalogue.loadEffects(["poisonCloud", "healingCloud", "poisonCloud"]);
if (!Object.isFrozen(cloud) || Object.keys(cloud).length !== 2 ||
    await catalogue.loadEffect("poisonCloud") !== cloud.poisonCloud)
    throw Error("Selected components must be cached and deduplicated");
echo(<cloud.poisonCloud>SELECTIVE_POISON</cloud.poisonCloud>);
echo(<cloud.healingCloud>SELECTIVE_HEAL</cloud.healingCloud>);
for (const name of ["__proto__", "missing"]) {
    try { await catalogue.loadEffect(name as never); }
    catch (error) { if (error instanceof TypeError) continue; throw error; }
    throw Error("Invalid effect names must reject");
}
const {Effects} = await import("@text-effects/all");
if (Object.keys(Effects).join() !== catalogue.effectNames.join() || !Object.isFrozen(Effects) ||
    Effects.fire !== Fire || Effects.poisonCloud !== cloud.poisonCloud ||
    Effects.healingCloud !== cloud.healingCloud)
    throw Error("The explicit full catalogue must reuse individual imports");
for (const name of catalogue.effectNames) {
    const Effect = Effects[name];
    const direct = await import(`@text-effects/${name}`);
    if (direct.default !== Effect || direct[name] !== Effect)
        throw Error(`${name} must have matching direct, named, lazy and eager exports`);
    // Exercise documented common boundaries against the actual native op/ABI.
    Effect({ scale: 4, intensity: 2, speed: 8, duration: 1, fadeIn: 1, fadeOut: 1,
        captureScale: 8, outset: 2048, overflow: "bounds", finish: "hold",
        colors: ["#00000000", "#ffffff", "#11223344"] }, "limits");
    Effect({ scale: 0.25, intensity: 0, speed: 0, duration: 0, fadeIn: 0, fadeOut: 0,
        captureScale: 1, outset: 0 }, "limits");
    for (const props of [{ scale: 0 }, { intensity: NaN }, { speed: Infinity },
        { duration: 1.5 }, { duration: 3600001 }, { fadeIn: -1 }, { fadeOut: 0.5 },
        { captureScale: 9 }, { colors: ["red", "#ffffff", "#ffffff"] }]) {
        try { Effect(props as never, "invalid"); }
        catch (error) { if (error instanceof TypeError) continue; throw error; }
        throw Error(`${name} accepted invalid props: ${JSON.stringify(props)}`);
    }
    echo(<Effect>{`ALL:${name}`}</Effect>);
}
echo("SELECTIVE_READY");
"##,
    )
    .unwrap();
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        effects_package_provider(),
    ));
    let mut lines = Vec::<Arc<StyledLine>>::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    'collect: loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "selective import timeout; transcript: {:?}",
                    lines.iter().map(|line| &line.text).collect::<Vec<_>>()
                )
            })
            .expect("session closed");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.clone());
                    if line.text == "SELECTIVE_READY" {
                        break 'collect;
                    }
                }
            }
        }
    }
    let shader = |text: &str| {
        let line = lines.iter().find(|line| line.text == text).unwrap();
        let settings = &line.decorations.as_ref().unwrap()[0].effect.shader;
        &settings.shader
    };
    let before = shader("PROBE:before").id;
    let loader = shader("PROBE:loader").id;
    let fire = shader("SELECTIVE_FIRE").id;
    let cloud = shader("SELECTIVE_POISON").id;
    let all = lines
        .iter()
        .filter(|line| line.text.starts_with("ALL:"))
        .map(|line| shader(&line.text).id)
        .max()
        .unwrap();
    assert_eq!(
        lines
            .iter()
            .filter_map(|line| line.text.strip_prefix("ALL:").map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>(),
        effect_component_names(),
        "every component file must be registered and importable"
    );
    // Inspect every eager component for the final ID, independent of module
    // loading order, without consuming another probe handle.
    assert_eq!(loader - before, 1, "load and types compile no shader");
    assert_eq!(fire - loader, 1, "Fire imports compile one shader");
    assert_eq!(
        cloud - fire,
        1,
        "cached Fire and cloud imports add one shader"
    );
    let program_count = std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects"),
    )
    .unwrap()
    .filter(|entry| {
        entry
            .as_ref()
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "wgsl")
    })
    .count();
    assert_eq!(
        all - cloud,
        u64::try_from(program_count - 2).unwrap(),
        "invalid names compile nothing; eager imports reuse loaded families"
    );
    assert_eq!(shader("SELECTIVE_FIRE").id, loader + 1);
    assert_eq!(shader("SELECTIVE_POISON").id, fire + 1);
    assert_eq!(shader("SELECTIVE_POISON").id, shader("SELECTIVE_HEAL").id);
    drop(events);
    join_fixture_runtime().await;
}

/// The actual package entry must work both as an import and as a consented sandbox install.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn package_root_opens_a_working_playground_on_import_and_install() {
    use smudgy_core::models::shared_packages::{self, UpdateMode};

    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    for installed in [false, true] {
        let (home, root, params) = session_fixture();
        let modules = home.path().join("InlineWidgets/modules");
        for file in ["showcase.tsx", "mud-effects.tsx", "effect-burst.ts"] {
            std::fs::remove_file(modules.join(file)).unwrap();
        }
        std::fs::write(
            modules.join("inline.tsx"),
            if installed {
                ""
            } else {
                "import '@text-effects';"
            },
        )
        .unwrap();
        let specifier = "smudgy:@text-effects";
        if installed {
            shared_packages::install_package("InlineWidgets", specifier, UpdateMode::Auto, true)
                .unwrap();
            let manifest = smudgy_script::PackageManifest::parse(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../packages/text-effects/smudgy.package.json"
            )))
            .unwrap();
            shared_packages::record_consent("InlineWidgets", specifier, &manifest.permissions)
                .unwrap();
        }
        let entry = r#"
import {echo, session} from "smudgy:core";
session.mainPane.split("right", {name: "Effect preview", terminal: true, width: 240}).show();
await import("./original-index.tsx");
await import("./original-index.tsx");
if (!session.panes.get("Effect preview")) throw Error("playground closed an unrelated pane");
echo("UNRELATED_PANE_PRESERVED");
"#;
        let mut events = Box::pin(spawn_with_package_provider(
            params,
            effects_package_provider_with_entry(Some(entry), true),
        ));
        let mut sender = None;
        let mut previews = 0;
        let mut transcript = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while previews < 4 {
            let event = tokio::time::timeout_at(deadline, events.next())
                .await
                .unwrap_or_else(|_| panic!("playground installed={installed}: {transcript:?}"))
                .expect("session closed");
            match event.event {
                SessionEvent::RuntimeReady(tx) => sender = Some(tx),
                SessionEvent::UpdateBuffer(updates) => {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            transcript.push(line.text.clone());
                            if line.text == "Ice crystallizes around the words." {
                                let settings = &line.decorations.as_ref().unwrap()[0].effect.shader;
                                assert!(
                                    settings.hold,
                                    "showcase must preserve Frost's authored finish"
                                );
                                assert!((2..=3).contains(&previews));
                                previews += 1;
                                if previews == 3 {
                                    sender
                                        .as_ref()
                                        .unwrap()
                                        .send(RuntimeAction::SubmitInput(Arc::new(
                                            "effects".into(),
                                        )))
                                        .unwrap();
                                }
                                continue;
                            }
                            if line.text != "Aria, your friend, has arrived." {
                                continue;
                            }
                            previews += 1;

                            assert_eq!(
                                line.fonts.as_ref().unwrap()[0].options.font_size,
                                Some(if previews == 1 { 24 } else { 40 })
                            );
                            if previews == 1 {
                                for message in click_playground_font(&root) {
                                    if let smudgy_widgets::WidgetMessage::InvokeCallback {
                                        callback,
                                        isolate,
                                        args,
                                    } = message
                                    {
                                        let (isolate, instance) = smudgy_core::session::runtime::IsolateId::from_widget_token(&isolate.0);
                                        assert_eq!(
                                            matches!(
                                                isolate,
                                                smudgy_core::session::runtime::IsolateId::Main
                                            ),
                                            !installed
                                        );
                                        sender
                                            .as_ref()
                                            .unwrap()
                                            .send(RuntimeAction::ExecuteWidgetCallback {
                                                isolate,
                                                instance,
                                                function: callback,
                                                args,
                                            })
                                            .unwrap();
                                    }
                                }
                            } else {
                                sender
                                    .as_ref()
                                    .unwrap()
                                    .send(RuntimeAction::SubmitInput(Arc::new(
                                        "effect frost".into(),
                                    )))
                                    .unwrap();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        assert!(
            transcript
                .iter()
                .any(|text| text == "UNRELATED_PANE_PRESERVED")
        );
        sender.unwrap().send(RuntimeAction::Shutdown).unwrap();
        drop(events);
        join_fixture_runtime().await;
        if installed {
            shared_packages::uninstall_package("InlineWidgets", specifier).unwrap();
        }
    }
}

#[tokio::test]
async fn sandboxed_widget_creation_does_not_authorize_shader_imports_or_raw_compilation() {
    use smudgy_core::models::shared_packages::{self, UpdateMode};
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (home, _root, params) = session_fixture();
    let modules = home.path().join("InlineWidgets/modules");
    for file in ["showcase.tsx", "mud-effects.tsx", "effect-burst.ts"] {
        std::fs::remove_file(modules.join(file)).unwrap();
    }
    std::fs::write(modules.join("inline.tsx"), "").unwrap();
    let specifier = "smudgy:@text-effects";
    shared_packages::install_package("InlineWidgets", specifier, UpdateMode::Auto, true).unwrap();
    let mut permissions = smudgy_script::PackagePermissions::default();
    permissions.smudgy.widgets = true;
    permissions.smudgy.echo = true;
    shared_packages::record_consent("InlineWidgets", specifier, &permissions).unwrap();
    let entry = r#"
import {echo} from "smudgy:core";
import {Button} from "smudgy:widgets";
import {effectNames} from "./load.ts";
if (effectNames.length === 0) throw Error("shader-free catalogue must remain usable");
for (const attempt of [
    () => import("./fire.wgsl"),
    () => import("./fire.ts"),
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
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        effects_package_provider_with_entry(Some(entry), false),
    ));
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
