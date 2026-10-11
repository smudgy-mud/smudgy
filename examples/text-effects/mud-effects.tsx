// Install text-effects and copy this file and effect-burst.ts into modules.
// Adapt the literal trigger phrases to your MUD; no commands are sent to the server.
import { buffer, createTimer, createTrigger, line } from "smudgy:core";
import type { EffectComponent } from "@text-effects/types";
import Greeting from "@text-effects/greeting";
import HealNeeded from "@text-effects/healNeeded";
import Hostile from "@text-effects/hostile";
import Expiring from "@text-effects/expiring";
import LevelUp from "@text-effects/levelUp";
import Dispel from "@text-effects/dispel";
import { createEffectBurst } from "./effect-burst.ts";

type Attention = { target: ReturnType<typeof buffer.line>; text: string; Effect: EffectComponent; duration: number };
const burst = createEffectBurst<Attention>(64, 3, 1200);
function mark(key: string, text: string, Effect: EffectComponent, duration = 1600) {
    // Capture a stored-line handle now. The global `line` changes at the next trigger.
    burst.enqueue(key, { target: buffer.line(line.number), text, Effect, duration });
}
createTimer({ name: "mud-effect-burst", intervalMs: 120, repeat: true }, () => {
    burst.drain(Date.now(), ({ target, text, Effect, duration }) => {
        // A line removed from scrollback is allowed to disappear without playing an effect.
        if (target.text.includes(text)) target.replace(text, <Effect duration={duration}>{text}</Effect>);
    });
});
createTrigger(/^(.+) arrives from the (.+)\.$/, args => mark(`greeting:${args[1]}`, args[1], Greeting));
createTrigger(/^(.+) is badly wounded!$/, args => mark(`heal:${args[1]}`, args[1], HealNeeded, 1800));
createTrigger(/^(.+) lunges at you!$/, args => mark(`hostile:${args[1]}`, args[1], Hostile, 900));
createTrigger(/^Your protective ward fades\.$/, () => mark("ward:expired", "protective ward", Expiring, 1200));
createTrigger(/^You have gained a level!$/, () => mark("level", line.text, LevelUp, 2200));
createTrigger(/^The binding spell breaks apart\.$/, () => mark("dispel", "binding spell", Dispel, 1600));
