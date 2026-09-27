import { test, expect } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import {
  catalog, endpoint, mudState, pixel, profile, resetFixture, seedCatalog, server,
  setAutoGreeting, seedPassword, seedLastWindow, waitForCommand, waitForOpen, waitForPurple,
  dropConnections, emit, lastWindow,
} from './support.mjs';

test.beforeEach(async ({ request }) => resetFixture(request));

// Coordinates are intentionally tied to the fixed 1100×900 viewport in
// playwright.config.mjs. They exercise iced's real canvas hit testing.
async function openWindow(page, servers = []) {
  if (servers.length) await seedCatalog(page, servers);
  await page.goto('/');
  await expect(page.locator('canvas')).toHaveCount(1);
  await waitForPurple(page, 500, 583); // Empty-state primary action.
}

async function openConnect(page) {
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165); // Shared modal title bar.
}

async function browserSettings(page) {
  return page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      return await new Promise((resolve, reject) => {
        const request = db.transaction('browser-settings').objectStore('browser-settings').get('current');
        request.onsuccess = () => resolve(request.result ? JSON.parse(request.result) : null);
        request.onerror = () => reject(request.error);
      });
    } finally {
      db.close();
    }
  });
}

test('empty state and shared Connect modal render with Smudgy accent', async ({ page }) => {
  await openWindow(page, [server('alpha', [profile('main')])]);
  const response = await page.request.get('/');
  expect(response.headers()['content-security-policy']).not.toContain("'unsafe-eval'");
  expect(response.headers()['content-security-policy']).toContain('frame-src http://127.0.0.1:8095');
  const runtime = await page.request.get('http://127.0.0.1:8095/runtime-frame.html');
  expect(runtime.headers()['content-security-policy'])
    .toContain('frame-ancestors http://127.0.0.1:8094');
  expect(await runtime.text()).toContain('content="http://127.0.0.1:8094"');
  await openConnect(page);
  expect((await pixel(page, 500, 165))[2]).toBeGreaterThan(70);
  await page.mouse.click(1050, 800); // Overlay outside the modal closes it.
  await waitForPurple(page, 500, 583);
});

test('shared Settings appearance controls persist and restore in the browser window', async ({ page }) => {
  await openWindow(page);
  await page.mouse.click(150, 20); // Toolbar Settings.
  await waitForPurple(page, 500, 215); // Shared modal title bar.
  await page.mouse.click(253, 343); // Disable blinking text.
  await page.mouse.click(290, 410); // Font size input.
  await page.keyboard.press('End');
  await page.keyboard.press('Backspace');
  await page.keyboard.press('Backspace');
  await page.keyboard.type('22');
  await page.keyboard.press('Enter');
  await page.mouse.click(290, 480); // Line wrap width.
  await page.keyboard.type('80');
  await page.keyboard.press('Enter');
  await page.mouse.click(290, 550); // Link tooltip delay.
  await page.keyboard.press('End');
  await page.keyboard.press('Backspace');
  await page.keyboard.type('250');
  await page.keyboard.press('Enter');
  await expect.poll(() => browserSettings(page)).toEqual({
    schema: 4,
    appearance: { font_size: 22, bold_mode: 'bold_and_bright', disable_blink: true, line_length: 80, link_tooltip_delay_ms: 250, theme_extended_colors: false, hide_pane_headers: false, scrollback_lines: 100000 },
    input: {
      command_input_behavior: 'select_all',
      mask_input_on_server_echo: true,
      reconnect_on_send_error: true,
      history_case_sensitive_match: false,
      max_history: 1000,
    },
    command_syntax: { separator: ';', raw_prefix: '\\\\' },
    theme: 'Smudgy',
  });
  await page.mouse.click(430, 456); // Blur the input before comparing pixels.
  const inputBeforeReload = await page.screenshot({ clip: { x: 245, y: 397, width: 120, height: 28 } });
  const wrapBeforeReload = await page.screenshot({ clip: { x: 245, y: 467, width: 120, height: 28 } });
  const tooltipBeforeReload = await page.screenshot({ clip: { x: 245, y: 539, width: 140, height: 28 } });
  await page.reload();
  await waitForPurple(page, 500, 583);
  await page.mouse.click(150, 20);
  await waitForPurple(page, 500, 215);
  await expect.poll(async () =>
    (await page.screenshot({ clip: { x: 245, y: 397, width: 120, height: 28 } }))
      .equals(inputBeforeReload)).toBe(true);
  expect((await page.screenshot({ clip: { x: 245, y: 467, width: 120, height: 28 } }))
    .equals(wrapBeforeReload)).toBe(true);
  expect((await page.screenshot({ clip: { x: 245, y: 539, width: 140, height: 28 } }))
    .equals(tooltipBeforeReload)).toBe(true);
});

test('shared input preferences persist beside appearance and survive reload', async ({ page }) => {
  await openWindow(page);
  await page.mouse.click(150, 20);
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await page.mouse.click(253, 394); // Keep input visible during server ECHO suppression.
  await page.mouse.click(253, 532); // Case-sensitive history search.
  await page.mouse.click(290, 614); // History limit.
  await page.keyboard.press('End');
  for (let i = 0; i < 4; i += 1) await page.keyboard.press('Backspace');
  await page.keyboard.type('50');
  await page.keyboard.press('Enter');
  await expect.poll(() => browserSettings(page)).toMatchObject({
    schema: 4,
    appearance: { font_size: 16, bold_mode: 'bold_and_bright', disable_blink: false },
    input: {
      command_input_behavior: 'select_all',
      mask_input_on_server_echo: false,
      history_case_sensitive_match: true,
      max_history: 50,
    },
  });
  await page.mouse.click(700, 390); // Blur the committed field before comparing pixels.
  const beforeReload = await page.screenshot({ clip: { x: 245, y: 597, width: 120, height: 32 } });
  await page.reload();
  await waitForPurple(page, 500, 583);
  await page.mouse.click(150, 20);
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await expect.poll(async () => (await page.screenshot({ clip: { x: 245, y: 597, width: 120, height: 32 } }))
    .equals(beforeReload)).toBe(true);
});

test('shared display options persist and hide pane headers only with collapsed toolbar', async ({ page, request }) => {
  await openWindow(page, [server('display', [profile('main')])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'display');
  await page.mouse.click(230, 20); // Settings follows Layouts with a live session.
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await page.mouse.wheel(0, -300); // Account for the shared syntax controls below Display.
  await page.mouse.click(253, 254); // Adjust extended colors to the theme.
  await page.mouse.click(290, 335); // Shrink scrollback in this live session.
  await page.keyboard.press('End');
  for (let i = 0; i < 6; i += 1) await page.keyboard.press('Backspace');
  await page.keyboard.type('150');
  await page.keyboard.press('Enter');
  await page.mouse.click(253, 388); // Hide headers with collapsed toolbar.
  await expect.poll(() => browserSettings(page)).toMatchObject({
    appearance: { theme_extended_colors: true, hide_pane_headers: true, scrollback_lines: 150 },
  });
  await page.mouse.click(825, 670); // Close Settings.
  const title = await page.screenshot({ clip: { x: 10, y: 48, width: 100, height: 28 } });
  await page.mouse.click(20, 20); // Collapse toolbar: hide pane title bar.
  expect((await page.screenshot({ clip: { x: 10, y: 48, width: 100, height: 28 } }))
    .equals(title)).toBe(false);
  await page.mouse.click(20, 20); // Expand toolbar: reveal title bar again.
  await expect.poll(async () => (await page.screenshot({ clip: { x: 10, y: 48, width: 100, height: 28 } }))
    .equals(title)).toBe(true);
});

test('shared command syntax persists and updates an active session worker', async ({ page, request }) => {
  await openWindow(page, [server('syntax-ui', [profile('main', {
    aliases: [{ pattern: '^x$', command: 'alias-x' }],
  })])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'syntax-ui');
  await page.mouse.click(235, 20); // Settings follows Layouts in a live window.
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await page.mouse.wheel(0, -120);
  await page.mouse.click(280, 318); // Separator.
  await page.keyboard.press('End');
  await page.keyboard.press('Backspace');
  await page.keyboard.type(';;');
  await page.mouse.click(280, 389); // Verbatim prefix.
  await page.keyboard.press('End');
  await page.keyboard.press('Backspace');
  await page.keyboard.press('Backspace');
  await page.keyboard.type('!');
  await expect.poll(() => browserSettings(page)).toMatchObject({
    schema: 4,
    command_syntax: { separator: ';;', raw_prefix: '!' },
  });
  await page.mouse.click(825, 670); // Close Settings.
  await page.mouse.click(300, 878);
  await page.keyboard.type('a;b;;x');
  await page.keyboard.press('Enter');
  await page.keyboard.type('!x;;x');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'syntax-ui', 'x;;x');
  const commands = (await mudState(request, 'syntax-ui')).buffers.map((buffer) => buffer.toString('utf8').trim());
  expect(commands).toEqual(['a;b', 'alias-x', 'x;;x']);

  await page.reload();
  await waitForPurple(page, 500, 583);
  expect(await browserSettings(page)).toMatchObject({
    schema: 4,
    command_syntax: { separator: ';;', raw_prefix: '!' },
  });
});

test('appearance-only settings migrate without losing terminal appearance', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('browser-settings');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise((resolve, reject) => {
      const transaction = db.transaction('browser-settings', 'readwrite');
      transaction.objectStore('browser-settings').put(JSON.stringify({
        schema: 1,
        appearance: { font_size: 23, bold_mode: 'bright', disable_blink: true, line_length: 90 },
      }), 'current');
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
    });
    db.close();
  });
  await page.goto('/');
  await waitForPurple(page, 500, 583);
  await page.mouse.click(150, 20);
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await page.mouse.click(253, 394); // First edit upgrades the entire snapshot.
  await expect.poll(() => browserSettings(page)).toEqual({
    schema: 4,
    appearance: { font_size: 23, bold_mode: 'bright', disable_blink: true, line_length: 90, link_tooltip_delay_ms: 0, theme_extended_colors: false, hide_pane_headers: false, scrollback_lines: 100000 },
    input: {
      command_input_behavior: 'select_all',
      mask_input_on_server_echo: false,
      reconnect_on_send_error: true,
      history_case_sensitive_match: false,
      max_history: 1000,
    },
    command_syntax: { separator: ';', raw_prefix: '\\\\' },
    theme: 'Smudgy',
  });
});

test('schema-2 input settings migrate to command syntax without losing saved values', async ({ page }) => {
  await openWindow(page);
  await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise((resolve, reject) => {
      const transaction = db.transaction('browser-settings', 'readwrite');
      transaction.objectStore('browser-settings').put(JSON.stringify({
        schema: 2,
        appearance: { font_size: 21, bold_mode: 'bright', disable_blink: true },
        input: {
          command_input_behavior: 'clear', mask_input_on_server_echo: false,
          history_case_sensitive_match: true, max_history: 50,
        },
      }), 'current');
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
    });
    db.close();
  });
  await page.reload();
  await waitForPurple(page, 500, 583);
  await page.mouse.click(150, 20);
  await waitForPurple(page, 500, 215);
  await page.mouse.move(650, 530);
  await page.mouse.wheel(0, 900);
  await page.mouse.wheel(0, -120);
  await page.mouse.click(280, 318);
  await page.keyboard.press('End');
  await page.keyboard.press('Backspace');
  await page.keyboard.type('::');
  await expect.poll(() => browserSettings(page)).toMatchObject({
    schema: 4,
    appearance: { font_size: 21, bold_mode: 'bright', disable_blink: true },
    input: {
      command_input_behavior: 'clear', mask_input_on_server_echo: false,
      history_case_sensitive_match: true, max_history: 50,
    },
    command_syntax: { separator: '::', raw_prefix: '\\\\' },
  });
});

test('server editor persists WSS settings through IndexedDB and reload', async ({ page, request }) => {
  await openWindow(page);
  await openConnect(page);
  await page.mouse.click(250, 718); // + New Server.
  await page.mouse.click(470, 280);
  await page.keyboard.type('persisted');
  await page.mouse.click(470, 345);
  await page.keyboard.type(endpoint('persisted'));
  await page.mouse.click(399, 461); // Save below the optional encoding field.
  await expect.poll(async () => (await catalog(page)).servers.length).toBe(1);
  expect((await catalog(page)).servers[0]).toMatchObject({
    name: 'persisted', endpoint: endpoint('persisted'),
  });
  await page.reload();
  await waitForPurple(page, 500, 583);
  await openConnect(page);
  await page.mouse.click(605, 287); // Create and connect with Default.
  await waitForOpen(request, 'persisted');
  expect((await catalog(page)).servers[0].profiles.map(({ name }) => name)).toEqual(['Default']);
  expect(page.workers()).toHaveLength(1);
});

test('server editor refuses insecure ws URLs', async ({ page }) => {
  await openWindow(page);
  await openConnect(page);
  await page.mouse.click(250, 718);
  await page.mouse.click(470, 280);
  await page.keyboard.type('insecure');
  await page.mouse.click(470, 345);
  await page.keyboard.type('ws://127.0.0.1:9443/mud/insecure');
  await page.mouse.click(399, 461);
  expect((await catalog(page)).servers).toEqual([]);
  expect(page.workers()).toHaveLength(0);
});

test('profile session uses pane input, disconnects, reconnects, and closes', async ({ page, request }) => {
  const saved = profile('main', {
    aliases: [{ pattern: '^look$', command: 'survey' }],
    script: 'function onLine(line, api) { if (line.startsWith("Welcome")) api.send("script-ready"); }',
  });
  saved.send_on_connect = 'login demo';
  await openWindow(page, [server('session', [saved])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401); // IndexedDB profile row has loaded.
  await page.mouse.click(875, 411); // Profile Connect.
  await waitForOpen(request, 'session');
  await waitForCommand(request, 'session', 'script-ready');
  await waitForCommand(request, 'session', 'login demo');
  expect(page.workers()).toHaveLength(1);
  expect(new URL(page.workers()[0].url()).origin).toBe('http://127.0.0.1:8095');
  expect((await mudState(request, 'session')).origins).toEqual(['http://127.0.0.1:8095']);
  const appDatabases = await page.evaluate(async () => (await indexedDB.databases()).map((db) => db.name));
  const runtimeDatabases = await page.workers()[0].evaluate(
    async () => (await indexedDB.databases()).map((db) => db.name),
  );
  expect(appDatabases).toContain('smudgy-web');
  expect(runtimeDatabases).not.toContain('smudgy-web');

  await page.mouse.click(300, 878); // Session input in pane footer.
  await page.keyboard.type('look');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'session', 'survey');
  await expect.poll(async () => (await mudState(request, 'session')).active).toBe(1);

  await dropConnections(request, 'session');
  await expect.poll(async () => (await mudState(request, 'session')).active).toBe(0);
  await page.waitForTimeout(150); // Let iced apply the disconnected worker frame.
  await page.mouse.click(300, 878);
  await page.keyboard.type('say retry');
  await page.keyboard.press('Enter');
  await waitForOpen(request, 'session', 2); // Failed send reconnects, but is never replayed.
  expect((await mudState(request, 'session')).buffers.map((buffer) => buffer.toString('utf8').trim()))
    .not.toContain('say retry');
  await page.keyboard.press('Enter'); // The rescued, selected input retries only on user action.
  await waitForCommand(request, 'session', 'say retry');

  await page.mouse.click(132, 57); // Shared tab-strip Disconnect.
  await expect.poll(async () => (await mudState(request, 'session')).active).toBe(0);
  await page.waitForTimeout(150); // Wait for iced to expose Reconnect in the strip.
  expect(page.workers()).toHaveLength(1); // Disconnected pane retains worker.
  await page.mouse.click(132, 57); // Reconnect from the shared tab strip.
  await waitForOpen(request, 'session', 3);
  await page.mouse.click(181, 57); // Close the main-session tab.
  await expect.poll(() => page.workers().length).toBe(0);
  await expect.poll(async () => (await mudState(request, 'session')).active).toBe(0);
  await waitForPurple(page, 500, 583); // Empty state restored.
});

test('Telnet-masked pane input bypasses browser script and alias callbacks', async ({ page, request }) => {
  const saved = profile('main', {
    aliases: [{ pattern: '^swordfish$', command: 'leaked-alias' }],
    script: 'function onInput(text, api) { if (text === "swordfish") { api.send("leaked-script"); return true; } return false; }',
  });
  await openWindow(page, [server('masked-input', [saved])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'masked-input');
  await emit(request, 'masked-input', Buffer.from([255, 251, 1])); // IAC WILL ECHO.
  await expect.poll(async () => (await mudState(request, 'masked-input')).buffers.some(
    (buffer) => buffer.equals(Buffer.from([255, 253, 1])), // IAC DO ECHO.
  )).toBeTruthy();
  await page.waitForTimeout(150); // Let the worker's ECHO state reach the input widget.

  await page.mouse.click(300, 878);
  await page.keyboard.type('swordfish');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'masked-input', 'swordfish');
  const sent = (await mudState(request, 'masked-input')).buffers
    .map((buffer) => buffer.toString('utf8'));
  expect(sent.join('')).not.toContain('leaked-');
});

test('profile password is read at connect and again at reconnect, never from catalog JSON', async ({ page, request }) => {
  const saved = profile('main');
  saved.send_on_connect = 'login demo\n$PASSWORD';
  await openWindow(page, [server('secret', [saved])]);
  await seedPassword(page, 'secret', 'main', 'first-secret');
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'secret');
  await waitForCommand(request, 'secret', 'first-secret');
  expect(JSON.stringify(await catalog(page))).not.toContain('first-secret');

  await page.mouse.click(132, 57); // Disconnect, retaining the session worker.
  await expect.poll(async () => (await mudState(request, 'secret')).active).toBe(0);
  await page.waitForTimeout(150);
  await seedPassword(page, 'secret', 'main', 'second-secret');
  await page.mouse.click(132, 57);
  await waitForOpen(request, 'secret', 2);
  await waitForCommand(request, 'secret', 'second-secret');
  const sent = (await mudState(request, 'secret')).buffers.map((buffer) => buffer.toString('utf8'));
  expect(sent.filter((value) => value.includes('first-secret'))).toHaveLength(1);
  expect(sent.filter((value) => value.includes('second-secret'))).toHaveLength(1);
});

test('a closed tab leaves one durable window to restore without reconnecting offline panes', async ({ page, request }) => {
  await openWindow(page, [server('restore', [profile('main')])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'restore');
  await page.mouse.click(132, 57); // Disconnect intentionally before reloading.
  await expect.poll(async () => (await mudState(request, 'restore')).active).toBe(0);
  await page.waitForTimeout(150);

  await expect.poll(() => lastWindow(page)).toMatchObject({
    sessions: [{ server: 'restore', profile: 'main', connect: false }],
  });
  const snapshot = await lastWindow(page);
  expect(snapshot).toMatchObject({
    version: 1,
    sessions: [{ server: 'restore', profile: 'main', connect: false }],
    windows: [{ clusters: [{ root: { group: { tabs: [{ id: 'main' }] } } }] }],
  });
  const reopened = await page.context().newPage();
  await page.close();
  await reopened.goto('/');
  await waitForPurple(reopened, 500, 583); // New tab starts empty until explicit restore.
  await openConnect(reopened);
  await reopened.mouse.click(824, 216); // Restore into this tab's one window.
  await reopened.waitForTimeout(150); // Restore has painted the shared tab strip.
  expect(reopened.workers()).toHaveLength(0);
  expect((await mudState(request, 'restore')).opens).toBe(1);
  await reopened.mouse.click(132, 57); // Reconnect the restored pane.
  await waitForOpen(request, 'restore', 2);
  expect(reopened.context().pages()).toHaveLength(1);
  await reopened.close();
});

test('restore resolves two profiles from the current catalog and preserves cluster weights', async ({ page, request }) => {
  await seedCatalog(page, [server('online', [profile('one')]), server('offline', [profile('two')])]);
  await page.evaluate(() => sessionStorage.setItem('smudgy-last-window-v1', JSON.stringify({
    version: 1,
    slots: [
      { server: 'online', profile: 'one', connect: true, weight: 2 },
      { server: 'offline', profile: 'two', connect: false, weight: 1 },
    ],
  })));
  await openWindow(page);
  await openConnect(page);
  await page.mouse.click(824, 216);
  await waitForOpen(request, 'online');
  expect((await mudState(request, 'offline')).opens).toBe(0);
  expect(page.workers()).toHaveLength(1);
  await expect.poll(() => lastWindow(page)).not.toBeNull();
  const restored = await lastWindow(page);
  expect(restored.sessions.map(({ server, profile, connect }) => ({ server, profile, connect })))
    .toEqual([
      { server: 'online', profile: 'one', connect: true },
      { server: 'offline', profile: 'two', connect: false },
    ]);
  expect(restored.windows[0].clusters.map(({ weight }) => weight)).toEqual([2, 1]);
});

test('Connect discovers another tab’s snapshot and Restore reads the latest selection', async ({ page, request }) => {
  await seedCatalog(page, [
    server('first-window', [profile('main')]),
    server('latest-window', [profile('main')]),
  ]);
  await page.goto('/');
  await waitForPurple(page, 500, 583);
  const other = await page.context().newPage();
  await other.goto('/e2e/worker.html');
  const snapshot = (name) => ({
    version: 1,
    sessions: [{ id: 1, server: name, profile: 'main', connect: true }],
    windows: [{
      id: 1,
      geometry: { x: 0, y: 0, width: 1100, height: 900, scale: 1 },
      active_slot: 1,
      clusters: [{ weight: 1, root: {
        group: { tabs: [{ slot: 1, id: 'main' }], selected: 0 },
      } }],
    }],
  });
  await seedLastWindow(other, snapshot('first-window'));
  await openConnect(page);
  await expect.poll(async () => (await pixel(page, 824, 216))[0]).toBeGreaterThan(50);
  // A tab opened before the save now offers Restore.
  await seedLastWindow(other, snapshot('latest-window'));
  await page.mouse.click(824, 216);
  await waitForOpen(request, 'latest-window');
  expect((await mudState(request, 'first-window')).opens).toBe(0);
  expect(page.context().pages()).toHaveLength(2); // Restore did not open another tab.
  await other.close();
});

test('profile-bound local package survives reload and runs through the shared Connect flow', async ({ page, request }) => {
  await page.goto('/e2e/worker.html');
  const installed = await page.evaluate(() => new Promise((resolve) => {
    const worker = new Worker('/package-install-worker.js', { type: 'module' });
    worker.onmessage = ({ data }) => { worker.terminate(); resolve(data); };
    worker.onerror = (event) => { worker.terminate(); resolve({ error: event.message }); };
    worker.postMessage({
      name: 'connected-package',
      files: [
        ['smudgy.package.json', JSON.stringify({ version: '1.0.0', target: 'both', entry: 'index.ts' })],
        ['index.ts', 'import { createAlias, send } from "smudgy:core"; createAlias(/^package$/, () => send("package-connected"));'],
      ],
    });
  }));
  expect(installed).toEqual({ name: 'connected-package' });
  const saved = profile('main');
  saved.packages = ['connected-package'];
  await openWindow(page, [server('package-ui', [saved])]);
  await page.reload();
  await waitForPurple(page, 500, 583);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'package-ui');
  await page.mouse.click(300, 878);
  await page.keyboard.type('package');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'package-ui', 'package-connected');
});

test('missing local package fails before opening a WSS connection', async ({ page, request }) => {
  const saved = profile('main');
  saved.packages = ['not-installed'];
  await openWindow(page, [server('missing-package', [saved])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await expect.poll(async () => {
    const [red, green, blue] = await pixel(page, 3, 850);
    return red > 150 && green > 70 && blue < 100;
  }).toBeTruthy();
  await expect.poll(() => page.workers().length).toBe(0);
  expect((await mudState(request, 'missing-package')).opens).toBe(0);
});

test('profile send waits for network text despite a local script print', async ({ page, request }) => {
  await setAutoGreeting(request, 'quiet-local', false);
  const saved = profile('main', {
    script: 'function onInput(text, api) { if (text === "show-local") { api.print("local output"); return true; } return false; }',
  });
  saved.send_on_connect = 'login local';
  await openWindow(page, [server('quiet-local', [saved])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'quiet-local');
  await page.mouse.click(300, 878);
  await page.keyboard.type('show-local');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(150); // Let the worker's 8 ms presentation flush finish.
  expect((await mudState(request, 'quiet-local')).buffers.join('')).not.toContain('login local');
  await emit(request, 'quiet-local', 'Login: ');
  await waitForCommand(request, 'quiet-local', 'login local');
});

test('reconnect does not send profile text for an old prompt', async ({ page, request }) => {
  await setAutoGreeting(request, 'quiet-reconnect', false);
  const saved = profile('main');
  saved.send_on_connect = 'login reconnect';
  await openWindow(page, [server('quiet-reconnect', [saved])]);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'quiet-reconnect');
  await emit(request, 'quiet-reconnect', 'Old prompt: ');
  await waitForCommand(request, 'quiet-reconnect', 'login reconnect');
  await page.mouse.click(132, 57); // Disconnect, preserving scrollback and worker.
  await expect.poll(async () => (await mudState(request, 'quiet-reconnect')).active).toBe(0);
  await page.waitForTimeout(150);
  await page.mouse.click(132, 57); // Reconnect with greeting still suppressed.
  await waitForOpen(request, 'quiet-reconnect', 2);
  await page.waitForTimeout(150);
  const beforeGreeting = (await mudState(request, 'quiet-reconnect')).buffers
    .filter((buffer) => buffer.toString('utf8').includes('login reconnect'));
  expect(beforeGreeting).toHaveLength(1);
  await emit(request, 'quiet-reconnect', 'New prompt: ');
  await expect.poll(async () => (await mudState(request, 'quiet-reconnect')).buffers
    .filter((buffer) => buffer.toString('utf8').includes('login reconnect')).length).toBe(2);
});

test('two WSS sessions have separate workers and panes', async ({ page, request }) => {
  await openWindow(page, [server('left'), server('right')]);
  await openConnect(page);
  await page.mouse.click(600, 287); // First server, without profile.
  await waitForOpen(request, 'left');
  await page.mouse.click(78, 20); // Toolbar Connect.
  await waitForPurple(page, 500, 165);
  await page.mouse.click(210, 275); // Select second server in rail.
  await page.mouse.click(600, 287);
  await waitForOpen(request, 'right');
  await expect.poll(() => page.workers().length).toBe(2);
  expect((await pixel(page, 543, 400))[0]).toBeGreaterThan((await pixel(page, 300, 400))[0]);
  await page.mouse.click(150, 878);
  await page.keyboard.type('left-command');
  await page.keyboard.press('Enter');
  await page.mouse.click(750, 878);
  await page.keyboard.type('right-command');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'left', 'left-command');
  await waitForCommand(request, 'right', 'right-command');
  expect((await mudState(request, 'left')).buffers.join('')).not.toContain('right-command');
  expect((await mudState(request, 'right')).buffers.join('')).not.toContain('left-command');
});

test('profile editor persists login commands in the shared modal', async ({ page, request }) => {
  await openWindow(page, [server('editor')]);
  await openConnect(page);
  await waitForPurple(page, 585, 458); // Empty-profile action has loaded.
  await page.mouse.click(643, 466);
  await page.mouse.click(470, 280);
  await page.keyboard.type('browser');
  await page.mouse.click(470, 345);
  await page.keyboard.type('Saved profile');
  await page.mouse.click(470, 415);
  await page.keyboard.type('hello profile');
  await page.mouse.move(750, 625);
  await page.mouse.wheel(0, 1495);
  await page.mouse.click(400, 716); // Save at bottom of scrolled form.
  await expect.poll(async () => (await catalog(page)).servers[0].profiles.length).toBe(1);
  expect((await catalog(page)).servers[0].profiles[0]).toMatchObject({
    name: 'browser', caption: 'Saved profile', send_on_connect: 'hello profile',
  });
  await page.reload();
  await waitForPurple(page, 500, 583);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'editor');
  await waitForCommand(request, 'editor', 'hello profile');
});

test('profile editor imports a local package through the browser picker', async ({ page, request }) => {
  await openWindow(page, [server('importer')]);
  await openConnect(page);
  await waitForPurple(page, 585, 458);
  await page.mouse.click(643, 466); // New profile.
  await page.mouse.click(470, 280);
  await page.keyboard.type('packages');
  await page.mouse.move(750, 625);
  await page.mouse.wheel(0, 1600);
  const chooserPromise = page.waitForEvent('filechooser');
  await page.mouse.click(500, 643); // Import button in the scrolled profile form.
  const chooser = await chooserPromise;
  const installerWorkerPromise = page.waitForEvent('worker');
  await chooser.setFiles(fileURLToPath(new URL('./fixtures/browser-package', import.meta.url)));
  const installerWorker = await installerWorkerPromise;
  await expect.poll(() => page.workers().includes(installerWorker)).toBe(false);
  const installed = await page.evaluate(() => new Promise((resolve) => {
    const opened = indexedDB.open('smudgy-web-packages', 1);
    opened.onsuccess = () => {
      const database = opened.result;
      const read = database.transaction('packages').objectStore('packages').get('browser-package');
      read.onsuccess = () => { resolve(Boolean(read.result)); database.close(); };
      read.onerror = () => { resolve(false); database.close(); };
    };
    opened.onerror = () => resolve(false);
  }));
  expect(installed).toBeTruthy();
  await page.waitForTimeout(100); // Let iced consume the completed import message.
  await page.mouse.move(750, 625);
  await page.mouse.wheel(0, 500); // New package row pushes Save below the viewport.
  await page.mouse.click(400, 716); // Save profile after import finishes.
  await expect.poll(async () => (await catalog(page)).servers[0].package_activations)
    .toEqual([{ name: 'browser-package', activation: { mode: 'selected', profiles: ['packages'] } }]);
  await page.reload();
  await waitForPurple(page, 500, 583);
  await openConnect(page);
  await waitForPurple(page, 838, 401);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'importer');
  await expect.poll(async () => (await mudState(request, 'importer')).active).toBe(1);
  await page.mouse.click(300, 878);
  await page.keyboard.type('imported');
  await page.keyboard.press('Enter');
  await waitForCommand(request, 'importer', 'from-imported-package');
});
