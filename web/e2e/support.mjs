import { expect } from '@playwright/test';

export const endpoint = (id) => `wss://127.0.0.1:9443/mud/${id}`;

export async function resetFixture(request) {
  const response = await request.post('/__test/reset');
  expect(response.ok()).toBeTruthy();
}

export async function setAutoGreeting(request, id, autoGreeting) {
  const response = await request.post(`/__test/configure?id=${id}`, {
    data: { autoGreeting },
  });
  expect(response.ok()).toBeTruthy();
}

export async function openWorker(page, id, automation = {}, {
  autoAck = true, url = endpoint(id), sendOnConnect = '', encoding = '', packages = [],
  startAfterBoot = false, profileAutomation = null, maxLines, commandSyntax, sendRedactions = [],
} = {}) {
  if (!page.url().endsWith('/e2e/worker.html')) {
    await page.goto('/e2e/worker.html');
  }
  await page.evaluate(async ({ id, url, automation, autoAck, sendOnConnect, encoding, packages, startAfterBoot, profileAutomation, maxLines, commandSyntax, sendRedactions }) => {
    const packageSources = [];
    if (packages.length) {
      const database = await new Promise((resolve, reject) => {
        const request = indexedDB.open('smudgy-web-packages', 1);
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      try {
        for (const name of packages) {
          const record = await new Promise((resolve, reject) => {
            const request = database.transaction('packages').objectStore('packages').get(name);
            request.onsuccess = () => resolve(request.result);
            request.onerror = () => reject(request.error);
          });
          packageSources.push({ name, code: record.code });
        }
      } finally {
        database.close();
      }
    }
    const worker = new Worker('/session-worker.js');
    const state = { frames: [], panes: [], fatals: [], errors: [] };
    window.testSessions ??= {};
    const start = {
      kind: 'start', endpoint: url, columns: 80, rows: 24,
      max_lines: maxLines,
      command_syntax: commandSyntax && JSON.stringify(commandSyntax),
      encoding,
      send_on_connect: sendOnConnect,
      send_on_connect_redactions: JSON.stringify(sendRedactions),
      automation: JSON.stringify({
        shared: { aliases: [], triggers: [], script: '', ...automation },
        profile: profileAutomation,
      }),
      packages: JSON.stringify(packages),
      package_sources: packageSources,
    };
    window.testSessions[id] = { worker, state, start };
    worker.onmessage = ({ data }) => {
      if (data instanceof ArrayBuffer) {
        const view = new DataView(data);
        const magic = String.fromCharCode(...new Uint8Array(data, 0, 4));
        const sequence = view.getBigUint64(4, true).toString();
        state.frames.push({
          magic,
          sequence,
          connection: view.getUint8(12),
          flags: view.getUint8(13),
          maxLines: Number(view.getBigUint64(22, true)),
          revision: Number(view.getBigUint64(30, true)),
          committed: Number(view.getBigUint64(38, true)),
          bytes: data.byteLength,
        });
        if (autoAck) worker.postMessage({ kind: 'ack', sequence });
      } else if (data.kind === 'fatal') {
        state.fatals.push(data.message);
      } else if (data.kind === 'pane') {
        state.panes.push(data);
      } else {
        state.errors.push(JSON.stringify(data));
      }
    };
    worker.onerror = (event) => state.errors.push(event.message);
    if (!startAfterBoot) worker.postMessage(start);
  }, { id, url, automation, autoAck, sendOnConnect, encoding, packages, startAfterBoot, profileAutomation, maxLines, commandSyntax, sendRedactions });
  if (startAfterBoot) {
    const worker = page.workers().at(-1);
    await expect.poll(() => worker.evaluate(() => ready)).toBeTruthy();
    await page.evaluate((id) => {
      const { worker, start } = window.testSessions[id];
      worker.postMessage(start);
    }, id);
  }
}

export async function postWorker(page, id, message) {
  await page.evaluate(({ id, message }) => {
    window.testSessions[id].worker.postMessage(message);
  }, { id, message });
}

export async function workerState(page, id) {
  return page.evaluate((id) => window.testSessions[id].state, id);
}

export async function mudState(request, id) {
  const response = await request.get(`/__test/state?id=${id}`);
  expect(response.ok()).toBeTruthy();
  const state = await response.json();
  return { ...state, buffers: state.messages.map((message) => Buffer.from(message, 'base64')) };
}

export async function emit(request, id, bytes) {
  const response = await request.post(`/__test/emit?id=${id}`, {
    data: { base64: Buffer.from(bytes).toString('base64') },
  });
  expect(response.ok()).toBeTruthy();
  expect((await response.json()).sent).toBeGreaterThan(0);
}

export async function emitText(request, id, text) {
  const response = await request.post(`/__test/emit?id=${id}`, { data: { text } });
  expect(response.ok()).toBeTruthy();
  expect((await response.json()).sent).toBeGreaterThan(0);
}

export async function dropConnections(request, id) {
  const response = await request.post(`/__test/drop?id=${id}`);
  expect(response.ok()).toBeTruthy();
  expect((await response.json()).dropped).toBeGreaterThan(0);
}

export async function waitForOpen(request, id, count = 1) {
  await expect.poll(async () => (await mudState(request, id)).opens).toBe(count);
}

export async function waitForCommand(request, id, command) {
  await expect.poll(async () => (await mudState(request, id)).buffers
    .some((buffer) => buffer.toString('utf8').includes(command))).toBeTruthy();
}

export async function seedCatalog(page, servers) {
  await page.goto('/e2e/worker.html');
  await page.evaluate(async (servers) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web', 5);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains('connection-catalog')) {
          request.result.createObjectStore('connection-catalog');
        }
        if (!request.result.objectStoreNames.contains('profile-credentials')) {
          request.result.createObjectStore('profile-credentials');
        }
        if (!request.result.objectStoreNames.contains('named-layouts')) {
          request.result.createObjectStore('named-layouts').createIndex('server', 'server');
        }
        if (!request.result.objectStoreNames.contains('last-window')) {
          request.result.createObjectStore('last-window');
        }
        if (!request.result.objectStoreNames.contains('browser-settings')) {
          request.result.createObjectStore('browser-settings');
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise((resolve, reject) => {
      const transaction = db.transaction('connection-catalog', 'readwrite');
      transaction.objectStore('connection-catalog').put(JSON.stringify({ generation: 1, servers }), 'current');
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
    });
    db.close();
  }, servers);
}

export async function lastWindow(page) {
  return page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      const json = await new Promise((resolve, reject) => {
        const transaction = db.transaction('last-window', 'readonly');
        const request = transaction.objectStore('last-window').get('current');
        request.onsuccess = () => resolve(request.result ?? null);
        request.onerror = () => reject(request.error);
      });
      return json ? JSON.parse(json) : null;
    } finally {
      db.close();
    }
  });
}

export async function seedLastWindow(page, workspace) {
  await page.evaluate(async (snapshot) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      await new Promise((resolve, reject) => {
        const transaction = db.transaction('last-window', 'readwrite');
        transaction.objectStore('last-window').put(JSON.stringify(snapshot), 'current');
        transaction.oncomplete = resolve;
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      db.close();
    }
  }, workspace);
}

export async function seedPassword(page, serverName, profileName, value) {
  await page.evaluate(async ({ serverName, profileName, value }) => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      await new Promise((resolve, reject) => {
        const transaction = db.transaction('profile-credentials', 'readwrite');
        transaction.objectStore('profile-credentials')
          .put(value, JSON.stringify([serverName, profileName]));
        transaction.oncomplete = resolve;
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      db.close();
    }
  }, { serverName, profileName, value });
}

export function server(id, profiles = []) {
  return { name: id, endpoint: endpoint(id), profiles };
}

export function profile(name, automation = {}) {
  return {
    name, caption: '', send_on_connect: '',
    automation: { aliases: [], triggers: [], script: '', ...automation },
  };
}

// iced paints one canvas. Decode a screenshot in a throwaway 2D canvas so
// readiness/theme checks still observe pixels produced by the real renderer.
export async function pixel(page, x, y) {
  const png = (await page.screenshot()).toString('base64');
  return page.evaluate(async ({ png, x, y }) => {
    const image = new Image();
    image.src = `data:image/png;base64,${png}`;
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = 1;
    canvas.height = 1;
    const context = canvas.getContext('2d');
    context.drawImage(image, -x, -y);
    return [...context.getImageData(0, 0, 1, 1).data];
  }, { png, x, y });
}

export async function waitForPurple(page, x, y) {
  await expect.poll(async () => {
    const [red, green, blue] = await pixel(page, x, y);
    return red > 25 && blue > red * 1.3 && blue > green * 1.5;
  }).toBeTruthy();
}

export async function catalog(page) {
  return page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open('smudgy-web');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      return await new Promise((resolve, reject) => {
        const transaction = db.transaction('connection-catalog', 'readonly');
        const request = transaction.objectStore('connection-catalog').get('current');
        request.onsuccess = () => {
          const record = JSON.parse(request.result ?? '{"generation":0,"servers":[]}');
          resolve(record.catalog ?? record);
        };
        request.onerror = () => reject(request.error);
      });
    } finally {
      db.close();
    }
  });
}
