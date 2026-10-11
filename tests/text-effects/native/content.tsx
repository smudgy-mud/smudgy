import {echo, style, link, line, buffer, createTrigger} from "smudgy:core";
import {Span, Container, Button, createWidget} from "smudgy:widgets";
import {Effects} from "@text-effects/all";
import {createEffectBurst} from "./effect-burst.ts";
const burst=createEffectBurst<number>(3,2,1000);
burst.enqueue("a",1);burst.enqueue("a",2);burst.enqueue("b",3);burst.enqueue("c",4);
const played:number[]=[];
if(burst.drain(0,v=>played.push(v))!==2 || played.join(",")!=="2,3" || burst.size!==1) throw Error("coalescing/budget failed");
burst.enqueue("a",5);
if(burst.drain(100,v=>played.push(v))!==1 || played.join(",")!=="2,3,4") throw Error("cooldown failed");
burst.enqueue("a",6);if(burst.drain(1001,v=>played.push(v))!==1) throw Error("cooldown expiry failed");
for(let i=0;i<5;i++)burst.enqueue(String(i),i);
if(burst.size!==3)throw Error("queue capacity failed");
burst.drain(-1,v=>played.push(v));burst.clear();if(burst.size!==0)throw Error("queue clear failed");
echo("BURST_QUEUE_OK");

echo(Effects.fire({ children: "builder preset" }));
const hot = <Effects.fire><Span>{style.red`café`}</Span></Effects.fire>;
echo(<Span>before {hot} after</Span>);
echo(<Effects.fire scale={2} duration={500} fadeIn={400} fadeOut={400} speed={0}>scaled intro</Effects.fire>);
echo(<Effects.smoke duration={0}>continuous smoke</Effects.smoke>);
echo(<Effects.smoke duration={500} finish="remove">smoke intro</Effects.smoke>);
echo(<Effects.frost>held frost</Effects.frost>);
echo(<Effects.emphasis><Span fontFace="serif" fontStyle="italic" fontWeight="bold" fontSize={24}>emphasis</Span></Effects.emphasis>);
createWidget("ordinary-effect-test", <Container><Effects.fire><Span fontFace="serif" fontStyle="italic" fontSize={24}>ordinary</Span></Effects.fire></Container>);
for (const makeBad of [
    () => <Effects.fire embers={-1}>bad</Effects.fire>,
    () => <Effects.explode pieces={2.5}>bad</Effects.explode>,
    () => <Effects.explode depth={NaN}>bad</Effects.explode>,
    () => <Effects.explode spin={3}>bad</Effects.explode>,
    () => <Effects.explode spread={0}>bad</Effects.explode>,
    () => <Effects.tide amplitude={-1}>bad</Effects.tide>,
    () => <Effects.wind direction="up">bad</Effects.wind>,
    () => <Effects.vanish mode="invalid">bad</Effects.vanish>,
    () => <Effects.emphasis peak={9}>bad</Effects.emphasis>,
    () => <Effects.emphasis yaw={NaN}>bad</Effects.emphasis>,
    () => <Effects.emphasis weight={-1}>bad</Effects.emphasis>,
    () => <Effects.emphasis anchor="invalid">bad</Effects.emphasis>,
    () => <Effects.poisonCloud particles={3}>bad</Effects.poisonCloud>,
    () => <Effects.fire><Button>not text</Button></Effects.fire>,
]) {
    try { makeBad(); echo("MISSED_ERROR"); }
    catch(e) { echo(e instanceof TypeError ? "EXPECTED_ERROR" : "WRONG_ERROR"); }
}
const flame = (text: string) => <Effects.fire duration={5000}>
    <Span fontFace="serif" fontStyle="italic" fontWeight="bold" fontSize={24} style={style.red}>
        {link("look")`${text}`}
    </Span>
</Effects.fire>;
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

await import("./mud-effects.tsx");
echo("CATALOGUE_READY");
