import {echo, style, link, line, buffer, createTrigger, createState} from "smudgy:core";
import {Span, TextEffect, Container, Text, Button, Canvas, Column, Tooltip, Slider, createWidget} from "smudgy:widgets";
import copyShader from "./copy.wgsl";
function FixtureEffect(props: any = {}, children?: any) {
    const options = {shader: copyShader, uniforms: {strength: 1}, outset: 16, fadeOut: 120, ...props};
    return children === undefined ? TextEffect(options) : TextEffect(options, children);
}
const sliderState = createState<{duration:number}>("slider-test");
sliderState.set({duration:1000});
echo(<Slider terminalText="Duration slider" min={0} max={2000} step={1}
    value={sliderState.bind("duration")} width={200}
    onChange={value => { sliderState.value.duration=value; echo("SLIDER_VALUE:"+typeof value+":"+value); }}
    onRelease={() => echo("SLIDER_RELEASE:"+sliderState.value.duration)} />);
echo(<TextEffect shader={copyShader} uniforms={{strength: 1}} composite="replace"><Span fontSize={24}>custom shader</Span></TextEffect>);
echo(Span({ fontSize: 20, children: "builder span" }));
echo(TextEffect({ shader: copyShader, uniforms: { strength: 1 },
    children: Span({ fontSize: 20, children: "builder effect" }) }));
echo(FixtureEffect({ children: "builder preset" }));
echo(Span({ children: "ignored child prop" }, "explicit child"));
echo(Button({ children: "builder button" }));
echo(<Button>{"x".repeat(1023) + "😀tail"}</Button>);
echo(<Container><Span>nested {style.red`styled projection`}</Span></Container>);
const builtinWidgets = await import("smudgy:widgets");
if ("FireEffect" in builtinWidgets) echo("MISSED_ERROR");
const hot = <FixtureEffect><Span>{style.red`café`}</Span></FixtureEffect>;
echo(<Span>before {hot} after</Span>);
echo(<TextEffect shader={copyShader} uniforms={{strength: 1}} duration={1200}>attention</TextEffect>);
echo(<Span>{link("north")`north`}</Span>);
echo(hot);
echo(<Span style={style.blue}><Span>inherited</Span></Span>);
createWidget("inline-span-test", <Container><Span>local {hot}</Span></Container>);
createWidget("ordinary-effect-test", <Container><FixtureEffect><Span fontFace="serif" fontStyle="italic" fontSize={24}>ordinary</Span></FixtureEffect></Container>);
echo(<FixtureEffect>same text</FixtureEffect>);
echo(<FixtureEffect scale={2} duration={500} fadeIn={400} fadeOut={400}>scaled intro</FixtureEffect>);
echo(<FixtureEffect duration={0}>continuous smoke</FixtureEffect>);
echo(<FixtureEffect duration={500} finish="remove">smoke intro</FixtureEffect>);
echo(<FixtureEffect><Span>same text</Span></FixtureEffect>);
echo(<FixtureEffect>flame {7} {style.red`lit`}</FixtureEffect>);
echo(<FixtureEffect><Span fontFace="serif" fontStyle="italic" fontSize={22}>styled <Span fontWeight="bold">runs</Span></Span></FixtureEffect>);
for (const makeBad of [
    () => <Span>{"two\nlines"}</Span>,
    () => <TextEffect shader={copyShader} duration={-1}>bad</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:1}} captureScale={9}>bad</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:1}} captureScale={NaN}>bad</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:1}} scale={5}>bad</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:1}} overflow="window">bad</TextEffect>,
    () => <TextEffect shader={copyShader}>missing uniform</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:1,typo:2}}>unknown uniform</TextEffect>,
    () => <TextEffect shader={copyShader} uniforms={{strength:[1,2]}}>bad scalar</TextEffect>,
    () => <TextEffect shader={{}} uniforms={{strength:1}}>forged handle</TextEffect>,
    () => <FixtureEffect><Span><FixtureEffect>nested</FixtureEffect></Span></FixtureEffect>,
    () => <Canvas overflow={3000}/>,
    () => echo(<Button terminalText={"bad\ntext"}>bad</Button>),
    () => <Span fontSize={0}>bad</Span>, () => <Span fontWeight={999}>bad</Span>,
    () => <FixtureEffect><Button>not text</Button></FixtureEffect>,
    () => <FixtureEffect><Text>label</Text></FixtureEffect>,
    () => <FixtureEffect><Container>widget</Container></FixtureEffect>,
    () => <FixtureEffect><Span>one</Span><Span>two</Span></FixtureEffect>,
    () => <FixtureEffect>mixed <Span>text</Span></FixtureEffect>,
    () => <FixtureEffect>{`${<Button>hidden widget</Button>}`}</FixtureEffect>,
    () => echo("\uE000smudgy-widget:foreign:1\uE001")]) {
    try { makeBad(); echo("MISSED_ERROR"); } catch(e) { echo(e instanceof TypeError ? "EXPECTED_ERROR" : "WRONG_ERROR"); }
}
const flame = (text: string) => <FixtureEffect duration={5000}>
    <Span fontFace="serif" fontStyle="italic" fontWeight="bold" fontSize={24} style={style.red}>
        {link("look")`${text}`}
    </Span>
</FixtureEffect>;
function editEffects(target: typeof line) {
    // Reuse one interpolated effect at both matches, including multibyte text.
    target.replace("foo", `${flame("café")}`);
    target.insert(flame("é"), 0);
    target.replaceAt(flame("焔"), 2, 9);
    // Overwrite an effect with a plain Span, then shift the remaining metadata.
    target.insert(<Span fontSize={30}>!!</Span>, 0, 2);
    target.removeAt(0, 1);
    target.highlightAt(5, 10, style.blue);
}
createTrigger("^replace ", () => editEffects(line));
let history: typeof line;
createTrigger("^history ", () => { history = buffer.line(line.number); });
createTrigger("^edit-history$", () => editEffects(history));
createTrigger("^verify-history$", () => echo(`HISTORY_TEXT:${history.text}`));
echo(`button: ${<Button onPress={() => echo("BUTTON_CLICKED")}>Click me</Button>} after`);
echo(<Container height={360} width={340} background="#192431" terminalText="Status panel"><Column padding={18} spacing={24}>
    <Text size={24} color="#ffffff">Expedition status</Text><Tooltip tip="Native tooltip"><Button onPress={() => echo("PANEL_CLICKED")}>Action</Button></Tooltip><Span>Selectable detail</Span>
</Column></Container>);
echo(<Canvas width={12} height={12} overflow="pane" terminalText="red pulse" scene={[{kind:"circle", cx:6, cy:6, r:3, fill:"#ff0000", opacity:0.2, stroke:{color:"#ff4040",width:3}, animate:{r:{to:1200,duration:2000,ease:"out"},opacity:{to:0,duration:2000}}, transient:true}]}/>);
const message = `Attention starts here ${<Canvas width={12} height={12} overflow="pane" terminalText="[red pulse]"/>}`;
echo(message); echo(message);
echo`tagged ${<FixtureEffect><Span fontSize={24} fontWeight="bold" fontStyle="italic" fontFace="monospace">hot</Span></FixtureEffect>} end`;
echo(<Span fontSize={30} fontWeight={700}>large <Span fontSize={10}>small</Span></Span>);
createWidget("font-span-test", <Container><FixtureEffect><Span fontSize={24} fontWeight="bold" fontStyle="italic" fontFace="monospace">Sized Span</Span></FixtureEffect></Container>);
const stale = `${<Span>old</Span>}`;
for (let i = 0; i < 1025; i++) String(<Span>{i}</Span>);
try { echo(stale); echo("MISSED_ERROR"); } catch(e) { echo(e instanceof TypeError ? "EXPECTED_ERROR" : "WRONG_ERROR"); }
try { await import("./bad.wgsl"); echo("MISSED_ERROR"); }
catch (error) { echo(String(error).includes("bad.wgsl") ? "SHADER_DIAGNOSTIC_OK" : "WRONG_ERROR"); }
echo("INLINE_READY");
