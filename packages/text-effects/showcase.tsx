// Interactive package entry, also available when installed without a launcher script.
import { createAlias, echo, session, style } from "smudgy:core";
import { Button, Column, Row, Scrollable, Slider, Span, Text, createWidget } from "smudgy:widgets";
import { effectNames, loadEffect, type EffectName, type EffectMap } from "./load.ts";

type Name = EffectName;
type Controls = { name: Name; scale: number; intensity: number; speed: number; duration: number; fontSize: number; face: string; palette: "default" | "warm" | "cool" | "mono"; peak: number; yaw: number; weight: number; pieces: number; depth: number; spin: number; spread: number; amplitude: number; mode: "default" | "arrival" | "departure" | "cycle"; direction: "left" | "right" };
// Playground controls belong to this module instance, not the cross-package state store.
const controls: Controls = { name: "greeting", scale: 1, intensity: 1, speed: 1, duration: 2000, fontSize: 24, face: "monospace", palette: "default", peak: 3, yaw: 0.35, weight: 0.5, pieces: 3, depth: 1, spin: 1, spread: 1, amplitude: 1, mode: "default", direction: "right" };
const captions: Record<Name, string> = {
    lastStand: "You are mortally wounded!",
    victoryStamp: "You have gained a level!",
    focusLock: "The assassin marks you.",
    rebound: "You recover your footing.",
    ricochet: "The shot glances off your shield.",
    overdrive: "Your attacks accelerate!",
    aftershock: "The ground answers your strike.",
    crosscut: "Your blade finds its mark.",
    knockback: "The ogre knocks you back!",
    unarmed: "You hit the wraith.",
    eventHorizon: "The inscription falls beyond the event horizon.",
    dragonBreath: "The ancient dragon exhales.",
    vanish: "The inscription slips edge-on into a seam.",
    psionic: "An invisible pressure passes through your thoughts.",
    weightPulse: "A quiet word carries unexpected weight.",
    decay: "The ancient warning crumbles at its edges.",
    lava: "Molten words cool beneath a black crust.",
    crystal: "The inscription becomes flawless crystal.",
    vitalSign: "Your groupmate needs a heal!",
    echoFrame: "A friend has entered the room.",
    alarm: "Your protective ward is gone!",
    prismSweep: "You discover a hidden passage.",
    cometCall: "A formidable creature approaches.",
    beacon: "The lost courier is here.",
    converge: "The archmage has chosen you.",
    sonar: "Aria calls for your attention.",
    phoenixRebirth: "Rise from the ashes.",
    astralConjunction: "The stars align.",
    interruptNow: "The lich begins a death spell!",
    spatialStitch: "The torn words are made whole.",
    poisoned: "Venom seeps into your blood.",
    charm: "Aria smiles warmly at you.",
    geas: "Your oath binds you.",
    verdant: "The forest remembers your name.",
    stoneSkin: "Your skin turns to living stone.",
    gilding: "The crown of the forgotten king.",
    silence: "Your voice falls silent.", possession: "Another will takes hold.",
    trueSight: "The illusion falls away.", forgetting: "You almost remember.",
    voidBloom: "The shadows consume you.", watchers: "Something watches you.",
    mycelium: "The inscription awakens.", barkMend: "Wounded words heal.",
    ritualKnot: "An oath binds you.", counterSeal: "Your counterspell takes hold.",
    soulDrain: "Your strength drains away.",
    blackTide: "The shadows claim you.", eldritchEcho: "Something remembers you.",
    aberration: "Something is wrong.", rootbind: "Roots bind the gate.",
    autumn: "The forest remembers.", spellSeal: "The seal awakens.",
    leyWeave: "The ley lines answer.", elderSign: "The elder sign awakens.",
    wind: "The desert wind rises.",
    sand: "Stone turns to sand.",
    recoil: "Your blow lands!",
    accordion: "The scroll unfolds.",
    pendulum: "Time swings onward.",
    domino: "The wards awaken.",
    tumble: "A goblin rolls into view.",
    magnetic: "The seal aligns.",
    vortex: "The maelstrom opens.",
    shatter: "The crystal fractures.",
    unravel: "The binding unravels.",
    liquefy: "The rune melts.",
    inkBloom: "The ink finds its shape.",
    duel: "Two titans collide!",
    eclipse: "The sun is swallowed.",
    dimensionalRift: "A rift opens between worlds.",
    shockfront: "The shockfront strikes!",
    judgement: "Judgement falls!",
    ascension: "You ascend!",
    emphasis: "Stand firm!", tide: "The sea rises beneath your feet.",
    elastic: "Boing!", quake: "The earth shudders!", explode: "The seal shatters!", implode: "The seal reforms!",
    fire: "Flames race across the orc's blade.", smoke: "The thief slips back into the shadows.",
    reverseSmoke: "A figure takes shape.",
    electricity: "An electric arc leaps between your fingers.", lightning: "A lightning bolt tears through the chamber.",
    poisonCloud: "A sickly cloud of poison swirls around you.",
    greeting: "Aria, your friend, has arrived.", hostile: "An enemy emerges from the shadows.",
    dread: "An ancient dragon towers above you.", quest: "The lost courier is here.",
    levelUp: "You have gained a level!", healNeeded: "Aria is badly wounded!",
    expiring: "Your protective ward fades.", rally: "Your group gathers around you.",
    discovery: "You discover a hidden passage.", radiate: "Attention starts here",
    holyGleam: "A brilliant holy light surrounds you.", animeImpact: "Their blades collide!",
    healingCloud: "Warm healing mist restores your strength.", frost: "Ice crystallizes around the words.",
    acid: "Caustic acid beads along the blade.", spectral: "A spectral presence drifts nearby.",
    swarm: "A swarm of insects gathers around you.", thorns: "Thorny vines bind the passage.",
    ward: "A sacred ward protects you.", impact: "A shockwave rolls through the chamber.",
    slash: "Your blade slices across the goblin's guard.", critical: "A critical strike lands!",
    parry: "You turn the incoming blade aside.", healWave: "Healing energy flows through Aria.",
    cleave: "Your axe cleaves through the troll's guard.", riposte: "You counter with a piercing riposte!",
    dispel: "The binding spell breaks apart.", divineShield: "A divine shield protects Aria.",
    fireflies: "Fireflies dance beside the forest path.", runicOrbit: "Ancient runes circle the inscription.",
    blessing: "A golden blessing settles over you.", teleport: "Aria vanishes and reforms beside you.",
};
const palettes: Record<"default" | "warm" | "cool" | "mono", readonly [string, string, string] | undefined> = {
    default: undefined, warm: ["#ac2929", "#ffab44", "#fff2c8"],
    cool: ["#364dc6", "#71d7ec", "#effbff"], mono: ["#616b79", "#b5c4d6", "#ffffff"],
};
function content(Effect: EffectMap[Name]) {
    const c = controls;
    // Presets read only their own controls. Leave finish and other authored defaults
    // to the component, so its preview behaves like an ordinary direct import.
    const props = {
        scale: c.scale, intensity: c.intensity, speed: c.speed, duration: c.duration,
        colors: palettes[c.palette], amplitude: c.amplitude,
        mode: c.mode === "default" ? undefined : c.mode, direction: c.direction,
        peak: c.peak, yaw: c.yaw, weight: c.weight,
        pieces: c.pieces, depth: c.depth, spin: c.spin, spread: c.spread,
    };
    return <Effect {...props}>
        <Span style={style.default.bg("default")} fontSize={c.fontSize} fontFace={c.face}>{captions[c.name]}</Span>
    </Effect>;
}
let replayVersion = 0;
async function replay(name: Name = controls.name) {
    const version = ++replayVersion;
    controls.name = name;
    render();
    try {
        const Effect = await loadEffect(name);
        // A slower previous import must not replay after a newer selection.
        if (version === replayVersion) echo(content(Effect));
    } catch (error) {
        if (version === replayVersion) echo(`Could not load ${name}: ${error}`);
    }
}
function cycle(key: "scale" | "intensity" | "speed" | "fontSize" | "peak" | "yaw" | "weight" | "pieces" | "depth" | "spin" | "spread" | "amplitude", values: number[]) {
    const at = values.indexOf(controls[key]);
    controls[key] = values[(at + 1) % values.length];
    replay();
}
const names = effectNames;
const families: readonly [string, readonly Name[]][] = [
    ["Readable hero signals", ["unarmed", "knockback", "crosscut", "aftershock", "overdrive", "ricochet", "rebound", "focusLock", "victoryStamp", "lastStand"]],
    ["Ink and forces", ["crystal", "lava", "decay", "weightPulse", "psionic", "vanish"]],
    ["Hero transformations", ["dragonBreath", "eventHorizon"]],
    ["Far-reaching attention", ["sonar", "converge", "beacon", "cometCall", "prismSweep", "alarm", "echoFrame", "vitalSign"]],
    ["Enchanted materials", ["stoneSkin", "gilding"]],
    ["Mind and perception", ["charm", "silence", "possession", "trueSight", "forgetting"]],
    ["Elemental", ["fire", "electricity", "lightning", "frost", "acid"]],
    ["Nature", ["poisoned", "verdant", "poisonCloud", "healingCloud", "thorns", "rootbind", "autumn", "mycelium", "barkMend", "swarm", "fireflies"]],
    ["Eldritch", ["blackTide", "eldritchEcho", "aberration", "elderSign", "voidBloom", "watchers", "soulDrain"]],
    ["Arcane inscriptions", ["spatialStitch", "geas", "spellSeal", "leyWeave", "ritualKnot", "counterSeal"]],
    ["Arcane and shadow", ["ward", "spectral", "smoke", "reverseSmoke", "teleport", "dispel", "runicOrbit"]],
    ["Divine and healing", ["holyGleam", "healWave", "blessing", "divineShield"]],
    ["Combat and spectacle", ["slash", "cleave", "critical", "parry", "riposte", "impact", "animeImpact"]],
    ["Ink and fragments", ["shatter", "unravel", "liquefy", "inkBloom"]],
    ["Hero text", ["phoenixRebirth", "astralConjunction", "duel", "eclipse", "dimensionalRift", "shockfront", "judgement", "ascension"]],
    ["Text gestures", ["recoil", "accordion", "pendulum", "domino", "tumble", "magnetic", "vortex"]],
    ["Grains", ["wind", "sand"]],
    ["Text motion", ["emphasis", "tide", "elastic", "quake", "explode", "implode"]],
    ["Attention", ["interruptNow", "radiate", "greeting", "hostile", "dread", "quest", "levelUp", "healNeeded", "expiring", "rally", "discovery"]],
];
function mount() {
    session.mainPane.split("left", { name: "Effect controls", terminal: false, width: 520 }).show();
    replay();
}
const rows: ReturnType<typeof Text>[] = [];
for (const [label, effects] of families) {
    rows.push(<Text size={18}>{label}</Text>);
    for (let i = 0; i < effects.length; i += 3) {
        rows.push(<Row spacing={8}>{effects.slice(i, i + 3).map(name => <Button onPress={() => replay(name)}>{name}</Button>)}</Row>);
    }
}
function render() {
    createWidget("effect-controls", <Column padding={16}><Scrollable height="fill">
        <Column spacing={10}>
            <Text size={22}>Text effects playground</Text>
            <Text>The package root opens this playground. In your scripts, import each effect from its own subpath:</Text>
            <Text size={13}>{'import Fire from\n    "@text-effects/fire";\nimport { echo } from "smudgy:core";\n\necho(<Fire duration={1200}>Flames!</Fire>);'}</Text>
            <Text>Individual imports load only the selected shader family. Use /load for on-demand loaders and /types for prop types. /all explicitly loads the entire catalogue.</Text>
            <Text>In package manifests, declare dependencies: ["smudgy:@text-effects"]. The same @text-effects imports use a local package first when one is present.</Text>
            <Text size={13}>{'Sandboxed packages request widgets: ["create", "shaders"], plus session: ["echo"] for output or display: ["change"] for line edits.'}</Text>
            <Text>Choose an effect below, tune it, and replay it in the terminal. Type effects to reopen this panel, or effect fire to try a preset.</Text>
            <Text>Selected: {controls.name}</Text>
            <Row spacing={8}>
                <Button onPress={() => cycle("scale", [0.5, 1, 1.5, 2, 3])}>Scale: {controls.scale}</Button>
                <Button onPress={() => cycle("intensity", [0.5, 1, 1.5, 2])}>Intensity: {controls.intensity}</Button>
                <Button onPress={() => cycle("speed", [0, 0.5, 1, 2])}>Speed: {controls.speed}</Button>
            </Row>
            <Text>Duration: {controls.duration} ms (0 repeats)</Text>
            <Slider min={0} max={2000} step={1} value={controls.duration}
                onChange={value => { controls.duration = value; render(); }}
                onRelease={() => replay()} width="fill" />
            <Button onPress={() => cycle("fontSize", [16, 24, 40, 64])}>Font: {controls.fontSize}</Button>
            <Row spacing={8}>
                <Button onPress={() => { const faces = ["monospace", "serif", "sans-serif"]; controls.face = faces[(faces.indexOf(controls.face) + 1) % faces.length]; replay(); }}>Face: {controls.face}</Button>
                <Button onPress={() => { const colors = ["default", "warm", "cool", "mono"] as const; controls.palette = colors[(colors.indexOf(controls.palette) + 1) % colors.length]; replay(); }}>Palette: {controls.palette}</Button>
            </Row>
            <Row spacing={8}>
                <Button onPress={() => replay()}>Replay in terminal</Button>
            </Row>
            <Text>Emphasis: growth, perspective and ink weight</Text>
            <Row spacing={8}>
                <Button onPress={() => { controls.name = "emphasis"; cycle("peak", [1.5, 3, 5, 8]); }}>Peak: {controls.peak}</Button>
                <Button onPress={() => { controls.name = "emphasis"; cycle("yaw", [0, 0.35, 0.7, -0.7]); }}>Yaw: {controls.yaw}</Button>
                <Button onPress={() => { controls.name = "emphasis"; cycle("weight", [0, 0.5, 1, 2]); }}>Weight: {controls.weight}</Button>
            </Row>
            <Text>Arrival / departure controls</Text>
            <Row spacing={8}>
                <Button onPress={() => { const modes = ["default", "arrival", "departure", "cycle"] as const; controls.mode = modes[(modes.indexOf(controls.mode) + 1) % modes.length]; replay(); }}>Mode: {controls.mode}</Button>
                <Button onPress={() => { controls.name = "wind"; controls.direction = controls.direction === "left" ? "right" : "left"; replay(); }}>Wind: {controls.direction}</Button>
            </Row>
            <Text>Explode / Implode: captured ink in three dimensions</Text>
            <Row spacing={8}>
                <Button onPress={() => { if (controls.name !== "implode") controls.name = "explode"; cycle("pieces", [2, 3, 4]); }}>Pieces/axis: {controls.pieces}</Button>
                <Button onPress={() => { if (controls.name !== "implode") controls.name = "explode"; cycle("depth", [0, 0.5, 1, 2]); }}>Depth: {controls.depth}</Button>
                <Button onPress={() => { if (controls.name !== "implode") controls.name = "explode"; cycle("spin", [0, 0.5, 1, 2]); }}>Spin: {controls.spin}</Button>
            </Row>
            <Row spacing={8}>
                <Button onPress={() => { if (controls.name !== "implode") controls.name = "explode"; cycle("spread", [0.5, 1, 1.5, 2]); }}>Spread: {controls.spread}</Button>
                <Button onPress={() => cycle("amplitude", [0, 0.5, 1, 2])}>Motion: {controls.amplitude}</Button>
            </Row>
            <Text>Motion amplitude applies to text motion presets. Peak growth fits the available pane; font size still reserves only its normal line height.</Text>
            <Text>Each replay adds a terminal line. Duration 0 continues while visible.</Text>
            <Column spacing={8}>{rows}</Column>
        </Column>
    </Scrollable></Column>, { pane: "Effect controls" });
}
createAlias("^effects$", mount);
createAlias("^effect ([A-Za-z]+)$", (args) => {
    const name = args[1] as Name;
    if (names.includes(name)) replay(name);
    else echo(`Unknown effect. Choose: ${names.join(", ")}`);
});
mount();
