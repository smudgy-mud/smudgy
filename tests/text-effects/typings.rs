//! Type-check the actual package, examples and a consumer of its emitted declarations.
use super::{SMUDGY_CORE_DTS, SMUDGY_MAPPER_DTS, SMUDGY_WIDGETS_DTS};
use std::collections::BTreeMap;

#[test]
#[allow(clippy::too_many_lines)]
fn text_effects_sources_and_published_declarations_keep_component_types() {
    // Include the real split module graph, including type-only edges and dynamic entries.
    fn add_effect_modules(
        directory: &std::path::Path,
        root: &std::path::Path,
        sources: &mut BTreeMap<String, String>,
    ) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                add_effect_modules(&path, root, sources);
            } else if path
                .extension()
                .is_some_and(|ext| matches!(ext.to_str(), Some("ts" | "tsx")))
            {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                sources.insert(
                    format!("text-effects/{relative}"),
                    std::fs::read_to_string(path).unwrap(),
                );
            }
        }
    }

    let ambient = BTreeMap::from([
        ("smudgy-core.d.ts".into(), SMUDGY_CORE_DTS.into()),
        ("smudgy-mapper.d.ts".into(), SMUDGY_MAPPER_DTS.into()),
        ("smudgy-widgets.d.ts".into(), SMUDGY_WIDGETS_DTS.into()),
    ]);
    let mut sources = BTreeMap::new();
    sources.insert(
            "inline.tsx".into(),
            r#"
            import { echo, line, buffer, style, link, session } from "smudgy:core";
            import { Span, TextEffect, Container, createWidget } from "smudgy:widgets";
            import Fire from "./text-effects/fire";
            import { loadEffect, loadEffects } from "./text-effects/load";
            import { Effects } from "./text-effects/all";
            import type { EffectComponent, FireProps, EffectProps } from "./text-effects/types";
            import customShader from "./custom.wgsl";
            const custom = <TextEffect shader={customShader} uniforms={{ amount: 1, tint: [1, 0, 0, 1] }}><Span fontSize={20}>Custom</Span></TextEffect>;
            // @ts-expect-error Shader handles come from WGSL imports, not strings.
            const badShader = <TextEffect shader="fire">bad</TextEffect>;
            const hot = <Fire embers={0.5}><Span fontFace="serif" fontStyle="italic" fontSize={24} fontWeight="bold" style={style.red}>{link("look")`foo`}</Span></Fire>;
            const explicit = Fire({}, [Span({ fontFace: "monospace", fontStyle: "oblique" }, "plain")]);
            const builders = [Span({ children: "text" }), Fire({ children: "fire" }),
                TextEffect({ shader: customShader, uniforms: { amount: 1, tint: [1, 0, 0, 1] }, children: "shader" })];
            const reusable: EffectComponent<FireProps> = Fire;
            reusable();
            Fire();
            Effects.emphasis({ peak: 4 }, "Attention");
            // @ts-expect-error The eager catalogue retains per-component prop types.
            Effects.emphasis({ peak: "large" });
            const ordinary: EffectComponent = reusable;
            // @ts-expect-error A shared component signature has no ember control.
            ordinary({ embers: 1 });
            const fragments = <Fire>level {7} {style.red`fire`}</Fire>;
            // @ts-expect-error A text effect accepts one Span, not sibling elements.
            const badCount = <Fire><Span>one</Span><Span>two</Span></Fire>;
            // @ts-expect-error Put the text and styled runs inside one Span.
            const badMixed = <Fire>outside <Span>inside</Span></Fire>;
            async function selective() {
                const Emphasis = await loadEffect("emphasis");
                const chosen = await loadEffects(["fire", "wind"]);
                const shared: EffectProps = { duration: 1200, scale: 1.5 };
                echo(<Emphasis {...shared} peak={3} yaw={0.35}>Attention</Emphasis>);
                echo(<chosen.fire embers={0.5}>Fire</chosen.fire>);
                echo(<chosen.wind mode="arrival" direction="left">Arrival</chosen.wind>);
                // @ts-expect-error Unrequested effects are absent from the result.
                chosen.smoke;
                // @ts-expect-error Selective loading preserves each effect's own prop types.
                const badEmbers = <chosen.fire embers="many">bad</chosen.fire>;
                // @ts-expect-error Emphasis retains its specific numeric peak type.
                const badPeak = <Emphasis peak="big">bad</Emphasis>;
                // @ts-expect-error Names must belong to the catalogue.
                await loadEffect("nonexistent");
                // @ts-expect-error The eager catalogue has moved to the explicit /all entry.
                const { Effects } = await import("./text-effects/index");
            }
            echo(<Span>before {hot} after</Span>);
            echo(`Text ${hot}`);
            echo`Text ${hot}`;
            echo(<Span fontSize={24} fontWeight="bold" fontStyle="italic" fontFace="monospace">Big</Span>);
            for (const target of [line, buffer.line(42)]) {
                target.replace("foo", hot);
                target.replace("foo", `before ${hot} after`);
                target.insert(hot, 0);
                target.insert(<Span fontSize={20}>suffix</Span>, 0, 3);
                target.replaceAt(hot, 0, 3);
                target.replaceAt(`${hot}`, 0, 3);
            }
            createWidget("inline", <Container>{hot}</Container>);
        "#
            .into(),
        );

    sources.insert(
        "inline-demo.tsx".into(),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/inline-effects.tsx"
        ))
        .replace("@text-effects/", "./text-effects/")
        .replace("@text-effects", "./text-effects/index"),
    );

    sources.insert(
        "effect-showcase.tsx".into(),
        "import '@text-effects';"
            .replace("@text-effects/", "./text-effects/")
            .replace("@text-effects", "./text-effects/index"),
    );

    sources.insert(
        "effect-burst.ts".into(),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/text-effects/effect-burst.ts"
        ))
        .into(),
    );
    sources.insert(
        "mud-effects.tsx".into(),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/text-effects/mud-effects.tsx"
        ))
        .replace("@text-effects/", "./text-effects/")
        .replace("@text-effects", "./text-effects/index")
        .replace("./effect-burst.ts", "./effect-burst"),
    );

    let effects_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects");
    add_effect_modules(&effects_root, &effects_root, &mut sources);

    let generated = smudgy_script::dts::generate_declarations(&sources, &ambient)
        .expect("generate the text-effects package declarations");
    assert!(
        generated.diagnostics.is_empty(),
        "package/examples: {:?}",
        generated.diagnostics
    );

    // Consumers resolve only the shipped .d.ts files in this compilation.
    // In particular, /all and /load must preserve the chosen effect's props.
    let mut published: BTreeMap<_, _> = generated
        .files
        .into_iter()
        .filter(|(name, _)| name.starts_with("text-effects/"))
        .collect();
    assert!(published.contains_key("text-effects/all.d.ts"));
    assert!(published.contains_key("text-effects/load.d.ts"));
    published.insert("inline.tsx".into(), sources.remove("inline.tsx").unwrap());
    let consumer = smudgy_script::dts::generate_declarations(&published, &ambient)
        .expect("type-check a consumer of the published declarations");
    assert!(
        consumer.diagnostics.is_empty(),
        "published consumer: {:?}",
        consumer.diagnostics
    );
}
