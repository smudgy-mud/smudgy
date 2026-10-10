// Install text-effects, then copy into a server's modules directory and reload scripts.
// For checkout testing, copy packages/text-effects into the server; the same imports prefer it locally.
// Type inline-demo to repeat.
import { createAlias, createTrigger, echo, line, link, style } from "smudgy:core";
import {
    Button, Canvas, Column, Container, ProgressBar, Row,
    Span, Text, Tooltip, createWidget,
} from "smudgy:widgets";

import Fire from "@text-effects/fire";
import Electricity from "@text-effects/electricity";
import Lightning from "@text-effects/lightning";
import Smoke from "@text-effects/smoke";
import PoisonCloud from "@text-effects/poisonCloud";

function radiate() {
    echo(`Attention starts here ${<Canvas width={12} height={12} overflow="pane"
        terminalText="[red pulse]" scene={[
            { kind: "circle", id: "dot", cx: 6, cy: 6, r: 4, fill: "#ff3030" },
            { kind: "circle", id: "wave", cx: 6, cy: 6, r: 4,
              fill: "#ff3030", opacity: 0.24, stroke: { color: "#ff3030", width: 3 },
              animate: {
                  r: { to: 4096, duration: 2400, ease: "out" },
                  opacity: { to: 0, duration: 2400, ease: "out" },
              }, transient: true },
        ]}/>}`);
}

function demo() {
    echo(<Span>Drag to select: <Fire>
        <Span fontFace="monospace" fontStyle="italic" fontWeight="bold" fontSize={20} style={style.red}>
            this text is on fire
        </Span>
    </Fire>. Copy and search still work.</Span>);
    echo(<Span>An ordinary inline control: <Button variant="primary" onPress={() => echo("You clicked the inline button.")}>Click me</Button> followed by terminal text.</Span>);
    echo(<Container width={400} height={360} background="#192431" terminalText="Status panel: health 72/100; pulse and inspect actions">
        <Column padding={18} spacing={18}>
            <Text size={24} color="#ffffff">Expedition status</Text>
            <Span fontSize={18} fontWeight="bold">Selectable details inside an ordinary panel.</Span>
            <Text color="#9eb9d4">Health: 72 / 100</Text>
            <Tooltip tip="This is an ordinary ProgressBar in terminal scrollback.">
                <ProgressBar width={350} height={16} value={72} color="#51cf9a"/>
            </Tooltip>
            <Row spacing={12}>
                <Tooltip tip="Expand a red circle across the terminal pane" position="top">
                    <Button variant="primary" onPress={radiate}>Pulse again</Button>
                </Tooltip>
                <Tooltip tip="The callback runs in the script that created this button" position="bottom">
                    <Button onPress={() => echo("Panel action: inspected.")}>Inspect</Button>
                </Tooltip>
            </Row>
            <Span>Exits: {link("north")`${style.cyan`north`}`}</Span>
            <Fire><Span>Ordinary widget, selectable fire text.</Span></Fire>
        </Column>
    </Container>);
    echo(<Electricity><Span fontSize={22}>Shocking grasp</Span></Electricity>);
    echo(<Lightning><Span fontSize={22}>Lightning bolt crosses this line</Span></Lightning>);
    echo(<Smoke duration={4000}><Span fontSize={22}>The thief fades from view...</Span></Smoke>);
    echo(<PoisonCloud particles={1.4} swirl={1.2}><Span fontFace="serif" fontSize={24}>A poisonous cloud coils around you.</Span></PoisonCloud>);
    radiate();
}

createAlias("^inline-demo$", demo);
createTrigger("^.*danger.*$", () => {
    line.replace("danger", <Fire duration={3000}>
        <Span fontSize={20} fontWeight="bold" fontStyle="italic">danger</Span>
    </Fire>);
    line.insert(<Span fontWeight="bold">⚠ </Span>, 0);
});
// These are the same ordinary widget components outside the terminal buffer.
createWidget("inline-demo-launcher", <Container>
    <Column spacing={8}>
        <Fire><Span fontFace="serif" fontStyle="italic" fontSize={24}>Inline widget demo</Span></Fire>
        <Button onPress={demo}>Run demo</Button>
    </Column>
</Container>);
demo();

// Numeric dimensions reserve layout space. Canvas overflow expands only painting;
// it never captures input outside the 12px box and always respects the pane clip.
// No per-frame JavaScript: native tweens and GPU fire request the next presentation frame.

// Fire accepts raw/styled text or one Span. The Span supplies text/font
// metadata directly; put layouts and controls outside the text effect.
