import { existsSync, readdirSync } from 'node:fs';
import { test, expect } from '@playwright/test';

// wasm-bindgen copies the production storage module into a hashed snippet
// directory. Import that emitted module, not an imitation of its transaction.
const snippetRoot = new URL('../e2e-dist/snippets/', import.meta.url);
const storageDirectory = readdirSync(snippetRoot).find((directory) =>
  existsSync(new URL(`${directory}/src/storage.js`, snippetRoot)));
if (!storageDirectory) throw new Error('release build did not emit the IndexedDB adapter');
const storageUrl = `/snippets/${storageDirectory}/src/storage.js`;

test('browser settings use one durable snapshot and ordered writes', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  const result = await page.evaluate(async (url) => {
    const storage = await import(url);
    const initial = await storage.loadSettings();
    const first = { schema: 2, appearance: { font_size: 18, bold_mode: 'bold', disable_blink: false }, input: { command_input_behavior: 'clear', mask_input_on_server_echo: false, history_case_sensitive_match: false, max_history: 100 } };
    const latest = { schema: 2, appearance: { font_size: 22, bold_mode: 'bright', disable_blink: true }, input: { command_input_behavior: 'select_all', mask_input_on_server_echo: true, history_case_sensitive_match: true, max_history: 500 } };
    await Promise.all([
      storage.saveSettings(JSON.stringify(first)),
      storage.saveSettings(JSON.stringify(latest)),
    ]);
    return { initial, saved: JSON.parse(await storage.loadSettings()) };
  }, storageUrl);
  expect(result.initial).toBeNull();
  expect(result.saved).toEqual({
    schema: 2,
    appearance: { font_size: 22, bold_mode: 'bright', disable_blink: true },
    input: { command_input_behavior: 'select_all', mask_input_on_server_echo: true, history_case_sensitive_match: true, max_history: 500 },
  });
});

test('concurrent tabs cannot overwrite a catalog generation', async ({ context }) => {
  const first = await context.newPage();
  const second = await context.newPage();
  await Promise.all([first.goto('/e2e/worker.html'), second.goto('/e2e/worker.html')]);

  const save = (page, name) => page.evaluate(async ({ storageUrl, name }) => {
    const storage = await import(storageUrl);
    return storage.saveCatalog('0', JSON.stringify({
      generation: 1,
      servers: [{ name, endpoint: `wss://${name}.example/ws`, profiles: [] }],
    }));
  }, { storageUrl, name });
  const outcomes = await Promise.all([save(first, 'one'), save(second, 'two')]);
  expect(outcomes.filter(Boolean)).toHaveLength(1);

  const stale = await save(second, 'stale');
  expect(stale).toBe(false);
  const catalogs = await Promise.all([first, second].map((page) => page.evaluate(async (url) => {
    const storage = await import(url);
    return JSON.parse(await storage.loadCatalog());
  }, storageUrl)));
  expect(catalogs[0]).toEqual(catalogs[1]);
  expect(catalogs[0].generation).toBe(1);
  expect(catalogs[0].servers[0].name).toBe(outcomes[0] ? 'one' : 'two');
});

test('window restore is one durable origin-wide record, not a tab history', async ({ context }) => {
  const first = await context.newPage();
  const second = await context.newPage();
  await Promise.all([first.goto('/e2e/worker.html'), second.goto('/e2e/worker.html')]);
  const write = (page, marker) => page.evaluate(async ({ storageUrl, marker }) => {
    const storage = await import(storageUrl);
    await storage.saveLastWindow(JSON.stringify({ marker }));
  }, { storageUrl, marker });
  await write(first, 'first');
  await write(second, 'second');
  await first.close();
  const third = await context.newPage();
  await third.goto('/e2e/worker.html');
  const saved = await third.evaluate(async (url) => {
    const storage = await import(url);
    return JSON.parse(await storage.loadLastWindow());
  }, storageUrl);
  expect(saved).toEqual({ marker: 'second' });
  const queued = await third.evaluate(async (url) => {
    const storage = await import(url);
    await Promise.all([
      storage.saveLastWindow(JSON.stringify({ marker: 'queued-first' })),
      storage.saveLastWindow(JSON.stringify({ marker: 'queued-last' })),
    ]);
    return JSON.parse(await storage.loadLastWindow());
  }, storageUrl);
  expect(queued).toEqual({ marker: 'queued-last' });
  await second.close();
  await third.close();
});

test('legacy catalog is wrapped by the next CAS save without changing its records', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  const result = await page.evaluate(async (url) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('connection-catalog');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const legacy = { generation: 7, servers: [{ name: 'old', endpoint: 'wss://example.org/ws', profiles: [] }] };
    await new Promise((resolve, reject) => {
      const transaction = db.transaction('connection-catalog', 'readwrite');
      transaction.objectStore('connection-catalog').put(JSON.stringify(legacy), 'current');
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
    });
    db.close();
    const storage = await import(url);
    const loaded = JSON.parse(await storage.loadCatalog());
    const saved = await storage.saveCatalog('7', JSON.stringify({ ...loaded, generation: 8 }));
    const upgraded = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web', 5);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const record = await new Promise((resolve, reject) => {
      const transaction = upgraded.transaction('connection-catalog', 'readonly');
      const request = transaction.objectStore('connection-catalog').get('current');
      request.onsuccess = () => resolve(JSON.parse(request.result));
      request.onerror = () => reject(request.error);
    });
    const credentialStore = upgraded.objectStoreNames.contains('profile-credentials');
    const layoutStore = upgraded.objectStoreNames.contains('named-layouts');
    const windowStore = upgraded.objectStoreNames.contains('last-window');
    const appearanceStore = upgraded.objectStoreNames.contains('browser-settings');
    upgraded.close();
    return { loaded, saved, record, credentialStore, layoutStore, windowStore, appearanceStore };
  }, storageUrl);
  expect(result.loaded.generation).toBe(7);
  expect(result.saved).toBe(true);
  expect(result.credentialStore).toBe(true);
  expect(result.layoutStore).toBe(true);
  expect(result.windowStore).toBe(true);
  expect(result.appearanceStore).toBe(true);
  expect(result.record).toEqual({ schema: 3, catalog: { ...result.loaded, generation: 8 } });
});

test('named layouts use server-scoped folded keys and preserve the workspace document', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  const result = await page.evaluate(async (url) => {
    const storage = await import(url);
    const workspace = {
      version: 1,
      sessions: [{ id: 1, server: 'arctic', profile: 'Default', connect: false }],
      windows: [{
        id: 1,
        geometry: { x: 0, y: 0, width: 1200, height: 800, scale: 1 },
        clusters: [{ weight: 1, root: { group: { tabs: [{ slot: 1, id: 'main' }], selected: 0 } } }],
      }],
    };
    const first = `${JSON.stringify(workspace)}\n`;
    const second = `${JSON.stringify({ ...workspace, sessions: [{ ...workspace.sessions[0], connect: true }] })}\n`;
    await storage.saveLayout('arctic', 'combat', 'Combat', first);
    await storage.saveLayout('other', 'combat', 'Combat', 'other server');
    const caseVariant = await storage.layoutExists('arctic', 'combat');
    let deniedSave = false;
    try { await storage.saveLayout('arctic', 'combat', 'COMBAT', second); }
    catch { deniedSave = true; }
    const beforeOverwrite = await storage.loadLayout('arctic', 'combat');
    await storage.saveLayout('arctic', 'combat', 'COMBAT', second, true);
    const namesAfterSave = JSON.parse(await storage.listLayouts('arctic'));
    const updated = await storage.loadLayout('arctic', 'combat');
    await storage.saveLayout('arctic', 'peace', 'Peace', 'displaced');
    let deniedRename = false;
    try { await storage.renameLayout('arctic', 'combat', 'peace', 'PEACE'); }
    catch { deniedRename = true; }
    const beforeRename = await storage.loadLayout('arctic', 'peace');
    await storage.renameLayout('arctic', 'combat', 'peace', 'PEACE', true);
    const namesAfterRename = JSON.parse(await storage.listLayouts('arctic'));
    const renamed = await storage.loadLayout('arctic', 'peace');
    const other = await storage.loadLayout('other', 'combat');
    await storage.deleteLayout('arctic', 'peace');
    return {
      caseVariant, deniedSave, beforeOverwrite, namesAfterSave, updated,
      deniedRename, beforeRename, namesAfterRename, renamed, other,
      deleted: await storage.layoutExists('arctic', 'peace'),
    };
  }, storageUrl);
  expect(result.caseVariant).toBe(true);
  expect(result.deniedSave).toBe(true);
  expect(result.beforeOverwrite).toContain('"connect":false');
  expect(result.namesAfterSave).toEqual(['Combat']);
  expect(JSON.parse(result.updated).windows).toHaveLength(1);
  expect(result.namesAfterRename).toEqual(['PEACE']);
  expect(result.deniedRename).toBe(true);
  expect(result.beforeRename).toBe('displaced');
  expect(result.renamed).toBe(result.updated);
  expect(result.other).toBe('other server');
  expect(result.deleted).toBe(false);
});

test('credential write, stale save, and profile deletion are atomic with catalog CAS', async ({ page }) => {
  await page.goto('/e2e/worker.html');
  const result = await page.evaluate(async (url) => {
    const storage = await import(url);
    const profile = { name: 'Named', caption: '', send_on_connect: '$PASSWORD' };
    const server = { name: 'mud', endpoint: 'wss://example.org/ws', profiles: [profile] };
    const initial = await storage.saveCatalog('0', JSON.stringify({ generation: 1, servers: [server] }),
      JSON.stringify({ server: 'mud', profile: 'Named', value: 'secret-123' }));
    const loaded = await storage.loadProfilePassword('mud', 'Named');
    const stale = await storage.saveCatalog('0', JSON.stringify({ generation: 1, servers: [] }),
      JSON.stringify({ server: 'mud', profile: 'Named', value: 'wrong' }));
    const afterStale = await storage.loadProfilePassword('mud', 'Named');
    const removed = await storage.saveCatalog('1', JSON.stringify({ generation: 2, servers: [
      { ...server, profiles: [] },
    ] }));
    const afterDelete = await storage.loadProfilePassword('mud', 'Named');
    const catalog = await storage.loadCatalog();
    return { initial, loaded, stale, afterStale, removed, afterDelete, catalog };
  }, storageUrl);
  expect(result.initial).toBe(true);
  expect(result.loaded).toBe('secret-123');
  expect(result.stale).toBe(false);
  expect(result.afterStale).toBe('secret-123');
  expect(result.removed).toBe(true);
  expect(result.afterDelete).toBeNull();
  expect(result.catalog).not.toContain('secret-123');
});
