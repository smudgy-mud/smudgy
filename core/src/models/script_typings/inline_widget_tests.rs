//! The host widget and WGSL typing contract, independent of any effect package.
use super::{SMUDGY_CORE_DTS, SMUDGY_MAPPER_DTS, SMUDGY_WIDGETS_DTS};
use std::collections::BTreeMap;

#[test]
fn inline_widget_types_accept_native_handles_and_reject_invalid_props() {
    let ambient = BTreeMap::from([
        ("smudgy-core.d.ts".into(), SMUDGY_CORE_DTS.into()),
        ("smudgy-mapper.d.ts".into(), SMUDGY_MAPPER_DTS.into()),
        ("smudgy-widgets.d.ts".into(), SMUDGY_WIDGETS_DTS.into()),
    ]);
    let sources = BTreeMap::from([("contract.tsx".into(), r#"
import { echo, line, buffer, style, link } from "smudgy:core";
import { Span, TextEffect, Button, Canvas, Slider, Container, createWidget } from "smudgy:widgets";
import shader from "./fixture.wgsl";
const text = <Span fontFace="serif" fontStyle="italic" fontWeight={700} fontSize={24} style={style.red}>{link("look")`café`}</Span>;
const effect = <TextEffect shader={shader} uniforms={{ amount: 1, tint: [1, 0, 0, 1] }} duration={1200} captureScale={4} composite="replace">{text}</TextEffect>;
echo(effect); echo`before ${effect} after`;
for (const target of [line, buffer.line(42)]) {
    target.replace("foo", effect); target.insert(effect, 0); target.replaceAt(effect, 0, 3);
}
createWidget("ordinary", <Container>{effect}</Container>);
echo(<Button onPress={() => echo("clicked")}>Action</Button>);
echo(<Canvas width={12} height={12} overflow="pane" terminalText="pulse" />);
echo(<Slider min={0} max={2000} step={1} onChange={value => value.toFixed(0)} />);
// @ts-expect-error Handles come from WGSL imports, not strings.
const stringShader = <TextEffect shader="fixture">bad</TextEffect>;
// @ts-expect-error Typography uses known weight values.
const invalidWeight = <Span fontWeight="heavy">bad</Span>;
// @ts-expect-error A text effect accepts one Span, not sibling elements.
const siblings = <TextEffect shader={shader}><Span>one</Span><Span>two</Span></TextEffect>;
// @ts-expect-error Put mixed text and elements inside one Span.
const mixed = <TextEffect shader={shader}>outside <Span>inside</Span></TextEffect>;
// @ts-expect-error Overflow is bounded by the host's paint policy.
const overflow = <TextEffect shader={shader} overflow="window">bad</TextEffect>;
// @ts-expect-error Sliders deliver numeric values.
const slider = <Slider onChange={(value: string) => value.toUpperCase()} />;
// @ts-expect-error The native pulse prototype is not part of the widget API.
const removed = (await import("smudgy:widgets")).PulseEffect;
"#.into())]);
    let result = smudgy_script::dts::generate_declarations(&sources, &ambient).unwrap();
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}
