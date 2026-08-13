// =============================================================================
//  Communications pane — filtered, color-coded terminal feed
// =============================================================================
//  Messages are always native terminal lines. The small widget mounted above
//  them is only the channel selector; changing channels clears and replays the
//  retained terminal feed for that channel. Same-turn GMCP bursts are kept by
//  onMessage rather than a coalescing state watch.

import { createTrigger, line, link, session, style, vars } from "smudgy:core";
import type { LineColorOptions, Pane, StyledText, StyleSpan } from "smudgy:core";
import {
  Button,
  Column,
  Container,
  Row,
  Scrollable,
  Text,
  createWidget,
  removeWidget,
} from "smudgy:widgets";
import { nukefire, onMessage, watchMessage } from "smudgy://kapusniak/nukefire-gmcp";
import { parseAnsiFragments } from "./ansi.ts";
import {
  auctionItemSpans,
  auctionPresentation,
  isAuctionCommand,
  isAuctioneer,
  messageIncludesAuthor,
} from "./comms-format.ts";
import { RawCommLogger } from "./comms-log.ts";
import { chatFontSize, chatRendering, widgetMetric, widgetTextSize } from "./config.ts";
import * as codex from "./codex.tsx";
import { fragmentsFromStyleSpans } from "./line-style.ts";
import {
  UI,
  channelColor,
  channelLinkColor,
  channelTextColor,
  stripColors,
  themeBackground,
} from "./theme.ts";

const PANE = "Comms";
const TABS_WIDGET = "nf-comms-tabs";
const BUFFER = 250;
const ALL = "all";
const SELECTOR_HEIGHT = 34;
const CHANNEL_MENU_HEIGHT = 260;
const CHANNEL_TABS = [
  ALL,
  "gossip",
  "newbie",
  "group",
  "tell",
  "auction",
  "grats",
  "ssf",
  "skynet",
  "system",
] as const;

// Clean up the full-feed widget mounted by versions before the terminal feed.
removeWidget("nf-comms");

interface Message {
  time: string;
  chan: string;
  player: string;
  plain: string;
  ansi: string;
  styled?: StyledText;
}

const feed: Message[] = [];
let pane: Pane | null = null;
let lastLogError = "";
const rawCommLog = new RawCommLogger((error) => {
  if (error === lastLogError) return;
  lastLogError = error;
  session.echo(style.warn`[Comms capture] ${error}`);
});
const savedChannel = typeof vars.nfCommsChannel === "string"
  ? normalizeChannel(vars.nfCommsChannel)
  : ALL;
let selected: string = CHANNEL_TABS.includes(savedChannel as typeof CHANNEL_TABS[number])
  ? savedChannel
  : ALL;
let channelMenuOpen = false;

function normalizeChannel(channel: string): string {
  return stripColors(channel).trim().toLowerCase() || "comms";
}

function rgb(hexColor: string): { r: number; g: number; b: number } {
  const hex = Number.parseInt(hexColor.slice(1), 16);
  return { r: hex >> 16, g: (hex >> 8) & 0xff, b: hex & 0xff };
}

function currentTime(): string {
  return new Intl.DateTimeFormat([], {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date());
}

function renderTag(message: Message): StyledText {
  return style`${style.fg(rgb(UI.timestamp))`[${message.time} `}${
    style.fg(rgb(channelColor(message.chan)))`${message.chan}`
  }${style.fg(rgb(UI.timestamp))`]`}`;
}

function templateArray(strings: string[]): TemplateStringsArray {
  return Object.assign([...strings], { raw: [...strings] }) as unknown as TemplateStringsArray;
}

function renderAnsiText(text: string): StyledText {
  const fragments = parseAnsiFragments(text).map((fragment) =>
    fragment.style
      ? style(fragment.style as LineColorOptions)(templateArray([fragment.text]))
      : fragment.text
  );
  return style(templateArray(new Array(fragments.length + 1).fill("")), ...fragments);
}

function renderStyleSpans(text: string, spans: readonly StyleSpan[]): StyledText {
  const fragments = fragmentsFromStyleSpans(text, spans).map((fragment) =>
    fragment.style
      ? style(fragment.style as LineColorOptions)(templateArray([fragment.text]))
      : fragment.text
  );
  return style(templateArray(new Array(fragments.length + 1).fill("")), ...fragments);
}

function suggestAuctionCommand(command: string): void {
  const input = pane?.input;
  if (!input) return;

  const amountAt = command.indexOf("<amount>");
  if (amountAt === -1) {
    input.propose(command);
  } else {
    input.replace(command);
    input.select(amountAt, amountAt + "<amount>".length);
  }
  input.focus();
}

function renderAuctionText(text: string): StyledText {
  const pattern = /\bbid max <amount>|\bbid <amount>|\bbid min\b/gi;
  const links: Array<{
    start: number;
    end: number;
    onPress: () => void;
  }> = auctionItemSpans(text).map((item) => ({
    start: item.start,
    end: item.end,
    onPress: () => codex.lookup(item.name),
  }));

  let commandMatch: RegExpExecArray | null;
  while ((commandMatch = pattern.exec(text))) {
    const command = commandMatch[0].toLowerCase();
    links.push({
      start: commandMatch.index,
      end: commandMatch.index + commandMatch[0].length,
      onPress: () => suggestAuctionCommand(command),
    });
  }
  links.sort((a, b) => a.start - b.start);

  const fragments: Array<string | StyledText> = [];
  let cursor = 0;

  for (const linked of links) {
    if (linked.start < cursor) continue;
    fragments.push(text.slice(cursor, linked.start));
    const label = text.slice(linked.start, linked.end);
    fragments.push(link(linked.onPress)`${
      style.fg(rgb(channelLinkColor("auction")))`${label}`
    }`);
    cursor = linked.end;
  }
  fragments.push(text.slice(cursor));

  return style(
    templateArray(new Array(fragments.length + 1).fill("")),
    ...fragments,
  );
}

function renderAuction(message: Message): StyledText {
  const auction = auctionPresentation(message.plain);
  const accent = style.fg(rgb(channelColor(message.chan)));
  return style`${renderTag(message)} ${style.white`Auctioneer`} ${
    accent`· ${auction.event} ·`
  } ${style.fg(rgb(channelTextColor(message.chan)))`${renderAuctionText(auction.text)}`}`;
}

function renderBody(message: Message): StyledText {
  return chatRendering === "full-ansi"
    ? message.styled ?? renderAnsiText(message.ansi)
    : style.fg(rgb(channelTextColor(message.chan)))`${message.plain}`;
}

function renderMessage(message: Message): StyledText {
  if (
    chatRendering === "plain" &&
    message.chan === "auction" &&
    isAuctioneer(message.player)
  ) return renderAuction(message);

  const speaker = message.player === "" || messageIncludesAuthor(message.player, message.plain)
    ? ""
    : style` ${style.white`${message.player}:`}`;
  return style`${renderTag(message)}${speaker} ${renderBody(message)}`;
}

function visible(message: Message): boolean {
  return selected === ALL || message.chan === selected;
}

function append(message: Message): void {
  if (!visible(message) || !pane) return;
  pane.echo(renderMessage(message));
  pane.echo("");
}

function replay(): void {
  if (!pane) return;
  pane.clear();
  for (const message of feed) append(message);
}

function selectChannel(channel: string): void {
  channelMenuOpen = false;
  if (channel === selected) {
    mountTabs();
    return;
  }
  selected = channel;
  vars.nfCommsChannel = channel;
  mountTabs();
  replay();
}

function channelOption(channel: string) {
  const active = channel === selected;
  return (
    <Button
      width="fill"
      variant={active ? "primary" : "subtle"}
      onPress={() => selectChannel(channel)}
    >
      <Text size={widgetTextSize(11)} color={active ? UI.bright : channelColor(channel)}>
        {channel.toUpperCase()}
      </Text>
    </Button>
  );
}

function channelMenu() {
  if (!channelMenuOpen) return null;
  return (
    <Container
      width={widgetMetric(240)}
      height={widgetMetric(CHANNEL_MENU_HEIGHT)}
      background={themeBackground.bind()}
    >
      <Scrollable width="fill" height="fill">
        <Column width={widgetMetric(240)} padding={8} spacing={3}>
          {[
            <Text size={widgetTextSize(12)} color={UI.header}>Comms channel</Text>,
            ...CHANNEL_TABS.map(channelOption),
          ]}
        </Column>
      </Scrollable>
    </Container>
  );
}

function configuredCommand(channel: string): string {
  if (channel === ALL || channel === "gossip") return "gossip";
  return nukefire.value?.Comm?.Channel?.List
    ?.find((entry) => normalizeChannel(entry.name) === channel)?.command ?? channel;
}

function inputHint(): string {
  if (selected === ALL || selected === "gossip") {
    return "All/Gossip input sends: gossip <message>";
  }
  if (selected === "auction") {
    return "Auction input: <item> <price>, auction, bid, endauction, whatsauc, aucstat";
  }
  if (selected === "system" || selected === "skynet") {
    return `${selected === "system" ? "System" : "Skynet"} input sends raw commands`;
  }
  return `${selected} input sends: ${configuredCommand(selected)} <message>`;
}

function mountTabs(): void {
  if (!pane) return;
  createWidget(
    TABS_WIDGET,
    <Container
      width="fill"
      height={widgetMetric(SELECTOR_HEIGHT + (channelMenuOpen ? CHANNEL_MENU_HEIGHT : 0))}
      background={themeBackground.bind()}
    >
      <Column width="fill" height="fill" spacing={0}>
        <Row height={widgetMetric(SELECTOR_HEIGHT)} spacing={6} padding={3}>
          <Button
            variant="subtle"
            onPress={() => {
              channelMenuOpen = !channelMenuOpen;
              mountTabs();
            }}
          >
            <Text size={widgetTextSize(10)} color={channelColor(selected)}>
              {selected.toUpperCase()} {channelMenuOpen ? "▴" : "▾"}
            </Text>
          </Button>
          <Text size={widgetTextSize(9)} color={UI.dim}>{inputHint()}</Text>
        </Row>
        {channelMenu()}
      </Column>
    </Container>,
    { pane },
  );
}

function record(message: Message): void {
  feed.push(message);
  if (feed.length > BUFFER) feed.splice(0, feed.length - BUFFER);
  append(message);
}

function ingest(channel: string, player: string, ansiMessage: string): void {
  const ansi = ansiMessage.replace(/[\r\n]+/g, " ").trim();
  record({
    time: currentTime(),
    chan: normalizeChannel(channel),
    player: stripColors(player).replace(/\s+/g, " ").trim(),
    plain: stripColors(ansi).replace(/\s+/g, " ").trim(),
    ansi,
  });
}

function ingestStyled(
  channel: string,
  player: string,
  text: string,
  spans: readonly StyleSpan[],
): void {
  record({
    time: currentTime(),
    chan: normalizeChannel(channel),
    player: stripColors(player).replace(/\s+/g, " ").trim(),
    plain: text.replace(/\s+/g, " ").trim(),
    ansi: text,
    styled: renderStyleSpans(text, spans),
  });
}

onMessage("Comm.Channel", (line) => {
  // Capture first: no color stripping, whitespace normalization, or shape loss.
  rawCommLog.append(line);
  ingest(line.chan ?? "", line.player ?? "", line.msg ?? "");
});

// Skynet announcements are ordinary game output rather than Comm.Channel
// GMCP, so mirror them into Comms without removing them from the main pane.
createTrigger(/^\(Skynet\)/, () => ingestStyled("skynet", "", line.text, line.styles ?? []), {
  name: "nf-comms-skynet",
});

watchMessage("Comm.Channel.List", () => mountTabs());

function startsWithCommand(text: string, command: string): boolean {
  return text === command || text.startsWith(`${command} `);
}

function speak(text: string): void {
  const commandText = text.trim();
  if (commandText === "") return;

  if (isAuctionCommand(commandText)) {
    session.send(commandText);
    return;
  }

  if (selected === "auction") {
    session.send(`auction ${commandText}`);
    return;
  }

  if (selected === "system" || selected === "skynet") {
    session.send(commandText);
    return;
  }

  const channel = selected === ALL ? "gossip" : selected;
  const configured = configuredCommand(channel);
  session.send(startsWithCommand(commandText, configured)
    ? commandText
    : `${configured} ${commandText}`);
}

export function open(): void {
  const parent = session.panes.get("Map") ?? session.panes.get("Atlas") ?? session.mainPane;

  // `terminal` is immutable after a pane is created. Replace the old
  // widgets-only Comms pane when this version first loads.
  const existing = session.panes.get(PANE);
  if (existing?.kind === "widgets") existing.close();

  pane = parent.split("bottom", {
    name: PANE,
    height: 380,
    terminal: true,
    fontSize: chatFontSize,
    input: {
      placeholder: "message or channel command…",
      onSubmit: speak,
    },
  });

  mountTabs();

  // A retained terminal already owns its scrollback. Only a newly created
  // terminal needs the messages accumulated in this module's ring buffer.
  if (pane.created) replay();
}

export function close(): void {
  channelMenuOpen = false;
  removeWidget(TABS_WIDGET);
  pane?.close();
  session.panes.get(PANE)?.close();
  pane = null;
}
