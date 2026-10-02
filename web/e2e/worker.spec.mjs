import { readFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';
import { test, expect } from '@playwright/test';
import {
  emit, emitText, mudState, openWorker, postWorker, resetFixture,
  setAutoGreeting, waitForCommand, waitForOpen, workerState,
} from './support.mjs';

const profileSend = JSON.parse(readFileSync(
  new URL('../../test_fixtures/connection_profile_send.json', import.meta.url), 'utf8',
));

test.beforeEach(async ({ request }) => resetFixture(request));

test('separate worker WASM boots under CSP without unsafe-eval', async ({ page, request }) => {
  const response = await page.goto('/e2e/worker.html');
  const csp = response.headers()['content-security-policy'];
  expect(csp).toContain("'wasm-unsafe-eval'");
  expect(csp).not.toContain("'unsafe-eval'");

  await openWorker(page, 'csp', { script: 'function onLine(line, api) { api.send(`seen:${line}`); }' });
  await waitForOpen(request, 'csp');
  await waitForCommand(request, 'csp', 'seen:Welcome csp');
  await expect.poll(async () => (await workerState(page, 'csp')).frames.some(
    (frame) => frame.magic === 'SMW7' && frame.connection === 2,
  )).toBeTruthy();
  const state = await workerState(page, 'csp');
  expect(state.fatals).toEqual([]);
  expect(state.errors).toEqual([]);
});

test('profile scripts can create and control terminal panes', async ({ page, request }) => {
  await openWorker(page, 'panes', {}, {
    profileAutomation: {
      aliases: [],
      triggers: [],
      script: [
        'const chat = api.session.mainPane.split("right", { name: "Chat", width: 280 });',
        'chat.echo("pane-start");',
        'const alerts = chat.addTab({ name: "Alerts", selected: true });',
        'alerts.echo("alert-start");',
        'function onLine(line) {',
        '  if (line !== "pane-control") return;',
        '  if (!chat.isHidden) chat.hide();',
        '  chat.show();',
        '  alerts.clear();',
        '  alerts.close();',
        '}',
      ].join('\n'),
    },
  });
  await waitForOpen(request, 'panes');
  await expect.poll(async () => (await workerState(page, 'panes')).panes.length).toBeGreaterThanOrEqual(4);

  const startup = (await workerState(page, 'panes')).panes.slice(0, 4);
  expect(startup.map(({ action, name, placement, direction, selected, text }) => ({
    action, name, placement, direction, selected, text,
  }))).toEqual([
    { action: 'opened', name: 'Chat', placement: 'split', direction: 'right', selected: undefined, text: undefined },
    { action: 'echo', name: undefined, placement: undefined, direction: undefined, selected: undefined, text: 'pane-start' },
    { action: 'opened', name: 'Alerts', placement: 'tab', direction: undefined, selected: true, text: undefined },
    { action: 'echo', name: undefined, placement: undefined, direction: undefined, selected: undefined, text: 'alert-start' },
  ]);

  await emit(request, 'panes', 'pane-control\r\n');
  await expect.poll(async () => (await workerState(page, 'panes')).panes.length).toBeGreaterThanOrEqual(8);
  const controls = (await workerState(page, 'panes')).panes.slice(4, 8);
  expect(controls.map(({ action, hidden }) => ({ action, hidden }))).toEqual([
    { action: 'updated', hidden: true },
    { action: 'updated', hidden: false },
    { action: 'clear', hidden: undefined },
    { action: 'closed', hidden: undefined },
  ]);
  const state = await workerState(page, 'panes');
  expect(state.fatals).toEqual([]);
  expect(state.errors).toEqual([]);
});

test('plaintext aliases, triggers, and synchronous scripts preserve command order', async ({ page, request }) => {
  await openWorker(page, 'order', {
    aliases: [{ pattern: '^go (.+)$', command: 'walk $1' }],
    triggers: [{ pattern: '^PROMPT (.+)$', command: 'say $1' }],
    script: [
      'function onInput(text, api) {',
      '  if (text === "special") { api.send("script-input"); return true; }',
      '  return false;',
      '}',
      'function onLine(line, api) {',
      '  if (line.startsWith("PROMPT ")) api.send("script-line");',
      '}',
    ].join('\n'),
  });
  await waitForOpen(request, 'order');
  await postWorker(page, 'order', { kind: 'input', text: 'go north' });
  await postWorker(page, 'order', { kind: 'input', text: 'special' });
  await emit(request, 'order', 'PROMPT ready\r\n');
  await waitForCommand(request, 'order', 'script-line');
  const commands = (await mudState(request, 'order')).buffers
    .map((buffer) => buffer.toString('utf8'))
    .filter((value) => /walk north|script-input|say ready|script-line/.test(value));
  expect(commands).toEqual([
    expect.stringContaining('walk north'),
    expect.stringContaining('script-input'),
    expect.stringContaining('say ready'),
    expect.stringContaining('script-line'),
  ]);
});

test('typed interception precedes aliases, while startup and masked sends bypass it', async ({ page, request }) => {
  await openWorker(page, 'input-boundary', {
    aliases: [
      { pattern: '^startup$', command: 'alias-startup' },
      { pattern: '^go$', command: 'alias-go' },
      { pattern: '^secret$', command: 'leaked-alias' },
    ],
    script: [
      'function onInput(text, api) {',
      '  if (text === "startup") api.send("leaked-startup");',
      '  if (text === "go") api.send("submit-go");',
      '  if (text === "secret") { api.send("leaked-script"); return true; }',
      '  return false;',
      '}',
    ].join('\n'),
  }, { sendOnConnect: 'startup' });
  await waitForCommand(request, 'input-boundary', 'alias-startup');
  await postWorker(page, 'input-boundary', { kind: 'input', text: 'go', masked: false });
  await postWorker(page, 'input-boundary', { kind: 'input', text: 'secret', masked: true });
  await waitForCommand(request, 'input-boundary', 'secret');
  const commands = (await mudState(request, 'input-boundary')).buffers
    .map((buffer) => buffer.toString('utf8').trim());
  expect(commands).toEqual(['alias-startup', 'submit-go', 'alias-go', 'secret']);
});

test('command separators and verbatim prefix follow native syntax and update live', async ({ page, request }) => {
  await openWorker(page, 'syntax', {
    aliases: [{ pattern: '^n$', command: 'north' }],
  }, { commandSyntax: { separator: ';;', raw_prefix: '!' } });
  await waitForOpen(request, 'syntax');
  // The server handshake can finish before the worker processes its onopen.
  await expect.poll(async () => (await workerState(page, 'syntax')).frames.at(-1)?.connection, {
    message: 'syntax worker must report Connected before input',
  }).toBe(2);
  await postWorker(page, 'syntax', { kind: 'input', text: 'a;b;;n' });
  await postWorker(page, 'syntax', { kind: 'input', text: '!n;;n' });
  await postWorker(page, 'syntax', { kind: 'input', text: '=n;;n' });
  await postWorker(page, 'syntax', {
    kind: 'set-command-syntax',
    command_syntax: JSON.stringify({ separator: ';', raw_prefix: '\\\\' }),
  });
  await postWorker(page, 'syntax', { kind: 'input', text: 'e;w' });
  await waitForCommand(request, 'syntax', 'w');
  const commands = (await mudState(request, 'syntax')).buffers.map((buffer) => buffer.toString('utf8').trim());
  expect(commands).toEqual(['a;b', 'north', 'n;;n', 'n;;n', 'e', 'w']);
});

test('script sends re-enter separators and aliases without re-entering typed handlers', async ({ page, request }) => {
  await openWorker(page, 'script-send', {
    aliases: [{ pattern: '^n$', command: 'north' }],
    triggers: [{ pattern: '^READY$', command: 'n;look' }],
    script: 'function onInput(text, api) { if (text === "go") { api.send("n;look"); return true; } }',
  });
  await waitForOpen(request, 'script-send');
  await postWorker(page, 'script-send', { kind: 'input', text: 'go' });
  await emit(request, 'script-send', 'READY\r\n');
  await waitForCommand(request, 'script-send', 'look');
  await expect.poll(async () => (await mudState(request, 'script-send')).buffers.length).toBe(4);
  const commands = (await mudState(request, 'script-send')).buffers
    .map((buffer) => buffer.toString('utf8').trim());
  expect(commands).toEqual(['north', 'look', 'north', 'look']);
});

test('successful commands echo on the prompt while masked and profile sends stay secret', async ({ page, request }) => {
  await setAutoGreeting(request, 'echo', false);
  await openWorker(page, 'echo', {
    aliases: [{ pattern: '^password (.+)$', command: 'leak $1' }],
  }, {
    sendOnConnect: 'password profile-secret',
    sendRedactions: ['profile-secret'],
  });
  await waitForOpen(request, 'echo');
  await page.evaluate(() => {
    const session = window.testSessions.echo;
    const original = session.worker.onmessage;
    session.payloads = [];
    session.worker.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        session.payloads.push(new TextDecoder().decode(event.data));
      }
      original(event);
    };
  });
  await emit(request, 'echo', 'Prompt> ');
  await waitForCommand(request, 'echo', 'password profile-secret');
  await expect.poll(async () => page.evaluate(() =>
    window.testSessions.echo.payloads.some((payload) => payload.includes('Prompt> password ********')),
  )).toBeTruthy();
  expect((await mudState(request, 'echo')).buffers.some((buffer) =>
    buffer.toString('utf8').includes('leak profile-secret'))).toBeFalsy();
  await postWorker(page, 'echo', { kind: 'input', text: 'look' });
  await waitForCommand(request, 'echo', 'look');
  await expect.poll(async () => page.evaluate(() =>
    window.testSessions.echo.payloads.some((payload) => payload.includes('look')),
  )).toBeTruthy();
  await postWorker(page, 'echo', { kind: 'input', text: 'typed-secret', masked: true });
  await waitForCommand(request, 'echo', 'typed-secret');
  await expect.poll(async () => page.evaluate(() =>
    window.testSessions.echo.payloads.some((payload) => payload.includes('********')),
  )).toBeTruthy();
  const payloads = await page.evaluate(() => window.testSessions.echo.payloads);
  expect(payloads.join(' ')).not.toContain('profile-secret');
  expect(payloads.join(' ')).not.toContain('typed-secret');
});

test('GA leaves the terminal prompt live until a successful command joins it', async ({ page, request }) => {
  await setAutoGreeting(request, 'ga-prompt', false);
  await openWorker(page, 'ga-prompt', {
    triggers: [{ pattern: '^next$', command: 'source-only' }],
  });
  await waitForOpen(request, 'ga-prompt');
  await page.evaluate(() => {
    const session = window.testSessions['ga-prompt'];
    const original = session.worker.onmessage;
    session.payloads = [];
    session.worker.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        session.payloads.push(new TextDecoder().decode(event.data));
      }
      original(event);
    };
  });
  await emit(request, 'ga-prompt', Buffer.concat([Buffer.from('Prompt> '), Buffer.from([255, 249])]));
  await expect.poll(async () => (await workerState(page, 'ga-prompt')).frames.some(
    (frame) => frame.committed === 0 && (frame.flags & 4) !== 0,
  )).toBeTruthy();
  await postWorker(page, 'ga-prompt', { kind: 'input', text: 'look' });
  await waitForCommand(request, 'ga-prompt', 'look');
  await expect.poll(async () => page.evaluate(() =>
    window.testSessions['ga-prompt'].payloads.some((payload) => payload.includes('Prompt> look')),
  )).toBeTruthy();
  await emit(request, 'ga-prompt', 'next\r\n');
  await waitForCommand(request, 'ga-prompt', 'source-only');
  await expect.poll(async () => (await workerState(page, 'ga-prompt')).frames.some(
    (frame) => frame.committed >= 3,
  )).toBeTruthy();
  const payloads = await page.evaluate(() => window.testSessions['ga-prompt'].payloads);
  expect(payloads.join(' ')).not.toContain('Prompt> next');
});

test('server-wide and selected-profile scopes compose with profile input precedence', async ({ page, request }) => {
  await openWorker(page, 'layers', {
    aliases: [{ pattern: '^go$', command: 'default-go' }],
    triggers: [{ pattern: '^PING$', command: 'default-trigger' }],
    script: 'function onLine(line, api) { if (line === "PING") api.send("default-script"); }\nfunction onInput(text, api) { if (text === "special") { api.send("default-input"); return true; } }',
  }, {
    profileAutomation: {
      aliases: [{ pattern: '^go$', command: 'profile-go' }],
      triggers: [{ pattern: '^PING$', command: 'profile-trigger' }],
      script: 'function onLine(line, api) { if (line === "PING") api.send("profile-script"); }\nfunction onInput(text, api) { if (text === "special") { api.send("profile-input"); return true; } }',
    },
  });
  await waitForOpen(request, 'layers');
  await postWorker(page, 'layers', { kind: 'input', text: 'go' });
  await postWorker(page, 'layers', { kind: 'input', text: 'special' });
  await emit(request, 'layers', 'PING\r\n');
  await waitForCommand(request, 'layers', 'profile-script');
  const commands = (await mudState(request, 'layers')).buffers
    .map((buffer) => buffer.toString('utf8'))
    .filter((value) => /go|input|trigger|script/.test(value));
  expect(commands).toEqual([
    expect.stringContaining('profile-go'),
    expect.stringContaining('profile-input'),
    expect.stringContaining('default-trigger'),
    expect.stringContaining('profile-trigger'),
    expect.stringContaining('default-script'),
    expect.stringContaining('profile-script'),
  ]);
});

test('shared profile-send contract waits for network display and orders trigger first', async ({ page, request }) => {
  await setAutoGreeting(request, 'profile-order', false);
  await openWorker(page, 'profile-order', {
    triggers: [{ pattern: profileSend.trigger_pattern, command: profileSend.trigger_command }],
  }, { sendOnConnect: profileSend.profile_command });
  await waitForOpen(request, 'profile-order');
  await emit(request, 'profile-order', profileSend.empty_packet);
  await page.waitForTimeout(150);
  expect((await mudState(request, 'profile-order')).buffers).toEqual([]);

  await emit(request, 'profile-order', profileSend.greeting);
  await waitForCommand(request, 'profile-order', profileSend.profile_command);
  const commands = (await mudState(request, 'profile-order')).buffers
    .map((buffer) => buffer.toString('utf8'));
  expect(commands).toEqual(profileSend.expected_outbound);
});

test('startup syntax errors fail one worker before it opens a socket', async ({ page, request }) => {
  await openWorker(page, 'syntax', { script: 'function onLine( {' });
  await expect.poll(async () => (await workerState(page, 'syntax')).fatals.length).toBe(1);
  expect((await workerState(page, 'syntax')).fatals[0]).toMatch(/SyntaxError/);
  expect((await mudState(request, 'syntax')).opens).toBe(0);
});

test('Promise handlers cannot send after returning to the event loop', async ({ page, request }) => {
  await openWorker(page, 'async', {
    script: 'async function onLine(line, api) { await Promise.resolve(); api.send("late-send"); }',
  });
  await waitForOpen(request, 'async');
  await emit(request, 'async', 'another line\r\n');
  await expect.poll(async () => (await workerState(page, 'async')).frames.length).toBeGreaterThan(0);
  await page.waitForTimeout(100);
  const output = (await mudState(request, 'async')).buffers.map((buffer) => buffer.toString('utf8'));
  expect(output.join('')).not.toContain('late-send');
});

test('sessions isolate scripts and a disconnected worker retains its script state', async ({ page, request }) => {
  await openWorker(page, 'first', { script: 'let count = 0; function onLine(line, api) { api.send(`first:${++count}`); }' });
  await openWorker(page, 'second', { script: 'let count = 0; function onLine(line, api) { api.send(`second:${++count}`); }' });
  await waitForOpen(request, 'first');
  await waitForOpen(request, 'second');
  await waitForCommand(request, 'first', 'first:1');
  await waitForCommand(request, 'second', 'second:1');
  await postWorker(page, 'first', { kind: 'disconnect' });
  await expect.poll(async () => (await mudState(request, 'first')).active).toBe(0);
  expect((await mudState(request, 'second')).active).toBe(1);
  await postWorker(page, 'first', { kind: 'connect', endpoint: 'wss://127.0.0.1:9443/mud/first' });
  await waitForOpen(request, 'first', 2);
  await waitForCommand(request, 'first', 'first:2');
  expect((await mudState(request, 'second')).opens).toBe(1);
});

test('local TypeScript package compiles, persists, and runs synchronously in its session worker', async ({ page, request }) => {
  await page.goto('/e2e/worker.html');
  const installed = await page.evaluate(() => new Promise((resolve) => {
    const worker = new Worker('/package-install-worker.js', { type: 'module' });
    worker.onmessage = ({ data }) => { worker.terminate(); resolve(data); };
    worker.onerror = (event) => { worker.terminate(); resolve({ error: event.message }); };
    worker.postMessage({
      name: 'typescript-test',
      files: [
        ['smudgy.package.json', JSON.stringify({ version: '1.0.0', target: 'web', entry: 'index.ts' })],
        ['index.ts', 'import { createAlias, createTrigger, echo, send } from "smudgy:core"; import { prefix } from "./prefix"; createAlias(/^pkg (?<word>.+)$/, ({ word }: { word: string }) => { send(`${prefix}:${word}`); }); createTrigger(/^Server (?<word>.+)$/, ({ word }: { word: string }) => { echo(`trigger:${word}`); });'],
        ['prefix.ts', 'export const prefix: string = "package";'],
      ],
    });
  }));
  expect(installed).toEqual({ name: 'typescript-test' });
  await openWorker(page, 'package', {}, { packages: ['typescript-test'] });
  await waitForOpen(request, 'package');
  await postWorker(page, 'package', { kind: 'input', text: 'pkg hello' });
  await waitForCommand(request, 'package', 'package:hello');
  await emit(request, 'package', 'Server ready\r\n');
  await expect.poll(async () => (await workerState(page, 'package')).fatals).toEqual([]);

  // A warm worker must still preload packages before dispatching `start` to Rust.
  await openWorker(page, 'package-late', {}, {
    packages: ['typescript-test'], startAfterBoot: true,
  });
  await waitForOpen(request, 'package-late');
  await postWorker(page, 'package-late', { kind: 'input', text: 'pkg late' });
  await waitForCommand(request, 'package-late', 'package:late');
});

test('browser installer rejects legacy native-only manifests and unsupported Smudgy imports', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  const attempt = (manifest, source) => page.evaluate(({ manifest, source }) => new Promise((resolve) => {
    const worker = new Worker('/package-install-worker.js', { type: 'module' });
    worker.onmessage = ({ data }) => { worker.terminate(); resolve(data); };
    worker.onerror = (event) => { worker.terminate(); resolve({ error: event.message }); };
    worker.postMessage({
      name: 'unsupported',
      files: [['smudgy.package.json', JSON.stringify(manifest)], ['index.ts', source]],
    });
  }), { manifest, source });
  expect((await attempt({ version: '1.0.0', entry: 'index.ts' }, 'export {};')).error)
    .toContain('must declare target');
  expect((await attempt({ version: '1.0.0', target: 'web', entry: 'index.ts', min_smudgy_version: '9.0.0' },
    'export {};')).error).toContain('cannot verify min_smudgy_version');
  expect((await attempt({ version: '1.0.0', target: 'web', entry: 'index.ts' },
    'import { getDataDir } from "smudgy:core"; getDataDir();')).error)
    .toContain('No matching export');
  expect((await attempt({ version: '1.0.0', target: 'web', entry: 'index.ts' },
    'void import("./late.ts");')).error)
    .toContain('Dynamic imports are not supported');
  expect(await attempt({ version: '1.0.0', target: 'web', entry: 'index.ts' },
    'export const first = 1;')).toEqual({ name: 'unsupported' });
  expect((await attempt({ version: '1.0.0', target: 'web', entry: 'index.ts' },
    'export const first = 2;')).error)
    .toContain('already installed with different contents');
});

test('Telnet negotiation uses binary replies and terminal scrollback stays bounded', async ({ page, request }) => {
  await openWorker(page, 'telnet');
  await waitForOpen(request, 'telnet');
  await emit(request, 'telnet', Buffer.from([255, 253, 31])); // IAC DO NAWS
  await expect.poll(async () => (await mudState(request, 'telnet')).buffers.some(
    (buffer) => buffer.includes(Buffer.from([255, 251, 31])), // IAC WILL NAWS
  )).toBeTruthy();

  const lines = Array.from({ length: 100_005 }, (_, index) => `line ${index}\r\n`).join('');
  await emit(request, 'telnet', lines);
  await expect.poll(async () => (await workerState(page, 'telnet')).frames.some(
    (frame) => frame.committed === 100_000,
  ), { timeout: 25_000 }).toBeTruthy();
  const frames = (await workerState(page, 'telnet')).frames;
  expect(frames.every((frame) => frame.maxLines === 100_000 && frame.committed <= 100_000)).toBeTruthy();
  expect(frames.some((frame) => frame.bytes > 100_000)).toBeTruthy();
});

test('live scrollback changes keep the worker cap and reset its presentation after trimming', async ({ page, request }) => {
  await openWorker(page, 'history', {}, { maxLines: 120 });
  await waitForOpen(request, 'history');
  await emit(request, 'history', Array.from({ length: 150 }, (_, index) => `line ${index}\r\n`).join(''));
  await expect.poll(async () => (await workerState(page, 'history')).frames.some(
    (frame) => frame.maxLines === 120 && frame.committed === 120,
  )).toBeTruthy();

  await postWorker(page, 'history', { kind: 'set-scrollback', max_lines: 200 });
  await expect.poll(async () => (await workerState(page, 'history')).frames.some(
    (frame) => frame.maxLines === 200,
  )).toBeTruthy();
  await emit(request, 'history', Array.from({ length: 30 }, (_, index) => `more ${index}\r\n`).join(''));
  await expect.poll(async () => (await workerState(page, 'history')).frames.some(
    (frame) => frame.maxLines === 200 && frame.committed === 150,
  )).toBeTruthy();

  await postWorker(page, 'history', { kind: 'set-scrollback', max_lines: 100 });
  await expect.poll(async () => (await workerState(page, 'history')).frames.some(
    (frame) => frame.maxLines === 100 && frame.committed === 100 && (frame.flags & 2) !== 0,
  )).toBeTruthy();
  expect((await workerState(page, 'history')).fatals).toEqual([]);
});

test('MCCP2 keeps Telnet replies and script callbacks ordered across WSS frames', async ({ page, request }) => {
  await openWorker(page, 'mccp', {
    script: 'function onLine(line, api) { if (line.startsWith("Compressed")) api.send(`seen:${line}`); }',
  });
  await waitForOpen(request, 'mccp');
  await emit(request, 'mccp', Buffer.from([255, 251, 86])); // WILL MCCP2
  await expect.poll(async () => (await mudState(request, 'mccp')).buffers.some(
    (buffer) => buffer.includes(Buffer.from([255, 253, 86])), // DO MCCP2
  )).toBeTruthy();
  await emit(request, 'mccp', Buffer.from([255, 250, 86, 255]));
  const compressed = deflateSync(Buffer.from('Compressed line\r\n'));
  await emit(request, 'mccp', Buffer.concat([Buffer.from([240]), compressed.subarray(0, 4)]));
  await emit(request, 'mccp', compressed.subarray(4));
  await waitForCommand(request, 'mccp', 'seen:Compressed line');
  await emit(request, 'mccp', 'Plain tail\r\n');
  await expect.poll(async () => (await workerState(page, 'mccp')).frames.some(
    (frame) => frame.committed >= 3,
  )).toBeTruthy();
});

test('corrupt MCCP2 closes only the affected worker socket', async ({ page, request }) => {
  await openWorker(page, 'corrupt-mccp');
  await openWorker(page, 'other-mccp');
  await waitForOpen(request, 'corrupt-mccp');
  await waitForOpen(request, 'other-mccp');
  await emit(request, 'corrupt-mccp', Buffer.from([255, 251, 86, 255, 250, 86, 255, 240]));
  await emit(request, 'corrupt-mccp', Buffer.from('not zlib'));
  await expect.poll(async () => (await mudState(request, 'corrupt-mccp')).active).toBe(0);
  expect((await mudState(request, 'other-mccp')).active).toBe(1);
});

test('configured charset transcodes binary Telnet input and outbound commands', async ({ page, request }) => {
  await openWorker(page, 'charset', {
    script: 'function onLine(line, api) { if (line === "café") api.send("ÿ"); }',
  }, { encoding: 'windows-1252' });
  await waitForOpen(request, 'charset');
  await emit(request, 'charset', Buffer.from([0x63, 0x61, 0x66, 0xe9, 0x0d, 0x0a]));
  await expect.poll(async () => (await mudState(request, 'charset')).buffers.some(
    (buffer) => buffer.equals(Buffer.from([255, 255, 13, 10])),
  )).toBeTruthy();
});

test('presentation remains one frame in flight until acknowledged', async ({ page, request }) => {
  await openWorker(page, 'ack', {}, { autoAck: false });
  await waitForOpen(request, 'ack');
  await expect.poll(async () => (await workerState(page, 'ack')).frames.length).toBe(1);
  const first = (await workerState(page, 'ack')).frames[0];
  await emit(request, 'ack', 'later one\r\nlater two\r\n');
  await page.waitForTimeout(80); // Beyond the worker's 8 ms coalescing timer.
  expect((await workerState(page, 'ack')).frames).toHaveLength(1);
  await postWorker(page, 'ack', { kind: 'ack', sequence: first.sequence });
  await expect.poll(async () => (await workerState(page, 'ack')).frames.length).toBe(2);
  const second = (await workerState(page, 'ack')).frames[1];
  expect(second.sequence).not.toBe(first.sequence);
  expect(second.revision).toBeGreaterThan(first.revision);
});

test('a non-binary WebSocket frame disconnects only its own session', async ({ page, request }) => {
  await openWorker(page, 'bad-frame');
  await openWorker(page, 'healthy');
  await waitForOpen(request, 'bad-frame');
  await waitForOpen(request, 'healthy');
  await emitText(request, 'bad-frame', 'text is not Telnet bytes');
  await expect.poll(async () => (await mudState(request, 'bad-frame')).active).toBe(0);
  await expect.poll(async () => (await workerState(page, 'bad-frame')).frames.some(
    (frame) => frame.connection === 0,
  )).toBeTruthy();
  expect((await mudState(request, 'healthy')).active).toBe(1);
});
