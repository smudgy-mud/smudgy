// =============================================================================
//  Context Deck — the room's services as a horizontal card strip
// =============================================================================
//  NukeFire.Context describes what you can do *here* (remorter, packrat vault,
//  zone intelligence, …) as titled blocks with status lines and invocable
//  actions. Each block becomes a card: status tones color the values, actions
//  become buttons. An action with a `confirm` prompt raises a Modal before
//  sending; one with `arguments` is proposed into the command input instead
//  (Enter sends, typing amends), so the player supplies the arguments.

import { input, send, session } from "smudgy:core";
import {
  Button,
  Column,
  Container,
  Modal,
  Row,
  Scrollable,
  Space,
  Text,
  Tooltip,
  createWidget,
} from "smudgy:widgets";
import {
  watchMessage,
  type NukeFireContextAction,
  type NukeFireContextEntry,
} from "smudgy://kapusniak/nukefire-gmcp";
import { widgetMetric, widgetTextSize } from "./config.ts";
import { visibleContexts } from "./context-model.ts";
import { UI, kindColor, themeBackground, toneColor } from "./theme.ts";

const PANE = "Deck";

let contexts: readonly NukeFireContextEntry[] = [];
let pendingConfirm: { action: NukeFireContextAction; from: string } | null = null;
let shown = false;

watchMessage("NukeFire.Context", (ctx) => {
  contexts = visibleContexts(ctx?.contexts ?? []);
  pendingConfirm = null; // room changed out from under a confirm prompt
  if (shown) mount();
});

function runAction(action: NukeFireContextAction, from: string): void {
  if (action.enabled === false) return;
  if (action.confirm) {
    pendingConfirm = { action, from };
    mount();
    return;
  }
  execute(action);
}

function execute(action: NukeFireContextAction): void {
  if (action.arguments && action.arguments.length > 0) {
    // Let the player fill the arguments: propose `command ` selected in the
    // input; Enter sends as-is, typing replaces.
    input.propose(`${action.command} `);
    input.focus();
    return;
  }
  send(action.command);
}

function actionButton(entry: NukeFireContextEntry, action: NukeFireContextAction) {
  if (action.enabled === false) {
    return (
      <Tooltip tip={action.disabledReason || "unavailable"}>
        <Text size={widgetTextSize(10)} color={UI.faint}>{action.label}</Text>
      </Tooltip>
    );
  }
  const color = action.style === "danger" ? UI.danger : action.style === "primary" ? UI.header : UI.text;
  const button = (
    <Button
      variant={action.style === "primary" ? "primary" : "subtle"}
      onPress={() => runAction(action, entry.title)}
    >
      <Text size={widgetTextSize(10)} color={color}>{action.label}</Text>
    </Button>
  );
  return action.help ? <Tooltip tip={action.help}>{button}</Tooltip> : button;
}

const CARD_W = widgetMetric(270);

function card(entry: NukeFireContextEntry) {
  return (
    <Container width={CARD_W} height="fill" background={themeBackground.bind()}>
      <Column width="fill" height="fill" padding={10} spacing={4}>
        {[
          <Row spacing={6}>
            <Text size={widgetTextSize(12)} color={kindColor(entry.kind)}>{entry.title}</Text>
            <Space width="fill" />
            <Text size={widgetTextSize(9)} color={UI.faint}>{entry.kind}</Text>
          </Row>,
          entry.id?.toLowerCase() === "zone-intelligence" ? null : (
            <Text size={widgetTextSize(10)} color={UI.dim}>{entry.summary}</Text>
          ),
          ...entry.status.map((s) => (
            <Row spacing={6}>
              <Text size={widgetTextSize(10)} color={UI.dim}>{s.label}:</Text>
              <Text size={widgetTextSize(10)} color={toneColor(s.tone)}>{String(s.value)}</Text>
            </Row>
          )),
          <Column spacing={4}>
            {entry.actions.map((action) => actionButton(entry, action))}
          </Column>,
        ]}
      </Column>
    </Container>
  );
}

function confirmModal() {
  const pending = pendingConfirm;
  if (!pending) return null;
  const dismiss = () => {
    pendingConfirm = null;
    mount();
  };
  return (
    <Modal onDismiss={dismiss}>
      <Container background={themeBackground.bind()}>
        <Column width={widgetMetric(340)} padding={14} spacing={10}>
          <Text size={widgetTextSize(12)} color={UI.header}>{pending.from}</Text>
          <Text size={widgetTextSize(12)} color={UI.text}>{pending.action.confirm}</Text>
          <Row spacing={8}>
            <Space width="fill" />
            <Button variant="subtle" onPress={dismiss}>
              <Text size={widgetTextSize(11)} color={UI.dim}>Cancel</Text>
            </Button>
            <Button
              variant="primary"
              onPress={() => {
                const action = pending.action;
                pendingConfirm = null;
                mount();
                execute(action);
              }}
            >
              <Text size={widgetTextSize(11)} color={pending.action.style === "danger" ? UI.danger : UI.bright}>
                {pending.action.label}
              </Text>
            </Button>
          </Row>
        </Column>
      </Container>
    </Modal>
  );
}

function mount(): void {
  createWidget(
    "nf-deck",
    <Column width="fill" height="fill" padding={4} spacing={4}>
      {[
        contexts.length === 0 ? (
          <Container width="fill" height="fill" align_x="center" align_y="center">
            <Text size={widgetTextSize(11)} color={UI.faint}>No services in this room.</Text>
          </Container>
        ) : (
          <Scrollable width="fill" height="fill" direction="horizontal">
            <Row spacing={8}>{contexts.map(card)}</Row>
          </Scrollable>
        ),
        confirmModal(),
      ]}
    </Column>,
    { pane: PANE },
  );
}

export function open(): void {
  const parent = session.panes.get("Affects") ?? session.mainPane;
  parent.split("bottom", {
    name: PANE,
    height: widgetMetric(185),
    terminal: false,
  });
  shown = true;
  mount();
}

export function close(): void {
  shown = false;
  pendingConfirm = null;
  session.panes.get(PANE)?.close();
}
