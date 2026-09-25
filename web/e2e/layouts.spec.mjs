import { test, expect } from '@playwright/test';
import { lastWindow, profile, resetFixture, seedCatalog, server, waitForOpen, waitForPurple } from './support.mjs';

test.beforeEach(async ({ request }) => resetFixture(request));

async function savedLayouts(page, serverName) {
  return page.evaluate(async (name) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      return await new Promise((resolve, reject) => {
        const transaction = db.transaction('named-layouts', 'readonly');
        const request = transaction.objectStore('named-layouts').index('server').getAll(name);
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
    } finally {
      db.close();
    }
  }, serverName);
}

async function seedLayout(page, serverName, name, workspace) {
  await page.evaluate(async ({ serverName, name, workspace }) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      await new Promise((resolve, reject) => {
        const transaction = db.transaction('named-layouts', 'readwrite');
        transaction.objectStore('named-layouts').put(
          { server: serverName, name, json: JSON.stringify(workspace) },
          [serverName, name.toLowerCase()],
        );
        transaction.oncomplete = resolve;
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      db.close();
    }
  }, { serverName, name, workspace });
}

test('shared Layouts dialog saves the active browser window as a versioned workspace', async ({ page, request }) => {
  await seedCatalog(page, [server('layouts', [profile('main')])]);
  await page.goto('/');
  await expect(page.locator('canvas')).toHaveCount(1);
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'layouts');
  await page.mouse.click(130, 20);
  await waitForPurple(page, 500, 217);
  await page.mouse.click(300, 670);
  await page.keyboard.type('combat');
  await page.mouse.click(270, 343);
  await expect.poll(async () => (await savedLayouts(page, 'layouts')).map(({ name }) => name))
    .toEqual(['combat']);
  const saved = JSON.parse((await savedLayouts(page, 'layouts'))[0].json);
  expect(saved).toMatchObject({
    version: 1,
    sessions: [{ server: 'layouts', profile: 'main', connect: true }],
    windows: [{ clusters: [{ root: { group: { tabs: [{ id: 'main' }] } } }] }],
  });
  expect(page.workers()).toHaveLength(1);

  await page.mouse.click(765, 289); // Rename the saved layout.
  await page.keyboard.press('End');
  for (let i = 0; i < 'combat'.length; i += 1) await page.keyboard.press('Backspace');
  await page.keyboard.type('peace');
  await page.mouse.click(270, 343);
  await expect.poll(async () => (await savedLayouts(page, 'layouts')).map(({ name }) => name))
    .toEqual(['peace']);

  await page.mouse.click(828, 289); // Delete requires confirmation.
  await waitForPurple(page, 250, 290);
  expect(await savedLayouts(page, 'layouts')).toHaveLength(1);
  await page.mouse.click(270, 290);
  await expect.poll(() => savedLayouts(page, 'layouts')).toEqual([]);
});

test('one-window split layout applies and survives explicit restore', async ({ page, request }) => {
  await seedCatalog(page, [
    server('layout-a', [profile('main')]),
    server('layout-b', [profile('main')]),
  ]);
  await page.goto('/');
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'layout-a');

  const group = (slot) => ({ group: { tabs: [{ slot, id: 'main' }], selected: 0 } });
  await seedLayout(page, 'layout-a', 'split', {
    version: 1,
    sessions: [
      { id: 11, server: 'layout-a', profile: 'main', connect: true },
      { id: 12, server: 'layout-b', profile: 'main', connect: false },
    ],
    windows: [{
      id: 1,
      geometry: { x: 0, y: 0, width: 1100, height: 900, scale: 1 },
      active_slot: 11,
      clusters: [{ weight: 1, root: {
        split: { axis: 'vertical', sizing: { ratio: 0.5 }, a: group(11), b: group(12) },
      } }],
    }],
  });
  await page.mouse.click(130, 20);
  await waitForPurple(page, 500, 217);
  await page.mouse.click(290, 288);
  await expect.poll(async () => (await lastWindow(page))?.sessions.length ?? 0).toBe(2);
  expect(page.workers()).toHaveLength(1);
  const before = await lastWindow(page);
  expect(before.windows[0].clusters[0].root.split.axis).toBe('vertical');

  await page.reload();
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(824, 216);
  await expect.poll(() => page.workers().length).toBe(1);
  const after = await lastWindow(page);
  expect(after.windows[0].clusters[0].root.split.axis).toBe('vertical');
  expect(after.sessions.map(({ server, connect }) => ({ server, connect }))).toEqual([
    { server: 'layout-a', connect: true },
    { server: 'layout-b', connect: false },
  ]);
});

test('one-window tab group restores its selection and switches main-session bodies', async ({ page, request }) => {
  await seedCatalog(page, [
    server('tab-a', [profile('main')]),
    server('tab-b', [profile('main')]),
  ]);
  await page.goto('/');
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'tab-a');

  await seedLayout(page, 'tab-a', 'tabs', {
    version: 1,
    sessions: [
      { id: 11, server: 'tab-a', profile: 'main', connect: true },
      { id: 12, server: 'tab-b', profile: 'main', connect: false },
    ],
    windows: [{
      id: 1,
      geometry: { x: 0, y: 0, width: 1100, height: 900, scale: 1 },
      active_slot: 12,
      clusters: [{ weight: 1, root: { group: {
        tabs: [{ slot: 11, id: 'main' }, { slot: 12, id: 'main' }],
        selected: 1,
      } } }],
    }],
  });
  await page.mouse.click(130, 20);
  await waitForPurple(page, 500, 217);
  await page.mouse.click(290, 288);
  await expect.poll(async () => (await lastWindow(page))?.windows[0].clusters[0].root.group.selected)
    .toBe(1);
  expect(page.workers()).toHaveLength(1);

  await page.mouse.click(80, 58); // Select the first tab in the pane header.
  await expect.poll(async () => (await lastWindow(page))?.windows[0].clusters[0].root.group.selected)
    .toBe(0);
  await page.reload();
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(824, 216);
  await expect.poll(async () => (await lastWindow(page))?.windows[0].clusters[0].root.group.selected)
    .toBe(0);
});

test('shared tab drag moves a browser tab to a whole-grid edge and persists the shape', async ({ page, request }) => {
  await seedCatalog(page, [
    server('drag-a', [profile('main')]),
    server('drag-b', [profile('main')]),
  ]);
  await page.goto('/');
  await expect(page.locator('canvas')).toHaveCount(1);
  await page.waitForTimeout(100);
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'drag-a');

  await seedLayout(page, 'drag-a', 'drag-tabs', {
    version: 1,
    sessions: [
      { id: 11, server: 'drag-a', profile: 'main', connect: true },
      { id: 12, server: 'drag-b', profile: 'main', connect: false },
    ],
    windows: [{
      id: 1,
      geometry: { x: 0, y: 0, width: 1100, height: 900, scale: 1 },
      active_slot: 12,
      clusters: [{ weight: 1, root: { group: {
        tabs: [{ slot: 11, id: 'main' }, { slot: 12, id: 'main' }],
        selected: 1,
      } } }],
    }],
  });
  await page.mouse.click(130, 20);
  await waitForPurple(page, 500, 217);
  await page.mouse.click(290, 288);
  await expect.poll(async () => (await lastWindow(page))?.windows[0].clusters[0].root.group.tabs.length)
    .toBe(2);

  // Drag the first tab through the shared strip into the grid's top edge.
  // Target precedence and the resulting top-level split match native.
  await page.mouse.move(35, 57);
  await page.mouse.down();
  await page.mouse.move(390, 57, { steps: 8 });
  await page.mouse.up();
  await expect.poll(async () => {
    const split = (await lastWindow(page))?.windows[0].clusters[0].root.split;
    return {
      axis: split?.axis,
      first: split?.a.group.tabs[0].slot,
      second: split?.b.group.tabs[0].slot,
    };
  }).toEqual({ axis: 'horizontal', first: 1, second: 2 });
});

test('applying a layout asks before closing an omitted live session', async ({ page, request }) => {
  await seedCatalog(page, [server('omitted', [profile('main')])]);
  await page.goto('/');
  await waitForPurple(page, 500, 583);
  await page.mouse.click(550, 583);
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'omitted');
  await page.mouse.click(75, 20); // Open Connect from the shared toolbar.
  await waitForPurple(page, 500, 165);
  await page.mouse.click(875, 411);
  await waitForOpen(request, 'omitted', 2);
  expect(page.workers()).toHaveLength(2);

  await seedLayout(page, 'omitted', 'single', {
    version: 1,
    sessions: [{ id: 1, server: 'omitted', profile: 'main', connect: true }],
    windows: [{
      id: 1,
      geometry: { x: 0, y: 0, width: 1100, height: 900, scale: 1 },
      active_slot: 1,
      clusters: [{ weight: 1, root: { group: { tabs: [{ slot: 1, id: 'main' }], selected: 0 } } }],
    }],
  });
  await page.mouse.click(130, 20);
  await waitForPurple(page, 500, 217);
  await page.mouse.click(290, 288);
  await waitForPurple(page, 250, 670); // Keep-or-close prompt is ready.
  expect(page.workers()).toHaveLength(2); // Nothing closes without an explicit answer.
  await page.mouse.click(832, 289); // Explicit Close for the omitted session.
  await waitForPurple(page, 814, 289);
  await page.mouse.click(295, 670); // Apply layout.
  await expect.poll(() => page.workers().length).toBe(1);
  await expect.poll(async () => (await lastWindow(page))?.sessions.length).toBe(1);
  const saved = await lastWindow(page);
  expect(saved.sessions).toHaveLength(1);
});
