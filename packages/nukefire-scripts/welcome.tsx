// =============================================================================
//  First-run welcome — native widgets over a quiet Canvas reading surface
// =============================================================================

import { vars } from "smudgy:core";
import {
  Button,
  Canvas,
  Column,
  Container,
  Modal,
  Row,
  Scrollable,
  Space,
  Stack,
  Text,
  createWidget,
  removeWidget,
  type CanvasShape,
} from "smudgy:widgets";
import { widgetMetric, widgetTextSize } from "./config.ts";
import { UI } from "./theme.ts";

const WIDGET = "nf-welcome";
const DISMISSED_VAR = "nfWelcomeDismissed";

const FRAME_WIDTH = 820;
const FRAME_HEIGHT = 940;
const VIEWPORT_HEIGHT = 680;

/** Quiet, borderless reading surface behind the native content. */
const backgroundScene: CanvasShape[] = [
  {
    kind: "rect",
    y: 0,
    width: FRAME_WIDTH,
    height: FRAME_HEIGHT,
    fill: {
      gradient: {
        from: [0, 0],
        to: [FRAME_WIDTH, FRAME_HEIGHT],
        stops: [[0, "#081426"], [0.58, "#0a1d35"], [1, "#111a35"]],
      },
    },
  },
];

const dividerScene: CanvasShape[] = [
  {
    kind: "line",
    x1: 1,
    y1: 6,
    x2: 744,
    y2: 6,
    stroke: {
      color: {
        gradient: {
          from: [0, 6],
          to: [744, 6],
          stops: [[0, UI.gold], [0.55, UI.steel], [1, "rgba(42,157,143,0)"]],
        },
      },
      width: 2,
    },
  },
];

function sectionTitle(index: string, title: string) {
  return <Text size={widgetTextSize(17)} color={UI.header}>{index} · {title}</Text>;
}

function codePanel(label: string, lines: readonly string[], height: number) {
  const scene: CanvasShape[] = [
    {
      kind: "rect",
      x: 0,
      y: 0,
      width: 744,
      height,
      rx: 9,
      fill: "rgba(3, 12, 24, 0.92)",
      stroke: { color: "rgba(69, 123, 157, 0.65)", width: 1.5 },
    },
    { kind: "rect", x: 0, y: 0, width: 5, height, rx: 3, fill: UI.teal },
    { kind: "text", x: 18, y: 12, text: label, size: 9, color: UI.faint, font: "monospace" },
    ...lines.map((text, index): CanvasShape => ({
      kind: "text",
      x: 18,
      y: 34 + index * 21,
      text,
      size: 13,
      color: UI.bright,
      font: "monospace",
    })),
  ];
  return (
    <Canvas
      width="fill"
      height={widgetMetric(height)}
      view_box={[0, 0, 744, height]}
      fit="fill"
      scene={scene}
    />
  );
}

function dismiss(): void {
  vars[DISMISSED_VAR] = true;
  close();
}

function content() {
  return (
    <Column width="fill" height="fill" padding={widgetMetric(40)} spacing={widgetMetric(15)}>
      <Row width="fill" spacing={widgetMetric(10)}>
        <Text size={widgetTextSize(26)} color={UI.bright}>Welcome to Smudgy's NukeFire scripts</Text>
        <Space width="fill" />
        <Button variant="subtle" onPress={dismiss}>
          <Text size={widgetTextSize(12)} color={UI.bright}>Close</Text>
        </Button>
      </Row>
      <Canvas width="fill" height={widgetMetric(12)} view_box={[0, 0, 744, 12]} fit="fill" scene={dividerScene} />
      <Text size={widgetTextSize(13)} color={UI.bright}>
        These scripts, like Smudgy, are still early and experimental. Thank you for giving them a try!
      </Text>

      {sectionTitle("01", "Connecting to multiple characters")}
      <Text size={widgetTextSize(13)} color={UI.bright}>
        To connect to multiple characters, use Connect at the top. As you connect, new sessions are positioned and managed by NukeFire Scripts.
      </Text>

      {sectionTitle("02", "Playing with multiple characters")}
      <Text size={widgetTextSize(13)} color={UI.bright}>
        Multi-session control adds F1–F4 hotkeys and redirection aliases. Session numbers follow the order of your connected sessions.
      </Text>
      <Text size={widgetTextSize(14)} color={UI.teal}>Hotkeys</Text>
      {codePanel("KEYBOARD ROUTING", [
        "Press F1, F2, F3, or F4 to switch between connected sessions.",
        "Press Ctrl+F1 through Ctrl+F4 to magnify a stacked session.",
      ], 88)}

      <Text size={widgetTextSize(14)} color={UI.teal}>Redirection aliases</Text>
      <Text size={widgetTextSize(13)} color={UI.bright}>
        Send commands from any connected session through any other connected session. Selectors can be compact or separated by a space.
      </Text>
      {codePanel("COMMAND ROUTING", [
        "134 look   ← sends ‘look’ from sessions 1, 3, and 4",
        "134look    ← same",
        "* look     ← sends ‘look’ from every session",
        "-4 look    ← sends ‘look’ from every session except session 4",
        "*-4 look   ← same",
      ], 150)}
      <Text size={widgetTextSize(13)} color={UI.bright}>
        These aliases also work inside triggers, hotkeys, and other aliases. A Ctrl+D hotkey that sends “2 doubletap” will always issue “doubletap” from session 2, regardless of which session is active.
      </Text>

      <Row width="fill" spacing={widgetMetric(10)}>
        <Text size={widgetTextSize(12)} color={UI.dim}>Run “nf help” anytime to see the available utilities.</Text>
        <Space width="fill" />
        <Button variant="primary" onPress={dismiss}>
          <Text size={widgetTextSize(12)} color={UI.bright}>Done</Text>
        </Button>
      </Row>
    </Column>
  );
}

export function open(): void {
  createWidget(
    WIDGET,
    <Modal onDismiss={dismiss} background="rgba(1, 5, 14, 0.84)">
      <Container
        width={widgetMetric(FRAME_WIDTH)}
        height={widgetMetric(VIEWPORT_HEIGHT)}
        background={UI.navyDeep}
      >
        <Scrollable width="fill" height="fill">
          <Container width="fill" height={widgetMetric(FRAME_HEIGHT)}>
            <Stack width="fill" height="fill">
              <Canvas
                width="fill"
                height="fill"
                view_box={[0, 0, FRAME_WIDTH, FRAME_HEIGHT]}
                fit="fill"
                scene={backgroundScene}
              />
              {content()}
            </Stack>
          </Container>
        </Scrollable>
      </Container>
    </Modal>,
  );
}

export function close(): void {
  removeWidget(WIDGET);
}

/** Show automatically until this profile dismisses the first-run welcome. */
export function showFirstRun(): void {
  if (vars[DISMISSED_VAR] !== true) open();
}
